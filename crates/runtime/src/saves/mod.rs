//! Save data management, local Switch user profiles, and `acc:u0`/`acc:u1` account helpers,
//! ported from `runtime/src/saves.cpp`, `runtime/src/users.cpp`, and
//! `runtime/src/svc_account.cpp`.
//!
//! `runtime/src/saves_wrapper.cpp` provides the thin C++ host callbacks (`data_root`, `log`,
//! `local_from_posix`, `zip_inflate`), forwards `nx::saves::*` and `nx::user_*` C++ APIs, and
//! registers `acc:u0` / `acc:u1` and `IStorage` / `IStorageAccessor` against the guest IPC
//! dispatcher.

use crate::ffi::{cstr_opt_lossy, host_log};
#[allow(unused_imports)]
use std::collections::BTreeSet;
#[allow(unused_imports)]
use std::ffi::{c_char, c_void, CStr, CString};
#[allow(unused_imports)]
use std::fs::{self, File};
#[allow(unused_imports)]
use std::io::{Read, Seek, SeekFrom, Write};
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
#[allow(unused_imports)]
use std::sync::{Mutex, OnceLock};
#[allow(unused_imports)]
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum number of local Switch users (Horizon limit).
pub const ACNH_MAX_USERS: usize = 8;
/// Default first user UID low 64 bits (kept so existing saves load unchanged).
pub const ACNH_FIRST_USER_LO: u64 = 0x0000_0000_0000_0001;
/// Default first user UID high 64 bits.
pub const ACNH_FIRST_USER_HI: u64 = 0x4E58_4855_5345_5231;
/// Account error returned when Network Service Account / id_token is unavailable.
pub const ACNH_NSA_UNAVAILABLE: u32 = (280 << 9) | 124;
/// Magic header in `am` preselected user storage (`IStorage`).
pub const ACNH_PRESELECTED_USER_MAGIC: u32 = 0xC794_97CA;

pub(crate) const TITLE_ID_UPPER: &str = "01006F8002326000";
pub(crate) const TITLE_ID_LOWER: &str = "01006f8002326000";
pub(crate) const KEEP_BACKUPS: usize = 10;
pub(crate) const KEEP_AUTO_BACKUPS: usize = 3;
pub(crate) const MAX_ZIP_BYTES: u64 = 1 << 30;

/// Local calendar breakdown used for backup timestamps, DOS zip timestamps, and `last_modified`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AcnhSavesLocalTime {
    pub year: i32,
    pub month: i32,
    pub day: i32,
    pub hour: i32,
    pub minute: i32,
    pub second: i32,
}

/// Host callbacks provided by `runtime/src/saves_wrapper.cpp`.
///
/// When unset (e.g. in standalone Rust unit tests), `data_root` falls back to `NX_DATA_ROOT` or
/// `"."`, `log` is a no-op, `local_from_posix` falls back to UTC calendar math, and
/// `zip_inflate` returns `false` ("zip not supported in this build").
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct AcnhSavesHost {
    pub data_root: Option<extern "C" fn(*mut c_char, usize) -> usize>,
    pub log: Option<extern "C" fn(*const c_char)>,
    pub local_from_posix: Option<extern "C" fn(i64, *mut AcnhSavesLocalTime) -> bool>,
    pub zip_inflate:
        Option<extern "C" fn(*const u8, usize, *mut u8, usize, *mut c_char, usize) -> bool>,
}

// SAFETY: `AcnhSavesHost` only holds plain function pointers.
unsafe impl Send for AcnhSavesHost {}
// SAFETY: `AcnhSavesHost` only holds plain function pointers.
unsafe impl Sync for AcnhSavesHost {}

/// C-compatible Switch user record (`account::Uid` + null-terminated UTF-8 nickname).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AcnhUserInfo {
    pub lo: u64,
    pub hi: u64,
    pub nickname: [c_char; 32],
}

pub type AcnhSavesStringCb = extern "C" fn(*const c_char, usize, *mut c_void);
pub type AcnhSavesFoundCb = extern "C" fn(*const c_char, usize, *const c_char, usize, *mut c_void);
pub type AcnhSavesUserCb = extern "C" fn(*const AcnhUserInfo, *mut c_void);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UserRecord {
    pub(crate) lo: u64,
    pub(crate) hi: u64,
    pub(crate) nickname: String,
}

impl UserRecord {
    pub(crate) fn valid(&self) -> bool {
        self.lo != 0 || self.hi != 0
    }

    pub(crate) fn to_ffi(&self) -> AcnhUserInfo {
        let mut out = AcnhUserInfo {
            lo: self.lo,
            hi: self.hi,
            nickname: [0; 32],
        };
        let bytes = self.nickname.as_bytes();
        let n = bytes.len().min(31);
        for (dst, &src) in out.nickname.iter_mut().zip(&bytes[..n]) {
            *dst = src as c_char;
        }
        out
    }
}

#[derive(Default)]
pub(crate) struct UsersState {
    pub(crate) users: Vec<UserRecord>,
    pub(crate) loaded: bool,
    pub(crate) last: Option<UserRecord>,
}

pub(crate) static HOST: OnceLock<Mutex<AcnhSavesHost>> = OnceLock::new();
pub(crate) static SAVES_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
pub(crate) static USERS_LOCK: OnceLock<Mutex<UsersState>> = OnceLock::new();

pub(crate) fn host_lock() -> &'static Mutex<AcnhSavesHost> {
    HOST.get_or_init(|| Mutex::new(AcnhSavesHost::default()))
}

pub(crate) fn current_host() -> AcnhSavesHost {
    *host_lock().lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn saves_lock() -> &'static Mutex<()> {
    SAVES_LOCK.get_or_init(|| Mutex::new(()))
}

pub(crate) fn users_lock() -> &'static Mutex<UsersState> {
    USERS_LOCK.get_or_init(|| Mutex::new(UsersState::default()))
}

pub(crate) fn call_host_string(
    f: Option<extern "C" fn(*mut c_char, usize) -> usize>,
) -> Option<String> {
    let f = f?;
    let mut buf = vec![0u8; 512];
    let len = f(buf.as_mut_ptr().cast::<c_char>(), buf.len());
    if len >= buf.len() {
        buf.resize(len + 1, 0);
        let len2 = f(buf.as_mut_ptr().cast::<c_char>(), buf.len());
        buf.truncate(len2.min(buf.len()));
    } else {
        buf.truncate(len);
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

pub(crate) fn host_data_root() -> String {
    let h = current_host();
    if let Some(root) = call_host_string(h.data_root) {
        if !root.is_empty() {
            return root;
        }
    }
    if let Ok(env_root) = std::env::var("NX_DATA_ROOT") {
        if !env_root.is_empty() {
            return env_root;
        }
    }
    String::from(".")
}

pub(crate) fn emit_string(
    cb: Option<extern "C" fn(*const c_char, usize, *mut c_void)>,
    ctx: *mut c_void,
    s: &str,
) {
    if let Some(cb) = cb {
        cb(s.as_ptr().cast::<c_char>(), s.len(), ctx);
    }
}

pub(crate) fn set_err(out: &mut String, msg: impl Into<String>) {
    *out = msg.into();
}

pub(crate) fn path_utf8(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

pub(crate) fn user_dir(data_root: &str) -> PathBuf {
    PathBuf::from(data_root).join("user")
}

pub(crate) fn device_dir(data_root: &str) -> PathBuf {
    user_dir(data_root).join("device_save")
}

pub(crate) fn backups_root(data_root: &str) -> PathBuf {
    user_dir(data_root).join("backups")
}

pub(crate) fn unix_now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

pub(crate) fn posix_to_utc_local(posix: i64) -> AcnhSavesLocalTime {
    let days = posix.div_euclid(86_400);
    let secs_of_day = posix.rem_euclid(86_400);
    let hour = (secs_of_day / 3600) as i32;
    let minute = ((secs_of_day % 3600) / 60) as i32;
    let second = (secs_of_day % 60) as i32;

    // Howard Hinnant's civil-from-days algorithm.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + i64::from(m <= 2);

    AcnhSavesLocalTime {
        year: year as i32,
        month: m as i32,
        day: d as i32,
        hour,
        minute,
        second,
    }
}

pub(crate) fn local_time_from_posix(posix: i64) -> AcnhSavesLocalTime {
    if let Some(f) = current_host().local_from_posix {
        let mut lt = AcnhSavesLocalTime::default();
        if f(posix, &mut lt) {
            return lt;
        }
    }
    posix_to_utc_local(posix)
}

/// The stamp for now in UTC, the clock the launcher's own backups (`acnh_core::backup::stamp`)
/// use, so both kinds of backup order the same way when they share an island folder.
pub(crate) fn stamp_now() -> String {
    let lt = posix_to_utc_local(unix_now_seconds());
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        lt.year, lt.month, lt.day, lt.hour, lt.minute, lt.second
    )
}

mod storage;
#[allow(unused_imports)]
pub use storage::*;
mod zip;
#[allow(unused_imports)]
pub use zip::*;
mod backup;
#[allow(unused_imports)]
pub use backup::*;
mod scan;
#[allow(unused_imports)]
pub use scan::*;
mod ffi;
#[allow(unused_imports)]
pub use ffi::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn test_mutex() -> &'static Mutex<()> {
        static M: OnceLock<Mutex<()>> = OnceLock::new();
        M.get_or_init(|| Mutex::new(()))
    }

    fn test_data_root_store() -> &'static Mutex<String> {
        static R: OnceLock<Mutex<String>> = OnceLock::new();
        R.get_or_init(|| Mutex::new(String::new()))
    }

    extern "C" fn test_host_data_root(buf: *mut c_char, cap: usize) -> usize {
        let s = test_data_root_store().lock().unwrap().clone();
        let bytes = s.as_bytes();
        if !buf.is_null() && cap > 0 {
            let n = bytes.len().min(cap - 1);
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.cast::<u8>(), n);
                *buf.add(n) = 0;
            }
        }
        bytes.len()
    }

    fn set_test_host(root: &str) {
        *test_data_root_store().lock().unwrap() = root.to_string();
        let host = AcnhSavesHost {
            data_root: Some(test_host_data_root),
            log: None,
            local_from_posix: None,
            zip_inflate: None,
        };
        // SAFETY: test-only: the host table lives on this test's stack and outlives every call that reads it.
        unsafe { acnh_saves_set_host(&host) };
    }

    struct TempRoot {
        path: PathBuf,
    }

    impl TempRoot {
        fn new(name: &str) -> Self {
            let pid = std::process::id();
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!("acnh_saves_test_{name}_{pid}_{nanos}"));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn str(&self) -> String {
            path_utf8(&self.path)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn reset_users_state() {
        let mut st = users_lock().lock().unwrap();
        *st = UsersState::default();
    }

    fn write_fake_save(dir: &Path, island_name: &str) {
        fs::create_dir_all(dir.join("Villager0")).unwrap();
        fs::write(dir.join("main.dat"), b"main-data").unwrap();
        fs::write(dir.join("mainHeader.dat"), b"header-data").unwrap();
        fs::write(dir.join("Villager0/personal.dat"), b"personal-data").unwrap();
        let mut land = Vec::new();
        for u in island_name.encode_utf16() {
            land.extend_from_slice(&u.to_le_bytes());
        }
        land.resize(20, 0);
        fs::write(dir.join("landname.dat"), &land).unwrap();
    }

    extern "C" fn collect_str(ptr: *const c_char, len: usize, ctx: *mut c_void) {
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        let out = unsafe { &mut *ctx.cast::<String>() };
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        let bytes = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) };
        *out = String::from_utf8_lossy(bytes).into_owned();
    }

    extern "C" fn collect_str_vec(ptr: *const c_char, len: usize, ctx: *mut c_void) {
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        let out = unsafe { &mut *ctx.cast::<Vec<String>>() };
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        let bytes = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) };
        out.push(String::from_utf8_lossy(bytes).into_owned());
    }

    #[test]
    fn user_save_dir_first_and_secondary() {
        let mut s = String::new();
        acnh_user_save_dir(
            ACNH_FIRST_USER_LO,
            ACNH_FIRST_USER_HI,
            Some(collect_str),
            (&mut s as *mut String).cast(),
        );
        assert_eq!(s, "save");

        acnh_user_save_dir(
            2,
            ACNH_FIRST_USER_HI,
            Some(collect_str),
            (&mut s as *mut String).cast(),
        );
        assert_eq!(s, "save_4e584855534552310000000000000002");
    }

    #[test]
    fn users_create_find_and_preselected_blob() {
        let _guard = test_mutex().lock().unwrap();
        let tmp = TempRoot::new("users");
        std::env::set_var("NX_USER_NAME", "Tester");
        set_test_host(&tmp.str());
        reset_users_state();

        assert_eq!(acnh_users_count(), 1);
        let mut u1 = AcnhUserInfo::default();
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        assert!(unsafe { acnh_user_find(ACNH_FIRST_USER_LO, ACNH_FIRST_USER_HI, &mut u1) });
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        let nick1 = unsafe { CStr::from_ptr(u1.nickname.as_ptr()) }
            .to_str()
            .unwrap();
        assert_eq!(nick1, "Tester");

        let mut u2 = AcnhUserInfo::default();
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        assert!(unsafe { acnh_user_create(&mut u2) });
        assert_eq!(u2.lo, 2);
        assert_eq!(acnh_users_count(), 2);

        acnh_user_opened(u2.lo, u2.hi);
        let mut boot = AcnhUserInfo::default();
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        unsafe { acnh_user_boot(&mut boot) };
        assert_eq!(boot.lo, 2);

        let mut blob = [0u8; 0x88];
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_account_preselected_user_blob(blob.as_mut_ptr(), blob.len()) },
            0x88
        );
        assert_eq!(
            u32::from_le_bytes(blob[0..4].try_into().unwrap()),
            ACNH_PRESELECTED_USER_MAGIC
        );
        assert_eq!(u32::from_le_bytes(blob[4..8].try_into().unwrap()), 1);
        assert_eq!(u64::from_le_bytes(blob[8..16].try_into().unwrap()), 2);
        assert_eq!(
            u64::from_le_bytes(blob[16..24].try_into().unwrap()),
            ACNH_FIRST_USER_HI
        );

        std::env::remove_var("NX_USER_NAME");
        // SAFETY: test-only: the host table lives on this test's stack and outlives every call that reads it.
        unsafe { acnh_saves_set_host(std::ptr::null()) };
        reset_users_state();
    }

    #[test]
    fn saves_import_backup_export_zip_and_restore() {
        let _guard = test_mutex().lock().unwrap();
        let tmp = TempRoot::new("saves");
        set_test_host(&tmp.str());

        assert!(!acnh_saves_has_save());

        let src = tmp.path.join("incoming");
        write_fake_save(&src, "NookIsle");

        let src_c = CString::new(path_utf8(&src)).unwrap();
        let mut why = String::new();
        assert!(acnh_saves_looks_like_acnh_save(
            src_c.as_ptr(),
            Some(collect_str),
            (&mut why as *mut String).cast()
        ));

        let mut err = String::new();
        assert!(acnh_saves_import_save(
            src_c.as_ptr(),
            Some(collect_str),
            (&mut err as *mut String).cast()
        ));
        assert!(acnh_saves_has_save());

        let mut backup_name = String::new();
        acnh_saves_backup_now(
            false,
            Some(collect_str),
            (&mut backup_name as *mut String).cast(),
            Some(collect_str),
            (&mut err as *mut String).cast(),
        );
        assert!(backup_name.starts_with("NookIsle "));

        let mut backups: Vec<String> = Vec::new();
        acnh_saves_list_backups(
            Some(collect_str_vec),
            (&mut backups as *mut Vec<String>).cast(),
        );
        assert_eq!(backups, vec![backup_name.clone()]);

        let zip_path = tmp.path.join("export.zip");
        let zip_c = CString::new(path_utf8(&zip_path)).unwrap();
        let mut written = String::new();
        assert!(acnh_saves_export_save(
            zip_c.as_ptr(),
            Some(collect_str),
            (&mut written as *mut String).cast(),
            Some(collect_str),
            (&mut err as *mut String).cast()
        ));
        assert!(zip_path.is_file());
        assert!(acnh_saves_looks_like_acnh_save(
            zip_c.as_ptr(),
            Some(collect_str),
            (&mut why as *mut String).cast()
        ));

        let b_c = CString::new(backup_name).unwrap();
        assert!(acnh_saves_restore_backup(
            b_c.as_ptr(),
            Some(collect_str),
            (&mut err as *mut String).cast()
        ));
        assert!(acnh_saves_has_save());

        // SAFETY: test-only: the host table lives on this test's stack and outlives every call that reads it.
        unsafe { acnh_saves_set_host(std::ptr::null()) };
    }

    #[test]
    fn zip_safe_name_rejects_traversal() {
        assert!(safe_name("main.dat").is_some());
        assert!(safe_name("Villager0\\personal.dat").is_some());
        assert!(safe_name("/etc/passwd").is_none());
        assert!(safe_name("../evil").is_none());
        assert!(safe_name("a/../b").is_none());
        assert!(safe_name("C:evil").is_none());
    }
}
