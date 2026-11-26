//! Filesystem HLE (`nn::fs`, `nn::aoc`), packed RomFS (`romfs.pak`), LayeredFS mods,
//! bundled SpeedFix mod fetcher, and desktop game-folder library helpers,
//! ported from `runtime/src/hle_fs.cpp`, `runtime/src/game_folder.cpp`, and
//! `runtime/src/mod_fetch.cpp`.
//!
//! `runtime/src/files_wrapper.cpp` provides the thin C++ host callbacks (`data_root`,
//! `game_root`, `log`, `current_thread_id`, `monotonic_ns`, `trace_enabled`,
//! `discord_presence_file`, `glyph_override`, `read_asset`), registers the `NX_HLE`
//! entries against the guest CPU dispatcher, and handles OS dynamic library loading /
//! SDL error dialogs for `nx::plat::load_game*`.

#[allow(unused_imports)]
use crate::sync::LockExt;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
#[allow(unused_imports)]
use std::ffi::{c_char, c_void, CStr, CString};
#[allow(unused_imports)]
use std::fs::{self, File, OpenOptions};
#[allow(unused_imports)]
use std::io::{Read, Seek, SeekFrom, Write};
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
#[allow(unused_imports)]
use std::sync::atomic::{AtomicU64, Ordering};
#[allow(unused_imports)]
use std::sync::{Arc, Mutex, OnceLock};
#[allow(unused_imports)]
use std::time::Instant;

#[allow(unused_imports)]
use crate::{config, saves};

/// Horizon `nn::fs` success result.
pub const FS_OK: u32 = 0;
/// Horizon `nn::fs::ResultPathNotFound` (`fs 1`).
pub const FS_PATH_NOT_FOUND: u32 = 0x202;
/// Horizon `nn::fs::ResultPathAlreadyExists` (`fs 2`).
pub const FS_PATH_ALREADY_EXISTS: u32 = 0x402;
/// Horizon `nn::fs::ResultTargetNotFound` (`fs 6001`-ish: generic "not mounted").
pub const FS_TARGET_NOT_FOUND: u32 = 0x2EE202;

/// Horizon `nn::fs::DirectoryEntryType::Directory`.
pub const ENTRY_TYPE_DIR: u32 = 0;
/// Horizon `nn::fs::DirectoryEntryType::File`.
pub const ENTRY_TYPE_FILE: u32 = 1;

pub(crate) const RECENT_OPS: usize = 24;
pub(crate) const RECENT_TEXT_MAX: usize = 199;
pub(crate) const DEFAULT_DEVICE_SAVE_SIZE: i64 = 0x4000_0000;

pub type AcnhFilesStringCb = extern "C" fn(*const c_char, usize, *mut c_void);
pub type AcnhFilesDirEntryCb = extern "C" fn(*const c_char, usize, bool, i64, *mut c_void);
pub type AcnhFilesBytesCb = extern "C" fn(*const u8, usize, *mut c_void);

/// Host callbacks provided by `runtime/src/files_wrapper.cpp`.
///
/// When unset (e.g. in standalone Rust unit tests), `data_root` falls back to `NX_DATA_ROOT`
/// or `"."`, `game_root` falls back to `NX_GAME_ROOT` or `data_root`, `log` is a no-op,
/// `current_thread_id` returns `0`, `monotonic_ns` uses a process-local `Instant`,
/// `trace_enabled` returns `false`, `discord_presence_file` is a no-op, `glyph_override`
/// returns `0`, and `read_asset` returns `false`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct AcnhFilesHost {
    pub data_root: Option<extern "C" fn(*mut c_char, usize) -> usize>,
    pub game_root: Option<extern "C" fn(*mut c_char, usize) -> usize>,
    pub log: Option<extern "C" fn(*const c_char)>,
    pub current_thread_id: Option<extern "C" fn() -> i64>,
    pub monotonic_ns: Option<extern "C" fn() -> u64>,
    pub trace_enabled: Option<extern "C" fn() -> bool>,
    pub discord_presence_file: Option<extern "C" fn(*const c_char)>,
    pub glyph_override: Option<extern "C" fn(*const c_char, *mut c_char, usize) -> usize>,
    pub read_asset: Option<
        extern "C" fn(
            *const c_char,
            Option<extern "C" fn(*const u8, usize, *mut c_void)>,
            *mut c_void,
        ) -> bool,
    >,
}

// SAFETY: `AcnhFilesHost` only holds plain function pointers.
unsafe impl Send for AcnhFilesHost {}
// SAFETY: `AcnhFilesHost` only holds plain function pointers.
unsafe impl Sync for AcnhFilesHost {}

#[derive(Clone, Debug)]
pub(crate) struct DirEntry {
    pub(crate) name: String,
    pub(crate) dir: bool,
    pub(crate) size: i64,
}

#[derive(Default)]
pub(crate) struct DirHandle {
    pub(crate) entries: Vec<DirEntry>,
    pub(crate) pos: usize,
}

pub(crate) enum FileBacking {
    Host {
        file: File,
        written: u64,
    },
    Pak {
        file: Arc<File>,
        base: i64,
        size: i64,
    },
    Virtual {
        data: Arc<Vec<u8>>,
    },
}

pub(crate) struct OpenFileEntry {
    pub(crate) path: String,
    pub(crate) backing: FileBacking,
}

pub(crate) struct Pak {
    pub(crate) file: Arc<File>,
    pub(crate) files: HashMap<String, (i64, i64)>,
    pub(crate) dirs: HashMap<String, Vec<DirEntry>>,
}

pub(crate) struct FsState {
    pub(crate) mounts: BTreeMap<String, PathBuf>,
    pub(crate) files: BTreeMap<u64, Arc<Mutex<OpenFileEntry>>>,
    pub(crate) dirs: BTreeMap<u64, DirHandle>,
    pub(crate) virtual_files: BTreeMap<String, Arc<Vec<u8>>>,
    pub(crate) next_handle: u64,
    pub(crate) told_lost: HashSet<String>,
}

impl Default for FsState {
    fn default() -> Self {
        Self {
            mounts: BTreeMap::new(),
            files: BTreeMap::new(),
            dirs: BTreeMap::new(),
            virtual_files: BTreeMap::new(),
            next_handle: 1,
            told_lost: HashSet::new(),
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct RecentOp {
    pub(crate) thread: i64,
    pub(crate) seq: u64,
    pub(crate) text: String,
}

pub(crate) struct RecentState {
    pub(crate) ring: Vec<RecentOp>,
    pub(crate) seq: u64,
}

impl Default for RecentState {
    fn default() -> Self {
        Self {
            ring: vec![RecentOp::default(); RECENT_OPS],
            seq: 0,
        }
    }
}

pub(crate) struct CachedPak {
    pub(crate) game_root: PathBuf,
    pub(crate) pak: Option<Arc<Pak>>,
}

pub(crate) struct CachedIndex {
    pub(crate) data_root: PathBuf,
    pub(crate) hashes: Vec<u64>,
}

pub(crate) static HOST: OnceLock<Mutex<AcnhFilesHost>> = OnceLock::new();
pub(crate) static FS_STATE: OnceLock<Mutex<FsState>> = OnceLock::new();
pub(crate) static RECENT_LOCK: OnceLock<Mutex<RecentState>> = OnceLock::new();
pub(crate) static STREAM_READS: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
pub(crate) static PAK_CACHE: OnceLock<Mutex<Option<CachedPak>>> = OnceLock::new();
pub(crate) static INDEX_CACHE: OnceLock<Mutex<Option<CachedIndex>>> = OnceLock::new();
pub(crate) static MONO_START: OnceLock<Instant> = OnceLock::new();
pub(crate) static AOC_LOGGED: AtomicU64 = AtomicU64::new(0);

pub(crate) fn host_lock() -> &'static Mutex<AcnhFilesHost> {
    HOST.get_or_init(|| Mutex::new(AcnhFilesHost::default()))
}

pub(crate) fn fs_lock() -> &'static Mutex<FsState> {
    FS_STATE.get_or_init(|| Mutex::new(FsState::default()))
}

pub(crate) fn recent_lock() -> &'static Mutex<RecentState> {
    RECENT_LOCK.get_or_init(|| Mutex::new(RecentState::default()))
}

pub(crate) fn stream_reads_lock() -> &'static Mutex<HashMap<String, u64>> {
    STREAM_READS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn current_host() -> AcnhFilesHost {
    *host_lock().lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn cstr_opt<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: caller guarantees `ptr` is a valid NUL-terminated C string when non-null.
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}

pub(crate) fn cstr_lossy(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: caller guarantees `ptr` is a valid NUL-terminated C string when non-null.
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

pub(crate) fn emit_string(
    cb: Option<extern "C" fn(*const c_char, usize, *mut c_void)>,
    ctx: *mut c_void,
    value: &str,
) {
    let Some(cb) = cb else {
        return;
    };
    if let Ok(c) = CString::new(value) {
        cb(c.as_ptr(), value.len(), ctx);
    } else {
        let sanitized = value.replace('\0', "");
        if let Ok(c) = CString::new(sanitized.as_str()) {
            cb(c.as_ptr(), sanitized.len(), ctx);
        }
    }
}

pub(crate) fn call_host_string(
    cb: Option<extern "C" fn(*mut c_char, usize) -> usize>,
) -> Option<String> {
    let cb = cb?;
    let mut buf = vec![0u8; 4096];
    let len = cb(buf.as_mut_ptr() as *mut c_char, buf.len());
    if len >= buf.len() {
        buf.resize(len + 1, 0);
        let len2 = cb(buf.as_mut_ptr() as *mut c_char, buf.len());
        let actual = len2.min(buf.len().saturating_sub(1));
        return Some(String::from_utf8_lossy(&buf[..actual]).into_owned());
    }
    Some(String::from_utf8_lossy(&buf[..len]).into_owned())
}

pub(crate) fn data_root() -> PathBuf {
    if let Some(s) = call_host_string(current_host().data_root) {
        if !s.is_empty() {
            return PathBuf::from(s);
        }
    }
    if let Ok(s) = std::env::var("NX_DATA_ROOT") {
        if !s.is_empty() {
            return PathBuf::from(s);
        }
    }
    PathBuf::from(".")
}

pub(crate) fn game_root() -> PathBuf {
    if let Some(s) = call_host_string(current_host().game_root) {
        if !s.is_empty() {
            return PathBuf::from(s);
        }
    }
    if let Ok(s) = std::env::var("NX_GAME_ROOT") {
        if !s.is_empty() {
            return PathBuf::from(s);
        }
    }
    data_root()
}

pub(crate) fn host_log(msg: &str) {
    if let Some(log_fn) = current_host().log {
        let sanitized = msg.replace('\0', " ");
        if let Ok(c) = CString::new(sanitized) {
            log_fn(c.as_ptr());
        }
    }
}

pub(crate) fn host_thread_id() -> i64 {
    current_host().current_thread_id.map(|f| f()).unwrap_or(0)
}

pub(crate) fn host_monotonic_ns() -> u64 {
    if let Some(f) = current_host().monotonic_ns {
        return f();
    }
    let start = MONO_START.get_or_init(Instant::now);
    start.elapsed().as_nanos() as u64
}

pub(crate) fn host_trace_global() -> bool {
    current_host().trace_enabled.map(|f| f()).unwrap_or(false)
}

pub(crate) fn trace_fs() -> bool {
    host_trace_global() || std::env::var_os("NX_TRACE_FS").is_some()
}

pub(crate) fn host_discord_presence_file(guest_path: &str) {
    if let Some(f) = current_host().discord_presence_file {
        if let Ok(c) = CString::new(guest_path.replace('\0', "")) {
            f(c.as_ptr());
        }
    }
}

pub(crate) fn host_glyph_override(romfs_rel: &str) -> Option<PathBuf> {
    let f = current_host().glyph_override?;
    let c = CString::new(romfs_rel.replace('\0', "")).ok()?;
    let mut buf = vec![0u8; 4096];
    let len = f(c.as_ptr(), buf.as_mut_ptr() as *mut c_char, buf.len());
    if len == 0 {
        return None;
    }
    let len = if len >= buf.len() {
        buf.resize(len + 1, 0);
        f(c.as_ptr(), buf.as_mut_ptr() as *mut c_char, buf.len()).min(buf.len().saturating_sub(1))
    } else {
        len
    };
    let s = String::from_utf8_lossy(&buf[..len]).into_owned();
    if s.is_empty() {
        None
    } else {
        Some(PathBuf::from(s))
    }
}

extern "C" fn collect_bytes_cb(data: *const u8, len: usize, ctx: *mut c_void) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: `ctx` points to a `&mut Vec<u8>` passed by `host_read_asset`.
    let out = unsafe { &mut *(ctx as *mut Vec<u8>) };
    if !data.is_null() && len > 0 {
        // SAFETY: `data` is valid for `len` bytes during callback invocation.
        let slice = unsafe { std::slice::from_raw_parts(data, len) };
        out.extend_from_slice(slice);
    }
}

pub(crate) fn host_read_asset(rel: &str) -> Option<Vec<u8>> {
    let f = current_host().read_asset?;
    let c = CString::new(rel.replace('\0', "")).ok()?;
    let mut out = Vec::new();
    let ok = f(
        c.as_ptr(),
        Some(collect_bytes_cb),
        (&mut out as *mut Vec<u8>).cast::<c_void>(),
    );
    if ok {
        Some(out)
    } else {
        None
    }
}

pub(crate) fn note_open(what: &str, guest_path: &str, result: &str) {
    if result == "ok" {
        host_discord_presence_file(guest_path);
    }
    let mut st = recent_lock().lock().unwrap_or_else(|e| e.into_inner());
    let seq = st.seq.wrapping_add(1);
    st.seq = seq;
    let idx = (seq.wrapping_sub(1) as usize) % RECENT_OPS;
    let mut text = format!("{what} {guest_path}: {result}");
    if text.len() > RECENT_TEXT_MAX {
        let mut cut = RECENT_TEXT_MAX;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    st.ring[idx] = RecentOp {
        thread: host_thread_id(),
        seq,
        text,
    };
}

pub(crate) fn normalize_slashes(s: &str) -> String {
    s.replace('\\', "/")
}

pub(crate) fn lexical_normalize_slashes(raw: &str) -> String {
    let s = normalize_slashes(raw);
    let leading_slash = s.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for seg in s.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if let Some(&last) = parts.last() {
                    if last != ".." {
                        parts.pop();
                    } else if !leading_slash {
                        parts.push("..");
                    }
                } else if !leading_slash {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    if leading_slash {
        format!("/{joined}")
    } else {
        joined
    }
}

pub(crate) fn virtual_key(path_str: &str) -> String {
    lexical_normalize_slashes(path_str).to_ascii_lowercase()
}

pub(crate) fn find_virtual(st: &FsState, resolved: &Path) -> Option<Arc<Vec<u8>>> {
    if st.virtual_files.is_empty() || resolved.as_os_str().is_empty() {
        return None;
    }
    let k = virtual_key(&resolved.to_string_lossy());
    let kb = k.as_bytes();
    for (key, data) in &st.virtual_files {
        if kb.len() > key.len() && k.ends_with(key.as_str()) {
            if kb[kb.len() - key.len() - 1] == b'/' {
                return Some(Arc::clone(data));
            }
        } else if k == *key {
            return Some(Arc::clone(data));
        }
    }
    None
}

pub(crate) fn romfs_root_for(gr: &Path) -> PathBuf {
    let direct = gr.join("romfs");
    if direct.is_dir() {
        direct
    } else {
        gr.join("extract").join("romfs")
    }
}

pub(crate) fn romfs_root() -> PathBuf {
    romfs_root_for(&game_root())
}

pub(crate) fn parse_pak(path: &Path) -> Option<Pak> {
    let mut f = File::open(path).ok()?;
    let mut hdr = [0u8; 12];
    if f.read_exact(&mut hdr).is_err() {
        host_log("fs: romfs.pak: not a packed RomFS");
        return None;
    }
    if &hdr[0..4] != b"NXRP" {
        host_log("fs: romfs.pak: not a packed RomFS");
        return None;
    }
    let version = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
    let count = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
    if version != 1 {
        host_log("fs: romfs.pak: not a packed RomFS");
        return None;
    }

    let mut files = HashMap::with_capacity(count as usize);
    let mut dirs: HashMap<String, Vec<DirEntry>> = HashMap::new();
    let mut seen_dirs = HashSet::new();

    for _ in 0..count {
        let mut len_buf = [0u8; 2];
        if f.read_exact(&mut len_buf).is_err() {
            return None;
        }
        let len = u16::from_le_bytes(len_buf) as usize;
        let mut path_buf = vec![0u8; len];
        let mut meta_buf = [0u8; 16];
        if f.read_exact(&mut path_buf).is_err() || f.read_exact(&mut meta_buf).is_err() {
            return None;
        }
        let entry_path = String::from_utf8_lossy(&path_buf).into_owned();
        let off = i64::from_le_bytes(meta_buf[0..8].try_into().unwrap());
        let size = i64::from_le_bytes(meta_buf[8..16].try_into().unwrap());
        files.insert(entry_path.clone(), (off, size));

        let mut child = entry_path;
        let mut child_size = size;
        let mut child_dir = false;
        loop {
            let (parent, name) = match child.rfind('/') {
                Some(slash) => (&child[..slash], &child[slash + 1..]),
                None => ("", child.as_str()),
            };
            dirs.entry(parent.to_string()).or_default().push(DirEntry {
                name: name.to_string(),
                dir: child_dir,
                size: if child_dir { 0 } else { child_size },
            });
            if parent.is_empty() || !seen_dirs.insert(parent.to_string()) {
                break;
            }
            child = parent.to_string();
            child_dir = true;
            child_size = 0;
        }
    }

    host_log(&format!("fs: packed RomFS: {count} files from romfs.pak"));
    Some(Pak {
        file: Arc::new(f),
        files,
        dirs,
    })
}

pub(crate) fn pak() -> Option<Arc<Pak>> {
    let gr = game_root();
    let cache_lock = PAK_CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache_lock.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(cached) = guard.as_ref() {
        if cached.game_root == gr {
            return cached.pak.clone();
        }
    }
    let pak_path = gr.join("romfs.pak");
    let loaded = if pak_path.is_file() {
        parse_pak(&pak_path).map(Arc::new)
    } else {
        None
    };
    *guard = Some(CachedPak {
        game_root: gr,
        pak: loaded.clone(),
    });
    loaded
}

pub(crate) fn in_pak(resolved: &Path) -> Option<(Arc<Pak>, String)> {
    let k = pak()?;
    let romfs = normalize_slashes(&romfs_root().to_string_lossy());
    let romfs = romfs.trim_end_matches('/');
    let s = normalize_slashes(&resolved.to_string_lossy());
    if !s.starts_with(romfs) {
        return None;
    }
    if s.len() > romfs.len() && s.as_bytes()[romfs.len()] != b'/' {
        return None;
    }
    let mut rel = if s.len() > romfs.len() {
        s[romfs.len() + 1..].to_string()
    } else {
        String::new()
    };
    while rel.ends_with('/') {
        rel.pop();
    }
    Some((k, rel))
}

pub(crate) fn mod_enabled(key: &str) -> bool {
    let v = config::config_get(key, "0");
    v == "1" || v == "on" || v == "true"
}

pub(crate) fn mod_roots() -> Vec<PathBuf> {
    let list = match std::env::var("NX_MODS") {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let dr = data_root();
    let mut out = Vec::new();
    for raw in list.split(',') {
        let name = raw.trim();
        if name.is_empty() || name.contains("..") {
            continue;
        }
        if (name == "ptbr" && !mod_enabled("mods.ptbr"))
            || (name == "speedfix60"
                && !mod_enabled("mods.speedfix")
                && config::config_get("video.frame_rate", "") != "60")
        {
            continue;
        }
        let root = dr.join("mods").join(name).join("romfs");
        if root.is_dir() {
            host_log(&format!("fs: mod {name} layered over the RomFS"));
            out.push(root);
        } else {
            host_log(&format!(
                "fs: mod {name} not installed ({} missing)",
                root.display()
            ));
        }
    }
    out
}

pub(crate) fn resolve(st: &FsState, guest: &str) -> Option<PathBuf> {
    let (mount_name, rest_raw) = guest.split_once(':')?;
    let mount_path = st.mounts.get(mount_name)?;
    let rest = rest_raw.trim_start_matches(['/', '\\']);
    let romfs = romfs_root();
    if mount_path == &romfs && !rest.is_empty() {
        if let Some(glyph) = host_glyph_override(rest) {
            return Some(glyph);
        }
        for root in mod_roots() {
            let candidate = root.join(rest);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    if rest.is_empty() {
        Some(mount_path.clone())
    } else {
        Some(mount_path.join(rest))
    }
}

pub(crate) fn mount(st: &mut FsState, name: &str, p: PathBuf) {
    let _ = fs::create_dir_all(&p);
    host_log(&format!("fs: mounted {name}: -> {}", p.display()));
    st.mounts.insert(name.to_string(), p);
}

pub(crate) fn game_file_path(rel: &str) -> bool {
    rel.starts_with("romfs/") || rel.starts_with("extract/romfs/") || rel.starts_with("aoc/")
}

pub(crate) fn fnv1a_64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub(crate) fn imported_files() -> Vec<u64> {
    let dr = data_root();
    let cache_lock = INDEX_CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache_lock.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(cached) = guard.as_ref() {
        if cached.data_root == dr {
            return cached.hashes.clone();
        }
    }
    let mut out = Vec::new();
    let index_path = dr.join(".import").join("index.tsv");
    if let Ok(text) = fs::read_to_string(&index_path) {
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            if !line.starts_with("romfs\t") {
                continue;
            }
            let mut parts = line.splitn(4, '\t');
            let _ = parts.next();
            let _ = parts.next();
            let _ = parts.next();
            if let Some(rel_raw) = parts.next() {
                let rel = normalize_slashes(rel_raw);
                if game_file_path(&rel) {
                    out.push(fnv1a_64(&rel));
                }
            }
        }
        out.sort_unstable();
        host_log(&format!(
            "fs: .import/index.tsv: {} files of the game, checked when one is missing",
            out.len()
        ));
    }
    *guard = Some(CachedIndex {
        data_root: dr,
        hashes: out.clone(),
    });
    out
}

pub(crate) fn note_lost_file(st: &mut FsState, p: &Path) {
    let full = normalize_slashes(&p.to_string_lossy());
    let mut root = normalize_slashes(&data_root().to_string_lossy());
    while root.ends_with('/') {
        root.pop();
    }
    if full.len() <= root.len() + 1
        || !full.starts_with(&root)
        || full.as_bytes()[root.len()] != b'/'
    {
        return;
    }
    let rel = &full[root.len() + 1..];
    if !game_file_path(rel) {
        return;
    }
    let hashes = imported_files();
    if hashes.binary_search(&fnv1a_64(rel)).is_err() || !st.told_lost.insert(rel.to_string()) {
        return;
    }
    host_log(&format!(
        "fs: {rel} was written when the game was prepared but is no longer in the game folder \
         (an antivirus or an unfinished copy may have removed it): prepare the game folder again \
         in the ACNHRecomp launcher from the same extracted files (a RomFS prepared in place: \
         extract it again), what is right is kept"
    ));
}

pub(crate) fn read_at_file(file: &File, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_at(buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        file.seek_read(buf, offset)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let mut f = file.try_clone()?;
        f.seek(SeekFrom::Start(offset))?;
        f.read(buf)
    }
}

pub(crate) fn aoc_indexes_vec() -> Vec<i32> {
    let mut out = Vec::new();
    let aoc_dir = game_root().join("aoc");
    let Ok(rd) = fs::read_dir(&aoc_dir) else {
        return out;
    };
    for entry in rd.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(idx) = name.parse::<i32>() {
                out.push(idx);
            }
        }
    }
    out.sort_unstable();
    out
}

mod ffi_core;
#[allow(unused_imports)]
pub use ffi_core::*;
mod ffi_extra;
#[allow(unused_imports)]
pub use ffi_extra::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    thread_local! {
        static TEST_ROOT: RefCell<PathBuf> = const { RefCell::new(PathBuf::new()) };
    }

    extern "C" fn test_root_cb(buf: *mut c_char, cap: usize) -> usize {
        TEST_ROOT.with(|cell| {
            let s = cell.borrow().to_string_lossy().into_owned();
            let bytes = s.as_bytes();
            if !buf.is_null() && cap > 0 {
                let n = bytes.len().min(cap - 1);
                // SAFETY: caller provides `buf` valid for `cap` bytes.
                unsafe {
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.cast::<u8>(), n);
                    *buf.add(n) = 0;
                }
            }
            bytes.len()
        })
    }

    fn set_test_root(p: &Path) {
        TEST_ROOT.with(|cell| *cell.borrow_mut() = p.to_path_buf());
        let host = AcnhFilesHost {
            data_root: Some(test_root_cb),
            game_root: Some(test_root_cb),
            ..Default::default()
        };
        // SAFETY: test-only: the host table lives on this test's stack and outlives every call that reads it.
        unsafe { acnh_files_set_host(&host) };
    }

    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "acnh_files_test_{prefix}_{}_{}_{}",
                std::process::id(),
                nanos,
                n
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn reset_state_for_test() {
        *host_lock().lock_or_recover() = AcnhFilesHost::default();
        *fs_lock().lock_or_recover() = FsState::default();
        *recent_lock().lock_or_recover() = RecentState::default();
        if let Some(c) = PAK_CACHE.get() {
            *c.lock_or_recover() = None;
        }
        if let Some(c) = INDEX_CACHE.get() {
            *c.lock_or_recover() = None;
        }
    }

    fn build_pak(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"NXRP");
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u32).to_le_bytes());

        let mut header_size = 12usize;
        for (p, _) in entries {
            header_size += 2 + p.len() + 8 + 8;
        }
        let mut data_off = header_size as i64;
        for (p, data) in entries {
            out.extend_from_slice(&(p.len() as u16).to_le_bytes());
            out.extend_from_slice(p.as_bytes());
            out.extend_from_slice(&data_off.to_le_bytes());
            out.extend_from_slice(&(data.len() as i64).to_le_bytes());
            data_off += data.len() as i64;
        }
        for (_, data) in entries {
            out.extend_from_slice(data);
        }
        out
    }

    #[test]
    fn virtual_key_normalizes_separators_dots_and_case() {
        assert_eq!(
            virtual_key(r"Content\Mods\..\Script\Test.LUA"),
            "content/script/test.lua"
        );
        assert_eq!(virtual_key("/a/./b/../c"), "/a/c");
    }

    #[test]
    fn virtual_file_takes_precedence_and_reads_pos() {
        let _g = test_guard();
        reset_state_for_test();
        let tmp = TempDir::new("virtual");
        set_test_root(&tmp.path);
        fs::create_dir_all(tmp.path.join("romfs/sub")).unwrap();
        fs::write(tmp.path.join("romfs/sub/hello.txt"), b"real").unwrap();

        let mount_name = CString::new("rom").unwrap();
        assert_eq!(acnh_files_mount_rom(mount_name.as_ptr()), FS_OK);

        let vpath = CString::new("Sub/Hello.txt").unwrap();
        let vdata = b"virtual-payload";
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        unsafe { acnh_files_add_virtual_file(vpath.as_ptr(), vdata.as_ptr(), vdata.len()) };

        let guest = CString::new("rom:/sub/hello.txt").unwrap();
        let mut et = 99u32;
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_files_get_entry_type(guest.as_ptr(), &mut et) },
            FS_OK
        );
        assert_eq!(et, ENTRY_TYPE_FILE);

        let mut h = 0u64;
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_files_open_file(guest.as_ptr(), 1, &mut h) },
            FS_OK
        );

        let mut sz = 0i64;
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        assert_eq!(unsafe { acnh_files_get_file_size(h, &mut sz) }, FS_OK);
        assert_eq!(sz, vdata.len() as i64);

        let mut buf = [0u8; 7];
        let mut n = 0u64;
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_files_read_file(h, 8, buf.as_mut_ptr(), 7, &mut n) },
            FS_OK
        );
        assert_eq!(n, 7);
        assert_eq!(&buf, b"payload");
        acnh_files_close_file(h);
    }

    #[test]
    fn pak_serves_files_and_directories() {
        let _g = test_guard();
        reset_state_for_test();
        let tmp = TempDir::new("pak");
        set_test_root(&tmp.path);
        fs::create_dir_all(tmp.path.join("romfs")).unwrap();

        let pak_bytes = build_pak(&[
            ("System/a.txt", b"alpha"),
            ("System/Sub/b.bin", b"beta-data"),
        ]);
        fs::write(tmp.path.join("romfs.pak"), pak_bytes).unwrap();

        let mount_name = CString::new("rom").unwrap();
        assert_eq!(acnh_files_mount_rom(mount_name.as_ptr()), FS_OK);

        let dir_guest = CString::new("rom:/System").unwrap();
        let mut et = 99u32;
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_files_get_entry_type(dir_guest.as_ptr(), &mut et) },
            FS_OK
        );
        assert_eq!(et, ENTRY_TYPE_DIR);

        let mut dh = 0u64;
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_files_open_directory(dir_guest.as_ptr(), 3, &mut dh) },
            FS_OK
        );
        let mut count = 0i64;
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_files_get_directory_entry_count(dh, &mut count) },
            FS_OK
        );
        assert_eq!(count, 2);

        extern "C" fn collect_dir(
            name: *const c_char,
            len: usize,
            dir: bool,
            size: i64,
            ctx: *mut c_void,
        ) {
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            let out = unsafe { &mut *(ctx as *mut Vec<(String, bool, i64)>) };
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            let s = unsafe { std::slice::from_raw_parts(name as *const u8, len) };
            out.push((String::from_utf8_lossy(s).into_owned(), dir, size));
        }

        let mut entries: Vec<(String, bool, i64)> = Vec::new();
        let mut read_n = 0i64;
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe {
                acnh_files_read_directory(
                    dh,
                    10,
                    &mut read_n,
                    Some(collect_dir),
                    (&mut entries as *mut Vec<_>).cast(),
                )
            },
            FS_OK
        );
        assert_eq!(read_n, 2);
        assert!(entries.contains(&(String::from("a.txt"), false, 5)));
        assert!(entries.contains(&(String::from("Sub"), true, 0)));
        acnh_files_close_directory(dh);

        let file_guest = CString::new("rom:/System/Sub/b.bin").unwrap();
        let mut fh = 0u64;
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_files_open_file(file_guest.as_ptr(), 1, &mut fh) },
            FS_OK
        );
        let mut fsz = 0i64;
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        assert_eq!(unsafe { acnh_files_get_file_size(fh, &mut fsz) }, FS_OK);
        assert_eq!(fsz, 9);

        let mut buf = [0u8; 4];
        let mut rn = 0u64;
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_files_read_file(fh, 5, buf.as_mut_ptr(), 4, &mut rn) },
            FS_OK
        );
        assert_eq!(rn, 4);
        assert_eq!(&buf, b"data");
        acnh_files_close_file(fh);

        // Write mode on packed file is rejected.
        let mut wh = 0u64;
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_files_open_file(file_guest.as_ptr(), 2, &mut wh) },
            FS_PATH_NOT_FOUND
        );
    }

    #[test]
    fn host_file_crud_and_directories() {
        let _g = test_guard();
        reset_state_for_test();
        let tmp = TempDir::new("crud");
        set_test_root(&tmp.path);

        let m = CString::new("save").unwrap();
        assert_eq!(acnh_files_mount_device_save_data(m.as_ptr()), FS_OK);

        let dir = CString::new("save:/slot").unwrap();
        assert_eq!(acnh_files_create_directory(dir.as_ptr()), FS_OK);
        assert_eq!(
            acnh_files_create_directory(dir.as_ptr()),
            FS_PATH_ALREADY_EXISTS
        );

        let fpath = CString::new("save:/slot/data.bin").unwrap();
        assert_eq!(acnh_files_create_file(fpath.as_ptr(), 4), FS_OK);
        assert_eq!(
            acnh_files_create_file(fpath.as_ptr(), 4),
            FS_PATH_ALREADY_EXISTS
        );

        let mut fh = 0u64;
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_files_open_file(fpath.as_ptr(), 3, &mut fh) },
            FS_OK
        );
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_files_write_file(fh, 0, b"ABCD".as_ptr(), 4) },
            FS_OK
        );
        assert_eq!(acnh_files_flush_file(fh), FS_OK);
        assert_eq!(acnh_files_set_file_size(fh, 2), FS_OK);

        let mut sz = 0i64;
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        assert_eq!(unsafe { acnh_files_get_file_size(fh, &mut sz) }, FS_OK);
        assert_eq!(sz, 2);
        acnh_files_close_file(fh);

        let dir2 = CString::new("save:/slot2").unwrap();
        assert_eq!(
            acnh_files_rename_directory(dir.as_ptr(), dir2.as_ptr()),
            FS_OK
        );
        assert_eq!(acnh_files_clean_directory_recursively(dir2.as_ptr()), FS_OK);
        assert_eq!(
            acnh_files_delete_directory_recursively(dir2.as_ptr()),
            FS_OK
        );
    }

    #[test]
    fn device_save_size_and_aoc_listing() {
        let _g = test_guard();
        reset_state_for_test();
        let tmp = TempDir::new("aoc");
        set_test_root(&tmp.path);

        let mut sz = 0i64;
        let mut j = 0i64;
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_files_get_device_save_data_size(&mut sz, &mut j) },
            FS_OK
        );
        assert_eq!(sz, DEFAULT_DEVICE_SAVE_SIZE);
        assert_eq!(acnh_files_extend_device_save_data(0x1000, 0x2000), FS_OK);
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_files_get_device_save_data_size(&mut sz, &mut j) },
            FS_OK
        );
        assert_eq!((sz, j), (0x1000, 0x2000));

        fs::create_dir_all(tmp.path.join("aoc/2")).unwrap();
        fs::create_dir_all(tmp.path.join("aoc/1")).unwrap();
        fs::create_dir_all(tmp.path.join("aoc/ignored")).unwrap();
        assert_eq!(acnh_files_count_aoc(), 2);

        let mut list = [0i32; 2];
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        assert_eq!(unsafe { acnh_files_list_aoc(list.as_mut_ptr(), 0, 2) }, 2);
        assert_eq!(list, [1, 2]);

        let aoc_m = CString::new("aoc").unwrap();
        assert_eq!(acnh_files_mount_aoc(aoc_m.as_ptr(), 2), FS_OK);
        assert_eq!(
            acnh_files_mount_aoc(aoc_m.as_ptr(), 99),
            FS_TARGET_NOT_FOUND
        );
    }

    #[test]
    fn mods_fetch_copies_speedfix_and_writes_marker() {
        let _g = test_guard();
        reset_state_for_test();
        let tmp = TempDir::new("modfetch");
        TEST_ROOT.with(|cell| *cell.borrow_mut() = tmp.path.clone());

        extern "C" fn fake_read_asset(
            rel: *const c_char,
            cb: Option<AcnhFilesBytesCb>,
            ctx: *mut c_void,
        ) -> bool {
            let Some(cb) = cb else { return false };
            let r = cstr_lossy(rel);
            match r.as_str() {
                "mods/bundle.txt" => {
                    let data =
                        b"v2\nspeedfix60/romfs/a.bin\nother/ignored.bin\nspeedfix60/../bad\n";
                    cb(data.as_ptr(), data.len(), ctx);
                    true
                }
                "mods/speedfix60/romfs/a.bin" => {
                    let data = b"sf-bytes";
                    cb(data.as_ptr(), data.len(), ctx);
                    true
                }
                _ => false,
            }
        }

        let host = AcnhFilesHost {
            data_root: Some(test_root_cb),
            game_root: Some(test_root_cb),
            read_asset: Some(fake_read_asset),
            ..Default::default()
        };
        // SAFETY: test-only: the host table lives on this test's stack and outlives every call that reads it.
        unsafe { acnh_files_set_host(&host) };
        acnh_files_mods_fetch();

        assert_eq!(
            fs::read(tmp.path.join("mods/speedfix60/romfs/a.bin")).unwrap(),
            b"sf-bytes"
        );
        assert_eq!(
            fs::read_to_string(tmp.path.join("mods/.bundle-speedfix")).unwrap(),
            "v2\n"
        );
        assert!(!tmp.path.join("mods/other").exists());
    }

    #[test]
    fn game_library_path_and_error_formatting() {
        assert_eq!(
            game_library_path_for_os("/data", "acnh", "linux"),
            "/data/game/linux-x64/libacnh_game.so"
        );
        assert_eq!(
            game_library_path_for_os("/data", "acnh", "macos"),
            "/data/game/macos-arm64/libacnh_game.dylib"
        );
        assert_eq!(
            game_library_path_for_os("C:/data", "acnh", "windows"),
            "C:/data/game/windows-x64/acnh_game.dll"
        );
        assert!(format_load_game_error("abi old").contains("Prepare again"));
        assert!(format_load_game_error("missing lib").contains("ExeFS and RomFS"));
    }
}
