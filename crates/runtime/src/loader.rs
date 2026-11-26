//! Module loader and dynamic linker (what `rtld` does on Horizon), indirect-call dispatch,
//! HLE thunks, logging, and boot sequence, ported from `runtime/src/loader.cpp`,
//! `runtime/src/main.cpp`, and `runtime/src/game_abi.cpp`.

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::{c_char, CStr, CString};
use std::fs::File;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

use crate::kernel::AcnhCpu;

pub const R_AARCH64_ABS64: u32 = 257;
pub const R_AARCH64_GLOB_DAT: u32 = 1025;
pub const R_AARCH64_JUMP_SLOT: u32 = 1026;
pub const R_AARCH64_RELATIVE: u32 = 1027;

const K_MEM_CODE: u32 = 0x03;
const K_MEM_CODE_DATA: u32 = 0x04;

const K_CALL_CACHE_BITS: usize = 16;
const K_CALL_CACHE_SIZE: usize = 1 << K_CALL_CACHE_BITS;
const K_MAX_FAST_MODULES: usize = 4;

pub type HleFn = extern "C" fn(*mut AcnhCpu);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AcnhFuncEntry {
    pub offset: u64,
    pub fn_ptr: Option<extern "C" fn(*mut AcnhCpu)>,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AcnhReloc {
    pub offset: u64,
    pub reloc_type: u32,
    pub sym: u32,
    pub addend: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AcnhSymbol {
    pub name: *const c_char,
    pub value: u64,
    pub size: u64,
    pub sym_type: u8,
    pub bind: u8,
    pub imported: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AcnhModuleInfo {
    pub name: *const c_char,
    pub base: u64,
    pub text: u64,
    pub text_size: u64,
    pub ro: u64,
    pub ro_size: u64,
    pub data: u64,
    pub data_size: u64,
    pub bss: u64,
    pub bss_size: u64,
    pub funcs: *const AcnhFuncEntry,
    pub func_count: usize,
    pub relocs: *const AcnhReloc,
    pub reloc_count: usize,
    pub symbols: *const AcnhSymbol,
    pub symbol_count: usize,
    pub init_array: *const u64,
    pub init_count: usize,
    pub fini_array: *const u64,
    pub fini_count: usize,
}

// SAFETY: `AcnhModuleInfo` points to static read-only tables generated at compile time.
unsafe impl Send for AcnhModuleInfo {}
// SAFETY: `AcnhModuleInfo` points to static read-only tables generated at compile time.
unsafe impl Sync for AcnhModuleInfo {}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AcnhLoadedModule {
    pub info: *const AcnhModuleInfo,
    pub base: u64,
    pub size: u64,
}

// SAFETY: `AcnhLoadedModule` only holds a pointer to static `AcnhModuleInfo` and addresses.
unsafe impl Send for AcnhLoadedModule {}
// SAFETY: `AcnhLoadedModule` only holds a pointer to static `AcnhModuleInfo` and addresses.
unsafe impl Sync for AcnhLoadedModule {}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct AcnhLoaderLayout {
    pub module_base: u64,
    pub thunk_base: u64,
    pub thunk_size: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct AcnhLoaderHost {
    pub data_root: Option<extern "C" fn(out: *mut c_char, cap: usize) -> usize>,
    pub game_root: Option<extern "C" fn(out: *mut c_char, cap: usize) -> usize>,
    pub setting: Option<extern "C" fn(key: *const c_char, out: *mut c_char, cap: usize) -> usize>,
    pub log_line: Option<extern "C" fn(line: *const c_char)>,
    pub show_fatal: Option<extern "C" fn(msg: *const c_char)>,
    pub exit_process: Option<extern "C" fn(code: i32)>,
    pub backtrace: Option<extern "C" fn(cpu: *mut AcnhCpu)>,
    pub commit: Option<extern "C" fn(addr: u64, size: u64) -> bool>,
    pub vm_set: Option<extern "C" fn(addr: u64, size: u64, state: u32, perm: u32)>,
    pub memory_init: Option<extern "C" fn()>,
    pub kernel_init: Option<extern "C" fn()>,
    pub set_tls_template: Option<extern "C" fn(start: u64, end: u64)>,
    pub create_main_thread: Option<extern "C" fn(stack_size: u64) -> u32>,
    pub current_cpu: Option<extern "C" fn() -> *mut AcnhCpu>,
    pub system_tick: Option<extern "C" fn() -> u64>,
    pub game_module: Option<extern "C" fn(name: *const c_char) -> *const AcnhModuleInfo>,
    pub game_longjmp: Option<extern "C" fn(buf: u64, val: u64)>,
    pub game_set_variant: Option<extern "C" fn(on: bool, lo: u64, hi: u64)>,
    pub after_init: Option<extern "C" fn()>,
    pub sync_variant: Option<extern "C" fn(on: bool, lo: u64, hi: u64)>,
    pub sync_trace: Option<extern "C" fn(trace: bool)>,
}

// SAFETY: `AcnhLoaderHost` only holds plain function pointers.
unsafe impl Send for AcnhLoaderHost {}
// SAFETY: `AcnhLoaderHost` only holds plain function pointers.
unsafe impl Sync for AcnhLoaderHost {}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct AcnhVariantState {
    pub on: bool,
    pub lo: u64,
    pub hi: u64,
}

static HOST: OnceLock<Mutex<AcnhLoaderHost>> = OnceLock::new();
static LAYOUT: OnceLock<Mutex<AcnhLoaderLayout>> = OnceLock::new();

fn host() -> AcnhLoaderHost {
    *HOST
        .get_or_init(|| Mutex::new(AcnhLoaderHost::default()))
        .lock()
        .unwrap()
}

fn layout() -> AcnhLoaderLayout {
    *LAYOUT
        .get_or_init(|| {
            Mutex::new(AcnhLoaderLayout {
                module_base: 0x0000_0040_0000_0000,
                thunk_base: 0x0000_006F_0000_0000,
                thunk_size: 0x10_0000,
            })
        })
        .lock()
        .unwrap()
}

fn cstr_or_empty<'a>(p: *const c_char) -> &'a str {
    if p.is_null() {
        return "";
    }
    // SAFETY: Caller passes a valid NUL-terminated C string or null.
    unsafe { CStr::from_ptr(p) }.to_str().unwrap_or("")
}

fn copy_str_to_buf(s: &str, out: *mut c_char, cap: usize) -> usize {
    let bytes = s.as_bytes();
    if !out.is_null() && cap > 0 {
        let n = bytes.len().min(cap - 1);
        // SAFETY: `out` is valid for `cap` bytes per caller contract.
        unsafe {
            if n > 0 {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.cast::<u8>(), n);
            }
            *out.add(n) = 0;
        }
    }
    bytes.len()
}

fn host_call_str(f: Option<extern "C" fn(*mut c_char, usize) -> usize>) -> String {
    let Some(cb) = f else {
        return String::new();
    };
    let mut buf = vec![0u8; 4096];
    let n = cb(buf.as_mut_ptr() as *mut c_char, buf.len());
    let len = n.min(buf.len().saturating_sub(1));
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

fn host_setting(key: &str) -> String {
    let Some(cb) = host().setting else {
        return String::new();
    };
    let Ok(c_key) = CString::new(key) else {
        return String::new();
    };
    let mut buf = vec![0u8; 1024];
    let n = cb(c_key.as_ptr(), buf.as_mut_ptr() as *mut c_char, buf.len());
    let len = n.min(buf.len().saturating_sub(1));
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

// ---- Logging -----------------------------------------------------------------------------------

struct LogState {
    file: Option<File>,
    path: String,
    pending: String,
    sync: bool,
    writer_started: bool,
}

impl LogState {
    const fn new() -> Self {
        Self {
            file: None,
            path: String::new(),
            pending: String::new(),
            sync: !cfg!(target_os = "android"),
            writer_started: false,
        }
    }
}

static WRITE_LOCK: Mutex<()> = Mutex::new(());
static LOG_LOCK: Mutex<LogState> = Mutex::new(LogState::new());
static LOG_CV: Condvar = Condvar::new();
static FATAL_SHOWN: AtomicBool = AtomicBool::new(false);

fn open_log_locked(state: &mut LogState) {
    if state.file.is_some() {
        return;
    }
    let env_path = std::env::var("NX_LOG").ok().filter(|s| !s.is_empty());
    let file_path = match env_path {
        Some(p) => p,
        None => {
            let root = host_call_str(host().data_root);
            if cfg!(target_os = "android") {
                format!("{root}/nx.log")
            } else {
                format!("{root}/../nx.log")
            }
        }
    };
    state.path = file_path.clone();
    let prev = format!("{file_path}.prev");
    let _ = std::fs::remove_file(&prev);
    let _ = std::fs::rename(&file_path, &prev);
    if let Ok(f) = File::create(&file_path) {
        state.file = Some(f);
    }
}

fn write_pending_locked() {
    let (lines, mut file_opt) = {
        let mut state = LOG_LOCK.lock().unwrap();
        if state.pending.is_empty() {
            return;
        }
        let lines = std::mem::take(&mut state.pending);
        let file = state.file.as_ref().and_then(|f| f.try_clone().ok());
        (lines, file)
    };
    if let Some(cb) = host().log_line {
        for line in lines.split('\n') {
            if line.is_empty() {
                continue;
            }
            if let Ok(c) = CString::new(line) {
                cb(c.as_ptr());
            }
        }
    }
    if let Some(f) = file_opt.as_mut() {
        let _ = f.write_all(lines.as_bytes());
        let _ = f.flush();
    }
}

fn log_msg(msg: &str) {
    let now = {
        let mut state = LOG_LOCK.lock().unwrap();
        open_log_locked(&mut state);
        if msg.starts_with("CRASH") || msg.starts_with("FATAL") || msg.contains("GPU hang") {
            state.sync = true;
        }
        state.pending.push_str(msg);
        state.pending.push('\n');
        let now = state.sync;
        if !now && !state.writer_started {
            state.writer_started = true;
            let _ = std::thread::Builder::new()
                .name("nx-log-writer".into())
                .spawn(|| loop {
                    {
                        let state = LOG_LOCK.lock().unwrap();
                        if state.pending.is_empty() {
                            let _ = LOG_CV
                                .wait_timeout(state, Duration::from_millis(250))
                                .unwrap();
                        }
                    }
                    let _w = WRITE_LOCK.lock().unwrap();
                    write_pending_locked();
                });
        }
        now
    };
    if now {
        let _w = WRITE_LOCK.lock().unwrap();
        write_pending_locked();
    } else {
        LOG_CV.notify_one();
    }
}

fn fatal_msg(msg: &str) -> ! {
    log_msg(&format!("FATAL: {msg}"));
    {
        let _w = WRITE_LOCK.lock().unwrap();
        write_pending_locked();
    }
    if !FATAL_SHOWN.swap(true, Ordering::SeqCst) {
        if let Some(cb) = host().show_fatal {
            if let Ok(c) = CString::new(msg) {
                cb(c.as_ptr());
            }
        }
    }
    if let Some(cb) = host().exit_process {
        cb(1);
    }
    std::process::exit(1);
}

// ---- Global runtime state ----------------------------------------------------------------------

static TRACE: AtomicBool = AtomicBool::new(false);
static VARIANT_ON: AtomicBool = AtomicBool::new(false);
static VARIANT_LO: AtomicU64 = AtomicU64::new(0);
static VARIANT_HI: AtomicU64 = AtomicU64::new(u64::MAX);

static MODULES: OnceLock<Mutex<Vec<AcnhLoadedModule>>> = OnceLock::new();
static EXPORTS: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
static REGISTRY: OnceLock<Mutex<HashMap<String, HleFn>>> = OnceLock::new();
static HOOKS: OnceLock<Mutex<HashMap<u64, HleFn>>> = OnceLock::new();
static VARIANTS: OnceLock<Mutex<HashMap<String, Vec<u32>>>> = OnceLock::new();
static HLE_CALL_OVERRIDES: OnceLock<Mutex<HashMap<u64, HleFn>>> = OnceLock::new();

struct Thunk {
    name: CString,
    fn_ptr: HleFn,
    leaf: bool,
}

struct ThunkTable {
    list: Vec<Thunk>,
    by_name: HashMap<String, u64>,
}

static THUNKS: OnceLock<Mutex<ThunkTable>> = OnceLock::new();

fn modules() -> &'static Mutex<Vec<AcnhLoadedModule>> {
    MODULES.get_or_init(|| Mutex::new(Vec::new()))
}

fn exports() -> &'static Mutex<HashMap<String, u64>> {
    EXPORTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn registry() -> &'static Mutex<HashMap<String, HleFn>> {
    REGISTRY.get_or_init(|| {
        let mut m: HashMap<String, HleFn> = HashMap::new();
        m.insert("nnMain".into(), hle_nn_main);
        m.insert("longjmp".into(), hle_long_jmp);
        m.insert("_longjmp".into(), hle_long_jmp);
        m.insert("__rtld_init".into(), hle_rtld_noop);
        m.insert("__rtld_fini".into(), hle_rtld_noop);
        m.insert("__rtld_mark".into(), hle_rtld_noop);
        m.insert("__process_return".into(), hle_process_return);
        Mutex::new(m)
    })
}

fn hooks() -> &'static Mutex<HashMap<u64, HleFn>> {
    HOOKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn variants() -> &'static Mutex<HashMap<String, Vec<u32>>> {
    VARIANTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn hle_call_overrides() -> &'static Mutex<HashMap<u64, HleFn>> {
    HLE_CALL_OVERRIDES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn thunks() -> &'static Mutex<ThunkTable> {
    THUNKS.get_or_init(|| {
        let cap = (layout().thunk_size as usize) / 16;
        Mutex::new(ThunkTable {
            list: Vec::with_capacity(cap),
            by_name: HashMap::new(),
        })
    })
}

thread_local! {
    static T_MISSING_NAME: Cell<*const c_char> = const { Cell::new(std::ptr::null()) };
}

// ---- Host libc string routines -----------------------------------------------------------------

extern "C" fn hle_memmove(c: *mut AcnhCpu) {
    // SAFETY: `c` is a valid guest CPU context; `x[0..3]` hold dst, src, count.
    unsafe {
        let cpu = &*c;
        let n = cpu.x[2] as usize;
        if n != 0 {
            std::ptr::copy(cpu.x[1] as *const u8, cpu.x[0] as *mut u8, n);
        }
    }
}

extern "C" fn hle_memset(c: *mut AcnhCpu) {
    // SAFETY: `c` is a valid guest CPU context; `x[0..3]` hold dst, val, count.
    unsafe {
        let cpu = &*c;
        let n = cpu.x[2] as usize;
        if n != 0 {
            std::ptr::write_bytes(cpu.x[0] as *mut u8, (cpu.x[1] & 0xFF) as u8, n);
        }
    }
}

extern "C" fn hle_memcmp(c: *mut AcnhCpu) {
    // SAFETY: `c` is a valid guest CPU context; `x[0..3]` hold a, b, count.
    unsafe {
        let cpu = &mut *c;
        let a = cpu.x[0] as *const u8;
        let b = cpu.x[1] as *const u8;
        let n = cpu.x[2] as usize;
        let mut k = 0usize;
        while k + 8 <= n {
            let x = std::ptr::read_unaligned(a.add(k) as *const u64);
            let y = std::ptr::read_unaligned(b.add(k) as *const u64);
            if x != y {
                break;
            }
            k += 8;
        }
        while k < n {
            let av = *a.add(k);
            let bv = *b.add(k);
            if av != bv {
                cpu.x[0] = ((av as i32) - (bv as i32)) as u32 as u64;
                return;
            }
            k += 1;
        }
        cpu.x[0] = 0;
    }
}

extern "C" fn hle_strlen(c: *mut AcnhCpu) {
    // SAFETY: `c` is a valid guest CPU context; `x[0]` holds a NUL-terminated string pointer.
    unsafe {
        let cpu = &mut *c;
        let s = CStr::from_ptr(cpu.x[0] as *const c_char);
        cpu.x[0] = s.to_bytes().len() as u64;
    }
}

fn libc_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("NX_HLE_LIBC").as_deref() != Ok("0"))
}

fn libc_lookup(name: &str) -> Option<(HleFn, bool)> {
    if !libc_on() {
        return None;
    }
    match name {
        "memcpy" | "memmove" => Some((hle_memmove, true)),
        "memset" => Some((hle_memset, true)),
        "memcmp" => Some((hle_memcmp, true)),
        "strlen" => Some((hle_strlen, true)),
        _ => None,
    }
}

fn is_leaf_fn(fn_ptr: HleFn) -> bool {
    let p = fn_ptr as *const () as usize;
    p == hle_memmove as *const () as usize
        || p == hle_memset as *const () as usize
        || p == hle_memcmp as *const () as usize
        || p == hle_strlen as *const () as usize
}

fn hle_lookup_internal(name: &str) -> Option<HleFn> {
    if let Some((fn_ptr, _)) = libc_lookup(name) {
        return Some(fn_ptr);
    }
    registry().lock().unwrap().get(name).copied()
}

extern "C" fn missing_stub(c: *mut AcnhCpu) {
    let name_ptr = T_MISSING_NAME.with(|cell| cell.get());
    let name = cstr_or_empty(name_ptr);
    // SAFETY: `c` is a valid guest CPU context.
    let lr = unsafe { (*c).x[30] };
    log_msg(&format!(
        "missing import called: {name} (lr {}) -> 0",
        describe_internal(lr)
    ));
    // SAFETY: `c` is a valid guest CPU context.
    unsafe {
        (*c).x[0] = 0;
    }
}

fn hle_address_or_internal(name: &str, fallback: HleFn) -> u64 {
    let lay = layout();
    let max_thunks = (lay.thunk_size as usize) / 16;
    let mut table = thunks().lock().unwrap();
    if let Some(&addr) = table.by_name.get(name) {
        return addr;
    }
    if table.list.len() >= max_thunks {
        drop(table);
        fatal_msg("out of HLE thunks");
    }
    let fn_opt = hle_lookup_internal(name);
    let fn_ptr = fn_opt.unwrap_or(fallback);
    let leaf = fn_opt.is_some_and(is_leaf_fn);
    let addr = lay.thunk_base + (table.list.len() as u64) * 16;
    let c_name = CString::new(name).unwrap_or_else(|_| CString::new("?").unwrap());
    table.list.push(Thunk {
        name: c_name,
        fn_ptr,
        leaf,
    });
    table.by_name.insert(name.to_owned(), addr);
    addr
}

// ---- Dispatch & fast call cache ----------------------------------------------------------------

struct FastModule {
    base: u64,
    text_size: u64,
    funcs: Vec<AcnhFuncEntry>,
    block: Vec<u32>,
}

// SAFETY: `AcnhFuncEntry` holds an offset and function pointer.
unsafe impl Send for FastModule {}
// SAFETY: `AcnhFuncEntry` holds an offset and function pointer.
unsafe impl Sync for FastModule {}

struct FastDispatch {
    modules: [Option<FastModule>; K_MAX_FAST_MODULES],
    count: usize,
    cache_base: u64,
    cache_span: u64,
    anchor: usize,
}

static FAST_DISPATCH: Mutex<FastDispatch> = Mutex::new(FastDispatch {
    modules: [const { None }; K_MAX_FAST_MODULES],
    count: 0,
    cache_base: 0,
    cache_span: 0,
    anchor: 0,
});

static CALL_CACHE: [AtomicU64; K_CALL_CACHE_SIZE] =
    [const { AtomicU64::new(0) }; K_CALL_CACHE_SIZE];

static CACHE_BASE: AtomicU64 = AtomicU64::new(0);
static CACHE_SPAN: AtomicU64 = AtomicU64::new(0);
static CALL_ANCHOR: AtomicU64 = AtomicU64::new(0);

extern "C" fn default_call_anchor(_: *mut AcnhCpu) {}

#[inline]
fn cache_slot(rel: u64) -> usize {
    let h = ((rel >> 2) as u32).wrapping_mul(0x9E37_79B1);
    (h >> (32 - K_CALL_CACHE_BITS)) as usize
}

fn build_fast_module(mi: &AcnhModuleInfo) -> FastModule {
    let mut funcs = if mi.funcs.is_null() || mi.func_count == 0 {
        Vec::new()
    } else {
        // SAFETY: `mi.funcs` is valid for `mi.func_count` entries.
        unsafe { std::slice::from_raw_parts(mi.funcs, mi.func_count) }.to_vec()
    };
    funcs.sort_by_key(|f| f.offset);
    funcs.push(AcnhFuncEntry {
        offset: u64::MAX,
        fn_ptr: None,
    });
    let blocks = (mi.text_size as usize).div_ceil(64);
    let mut block = Vec::with_capacity(blocks);
    let mut k = 0usize;
    for b in 0..blocks {
        let limit = (b as u64) * 64;
        while k < mi.func_count && funcs[k].offset < limit {
            k += 1;
        }
        block.push(k as u32);
    }
    FastModule {
        base: mi.base,
        text_size: mi.text_size,
        funcs,
        block,
    }
}

fn fast_lookup(m: &FastModule, off: u64) -> Option<HleFn> {
    let blk = (off >> 6) as usize;
    let mut idx = *m.block.get(blk)? as usize;
    while m.funcs[idx].offset < off {
        idx += 1;
    }
    if m.funcs[idx].offset == off {
        m.funcs[idx].fn_ptr
    } else {
        None
    }
}

fn module_at_internal(addr: u64) -> Option<AcnhLoadedModule> {
    let mods = modules().lock().unwrap();
    mods.iter()
        .find(|m| addr.wrapping_sub(m.base) < m.size)
        .copied()
}

fn describe_internal(addr: u64) -> String {
    let lay = layout();
    let thunk_off = addr.wrapping_sub(lay.thunk_base);
    if thunk_off < lay.thunk_size {
        let idx = (thunk_off / 16) as usize;
        let table = thunks().lock().unwrap();
        let name = table
            .list
            .get(idx)
            .and_then(|t| t.name.to_str().ok())
            .unwrap_or("?");
        return format!("hle:{name}");
    }
    let Some(lm) = module_at_internal(addr) else {
        return format!("{addr:x}");
    };
    // SAFETY: `lm.info` points to static `AcnhModuleInfo`.
    let mi = unsafe { &*lm.info };
    let mod_name = cstr_or_empty(mi.name);
    let off = addr.wrapping_sub(lm.base);
    let syms = if mi.symbols.is_null() || mi.symbol_count == 0 {
        &[]
    } else {
        // SAFETY: `mi.symbols` is valid for `mi.symbol_count` entries.
        unsafe { std::slice::from_raw_parts(mi.symbols, mi.symbol_count) }
    };
    let mut best: Option<&AcnhSymbol> = None;
    for s in syms {
        if s.imported != 0 || s.sym_type != 2 || s.value > off {
            continue;
        }
        if best.is_none_or(|b| s.value > b.value) {
            best = Some(s);
        }
    }
    if let Some(b) = best {
        if off - b.value < 0x10000 {
            return format!(
                "{mod_name}+{off:x} ({}+{:x})",
                cstr_or_empty(b.name),
                off - b.value
            );
        }
    }
    format!("{mod_name}+{off:x}")
}

pub fn hle_vtable_override_name(name: &str) -> bool {
    if name.starts_with("_ZN2nn3nfp6client") || name.starts_with("_ZN2nn3nfc6client") {
        return false;
    }
    if name.starts_with("_ZN2nn3nfp") || name.starts_with("_ZN2nn3nfc") {
        return true;
    }
    if name.starts_with("_ZN2nn3hid6system") && name.contains("Nfc") {
        return true;
    }
    if name.starts_with("_ZN2nn8settings6system") && name.contains("Nfc") {
        return true;
    }
    false
}

fn register_hle_call_overrides() {
    let mods = modules().lock().unwrap().clone();
    let mut ov = hle_call_overrides().lock().unwrap();
    ov.clear();
    let mut n = 0usize;
    for lm in mods {
        // SAFETY: `lm.info` points to static `AcnhModuleInfo`.
        let mi = unsafe { &*lm.info };
        let syms = if mi.symbols.is_null() || mi.symbol_count == 0 {
            &[]
        } else {
            // SAFETY: `mi.symbols` is valid for `mi.symbol_count` entries.
            unsafe { std::slice::from_raw_parts(mi.symbols, mi.symbol_count) }
        };
        for s in syms {
            if s.imported != 0 || s.sym_type != 2 {
                continue;
            }
            let name = cstr_or_empty(s.name);
            if name.is_empty() || !hle_vtable_override_name(name) {
                continue;
            }
            if let Some(fn_ptr) = hle_lookup_internal(name) {
                ov.insert(lm.base + s.value, fn_ptr);
                n += 1;
            }
        }
    }
    if n > 0 {
        log_msg(&format!(
            "loader: {n} NFC-related SDK entry points wired to HLE vtables"
        ));
    }
}

fn dump_regs_and_backtrace(c: *mut AcnhCpu) {
    // SAFETY: `c` is a valid guest CPU context.
    let cpu = unsafe { &*c };
    for k in (0..31).step_by(4) {
        let x3 = if k + 3 < 31 { cpu.x[k + 3] } else { cpu.x[31] };
        log_msg(&format!(
            "  x{:<2} {:016x} {:016x} {:016x} {:016x}",
            k,
            cpu.x[k],
            cpu.x[k + 1],
            cpu.x[k + 2],
            x3
        ));
    }
    if let Some(bt) = host().backtrace {
        bt(c);
    }
}

#[inline(never)]
fn call_slow(c: *mut AcnhCpu, t: u64) {
    if let Some(fn_ptr) = hle_call_overrides().lock().unwrap().get(&t).copied() {
        fn_ptr(c);
        return;
    }
    {
        let fd = FAST_DISPATCH.lock().unwrap();
        for k in 0..fd.count {
            if let Some(m) = &fd.modules[k] {
                let off = t.wrapping_sub(m.base);
                if off < m.text_size {
                    let Some(fn_ptr) = fast_lookup(m, off) else {
                        drop(fd);
                        dump_regs_and_backtrace(c);
                        // SAFETY: `c` is a valid guest CPU context.
                        let lr = unsafe { (*c).x[30] };
                        fatal_msg(&format!(
                            "call into the middle of a function: {} (lr {})",
                            describe_internal(t),
                            describe_internal(lr)
                        ));
                    };
                    let rel = t.wrapping_sub(fd.cache_base);
                    let delta = (fn_ptr as usize as isize) - (fd.anchor as isize);
                    if rel < fd.cache_span
                        && (rel & 3) == 0
                        && delta >= i32::MIN as isize
                        && delta <= i32::MAX as isize
                    {
                        let entry = (((rel >> 2) + 1) << 32) | ((delta as i32 as u32) as u64);
                        CALL_CACHE[cache_slot(rel)].store(entry, Ordering::Relaxed);
                    }
                    drop(fd);
                    fn_ptr(c);
                    return;
                }
            }
        }
    }
    let lay = layout();
    let off = t.wrapping_sub(lay.thunk_base);
    if off < lay.thunk_size {
        let idx = (off / 16) as usize;
        let thunk_info = {
            let table = thunks().lock().unwrap();
            table.list.get(idx).map(|th| {
                (
                    th.fn_ptr,
                    th.leaf,
                    th.name.as_ptr(),
                    th.name.to_string_lossy().into_owned(),
                )
            })
        };
        if let Some((fn_ptr, leaf, name_ptr, name_str)) = thunk_info {
            let trace = TRACE.load(Ordering::Relaxed);
            if leaf && !trace {
                fn_ptr(c);
                return;
            }
            if trace {
                log_msg(&format!("hle {name_str}"));
            }
            let prev = T_MISSING_NAME.with(|cell| {
                let p = cell.get();
                cell.set(name_ptr);
                p
            });
            fn_ptr(c);
            T_MISSING_NAME.with(|cell| cell.set(prev));
            return;
        }
    }
    // SAFETY: `c` is a valid guest CPU context.
    let lr = unsafe { (*c).x[30] };
    fatal_msg(&format!(
        "call to {t:x} ({}), lr {}",
        describe_internal(t),
        describe_internal(lr)
    ));
}

fn call_internal(c: *mut AcnhCpu, t: u64) {
    let base = CACHE_BASE.load(Ordering::Relaxed);
    let span = CACHE_SPAN.load(Ordering::Relaxed);
    let rel = t.wrapping_sub(base);
    if rel < span {
        let e = CALL_CACHE[cache_slot(rel)].load(Ordering::Relaxed);
        if (e >> 32) as u32 == ((rel >> 2) + 1) as u32 && (rel & 3) == 0 {
            let anchor = CALL_ANCHOR.load(Ordering::Relaxed) as usize;
            let delta = (e as u32 as i32) as isize;
            let addr = (anchor as isize).wrapping_add(delta) as usize;
            // SAFETY: `addr` was stored from a valid `HleFn` in `call_slow`.
            let fn_ptr: HleFn = unsafe { std::mem::transmute(addr) };
            fn_ptr(c);
            return;
        }
    }
    call_slow(c, t);
}

// ---- Module loading & linking ------------------------------------------------------------------

fn put_env(name: &str, value: &str) {
    // SAFETY: Single-threaded during boot before game threads spawn.
    unsafe {
        std::env::set_var(name, value);
    }
}

fn find_symbol_internal(name: &str) -> u64 {
    exports().lock().unwrap().get(name).copied().unwrap_or(0)
}

fn load_modules_internal(mods: &[*const AcnhModuleInfo], data_dir: &str, dynamic: bool) {
    let mut variant_on = std::env::var("NX_SPEEDFIX").as_deref() == Ok("1");
    let has_main_variant = variants().lock().unwrap().contains_key("main");
    if variant_on && !has_main_variant {
        log_msg("loader: frame rate 60 asked, but this game's code has no SpeedFix: 30 fps");
        variant_on = false;
        put_env("NX_SPEEDFIX", "0");
        put_env("NX_PRESENT_INTERVAL", "2");
        if let Ok(m) = std::env::var("NX_MODS") {
            let kept: Vec<&str> = m
                .split(',')
                .filter(|one| !one.is_empty() && *one != "speedfix60")
                .collect();
            put_env("NX_MODS", &kept.join(","));
        }
    }
    let mut variant_lo = 0u64;
    let mut variant_hi = u64::MAX;
    if let Ok(r) = std::env::var("NX_VARIANT_RANGE") {
        if let Some((a, b)) = r.split_once('-') {
            if let (Ok(lo), Ok(hi)) = (u64::from_str_radix(a, 16), u64::from_str_radix(b, 16)) {
                variant_lo = lo;
                variant_hi = hi;
            }
        }
    }
    let variant_code = std::env::var("NX_VARIANT_CODE").as_deref() != Ok("0");
    let words_on = std::env::var("NX_VARIANT_WORDS").as_deref() != Ok("0");
    let cache_on = std::env::var("NX_CALL_CACHE").as_deref() != Ok("0");

    let h = host();
    for &mi_ptr in mods {
        if mi_ptr.is_null() {
            continue;
        }
        // SAFETY: `mi_ptr` points to static `AcnhModuleInfo`.
        let mi = unsafe { &*mi_ptr };
        let mod_name = cstr_or_empty(mi.name);
        let image = mi.data + mi.data_size;
        let size = (mi.bss + mi.bss_size + 0xFFF) & !0xFFF;
        if let Some(commit) = h.commit {
            if !commit(mi.base, size) {
                fatal_msg(&format!("cannot commit module {mod_name}"));
            }
        }
        let path = format!("{data_dir}/{mod_name}.bin");
        let mut f = match File::open(&path) {
            Ok(f) => f,
            Err(_) => fatal_msg(&format!("missing {path} (run tools/nso2bin.py)")),
        };
        let have = f.metadata().map(|m| m.len()).unwrap_or(0);
        if have != image {
            fatal_msg(&format!(
                "{path} is from another version of the game than its recompiled code ({have:x} bytes, expected {image:x}): the game folder mixes two versions. Prepare the game again into a new, empty folder, then copy your user folder (the save) into it."
            ));
        }
        // SAFETY: `mi.base..mi.base + image` was just committed for read/write.
        let dst = unsafe { std::slice::from_raw_parts_mut(mi.base as *mut u8, image as usize) };
        if f.read_exact(dst).is_err() {
            fatal_msg(&format!("{path}: expected {image:x} bytes"));
        }
        if let Some(words) = variants().lock().unwrap().get(mod_name).cloned() {
            let count = words.len() / 2;
            if variant_on && words_on {
                for &[off, word] in words.as_chunks::<2>().0 {
                    // SAFETY: `mi.base + off` is within the committed module image.
                    unsafe {
                        std::ptr::write_unaligned((mi.base + off as u64) as *mut u32, word);
                    }
                }
            }
            log_msg(&format!(
                "loader: {mod_name}: variant {} ({count} words)",
                if variant_on { "on" } else { "off" }
            ));
        }
        if !variant_code {
            variant_on = false;
        }
        if let Some(vm_set) = h.vm_set {
            vm_set(mi.base + mi.text, mi.text_size, K_MEM_CODE, 5);
            vm_set(mi.base + mi.ro, mi.ro_size, K_MEM_CODE, 1);
            vm_set(mi.base + mi.data, size - mi.data, K_MEM_CODE_DATA, 3);
        }

        modules().lock().unwrap().push(AcnhLoadedModule {
            info: mi_ptr,
            base: mi.base,
            size,
        });

        {
            let mut fd = FAST_DISPATCH.lock().unwrap();
            if dynamic && fd.anchor == 0 && mi.func_count > 0 && !mi.funcs.is_null() {
                // SAFETY: `mi.funcs` has at least one entry.
                let first = unsafe { (*mi.funcs).fn_ptr };
                if let Some(fp) = first {
                    fd.anchor = fp as usize;
                    CALL_ANCHOR.store(fd.anchor as u64, Ordering::Relaxed);
                }
            } else if !dynamic && fd.anchor == 0 {
                fd.anchor = default_call_anchor as *const () as usize;
                CALL_ANCHOR.store(fd.anchor as u64, Ordering::Relaxed);
            }
            if fd.count < K_MAX_FAST_MODULES {
                let idx = fd.count;
                fd.modules[idx] = Some(build_fast_module(mi));
                fd.count += 1;
            }
            let mut lo = u64::MAX;
            let mut hi = 0u64;
            for m in fd.modules.iter().take(fd.count).flatten() {
                lo = lo.min(m.base);
                hi = hi.max(m.base + m.text_size);
            }
            fd.cache_span = 0;
            CACHE_SPAN.store(0, Ordering::Relaxed);
            if cache_on && hi >= lo && (hi - lo) < (1u64 << 34) {
                fd.cache_base = lo;
                fd.cache_span = hi - lo;
                CACHE_BASE.store(lo, Ordering::Relaxed);
                CACHE_SPAN.store(hi - lo, Ordering::Relaxed);
            }
            for e in &CALL_CACHE {
                e.store(0, Ordering::Relaxed);
            }
        }

        let syms = if mi.symbols.is_null() || mi.symbol_count == 0 {
            &[]
        } else {
            // SAFETY: `mi.symbols` is valid for `mi.symbol_count` entries.
            unsafe { std::slice::from_raw_parts(mi.symbols, mi.symbol_count) }
        };
        {
            let mut exp = exports().lock().unwrap();
            for s in syms {
                if s.imported != 0 || s.bind == 0 {
                    continue;
                }
                let sname = cstr_or_empty(s.name);
                if sname.is_empty() {
                    continue;
                }
                exp.entry(sname.to_owned()).or_insert(mi.base + s.value);
            }
        }
        log_msg(&format!(
            "loaded {mod_name} at {:x} ({} functions, {} relocs)",
            mi.base, mi.func_count, mi.reloc_count
        ));
    }

    VARIANT_ON.store(variant_on, Ordering::Relaxed);
    VARIANT_LO.store(variant_lo, Ordering::Relaxed);
    VARIANT_HI.store(variant_hi, Ordering::Relaxed);
    if let Some(sync) = h.sync_variant {
        sync(variant_on, variant_lo, variant_hi);
    }

    let mut hle = 0usize;
    let mut missing = 0usize;
    let mut linked = 0usize;
    let loaded = modules().lock().unwrap().clone();
    for lm in loaded {
        // SAFETY: `lm.info` points to static `AcnhModuleInfo`.
        let mi = unsafe { &*lm.info };
        let mod_name = cstr_or_empty(mi.name);
        let relocs = if mi.relocs.is_null() || mi.reloc_count == 0 {
            &[]
        } else {
            // SAFETY: `mi.relocs` is valid for `mi.reloc_count` entries.
            unsafe { std::slice::from_raw_parts(mi.relocs, mi.reloc_count) }
        };
        let syms = if mi.symbols.is_null() || mi.symbol_count == 0 {
            &[]
        } else {
            // SAFETY: `mi.symbols` is valid for `mi.symbol_count` entries.
            unsafe { std::slice::from_raw_parts(mi.symbols, mi.symbol_count) }
        };
        for r in relocs {
            let slot = (lm.base + r.offset) as *mut u64;
            if r.reloc_type == R_AARCH64_RELATIVE {
                // SAFETY: `slot` is inside committed module memory.
                unsafe {
                    *slot = lm.base.wrapping_add(r.addend as u64);
                }
                continue;
            }
            if r.reloc_type != R_AARCH64_GLOB_DAT
                && r.reloc_type != R_AARCH64_JUMP_SLOT
                && r.reloc_type != R_AARCH64_ABS64
            {
                log_msg(&format!(
                    "{mod_name}: relocation type {} at {:x} not handled",
                    r.reloc_type, r.offset
                ));
                continue;
            }
            let s = &syms[r.sym as usize];
            let sname = cstr_or_empty(s.name);
            let mut target = 0u64;
            if (s.sym_type == 2 || s.imported != 0) && hle_lookup_internal(sname).is_some() {
                target = hle_address_or_internal(sname, missing_stub);
                hle += 1;
            }
            if target == 0 {
                target = find_symbol_internal(sname);
                if target != 0 {
                    linked += 1;
                }
            }
            if target == 0 && s.imported == 0 {
                target = lm.base + s.value;
            }
            if target == 0 {
                if s.sym_type == 2 || s.sym_type == 0 {
                    target = hle_address_or_internal(sname, missing_stub);
                }
                missing += 1;
            }
            // SAFETY: `slot` is inside committed module memory.
            unsafe {
                *slot = target.wrapping_add(r.addend as u64);
            }
        }
    }
    log_msg(&format!(
        "linked: {hle} to HLE, {linked} across modules, {missing} unresolved"
    ));
    if dynamic {
        if let Some(set_var) = h.game_set_variant {
            set_var(variant_on, variant_lo, variant_hi);
        }
    }
    register_hle_call_overrides();
}

// ---- Module initializers & boot (`main.cpp`) ---------------------------------------------------

/// Read `DT_INIT` from a module's `MOD0` + `.dynamic` table, falling back to `base + 0x30`.
///
/// # Safety
/// `base..base + 0x1000` (and the `MOD0` / `.dynamic` offsets) must be readable guest memory.
pub unsafe fn dt_init_from_memory(base: u64) -> u64 {
    let mod0 = base + (std::ptr::read_unaligned((base + 4) as *const u32) as u64);
    let dyn_off = std::ptr::read_unaligned((mod0 + 4) as *const i32) as i64;
    let mut p = mod0.wrapping_add(dyn_off as u64);
    loop {
        let tag = std::ptr::read_unaligned(p as *const i64);
        if tag == 0 {
            break;
        }
        if tag == 12 {
            return base + std::ptr::read_unaligned((p + 8) as *const u64);
        }
        p += 16;
    }
    base + 0x30
}

/// Scan `DT_INIT` for `adrp xN, page` ... `ldr w8, [xN, #off]` to locate the init status word.
///
/// # Safety
/// `init..init + 0x40` must be readable guest memory.
pub unsafe fn init_status_addr_from_memory(init: u64) -> Option<u64> {
    let mut page = [0u64; 32];
    let mut pc = init;
    while pc < init + 0x40 {
        let w = std::ptr::read_unaligned(pc as *const u32);
        if (w & 0x9F00_0000) == 0x9000_0000 {
            let immlo = ((w >> 29) & 3) as i64;
            let immhi = ((w >> 5) & 0x7FFFF) as i64;
            let imm = ((immhi << 2) | immlo) << 43 >> 31;
            page[(w & 31) as usize] = (pc & !0xFFF).wrapping_add(imm as u64);
        } else if (w & 0xFFC0_001F) == 0xB940_0008 {
            let rn = ((w >> 5) & 31) as usize;
            if page[rn] != 0 {
                let off = (((w >> 10) & 0xFFF) as u64) * 4;
                return Some(page[rn] + off);
            }
        }
        pc += 4;
    }
    None
}

/// Scan `DT_INIT` GOT slots for `.tdata` start/end (slots 13 and 14).
///
/// # Safety
/// `init..init + 0x100` and the referenced GOT slots must be readable guest memory.
pub unsafe fn tdata_range_from_memory(base: u64, init: u64) -> Option<(u64, u64)> {
    let mut page = [0u64; 32];
    let mut lo = u64::MAX;
    let mut hi = 0u64;
    let mut pc = init;
    while pc < init + 0x100 {
        let w = std::ptr::read_unaligned(pc as *const u32);
        if (w & 0x9F00_0000) == 0x9000_0000 {
            let immlo = ((w >> 29) & 3) as i64;
            let immhi = ((w >> 5) & 0x7FFFF) as i64;
            let imm = ((immhi << 2) | immlo) << 43 >> 31;
            page[(w & 31) as usize] = (pc & !0xFFF).wrapping_add(imm as u64);
        } else if (w & 0xFFC0_0000) == 0xF940_0000 {
            let rn = ((w >> 5) & 31) as usize;
            if page[rn] != 0 {
                let slot = page[rn] + (((w >> 10) & 0xFFF) as u64) * 8;
                lo = lo.min(slot);
                hi = hi.max(slot);
            }
        }
        pc += 4;
    }
    if hi < lo || (hi - lo) / 8 + 1 < 15 {
        return None;
    }
    let start = std::ptr::read_unaligned((lo + 13 * 8) as *const u64);
    let end = std::ptr::read_unaligned((lo + 14 * 8) as *const u64);
    if start <= end && end - start <= 0x1000 && start >= base && end <= base + 0x1000_0000 {
        Some((start, end))
    } else {
        None
    }
}

fn call_preserving(c: *mut AcnhCpu, target: u64) {
    // SAFETY: `c` is a valid guest CPU context.
    unsafe {
        let saved: [u64; 13] = std::array::from_fn(|i| (*c).x[19 + i]);
        call_internal(c, target);
        for (i, val) in saved.into_iter().enumerate() {
            (*c).x[19 + i] = val;
        }
    }
}

fn init_module(c: *mut AcnhCpu, m: &AcnhModuleInfo) {
    // SAFETY: `m.base` is a mapped guest module.
    let init = unsafe { dt_init_from_memory(m.base) };
    call_preserving(c, init);
    // SAFETY: `init` is in the mapped guest module.
    let Some(status_addr) = (unsafe { init_status_addr_from_memory(init) }) else {
        fatal_msg(&format!(
            "{}: DT_INIT does not load its init status as expected",
            cstr_or_empty(m.name)
        ));
    };
    // SAFETY: `status_addr` points to the module's init status word in guest memory.
    let status = unsafe { std::ptr::read_unaligned(status_addr as *const u32) };
    if status == 1 && !m.init_array.is_null() {
        // SAFETY: `m.init_array` is valid for `m.init_count` entries.
        let arr = unsafe { std::slice::from_raw_parts(m.init_array, m.init_count) };
        for &off in arr {
            call_preserving(c, m.base + off);
        }
    }
    log_msg(&format!(
        "{} constructors: {}",
        cstr_or_empty(m.name),
        if status == 1 { m.init_count } else { 0 }
    ));
}

fn game_module(name: &str) -> Option<&'static AcnhModuleInfo> {
    let cb = host().game_module?;
    let c_name = CString::new(name).ok()?;
    let ptr = cb(c_name.as_ptr());
    if ptr.is_null() {
        None
    } else {
        // SAFETY: `ptr` points to static `AcnhModuleInfo`.
        Some(unsafe { &*ptr })
    }
}

extern "C" fn hle_nn_main(c: *mut AcnhCpu) {
    if let Some(sdk) = game_module("sdk") {
        init_module(c, sdk);
    }
    if let Some(sub) = game_module("subsdk0") {
        init_module(c, sub);
    }
    let Some(main_mod) = game_module("main") else {
        fatal_msg("main module missing in nnMain");
    };
    init_module(c, main_mod);
    if let Some(cb) = host().after_init {
        cb();
    }
    log_msg("entering nnMain");
    let mut real = 0u64;
    if !main_mod.symbols.is_null() && main_mod.symbol_count > 0 {
        // SAFETY: `main_mod.symbols` is valid for `main_mod.symbol_count` entries.
        let syms = unsafe { std::slice::from_raw_parts(main_mod.symbols, main_mod.symbol_count) };
        for s in syms {
            if s.imported == 0 && cstr_or_empty(s.name) == "nnMain" {
                real = main_mod.base + s.value;
            }
        }
    }
    call_internal(c, real);
}

extern "C" fn hle_long_jmp(c: *mut AcnhCpu) {
    // SAFETY: `c` is a valid guest CPU context.
    let (buf, val) = unsafe { ((*c).x[0], ((*c).x[1] as u32) as u64) };
    if let Some(cb) = host().game_longjmp {
        cb(buf, val);
    }
    fatal_msg("longjmp handler missing");
}

extern "C" fn hle_rtld_noop(_: *mut AcnhCpu) {}

extern "C" fn hle_process_return(_: *mut AcnhCpu) {
    log_msg("nn::init::Start returned");
    if let Some(cb) = host().exit_process {
        cb(0);
    }
    std::process::exit(0);
}

// ---- Public `extern "C"` ABI -------------------------------------------------------------------

/// # Safety
/// `h` must be null or point to a valid `AcnhLoaderHost`.
#[no_mangle]
pub unsafe extern "C" fn acnh_loader_set_host(h: *const AcnhLoaderHost) {
    if h.is_null() {
        return;
    }
    // SAFETY: Caller passes a valid `AcnhLoaderHost` pointer.
    let val = unsafe { *h };
    *HOST
        .get_or_init(|| Mutex::new(AcnhLoaderHost::default()))
        .lock()
        .unwrap() = val;
}

/// # Safety
/// `lay` must be null or point to a valid `AcnhLoaderLayout`.
#[no_mangle]
pub unsafe extern "C" fn acnh_loader_set_layout(lay: *const AcnhLoaderLayout) {
    if lay.is_null() {
        return;
    }
    // SAFETY: Caller passes a valid `AcnhLoaderLayout` pointer.
    let val = unsafe { *lay };
    *LAYOUT.get_or_init(|| Mutex::new(val)).lock().unwrap() = val;
}

#[no_mangle]
pub extern "C" fn acnh_loader_set_trace(on: bool) {
    TRACE.store(on, Ordering::Relaxed);
    if let Some(sync) = host().sync_trace {
        sync(on);
    }
}

#[no_mangle]
pub extern "C" fn acnh_loader_trace() -> bool {
    TRACE.load(Ordering::Relaxed)
}

#[no_mangle]
pub extern "C" fn acnh_loader_log(msg: *const c_char) {
    log_msg(cstr_or_empty(msg));
}

#[no_mangle]
pub extern "C" fn acnh_loader_log_flush() {
    let _w = WRITE_LOCK.lock().unwrap();
    write_pending_locked();
}

#[no_mangle]
pub extern "C" fn acnh_loader_log_file_path(out: *mut c_char, cap: usize) -> usize {
    let state = LOG_LOCK.lock().unwrap();
    copy_str_to_buf(&state.path, out, cap)
}

#[no_mangle]
pub extern "C" fn acnh_loader_fatal(msg: *const c_char) -> ! {
    fatal_msg(cstr_or_empty(msg));
}

#[no_mangle]
pub extern "C" fn acnh_loader_register_hle(
    name: *const c_char,
    fn_ptr: Option<extern "C" fn(*mut AcnhCpu)>,
) {
    let Some(fp) = fn_ptr else {
        return;
    };
    let s = cstr_or_empty(name);
    if !s.is_empty() {
        registry().lock().unwrap().insert(s.to_owned(), fp);
    }
}

#[no_mangle]
pub extern "C" fn acnh_loader_hle_lookup(
    name: *const c_char,
) -> Option<extern "C" fn(*mut AcnhCpu)> {
    hle_lookup_internal(cstr_or_empty(name))
}

#[no_mangle]
pub extern "C" fn acnh_loader_hle_address(name: *const c_char) -> u64 {
    hle_address_or_internal(cstr_or_empty(name), missing_stub)
}

#[no_mangle]
pub extern "C" fn acnh_loader_hle_address_or(
    name: *const c_char,
    fallback: Option<extern "C" fn(*mut AcnhCpu)>,
) -> u64 {
    hle_address_or_internal(cstr_or_empty(name), fallback.unwrap_or(missing_stub))
}

#[no_mangle]
pub extern "C" fn acnh_loader_hle_current_name() -> *const c_char {
    T_MISSING_NAME.with(|cell| cell.get())
}

#[no_mangle]
pub extern "C" fn acnh_loader_register_hook(
    offset: u64,
    fn_ptr: Option<extern "C" fn(*mut AcnhCpu)>,
) {
    if let Some(fp) = fn_ptr {
        hooks().lock().unwrap().insert(offset, fp);
    }
}

#[no_mangle]
pub extern "C" fn acnh_loader_hook(c: *mut AcnhCpu, pc: u64) {
    let off = pc.wrapping_sub(layout().module_base);
    let fn_opt = hooks().lock().unwrap().get(&off).copied();
    if let Some(fp) = fn_opt {
        fp(c);
    } else {
        static LOGGED: AtomicI32 = AtomicI32::new(0);
        if LOGGED.fetch_add(1, Ordering::Relaxed) < 8 {
            log_msg(&format!(
                "hook at {} compiled in, nothing registered for it",
                describe_internal(pc)
            ));
        }
    }
}

/// # Safety
/// `module` must be NUL-terminated and `words` must point to `count * 2` `u32` values.
#[no_mangle]
pub unsafe extern "C" fn acnh_loader_register_variant(
    module: *const c_char,
    words: *const u32,
    count: usize,
) {
    let name = cstr_or_empty(module);
    if name.is_empty() || words.is_null() || count == 0 {
        return;
    }
    // SAFETY: `words` holds `2 * count` u32s (`(offset, word)` pairs).
    let slice = unsafe { std::slice::from_raw_parts(words, count * 2) };
    variants()
        .lock()
        .unwrap()
        .insert(name.to_owned(), slice.to_vec());
}

#[no_mangle]
pub extern "C" fn acnh_loader_variant_state() -> AcnhVariantState {
    AcnhVariantState {
        on: VARIANT_ON.load(Ordering::Relaxed),
        lo: VARIANT_LO.load(Ordering::Relaxed),
        hi: VARIANT_HI.load(Ordering::Relaxed),
    }
}

/// # Safety
/// `out` must be null or point to a writable `AcnhLoadedModule`.
#[no_mangle]
pub unsafe extern "C" fn acnh_loader_module_at(addr: u64, out: *mut AcnhLoadedModule) -> bool {
    let Some(m) = module_at_internal(addr) else {
        return false;
    };
    if !out.is_null() {
        // SAFETY: `out` is a valid writable `AcnhLoadedModule` pointer.
        unsafe {
            *out = m;
        }
    }
    true
}

#[no_mangle]
pub extern "C" fn acnh_loader_describe(addr: u64, out: *mut c_char, cap: usize) -> usize {
    copy_str_to_buf(&describe_internal(addr), out, cap)
}

#[no_mangle]
pub extern "C" fn acnh_loader_find_symbol(name: *const c_char) -> u64 {
    find_symbol_internal(cstr_or_empty(name))
}

#[no_mangle]
pub extern "C" fn acnh_loader_call(c: *mut AcnhCpu, target: u64) {
    call_internal(c, target);
}

/// # Safety
/// `c` must point to a valid `AcnhCpu`.
#[no_mangle]
pub unsafe extern "C" fn acnh_loader_trace_sp(c: *mut AcnhCpu, pc: u64) {
    // SAFETY: `c` is a valid guest CPU context.
    let (sp, x19) = unsafe { ((*c).x[31], (*c).x[19]) };
    log_msg(&format!(
        "sp {sp:x} x19 {x19:x} at {}",
        describe_internal(pc)
    ));
}

/// # Safety
/// `c` must point to a valid `AcnhCpu` and `what` must be null or NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn acnh_loader_unimpl(c: *mut AcnhCpu, pc: u64, what: *const c_char) -> ! {
    dump_regs_and_backtrace(c);
    // SAFETY: `c` is a valid guest CPU context.
    let lr = unsafe { (*c).x[30] };
    fatal_msg(&format!(
        "unimplemented instruction at {}: {} (lr {})",
        describe_internal(pc),
        cstr_or_empty(what),
        describe_internal(lr)
    ));
}

/// # Safety
/// `c` must point to a valid `AcnhCpu`.
#[no_mangle]
pub unsafe extern "C" fn acnh_loader_mrs(c: *mut AcnhCpu, sysreg: u32) -> u64 {
    match sysreg {
        0xDF02 | 0xDF01 => host().system_tick.map_or(0, |f| f()),
        0xDF00 => 19_200_000,
        0xD807 => 4,
        0xC000 => 0x411F_D071,
        0xD801 => 0x8444_C004,
        _ => {
            // SAFETY: `c` is a valid guest CPU context.
            let lr = unsafe { (*c).x[30] };
            log_msg(&format!("mrs {sysreg:#x} at lr {}", describe_internal(lr)));
            0
        }
    }
}

/// # Safety
/// `c` must point to a valid `AcnhCpu`.
#[no_mangle]
pub unsafe extern "C" fn acnh_loader_msr(c: *mut AcnhCpu, sysreg: u32, value: u64) {
    // SAFETY: `c` is a valid guest CPU context.
    let lr = unsafe { (*c).x[30] };
    log_msg(&format!(
        "msr {sysreg:#x} = {value:x} at lr {}",
        describe_internal(lr)
    ));
}

/// # Safety
/// `mods` must be null or point to `count` valid `AcnhModuleInfo` pointers, and `data_dir` must be NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn acnh_loader_load_modules(
    mods: *const *const AcnhModuleInfo,
    count: usize,
    data_dir: *const c_char,
    dynamic: bool,
) {
    let slice = if mods.is_null() || count == 0 {
        &[]
    } else {
        // SAFETY: `mods` is valid for `count` pointers.
        unsafe { std::slice::from_raw_parts(mods, count) }
    };
    load_modules_internal(slice, cstr_or_empty(data_dir), dynamic);
}

#[no_mangle]
pub extern "C" fn acnh_loader_boot(dynamic: bool) {
    log_msg(&format!(
        "runtime: Rust ABI {}",
        crate::acnh_runtime_abi_version()
    ));
    let mut trace = std::env::var("NX_TRACE").is_ok_and(|v| v.starts_with('1'));
    if host_setting("trace") == "1" {
        trace = true;
    }
    acnh_loader_set_trace(trace);

    let h = host();
    if let Some(mem_init) = h.memory_init {
        mem_init();
    }
    if let Some(kern_init) = h.kernel_init {
        kern_init();
    }
    let Some(main_mod) = game_module("main") else {
        fatal_msg("main module missing at boot");
    };
    let mut mods: Vec<*const AcnhModuleInfo> = vec![main_mod as *const AcnhModuleInfo];
    if let Some(sub) = game_module("subsdk0") {
        mods.push(sub as *const AcnhModuleInfo);
    }
    if let Some(sdk) = game_module("sdk") {
        mods.push(sdk as *const AcnhModuleInfo);
    }
    let analysis_dir = format!("{}/analysis", host_call_str(h.game_root));
    load_modules_internal(&mods, &analysis_dir, dynamic);

    // SAFETY: `main_mod.base` is mapped and committed after `load_modules_internal`.
    let tdata = unsafe {
        let init = dt_init_from_memory(main_mod.base);
        tdata_range_from_memory(main_mod.base, init)
    };
    if let Some((start, end)) = tdata {
        if end > start {
            if let Some(set_tls) = h.set_tls_template {
                set_tls(start, end);
            }
            log_msg(&format!("main .tdata: {} bytes", end - start));
        }
    }

    let main_thread = h.create_main_thread.map_or(0, |f| f(16 << 20));
    let c = h.current_cpu.map_or(std::ptr::null_mut(), |f| f());
    if c.is_null() {
        fatal_msg("current_cpu returned null at boot");
    }
    // SAFETY: `c` is a valid guest CPU context.
    unsafe {
        (*c).x[0] = main_thread as u64;
        (*c).x[1] = main_mod.base;
    }
    let mut start = find_symbol_internal("_ZN2nn4init5StartEmmPFvvES2_");
    if start != 0 {
        // SAFETY: `c` is a valid guest CPU context.
        unsafe {
            (*c).x[2] = hle_address_or_internal("__rtld_init", missing_stub);
            (*c).x[3] = hle_address_or_internal("__rtld_fini", missing_stub);
        }
    } else {
        start = find_symbol_internal("_ZN2nn4init5StartEmmPFvvES2_S2_");
        if start != 0 {
            // SAFETY: `c` is a valid guest CPU context.
            unsafe {
                (*c).x[2] = hle_address_or_internal("__rtld_mark", missing_stub);
                (*c).x[3] = hle_address_or_internal("__rtld_init", missing_stub);
                (*c).x[4] = hle_address_or_internal("__rtld_fini", missing_stub);
            }
        } else {
            fatal_msg("nn::init::Start not found");
        }
    }
    // SAFETY: `c` is a valid guest CPU context.
    unsafe {
        (*c).x[30] = hle_address_or_internal("__process_return", missing_stub);
    }
    log_msg(&format!(
        "entering nn::init::Start at {}",
        describe_internal(start)
    ));
    call_internal(c, start);
    log_msg("returned from nn::init::Start");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libc_hle_string_routines() {
        let mut cpu = AcnhCpu::default();
        let mut buf = [0u8; 16];
        let src = b"hello\0";
        cpu.x[0] = buf.as_mut_ptr() as u64;
        cpu.x[1] = src.as_ptr() as u64;
        cpu.x[2] = 6;
        hle_memmove(&mut cpu);
        assert_eq!(&buf[..6], b"hello\0");

        cpu.x[0] = buf.as_mut_ptr() as u64;
        hle_strlen(&mut cpu);
        assert_eq!(cpu.x[0], 5);

        cpu.x[0] = buf.as_ptr() as u64;
        cpu.x[1] = c"helLo".as_ptr() as u64;
        cpu.x[2] = 5;
        hle_memcmp(&mut cpu);
        assert_eq!(cpu.x[0] as u32 as i32, (b'l' as i32) - (b'L' as i32));

        cpu.x[0] = buf.as_mut_ptr() as u64;
        cpu.x[1] = 0xAA;
        cpu.x[2] = 4;
        hle_memset(&mut cpu);
        assert_eq!(&buf[..4], &[0xAA, 0xAA, 0xAA, 0xAA]);
    }

    #[test]
    fn vtable_override_filter() {
        assert!(hle_vtable_override_name("_ZN2nn3nfp4InitEv"));
        assert!(hle_vtable_override_name("_ZN2nn3nfc4InitEv"));
        assert!(!hle_vtable_override_name("_ZN2nn3nfp6client4OpenEv"));
        assert!(!hle_vtable_override_name("_ZN2nn3nfc6client4OpenEv"));
        assert!(hle_vtable_override_name("_ZN2nn3hid6system10GetNfcInfoEv"));
        assert!(hle_vtable_override_name(
            "_ZN2nn8settings6system10GetNfcModeEv"
        ));
        assert!(!hle_vtable_override_name("_ZN2nn2fs9MountRomEv"));
    }

    #[test]
    fn mrs_known_sysregs() {
        let mut cpu = AcnhCpu::default();
        // SAFETY: `cpu` is a valid `AcnhCpu`.
        unsafe {
            assert_eq!(acnh_loader_mrs(&mut cpu, 0xDF00), 19_200_000);
            assert_eq!(acnh_loader_mrs(&mut cpu, 0xD807), 4);
            assert_eq!(acnh_loader_mrs(&mut cpu, 0xC000), 0x411F_D071);
            assert_eq!(acnh_loader_mrs(&mut cpu, 0xD801), 0x8444_C004);
        }
    }

    #[test]
    fn fast_module_lookup_finds_sorted_and_unsorted() {
        extern "C" fn fn_a(_: *mut AcnhCpu) {}
        extern "C" fn fn_b(_: *mut AcnhCpu) {}

        let entries = [
            AcnhFuncEntry {
                offset: 0x80,
                fn_ptr: Some(fn_b),
            },
            AcnhFuncEntry {
                offset: 0x10,
                fn_ptr: Some(fn_a),
            },
        ];
        let mi = AcnhModuleInfo {
            name: c"test".as_ptr(),
            base: 0x40_0000_0000,
            text: 0,
            text_size: 0x100,
            ro: 0,
            ro_size: 0,
            data: 0,
            data_size: 0,
            bss: 0,
            bss_size: 0,
            funcs: entries.as_ptr(),
            func_count: entries.len(),
            relocs: std::ptr::null(),
            reloc_count: 0,
            symbols: std::ptr::null(),
            symbol_count: 0,
            init_array: std::ptr::null(),
            init_count: 0,
            fini_array: std::ptr::null(),
            fini_count: 0,
        };
        let fm = build_fast_module(&mi);
        assert_eq!(
            fast_lookup(&fm, 0x10).map(|f| f as usize),
            Some(fn_a as *const () as usize)
        );
        assert_eq!(
            fast_lookup(&fm, 0x80).map(|f| f as usize),
            Some(fn_b as *const () as usize)
        );
        assert!(fast_lookup(&fm, 0x14).is_none());
    }

    #[test]
    fn dt_init_and_status_scan() {
        // Synthetic 4KB page: MOD0 at +0x10, .dynamic at +0x20 with DT_INIT(12)=0x100, DT_NULL(0).
        let mut page = vec![0u32; 1024];
        let base = page.as_mut_ptr() as u64;
        // MOD0 offset at +4 = 0x10
        page[1] = 0x10;
        // .dynamic offset relative to MOD0 at MOD0+4 = 0x10 (so .dynamic at +0x20)
        page[5] = 0x10;
        // .dynamic entry 0: tag = 12 (u64), val = 0x100 (u64) at +0x20 (u32 idx 8)
        page[8] = 12;
        page[10] = 0x100;
        // DT_INIT at +0x100 (u32 idx 64):
        // adrp x9, #0 (pc & !0xFFF): 0x9000_0009
        // ldr w8, [x9, #0x200]: 0xB942_0128 ((0x200/4) = 0x80 -> <<10 = 0x20000; rn=9 -> 9<<5=0x120)
        page[64] = 0x9000_0009;
        page[65] = 0xB940_0008 | (0x80 << 10) | (9 << 5);

        // SAFETY: `page` is a valid 4KB buffer.
        unsafe {
            let init = dt_init_from_memory(base);
            assert_eq!(init, base + 0x100);
            let status_addr = init_status_addr_from_memory(init);
            // adrp takes the page of its own pc (DT_INIT, at base + 0x100), and a Vec is not
            // page-aligned, so that page can be the one after base's.
            assert_eq!(status_addr, Some(((base + 0x100) & !0xFFF) + 0x200));
        }
    }

    #[test]
    fn hle_register_and_lookup_roundtrip() {
        extern "C" fn dummy(_: *mut AcnhCpu) {}
        acnh_loader_register_hle(c"test_hle_sym_61".as_ptr(), Some(dummy));
        let found = acnh_loader_hle_lookup(c"test_hle_sym_61".as_ptr());
        assert_eq!(found.map(|f| f as usize), Some(dummy as *const () as usize));
        let addr = acnh_loader_hle_address(c"test_hle_sym_61".as_ptr());
        assert_ne!(addr, 0);
        let mut desc = [0 as c_char; 128];
        let n = acnh_loader_describe(addr, desc.as_mut_ptr(), desc.len());
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        let s = unsafe { CStr::from_ptr(desc.as_ptr()) }.to_str().unwrap();
        assert_eq!(n, s.len());
        assert_eq!(s, "hle:test_hle_sym_61");
    }

    #[test]
    fn all_eleven_services_are_ported() {
        for s in crate::SERVICES {
            assert!(crate::ported(s), "{s:?} should be ported");
        }
    }
}
