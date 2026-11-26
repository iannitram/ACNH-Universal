//! Horizon kernel, synchronization, IPC (HIPC/CMIF/TIPC), and diagnostics in Rust.
//! Replaces `runtime/src/kernel.cpp`, `runtime/src/ipc.cpp`, and `runtime/src/kernel_watch.cpp`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

// ---- Constants & result codes ------------------------------------------------------------------

pub const HANDLE_WAIT_MASK: u32 = 0x4000_0000;
pub const PSEUDO_CURRENT_THREAD: u32 = 0xFFFF_8000;
pub const PSEUDO_CURRENT_PROCESS: u32 = 0xFFFF_8001;

pub const RC_OK: u32 = 0;
pub const RC_OUT_OF_MEMORY: u32 = 0xD001;
pub const RC_INVALID_HANDLE: u32 = 0xE401;
pub const RC_TIMEOUT: u32 = 0xEA01;
pub const RC_CANCELLED: u32 = 0xEC01;
pub const RC_INVALID_ENUM: u32 = 0xF001;
pub const RC_IPC_DOMAIN_NOT_FOUND: u32 = 0xF601;
pub const RC_INVALID_STATE: u32 = 0xFA01;

pub const MEM_FREE: u32 = 0x00;
pub const MEM_SHARED: u32 = 0x06;
pub const MEM_HEAP: u32 = 0x05;
pub const MEM_STACK: u32 = 0x0B;
pub const MEM_ALIAS: u32 = 0x0E;
pub const MEM_INACCESSIBLE: u32 = 0x10;

pub const SFCI: u32 = 0x4943_4653;
pub const SFCO: u32 = 0x4F43_4653;

// ---- C-ABI structs -----------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AcnhV128 {
    pub u64: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AcnhCpu {
    pub x: [u64; 32],
    pub v: [AcnhV128; 32],
    pub nf: u8,
    pub zf: u8,
    pub cf: u8,
    pub vf: u8,
    pub fpcr: u64,
    pub fpsr: u64,
    pub tpidr: u64,
    pub tpidrro: u64,
    pub excl_addr: u64,
    pub excl_val: u64,
    pub excl_val_hi: u64,
}

impl Default for AcnhCpu {
    fn default() -> Self {
        Self {
            x: [0; 32],
            v: [AcnhV128::default(); 32],
            nf: 0,
            zf: 0,
            cf: 0,
            vf: 0,
            fpcr: 0,
            fpsr: 0,
            tpidr: 0,
            tpidrro: 0,
            excl_addr: !0u64,
            excl_val: 0,
            excl_val_hi: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AcnhRegionBounds {
    pub module_base: u64,
    pub heap_base: u64,
    pub heap_max: u64,
    pub alias_base: u64,
    pub alias_size: u64,
    pub stack_base: u64,
    pub stack_size: u64,
    pub guest_shift: u64,
    pub compact_layout: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AcnhIpcBuffer {
    pub addr: u64,
    pub size: u64,
}

#[repr(C)]
pub struct AcnhIpcDispatch {
    pub cpu: *mut AcnhCpu,
    pub service: u64,
    pub cmd: u32,
    pub msg_type: u32,
    pub pid: u64,
    pub in_ptr: *const u8,
    pub in_size: usize,
    pub in_copy: *const u32,
    pub in_copy_len: usize,
    pub in_move: *const u32,
    pub in_move_len: usize,
    pub a_bufs: *const AcnhIpcBuffer,
    pub a_len: usize,
    pub b_bufs: *const AcnhIpcBuffer,
    pub b_len: usize,
    pub x_bufs: *const AcnhIpcBuffer,
    pub x_len: usize,
    pub c_bufs: *const AcnhIpcBuffer,
    pub c_len: usize,
    pub in_services: *const u64,
    pub in_services_len: usize,
    pub out_buf: *mut u8,
    pub out_cap: usize,
    pub out_len: *mut usize,
    pub out_copy: *mut u32,
    pub out_copy_cap: usize,
    pub out_copy_len: *mut usize,
    pub out_move: *mut u32,
    pub out_move_cap: usize,
    pub out_move_len: *mut usize,
    pub out_services: *mut u64,
    pub out_services_cap: usize,
    pub out_services_len: *mut usize,
}

#[repr(C)]
pub struct AcnhKernelHost {
    pub monotonic_ns: extern "C" fn() -> u64,
    pub sleep_ns: extern "C" fn(ns: i64),
    pub yield_thread: extern "C" fn(),
    pub current_thread_id: extern "C" fn() -> i64,
    pub set_thread_name: extern "C" fn(name: *const c_char),
    pub start_thread:
        extern "C" fn(entry: extern "C" fn(*mut c_void), arg: *mut c_void, stack_size: usize),
    pub exit_thread: extern "C" fn(),
    pub exit_process: extern "C" fn(code: i32),
    pub window_minimized: extern "C" fn() -> bool,
    pub data_root: extern "C" fn(buf: *mut c_char, cap: usize) -> usize,
    pub profile_samples: extern "C" fn(
        tids: *const i64,
        lrs: *const *const u64,
        count: usize,
        seconds: i32,
        out_path: *const c_char,
    ) -> bool,
    pub log: extern "C" fn(msg: *const c_char),
    pub fatal: extern "C" fn(msg: *const c_char),
    pub describe: extern "C" fn(addr: u64, buf: *mut c_char, cap: usize) -> usize,
    pub kernel_alloc: extern "C" fn(size: u64, align: u64) -> u64,
    pub commit: extern "C" fn(addr: u64, size: u64) -> bool,
    pub decommit: extern "C" fn(addr: u64, size: u64),
    pub vm_set: extern "C" fn(addr: u64, size: u64, state: u32, perm: u32),
    pub vm_query: extern "C" fn(
        addr: u64,
        out_addr: *mut u64,
        out_size: *mut u64,
        out_state: *mut u32,
        out_perm: *mut u32,
    ),
    pub call_guest: extern "C" fn(cpu: *mut AcnhCpu, target: u64),
    pub cpu_profile_register: extern "C" fn(name: *const c_char),
    pub init_tls_block: extern "C" fn(block: u64),
    pub get_info: extern "C" fn(info_type: u32, handle: u32, sub: u64, out_rc: *mut u32) -> u64,
    pub set_thread_core_mask:
        extern "C" fn(handle: u32, ideal: i32, mask: u64, out_core: *mut i32) -> u32,
    pub map_transfer_memory: extern "C" fn(handle: u32, addr: u64, size: u64) -> u32,
    pub unmap_transfer_memory: extern "C" fn(handle: u32, addr: u64, size: u64) -> u32,
    pub is_hades: extern "C" fn() -> bool,
    pub get_trace: extern "C" fn() -> bool,
    pub get_frame: extern "C" fn() -> u64,
    pub get_guest_base: extern "C" fn() -> u64,
    pub get_region_bounds: extern "C" fn(out: *mut AcnhRegionBounds),
    pub obj_signaled: extern "C" fn(handle: u32) -> i32,
    pub obj_consume: extern "C" fn(handle: u32),
    pub event_signal: extern "C" fn(handle: u32) -> u32,
    pub event_clear: extern "C" fn(handle: u32, reset_signal: bool) -> u32,
    pub on_thread_created:
        extern "C" fn(handle: u32, id: u64, entry: u64, tls: u64, prio: i32, core: i32),
    pub on_thread_exited: extern "C" fn(handle: u32),
    pub on_event_created: extern "C" fn(w_handle: u32, r_handle: u32),
    pub on_transfer_memory_created: extern "C" fn(handle: u32, addr: u64, size: u64),
    pub on_handle_close: extern "C" fn(handle: u32),
    pub service_make: extern "C" fn(name: *const c_char) -> u64,
    pub service_name: extern "C" fn(service: u64, buf: *mut c_char, cap: usize) -> usize,
    pub service_dispatch: extern "C" fn(dispatch: *mut AcnhIpcDispatch) -> u32,
    pub service_drop: extern "C" fn(service: u64),
    pub shared_mem_create: extern "C" fn(name: *const c_char, size: u64, host_addr: u64) -> u32,
    pub shared_mem_map: extern "C" fn(handle: u32, addr: u64, size: u64) -> u32,
}

// Safety: AcnhKernelHost only contains plain C function pointers.
unsafe impl Send for AcnhKernelHost {}
// SAFETY: the host table holds only plain function pointers installed once and never mutated afterwards, so sharing it across threads is sound.
unsafe impl Sync for AcnhKernelHost {}

static HOST: OnceLock<AcnhKernelHost> = OnceLock::new();

fn host() -> &'static AcnhKernelHost {
    HOST.get()
        .expect("acnh_kernel_set_host must be called first")
}

fn host_log(msg: &str) {
    if let Some(h) = HOST.get() {
        if let Ok(c) = CString::new(msg) {
            (h.log)(c.as_ptr());
        }
    }
}

fn host_fatal(msg: &str) -> ! {
    if let Some(h) = HOST.get() {
        if let Ok(c) = CString::new(msg) {
            (h.fatal)(c.as_ptr());
        }
    }
    std::process::abort();
}

fn host_describe(addr: u64) -> String {
    let Some(h) = HOST.get() else {
        return format!("{addr:#x}");
    };
    let mut buf = [0u8; 256];
    let len = (h.describe)(addr, buf.as_mut_ptr().cast::<c_char>(), buf.len());
    let n = len.min(buf.len().saturating_sub(1));
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

fn host_data_root() -> String {
    let Some(h) = HOST.get() else {
        return String::new();
    };
    let mut buf = [0u8; 512];
    let len = (h.data_root)(buf.as_mut_ptr().cast::<c_char>(), buf.len());
    let n = len.min(buf.len().saturating_sub(1));
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

fn region_bounds() -> AcnhRegionBounds {
    let mut b = AcnhRegionBounds::default();
    if let Some(h) = HOST.get() {
        (h.get_region_bounds)(&mut b);
    }
    b
}

// ---- Reentrant kernel lock ---------------------------------------------------------------------

struct ReentrantState {
    owner: Option<std::thread::ThreadId>,
    depth: usize,
}

struct ReentrantMutex {
    state: Mutex<ReentrantState>,
    cv: Condvar,
}

impl ReentrantMutex {
    const fn new() -> Self {
        Self {
            state: Mutex::new(ReentrantState {
                owner: None,
                depth: 0,
            }),
            cv: Condvar::new(),
        }
    }

    fn lock(&self) -> ReentrantGuard<'_> {
        let me = std::thread::current().id();
        let mut st = self.state.lock().unwrap();
        while let Some(owner) = st.owner {
            if owner == me {
                st.depth += 1;
                return ReentrantGuard { mutex: self };
            }
            st = self.cv.wait(st).unwrap();
        }
        st.owner = Some(me);
        st.depth = 1;
        ReentrantGuard { mutex: self }
    }

    fn try_lock(&self) -> Option<ReentrantGuard<'_>> {
        let me = std::thread::current().id();
        let mut st = self.state.lock().unwrap();
        match st.owner {
            None => {
                st.owner = Some(me);
                st.depth = 1;
                Some(ReentrantGuard { mutex: self })
            }
            Some(owner) if owner == me => {
                st.depth += 1;
                Some(ReentrantGuard { mutex: self })
            }
            Some(_) => None,
        }
    }

    fn unlock_one(&self) {
        let mut st = self.state.lock().unwrap();
        st.depth -= 1;
        if st.depth == 0 {
            st.owner = None;
            self.cv.notify_one();
        }
    }

    fn unlock_all(&self) -> usize {
        let mut st = self.state.lock().unwrap();
        let saved = st.depth;
        st.depth = 0;
        st.owner = None;
        self.cv.notify_one();
        saved
    }

    fn relock_all(&self, depth: usize) {
        let me = std::thread::current().id();
        let mut st = self.state.lock().unwrap();
        while st.owner.is_some() {
            st = self.cv.wait(st).unwrap();
        }
        st.owner = Some(me);
        st.depth = depth;
    }
}

struct ReentrantGuard<'a> {
    mutex: &'a ReentrantMutex,
}

impl Drop for ReentrantGuard<'_> {
    fn drop(&mut self) {
        self.mutex.unlock_one();
    }
}

static KERNEL_LOCK: ReentrantMutex = ReentrantMutex::new();

// ---- Thread & Kernel State ---------------------------------------------------------------------

struct ThreadData {
    handle: u32,
    id: u64,
    ctx: AcnhCpu,
    tls: u64,
    entry: u64,
    host_tid: i64,
    last_yield: u64,
    yields: u32,
    yb_run: u64,
    yb_regs: [u64; 10],
    last_tick_call: u64,
    tick_spins: u32,
    prio: i32,
    core: i32,
    started: bool,
    exited: bool,
    cancel: bool,
    wake: bool,
    wait_result: u32,
    wait_mutex: u64,
    wait_key: u64,
    wait_addr: u64,
    wait_tag: u32,
}

struct KThread {
    data: Mutex<ThreadData>,
    cv: Condvar,
}

impl KThread {
    #[allow(clippy::too_many_arguments)]
    fn new(
        handle: u32,
        id: u64,
        entry: u64,
        arg: u64,
        sp: u64,
        tls: u64,
        block: u64,
        prio: i32,
        core: i32,
    ) -> Arc<Self> {
        let mut ctx = AcnhCpu {
            tpidr: block,
            tpidrro: tls,
            ..Default::default()
        };
        ctx.x[0] = arg;
        ctx.x[31] = sp;
        Arc::new(Self {
            data: Mutex::new(ThreadData {
                handle,
                id,
                ctx,
                tls,
                entry,
                host_tid: 0,
                last_yield: 0,
                yields: 0,
                yb_run: 0,
                yb_regs: [0; 10],
                last_tick_call: 0,
                tick_spins: 0,
                prio,
                core,
                started: false,
                exited: false,
                cancel: false,
                wake: false,
                wait_result: 0,
                wait_mutex: 0,
                wait_key: 0,
                wait_addr: 0,
                wait_tag: 0,
            }),
            cv: Condvar::new(),
        })
    }

    fn ptr_id(&self) -> usize {
        self as *const KThread as usize
    }
}

struct KernelState {
    next_handle: u32,
    heap_size: u64,
    threads: HashMap<u32, Arc<KThread>>,
    sync_waiters: Vec<Arc<KThread>>,
    mutex_waiters: Vec<Arc<KThread>>,
    cv_waiters: Vec<Arc<KThread>>,
    addr_waiters: Vec<Arc<KThread>>,
    invalid_wait_handles: HashSet<u32>,
}

impl KernelState {
    fn new() -> Self {
        Self {
            next_handle: 0x10000,
            heap_size: 0,
            threads: HashMap::new(),
            sync_waiters: Vec::new(),
            mutex_waiters: Vec::new(),
            cv_waiters: Vec::new(),
            addr_waiters: Vec::new(),
            invalid_wait_handles: HashSet::new(),
        }
    }
}

static KSTATE: OnceLock<Mutex<KernelState>> = OnceLock::new();
static ALL_THREADS: OnceLock<Mutex<Vec<Arc<KThread>>>> = OnceLock::new();
static NEXT_THREAD_ID: AtomicU64 = AtomicU64::new(0x50);

static TICK_CALLS: AtomicU64 = AtomicU64::new(0);
static YIELD_CALLS: AtomicU64 = AtomicU64::new(0);
static YIELD_NAPS: AtomicU64 = AtomicU64::new(0);
static SLEEP_CALLS: AtomicU64 = AtomicU64::new(0);

fn kstate() -> &'static Mutex<KernelState> {
    KSTATE.get_or_init(|| Mutex::new(KernelState::new()))
}

fn all_threads() -> &'static Mutex<Vec<Arc<KThread>>> {
    ALL_THREADS.get_or_init(|| Mutex::new(Vec::new()))
}

std::thread_local! {
    static SELF_THREAD: std::cell::RefCell<Option<Arc<KThread>>> = const { std::cell::RefCell::new(None) };
    static DEFERRED_WAKES: std::cell::RefCell<Vec<Arc<KThread>>> = const { std::cell::RefCell::new(Vec::new()) };
    static SVC_ROLE: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
}

fn current_thread() -> Option<Arc<KThread>> {
    SELF_THREAD.with(|cell| cell.borrow().clone())
}

fn set_current_thread(t: Option<Arc<KThread>>) {
    SELF_THREAD.with(|cell| *cell.borrow_mut() = t);
}

// ---- Pure helpers (also unit-tested) -----------------------------------------------------------

pub const fn yield_backoff_sleep_ns(run_ns: u64, start_ns: u64, cap_ns: u64) -> u64 {
    if run_ns < start_ns {
        return 0;
    }
    let mut s = run_ns / 8;
    if s < 10_000 {
        s = 10_000;
    }
    if s > cap_ns {
        s = cap_ns;
    }
    s
}

pub fn yield_backoff_progress(prev: &[u64; 10], now: &[u64; 10]) -> bool {
    prev != now
}

pub fn parse_yield_backoff_env(val: Option<&str>) -> (bool, u64, u64) {
    let mut on = true;
    let mut start_ns = 2_000_000u64;
    let mut cap_ns = 250_000u64;
    if let Some(e) = val {
        let e = e.trim();
        if e.is_empty() || e == "0" {
            on = false;
            return (on, start_ns, cap_ns);
        }
        let mut parts = e.splitn(2, ',');
        if let Some(s_str) = parts.next() {
            if let Ok(s) = s_str.trim().parse::<u64>() {
                start_ns = s.saturating_mul(1000);
            }
        }
        if let Some(c_str) = parts.next() {
            if let Ok(mut cap) = c_str.trim().parse::<u64>() {
                if cap < 10 {
                    cap = 10;
                }
                cap_ns = cap.saturating_mul(1000);
            }
        }
    }
    (on, start_ns, cap_ns)
}

fn spin_naps_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        if let Ok(v) = std::env::var("NX_SPIN_NAPS") {
            return v.starts_with('1');
        }
        HOST.get().is_some_and(|h| (h.is_hades)())
    })
}

fn yb_config() -> (bool, u64, u64) {
    static CFG: OnceLock<(bool, u64, u64)> = OnceLock::new();
    *CFG.get_or_init(|| parse_yield_backoff_env(std::env::var("NX_YIELD_BACKOFF").ok().as_deref()))
}

fn deferred_wakes_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("NX_DEFER_WAKES").ok().as_deref() != Some("0"))
}

fn trace_addr() -> u64 {
    static ADDR: OnceLock<u64> = OnceLock::new();
    *ADDR.get_or_init(|| {
        std::env::var("NX_TRACE_ADDR")
            .ok()
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0)
    })
}

pub const fn compute_system_tick(monotonic_ns: u64) -> u64 {
    monotonic_ns.wrapping_mul(192) / 10_000
}

pub fn desc_addr_with(v: u64, base: u64, shift: u64, compact_layout: bool) -> u64 {
    if v == 0 {
        return 0;
    }
    if compact_layout {
        base.wrapping_add(v.wrapping_sub(base) & 0x7F_FFFF_FFFF)
    } else if shift == 0 {
        v
    } else {
        shift.wrapping_add(v.wrapping_sub(shift) & 0x7F_FFFF_FFFF)
    }
}

fn desc_addr(v: u64) -> u64 {
    let b = region_bounds();
    let base = if b.compact_layout {
        (host().get_guest_base)()
    } else {
        0
    };
    desc_addr_with(v, base, b.guest_shift, b.compact_layout)
}

// ---- Guest atomic memory helpers ---------------------------------------------------------------

unsafe fn load32(addr: u64) -> u32 {
    // SAFETY: callers pass a guest address that is mapped for the life of the process; AtomicU32::from_ptr needs 4-byte alignment, which futex words and handle slots have, and unaligned reads and writes need none.
    unsafe { AtomicU32::from_ptr(addr as *mut u32).load(Ordering::SeqCst) }
}

unsafe fn store32(addr: u64, val: u32) {
    // SAFETY: callers pass a guest address that is mapped for the life of the process; AtomicU32::from_ptr needs 4-byte alignment, which futex words and handle slots have, and unaligned reads and writes need none.
    unsafe { AtomicU32::from_ptr(addr as *mut u32).store(val, Ordering::SeqCst) }
}

unsafe fn cas32(addr: u64, expected: u32, new_val: u32) -> bool {
    // SAFETY: callers pass a guest address that is mapped for the life of the process and 4-byte aligned, as futex words are.
    unsafe {
        AtomicU32::from_ptr(addr as *mut u32)
            .compare_exchange(expected, new_val, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

unsafe fn ld_u8(addr: u64) -> u8 {
    // SAFETY: callers pass a guest address that is mapped for the life of the process; AtomicU32::from_ptr needs 4-byte alignment, which futex words and handle slots have, and unaligned reads and writes need none.
    unsafe { *(addr as *const u8) }
}

unsafe fn ld_u16(addr: u64) -> u16 {
    // SAFETY: callers pass a guest address that is mapped for the life of the process; AtomicU32::from_ptr needs 4-byte alignment, which futex words and handle slots have, and unaligned reads and writes need none.
    unsafe { std::ptr::read_unaligned(addr as *const u16) }
}

unsafe fn ld_u32(addr: u64) -> u32 {
    // SAFETY: callers pass a guest address that is mapped for the life of the process; AtomicU32::from_ptr needs 4-byte alignment, which futex words and handle slots have, and unaligned reads and writes need none.
    unsafe { std::ptr::read_unaligned(addr as *const u32) }
}

unsafe fn ld_u64(addr: u64) -> u64 {
    // SAFETY: callers pass a guest address that is mapped for the life of the process; AtomicU32::from_ptr needs 4-byte alignment, which futex words and handle slots have, and unaligned reads and writes need none.
    unsafe { std::ptr::read_unaligned(addr as *const u64) }
}

unsafe fn st_u32(addr: u64, v: u32) {
    // SAFETY: callers pass a guest address that is mapped for the life of the process; AtomicU32::from_ptr needs 4-byte alignment, which futex words and handle slots have, and unaligned reads and writes need none.
    unsafe { std::ptr::write_unaligned(addr as *mut u32, v) }
}

unsafe fn st_u64(addr: u64, v: u64) {
    // SAFETY: callers pass a guest address that is mapped for the life of the process; AtomicU32::from_ptr needs 4-byte alignment, which futex words and handle slots have, and unaligned reads and writes need none.
    unsafe { std::ptr::write_unaligned(addr as *mut u64, v) }
}

// ---- Deferred wakes & Wait primitives ----------------------------------------------------------

struct DeferredWakeScope;

impl DeferredWakeScope {
    fn flush() {
        let wakes: Vec<Arc<KThread>> =
            DEFERRED_WAKES.with(|cell| cell.borrow_mut().drain(..).collect());
        for t in wakes {
            t.cv.notify_one();
        }
    }
}

impl Drop for DeferredWakeScope {
    fn drop(&mut self) {
        Self::flush();
    }
}

fn wake_thread(t: &Arc<KThread>) {
    {
        let mut d = t.data.lock().unwrap();
        d.wake = true;
        d.wait_result = 0;
    }
    if deferred_wakes_on() {
        DEFERRED_WAKES.with(|cell| cell.borrow_mut().push(Arc::clone(t)));
    } else {
        t.cv.notify_one();
    }
}

fn remove_thread(vec: &mut Vec<Arc<KThread>>, target: &Arc<KThread>) {
    let id = target.ptr_id();
    vec.retain(|t| t.ptr_id() != id);
}

fn block_thread(t: &Arc<KThread>, timeout_ns: i64) -> bool {
    DeferredWakeScope::flush();
    let depth = KERNEL_LOCK.unlock_all();
    let ok = if timeout_ns < 0 {
        let mut d = t.data.lock().unwrap();
        while !d.wake {
            d = t.cv.wait(d).unwrap();
        }
        true
    } else {
        let dur = Duration::from_nanos(timeout_ns as u64);
        let deadline = Instant::now() + dur;
        let mut d = t.data.lock().unwrap();
        while !d.wake {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (guard, res) = t.cv.wait_timeout(d, deadline - now).unwrap();
            d = guard;
            if res.timed_out() && !d.wake {
                break;
            }
        }
        d.wake
    };
    KERNEL_LOCK.relock_all(depth);
    ok
}

fn unlock_mutex_locked(ks: &mut KernelState, addr: u64) {
    let mut next_idx: Option<usize> = None;
    let mut more = 0usize;
    for (idx, t) in ks.mutex_waiters.iter().enumerate() {
        if t.data.lock().unwrap().wait_mutex == addr {
            if next_idx.is_none() {
                next_idx = Some(idx);
            } else {
                more += 1;
            }
        }
    }
    let Some(idx) = next_idx else {
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        unsafe { store32(addr, 0) };
        return;
    };
    let next = ks.mutex_waiters.remove(idx);
    let tag = next.data.lock().unwrap().wait_tag;
    let val = tag | if more > 0 { HANDLE_WAIT_MASK } else { 0 };
    // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
    unsafe { store32(addr, val) };
    wake_thread(&next);
}

// ---- SVC implementations -----------------------------------------------------------------------

fn svc_arbitrate_lock(owner: u32, addr: u64, tag: u32) -> u32 {
    let _kl = KERNEL_LOCK.lock();
    let Some(self_t) = current_thread() else {
        return RC_INVALID_STATE;
    };
    // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
    if unsafe { load32(addr) } != (owner | HANDLE_WAIT_MASK) {
        return RC_OK;
    }
    {
        let mut d = self_t.data.lock().unwrap();
        d.wake = false;
        d.wait_mutex = addr;
        d.wait_tag = tag;
    }
    kstate()
        .lock()
        .unwrap()
        .mutex_waiters
        .push(Arc::clone(&self_t));
    block_thread(&self_t, -1);
    let res = self_t.data.lock().unwrap().wait_result;
    res
}

fn svc_arbitrate_unlock(addr: u64) -> u32 {
    let _wakes = DeferredWakeScope;
    let _kl = KERNEL_LOCK.lock();
    let mut ks = kstate().lock().unwrap();
    unlock_mutex_locked(&mut ks, addr);
    RC_OK
}

fn svc_wait_process_wide_key(mutex: u64, key: u64, tag: u32, timeout: i64) -> u32 {
    let _wakes = DeferredWakeScope;
    let _kl = KERNEL_LOCK.lock();
    let Some(self_t) = current_thread() else {
        return RC_INVALID_STATE;
    };
    {
        let mut d = self_t.data.lock().unwrap();
        d.wake = false;
        d.wait_mutex = mutex;
        d.wait_key = key;
        d.wait_tag = tag;
        d.wait_result = 0;
    }
    {
        let mut ks = kstate().lock().unwrap();
        ks.cv_waiters.push(Arc::clone(&self_t));
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        unsafe { store32(key, 1) };
        unlock_mutex_locked(&mut ks, mutex);
    }
    if block_thread(&self_t, timeout) {
        return self_t.data.lock().unwrap().wait_result;
    }
    let mut ks = kstate().lock().unwrap();
    remove_thread(&mut ks.cv_waiters, &self_t);
    remove_thread(&mut ks.mutex_waiters, &self_t);
    let others = ks
        .cv_waiters
        .iter()
        .any(|t| t.data.lock().unwrap().wait_key == key);
    if !others {
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        unsafe { store32(key, 0) };
    }
    RC_TIMEOUT
}

fn svc_signal_process_wide_key(key: u64, count: i32) -> u32 {
    let _wakes = DeferredWakeScope;
    let _kl = KERNEL_LOCK.lock();
    let mut ks = kstate().lock().unwrap();
    let mut n = 0i32;
    let mut i = 0usize;
    while i < ks.cv_waiters.len() && (count <= 0 || n < count) {
        let match_key = ks.cv_waiters[i].data.lock().unwrap().wait_key == key;
        if !match_key {
            i += 1;
            continue;
        }
        let t = ks.cv_waiters.remove(i);
        n += 1;
        let (wait_mutex, wait_tag) = {
            let d = t.data.lock().unwrap();
            (d.wait_mutex, d.wait_tag)
        };
        loop {
            // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
            let v = unsafe { load32(wait_mutex) };
            if v == 0 {
                // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
                if unsafe { !cas32(wait_mutex, 0, wait_tag) } {
                    continue;
                }
                wake_thread(&t);
                break;
            }
            // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
            if (v & HANDLE_WAIT_MASK) == 0 && unsafe { !cas32(wait_mutex, v, v | HANDLE_WAIT_MASK) }
            {
                continue;
            }
            ks.mutex_waiters.push(t);
            break;
        }
    }
    let others = ks
        .cv_waiters
        .iter()
        .any(|t| t.data.lock().unwrap().wait_key == key);
    if !others {
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        unsafe { store32(key, 0) };
    }
    RC_OK
}

fn svc_wait_for_address(addr: u64, wait_type: u32, value: i32, timeout: i64) -> u32 {
    let self_t = current_thread();
    if addr == trace_addr() && addr != 0 {
        let h = self_t.as_ref().map_or(0, |t| t.data.lock().unwrap().handle);
        host_log(&format!(
            "arb: thread {h:x} waits on {addr:x} type {wait_type} value {value} (memory {}) timeout {timeout}",
            // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
            unsafe { load32(addr) as i32 }
        ));
    }
    let _kl = KERNEL_LOCK.lock();
    let Some(self_t) = self_t else {
        return RC_INVALID_STATE;
    };
    // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
    let cur = unsafe { load32(addr) as i32 };
    match wait_type {
        0 => {
            if cur >= value {
                return RC_INVALID_STATE;
            }
        }
        1 => {
            if cur >= value {
                return RC_INVALID_STATE;
            }
            // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
            unsafe { store32(addr, (cur - 1) as u32) };
        }
        2 => {
            if cur != value {
                return RC_INVALID_STATE;
            }
        }
        _ => return RC_INVALID_ENUM,
    }
    if timeout == 0 {
        return RC_TIMEOUT;
    }
    {
        let mut d = self_t.data.lock().unwrap();
        d.wake = false;
        d.wait_addr = addr;
        d.wait_result = 0;
    }
    kstate()
        .lock()
        .unwrap()
        .addr_waiters
        .push(Arc::clone(&self_t));
    if !block_thread(&self_t, timeout) {
        let mut ks = kstate().lock().unwrap();
        remove_thread(&mut ks.addr_waiters, &self_t);
        return RC_TIMEOUT;
    }
    let res = self_t.data.lock().unwrap().wait_result;
    res
}

pub fn compute_signal_to_address_update(
    wait_type: u32,
    value: i32,
    count: i32,
    waiters: usize,
) -> Result<i32, u32> {
    match wait_type {
        0 => Ok(value),
        1 => Ok(value.wrapping_add(1)),
        2 => {
            let updated = if count <= 0 {
                if waiters > 0 {
                    value.wrapping_sub(2)
                } else {
                    value.wrapping_add(1)
                }
            } else if waiters > 0 {
                if waiters <= count as usize {
                    value.wrapping_sub(1)
                } else {
                    value
                }
            } else {
                value.wrapping_add(1)
            };
            Ok(updated)
        }
        _ => Err(RC_INVALID_ENUM),
    }
}

fn svc_signal_to_address(addr: u64, sig_type: u32, value: i32, count: i32) -> u32 {
    if addr == trace_addr() && addr != 0 {
        let h = current_thread().map_or(0, |t| t.data.lock().unwrap().handle);
        host_log(&format!(
            "arb: thread {h:x} signals {addr:x} type {sig_type} value {value} count {count} (memory {})",
            // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
            unsafe { load32(addr) as i32 }
        ));
    }
    let _wakes = DeferredWakeScope;
    let _kl = KERNEL_LOCK.lock();
    let mut ks = kstate().lock().unwrap();
    let waiters = ks
        .addr_waiters
        .iter()
        .filter(|t| t.data.lock().unwrap().wait_addr == addr)
        .count();
    match sig_type {
        0 => {}
        1 => {
            // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
            if unsafe { !cas32(addr, value as u32, value.wrapping_add(1) as u32) } {
                return RC_INVALID_STATE;
            }
        }
        2 => {
            let updated = match compute_signal_to_address_update(sig_type, value, count, waiters) {
                Ok(v) => v,
                Err(rc) => return rc,
            };
            // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
            if unsafe { !cas32(addr, value as u32, updated as u32) } {
                return RC_INVALID_STATE;
            }
        }
        _ => return RC_INVALID_ENUM,
    }
    let mut n = 0i32;
    let mut i = 0usize;
    while i < ks.addr_waiters.len() && (count <= 0 || n < count) {
        if ks.addr_waiters[i].data.lock().unwrap().wait_addr != addr {
            i += 1;
            continue;
        }
        let t = ks.addr_waiters.remove(i);
        wake_thread(&t);
        n += 1;
    }
    RC_OK
}

fn svc_wait_synchronization(c: &mut AcnhCpu, handles_ptr: u64, count: i32, timeout: i64) -> u32 {
    let _kl = KERNEL_LOCK.lock();
    let mut handles = Vec::with_capacity(count.max(0) as usize);
    for i in 0..count.max(0) {
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        let h = unsafe { ld_u32(handles_ptr + 4 * (i as u64)) };
        let sig = (host().obj_signaled)(h);
        if sig < 0 {
            let first_time = kstate().lock().unwrap().invalid_wait_handles.insert(h);
            if first_time {
                host_log(&format!(
                    "svcWaitSynchronization: invalid handle {h:x} (lr {})",
                    host_describe(c.x[30])
                ));
                backtrace_cpu(c);
            }
            return RC_INVALID_HANDLE;
        }
        handles.push(h);
    }

    let ready_idx = |hs: &[u32]| -> Option<usize> {
        for (i, &h) in hs.iter().enumerate() {
            if (host().obj_signaled)(h) > 0 {
                return Some(i);
            }
        }
        None
    };

    let Some(self_t) = current_thread() else {
        return RC_INVALID_STATE;
    };
    kstate()
        .lock()
        .unwrap()
        .sync_waiters
        .push(Arc::clone(&self_t));
    struct LeaveGuard(Arc<KThread>);
    impl Drop for LeaveGuard {
        fn drop(&mut self) {
            let mut ks = kstate().lock().unwrap();
            remove_thread(&mut ks.sync_waiters, &self.0);
        }
    }
    let _leave = LeaveGuard(Arc::clone(&self_t));

    let deadline = if timeout < 0 {
        None
    } else {
        Some(Instant::now() + Duration::from_nanos(timeout as u64))
    };

    loop {
        if let Some(idx) = ready_idx(&handles) {
            c.x[1] = idx as u64;
            (host().obj_consume)(handles[idx]);
            return RC_OK;
        }
        if timeout == 0 {
            return RC_TIMEOUT;
        }
        {
            let mut d = self_t.data.lock().unwrap();
            if d.cancel {
                d.cancel = false;
                return RC_CANCELLED;
            }
        }
        let depth = KERNEL_LOCK.unlock_all();
        if let Some(dl) = deadline {
            let now = Instant::now();
            if now >= dl {
                KERNEL_LOCK.relock_all(depth);
                if ready_idx(&handles).is_none() {
                    return RC_TIMEOUT;
                }
                continue;
            }
            let d = self_t.data.lock().unwrap();
            let (_g, res) = self_t.cv.wait_timeout(d, dl - now).unwrap();
            let timed_out = res.timed_out();
            drop(_g);
            KERNEL_LOCK.relock_all(depth);
            if timed_out && ready_idx(&handles).is_none() {
                return RC_TIMEOUT;
            }
        } else {
            let d = self_t.data.lock().unwrap();
            let _g = self_t.cv.wait(d).unwrap();
            drop(_g);
            KERNEL_LOCK.relock_all(depth);
        }
    }
}

// ---- Threads -----------------------------------------------------------------------------------

extern "C" fn thread_main_trampoline(p: *mut c_void) {
    // SAFETY: p came from Arc::into_raw on a KThread when the handle was created, and the handle table holds that strong count until the handle is closed.
    let t = unsafe { Arc::from_raw(p as *const KThread) };
    set_current_thread(Some(Arc::clone(&t)));
    let (id, entry, host_tid) = {
        let mut d = t.data.lock().unwrap();
        let tid = (host().current_thread_id)();
        d.host_tid = tid;
        (d.id, d.entry, tid)
    };
    let _ = host_tid;
    let name = format!("g{id:x}");
    if let Ok(c_name) = CString::new(name) {
        (host().set_thread_name)(c_name.as_ptr());
        (host().cpu_profile_register)(c_name.as_ptr());
    }
    let mut ctx_copy = t.data.lock().unwrap().ctx;
    (host().call_guest)(&mut ctx_copy, entry);
    t.data.lock().unwrap().ctx = ctx_copy;

    let _kl = KERNEL_LOCK.lock();
    let handle = {
        let mut d = t.data.lock().unwrap();
        d.exited = true;
        d.handle
    };
    (host().on_thread_exited)(handle);
    notify_all_locked();
}

fn new_thread(entry: u64, arg: u64, sp: u64, prio: i32, core: i32) -> Arc<KThread> {
    let id = NEXT_THREAD_ID.fetch_add(1, Ordering::SeqCst);
    let tls = (host().kernel_alloc)(0x1000, 0x1000);
    let block = (host().kernel_alloc)(0x4000, 0x1000);
    (host().init_tls_block)(block);
    // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
    unsafe { st_u64(tls + 0x1F0, block) };
    let handle = alloc_handle();
    let t = KThread::new(handle, id, entry, arg, sp, tls, block, prio, core);
    {
        let _kl = KERNEL_LOCK.lock();
        kstate()
            .lock()
            .unwrap()
            .threads
            .insert(handle, Arc::clone(&t));
    }
    all_threads().lock().unwrap().push(Arc::clone(&t));
    (host().on_thread_created)(handle, id, entry, tls, prio, core);
    t
}

fn alloc_handle() -> u32 {
    let _kl = KERNEL_LOCK.lock();
    let mut ks = kstate().lock().unwrap();
    let h = ks.next_handle;
    ks.next_handle = ks.next_handle.wrapping_add(1);
    h
}

fn notify_all_locked() {
    let waiters: Vec<Arc<KThread>> = kstate().lock().unwrap().sync_waiters.clone();
    for t in waiters {
        t.cv.notify_one();
    }
}

fn get_thread(handle: u32) -> Option<Arc<KThread>> {
    if handle == PSEUDO_CURRENT_THREAD {
        return current_thread();
    }
    let _kl = KERNEL_LOCK.lock();
    kstate().lock().unwrap().threads.get(&handle).cloned()
}

// ---- SVC Dispatch ------------------------------------------------------------------------------

fn write_meminfo(p: u64, addr: u64, size: u64, state: u32, perm: u32) {
    // SAFETY: `p` is a guest-supplied MemoryInfo output slot of 0x20 bytes, mapped for the life of the process; the stores are unaligned-safe.
    unsafe {
        st_u64(p, addr);
        st_u64(p + 8, size);
        st_u32(p + 16, state);
        st_u32(p + 20, 0);
        st_u32(p + 24, perm);
        st_u32(p + 28, 0);
        st_u32(p + 32, 0);
        st_u32(p + 36, 0);
    }
}

fn svc_impl(c: &mut AcnhCpu, imm: u32, pc: u64) {
    if (host().get_trace)() {
        host_log(&format!("svc {imm:#x} from {}", host_describe(pc)));
    }
    let mut rc = RC_OK;
    if imm != 0x0B {
        if let Some(t) = current_thread() {
            t.data.lock().unwrap().yb_run = 0;
        }
    }
    let bounds = region_bounds();
    match imm {
        0x01 => {
            // SetHeapSize(size) -> addr
            let size = c.x[1];
            if size > bounds.heap_max {
                rc = RC_OUT_OF_MEMORY;
            } else {
                let prev = kstate().lock().unwrap().heap_size;
                if size > prev {
                    (host().commit)(bounds.heap_base + prev, size - prev);
                }
                (host().vm_set)(bounds.heap_base, bounds.heap_max, MEM_INACCESSIBLE, 0);
                if size > 0 {
                    (host().vm_set)(bounds.heap_base, size, MEM_HEAP, 3);
                }
                if size != prev {
                    host_log(&format!(
                        "svcSetHeapSize({size:#x}) from {}",
                        host_describe(c.x[30])
                    ));
                }
                kstate().lock().unwrap().heap_size = size;
                c.x[1] = bounds.heap_base;
            }
        }
        0x02 | 0x03 => {}
        0x04 => {
            // MapMemory(dst, src, size)
            let (dst, src, size) = (c.x[0], c.x[1], c.x[2]);
            (host().commit)(dst, size);
            // SAFETY: the range is guest or caller memory of the length passed, mapped for the life of the process; the C++ dispatcher is stopped while this call runs, so nothing else writes it.
            unsafe {
                std::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, size as usize);
            }
            let st = if dst.wrapping_sub(bounds.stack_base) < bounds.stack_size {
                MEM_STACK
            } else {
                MEM_ALIAS
            };
            (host().vm_set)(dst, size, st, 3);
        }
        0x05 => {
            // UnmapMemory(dst, src, size)
            let (dst, src, size) = (c.x[0], c.x[1], c.x[2]);
            // SAFETY: the range is guest or caller memory of the length passed, mapped for the life of the process; the C++ dispatcher is stopped while this call runs, so nothing else writes it.
            unsafe {
                std::ptr::copy_nonoverlapping(dst as *const u8, src as *mut u8, size as usize);
            }
            (host().decommit)(dst, size);
            (host().vm_set)(dst, size, MEM_FREE, 0);
        }
        0x06 => {
            // QueryMemory(meminfo*, pageinfo*, addr)
            let (mut addr, mut size, mut state, mut perm) = (0, 0, 0, 0);
            (host().vm_query)(c.x[2], &mut addr, &mut size, &mut state, &mut perm);
            write_meminfo(c.x[0], addr, size, state, perm);
            c.x[1] = 0;
        }
        0x07 => {
            host_log("svcExitProcess");
            (host().exit_process)(0);
        }
        0x08 => {
            // CreateThread(entry x1, arg x2, stack_top x3, prio w4, core w5) -> handle
            let t = new_thread(c.x[1], c.x[2], c.x[3], c.x[4] as i32, c.x[5] as i32);
            let h = t.data.lock().unwrap().handle;
            c.x[1] = h as u64;
            if (host().get_trace)() {
                host_log(&format!(
                    "CreateThread {} handle {h:x}",
                    host_describe(c.x[1])
                ));
            }
        }
        0x09 => {
            // StartThread(handle)
            if let Some(t) = get_thread(c.x[0] as u32) {
                t.data.lock().unwrap().started = true;
                let raw = Arc::into_raw(t) as *mut c_void;
                (host().start_thread)(thread_main_trampoline, raw, 64usize << 20);
            } else {
                rc = RC_INVALID_HANDLE;
            }
        }
        0x0A => {
            // ExitThread
            if let Some(t) = current_thread() {
                let _kl = KERNEL_LOCK.lock();
                let handle = {
                    let mut d = t.data.lock().unwrap();
                    d.exited = true;
                    d.handle
                };
                (host().on_thread_exited)(handle);
                notify_all_locked();
            }
            (host().exit_thread)();
        }
        0x0B => {
            // SleepThread(ns)
            let ns = c.x[0] as i64;
            if ns > 0 {
                SLEEP_CALLS.fetch_add(1, Ordering::Relaxed);
                if let Some(t) = current_thread() {
                    t.data.lock().unwrap().yb_run = 0;
                }
                (host().sleep_ns)(ns);
                return;
            }
            YIELD_CALLS.fetch_add(1, Ordering::Relaxed);
            let Some(t) = current_thread() else {
                (host().yield_thread)();
                return;
            };
            let now = (host().monotonic_ns)();
            let (yb_on, start_ns, cap_ns) = yb_config();
            if !spin_naps_enabled() && yb_on {
                let mut regs = [0u64; 10];
                regs.copy_from_slice(&c.x[19..29]);
                let action = {
                    let mut d = t.data.lock().unwrap();
                    if d.yb_run == 0 || yield_backoff_progress(&d.yb_regs, &regs) {
                        d.yb_run = now;
                        d.yb_regs = regs;
                        0
                    } else {
                        yield_backoff_sleep_ns(now.saturating_sub(d.yb_run), start_ns, cap_ns)
                    }
                };
                if action > 0 {
                    YIELD_NAPS.fetch_add(1, Ordering::Relaxed);
                    (host().sleep_ns)(action as i64);
                } else {
                    (host().yield_thread)();
                }
                return;
            }
            let do_yield = {
                let mut d = t.data.lock().unwrap();
                if now.saturating_sub(d.last_yield) > 2_000_000 {
                    d.yields = 0;
                }
                if !spin_naps_enabled() {
                    true
                } else {
                    d.yields += 1;
                    d.yields <= 32
                }
            };
            if do_yield {
                (host().yield_thread)();
            } else {
                (host().sleep_ns)(500_000);
            }
            t.data.lock().unwrap().last_yield = (host().monotonic_ns)();
            return;
        }
        0x0C => {
            // GetThreadPriority(handle) -> prio
            c.x[1] = get_thread(c.x[1] as u32)
                .map_or(44u64, |t| t.data.lock().unwrap().prio as u32 as u64);
        }
        0x0D => {
            // SetThreadPriority(handle, prio)
            if let Some(t) = get_thread(c.x[0] as u32) {
                t.data.lock().unwrap().prio = c.x[1] as i32;
            }
        }
        0x0E => {
            // GetThreadCoreMask(handle) -> core, mask
            c.x[1] = get_thread(c.x[2] as u32)
                .map_or(0u64, |t| t.data.lock().unwrap().core as u32 as u64);
            c.x[2] = 0xF;
        }
        0x0F => {
            // SetThreadCoreMask(handle, ideal core, mask)
            let h = c.x[0] as u32;
            let mut core_out = 0i32;
            rc = (host().set_thread_core_mask)(h, c.x[1] as i32, c.x[2], &mut core_out);
            if let Some(t) = get_thread(h) {
                t.data.lock().unwrap().core = core_out;
            }
        }
        0x10 => {
            // GetCurrentProcessorNumber
            c.x[0] = current_thread().map_or(0u64, |t| t.data.lock().unwrap().core.max(0) as u64);
            return;
        }
        0x11 => {
            // SignalEvent(handle)
            let h = c.x[0] as u32;
            let _kl = KERNEL_LOCK.lock();
            rc = (host().event_signal)(h);
            if rc == RC_OK {
                notify_all_locked();
            }
        }
        0x12 | 0x17 => {
            // ClearEvent(handle) / ResetSignal(handle)
            let h = c.x[0] as u32;
            let _kl = KERNEL_LOCK.lock();
            rc = (host().event_clear)(h, imm == 0x17);
        }
        0x13 => {
            // MapSharedMemory(handle, addr, size, perm)
            rc = (host().shared_mem_map)(c.x[0] as u32, c.x[1], c.x[2]);
        }
        0x14 => {}
        0x15 => {
            // CreateTransferMemory(addr x1, size x2, perm w3) -> handle
            let h = alloc_handle();
            (host().on_transfer_memory_created)(h, c.x[1], c.x[2]);
            c.x[1] = h as u64;
        }
        0x16 => {
            // CloseHandle
            close_handle(c.x[0] as u32);
        }
        0x18 => {
            // WaitSynchronization
            rc = svc_wait_synchronization(c, c.x[1], c.x[2] as i32, c.x[3] as i64);
        }
        0x19 => {
            // CancelSynchronization(thread handle)
            let _kl = KERNEL_LOCK.lock();
            if let Some(t) = get_thread(c.x[0] as u32) {
                t.data.lock().unwrap().cancel = true;
            }
            notify_all_locked();
        }
        0x1A => rc = svc_arbitrate_lock(c.x[0] as u32, c.x[1], c.x[2] as u32),
        0x1B => rc = svc_arbitrate_unlock(c.x[0]),
        0x1C => rc = svc_wait_process_wide_key(c.x[0], c.x[1], c.x[2] as u32, c.x[3] as i64),
        0x1D => rc = svc_signal_process_wide_key(c.x[0], c.x[1] as i32),
        0x1E => {
            c.x[0] = compute_system_tick((host().monotonic_ns)());
            return;
        }
        0x1F => {
            // ConnectToNamedPort(name x1) -> handle
            let mut out_h = 0u64;
            rc = ipc_connect_ptr(
                c.x[1] as *const c_char,
                if c.x[1] != 0 { Some(&mut out_h) } else { None },
            );
            if c.x[1] != 0 {
                c.x[1] = out_h;
            }
        }
        0x21 => {
            // SendSyncRequest(handle)
            let tls = current_thread().map_or(0, |t| t.data.lock().unwrap().tls);
            rc = ipc_request_inner(c, c.x[0] as u32, tls, 0x100);
        }
        0x22 => {
            // SendSyncRequestWithUserBuffer(buf, size, handle)
            rc = ipc_request_inner(c, c.x[2] as u32, c.x[0], c.x[1]);
        }
        0x24 => {
            c.x[1] = 0x51;
        }
        0x25 => {
            // GetThreadId(handle) -> id
            c.x[1] = get_thread(c.x[1] as u32).map_or(0, |t| t.data.lock().unwrap().id);
        }
        0x26 => {
            // Break
            if c.x[2] == 4 && c.x[1] != 0 {
                // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
                let r = unsafe { ld_u32(c.x[1]) };
                host_log(&format!(
                    "abort with Result {r:#x} (module {}, description {})",
                    r & 0x1FF,
                    (r >> 9) & 0x1FFF
                ));
            }
            backtrace_cpu(c);
            host_fatal(&format!(
                "svcBreak({:x}, {:x}, {:x}) from {}",
                c.x[0],
                c.x[1],
                c.x[2],
                host_describe(c.x[30])
            ));
        }
        0x27 => {
            // OutputDebugString(str, len)
            if c.x[0] != 0 {
                let slice =
                    // SAFETY: the range is guest or caller memory of the length passed, mapped for the life of the process; the C++ dispatcher is stopped while this call runs, so nothing else writes it.
                    unsafe { std::slice::from_raw_parts(c.x[0] as *const u8, c.x[1] as usize) };
                let s = String::from_utf8_lossy(slice);
                let trimmed = s.trim_end_matches(['\n', '\r']);
                host_log(&format!("[guest] {trimmed}"));
            }
        }
        0x28 => {
            host_fatal(&format!("svcReturnFromException({:x})", c.x[0]));
        }
        0x29 => {
            // GetInfo(type w1, handle w2, sub x3) -> x1
            c.x[1] = (host().get_info)(c.x[1] as u32, c.x[2] as u32, c.x[3], &mut rc);
        }
        0x2A => {}
        0x2C => {
            // MapPhysicalMemory
            (host().commit)(c.x[0], c.x[1]);
            (host().vm_set)(c.x[0], c.x[1], MEM_HEAP, 3);
        }
        0x2D => {
            // UnmapPhysicalMemory
            (host().decommit)(c.x[0], c.x[1]);
            let st = if c.x[0].wrapping_sub(bounds.alias_base) < bounds.alias_size {
                MEM_FREE
            } else {
                MEM_INACCESSIBLE
            };
            (host().vm_set)(c.x[0], c.x[1], st, 0);
        }
        0x34 => rc = svc_wait_for_address(c.x[0], c.x[1] as u32, c.x[2] as i32, c.x[3] as i64),
        0x35 => rc = svc_signal_to_address(c.x[0], c.x[1] as u32, c.x[2] as i32, c.x[3] as i32),
        0x36 => {}
        0x45 => {
            // CreateEvent -> write handle x1, read handle x2
            let w = alloc_handle();
            let r = alloc_handle();
            (host().on_event_created)(w, r);
            c.x[1] = w as u64;
            c.x[2] = r as u64;
        }
        0x51 => rc = (host().map_transfer_memory)(c.x[0] as u32, c.x[1], c.x[2]),
        0x52 => rc = (host().unmap_transfer_memory)(c.x[0] as u32, c.x[1], c.x[2]),
        _ => {
            host_log(&format!(
                "unhandled svc {imm:#x} from {} (x0 {:x} x1 {:x} x2 {:x})",
                host_describe(pc),
                c.x[0],
                c.x[1],
                c.x[2]
            ));
            rc = 0;
        }
    }
    c.x[0] = rc as u64;
}

fn close_handle(h: u32) {
    {
        let _kl = KERNEL_LOCK.lock();
        kstate().lock().unwrap().threads.remove(&h);
    }
    ipc_close_session(h);
    (host().on_handle_close)(h);
}

// ---- IPC (HIPC / CMIF / TIPC) ------------------------------------------------------------------

struct DomainState {
    objects: BTreeMap<u32, u64>,
    next_object: u32,
}

impl DomainState {
    fn new() -> Self {
        Self {
            objects: BTreeMap::new(),
            next_object: 1,
        }
    }
}

struct KSession {
    service: u64,
    domain: bool,
    dom: Arc<Mutex<DomainState>>,
}

static IPC_LOCK: ReentrantMutex = ReentrantMutex::new();
static SESSIONS: OnceLock<Mutex<HashMap<u32, KSession>>> = OnceLock::new();

fn sessions() -> &'static Mutex<HashMap<u32, KSession>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn ipc_close_session(h: u32) {
    let _il = IPC_LOCK.lock();
    if let Some(sess) = sessions().lock().unwrap().remove(&h) {
        if Arc::strong_count(&sess.dom) == 1 {
            let objs: Vec<u64> = sess.dom.lock().unwrap().objects.values().copied().collect();
            for s in objs {
                if s != sess.service {
                    (host().service_drop)(s);
                }
            }
        }
        (host().service_drop)(sess.service);
    }
}

fn new_session_handle(service: u64) -> u32 {
    let h = alloc_handle();
    sessions().lock().unwrap().insert(
        h,
        KSession {
            service,
            domain: false,
            dom: Arc::new(Mutex::new(DomainState::new())),
        },
    );
    h
}

fn service_name_str(service: u64) -> String {
    let mut buf = [0u8; 64];
    let len = (host().service_name)(service, buf.as_mut_ptr().cast::<c_char>(), buf.len());
    let n = len.min(buf.len().saturating_sub(1));
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

pub struct ParsedHipcHeader {
    pub msg_type: u32,
    pub ndata: u32,
    pub pid: u64,
    pub in_copy: Vec<u32>,
    pub in_move: Vec<u32>,
    pub x: Vec<AcnhIpcBuffer>,
    pub a: Vec<AcnhIpcBuffer>,
    pub b: Vec<AcnhIpcBuffer>,
    pub c: Vec<AcnhIpcBuffer>,
    pub data_start: usize,
}

pub fn parse_hipc_words<F>(w: &[u32], fix_addr: F) -> ParsedHipcHeader
where
    F: Fn(u64) -> u64,
{
    let msg_type = w[0] & 0xFFFF;
    let nx = ((w[0] >> 16) & 0xF) as usize;
    let na = ((w[0] >> 20) & 0xF) as usize;
    let nb = ((w[0] >> 24) & 0xF) as usize;
    let nw = ((w[0] >> 28) & 0xF) as usize;
    let ndata = w[1] & 0x3FF;
    let cflags = (w[1] >> 10) & 0xF;
    let special = (w[1] >> 31) != 0;

    let mut p = 2usize;
    let mut pid = 0u64;
    let mut in_copy = Vec::new();
    let mut in_move = Vec::new();
    if special {
        let sh = w[p];
        p += 1;
        let has_pid = (sh & 1) != 0;
        let ncopy = ((sh >> 1) & 0xF) as usize;
        let nmove = ((sh >> 5) & 0xF) as usize;
        if has_pid {
            pid = (w[p] as u64) | ((w[p + 1] as u64) << 32);
            p += 2;
        }
        for _ in 0..ncopy {
            in_copy.push(w[p]);
            p += 1;
        }
        for _ in 0..nmove {
            in_move.push(w[p]);
            p += 1;
        }
    }

    let mut x = Vec::with_capacity(nx);
    for _ in 0..nx {
        let d0 = w[p];
        let d1 = w[p + 1];
        p += 2;
        let addr =
            (d1 as u64) | ((((d0 >> 12) & 0xF) as u64) << 32) | ((((d0 >> 6) & 0x7) as u64) << 36);
        x.push(AcnhIpcBuffer {
            addr: fix_addr(addr),
            size: (d0 >> 16) as u64,
        });
    }

    let mut read_abw = |n: usize| -> Vec<AcnhIpcBuffer> {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let size_lo = w[p];
            let addr_lo = w[p + 1];
            let d2 = w[p + 2];
            p += 3;
            let addr = (addr_lo as u64)
                | ((((d2 >> 28) & 0xF) as u64) << 32)
                | ((((d2 >> 2) & 0x7) as u64) << 36);
            let size = (size_lo as u64) | ((((d2 >> 24) & 0xF) as u64) << 32);
            out.push(AcnhIpcBuffer {
                addr: fix_addr(addr),
                size,
            });
        }
        out
    };

    let a = read_abw(na);
    let b = read_abw(nb);
    let _w = read_abw(nw);
    let data_start = p;

    let mut c = Vec::new();
    if cflags >= 2 {
        let mut q = data_start + ndata as usize;
        let nc = if cflags == 2 { 1 } else { cflags - 2 };
        for _ in 0..nc {
            if q + 1 < w.len() {
                let addr = (w[q] as u64) | (((w[q + 1] & 0xFFFF) as u64) << 32);
                c.push(AcnhIpcBuffer {
                    addr,
                    size: (w[q + 1] >> 16) as u64,
                });
            }
            q += 2;
        }
    }

    ParsedHipcHeader {
        msg_type,
        ndata,
        pid,
        in_copy,
        in_move,
        x,
        a,
        b,
        c,
        data_start,
    }
}

fn ipc_connect_ptr(port: *const c_char, out_handle: Option<&mut u64>) -> u32 {
    let port_str = if port.is_null() {
        String::new()
    } else {
        // SAFETY: the buffer is NUL-terminated: it is a fixed-size array we zero-initialised and the host callback filled, or a C string the caller passed.
        unsafe { CStr::from_ptr(port) }
            .to_string_lossy()
            .into_owned()
    };
    host_log(&format!("ConnectToNamedPort({port_str})"));
    let c_port = CString::new(port_str).unwrap_or_default();
    let svc = (host().service_make)(c_port.as_ptr());
    let h = new_session_handle(svc);
    if let Some(out) = out_handle {
        *out = h as u64;
    }
    RC_OK
}

fn ipc_request_inner(c: &mut AcnhCpu, handle: u32, buf: u64, _size: u64) -> u32 {
    let _il = IPC_LOCK.lock();
    let (sess_service, sess_domain, sess_dom) = {
        let map = sessions().lock().unwrap();
        let Some(sess) = map.get(&handle) else {
            host_log(&format!("ipc: request on invalid handle {handle:x}"));
            return RC_INVALID_HANDLE;
        };
        (sess.service, sess.domain, Arc::clone(&sess.dom))
    };

    // SAFETY: the range is guest or caller memory of the length passed, mapped for the life of the process; the C++ dispatcher is stopped while this call runs, so nothing else writes it.
    let w = unsafe { std::slice::from_raw_parts_mut(buf as *mut u32, 0x40) };
    let hdr = parse_hipc_words(w, desc_addr);

    if hdr.msg_type == 2 || hdr.msg_type == 15 {
        close_handle(handle);
        return RC_OK;
    }

    let mut out_buf = [0u8; 0x100];
    let mut out_len = 0usize;
    let mut out_copy = [0u32; 16];
    let mut out_copy_len = 0usize;
    let mut out_move = [0u32; 16];
    let mut out_move_len = 0usize;
    let mut out_services = [0u64; 16];
    let mut out_services_len = 0usize;

    if hdr.msg_type >= 16 {
        let cmd = hdr.msg_type - 16;
        let in_ptr = (buf + (hdr.data_start * 4) as u64) as *const u8;
        let in_size = (hdr.ndata as usize) * 4;
        if (host().get_trace)() {
            host_log(&format!(
                "ipc: tipc {} cmd {cmd}",
                service_name_str(sess_service)
            ));
        }
        let mut dispatch = AcnhIpcDispatch {
            cpu: c,
            service: sess_service,
            cmd,
            msg_type: hdr.msg_type,
            pid: hdr.pid,
            in_ptr,
            in_size,
            in_copy: hdr.in_copy.as_ptr(),
            in_copy_len: hdr.in_copy.len(),
            in_move: hdr.in_move.as_ptr(),
            in_move_len: hdr.in_move.len(),
            a_bufs: hdr.a.as_ptr(),
            a_len: hdr.a.len(),
            b_bufs: hdr.b.as_ptr(),
            b_len: hdr.b.len(),
            x_bufs: hdr.x.as_ptr(),
            x_len: hdr.x.len(),
            c_bufs: hdr.c.as_ptr(),
            c_len: hdr.c.len(),
            in_services: std::ptr::null(),
            in_services_len: 0,
            out_buf: out_buf.as_mut_ptr(),
            out_cap: out_buf.len(),
            out_len: &mut out_len,
            out_copy: out_copy.as_mut_ptr(),
            out_copy_cap: out_copy.len(),
            out_copy_len: &mut out_copy_len,
            out_move: out_move.as_mut_ptr(),
            out_move_cap: out_move.len(),
            out_move_len: &mut out_move_len,
            out_services: out_services.as_mut_ptr(),
            out_services_cap: out_services.len(),
            out_services_len: &mut out_services_len,
        };
        let rc = (host().service_dispatch)(&mut dispatch);
        for &o in &out_services[..out_services_len] {
            if out_move_len < out_move.len() {
                out_move[out_move_len] = new_session_handle(o);
                out_move_len += 1;
            }
        }
        // SAFETY: the range is guest or caller memory of the length passed, mapped for the life of the process; the C++ dispatcher is stopped while this call runs, so nothing else writes it.
        unsafe { std::ptr::write_bytes(buf as *mut u8, 0, 0x100) };
        let has_special = out_copy_len > 0 || out_move_len > 0;
        let mut q = 2usize;
        if has_special {
            w[q] = ((out_copy_len as u32) << 1) | ((out_move_len as u32) << 5);
            q += 1;
            for &h in &out_copy[..out_copy_len] {
                w[q] = h;
                q += 1;
            }
            for &h in &out_move[..out_move_len] {
                w[q] = h;
                q += 1;
            }
        }
        w[q] = rc;
        // SAFETY: the range is guest or caller memory of the length passed, mapped for the life of the process; the C++ dispatcher is stopped while this call runs, so nothing else writes it.
        unsafe {
            std::ptr::copy_nonoverlapping(
                out_buf.as_ptr(),
                (&mut w[q + 1] as *mut u32).cast::<u8>(),
                out_len,
            );
        }
        w[0] = hdr.msg_type;
        w[1] = (1 + out_len.div_ceil(4) as u32) | if has_special { 0x8000_0000 } else { 0 };
        return RC_OK;
    }

    let mut raw = (buf + (hdr.data_start * 4) as u64 + 15) & !15u64;
    if (host().get_trace)() {
        host_log(&format!(
            "ipc: request type {} on {} (handle {handle:x}, domain {}, raw word {:08x})",
            hdr.msg_type,
            service_name_str(sess_service),
            sess_domain as u8,
            // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
            unsafe { ld_u32(raw) }
        ));
    }

    let mut target = sess_service;
    let mut in_objects = Vec::new();
    if sess_domain && (hdr.msg_type == 4 || hdr.msg_type == 6) {
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        let domain_cmd = unsafe { ld_u8(raw) };
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        let nobj = unsafe { ld_u8(raw + 1) } as u32;
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        let dsize = unsafe { ld_u16(raw + 2) } as u64;
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        let domain_obj = unsafe { ld_u32(raw + 4) };
        for i in 0..nobj {
            // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
            in_objects.push(unsafe { ld_u32(raw + 16 + dsize + 4 * (i as u64)) });
        }
        raw += 16;
        if domain_cmd == 2 {
            sess_dom.lock().unwrap().objects.remove(&domain_obj);
            // SAFETY: the range is guest or caller memory of the length passed, mapped for the life of the process; the C++ dispatcher is stopped while this call runs, so nothing else writes it.
            unsafe { std::ptr::write_bytes(buf as *mut u8, 0, 0x100) };
            w[1] = 8;
            return RC_OK;
        }
        let found = sess_dom.lock().unwrap().objects.get(&domain_obj).copied();
        let Some(t) = found else {
            host_log(&format!(
                "ipc: {} domain object {domain_obj} not found",
                service_name_str(sess_service)
            ));
            return RC_IPC_DOMAIN_NOT_FOUND;
        };
        target = t;
    }

    // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
    let magic = unsafe { ld_u32(raw) };
    // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
    let cmd = unsafe { ld_u32(raw + 8) };
    let in_ptr = (raw + 16) as *const u8;
    let in_size = if hdr.ndata * 4 > 32 {
        (hdr.ndata * 4 - 32) as usize
    } else {
        0
    };
    if magic != SFCI {
        host_log(&format!(
            "ipc: {} bad magic {magic:08x} (data_start {}, ndata {})",
            service_name_str(target),
            hdr.data_start,
            hdr.ndata
        ));
    }

    let rc: u32;
    if hdr.msg_type == 5 || hdr.msg_type == 7 {
        match cmd {
            0 => {
                // ConvertCurrentObjectToDomain
                if let Some(sess) = sessions().lock().unwrap().get_mut(&handle) {
                    sess.domain = true;
                }
                let mut dom = sess_dom.lock().unwrap();
                let id = dom.next_object;
                dom.next_object += 1;
                dom.objects.insert(id, sess_service);
                out_buf[..4].copy_from_slice(&id.to_le_bytes());
                out_len = 4;
                rc = 0;
            }
            1 => rc = 0,
            2 | 4 => {
                // CloneCurrentObject / CloneCurrentObjectEx
                let clone_h = alloc_handle();
                sessions().lock().unwrap().insert(
                    clone_h,
                    KSession {
                        service: sess_service,
                        domain: sess_domain,
                        dom: Arc::clone(&sess_dom),
                    },
                );
                out_move[out_move_len] = clone_h;
                out_move_len += 1;
                rc = 0;
            }
            3 => {
                // QueryPointerBufferSize
                out_buf[..2].copy_from_slice(&0x8000u16.to_le_bytes());
                out_len = 2;
                rc = 0;
            }
            _ => {
                host_log(&format!("ipc: control cmd {cmd}"));
                rc = 0;
            }
        }
    } else {
        if (host().get_trace)() {
            host_log(&format!("ipc: {} cmd {cmd}", service_name_str(target)));
        }
        let mut in_services = Vec::new();
        if sess_domain {
            let dom = sess_dom.lock().unwrap();
            for id in in_objects {
                in_services.push(dom.objects.get(&id).copied().unwrap_or(0));
            }
        } else {
            let map = sessions().lock().unwrap();
            for &h in &hdr.in_move {
                in_services.push(map.get(&h).map_or(0, |s| s.service));
            }
        }
        let mut dispatch = AcnhIpcDispatch {
            cpu: c,
            service: target,
            cmd,
            msg_type: hdr.msg_type,
            pid: hdr.pid,
            in_ptr,
            in_size,
            in_copy: hdr.in_copy.as_ptr(),
            in_copy_len: hdr.in_copy.len(),
            in_move: hdr.in_move.as_ptr(),
            in_move_len: hdr.in_move.len(),
            a_bufs: hdr.a.as_ptr(),
            a_len: hdr.a.len(),
            b_bufs: hdr.b.as_ptr(),
            b_len: hdr.b.len(),
            x_bufs: hdr.x.as_ptr(),
            x_len: hdr.x.len(),
            c_bufs: hdr.c.as_ptr(),
            c_len: hdr.c.len(),
            in_services: in_services.as_ptr(),
            in_services_len: in_services.len(),
            out_buf: out_buf.as_mut_ptr(),
            out_cap: out_buf.len(),
            out_len: &mut out_len,
            out_copy: out_copy.as_mut_ptr(),
            out_copy_cap: out_copy.len(),
            out_copy_len: &mut out_copy_len,
            out_move: out_move.as_mut_ptr(),
            out_move_cap: out_move.len(),
            out_move_len: &mut out_move_len,
            out_services: out_services.as_mut_ptr(),
            out_services_cap: out_services.len(),
            out_services_len: &mut out_services_len,
        };
        rc = (host().service_dispatch)(&mut dispatch);
    }

    let mut out_obj_ids = Vec::new();
    let is_dom = sessions()
        .lock()
        .unwrap()
        .get(&handle)
        .is_some_and(|s| s.domain);
    for &o in &out_services[..out_services_len] {
        if is_dom {
            let mut dom = sess_dom.lock().unwrap();
            let id = dom.next_object;
            dom.next_object += 1;
            dom.objects.insert(id, o);
            out_obj_ids.push(id);
        } else if out_move_len < out_move.len() {
            out_move[out_move_len] = new_session_handle(o);
            out_move_len += 1;
        }
    }

    // SAFETY: the range is guest or caller memory of the length passed, mapped for the life of the process; the C++ dispatcher is stopped while this call runs, so nothing else writes it.
    unsafe { std::ptr::write_bytes(buf as *mut u8, 0, 0x100) };
    let has_special = out_copy_len > 0 || out_move_len > 0;
    let mut q = 2usize;
    if has_special {
        w[q] = ((out_copy_len as u32) << 1) | ((out_move_len as u32) << 5);
        q += 1;
        for &h in &out_copy[..out_copy_len] {
            w[q] = h;
            q += 1;
        }
        for &h in &out_move[..out_move_len] {
            w[q] = h;
            q += 1;
        }
    }
    let dstart = q;
    let rawo = (buf + (q * 4) as u64 + 15) & !15u64;
    let mut o = rawo;
    if is_dom && (hdr.msg_type == 4 || hdr.msg_type == 6) {
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        unsafe { st_u32(o, out_obj_ids.len() as u32) };
        o += 16;
    }
    // SAFETY: `o` is inside the guest IPC message buffer, which the guest sized for the reply header plus `out_len` bytes; it is mapped for the life of the process.
    unsafe {
        st_u32(o, SFCO);
        st_u32(o + 4, 0);
        st_u32(o + 8, rc);
        st_u32(o + 12, 0);
        std::ptr::copy_nonoverlapping(out_buf.as_ptr(), (o + 16) as *mut u8, out_len);
    }
    let mut end = o + 16 + ((out_len + 3) & !3) as u64;
    for id in out_obj_ids {
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        unsafe { st_u32(end, id) };
        end += 4;
    }
    let words = ((end - (buf + (dstart * 4) as u64)) / 4) as u32 + 4;
    w[0] = 0;
    w[1] = words | if has_special { 0x8000_0000 } else { 0 };
    RC_OK
}

// ---- Kernel watch / diagnostics ----------------------------------------------------------------

fn backtrace_cpu(c: &AcnhCpu) {
    host_log(&format!("  at lr {}", host_describe(c.x[30])));
    let mut fp = c.x[29];
    for k in 0..24 {
        if fp == 0 || (fp & 7) != 0 {
            break;
        }
        let (mut _addr, mut _size, mut state, mut _perm) = (0, 0, 0, 0);
        (host().vm_query)(fp, &mut _addr, &mut _size, &mut state, &mut _perm);
        if state == MEM_FREE {
            break;
        }
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        let lr = unsafe { ld_u64(fp + 8) };
        if lr == 0 {
            break;
        }
        host_log(&format!("  #{k:<2} {}  (fp {fp:x})", host_describe(lr)));
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        fp = unsafe { ld_u64(fp) };
    }
}

fn report_threads() {
    let all = all_threads().lock().unwrap().clone();
    let mut guard = KERNEL_LOCK.try_lock();
    for _ in 0..100 {
        if guard.is_some() {
            break;
        }
        (host().sleep_ns)(10_000_000);
        guard = KERNEL_LOCK.try_lock();
    }
    let owns_lock = guard.is_some();
    if !owns_lock {
        host_log("hang: kernel lock held by someone, reading without it");
    }
    let (mutex_w, cv_w, addr_w): (HashSet<usize>, HashSet<usize>, HashSet<usize>) = if owns_lock {
        let ks = kstate().lock().unwrap();
        (
            ks.mutex_waiters.iter().map(|t| t.ptr_id()).collect(),
            ks.cv_waiters.iter().map(|t| t.ptr_id()).collect(),
            ks.addr_waiters.iter().map(|t| t.ptr_id()).collect(),
        )
    } else {
        (HashSet::new(), HashSet::new(), HashSet::new())
    };
    for t in all {
        let (started, exited, id, handle, entry, wait_mutex, wait_key, wait_addr, ctx) = {
            let d = t.data.lock().unwrap();
            (
                d.started,
                d.exited,
                d.id,
                d.handle,
                d.entry,
                d.wait_mutex,
                d.wait_key,
                d.wait_addr,
                d.ctx,
            )
        };
        if !started || exited {
            continue;
        }
        let pid = t.ptr_id();
        let (state, on) = if owns_lock && mutex_w.contains(&pid) {
            ("waits for mutex", wait_mutex)
        } else if owns_lock && cv_w.contains(&pid) {
            ("waits on condvar", wait_key)
        } else if owns_lock && addr_w.contains(&pid) {
            ("waits on address", wait_addr)
        } else {
            ("running (guest code or a host call)", 0)
        };
        host_log(&format!(
            "hang: thread {id:x} (handle {handle:x}, entry {}): {state} {on:x}",
            host_describe(entry)
        ));
        if on != 0 && state == "waits for mutex" {
            // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
            let w = unsafe { load32(on) };
            host_log(&format!(
                "  mutex word {w:08x} (owner handle {:x})",
                w & !HANDLE_WAIT_MASK
            ));
        }
        backtrace_cpu(&ctx);
    }
}

extern "C" fn watchdog_thread(_: *mut c_void) {
    let mut last = 0u64;
    let mut since = (host().monotonic_ns)();
    let mut reported = false;
    loop {
        (host().sleep_ns)(1_000_000_000);
        let root = host_data_root();
        if !root.is_empty() {
            let dump_req = format!("{root}/dump_threads");
            if std::fs::metadata(&dump_req).is_ok() {
                let _ = std::fs::remove_file(&dump_req);
                host_log("hang: thread dump requested");
                report_threads();
            }
            let prof_req = format!("{root}/profile");
            if let Ok(contents) = std::fs::read_to_string(&prof_req) {
                let _ = std::fs::remove_file(&prof_req);
                let seconds = contents
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse::<i32>().ok())
                    .filter(|&n| n > 0)
                    .unwrap_or(10);
                let all = all_threads().lock().unwrap().clone();
                let mut tids = Vec::new();
                let mut lrs = Vec::new();
                let mut keep = Vec::new();
                for t in all {
                    let (host_tid, exited, id, entry, lr_ptr) = {
                        let d = t.data.lock().unwrap();
                        (
                            d.host_tid,
                            d.exited,
                            d.id,
                            d.entry,
                            &d.ctx.x[30] as *const u64,
                        )
                    };
                    if host_tid != 0 && !exited {
                        tids.push(host_tid);
                        lrs.push(lr_ptr);
                        host_log(&format!(
                            "profile: thread {id:x} is host tid {host_tid} (entry {})",
                            host_describe(entry)
                        ));
                        keep.push(t);
                    }
                }
                host_log(&format!(
                    "profile: sampling {} threads for {seconds} s",
                    tids.len()
                ));
                let out_path = CString::new(format!("{root}/profile.txt")).unwrap_or_default();
                let ok = (host().profile_samples)(
                    tids.as_ptr(),
                    lrs.as_ptr(),
                    tids.len(),
                    seconds,
                    out_path.as_ptr(),
                );
                host_log(&format!(
                    "profile: {}",
                    if ok {
                        "written to profile.txt"
                    } else {
                        "not available on this platform"
                    }
                ));
            }
        }
        let f = (host().get_frame)();
        let now = (host().monotonic_ns)();
        if f != last || (host().window_minimized)() {
            last = f;
            since = now;
            reported = false;
            continue;
        }
        if f != 0 && !reported && now.saturating_sub(since) > 5_000_000_000 {
            reported = true;
            host_log(&format!(
                "hang: no frame for 5 s after frame {f}; threads (two samples, 1 s apart):"
            ));
            report_threads();
            (host().sleep_ns)(1_000_000_000);
            host_log("hang: second sample");
            report_threads();
        }
    }
}

fn dump_threads() {
    let bounds = region_bounds();
    let all = all_threads().lock().unwrap().clone();
    for t in all {
        let (started, exited, id, ctx) = {
            let d = t.data.lock().unwrap();
            (d.started, d.exited, d.id, d.ctx)
        };
        if !started || exited {
            continue;
        }
        let mut chain = host_describe(ctx.x[30]);
        let mut fp = ctx.x[29];
        for _ in 0..12 {
            if fp == 0 || (fp & 7) != 0 || fp < bounds.module_base {
                break;
            }
            // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
            let lr = unsafe { ld_u64(fp + 8) };
            if lr == 0 {
                break;
            }
            chain.push_str(" < ");
            chain.push_str(&host_describe(lr));
            // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
            let next = unsafe { ld_u64(fp) };
            if next <= fp {
                break;
            }
            fp = next;
        }
        host_log(&format!("threads: {id:x} {chain}"));
    }
}

extern "C" fn stall_sampler_thread(_: *mut c_void) {
    let mut dump_at = std::env::var("NX_THREAD_DUMP_AT")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .map(|secs| (host().monotonic_ns)() + (secs * 1e9) as u64)
        .unwrap_or(0);
    let report_ms = std::env::var("NX_STALL_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(100);
    let sample_ns = report_ms.saturating_mul(600_000);
    let mut last_frame = 0u64;
    let mut since = (host().monotonic_ns)();
    let mut hits: BTreeMap<(u64, u64), u32> = BTreeMap::new();

    loop {
        (host().sleep_ns)(5_000_000);
        let f = (host().get_frame)();
        let now = (host().monotonic_ns)();
        if dump_at != 0 && now >= dump_at {
            dump_threads();
            dump_at = 0;
        }
        if f != last_frame || (host().window_minimized)() {
            let ms = now.saturating_sub(since) / 1_000_000;
            if last_frame != 0 && ms > report_ms && !hits.is_empty() {
                host_log(&format!(
                    "stall: {ms} ms without a frame after frame {last_frame}; where the threads were:"
                ));
                let mut top: BTreeMap<u64, (u64, u32)> = BTreeMap::new();
                let mut total: BTreeMap<u64, u32> = BTreeMap::new();
                for (&(tid, lr), &n) in &hits {
                    *total.entry(tid).or_insert(0) += n;
                    let e = top.entry(tid).or_insert((0, 0));
                    if n > e.1 {
                        *e = (lr, n);
                    }
                }
                for (id, (lr, count)) in top {
                    let tot = total.get(&id).copied().unwrap_or(0);
                    host_log(&format!(
                        "  thread {id:x}: {} ({count} of {tot} samples)",
                        host_describe(lr)
                    ));
                }
            }
            hits.clear();
            last_frame = f;
            since = now;
            continue;
        }
        if f == 0 || now.saturating_sub(since) < sample_ns {
            continue;
        }
        let all = all_threads().lock().unwrap().clone();
        for t in all {
            let d = t.data.lock().unwrap();
            if d.started && !d.exited {
                *hits.entry((d.id, d.ctx.x[30])).or_insert(0) += 1;
            }
        }
    }
}

// ---- Profiling ---------------------------------------------------------------------------------

type SvcProfMap = BTreeMap<(i32, u32, u64), (u64, u64)>;
static SVC_PROF_LOCK: Mutex<Option<SvcProfMap>> = Mutex::new(None);
static FRAME_SVC_NS: AtomicU64 = AtomicU64::new(0);
static FRAME_SVC_TOP_NS: AtomicU64 = AtomicU64::new(0);
static FRAME_SVC_TOP_PC: AtomicU64 = AtomicU64::new(0);
static FRAME_SVC_TOP_IMM: AtomicU32 = AtomicU32::new(0);

fn svc_profile_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("NX_SVC_PROFILE").is_some())
}

fn frame_profile_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("NX_FRAME_PROFILE").is_some())
}

// ---- C-ABI Exports -----------------------------------------------------------------------------

/// # Safety
/// `host_ptr` must be null or point to a valid `AcnhKernelHost` struct.
#[no_mangle]
pub unsafe extern "C" fn acnh_kernel_set_host(host_ptr: *const AcnhKernelHost) {
    if !host_ptr.is_null() {
        // SAFETY: host_ptr points at a live AcnhKernelHost owned by the caller for this call; we copy it by value.
        let _ = HOST.set(unsafe { std::ptr::read(host_ptr) });
    }
}

#[no_mangle]
pub extern "C" fn acnh_kernel_init() {
    (host().start_thread)(watchdog_thread, std::ptr::null_mut(), 1usize << 20);
    (host().start_thread)(stall_sampler_thread, std::ptr::null_mut(), 1usize << 20);
}

#[no_mangle]
pub extern "C" fn acnh_kernel_alloc_handle() -> u32 {
    alloc_handle()
}

#[no_mangle]
pub extern "C" fn acnh_kernel_handle_close(h: u32) {
    close_handle(h);
}

#[no_mangle]
pub extern "C" fn acnh_kernel_notify_all() {
    let _kl = KERNEL_LOCK.lock();
    notify_all_locked();
}

#[no_mangle]
pub extern "C" fn acnh_kernel_heap_size() -> u64 {
    kstate().lock().unwrap().heap_size
}

#[no_mangle]
pub extern "C" fn acnh_kernel_system_tick() -> u64 {
    compute_system_tick((host().monotonic_ns)())
}

/// # Safety
/// `c` must be null or point to a valid `AcnhCpu`.
#[no_mangle]
pub unsafe extern "C" fn acnh_kernel_hle_get_system_tick(c: *mut AcnhCpu) {
    TICK_CALLS.fetch_add(1, Ordering::Relaxed);
    if spin_naps_enabled() {
        if let Some(t) = current_thread() {
            let now = (host().monotonic_ns)();
            let sleep = {
                let mut d = t.data.lock().unwrap();
                if now.saturating_sub(d.last_tick_call) < 20_000 {
                    d.tick_spins += 1;
                    if d.tick_spins >= 50 {
                        d.tick_spins = 0;
                        true
                    } else {
                        false
                    }
                } else {
                    d.tick_spins = 0;
                    false
                }
            };
            if sleep {
                (host().sleep_ns)(500_000);
            }
            t.data.lock().unwrap().last_tick_call = (host().monotonic_ns)();
        }
    }
    // SAFETY: the C++ dispatcher passes its own CPU context, or null, which as_mut and as_ref check; it stays valid and unaliased for the duration of this call.
    if let Some(cpu) = unsafe { c.as_mut() } {
        cpu.x[0] = compute_system_tick((host().monotonic_ns)());
    }
}

#[no_mangle]
pub extern "C" fn acnh_kernel_create_main_thread(stack_size: u64) -> u32 {
    let stack = (host().kernel_alloc)(stack_size, 0x1000);
    (host().vm_set)(stack, stack_size, MEM_STACK, 3);
    let t = new_thread(0, 0, stack + stack_size, 44, 0);
    let (handle, tid) = {
        let mut d = t.data.lock().unwrap();
        d.started = true;
        let tid = (host().current_thread_id)();
        d.host_tid = tid;
        (d.handle, tid)
    };
    let _ = tid;
    set_current_thread(Some(t));
    handle
}

#[no_mangle]
pub extern "C" fn acnh_kernel_current_cpu() -> *mut AcnhCpu {
    thread_local! {
        static FALLBACK: std::cell::UnsafeCell<AcnhCpu> = std::cell::UnsafeCell::new(AcnhCpu::default());
    }
    if let Some(t) = current_thread() {
        let mut d = t.data.lock().unwrap();
        &mut d.ctx as *mut AcnhCpu
    } else {
        FALLBACK.with(|f| f.get())
    }
}

/// # Safety
/// `c` must be null or point to a valid `AcnhCpu`.
#[no_mangle]
pub unsafe extern "C" fn acnh_kernel_backtrace(c: *const AcnhCpu) {
    // SAFETY: the C++ dispatcher passes its own CPU context, or null, which as_mut and as_ref check; it stays valid and unaliased for the duration of this call.
    if let Some(cpu) = unsafe { c.as_ref() } {
        backtrace_cpu(cpu);
    }
}

/// # Safety
/// `c` must be null or point to a valid `AcnhCpu`.
#[no_mangle]
pub unsafe extern "C" fn acnh_kernel_svc(c: *mut AcnhCpu, imm: u32, pc: u64) {
    // SAFETY: the C++ dispatcher passes its own CPU context, or null, which as_mut and as_ref check; it stays valid and unaliased for the duration of this call.
    let Some(cpu) = (unsafe { c.as_mut() }) else {
        return;
    };
    let role = SVC_ROLE.with(|r| r.get());
    if role == 0 || !(svc_profile_on() || frame_profile_on()) {
        svc_impl(cpu, imm, pc);
        return;
    }
    let t0 = (host().monotonic_ns)();
    svc_impl(cpu, imm, pc);
    let dt = (host().monotonic_ns)().saturating_sub(t0);
    if frame_profile_on() && role == 2 {
        FRAME_SVC_NS.fetch_add(dt, Ordering::Relaxed);
        let prev = FRAME_SVC_TOP_NS.load(Ordering::Relaxed);
        if dt > prev {
            FRAME_SVC_TOP_NS.store(dt, Ordering::Relaxed);
            FRAME_SVC_TOP_IMM.store(imm, Ordering::Relaxed);
            FRAME_SVC_TOP_PC.store(pc, Ordering::Relaxed);
        }
        if !svc_profile_on() {
            return;
        }
    }
    if dt >= 30_000 {
        let mut map_guard = SVC_PROF_LOCK.lock().unwrap();
        let map = map_guard.get_or_insert_with(BTreeMap::new);
        let e = map.entry((role, imm, pc)).or_insert((0, 0));
        e.0 += dt;
        e.1 += 1;
    }
}

#[no_mangle]
pub extern "C" fn acnh_kernel_svc_profile_mark(role: i32) {
    SVC_ROLE.with(|r| r.set(role));
}

#[no_mangle]
pub extern "C" fn acnh_kernel_svc_profile_report(frames: u64, role: i32) {
    if !svc_profile_on() || frames == 0 {
        return;
    }
    let mut map_guard = SVC_PROF_LOCK.lock().unwrap();
    let Some(map) = map_guard.as_mut() else {
        return;
    };
    let mut entries: Vec<(u64, (i32, u32, u64), u64)> = map
        .iter()
        .filter(|(k, _)| k.0 == role)
        .map(|(&k, &val)| (val.0, k, val.1))
        .collect();
    entries.sort_by_key(|a| std::cmp::Reverse(a.0));
    let total: u64 = entries.iter().map(|e| e.0).sum();
    host_log(&format!(
        "svc profile: role {role} thread blocked {:.2} ms per frame in calls over 30 us (over {frames} frames)",
        (total as f64) / 1e6 / (frames as f64)
    ));
    for (ns, (_, imm, pc), calls) in entries.iter().take(8) {
        host_log(&format!(
            "svc profile: role {role}: {:.2} ms/frame, {:.1} calls/frame: svc {imm:#x} from {}",
            (*ns as f64) / 1e6 / (frames as f64),
            (*calls as f64) / (frames as f64),
            host_describe(*pc)
        ));
    }
    map.retain(|k, _| k.0 != role);
}

/// # Safety
/// Each non-null pointer must point to a valid writable `u64`/`u32`.
#[no_mangle]
pub unsafe extern "C" fn acnh_kernel_svc_frame_take(
    total: *mut u64,
    top_ns: *mut u64,
    top_imm: *mut u32,
    top_pc: *mut u64,
) {
    // SAFETY: each out-pointer is checked for null first; the C++ caller passes valid u64 slots for the others.
    unsafe {
        if !total.is_null() {
            *total = FRAME_SVC_NS.swap(0, Ordering::Relaxed);
        }
        if !top_ns.is_null() {
            *top_ns = FRAME_SVC_TOP_NS.swap(0, Ordering::Relaxed);
        }
        if !top_imm.is_null() {
            *top_imm = FRAME_SVC_TOP_IMM.load(Ordering::Relaxed);
        }
        if !top_pc.is_null() {
            *top_pc = FRAME_SVC_TOP_PC.load(Ordering::Relaxed);
        }
    }
}

/// # Safety
/// Each non-null pointer must point to a valid writable `u64`.
#[no_mangle]
pub unsafe extern "C" fn acnh_kernel_counters(
    tick_calls: *mut u64,
    yield_calls: *mut u64,
    yield_naps: *mut u64,
    sleep_calls: *mut u64,
) {
    // SAFETY: each out-pointer is checked for null first; the C++ caller passes valid u64 slots for the others.
    unsafe {
        if !tick_calls.is_null() {
            *tick_calls = TICK_CALLS.load(Ordering::Relaxed);
        }
        if !yield_calls.is_null() {
            *yield_calls = YIELD_CALLS.load(Ordering::Relaxed);
        }
        if !yield_naps.is_null() {
            *yield_naps = YIELD_NAPS.load(Ordering::Relaxed);
        }
        if !sleep_calls.is_null() {
            *sleep_calls = SLEEP_CALLS.load(Ordering::Relaxed);
        }
    }
}

/// # Safety
/// `port` must be null or a valid NUL-terminated string, and `out_handle` null or writable.
#[no_mangle]
pub unsafe extern "C" fn acnh_kernel_ipc_connect(port: *const c_char, out_handle: *mut u64) -> u32 {
    let mut h = 0u64;
    let rc = ipc_connect_ptr(port, Some(&mut h));
    if !out_handle.is_null() {
        // SAFETY: the C++ caller passes a writable pointer to a u32 or u64 slot (or null where the code checks first), valid for this call.
        unsafe { *out_handle = h };
    }
    rc
}

/// # Safety
/// `c` must be null or point to a valid `AcnhCpu`, and `buf` must point to a valid guest IPC buffer.
#[no_mangle]
pub unsafe extern "C" fn acnh_kernel_ipc_request(
    c: *mut AcnhCpu,
    handle: u32,
    buf: u64,
    size: u64,
) -> u32 {
    // SAFETY: the C++ dispatcher passes its own CPU context, or null, which as_mut and as_ref check; it stays valid and unaliased for the duration of this call.
    let Some(cpu) = (unsafe { c.as_mut() }) else {
        return RC_INVALID_STATE;
    };
    ipc_request_inner(cpu, handle, buf, size)
}

#[no_mangle]
pub extern "C" fn acnh_kernel_make_shared_memory(name: *const c_char, size: u64) -> u32 {
    let host_addr = (host().kernel_alloc)(size, 0x1000);
    (host().shared_mem_create)(name, size, host_addr)
}

#[no_mangle]
pub extern "C" fn acnh_kernel_map_shared_memory(
    addr: u64,
    size: u64,
    host_addr: u64,
    shm_size: u64,
) -> u32 {
    (host().commit)(addr, size);
    (host().vm_set)(addr, size, MEM_SHARED, 3);
    let copy_len = size.min(shm_size) as usize;
    // SAFETY: the range is guest or caller memory of the length passed, mapped for the life of the process; the C++ dispatcher is stopped while this call runs, so nothing else writes it.
    unsafe {
        std::ptr::copy_nonoverlapping(host_addr as *const u8, addr as *mut u8, copy_len);
    }
    RC_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yield_backoff_matches_nxrt_table() {
        assert_eq!(yield_backoff_sleep_ns(999, 1000, 250_000), 0);
        assert_eq!(yield_backoff_sleep_ns(1000, 1000, 250_000), 10_000);
        assert_eq!(yield_backoff_sleep_ns(800_000, 1000, 250_000), 100_000);
        assert_eq!(yield_backoff_sleep_ns(100_000_000, 1000, 250_000), 250_000);

        let a = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let b = [1, 2, 3, 4, 5, 6, 7, 8, 9, 11];
        assert!(!yield_backoff_progress(&a, &a));
        assert!(yield_backoff_progress(&a, &b));
    }

    #[test]
    fn yield_backoff_env_parsing() {
        assert_eq!(parse_yield_backoff_env(None), (true, 2_000_000, 250_000));
        assert_eq!(
            parse_yield_backoff_env(Some("0")),
            (false, 2_000_000, 250_000)
        );
        assert_eq!(
            parse_yield_backoff_env(Some("")),
            (false, 2_000_000, 250_000)
        );
        assert_eq!(
            parse_yield_backoff_env(Some("500,5")),
            (true, 500_000, 10_000)
        );
        assert_eq!(
            parse_yield_backoff_env(Some("3000,120")),
            (true, 3_000_000, 120_000)
        );
    }

    #[test]
    fn system_tick_is_19_2_mhz() {
        assert_eq!(compute_system_tick(0), 0);
        assert_eq!(compute_system_tick(1_000_000_000), 19_200_000);
    }

    #[test]
    fn desc_addr_handles_shift_and_compact() {
        assert_eq!(desc_addr_with(0, 0, 0, false), 0);
        assert_eq!(desc_addr_with(0x40_1234_5678, 0, 0, false), 0x40_1234_5678);
        // macOS shift: 0xC0_0000_0000 + 0x10_1234_5678 -> low 39 bits only -> restored
        let shift = 0xC0_0000_0000u64;
        let full = shift + 0x10_1234_5678u64;
        let truncated = full & 0x7F_FFFF_FFFF;
        assert_eq!(desc_addr_with(truncated, 0, shift, false), full);
        // iOS compact layout
        let base = 0x1_4000_0000u64;
        let compact_full = base + 0x1234_5678u64;
        let compact_trunc = compact_full & 0x7F_FFFF_FFFF;
        assert_eq!(desc_addr_with(compact_trunc, base, 0, true), compact_full);
    }

    #[test]
    fn signal_to_address_count_math() {
        assert_eq!(compute_signal_to_address_update(2, 10, 0, 0), Ok(11));
        assert_eq!(compute_signal_to_address_update(2, 10, 0, 3), Ok(8));
        assert_eq!(compute_signal_to_address_update(2, 10, 2, 2), Ok(9));
        assert_eq!(compute_signal_to_address_update(2, 10, 2, 3), Ok(10));
        assert_eq!(compute_signal_to_address_update(2, 10, 2, 0), Ok(11));
        assert_eq!(
            compute_signal_to_address_update(9, 10, 1, 0),
            Err(RC_INVALID_ENUM)
        );
    }

    #[test]
    fn parse_hipc_header_extracts_special_and_descriptors() {
        // type = 4, nx = 1, na = 1, nb = 0, nw = 0; ndata = 8, cflags = 0, special = 1
        let w0 = 4u32 | (1 << 16) | (1 << 20);
        let w1 = 8u32 | (1 << 31);
        // special: pid=1, ncopy=1, nmove=1
        let sh = 1u32 | (1 << 1) | (1 << 5);
        // x0: size = 0x20, addr = 0x12_3456_789A
        let x_d0 = (0x20u32 << 16) | (0x2 << 12) | (0x1 << 6);
        let x_d1 = 0x3456_789Au32;
        // a0: size = 0x100, addr = 0x05_1122_3344
        let a_sz = 0x100u32;
        let a_ad = 0x1122_3344u32;
        let a_d2 = 0x5u32 << 28;
        let words = [
            w0,
            w1,
            sh,
            0xAAAA_BBBB,
            0x0000_0001,
            0x44,
            0x55,
            x_d0,
            x_d1,
            a_sz,
            a_ad,
            a_d2,
        ];
        let parsed = parse_hipc_words(&words, |a| a);
        assert_eq!(parsed.msg_type, 4);
        assert_eq!(parsed.ndata, 8);
        assert_eq!(parsed.pid, 0x0000_0001_AAAA_BBBB);
        assert_eq!(parsed.in_copy, vec![0x44]);
        assert_eq!(parsed.in_move, vec![0x55]);
        assert_eq!(parsed.x.len(), 1);
        assert_eq!(parsed.x[0].addr, 0x12_3456_789A);
        assert_eq!(parsed.x[0].size, 0x20);
        assert_eq!(parsed.a.len(), 1);
        assert_eq!(parsed.a[0].addr, 0x05_1122_3344);
        assert_eq!(parsed.a[0].size, 0x100);
        assert_eq!(parsed.data_start, 12);
    }
}
