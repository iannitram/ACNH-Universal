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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::{c_char, c_void, CStr, CString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

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

const RECENT_OPS: usize = 24;
const RECENT_TEXT_MAX: usize = 199;
const DEFAULT_DEVICE_SAVE_SIZE: i64 = 0x4000_0000;

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
    pub read_asset:
        Option<extern "C" fn(*const c_char, Option<AcnhFilesBytesCb>, *mut c_void) -> bool>,
}

// SAFETY: `AcnhFilesHost` only holds plain function pointers.
unsafe impl Send for AcnhFilesHost {}
// SAFETY: `AcnhFilesHost` only holds plain function pointers.
unsafe impl Sync for AcnhFilesHost {}

#[derive(Clone, Debug)]
struct DirEntry {
    name: String,
    dir: bool,
    size: i64,
}

#[derive(Default)]
struct DirHandle {
    entries: Vec<DirEntry>,
    pos: usize,
}

enum FileBacking {
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

struct OpenFileEntry {
    path: String,
    backing: FileBacking,
}

struct Pak {
    file: Arc<File>,
    files: HashMap<String, (i64, i64)>,
    dirs: HashMap<String, Vec<DirEntry>>,
}

struct FsState {
    mounts: BTreeMap<String, PathBuf>,
    files: BTreeMap<u64, Arc<Mutex<OpenFileEntry>>>,
    dirs: BTreeMap<u64, DirHandle>,
    virtual_files: BTreeMap<String, Arc<Vec<u8>>>,
    next_handle: u64,
    told_lost: HashSet<String>,
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
struct RecentOp {
    thread: i64,
    seq: u64,
    text: String,
}

struct RecentState {
    ring: Vec<RecentOp>,
    seq: u64,
}

impl Default for RecentState {
    fn default() -> Self {
        Self {
            ring: vec![RecentOp::default(); RECENT_OPS],
            seq: 0,
        }
    }
}

struct CachedPak {
    game_root: PathBuf,
    pak: Option<Arc<Pak>>,
}

struct CachedIndex {
    data_root: PathBuf,
    hashes: Vec<u64>,
}

static HOST: OnceLock<Mutex<AcnhFilesHost>> = OnceLock::new();
static FS_STATE: OnceLock<Mutex<FsState>> = OnceLock::new();
static RECENT_LOCK: OnceLock<Mutex<RecentState>> = OnceLock::new();
static STREAM_READS: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
static PAK_CACHE: OnceLock<Mutex<Option<CachedPak>>> = OnceLock::new();
static INDEX_CACHE: OnceLock<Mutex<Option<CachedIndex>>> = OnceLock::new();
static MONO_START: OnceLock<Instant> = OnceLock::new();
static AOC_LOGGED: AtomicU64 = AtomicU64::new(0);

fn host_lock() -> &'static Mutex<AcnhFilesHost> {
    HOST.get_or_init(|| Mutex::new(AcnhFilesHost::default()))
}

fn fs_lock() -> &'static Mutex<FsState> {
    FS_STATE.get_or_init(|| Mutex::new(FsState::default()))
}

fn recent_lock() -> &'static Mutex<RecentState> {
    RECENT_LOCK.get_or_init(|| Mutex::new(RecentState::default()))
}

fn stream_reads_lock() -> &'static Mutex<HashMap<String, u64>> {
    STREAM_READS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn current_host() -> AcnhFilesHost {
    *host_lock().lock().unwrap_or_else(|e| e.into_inner())
}

fn cstr_opt<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: caller guarantees `ptr` is a valid NUL-terminated C string when non-null.
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}

fn cstr_lossy(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: caller guarantees `ptr` is a valid NUL-terminated C string when non-null.
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

fn emit_string(cb: Option<AcnhFilesStringCb>, ctx: *mut c_void, value: &str) {
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

fn call_host_string(cb: Option<extern "C" fn(*mut c_char, usize) -> usize>) -> Option<String> {
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

fn data_root() -> PathBuf {
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

fn game_root() -> PathBuf {
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

fn host_log(msg: &str) {
    if let Some(log_fn) = current_host().log {
        let sanitized = msg.replace('\0', " ");
        if let Ok(c) = CString::new(sanitized) {
            log_fn(c.as_ptr());
        }
    }
}

fn host_thread_id() -> i64 {
    current_host().current_thread_id.map(|f| f()).unwrap_or(0)
}

fn host_monotonic_ns() -> u64 {
    if let Some(f) = current_host().monotonic_ns {
        return f();
    }
    let start = MONO_START.get_or_init(Instant::now);
    start.elapsed().as_nanos() as u64
}

fn host_trace_global() -> bool {
    current_host().trace_enabled.map(|f| f()).unwrap_or(false)
}

fn trace_fs() -> bool {
    host_trace_global() || std::env::var_os("NX_TRACE_FS").is_some()
}

fn host_discord_presence_file(guest_path: &str) {
    if let Some(f) = current_host().discord_presence_file {
        if let Ok(c) = CString::new(guest_path.replace('\0', "")) {
            f(c.as_ptr());
        }
    }
}

fn host_glyph_override(romfs_rel: &str) -> Option<PathBuf> {
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

fn host_read_asset(rel: &str) -> Option<Vec<u8>> {
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

fn note_open(what: &str, guest_path: &str, result: &str) {
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

fn normalize_slashes(s: &str) -> String {
    s.replace('\\', "/")
}

fn lexical_normalize_slashes(raw: &str) -> String {
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

fn virtual_key(path_str: &str) -> String {
    lexical_normalize_slashes(path_str).to_ascii_lowercase()
}

fn find_virtual(st: &FsState, resolved: &Path) -> Option<Arc<Vec<u8>>> {
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

fn romfs_root_for(gr: &Path) -> PathBuf {
    let direct = gr.join("romfs");
    if direct.is_dir() {
        direct
    } else {
        gr.join("extract").join("romfs")
    }
}

fn romfs_root() -> PathBuf {
    romfs_root_for(&game_root())
}

fn parse_pak(path: &Path) -> Option<Pak> {
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

fn pak() -> Option<Arc<Pak>> {
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

fn in_pak(resolved: &Path) -> Option<(Arc<Pak>, String)> {
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

fn mod_enabled(key: &str) -> bool {
    let v = config::config_get(key, "0");
    v == "1" || v == "on" || v == "true"
}

fn mod_roots() -> Vec<PathBuf> {
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

fn resolve(st: &FsState, guest: &str) -> Option<PathBuf> {
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

fn mount(st: &mut FsState, name: &str, p: PathBuf) {
    let _ = fs::create_dir_all(&p);
    host_log(&format!("fs: mounted {name}: -> {}", p.display()));
    st.mounts.insert(name.to_string(), p);
}

fn game_file_path(rel: &str) -> bool {
    rel.starts_with("romfs/") || rel.starts_with("extract/romfs/") || rel.starts_with("aoc/")
}

fn fnv1a_64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn imported_files() -> Vec<u64> {
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

fn note_lost_file(st: &mut FsState, p: &Path) {
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

fn read_at_file(file: &File, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
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

fn aoc_indexes_vec() -> Vec<i32> {
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

// ---- FFI exports: lifecycle & host -------------------------------------------------------------

/// Installs the host callback table from `runtime/src/files_wrapper.cpp`.
///
/// # Safety
/// `host` must be null or point to a valid `AcnhFilesHost`.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_set_host(host: *const AcnhFilesHost) {
    if host.is_null() {
        return;
    }
    // SAFETY: caller guarantees `host` is valid for reads.
    let h = unsafe { *host };
    *host_lock().lock().unwrap_or_else(|e| e.into_inner()) = h;
}

/// Logs the recent file/directory opens ring buffer (safe from crash handlers via `try_lock`).
#[no_mangle]
pub extern "C" fn acnh_files_log_recent() {
    let Ok(st) = recent_lock().try_lock() else {
        return;
    };
    host_log("  last file opens (newest last; thread = host thread id):");
    for k in 0..RECENT_OPS {
        let idx = (st.seq as usize + k) % RECENT_OPS;
        let r = &st.ring[idx];
        if r.seq != 0 {
            host_log(&format!("    #{} thread {}  {}", r.seq, r.thread, r.text));
        }
    }
}

/// Registers an in-memory virtual file served under any mount matching `path`.
///
/// # Safety
/// When `len > 0` and `data` is non-null, `data` must be valid for `len` bytes of reads.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_add_virtual_file(
    path: *const c_char,
    data: *const u8,
    len: usize,
) {
    let Some(p) = cstr_opt(path) else {
        return;
    };
    let bytes = if data.is_null() || len == 0 {
        Vec::new()
    } else {
        // SAFETY: caller guarantees `data` is valid for `len` bytes.
        unsafe { std::slice::from_raw_parts(data, len) }.to_vec()
    };
    let key = virtual_key(p);
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    st.virtual_files.insert(key, Arc::new(bytes));
}

/// Reads a file relative to the RomFS root into `cb` (empty if missing).
#[no_mangle]
pub extern "C" fn acnh_files_read_game_file(
    relative_to_romfs: *const c_char,
    cb: Option<AcnhFilesBytesCb>,
    ctx: *mut c_void,
) {
    let Some(cb) = cb else {
        return;
    };
    let rel = cstr_lossy(relative_to_romfs);
    let rel = rel.trim_start_matches(['/', '\\']);
    let full = romfs_root().join(rel);
    if let Ok(bytes) = fs::read(&full) {
        cb(bytes.as_ptr(), bytes.len(), ctx);
    } else {
        cb(std::ptr::null(), 0, ctx);
    }
}

// ---- FFI exports: mounts -----------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn acnh_files_mount_rom(name: *const c_char) -> u32 {
    let n = cstr_lossy(name);
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    mount(&mut st, &n, romfs_root());
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_ensure_save_data(uid_lo: u64, uid_hi: u64) -> u32 {
    saves::acnh_user_opened(uid_lo, uid_hi);
    let dir = data_root()
        .join("user")
        .join(saves::user_save_dir_name(uid_lo, uid_hi));
    let _ = fs::create_dir_all(&dir);
    host_log(&format!("fs: EnsureSaveData -> {}", dir.display()));
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_mount_save_data(name: *const c_char, uid_lo: u64, uid_hi: u64) -> u32 {
    let n = cstr_lossy(name);
    saves::acnh_user_opened(uid_lo, uid_hi);
    let dir = data_root()
        .join("user")
        .join(saves::user_save_dir_name(uid_lo, uid_hi));
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    mount(&mut st, &n, dir);
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_unmount(name: *const c_char) -> u32 {
    let n = cstr_lossy(name);
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    st.mounts.remove(&n);
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_commit(names: *const c_char) -> u32 {
    let n = cstr_lossy(names);
    host_log(&format!("fs: save: Commit({n})"));
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_mount_device_save_data(name: *const c_char) -> u32 {
    let n = cstr_lossy(name);
    let dir = data_root().join("user").join("device_save");
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    mount(&mut st, &n, dir);
    FS_OK
}

/// Writes device save data size and journal size.
///
/// # Safety
/// Non-null `out_size` and `out_journal` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_get_device_save_data_size(
    out_size: *mut i64,
    out_journal: *mut i64,
) -> u32 {
    let mut size = DEFAULT_DEVICE_SAVE_SIZE;
    let mut journal = DEFAULT_DEVICE_SAVE_SIZE;
    let size_file = data_root().join("user").join("device_save.size");
    if let Ok(text) = fs::read_to_string(&size_file) {
        let mut nums = text.split_whitespace();
        if let (Some(a), Some(b)) = (nums.next(), nums.next()) {
            if let (Ok(sa), Ok(sb)) = (a.parse::<i64>(), b.parse::<i64>()) {
                size = sa;
                journal = sb;
            }
        }
    }
    if !out_size.is_null() {
        // SAFETY: caller guarantees `out_size` is valid for writes.
        unsafe { *out_size = size };
    }
    if !out_journal.is_null() {
        // SAFETY: caller guarantees `out_journal` is valid for writes.
        unsafe { *out_journal = journal };
    }
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_extend_device_save_data(size: i64, journal: i64) -> u32 {
    let user_dir = data_root().join("user");
    let _ = fs::create_dir_all(&user_dir);
    let _ = fs::write(
        user_dir.join("device_save.size"),
        format!("{size} {journal}\n"),
    );
    host_log(&format!(
        "fs: ExtendDeviceSaveData({size:#x}, {journal:#x})"
    ));
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_mount_cache_storage(name: *const c_char, index: i32) -> u32 {
    let n = cstr_lossy(name);
    let dir = data_root().join("user").join(format!("cache{index}"));
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    mount(&mut st, &n, dir);
    FS_OK
}

// ---- FFI exports: files & directories ----------------------------------------------------------

#[no_mangle]
pub extern "C" fn acnh_files_create_file(path: *const c_char, size: i64) -> u32 {
    let guest = cstr_lossy(path);
    let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = resolve(&st, &guest) else {
        host_log(&format!(
            "fs: save: CreateFile({guest}, {size}): no parent directory"
        ));
        return FS_PATH_NOT_FOUND;
    };
    let parent_ok = p.parent().is_some_and(|parent| parent.exists());
    if !parent_ok {
        host_log(&format!(
            "fs: save: CreateFile({guest}, {size}): no parent directory"
        ));
        return FS_PATH_NOT_FOUND;
    }
    if p.exists() {
        return FS_PATH_ALREADY_EXISTS;
    }
    let ok = match File::create(&p) {
        Ok(mut f) => {
            if size > 0 {
                f.seek(SeekFrom::Start((size - 1) as u64)).is_ok() && f.write_all(&[0u8]).is_ok()
            } else {
                true
            }
        }
        Err(_) => false,
    };
    host_log(&format!(
        "fs: save: CreateFile({guest}, {size}){}",
        if ok { "" } else { " failed on the host" }
    ));
    if ok {
        FS_OK
    } else {
        FS_PATH_NOT_FOUND
    }
}

#[no_mangle]
pub extern "C" fn acnh_files_delete_file(path: *const c_char) -> u32 {
    let guest = cstr_lossy(path);
    let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let ok = resolve(&st, &guest).is_some_and(|p| fs::remove_file(p).is_ok());
    host_log(&format!(
        "fs: save: DeleteFile({guest}){}",
        if ok { "" } else { ": not found" }
    ));
    if ok {
        FS_OK
    } else {
        FS_PATH_NOT_FOUND
    }
}

#[no_mangle]
pub extern "C" fn acnh_files_create_directory(path: *const c_char) -> u32 {
    let guest = cstr_lossy(path);
    let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = resolve(&st, &guest) else {
        return FS_PATH_NOT_FOUND;
    };
    if p.exists() {
        return FS_PATH_ALREADY_EXISTS;
    }
    let ok = fs::create_dir(&p).is_ok();
    host_log(&format!(
        "fs: save: CreateDirectory({guest}){}",
        if ok { "" } else { ": failed" }
    ));
    if ok {
        FS_OK
    } else {
        FS_PATH_NOT_FOUND
    }
}

/// Resolves `path` and writes its `DirectoryEntryType` (`0` dir, `1` file) to `out_type`.
///
/// # Safety
/// When non-null, `out_type` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_get_entry_type(path: *const c_char, out_type: *mut u32) -> u32 {
    let guest = cstr_lossy(path);
    let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = resolve(&st, &guest) else {
        return FS_PATH_NOT_FOUND;
    };
    if find_virtual(&st, &p).is_some() {
        if !out_type.is_null() {
            // SAFETY: caller guarantees `out_type` is valid.
            unsafe { *out_type = ENTRY_TYPE_FILE };
        }
        return FS_OK;
    }
    if let Some((k, rel)) = in_pak(&p) {
        let is_file = k.files.contains_key(&rel);
        let is_dir = rel.is_empty() || k.dirs.contains_key(&rel);
        if is_file || is_dir {
            if !out_type.is_null() {
                // SAFETY: caller guarantees `out_type` is valid.
                unsafe {
                    *out_type = if is_dir {
                        ENTRY_TYPE_DIR
                    } else {
                        ENTRY_TYPE_FILE
                    }
                };
            }
            return FS_OK;
        }
        return FS_PATH_NOT_FOUND;
    }
    if !p.exists() {
        return FS_PATH_NOT_FOUND;
    }
    if !out_type.is_null() {
        // SAFETY: caller guarantees `out_type` is valid.
        unsafe {
            *out_type = if p.is_dir() {
                ENTRY_TYPE_DIR
            } else {
                ENTRY_TYPE_FILE
            }
        };
    }
    FS_OK
}

/// Opens a file at `path` with `mode` (`1` read, `2` write, `4` append) and writes handle to `out_handle`.
///
/// # Safety
/// When non-null, `out_handle` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_open_file(
    path: *const c_char,
    mode: i32,
    out_handle: *mut u64,
) -> u32 {
    let guest = cstr_lossy(path);
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let resolved = resolve(&st, &guest);
    let Some(p) = resolved else {
        note_open("OpenFile", &guest, "unknown mount");
        if trace_fs() || (mode & 2) != 0 {
            host_log(&format!("fs: OpenFile({guest}, {mode}) not found"));
        }
        return FS_PATH_NOT_FOUND;
    };

    if let Some(v) = find_virtual(&st, &p) {
        let h = st.next_handle;
        st.next_handle += 1;
        let len = v.len();
        st.files.insert(
            h,
            Arc::new(Mutex::new(OpenFileEntry {
                path: guest.clone(),
                backing: FileBacking::Virtual { data: v },
            })),
        );
        if !out_handle.is_null() {
            // SAFETY: caller guarantees `out_handle` is valid.
            unsafe { *out_handle = h };
        }
        note_open("OpenFile", &guest, "virtual");
        if trace_fs() {
            host_log(&format!("fs: OpenFile({guest}) -> virtual, {len} bytes"));
        }
        return FS_OK;
    }

    if let Some((k, rel)) = in_pak(&p) {
        let Some(&(off, size)) = k.files.get(&rel) else {
            note_open("OpenFile", &guest, "not found");
            if trace_fs() {
                host_log(&format!("fs: OpenFile({guest}) not in romfs.pak"));
            }
            return FS_PATH_NOT_FOUND;
        };
        if (mode & 2) != 0 {
            note_open("OpenFile", &guest, "not found");
            if trace_fs() {
                host_log(&format!("fs: OpenFile({guest}) not in romfs.pak"));
            }
            return FS_PATH_NOT_FOUND;
        }
        let h = st.next_handle;
        st.next_handle += 1;
        st.files.insert(
            h,
            Arc::new(Mutex::new(OpenFileEntry {
                path: guest.clone(),
                backing: FileBacking::Pak {
                    file: Arc::clone(&k.file),
                    base: off,
                    size,
                },
            })),
        );
        if !out_handle.is_null() {
            // SAFETY: caller guarantees `out_handle` is valid.
            unsafe { *out_handle = h };
        }
        note_open("OpenFile", &guest, "ok");
        if trace_fs() {
            host_log(&format!("fs: OpenFile({guest}, {mode}) -> {h} (packed)"));
        }
        return FS_OK;
    }

    if !p.is_file() {
        note_open("OpenFile", &guest, "not found");
        if trace_fs() || (mode & 2) != 0 {
            host_log(&format!("fs: OpenFile({guest}, {mode}) not found"));
        }
        note_lost_file(&mut st, &p);
        return FS_PATH_NOT_FOUND;
    }

    let open_res = if (mode & 2) != 0 {
        OpenOptions::new().read(true).write(true).open(&p)
    } else {
        File::open(&p)
    };
    let file = match open_res {
        Ok(f) => f,
        Err(err) => {
            note_open("OpenFile", &guest, "the host could not open it");
            if trace_fs() {
                host_log(&format!(
                    "fs: OpenFile({guest}) failed to open on the host ({err})"
                ));
            }
            return FS_PATH_NOT_FOUND;
        }
    };
    let h = st.next_handle;
    st.next_handle += 1;
    st.files.insert(
        h,
        Arc::new(Mutex::new(OpenFileEntry {
            path: guest.clone(),
            backing: FileBacking::Host { file, written: 0 },
        })),
    );
    if !out_handle.is_null() {
        // SAFETY: caller guarantees `out_handle` is valid.
        unsafe { *out_handle = h };
    }
    note_open("OpenFile", &guest, "ok");
    if trace_fs() {
        host_log(&format!("fs: OpenFile({guest}, {mode}) -> {h}"));
    }
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_close_file(handle: u64) {
    let removed = {
        let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
        st.files.remove(&handle)
    };
    if let Some(entry) = removed {
        let guard = entry.lock().unwrap_or_else(|e| e.into_inner());
        if let FileBacking::Host { written, .. } = &guard.backing {
            if *written > 0 {
                host_log(&format!(
                    "fs: save: wrote {written} bytes to {}",
                    guard.path
                ));
            }
        }
    }
}

/// Writes the file size to `out_size`.
///
/// # Safety
/// When non-null, `out_size` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_get_file_size(handle: u64, out_size: *mut i64) -> u32 {
    let entry = {
        let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
        st.files.get(&handle).cloned()
    };
    let Some(entry) = entry else {
        return FS_TARGET_NOT_FOUND;
    };
    let guard = entry.lock().unwrap_or_else(|e| e.into_inner());
    let size = match &guard.backing {
        FileBacking::Pak { size, .. } => *size,
        FileBacking::Virtual { data } => data.len() as i64,
        FileBacking::Host { file, .. } => {
            let sz = file.metadata().map(|m| m.len() as i64).unwrap_or(0);
            if host_trace_global() {
                host_log(&format!("fs: size {} = {sz}", guard.path));
            }
            sz
        }
    };
    if !out_size.is_null() {
        // SAFETY: caller guarantees `out_size` is valid.
        unsafe { *out_size = size };
    }
    FS_OK
}

/// Reads `size` bytes at `off` from `handle` into `buf`, optionally writing bytes read to `out_read`.
///
/// # Safety
/// `buf` must be valid for `size` bytes of writes when `size > 0`, and `out_read` when non-null.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_read_file(
    handle: u64,
    off: i64,
    buf: *mut u8,
    size: u64,
    out_read: *mut u64,
) -> u32 {
    let entry = {
        let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
        st.files.get(&handle).cloned()
    };
    let Some(entry) = entry else {
        return FS_TARGET_NOT_FOUND;
    };

    // Snapshot the underlying handle/slice so positional reads run without holding any lock.
    enum ReadSource {
        Host(File, String),
        Pak(Arc<File>, i64, i64, String),
        Virtual(Arc<Vec<u8>>, String),
    }

    let src = {
        let guard = entry.lock().unwrap_or_else(|e| e.into_inner());
        match &guard.backing {
            FileBacking::Host { file, .. } => match file.try_clone() {
                Ok(cloned) => ReadSource::Host(cloned, guard.path.clone()),
                Err(_) => return FS_PATH_NOT_FOUND,
            },
            FileBacking::Pak { file, base, size } => {
                ReadSource::Pak(Arc::clone(file), *base, *size, guard.path.clone())
            }
            FileBacking::Virtual { data } => {
                ReadSource::Virtual(Arc::clone(data), guard.path.clone())
            }
        }
    };

    let t0 = host_monotonic_ns();
    let (n, path, effective_off) = if size == 0 || buf.is_null() || off < 0 {
        let p = match &src {
            ReadSource::Host(_, p) | ReadSource::Pak(_, _, _, p) | ReadSource::Virtual(_, p) => {
                p.clone()
            }
        };
        (0u64, p, off)
    } else {
        // SAFETY: caller guarantees `buf` is valid for `size` bytes of writes.
        let dst = unsafe { std::slice::from_raw_parts_mut(buf, size as usize) };
        match src {
            ReadSource::Virtual(data, p) => {
                let start = off as usize;
                let n = if start >= data.len() {
                    0
                } else {
                    let avail = (data.len() - start).min(dst.len());
                    dst[..avail].copy_from_slice(&data[start..start + avail]);
                    avail as u64
                };
                (n, p, off)
            }
            ReadSource::Pak(file, base, file_size, p) => {
                if off >= file_size {
                    (0, p, off)
                } else {
                    let max_read = ((file_size - off) as u64).min(size) as usize;
                    let eff = base + off;
                    let mut total = 0usize;
                    while total < max_read {
                        match read_at_file(
                            &file,
                            (eff as u64) + total as u64,
                            &mut dst[total..max_read],
                        ) {
                            Ok(0) | Err(_) => break,
                            Ok(k) => total += k,
                        }
                    }
                    (total as u64, p, eff)
                }
            }
            ReadSource::Host(file, p) => {
                let mut total = 0usize;
                while total < dst.len() {
                    match read_at_file(&file, (off as u64) + total as u64, &mut dst[total..]) {
                        Ok(0) | Err(_) => break,
                        Ok(k) => total += k,
                    }
                }
                (total as u64, p, off)
            }
        }
    };

    let ms = host_monotonic_ns().saturating_sub(t0) / 1_000_000;
    if host_trace_global() {
        host_log(&format!(
            "fs: read {path} at {effective_off} size {size} -> {n}"
        ));
    }
    if trace_fs() && path.contains(".bwav") {
        let mut map = stream_reads_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let k = map.entry(path.clone()).or_insert(0);
        *k += 1;
        let count = *k;
        if count <= 3 || count.is_multiple_of(50) {
            host_log(&format!(
                "fs: stream read #{count} {path} at {effective_off}"
            ));
        }
    }
    if ms >= 30 {
        host_log(&format!(
            "fs: slow read {path}: {size} bytes at {effective_off} took {ms} ms"
        ));
    }
    if !out_read.is_null() {
        // SAFETY: caller guarantees `out_read` is valid.
        unsafe { *out_read = n };
    }
    FS_OK
}

/// Writes `size` bytes from `buf` at `off` into `handle`.
///
/// # Safety
/// `buf` must be valid for `size` bytes of reads when `size > 0`.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_write_file(
    handle: u64,
    off: i64,
    buf: *const u8,
    size: u64,
) -> u32 {
    let entry = {
        let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
        st.files.get(&handle).cloned()
    };
    let Some(entry) = entry else {
        return FS_TARGET_NOT_FOUND;
    };
    let mut guard = entry.lock().unwrap_or_else(|e| e.into_inner());
    let path = guard.path.clone();
    let FileBacking::Host { file, written } = &mut guard.backing else {
        return FS_PATH_NOT_FOUND;
    };
    if off < 0 {
        return FS_PATH_NOT_FOUND;
    }
    if file.seek(SeekFrom::Start(off as u64)).is_err() {
        return FS_PATH_NOT_FOUND;
    }
    if size > 0 && !buf.is_null() {
        // SAFETY: caller guarantees `buf` is valid for `size` bytes of reads.
        let src = unsafe { std::slice::from_raw_parts(buf, size as usize) };
        match file.write(src) {
            Ok(n) => {
                let _ = file.flush();
                *written += n as u64;
                if (n as u64) != size {
                    host_log(&format!(
                        "fs: save: WriteFile({path}) wrote {n} of {size} bytes"
                    ));
                }
            }
            Err(e) => {
                host_log(&format!(
                    "fs: save: WriteFile({path}) wrote 0 of {size} bytes ({e})"
                ));
            }
        }
    }
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_flush_file(handle: u64) -> u32 {
    let entry = {
        let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
        st.files.get(&handle).cloned()
    };
    if let Some(entry) = entry {
        let mut guard = entry.lock().unwrap_or_else(|e| e.into_inner());
        if let FileBacking::Host { file, .. } = &mut guard.backing {
            let _ = file.flush();
        }
    }
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_set_file_size(handle: u64, size: i64) -> u32 {
    let entry = {
        let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
        st.files.get(&handle).cloned()
    };
    let Some(entry) = entry else {
        return FS_TARGET_NOT_FOUND;
    };
    let mut guard = entry.lock().unwrap_or_else(|e| e.into_inner());
    let FileBacking::Host { file, .. } = &mut guard.backing else {
        return FS_PATH_NOT_FOUND;
    };
    if size < 0 {
        return FS_PATH_NOT_FOUND;
    }
    let _ = file.flush();
    if file.set_len(size as u64).is_ok() {
        FS_OK
    } else {
        FS_PATH_NOT_FOUND
    }
}

/// Opens a directory at `path` with `mode` (`1` dirs, `2` files) and writes handle to `out_handle`.
///
/// # Safety
/// When non-null, `out_handle` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_open_directory(
    path: *const c_char,
    mode: i32,
    out_handle: *mut u64,
) -> u32 {
    let guest = cstr_lossy(path);
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = resolve(&st, &guest) else {
        note_open("OpenDirectory", &guest, "not found");
        if trace_fs() {
            host_log(&format!("fs: OpenDirectory({guest}, {mode}) not found"));
        }
        return FS_PATH_NOT_FOUND;
    };

    if let Some((k, rel)) = in_pak(&p) {
        let Some(entries) = k.dirs.get(&rel) else {
            note_open("OpenDirectory", &guest, "not found");
            return FS_PATH_NOT_FOUND;
        };
        note_open("OpenDirectory", &guest, "ok");
        let filtered: Vec<DirEntry> = entries
            .iter()
            .filter(|e| (e.dir && (mode & 1) != 0) || (!e.dir && (mode & 2) != 0))
            .cloned()
            .collect();
        let h = st.next_handle;
        st.next_handle += 1;
        st.dirs.insert(
            h,
            DirHandle {
                entries: filtered,
                pos: 0,
            },
        );
        if !out_handle.is_null() {
            // SAFETY: caller guarantees `out_handle` is valid.
            unsafe { *out_handle = h };
        }
        return FS_OK;
    }

    let found = p.is_dir();
    note_open(
        "OpenDirectory",
        &guest,
        if found { "ok" } else { "not found" },
    );
    if trace_fs() {
        host_log(&format!(
            "fs: OpenDirectory({guest}, {mode}){}",
            if found { "" } else { " not found" }
        ));
    }
    if !found {
        return FS_PATH_NOT_FOUND;
    }

    let mut entries = Vec::new();
    if let Ok(rd) = fs::read_dir(&p) {
        for item in rd.flatten() {
            let meta = match item.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let is_dir = meta.is_dir();
            if (is_dir && (mode & 1) != 0) || (!is_dir && (mode & 2) != 0) {
                entries.push(DirEntry {
                    name: item.file_name().to_string_lossy().into_owned(),
                    dir: is_dir,
                    size: if is_dir { 0 } else { meta.len() as i64 },
                });
            }
        }
    }

    let h = st.next_handle;
    st.next_handle += 1;
    st.dirs.insert(h, DirHandle { entries, pos: 0 });
    if !out_handle.is_null() {
        // SAFETY: caller guarantees `out_handle` is valid.
        unsafe { *out_handle = h };
    }
    FS_OK
}

/// Reads up to `max_entries` from directory `handle` via `cb`, writing actual count to `out_count`.
///
/// # Safety
/// When non-null, `out_count` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_read_directory(
    handle: u64,
    max_entries: i64,
    out_count: *mut i64,
    cb: Option<AcnhFilesDirEntryCb>,
    ctx: *mut c_void,
) -> u32 {
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(dir) = st.dirs.get_mut(&handle) else {
        return FS_TARGET_NOT_FOUND;
    };
    let mut n = 0i64;
    while n < max_entries && dir.pos < dir.entries.len() {
        let e = &dir.entries[dir.pos];
        if let Some(cb) = cb {
            let sanitized = e.name.replace('\0', "");
            if let Ok(c) = CString::new(sanitized.as_str()) {
                cb(c.as_ptr(), sanitized.len(), e.dir, e.size, ctx);
            }
        }
        dir.pos += 1;
        n += 1;
    }
    if !out_count.is_null() {
        // SAFETY: caller guarantees `out_count` is valid.
        unsafe { *out_count = n };
    }
    FS_OK
}

/// Writes total entry count for directory `handle` to `out_count`.
///
/// # Safety
/// When non-null, `out_count` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_get_directory_entry_count(
    handle: u64,
    out_count: *mut i64,
) -> u32 {
    let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let count = st
        .dirs
        .get(&handle)
        .map(|d| d.entries.len() as i64)
        .unwrap_or(0);
    if !out_count.is_null() {
        // SAFETY: caller guarantees `out_count` is valid.
        unsafe { *out_count = count };
    }
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_close_directory(handle: u64) {
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    st.dirs.remove(&handle);
}

#[no_mangle]
pub extern "C" fn acnh_files_clean_directory_recursively(path: *const c_char) -> u32 {
    let guest = cstr_lossy(path);
    let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = resolve(&st, &guest) else {
        return FS_PATH_NOT_FOUND;
    };
    if !p.is_dir() {
        return FS_PATH_NOT_FOUND;
    }
    let Ok(rd) = fs::read_dir(&p) else {
        return FS_PATH_NOT_FOUND;
    };
    for entry in rd.flatten() {
        let ep = entry.path();
        let res = if ep.is_dir() {
            fs::remove_dir_all(&ep)
        } else {
            fs::remove_file(&ep)
        };
        if res.is_err() {
            return FS_PATH_NOT_FOUND;
        }
    }
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_rename_directory(from: *const c_char, to: *const c_char) -> u32 {
    let guest_from = cstr_lossy(from);
    let guest_to = cstr_lossy(to);
    let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let (Some(a), Some(b)) = (resolve(&st, &guest_from), resolve(&st, &guest_to)) else {
        return FS_PATH_NOT_FOUND;
    };
    if !a.is_dir() {
        return FS_PATH_NOT_FOUND;
    }
    if b.exists() {
        return FS_PATH_ALREADY_EXISTS;
    }
    let ok = fs::rename(&a, &b).is_ok();
    host_log(&format!(
        "fs: save: RenameDirectory({guest_from}, {guest_to}){}",
        if ok { "" } else { ": failed" }
    ));
    if ok {
        FS_OK
    } else {
        FS_PATH_NOT_FOUND
    }
}

#[no_mangle]
pub extern "C" fn acnh_files_delete_directory_recursively(path: *const c_char) -> u32 {
    let guest = cstr_lossy(path);
    let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = resolve(&st, &guest) else {
        return FS_PATH_NOT_FOUND;
    };
    if !p.is_dir() {
        return FS_PATH_NOT_FOUND;
    }
    if fs::remove_dir_all(&p).is_ok() {
        FS_OK
    } else {
        FS_PATH_NOT_FOUND
    }
}

// ---- FFI exports: add-on content (DLC) ---------------------------------------------------------

#[no_mangle]
pub extern "C" fn acnh_files_count_aoc() -> u64 {
    let v = aoc_indexes_vec();
    if AOC_LOGGED
        .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        let mut list = String::new();
        for idx in &v {
            list.push_str(&format!(" {idx}"));
        }
        host_log(&format!("aoc: {} add-on content(s):{list}", v.len()));
    }
    v.len() as u64
}

/// Writes up to `count` AOC indexes starting at `offset` into `out`.
///
/// # Safety
/// `out` must be valid for `count` `i32` writes when `count > 0`.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_list_aoc(out: *mut i32, offset: i32, count: i32) -> i32 {
    if out.is_null() || count <= 0 {
        return 0;
    }
    let v = aoc_indexes_vec();
    let start = offset.max(0) as usize;
    let mut k = 0i32;
    for &val in v.iter().skip(start).take(count as usize) {
        // SAFETY: caller guarantees `out` has capacity for `count` i32s.
        unsafe { *out.add(k as usize) = val };
        k += 1;
    }
    k
}

#[no_mangle]
pub extern "C" fn acnh_files_mount_aoc(name: *const c_char, index: i32) -> u32 {
    let n = cstr_lossy(name);
    let p = game_root().join("aoc").join(index.to_string());
    if !p.is_dir() {
        host_log(&format!("aoc: MountAddOnContent({index}): not imported"));
        return FS_TARGET_NOT_FOUND;
    }
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    host_log(&format!(
        "fs: mounted {n}: -> {} (add-on {index})",
        p.display()
    ));
    st.mounts.insert(n, p);
    FS_OK
}

// ---- FFI exports: mod_fetch --------------------------------------------------------------------

fn bundle_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let canon = fs::canonicalize(&exe).unwrap_or(exe);
    let dir = canon.parent()?;
    if dir.file_name().and_then(|n| n.to_str()) == Some("MacOS")
        && dir
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            == Some("Contents")
    {
        return Some(dir.parent()?.join("Resources"));
    }
    Some(dir.to_path_buf())
}

fn read_bundled_mod_file(rel: &str) -> Option<Vec<u8>> {
    if let Some(bytes) = host_read_asset(&format!("mods/{rel}")) {
        return Some(bytes);
    }
    let dir = bundle_dir()?;
    fs::read(dir.join("mods").join(rel)).ok()
}

fn first_line(s: &str) -> String {
    s.lines()
        .next()
        .unwrap_or("")
        .trim_end_matches('\r')
        .to_string()
}

/// Copies bundled SpeedFix parameters into `<data_root>/mods` when opted in (`mod_fetch.cpp`).
#[no_mangle]
pub extern "C" fn acnh_files_mods_fetch() {
    if let Ok(e) = std::env::var("NX_MOD_FETCH") {
        if e.starts_with('0') {
            return;
        }
    }
    if config::config_get("game.mod_fetch", "on") == "off" {
        return;
    }
    let Some(list_bytes) = read_bundled_mod_file("bundle.txt") else {
        return;
    };
    let list = String::from_utf8_lossy(&list_bytes);
    let version = first_line(&list);
    if version.is_empty() {
        return;
    }
    let mods = data_root().join("mods");
    let marker = mods.join(".bundle-speedfix");
    if let Ok(existing) = fs::read_to_string(&marker) {
        if first_line(&existing) == version {
            return;
        }
    }
    let files: Vec<&str> = list
        .lines()
        .skip(1)
        .map(|l| l.trim_end_matches('\r'))
        .filter(|rel| rel.starts_with("speedfix60/") && !rel.contains(".."))
        .collect();
    if files.is_empty() {
        return;
    }
    let mut top_dirs = BTreeSet::new();
    for rel in &files {
        if let Some((top, _)) = rel.split_once('/') {
            top_dirs.insert(top);
        }
    }
    for top in top_dirs {
        let _ = fs::remove_dir_all(mods.join(top));
    }

    let mut copied = 0usize;
    let mut bytes = 0usize;
    for rel in &files {
        let Some(data) = read_bundled_mod_file(rel) else {
            host_log(&format!(
                "mods: {rel} could not be copied into {}",
                mods.display()
            ));
            continue;
        };
        let dst = mods.join(rel);
        let wrote = dst.parent().is_some_and(|p| fs::create_dir_all(p).is_ok())
            && fs::write(&dst, &data).is_ok();
        if !wrote {
            host_log(&format!(
                "mods: {rel} could not be copied into {}",
                mods.display()
            ));
            continue;
        }
        copied += 1;
        bytes += data.len();
    }
    let ok = copied == files.len();
    if ok {
        let _ = fs::create_dir_all(&mods);
        let _ = fs::write(&marker, format!("{version}\n"));
    }
    host_log(&format!(
        "mods: the package's mods ({version}) copied into {}: {copied} of {} files, {} KB{}",
        mods.display(),
        files.len(),
        bytes >> 10,
        if ok {
            ""
        } else {
            " (the rest: at the next start)"
        }
    ));
}

// ---- FFI exports: game_folder helpers ----------------------------------------------------------

/// Builds the platform-specific game shared library path under `folder`.
pub fn game_library_path_for_os(folder: &str, game_id: &str, os: &str) -> String {
    match os {
        "windows" => format!("{folder}/game/windows-x64/{game_id}_game.dll"),
        "macos" => format!("{folder}/game/macos-arm64/lib{game_id}_game.dylib"),
        _ => format!("{folder}/game/linux-x64/lib{game_id}_game.so"),
    }
}

/// Formats the user-facing explanation shown when `load_game` fails.
pub fn format_load_game_error(why: &str) -> String {
    if why.starts_with("abi") {
        format!(
            "This game folder was prepared for another version of ACNHRecomp. Open the ACNHRecomp \
             launcher and choose Prepare again on its home page: the game's code is built again \
             for this version, the rest is kept.\n\n{why}"
        )
    } else {
        format!(
            "This is not a game folder prepared for this computer. Open the ACNHRecomp launcher \
             and prepare your game there, from the ExeFS and RomFS you extracted from your own \
             copy with your emulator (a folder prepared before: Prepare again, with this \
             computer ticked).\n\n{why}"
        )
    }
}

#[no_mangle]
pub extern "C" fn acnh_files_game_library_path(
    folder: *const c_char,
    game_id: *const c_char,
    cb: Option<AcnhFilesStringCb>,
    ctx: *mut c_void,
) {
    let f = cstr_lossy(folder);
    let id = cstr_lossy(game_id);
    let os = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let path = game_library_path_for_os(&f, &id, os);
    emit_string(cb, ctx, &path);
}

#[no_mangle]
pub extern "C" fn acnh_files_format_load_error(
    why: *const c_char,
    cb: Option<AcnhFilesStringCb>,
    ctx: *mut c_void,
) {
    let w = cstr_lossy(why);
    let msg = format_load_game_error(&w);
    emit_string(cb, ctx, &msg);
}

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
        *host_lock().lock().unwrap() = AcnhFilesHost::default();
        *fs_lock().lock().unwrap() = FsState::default();
        *recent_lock().lock().unwrap() = RecentState::default();
        if let Some(c) = PAK_CACHE.get() {
            *c.lock().unwrap() = None;
        }
        if let Some(c) = INDEX_CACHE.get() {
            *c.lock().unwrap() = None;
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
        unsafe { acnh_files_add_virtual_file(vpath.as_ptr(), vdata.as_ptr(), vdata.len()) };

        let guest = CString::new("rom:/sub/hello.txt").unwrap();
        let mut et = 99u32;
        assert_eq!(
            unsafe { acnh_files_get_entry_type(guest.as_ptr(), &mut et) },
            FS_OK
        );
        assert_eq!(et, ENTRY_TYPE_FILE);

        let mut h = 0u64;
        assert_eq!(
            unsafe { acnh_files_open_file(guest.as_ptr(), 1, &mut h) },
            FS_OK
        );

        let mut sz = 0i64;
        assert_eq!(unsafe { acnh_files_get_file_size(h, &mut sz) }, FS_OK);
        assert_eq!(sz, vdata.len() as i64);

        let mut buf = [0u8; 7];
        let mut n = 0u64;
        assert_eq!(
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
            unsafe { acnh_files_get_entry_type(dir_guest.as_ptr(), &mut et) },
            FS_OK
        );
        assert_eq!(et, ENTRY_TYPE_DIR);

        let mut dh = 0u64;
        assert_eq!(
            unsafe { acnh_files_open_directory(dir_guest.as_ptr(), 3, &mut dh) },
            FS_OK
        );
        let mut count = 0i64;
        assert_eq!(
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
            let out = unsafe { &mut *(ctx as *mut Vec<(String, bool, i64)>) };
            let s = unsafe { std::slice::from_raw_parts(name as *const u8, len) };
            out.push((String::from_utf8_lossy(s).into_owned(), dir, size));
        }

        let mut entries: Vec<(String, bool, i64)> = Vec::new();
        let mut read_n = 0i64;
        assert_eq!(
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
            unsafe { acnh_files_open_file(file_guest.as_ptr(), 1, &mut fh) },
            FS_OK
        );
        let mut fsz = 0i64;
        assert_eq!(unsafe { acnh_files_get_file_size(fh, &mut fsz) }, FS_OK);
        assert_eq!(fsz, 9);

        let mut buf = [0u8; 4];
        let mut rn = 0u64;
        assert_eq!(
            unsafe { acnh_files_read_file(fh, 5, buf.as_mut_ptr(), 4, &mut rn) },
            FS_OK
        );
        assert_eq!(rn, 4);
        assert_eq!(&buf, b"data");
        acnh_files_close_file(fh);

        // Write mode on packed file is rejected.
        let mut wh = 0u64;
        assert_eq!(
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
            unsafe { acnh_files_open_file(fpath.as_ptr(), 3, &mut fh) },
            FS_OK
        );
        assert_eq!(
            unsafe { acnh_files_write_file(fh, 0, b"ABCD".as_ptr(), 4) },
            FS_OK
        );
        assert_eq!(acnh_files_flush_file(fh), FS_OK);
        assert_eq!(acnh_files_set_file_size(fh, 2), FS_OK);

        let mut sz = 0i64;
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
            unsafe { acnh_files_get_device_save_data_size(&mut sz, &mut j) },
            FS_OK
        );
        assert_eq!(sz, DEFAULT_DEVICE_SAVE_SIZE);
        assert_eq!(acnh_files_extend_device_save_data(0x1000, 0x2000), FS_OK);
        assert_eq!(
            unsafe { acnh_files_get_device_save_data_size(&mut sz, &mut j) },
            FS_OK
        );
        assert_eq!((sz, j), (0x1000, 0x2000));

        fs::create_dir_all(tmp.path.join("aoc/2")).unwrap();
        fs::create_dir_all(tmp.path.join("aoc/1")).unwrap();
        fs::create_dir_all(tmp.path.join("aoc/ignored")).unwrap();
        assert_eq!(acnh_files_count_aoc(), 2);

        let mut list = [0i32; 2];
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
