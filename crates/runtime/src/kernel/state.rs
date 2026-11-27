//! Split from the old `kernel.rs` (issue #356).

use super::*;
#[allow(unused_imports)]
use crate::sync::{CondvarExt, LockExt};

// ---- Thread & Kernel State ---------------------------------------------------------------------

pub(crate) struct ThreadData {
    pub(crate) handle: u32,
    pub(crate) id: u64,
    pub(crate) ctx: AcnhCpu,
    pub(crate) tls: u64,
    pub(crate) entry: u64,
    pub(crate) host_tid: i64,
    pub(crate) last_yield: u64,
    pub(crate) yields: u32,
    pub(crate) yb_run: u64,
    pub(crate) yb_regs: [u64; 10],
    pub(crate) last_tick_call: u64,
    pub(crate) tick_spins: u32,
    pub(crate) prio: i32,
    pub(crate) core: i32,
    pub(crate) started: bool,
    pub(crate) exited: bool,
    pub(crate) cancel: bool,
    pub(crate) wake: bool,
    pub(crate) wait_result: u32,
    pub(crate) wait_mutex: u64,
    pub(crate) wait_key: u64,
    pub(crate) wait_addr: u64,
    pub(crate) wait_tag: u32,
}

pub(crate) struct KThread {
    pub(crate) data: Mutex<ThreadData>,
    pub(crate) cv: Condvar,
}

impl KThread {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
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

    pub(crate) fn ptr_id(&self) -> usize {
        self as *const KThread as usize
    }
}

pub(crate) struct KernelState {
    pub(crate) next_handle: u32,
    pub(crate) heap_size: u64,
    pub(crate) threads: HashMap<u32, Arc<KThread>>,
    pub(crate) sync_waiters: Vec<Arc<KThread>>,
    pub(crate) mutex_waiters: Vec<Arc<KThread>>,
    pub(crate) cv_waiters: Vec<Arc<KThread>>,
    pub(crate) addr_waiters: Vec<Arc<KThread>>,
    pub(crate) invalid_wait_handles: HashSet<u32>,
}

impl KernelState {
    pub(crate) fn new() -> Self {
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

pub(crate) static KSTATE: OnceLock<Mutex<KernelState>> = OnceLock::new();
pub(crate) static ALL_THREADS: OnceLock<Mutex<Vec<Arc<KThread>>>> = OnceLock::new();
pub(crate) static NEXT_THREAD_ID: AtomicU64 = AtomicU64::new(0x50);

pub(crate) static TICK_CALLS: AtomicU64 = AtomicU64::new(0);
pub(crate) static YIELD_CALLS: AtomicU64 = AtomicU64::new(0);
pub(crate) static YIELD_NAPS: AtomicU64 = AtomicU64::new(0);
pub(crate) static SLEEP_CALLS: AtomicU64 = AtomicU64::new(0);

pub(crate) fn kstate() -> &'static Mutex<KernelState> {
    KSTATE.get_or_init(|| Mutex::new(KernelState::new()))
}

pub(crate) fn all_threads() -> &'static Mutex<Vec<Arc<KThread>>> {
    ALL_THREADS.get_or_init(|| Mutex::new(Vec::new()))
}

std::thread_local! {
    pub(crate) static SELF_THREAD: std::cell::RefCell<Option<Arc<KThread>>> = const { std::cell::RefCell::new(None) };
    pub(crate) static DEFERRED_WAKES: std::cell::RefCell<Vec<Arc<KThread>>> = const { std::cell::RefCell::new(Vec::new()) };
    pub(crate) static SVC_ROLE: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
}

pub(crate) fn current_thread() -> Option<Arc<KThread>> {
    SELF_THREAD.with(|cell| cell.borrow().clone())
}

pub(crate) fn set_current_thread(t: Option<Arc<KThread>>) {
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

pub(crate) fn spin_naps_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        if let Ok(v) = std::env::var("NX_SPIN_NAPS") {
            return v.starts_with('1');
        }
        HOST.get().is_some_and(|h| (h.is_hades)())
    })
}

pub(crate) fn yb_config() -> (bool, u64, u64) {
    static CFG: OnceLock<(bool, u64, u64)> = OnceLock::new();
    *CFG.get_or_init(|| parse_yield_backoff_env(std::env::var("NX_YIELD_BACKOFF").ok().as_deref()))
}

pub(crate) fn deferred_wakes_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("NX_DEFER_WAKES").ok().as_deref() != Some("0"))
}

pub(crate) fn trace_addr() -> u64 {
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

pub(crate) fn desc_addr(v: u64) -> u64 {
    let b = region_bounds();
    let base = if b.compact_layout {
        (host().get_guest_base)()
    } else {
        0
    };
    desc_addr_with(v, base, b.guest_shift, b.compact_layout)
}

// ---- Guest atomic memory helpers ---------------------------------------------------------------

pub(crate) unsafe fn load32(addr: u64) -> u32 {
    // SAFETY: callers pass a guest address that is mapped for the life of the process; AtomicU32::from_ptr needs 4-byte alignment, which futex words and handle slots have, and unaligned reads and writes need none.
    unsafe { AtomicU32::from_ptr(addr as *mut u32).load(Ordering::SeqCst) }
}

pub(crate) unsafe fn store32(addr: u64, val: u32) {
    // SAFETY: callers pass a guest address that is mapped for the life of the process; AtomicU32::from_ptr needs 4-byte alignment, which futex words and handle slots have, and unaligned reads and writes need none.
    unsafe { AtomicU32::from_ptr(addr as *mut u32).store(val, Ordering::SeqCst) }
}

pub(crate) unsafe fn cas32(addr: u64, expected: u32, new_val: u32) -> bool {
    // SAFETY: callers pass a guest address that is mapped for the life of the process and 4-byte aligned, as futex words are.
    unsafe {
        AtomicU32::from_ptr(addr as *mut u32)
            .compare_exchange(expected, new_val, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

pub(crate) unsafe fn ld_u8(addr: u64) -> u8 {
    // SAFETY: callers pass a guest address that is mapped for the life of the process; AtomicU32::from_ptr needs 4-byte alignment, which futex words and handle slots have, and unaligned reads and writes need none.
    unsafe { *(addr as *const u8) }
}

pub(crate) unsafe fn ld_u16(addr: u64) -> u16 {
    // SAFETY: callers pass a guest address that is mapped for the life of the process; AtomicU32::from_ptr needs 4-byte alignment, which futex words and handle slots have, and unaligned reads and writes need none.
    unsafe { std::ptr::read_unaligned(addr as *const u16) }
}

pub(crate) unsafe fn ld_u32(addr: u64) -> u32 {
    // SAFETY: callers pass a guest address that is mapped for the life of the process; AtomicU32::from_ptr needs 4-byte alignment, which futex words and handle slots have, and unaligned reads and writes need none.
    unsafe { std::ptr::read_unaligned(addr as *const u32) }
}

pub(crate) unsafe fn ld_u64(addr: u64) -> u64 {
    // SAFETY: callers pass a guest address that is mapped for the life of the process; AtomicU32::from_ptr needs 4-byte alignment, which futex words and handle slots have, and unaligned reads and writes need none.
    unsafe { std::ptr::read_unaligned(addr as *const u64) }
}

pub(crate) unsafe fn st_u32(addr: u64, v: u32) {
    // SAFETY: callers pass a guest address that is mapped for the life of the process; AtomicU32::from_ptr needs 4-byte alignment, which futex words and handle slots have, and unaligned reads and writes need none.
    unsafe { std::ptr::write_unaligned(addr as *mut u32, v) }
}

pub(crate) unsafe fn st_u64(addr: u64, v: u64) {
    // SAFETY: callers pass a guest address that is mapped for the life of the process; AtomicU32::from_ptr needs 4-byte alignment, which futex words and handle slots have, and unaligned reads and writes need none.
    unsafe { std::ptr::write_unaligned(addr as *mut u64, v) }
}

// ---- Deferred wakes & Wait primitives ----------------------------------------------------------

pub(crate) struct DeferredWakeScope;

impl DeferredWakeScope {
    pub(crate) fn flush() {
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

pub(crate) fn wake_thread(t: &Arc<KThread>) {
    {
        let mut d = t.data.lock_or_recover();
        d.wake = true;
        d.wait_result = 0;
    }
    if deferred_wakes_on() {
        DEFERRED_WAKES.with(|cell| cell.borrow_mut().push(Arc::clone(t)));
    } else {
        t.cv.notify_one();
    }
}

pub(crate) fn remove_thread(vec: &mut Vec<Arc<KThread>>, target: &Arc<KThread>) {
    let id = target.ptr_id();
    vec.retain(|t| t.ptr_id() != id);
}

pub(crate) fn block_thread(t: &Arc<KThread>, timeout_ns: i64) -> bool {
    DeferredWakeScope::flush();
    let depth = KERNEL_LOCK.unlock_all();
    let ok = if timeout_ns < 0 {
        let mut d = t.data.lock_or_recover();
        while !d.wake {
            d = t.cv.wait_or_recover(d);
        }
        true
    } else {
        let dur = Duration::from_nanos(timeout_ns as u64);
        let deadline = Instant::now() + dur;
        let mut d = t.data.lock_or_recover();
        while !d.wake {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (guard, res) = t.cv.wait_timeout_or_recover(d, deadline - now);
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

pub(crate) fn unlock_mutex_locked(ks: &mut KernelState, addr: u64) {
    let mut next_idx: Option<usize> = None;
    let mut more = 0usize;
    for (idx, t) in ks.mutex_waiters.iter().enumerate() {
        if t.data.lock_or_recover().wait_mutex == addr {
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
    let tag = next.data.lock_or_recover().wait_tag;
    let val = tag | if more > 0 { HANDLE_WAIT_MASK } else { 0 };
    // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
    unsafe { store32(addr, val) };
    wake_thread(&next);
}
