//! Save data management, local Switch user profiles, and `acc:u0`/`acc:u1` account helpers,
//! ported from `runtime/src/saves.cpp`, `runtime/src/users.cpp`, and
//! `runtime/src/svc_account.cpp`.
//!
//! `runtime/src/saves_wrapper.cpp` provides the thin C++ host callbacks (`data_root`, `log`,
//! `local_from_posix`, `zip_inflate`), forwards `nx::saves::*` and `nx::user_*` C++ APIs, and
//! registers `acc:u0` / `acc:u1` and `IStorage` / `IStorageAccessor` against the guest IPC
//! dispatcher.

use std::collections::BTreeSet;
use std::ffi::{c_char, c_void, CStr, CString};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
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

const TITLE_ID_UPPER: &str = "01006F8002326000";
const TITLE_ID_LOWER: &str = "01006f8002326000";
const KEEP_BACKUPS: usize = 10;
const KEEP_AUTO_BACKUPS: usize = 3;
const MAX_ZIP_BYTES: u64 = 1 << 30;

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

pub type AcnhSavesStringCb = Option<extern "C" fn(*const c_char, usize, *mut c_void)>;
pub type AcnhSavesFoundCb =
    Option<extern "C" fn(*const c_char, usize, *const c_char, usize, *mut c_void)>;
pub type AcnhSavesUserCb = Option<extern "C" fn(*const AcnhUserInfo, *mut c_void)>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct UserRecord {
    lo: u64,
    hi: u64,
    nickname: String,
}

impl UserRecord {
    fn valid(&self) -> bool {
        self.lo != 0 || self.hi != 0
    }

    fn to_ffi(&self) -> AcnhUserInfo {
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
struct UsersState {
    users: Vec<UserRecord>,
    loaded: bool,
    last: Option<UserRecord>,
}

static HOST: OnceLock<Mutex<AcnhSavesHost>> = OnceLock::new();
static SAVES_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static USERS_LOCK: OnceLock<Mutex<UsersState>> = OnceLock::new();

fn host_lock() -> &'static Mutex<AcnhSavesHost> {
    HOST.get_or_init(|| Mutex::new(AcnhSavesHost::default()))
}

fn current_host() -> AcnhSavesHost {
    *host_lock().lock().unwrap_or_else(|e| e.into_inner())
}

fn saves_lock() -> &'static Mutex<()> {
    SAVES_LOCK.get_or_init(|| Mutex::new(()))
}

fn users_lock() -> &'static Mutex<UsersState> {
    USERS_LOCK.get_or_init(|| Mutex::new(UsersState::default()))
}

fn call_host_string(f: Option<extern "C" fn(*mut c_char, usize) -> usize>) -> Option<String> {
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

fn host_data_root() -> String {
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

fn host_log(msg: &str) {
    if let Some(log_fn) = current_host().log {
        if let Ok(c) = CString::new(msg) {
            log_fn(c.as_ptr());
        }
    }
}

fn cstr_opt(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: caller guarantees `ptr` is a valid NUL-terminated C string when non-null.
    let s = unsafe { CStr::from_ptr(ptr) };
    Some(s.to_string_lossy().into_owned())
}

fn emit_string(cb: AcnhSavesStringCb, ctx: *mut c_void, s: &str) {
    if let Some(cb) = cb {
        cb(s.as_ptr().cast::<c_char>(), s.len(), ctx);
    }
}

fn set_err(out: &mut String, msg: impl Into<String>) {
    *out = msg.into();
}

fn path_utf8(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn user_dir(data_root: &str) -> PathBuf {
    PathBuf::from(data_root).join("user")
}

fn device_dir(data_root: &str) -> PathBuf {
    user_dir(data_root).join("device_save")
}

fn backups_root(data_root: &str) -> PathBuf {
    user_dir(data_root).join("backups")
}

fn unix_now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

fn posix_to_utc_local(posix: i64) -> AcnhSavesLocalTime {
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

fn local_time_from_posix(posix: i64) -> AcnhSavesLocalTime {
    if let Some(f) = current_host().local_from_posix {
        let mut lt = AcnhSavesLocalTime::default();
        if f(posix, &mut lt) {
            return lt;
        }
    }
    posix_to_utc_local(posix)
}

fn stamp_now() -> String {
    let lt = local_time_from_posix(unix_now_seconds());
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        lt.year, lt.month, lt.day, lt.hour, lt.minute, lt.second
    )
}

// ---- save_storage equivalents ---------------------------------------------------------------

fn flat_save(d: &Path, why: Option<&mut String>) -> bool {
    if !d.is_dir() {
        if let Some(w) = why {
            *w = String::from("folder not found");
        }
        return false;
    }
    if !d.join("main.dat").is_file() {
        if let Some(w) = why {
            *w = String::from("main.dat missing");
        }
        return false;
    }
    if !d.join("mainHeader.dat").is_file() {
        if let Some(w) = why {
            *w = String::from("mainHeader.dat missing");
        }
        return false;
    }
    if !d.join("Villager0").join("personal.dat").is_file() {
        if let Some(w) = why {
            *w = String::from("Villager0/personal.dat missing");
        }
        return false;
    }
    true
}

fn non_empty_dir(d: &Path) -> bool {
    if !d.is_dir() {
        return false;
    }
    match fs::read_dir(d) {
        Ok(mut rd) => rd.next().is_some(),
        Err(_) => false,
    }
}

fn recover(data_root: &str) {
    let dir = device_dir(data_root);
    let old = PathBuf::from(format!("{}.old", path_utf8(&dir)));
    if !dir.exists() && old.is_dir() && fs::rename(&old, &dir).is_ok() {
        host_log(&format!(
            "save_storage: restored {} after an interrupted swap",
            path_utf8(&dir)
        ));
    }
}

fn swap_in(data_root: &str, staging: &Path, err: &mut String) -> bool {
    let dir = device_dir(data_root);
    let old = PathBuf::from(format!("{}.old", path_utf8(&dir)));
    let _ = fs::remove_dir_all(&old);
    let had = dir.exists();
    if had {
        if let Err(e) = fs::rename(&dir, &old) {
            set_err(err, format!("cannot move the current save aside: {e}"));
            return false;
        }
    }
    if let Err(e) = fs::rename(staging, &dir) {
        if had {
            let _ = fs::rename(&old, &dir);
        }
        set_err(err, format!("cannot move the new save in: {e}"));
        return false;
    }
    let _ = fs::remove_dir_all(&old);
    true
}

fn copy_tree_inner(src: &Path, dst: &Path, err: &mut String) -> bool {
    if let Err(e) = fs::create_dir_all(dst) {
        set_err(err, format!("cannot create {}: {e}", path_utf8(dst)));
        return false;
    }
    let entries = match fs::read_dir(src) {
        Ok(rd) => rd,
        Err(e) => {
            set_err(err, format!("cannot read {}: {e}", path_utf8(src)));
            return false;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                set_err(err, format!("cannot read {}: {e}", path_utf8(src)));
                return false;
            }
        };
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                set_err(err, format!("cannot copy {}: {e}", path_utf8(&path)));
                return false;
            }
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        let to = dst.join(entry.file_name());
        if meta.is_dir() {
            if !copy_tree_inner(&path, &to, err) {
                return false;
            }
        } else if meta.is_file() {
            if let Err(e) = fs::copy(&path, &to) {
                set_err(err, format!("cannot copy {}: {e}", path_utf8(&path)));
                return false;
            }
        }
    }
    true
}

fn island_display_name(save: &Path) -> String {
    let land = save.join("landname.dat");
    let mut f = match File::open(&land) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let mut buf = [0u8; 20];
    let n = f.read(&mut buf).unwrap_or(0) / 2;
    let mut units = Vec::with_capacity(n);
    for i in 0..n {
        let c = u16::from_le_bytes([buf[2 * i], buf[2 * i + 1]]);
        if c == 0 {
            break;
        }
        units.push(c);
    }
    String::from_utf16_lossy(&units)
}

fn last_modified_str(device_save: &Path) -> String {
    let meta = match fs::metadata(device_save.join("main.dat")) {
        Ok(m) => m,
        Err(_) => return String::new(),
    };
    let modified = match meta.modified() {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    let posix = match modified.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs().min(i64::MAX as u64) as i64,
        Err(e) => -(e.duration().as_secs().min(i64::MAX as u64) as i64),
    };
    let lt = local_time_from_posix(posix);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        lt.year, lt.month, lt.day, lt.hour, lt.minute
    )
}

// ---- saves helpers --------------------------------------------------------------------------

fn subdirs(d: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(d) else {
        return out;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            out.push(p);
        }
    }
    out.sort();
    out
}

fn island_folder(save: &Path) -> String {
    let mut n: String = island_display_name(save)
        .chars()
        .map(|ch| {
            if (ch as u32) < 0x20
                || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
            {
                '_'
            } else {
                ch
            }
        })
        .collect();
    while n.ends_with(' ') || n.ends_with('.') {
        n.pop();
    }
    while n.starts_with(' ') {
        n.remove(0);
    }
    if n.is_empty() {
        return String::from("Unnamed island");
    }
    const RESERVED: [&str; 22] = [
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    let lower = n.to_ascii_lowercase();
    if RESERVED.contains(&lower.as_str()) {
        return format!("{n}_");
    }
    n
}

fn landname_hash(save: &Path) -> u32 {
    let mut h: u32 = 2_166_136_261;
    if let Ok(bytes) = fs::read(save.join("landname.dat")) {
        for b in bytes {
            h = (h ^ u32::from(b)).wrapping_mul(16_777_619);
        }
    }
    h
}

fn island_dir_for(data_root: &str, save: &Path) -> PathBuf {
    backups_root(data_root).join(format!(
        "{} {:08x}",
        island_folder(save),
        landname_hash(save)
    ))
}

fn backups_dir(data_root: &str) -> PathBuf {
    island_dir_for(data_root, &device_dir(data_root))
}

fn unique_in(dir: &Path, base: &str, ext: &str) -> PathBuf {
    let mut p = dir.join(format!("{base}{ext}"));
    for n in 2..1000 {
        if !p.exists() {
            break;
        }
        p = dir.join(format!("{base}-{n}{ext}"));
    }
    p
}

fn find_root(d: &Path, err: &mut String) -> Option<PathBuf> {
    let mut own_why = String::new();
    if flat_save(d, Some(&mut own_why)) {
        return Some(d.to_path_buf());
    }
    if !d.is_dir() {
        set_err(err, own_why);
        return None;
    }
    let mut found = Vec::new();
    for a in subdirs(d) {
        if flat_save(&a, None) {
            found.push(a);
            continue;
        }
        for b in subdirs(&a) {
            if flat_save(&b, None) {
                found.push(b);
            }
        }
    }
    if found.len() == 2
        && found[0].file_name().and_then(|s| s.to_str()) == Some("0")
        && found[1].file_name().and_then(|s| s.to_str()) == Some("1")
        && found[0].parent() == found[1].parent()
    {
        found.pop();
    }
    if found.is_empty() {
        set_err(err, own_why);
        return None;
    }
    if found.len() > 1 {
        set_err(
            err,
            format!("{} saves in this folder: choose one of them", found.len()),
        );
        return None;
    }
    Some(found.remove(0))
}

// ---- zip ------------------------------------------------------------------------------------

fn rd16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn rd32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn wr16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn wr32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn crc32_of(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if (c & 1) != 0 {
                0xEDB8_8320u32 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    c ^ 0xFFFF_FFFFu32
}

struct ZipEntry {
    name: String,
    flags: u16,
    method: u16,
    crc: u32,
    csize: u32,
    usize: u32,
    offset: u32,
}

fn safe_name(raw: &str) -> Option<String> {
    let normalized = raw.replace('\\', "/");
    if normalized.is_empty() || normalized.starts_with('/') || normalized.contains(':') {
        return None;
    }
    if normalized.split('/').any(|seg| seg == "..") {
        return None;
    }
    Some(normalized)
}

fn read_exact_at(f: &mut File, off: u64, len: usize) -> Option<Vec<u8>> {
    f.seek(SeekFrom::Start(off)).ok()?;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn zip_list(zip: &Path, err: &mut String) -> Option<Vec<ZipEntry>> {
    let mut f = match File::open(zip) {
        Ok(f) => f,
        Err(_) => {
            set_err(err, format!("cannot open {}", path_utf8(zip)));
            return None;
        }
    };
    let size = match f.seek(SeekFrom::End(0)) {
        Ok(s) => s,
        Err(_) => {
            set_err(err, "cannot read the zip file");
            return None;
        }
    };
    if size < 22 {
        set_err(err, "not a zip file");
        return None;
    }
    let tail = size.min(22 + 0xFFFF) as usize;
    let Some(buf) = read_exact_at(&mut f, size - tail as u64, tail) else {
        set_err(err, "cannot read the zip file");
        return None;
    };
    let mut eocd = None;
    for i in (0..=tail - 22).rev() {
        if rd32(&buf[i..]) == 0x0605_4b50 {
            eocd = Some(i);
            break;
        }
    }
    let Some(eocd) = eocd else {
        set_err(err, "not a zip file");
        return None;
    };
    let count = rd16(&buf[eocd + 10..]);
    let cd_size = rd32(&buf[eocd + 12..]);
    let cd_off = rd32(&buf[eocd + 16..]);
    if count == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_off == 0xFFFF_FFFF {
        set_err(err, "zip64 not supported");
        return None;
    }
    if u64::from(cd_off) + u64::from(cd_size) > size {
        set_err(err, "damaged zip file");
        return None;
    }
    let Some(cd) = read_exact_at(&mut f, u64::from(cd_off), cd_size as usize) else {
        set_err(err, "cannot read the zip file");
        return None;
    };
    let mut pos = 0usize;
    let mut out = Vec::new();
    for _ in 0..count {
        if pos + 46 > cd.len() || rd32(&cd[pos..]) != 0x0201_4b50 {
            set_err(err, "damaged zip file");
            return None;
        }
        let flags = rd16(&cd[pos + 8..]);
        let method = rd16(&cd[pos + 10..]);
        let crc = rd32(&cd[pos + 16..]);
        let csize = rd32(&cd[pos + 20..]);
        let usize_val = rd32(&cd[pos + 24..]);
        let nlen = usize::from(rd16(&cd[pos + 28..]));
        let xlen = usize::from(rd16(&cd[pos + 30..]));
        let clen = usize::from(rd16(&cd[pos + 32..]));
        let offset = rd32(&cd[pos + 42..]);
        if pos + 46 + nlen + xlen + clen > cd.len() {
            set_err(err, "damaged zip file");
            return None;
        }
        let raw_name = String::from_utf8_lossy(&cd[pos + 46..pos + 46 + nlen]).into_owned();
        pos += 46 + nlen + xlen + clen;
        let Some(name) = safe_name(&raw_name) else {
            set_err(err, format!("unsafe path in zip: {raw_name}"));
            return None;
        };
        if name.ends_with('/') || name.starts_with("__MACOSX/") {
            continue;
        }
        out.push(ZipEntry {
            name,
            flags,
            method,
            crc,
            csize,
            usize: usize_val,
            offset,
        });
    }
    Some(out)
}

fn zip_find_root(entries: &[ZipEntry], why: &mut String) -> Option<String> {
    let names: BTreeSet<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    let mut found = Vec::new();
    let mut any_main = false;
    for &n in &names {
        let Some(p) = n.strip_suffix("main.dat") else {
            continue;
        };
        if !p.is_empty() && !p.ends_with('/') {
            continue;
        }
        if p.bytes().filter(|&b| b == b'/').count() > 2 {
            continue;
        }
        any_main = true;
        let hdr = format!("{p}mainHeader.dat");
        let vil = format!("{p}Villager0/personal.dat");
        if names.contains(hdr.as_str()) && names.contains(vil.as_str()) {
            found.push(p.to_string());
        }
    }
    if found.is_empty() {
        if !any_main {
            set_err(why, "main.dat missing");
        } else {
            set_err(why, "mainHeader.dat or Villager0/personal.dat missing");
        }
        return None;
    }
    if found.len() > 1 {
        set_err(
            why,
            format!("{} saves in this zip: it must hold one", found.len()),
        );
        return None;
    }
    Some(found.remove(0))
}

fn zip_inflate_host(compressed: &[u8], out_len: usize, err: &mut String) -> Option<Vec<u8>> {
    let Some(inflate_fn) = current_host().zip_inflate else {
        set_err(err, "zip not supported in this build");
        return None;
    };
    let mut plain = vec![0u8; out_len];
    let mut err_buf = vec![0u8; 256];
    let ok = inflate_fn(
        compressed.as_ptr(),
        compressed.len(),
        plain.as_mut_ptr(),
        plain.len(),
        err_buf.as_mut_ptr().cast::<c_char>(),
        err_buf.len(),
    );
    if !ok {
        let nul = err_buf
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(err_buf.len());
        let msg = String::from_utf8_lossy(&err_buf[..nul]).into_owned();
        set_err(
            err,
            if msg.is_empty() {
                String::from("damaged zip file (inflate)")
            } else {
                msg
            },
        );
        return None;
    }
    Some(plain)
}

fn zip_extract(
    zip: &Path,
    entries: &[ZipEntry],
    prefix: &str,
    dest: &Path,
    err: &mut String,
) -> bool {
    let mut f = match File::open(zip) {
        Ok(f) => f,
        Err(_) => {
            set_err(err, format!("cannot open {}", path_utf8(zip)));
            return false;
        }
    };
    let mut total = 0u64;
    for e in entries {
        let Some(rel) = e.name.strip_prefix(prefix) else {
            continue;
        };
        if (e.flags & 1) != 0 {
            set_err(err, "encrypted zip entries are not supported");
            return false;
        }
        if e.method != 0 && e.method != 8 {
            set_err(err, format!("unsupported zip compression in {}", e.name));
            return false;
        }
        total += u64::from(e.usize);
        if total > MAX_ZIP_BYTES {
            set_err(err, "zip too large for a save");
            return false;
        }
        let Some(head) = read_exact_at(&mut f, u64::from(e.offset), 30) else {
            set_err(err, "damaged zip file");
            return false;
        };
        if rd32(&head) != 0x0403_4b50 {
            set_err(err, "damaged zip file");
            return false;
        }
        let at =
            u64::from(e.offset) + 30 + u64::from(rd16(&head[26..])) + u64::from(rd16(&head[28..]));
        let Some(data) = read_exact_at(&mut f, at, e.csize as usize) else {
            set_err(err, "damaged zip file");
            return false;
        };
        let plain = if e.method == 0 {
            if e.csize != e.usize {
                set_err(err, "damaged zip file");
                return false;
            }
            data
        } else {
            let Some(p) = zip_inflate_host(&data, e.usize as usize, err) else {
                return false;
            };
            p
        };
        if crc32_of(&plain) != e.crc {
            set_err(
                err,
                format!("damaged zip file (CRC mismatch in {})", e.name),
            );
            return false;
        }
        let out = dest.join(rel);
        if let Some(parent) = out.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if fs::write(&out, &plain).is_err() {
            set_err(err, format!("cannot write {}", path_utf8(&out)));
            return false;
        }
    }
    true
}

fn collect_files_sorted(root: &Path, cur: &Path, out: &mut Vec<PathBuf>, err: &mut String) -> bool {
    let entries = match fs::read_dir(cur) {
        Ok(rd) => rd,
        Err(e) => {
            set_err(err, format!("cannot read {}: {e}", path_utf8(root)));
            return false;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                set_err(err, format!("cannot read {}: {e}", path_utf8(root)));
                return false;
            }
        };
        let p = entry.path();
        if p.is_dir() {
            if !collect_files_sorted(root, &p, out, err) {
                return false;
            }
        } else if p.is_file() {
            out.push(p);
        }
    }
    true
}

fn zip_write(src: &Path, zip: &Path, err: &mut String) -> bool {
    let mut files = Vec::new();
    if !collect_files_sorted(src, src, &mut files, err) {
        return false;
    }
    files.sort();

    let lt = local_time_from_posix(unix_now_seconds());
    let dos_time = ((lt.hour as u16) << 11) | ((lt.minute as u16) << 5) | ((lt.second as u16) / 2);
    let dos_date =
        (((lt.year - 1980).max(0) as u16) << 9) | ((lt.month as u16) << 5) | (lt.day as u16);

    let mut o = match File::create(zip) {
        Ok(f) => f,
        Err(_) => {
            set_err(err, format!("cannot write {}", path_utf8(zip)));
            return false;
        }
    };

    let mut central = Vec::new();
    let mut head = Vec::new();
    let mut offset = 0u64;
    for p in &files {
        let rel = match p.strip_prefix(src) {
            Ok(r) => r,
            Err(_) => {
                set_err(err, format!("cannot read {}", path_utf8(p)));
                return false;
            }
        };
        let name = rel.to_string_lossy().replace('\\', "/");
        let data = match fs::read(p) {
            Ok(d) => d,
            Err(_) => {
                set_err(err, format!("cannot read {}", path_utf8(p)));
                return false;
            }
        };
        if data.len() as u64 >= 0xFFFF_FFFF || offset >= 0xFFFF_FFFF {
            set_err(err, "save too large for a zip");
            return false;
        }
        let crc = crc32_of(&data);
        let dlen = data.len() as u32;
        let nlen = name.len() as u16;

        head.clear();
        wr32(&mut head, 0x0403_4b50);
        wr16(&mut head, 20);
        wr16(&mut head, 1 << 11);
        wr16(&mut head, 0);
        wr16(&mut head, dos_time);
        wr16(&mut head, dos_date);
        wr32(&mut head, crc);
        wr32(&mut head, dlen);
        wr32(&mut head, dlen);
        wr16(&mut head, nlen);
        wr16(&mut head, 0);

        wr32(&mut central, 0x0201_4b50);
        wr16(&mut central, 20);
        wr16(&mut central, 20);
        wr16(&mut central, 1 << 11);
        wr16(&mut central, 0);
        wr16(&mut central, dos_time);
        wr16(&mut central, dos_date);
        wr32(&mut central, crc);
        wr32(&mut central, dlen);
        wr32(&mut central, dlen);
        wr16(&mut central, nlen);
        wr16(&mut central, 0);
        wr16(&mut central, 0);
        wr16(&mut central, 0);
        wr16(&mut central, 0);
        wr32(&mut central, 0);
        wr32(&mut central, offset as u32);
        central.extend_from_slice(name.as_bytes());

        if o.write_all(&head).is_err()
            || o.write_all(name.as_bytes()).is_err()
            || o.write_all(&data).is_err()
        {
            set_err(err, format!("cannot write {}", path_utf8(zip)));
            return false;
        }
        offset += (head.len() + name.len() + data.len()) as u64;
    }
    if files.len() > 0xFFFF || offset > 0xFFFF_FFFF {
        set_err(err, "save too large for a zip");
        return false;
    }
    let mut end = Vec::new();
    let count = files.len() as u16;
    wr32(&mut end, 0x0605_4b50);
    wr16(&mut end, 0);
    wr16(&mut end, 0);
    wr16(&mut end, count);
    wr16(&mut end, count);
    wr32(&mut end, central.len() as u32);
    wr32(&mut end, offset as u32);
    wr16(&mut end, 0);

    if o.write_all(&central).is_err() || o.write_all(&end).is_err() || o.flush().is_err() {
        set_err(err, format!("cannot write {}", path_utf8(zip)));
        return false;
    }
    true
}

// ---- backups --------------------------------------------------------------------------------

fn backup_name(n: &str) -> bool {
    let b = n.as_bytes();
    if b.len() < 15 || b[8] != b'-' || n.contains(".tmp") {
        return false;
    }
    b[..15]
        .iter()
        .enumerate()
        .all(|(i, &c)| i == 8 || c.is_ascii_digit())
}

fn auto_backup(n: &str) -> bool {
    n.len() > 5 && n.ends_with(".auto")
}

fn parse_suffix(n: &str) -> i32 {
    if n.len() <= 16 {
        return 0;
    }
    let rest = &n[16..];
    let digits_end = rest
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..digits_end].parse::<i32>().unwrap_or(0)
}

fn backups_sorted(dir: &Path) -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                    if backup_name(name) {
                        v.push(name.to_string());
                    }
                }
            }
        }
    }
    v.sort_by(|a, b| {
        let ka = (&a[..15], parse_suffix(a));
        let kb = (&b[..15], parse_suffix(b));
        kb.cmp(&ka)
    });
    v
}

fn migrate_flat_backups(data_root: &str) {
    let root = backups_root(data_root);
    let mut flat = Vec::new();
    if let Ok(rd) = fs::read_dir(&root) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                    if backup_name(name) {
                        flat.push(p);
                    }
                }
            }
        }
    }
    for p in flat {
        let Some(fname) = p.file_name() else {
            continue;
        };
        let dest = island_dir_for(data_root, &p).join(fname);
        if let Some(parent) = dest.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if !dest.exists() && fs::rename(&p, &dest).is_err() {
            host_log(&format!(
                "saves: cannot move the backup {} into its island folder",
                path_utf8(&p)
            ));
        }
    }
}

fn backup_locked(data_root: &str, err: &mut String, automatic: bool) -> String {
    let dir = device_dir(data_root);
    if !non_empty_dir(&dir) {
        set_err(err, "no save to back up");
        return String::new();
    }
    let bdir = backups_dir(data_root);
    let _ = fs::create_dir_all(&bdir);
    let base = stamp_now();
    let mut next = 0i32;
    for n in backups_sorted(&bdir) {
        if n.starts_with(&base) {
            let candidate = if n.len() > 16 && n.as_bytes()[15] == b'-' {
                parse_suffix(&n)
            } else {
                1
            };
            next = next.max(candidate);
        }
    }
    let stem = if next > 0 {
        format!("{base}-{}", next + 1)
    } else {
        base
    };
    let final_path = unique_in(&bdir, &stem, if automatic { ".auto" } else { "" });
    let tmp = PathBuf::from(format!("{}.tmp", path_utf8(&final_path)));
    let _ = fs::remove_dir_all(&tmp);
    if !copy_tree_inner(&dir, &tmp, err) {
        let _ = fs::remove_dir_all(&tmp);
        return String::new();
    }
    if let Err(e) = fs::rename(&tmp, &final_path) {
        let _ = fs::remove_dir_all(&tmp);
        set_err(err, format!("cannot finish the backup: {e}"));
        return String::new();
    }
    let name = format!(
        "{}/{}",
        bdir.file_name().and_then(|s| s.to_str()).unwrap_or(""),
        final_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
    );
    let mut kept = 0usize;
    let limit = if automatic {
        KEEP_AUTO_BACKUPS
    } else {
        KEEP_BACKUPS
    };
    for n in backups_sorted(&bdir) {
        if auto_backup(&n) == automatic {
            kept += 1;
            if kept > limit {
                let _ = fs::remove_dir_all(bdir.join(&n));
            }
        }
    }
    if let Ok(rd) = fs::read_dir(&bdir) {
        for entry in rd.flatten() {
            if let Some(fname) = entry.file_name().to_str() {
                if fname.contains(".tmp") {
                    let _ = fs::remove_dir_all(entry.path());
                }
            }
        }
    }
    host_log(&format!("saves: backup {name}"));
    name
}

fn all_backups(data_root: &str) -> Vec<String> {
    migrate_flat_backups(data_root);
    let mut dirs: Vec<PathBuf> = subdirs(&backups_root(data_root))
        .into_iter()
        .filter(|d| {
            d.file_name()
                .and_then(|s| s.to_str())
                .map(|n| !backup_name(n))
                .unwrap_or(false)
        })
        .collect();
    let current = backups_dir(data_root);
    dirs.sort_by_key(|d| usize::from(*d != current));
    let mut v = Vec::new();
    for d in dirs {
        let folder = d.file_name().and_then(|s| s.to_str()).unwrap_or("");
        for n in backups_sorted(&d) {
            v.push(format!("{folder}/{n}"));
        }
    }
    v
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

fn import_locked(data_root: &str, from: &Path, err: &mut String) -> bool {
    recover(data_root);
    let staging = PathBuf::from(format!("{}.new", path_utf8(&device_dir(data_root))));
    let _ = fs::create_dir_all(user_dir(data_root));
    let _ = fs::remove_dir_all(&staging);
    if from.is_file() {
        let Some(entries) = zip_list(from, err) else {
            return false;
        };
        let Some(prefix) = zip_find_root(&entries, err) else {
            return false;
        };
        if !zip_extract(from, &entries, &prefix, &staging, err) {
            let _ = fs::remove_dir_all(&staging);
            return false;
        }
    } else {
        let Some(root) = find_root(from, err) else {
            return false;
        };
        if same_path(&root, &device_dir(data_root)) {
            set_err(err, "this is the current save");
            return false;
        }
        if !copy_tree_inner(&root, &staging, err) {
            let _ = fs::remove_dir_all(&staging);
            return false;
        }
    }
    if !flat_save(&staging, Some(err)) {
        let _ = fs::remove_dir_all(&staging);
        return false;
    }
    if non_empty_dir(&device_dir(data_root)) {
        let mut berr = String::new();
        if backup_locked(data_root, &mut berr, false).is_empty() {
            let _ = fs::remove_dir_all(&staging);
            set_err(err, format!("cannot back up the current save: {berr}"));
            return false;
        }
    }
    if !swap_in(data_root, &staging, err) {
        let _ = fs::remove_dir_all(&staging);
        return false;
    }
    host_log(&format!("saves: imported {}", path_utf8(from)));
    true
}

// ---- detection ------------------------------------------------------------------------------

struct Scan {
    out: Vec<(String, String)>,
    seen: BTreeSet<String>,
    own: Option<PathBuf>,
}

impl Scan {
    fn add(&mut self, label: &str, p: &Path) {
        let c = fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        if let Some(own) = &self.own {
            if c == *own {
                return;
            }
        }
        if !self.seen.insert(path_utf8(&c)) {
            return;
        }
        self.out.push((label.to_string(), path_utf8(p)));
    }
}

fn scan_yuzu(s: &mut Scan, root: &Path, label: &str) {
    let save = root.join("nand").join("user").join("save");
    if !save.is_dir() {
        return;
    }
    let titles = [TITLE_ID_UPPER, TITLE_ID_LOWER];
    for t in titles {
        let p = save.join("device").join(t).join("0");
        if flat_save(&p, None) {
            s.add(label, &p);
        }
    }
    let zero_user = save
        .join("0000000000000000")
        .join("00000000000000000000000000000000");
    for t in titles {
        let p = zero_user.join(t);
        if flat_save(&p, None) {
            s.add(label, &p);
        }
    }
    for user in subdirs(&save.join("0000000000000000")) {
        for t in titles {
            let p = user.join(t);
            if flat_save(&p, None) {
                s.add(label, &p);
            }
        }
    }
}

fn scan_ryujinx(s: &mut Scan, root: &Path, label: &str) {
    let save = root.join("bis").join("user").join("save");
    for d in subdirs(&save) {
        if d.file_name().and_then(|n| n.to_str()).map(|n| n.len()) != Some(16) {
            continue;
        }
        let p0 = d.join("0");
        let p1 = d.join("1");
        if flat_save(&p0, None) {
            s.add(label, &p0);
        } else if flat_save(&p1, None) {
            s.add(label, &p1);
        }
    }
}

fn ends_with_zip(s: &str) -> bool {
    s.len() >= 4 && s[s.len() - 4..].eq_ignore_ascii_case(".zip")
}

fn zip_is_save(zip: &Path) -> bool {
    let mut err = String::new();
    let Some(entries) = zip_list(zip, &mut err) else {
        return false;
    };
    zip_find_root(&entries, &mut err).is_some()
}

fn mtime(p: &Path) -> SystemTime {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .unwrap_or(UNIX_EPOCH)
}

fn add_newest_backup(s: &mut Scan, title_dir: &Path, label: &str) {
    let mut best: Option<PathBuf> = None;
    let mut best_time = UNIX_EPOCH;
    let mut consider = |p: &Path| -> bool {
        let t = if p.is_dir() && flat_save(p, None) {
            mtime(&p.join("main.dat"))
        } else if p.is_file()
            && p.file_name()
                .and_then(|n| n.to_str())
                .map(ends_with_zip)
                .unwrap_or(false)
            && zip_is_save(p)
        {
            mtime(p)
        } else {
            return false;
        };
        if best.is_none() || t > best_time {
            best = Some(p.to_path_buf());
            best_time = t;
        }
        true
    };
    if let Ok(rd) = fs::read_dir(title_dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if consider(&p) || !p.is_dir() {
                continue;
            }
            if let Ok(rd2) = fs::read_dir(&p) {
                for e2 in rd2.flatten() {
                    consider(&e2.path());
                }
            }
        }
    }
    if let Some(b) = best {
        s.add(label, &b);
    }
}

fn scan_volume(s: &mut Scan, v: &Path, name: &str) {
    for t in subdirs(&v.join("JKSV")) {
        let n = t
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if n.contains("animal crossing") || n.contains(TITLE_ID_LOWER) {
            add_newest_backup(s, &t, &format!("JKSV ({name})"));
        }
    }
    let cp_prefix = format!("0x{TITLE_ID_LOWER}");
    for t in subdirs(&v.join("switch").join("Checkpoint").join("saves")) {
        let n = t
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if n.starts_with(&cp_prefix) {
            add_newest_backup(s, &t, &format!("Checkpoint ({name})"));
        }
    }
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    std::env::var(key)
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn volumes() -> Vec<PathBuf> {
    let mut v = Vec::new();
    #[cfg(windows)]
    {
        for d in b'D'..=b'Z' {
            let p = PathBuf::from(format!("{}:\\", d as char));
            if p.is_dir() {
                v.push(p);
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        v.extend(subdirs(Path::new("/Volumes")));
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let user = std::env::var("USER").unwrap_or_default();
        if !user.is_empty() {
            v.extend(subdirs(&PathBuf::from(format!("/media/{user}"))));
            v.extend(subdirs(&PathBuf::from(format!("/run/media/{user}"))));
        }
        for base in ["/media", "/mnt"] {
            for p in subdirs(Path::new(base)) {
                if p.file_name().and_then(|s| s.to_str()) != Some(user.as_str()) {
                    v.push(p);
                }
            }
        }
    }
    v
}

fn scan_emu_base(s: &mut Scan, base: &Path, suffix: &str) {
    const EMUS: [(&str, &str, bool); 7] = [
        ("yuzu", "Yuzu", false),
        ("suyu", "Suyu", false),
        ("sudachi", "Sudachi", false),
        ("citron", "Citron", false),
        ("eden", "Eden", false),
        ("torzu", "Torzu", false),
        ("Ryujinx", "Ryujinx", true),
    ];
    for (folder, label, ryujinx) in EMUS {
        let root = base.join(folder);
        if !root.is_dir() {
            continue;
        }
        let full_label = format!("{label}{suffix}");
        if ryujinx {
            scan_ryujinx(s, &root, &full_label);
        } else {
            scan_yuzu(s, &root, &full_label);
        }
    }
}

fn scan_recomps(s: &mut Scan) {
    let Some(h) = home_dir() else {
        return;
    };
    for sub in [
        "",
        "Downloads",
        "Desktop",
        "Documents",
        "Games",
        "Applications",
        "Projects",
    ] {
        let base = if sub.is_empty() {
            h.clone()
        } else {
            h.join(sub)
        };
        for d in subdirs(&base) {
            let fname = d.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !fname.to_ascii_lowercase().contains("acnh") {
                continue;
            }
            let p = d
                .join("private")
                .join("acnh")
                .join("user")
                .join("device_save");
            if flat_save(&p, None) {
                s.add(&format!("ACNHRecomp ({fname})"), &p);
            }
        }
    }
}

// ---- users ----------------------------------------------------------------------------------

fn users_path(data_root: &str) -> PathBuf {
    PathBuf::from(data_root).join("users.txt")
}

fn hex_uid(lo: u64, hi: u64) -> String {
    format!("{hi:016x}{lo:016x}")
}

fn save_users_locked(data_root: &str, users: &[UserRecord]) {
    let mut out = String::new();
    for u in users {
        out.push_str(&format!("{} {}\n", hex_uid(u.lo, u.hi), u.nickname));
    }
    let _ = fs::write(users_path(data_root), out);
}

fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

fn default_user_name() -> String {
    for key in ["NX_USER_NAME", "USER", "USERNAME"] {
        if let Ok(v) = std::env::var(key) {
            if !v.is_empty() {
                return truncate_utf8(&v, 10);
            }
        }
    }
    String::from("Player")
}

fn load_users_locked(data_root: &str, st: &mut UsersState) {
    if st.loaded {
        return;
    }
    st.loaded = true;
    let path = users_path(data_root);
    if let Ok(contents) = fs::read_to_string(&path) {
        for raw_line in contents.lines() {
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            if line.len() < 33 || line.as_bytes()[32] != b' ' {
                continue;
            }
            let Ok(hi) = u64::from_str_radix(&line[..16], 16) else {
                continue;
            };
            let Ok(lo) = u64::from_str_radix(&line[16..32], 16) else {
                continue;
            };
            let nickname = truncate_utf8(&line[33..], 31);
            let rec = UserRecord { lo, hi, nickname };
            if rec.valid() && st.users.len() < ACNH_MAX_USERS {
                st.users.push(rec);
            }
        }
    }
    if st.users.is_empty() {
        st.users.push(UserRecord {
            lo: ACNH_FIRST_USER_LO,
            hi: ACNH_FIRST_USER_HI,
            nickname: default_user_name(),
        });
        save_users_locked(data_root, &st.users);
        host_log(&format!(
            "users: created {} with the first user",
            path_utf8(&path)
        ));
    }
    for u in &st.users {
        host_log(&format!("users: {} {}", hex_uid(u.lo, u.hi), u.nickname));
    }
}

fn users_all_vec() -> Vec<UserRecord> {
    let data_root = host_data_root();
    let mut st = users_lock().lock().unwrap_or_else(|e| e.into_inner());
    load_users_locked(&data_root, &mut st);
    st.users.clone()
}

fn user_last_opened_rec() -> UserRecord {
    let all = users_all_vec();
    let st = users_lock().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(last) = &st.last {
        return last.clone();
    }
    if let Ok(e) = std::env::var("NX_USER") {
        if let Ok(n) = e.trim().parse::<usize>() {
            if n >= 1 && n <= all.len() {
                return all[n - 1].clone();
            }
        }
    }
    all.into_iter().next().unwrap_or(UserRecord {
        lo: ACNH_FIRST_USER_LO,
        hi: ACNH_FIRST_USER_HI,
        nickname: String::from("Player"),
    })
}

// ---- FFI exports ----------------------------------------------------------------------------

/// Registers the C++ host callbacks for saves and users.
///
/// # Safety
/// When non-null, `host` must point to a valid `AcnhSavesHost`.
#[no_mangle]
pub unsafe extern "C" fn acnh_saves_set_host(host: *const AcnhSavesHost) {
    let mut guard = host_lock().lock().unwrap_or_else(|e| e.into_inner());
    if host.is_null() {
        *guard = AcnhSavesHost::default();
    } else {
        // SAFETY: caller guarantees `host` points to a valid `AcnhSavesHost`.
        *guard = unsafe { *host };
    }
}

/// Returns `<data root>/user/device_save` via `cb`.
#[no_mangle]
pub extern "C" fn acnh_saves_save_dir(cb: AcnhSavesStringCb, ctx: *mut c_void) {
    let s = path_utf8(&device_dir(&host_data_root()));
    emit_string(cb, ctx, &s);
}

/// Returns `true` when a complete ACNH device save exists.
#[no_mangle]
pub extern "C" fn acnh_saves_has_save() -> bool {
    let _lk = saves_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = host_data_root();
    recover(&root);
    flat_save(&device_dir(&root), None)
}

/// Returns `"YYYY-MM-DD HH:MM"` of `main.dat` (or `""` if missing) via `cb`.
#[no_mangle]
pub extern "C" fn acnh_saves_last_modified(cb: AcnhSavesStringCb, ctx: *mut c_void) {
    let s = last_modified_str(&device_dir(&host_data_root()));
    emit_string(cb, ctx, &s);
}

/// Detects emulator, recomp, and SD-card ACNH saves, invoking `cb` for each `(label, path)`.
#[no_mangle]
pub extern "C" fn acnh_saves_detect_emulator_saves(cb: AcnhSavesFoundCb, ctx: *mut c_void) {
    let root = host_data_root();
    let own = fs::canonicalize(device_dir(&root)).ok();
    let mut s = Scan {
        out: Vec::new(),
        seen: BTreeSet::new(),
        own,
    };
    #[cfg(windows)]
    {
        if let Ok(a) = std::env::var("APPDATA") {
            if !a.is_empty() {
                scan_emu_base(&mut s, Path::new(&a), "");
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(h) = home_dir() {
            scan_emu_base(&mut s, &h.join("Library").join("Application Support"), "");
        }
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let h = home_dir();
        let data = std::env::var("XDG_DATA_HOME")
            .ok()
            .filter(|v| !v.is_empty());
        let config = std::env::var("XDG_CONFIG_HOME")
            .ok()
            .filter(|v| !v.is_empty());
        if let Some(d) = data.or_else(|| h.as_ref().map(|p| path_utf8(&p.join(".local/share")))) {
            scan_emu_base(&mut s, Path::new(&d), "");
        }
        if let Some(c) = config.or_else(|| h.as_ref().map(|p| path_utf8(&p.join(".config")))) {
            scan_emu_base(&mut s, Path::new(&c), "");
        }
        if let Some(h) = h {
            for app in subdirs(&h.join(".var").join("app")) {
                scan_emu_base(&mut s, &app.join("data"), " (Flatpak)");
                scan_emu_base(&mut s, &app.join("config"), " (Flatpak)");
            }
        }
    }
    scan_recomps(&mut s);
    for v in volumes() {
        let vname = v
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_string())
            .unwrap_or_else(|| path_utf8(&v));
        scan_volume(&mut s, &v, &vname);
    }
    if let Some(cb) = cb {
        for (label, path) in &s.out {
            cb(
                label.as_ptr().cast::<c_char>(),
                label.len(),
                path.as_ptr().cast::<c_char>(),
                path.len(),
                ctx,
            );
        }
    }
}

/// Checks if `dir` (folder or `.zip`) looks like an ACNH save; writes reason to `why_cb` on failure.
#[no_mangle]
pub extern "C" fn acnh_saves_looks_like_acnh_save(
    dir: *const c_char,
    why_cb: AcnhSavesStringCb,
    why_ctx: *mut c_void,
) -> bool {
    let Some(dir_str) = cstr_opt(dir) else {
        emit_string(why_cb, why_ctx, "folder not found");
        return false;
    };
    let p = PathBuf::from(&dir_str);
    let mut why = String::new();
    let ok = if p.is_file() {
        zip_list(&p, &mut why)
            .and_then(|entries| zip_find_root(&entries, &mut why))
            .is_some()
    } else {
        find_root(&p, &mut why).is_some()
    };
    if !ok {
        emit_string(why_cb, why_ctx, &why);
    }
    ok
}

/// Replaces the current device save with `from` (folder or `.zip`), backing up the current save first.
#[no_mangle]
pub extern "C" fn acnh_saves_import_save(
    from: *const c_char,
    err_cb: AcnhSavesStringCb,
    err_ctx: *mut c_void,
) -> bool {
    let Some(from_str) = cstr_opt(from) else {
        emit_string(err_cb, err_ctx, "invalid path");
        return false;
    };
    let _lk = saves_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = host_data_root();
    let mut err = String::new();
    let ok = import_locked(&root, Path::new(&from_str), &mut err);
    if !ok {
        emit_string(err_cb, err_ctx, &err);
    }
    ok
}

/// Exports the current device save to `to` (a `.zip` path or a parent directory).
#[no_mangle]
pub extern "C" fn acnh_saves_export_save(
    to: *const c_char,
    written_cb: AcnhSavesStringCb,
    written_ctx: *mut c_void,
    err_cb: AcnhSavesStringCb,
    err_ctx: *mut c_void,
) -> bool {
    let Some(to_str) = cstr_opt(to) else {
        emit_string(err_cb, err_ctx, "invalid path");
        return false;
    };
    let _lk = saves_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = host_data_root();
    recover(&root);
    let src = device_dir(&root);
    let mut err = String::new();
    if !flat_save(&src, Some(&mut err)) {
        emit_string(err_cb, err_ctx, "no save to export");
        return false;
    }
    let mut dest = PathBuf::from(&to_str);
    if ends_with_zip(&to_str) {
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() && !parent.is_dir() {
                emit_string(
                    err_cb,
                    err_ctx,
                    &format!("folder not found: {}", path_utf8(parent)),
                );
                return false;
            }
        }
        let tmp = PathBuf::from(format!("{to_str}.tmp"));
        if !zip_write(&src, &tmp, &mut err) {
            let _ = fs::remove_file(&tmp);
            emit_string(err_cb, err_ctx, &err);
            return false;
        }
        let _ = fs::remove_file(&dest);
        if fs::rename(&tmp, &dest).is_err() {
            let _ = fs::remove_file(&tmp);
            emit_string(err_cb, err_ctx, &format!("cannot write {to_str}"));
            return false;
        }
    } else {
        let _ = fs::create_dir_all(&dest);
        if !dest.is_dir() {
            emit_string(err_cb, err_ctx, &format!("cannot create {to_str}"));
            return false;
        }
        let final_path = unique_in(&dest, &format!("ACNH-{}", stamp_now()), "");
        let tmp = PathBuf::from(format!("{}.partial", path_utf8(&final_path)));
        let _ = fs::remove_dir_all(&tmp);
        if !copy_tree_inner(&src, &tmp, &mut err) {
            let _ = fs::remove_dir_all(&tmp);
            emit_string(err_cb, err_ctx, &err);
            return false;
        }
        if fs::rename(&tmp, &final_path).is_err() {
            let _ = fs::remove_dir_all(&tmp);
            emit_string(
                err_cb,
                err_ctx,
                &format!("cannot finish {}", path_utf8(&final_path)),
            );
            return false;
        }
        dest = final_path;
    }
    let written = path_utf8(&dest);
    emit_string(written_cb, written_ctx, &written);
    host_log(&format!("saves: exported to {written}"));
    true
}

/// Creates a backup of the current device save; emits `"<island folder>/<backup>"` via `out_cb`
/// (or `""` and an error via `err_cb`).
#[no_mangle]
pub extern "C" fn acnh_saves_backup_now(
    automatic: bool,
    out_cb: AcnhSavesStringCb,
    out_ctx: *mut c_void,
    err_cb: AcnhSavesStringCb,
    err_ctx: *mut c_void,
) {
    let _lk = saves_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = host_data_root();
    recover(&root);
    let mut err = String::new();
    let name = backup_locked(&root, &mut err, automatic);
    if name.is_empty() && !err.is_empty() {
        emit_string(err_cb, err_ctx, &err);
    }
    emit_string(out_cb, out_ctx, &name);
}

/// Returns `<data root>/user/backups/<island folder>` via `cb`.
#[no_mangle]
pub extern "C" fn acnh_saves_backups_folder(cb: AcnhSavesStringCb, ctx: *mut c_void) {
    let s = path_utf8(&backups_dir(&host_data_root()));
    emit_string(cb, ctx, &s);
}

/// Lists all backups across all islands, invoking `cb` for each `"<island folder>/<backup>"`.
#[no_mangle]
pub extern "C" fn acnh_saves_list_backups(cb: AcnhSavesStringCb, ctx: *mut c_void) {
    let _lk = saves_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = host_data_root();
    for item in all_backups(&root) {
        emit_string(cb, ctx, &item);
    }
}

/// Restores a backup named `"<island folder>/<backup>"`, backing up the current save first.
#[no_mangle]
pub extern "C" fn acnh_saves_restore_backup(
    name: *const c_char,
    err_cb: AcnhSavesStringCb,
    err_ctx: *mut c_void,
) -> bool {
    let Some(name_str) = cstr_opt(name) else {
        emit_string(err_cb, err_ctx, "invalid backup name");
        return false;
    };
    let _lk = saves_lock().lock().unwrap_or_else(|e| e.into_inner());
    let (folder, backup) = match name_str.split_once('/') {
        Some((f, b)) => (f, b),
        None => (name_str.as_str(), ""),
    };
    if folder.is_empty()
        || folder == "."
        || folder == ".."
        || backup.contains('/')
        || name_str.contains(['\\', ':'])
        || !backup_name(backup)
    {
        emit_string(err_cb, err_ctx, "invalid backup name");
        return false;
    }
    let root = host_data_root();
    let p = backups_root(&root).join(folder).join(backup);
    if !p.is_dir() {
        emit_string(err_cb, err_ctx, &format!("backup not found: {name_str}"));
        return false;
    }
    let mut err = String::new();
    let ok = import_locked(&root, &p, &mut err);
    if !ok {
        emit_string(err_cb, err_ctx, &err);
    }
    ok
}

/// Enumerates all local Switch users, invoking `cb` for each `AcnhUserInfo`.
#[no_mangle]
pub extern "C" fn acnh_users_all(cb: AcnhSavesUserCb, ctx: *mut c_void) {
    let users = users_all_vec();
    if let Some(cb) = cb {
        for u in &users {
            let ffi = u.to_ffi();
            cb(&ffi, ctx);
        }
    }
}

/// Returns the number of local Switch users.
#[no_mangle]
pub extern "C" fn acnh_users_count() -> i32 {
    users_all_vec().len() as i32
}

/// Finds a user by `(lo, hi)`. Writes to `out` if non-null and returns `true` when found.
///
/// # Safety
/// When non-null, `out` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_user_find(lo: u64, hi: u64, out: *mut AcnhUserInfo) -> bool {
    for u in users_all_vec() {
        if u.lo == lo && u.hi == hi {
            if !out.is_null() {
                // SAFETY: caller guarantees `out` is valid when non-null.
                unsafe { *out = u.to_ffi() };
            }
            return true;
        }
    }
    false
}

/// Creates a new user (`"Player 2"`, etc.) if fewer than 8 exist. Returns `true` on success.
///
/// # Safety
/// When non-null, `out` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_user_create(out: *mut AcnhUserInfo) -> bool {
    let data_root = host_data_root();
    let mut st = users_lock().lock().unwrap_or_else(|e| e.into_inner());
    load_users_locked(&data_root, &mut st);
    if st.users.len() >= ACNH_MAX_USERS {
        if !out.is_null() {
            // SAFETY: caller guarantees `out` is valid when non-null.
            unsafe { *out = AcnhUserInfo::default() };
        }
        return false;
    }
    let next_lo = st.users.iter().map(|u| u.lo).max().unwrap_or(0) + 1;
    let mut nickname = String::new();
    for k in (st.users.len() + 1).. {
        let candidate = format!("Player {k}");
        if !st.users.iter().any(|u| u.nickname == candidate) {
            nickname = candidate;
            break;
        }
    }
    let rec = UserRecord {
        lo: next_lo,
        hi: ACNH_FIRST_USER_HI,
        nickname,
    };
    st.users.push(rec.clone());
    save_users_locked(&data_root, &st.users);
    host_log(&format!(
        "users: created user {} {}",
        hex_uid(rec.lo, rec.hi),
        rec.nickname
    ));
    if !out.is_null() {
        // SAFETY: caller guarantees `out` is valid when non-null.
        unsafe { *out = rec.to_ffi() };
    }
    true
}

/// Returns the user the game boots into.
///
/// # Safety
/// When non-null, `out` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_user_boot(out: *mut AcnhUserInfo) {
    // SAFETY: caller guarantees `out` is valid when non-null.
    unsafe { acnh_user_last_opened(out) };
}

/// Remembers `(lo, hi)` as the last opened user if it exists.
#[no_mangle]
pub extern "C" fn acnh_user_opened(lo: u64, hi: u64) {
    let found = users_all_vec()
        .into_iter()
        .find(|u| u.lo == lo && u.hi == hi);
    if let Some(u) = found {
        let mut st = users_lock().lock().unwrap_or_else(|e| e.into_inner());
        st.last = Some(u);
    }
}

/// Returns the last opened user (or `NX_USER`, or the first user).
///
/// # Safety
/// When non-null, `out` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_user_last_opened(out: *mut AcnhUserInfo) {
    if out.is_null() {
        return;
    }
    let u = user_last_opened_rec();
    // SAFETY: caller guarantees `out` is valid.
    unsafe { *out = u.to_ffi() };
}

/// Returns `"save"` for the first user or `"save_<uid hex>"` for others.
pub fn user_save_dir_name(lo: u64, hi: u64) -> String {
    if lo == ACNH_FIRST_USER_LO && hi == ACNH_FIRST_USER_HI {
        String::from("save")
    } else {
        format!("save_{}", hex_uid(lo, hi))
    }
}

/// Returns `"save"` for the first user or `"save_<uid hex>"` for others, via `cb`.
#[no_mangle]
pub extern "C" fn acnh_user_save_dir(lo: u64, hi: u64, cb: AcnhSavesStringCb, ctx: *mut c_void) {
    let s = user_save_dir_name(lo, hi);
    emit_string(cb, ctx, &s);
}

/// Writes the `0x88`-byte preselected user `IStorage` payload into `out` and returns `0x88`.
///
/// # Safety
/// When non-null, `out` must be valid for `cap` bytes of writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_account_preselected_user_blob(out: *mut u8, cap: usize) -> usize {
    const SIZE: usize = 0x88;
    let u = user_last_opened_rec();
    host_log(&format!("account: preselected user {}", u.nickname));
    if !out.is_null() && cap >= SIZE {
        // SAFETY: caller guarantees `out` is valid for `cap >= 0x88` bytes.
        let slice = unsafe { std::slice::from_raw_parts_mut(out, SIZE) };
        slice.fill(0);
        slice[0..4].copy_from_slice(&ACNH_PRESELECTED_USER_MAGIC.to_le_bytes());
        slice[4..8].copy_from_slice(&1u32.to_le_bytes());
        slice[8..16].copy_from_slice(&u.lo.to_le_bytes());
        slice[16..24].copy_from_slice(&u.hi.to_le_bytes());
    }
    SIZE
}

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
        let out = unsafe { &mut *ctx.cast::<String>() };
        let bytes = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) };
        *out = String::from_utf8_lossy(bytes).into_owned();
    }

    extern "C" fn collect_str_vec(ptr: *const c_char, len: usize, ctx: *mut c_void) {
        let out = unsafe { &mut *ctx.cast::<Vec<String>>() };
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
        assert!(unsafe { acnh_user_find(ACNH_FIRST_USER_LO, ACNH_FIRST_USER_HI, &mut u1) });
        let nick1 = unsafe { CStr::from_ptr(u1.nickname.as_ptr()) }
            .to_str()
            .unwrap();
        assert_eq!(nick1, "Tester");

        let mut u2 = AcnhUserInfo::default();
        assert!(unsafe { acnh_user_create(&mut u2) });
        assert_eq!(u2.lo, 2);
        assert_eq!(acnh_users_count(), 2);

        acnh_user_opened(u2.lo, u2.hi);
        let mut boot = AcnhUserInfo::default();
        unsafe { acnh_user_boot(&mut boot) };
        assert_eq!(boot.lo, 2);

        let mut blob = [0u8; 0x88];
        assert_eq!(
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
