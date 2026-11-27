//! Guest address space (`memory.cpp`), write watches (`write_watch.cpp`), and memory guard
//! (`memguard_*.cpp`) in Rust.

use crate::ffi::host_log;
use std::collections::BTreeMap;
use std::ffi::{c_char, CStr, CString};
use std::ops::Bound;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

pub const K_MEM_FREE: u32 = 0x00;
pub const K_MEM_CODE: u32 = 0x03;
pub const K_MEM_CODE_DATA: u32 = 0x04;
pub const K_MEM_HEAP: u32 = 0x05;
pub const K_MEM_STATIC: u32 = 0x0C;
pub const K_MEM_STACK: u32 = 0x0B;
pub const K_MEM_THREAD_LOCAL: u32 = 0x0C;
pub const K_MEM_ALIAS: u32 = 0x0E;
pub const K_MEM_INACCESSIBLE: u32 = 0x10;

pub const MEMGUARD_LINUX: u32 = 0;
pub const MEMGUARD_ANDROID: u32 = 1;
pub const MEMGUARD_MAC: u32 = 2;
pub const MEMGUARD_IOS: u32 = 3;
pub const MEMGUARD_WIN: u32 = 4;

const GIB: f64 = (1u64 << 30) as f64;
const MIB: u64 = 1u64 << 20;
const K_WATCHES: usize = 16;
const K_READS: usize = 16;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AcnhVmInfo {
    pub addr: u64,
    pub size: u64,
    pub state: u32,
    pub perm: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AcnhMemoryBounds {
    pub address_space_base: u64,
    pub address_space_size: u64,
    pub module_base: u64,
    pub module_size: u64,
    pub heap_base: u64,
    pub heap_max: u64,
    pub alias_base: u64,
    pub alias_size: u64,
    pub stack_base: u64,
    pub stack_size: u64,
    pub kernel_base: u64,
    pub kernel_size: u64,
    pub thunk_base: u64,
    pub thunk_size: u64,
    pub compact_layout: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct AcnhMemoryHost {
    pub reserve: Option<extern "C" fn(addr: u64, size: u64) -> bool>,
    pub commit: Option<extern "C" fn(addr: u64, size: u64) -> bool>,
    pub decommit: Option<extern "C" fn(addr: u64, size: u64)>,
    pub log: Option<extern "C" fn(msg: *const c_char)>,
    pub fatal: Option<extern "C" fn(msg: *const c_char)>,
    pub mprotect_ro: Option<extern "C" fn(addr: u64, size: u64) -> bool>,
    pub mprotect_rw: Option<extern "C" fn(addr: u64, size: u64) -> bool>,
    pub page_size: Option<extern "C" fn() -> u64>,
    pub monotonic_ns: Option<extern "C" fn() -> u64>,
    pub log_flush: Option<extern "C" fn()>,
    pub memguard_footprint: Option<extern "C" fn(use_smaps: bool) -> u64>,
    pub memguard_total_ram_bytes: Option<extern "C" fn() -> u64>,
    pub memguard_ios_available_bytes: Option<extern "C" fn() -> u64>,
    pub memguard_terminate: Option<extern "C" fn(code: i32)>,
    pub memguard_stderr: Option<extern "C" fn(msg: *const c_char)>,
}

static HOST: Mutex<AcnhMemoryHost> = Mutex::new(AcnhMemoryHost {
    reserve: None,
    commit: None,
    decommit: None,
    log: None,
    fatal: None,
    mprotect_ro: None,
    mprotect_rw: None,
    page_size: None,
    monotonic_ns: None,
    log_flush: None,
    memguard_footprint: None,
    memguard_total_ram_bytes: None,
    memguard_ios_available_bytes: None,
    memguard_terminate: None,
    memguard_stderr: None,
});

struct VmState {
    bounds: AcnhMemoryBounds,
    vm: BTreeMap<u64, AcnhVmInfo>,
    kernel_next: u64,
}

impl VmState {
    const fn new() -> Self {
        Self {
            bounds: AcnhMemoryBounds {
                address_space_base: 0,
                address_space_size: 0,
                module_base: 0,
                module_size: 0,
                heap_base: 0,
                heap_max: 0,
                alias_base: 0,
                alias_size: 0,
                stack_base: 0,
                stack_size: 0,
                kernel_base: 0,
                kernel_size: 0,
                thunk_base: 0,
                thunk_size: 0,
                compact_layout: false,
            },
            vm: BTreeMap::new(),
            kernel_next: 0,
        }
    }
}

static VM_STATE: Mutex<VmState> = Mutex::new(VmState::new());

struct WatchSlot {
    addr: AtomicU64,
    begin: AtomicU64,
    end: AtomicU64,
    size: AtomicU64,
    written: AtomicBool,
    lo: AtomicU64,
    hi: AtomicU64,
}

impl WatchSlot {
    const fn new() -> Self {
        Self {
            addr: AtomicU64::new(0),
            begin: AtomicU64::new(0),
            end: AtomicU64::new(0),
            size: AtomicU64::new(0),
            written: AtomicBool::new(false),
            lo: AtomicU64::new(u64::MAX),
            hi: AtomicU64::new(0),
        }
    }
}

static WATCHES: [WatchSlot; K_WATCHES] = [const { WatchSlot::new() }; K_WATCHES];

struct MemguardRead {
    begin: AtomicU64,
    end: AtomicU64,
}

impl MemguardRead {
    const fn new() -> Self {
        Self {
            begin: AtomicU64::new(0),
            end: AtomicU64::new(0),
        }
    }
}

static MEMGUARD_READS: [MemguardRead; K_READS] = [const { MemguardRead::new() }; K_READS];
static MEMGUARD_READ_NEXT: AtomicU32 = AtomicU32::new(0);

fn host_copy() -> AcnhMemoryHost {
    *HOST.lock().unwrap()
}

fn host_fatal(msg: &str) -> ! {
    if let Some(f) = host_copy().fatal {
        if let Ok(c) = CString::new(msg) {
            f(c.as_ptr());
        }
    }
    panic!("fatal: {msg}");
}

fn host_reserve(addr: u64, size: u64) -> bool {
    host_copy().reserve.is_some_and(|f| f(addr, size))
}

fn reserve_or_fatal(addr: u64, size: u64, what: &str) {
    if !host_reserve(addr, size) {
        host_fatal(&format!(
            "cannot reserve {what} at {addr:x} (+{size:x}): the range is in use"
        ));
    }
}

fn page_size() -> u64 {
    static PAGE_SIZE: OnceLock<u64> = OnceLock::new();
    *PAGE_SIZE.get_or_init(|| host_copy().page_size.map_or(4096, |f| f().max(1)))
}

fn write_watch_enabled() -> bool {
    !matches!(std::env::var("NX_WRITE_WATCH"), Ok(v) if v.starts_with('0'))
}

fn vm_set_locked(vm: &mut BTreeMap<u64, AcnhVmInfo>, addr: u64, size: u64, state: u32, perm: u32) {
    if size == 0 {
        return;
    }
    let end = addr.saturating_add(size);

    let mut overlapping = Vec::new();
    if let Some((&prev_k, _)) = vm.range(..addr).next_back() {
        overlapping.push(prev_k);
    }
    for (&k, _) in vm.range(addr..end) {
        overlapping.push(k);
    }

    for k in overlapping {
        if let Some(b) = vm.get(&k).copied() {
            let b_end = b.addr.saturating_add(b.size);
            if b_end <= addr || b.addr >= end {
                continue;
            }
            vm.remove(&k);
            if b.addr < addr {
                vm.insert(
                    b.addr,
                    AcnhVmInfo {
                        addr: b.addr,
                        size: addr - b.addr,
                        state: b.state,
                        perm: b.perm,
                    },
                );
            }
            if b_end > end {
                vm.insert(
                    end,
                    AcnhVmInfo {
                        addr: end,
                        size: b_end - end,
                        state: b.state,
                        perm: b.perm,
                    },
                );
            }
        }
    }

    if state != K_MEM_FREE {
        vm.insert(
            addr,
            AcnhVmInfo {
                addr,
                size,
                state,
                perm,
            },
        );
    }
}

fn vm_query_locked(st: &VmState, addr: u64) -> AcnhVmInfo {
    let base = st.bounds.address_space_base;
    let end = base.saturating_add(st.bounds.address_space_size);
    if st.bounds.compact_layout {
        if addr < base {
            return AcnhVmInfo {
                addr: 0,
                size: base,
                state: K_MEM_INACCESSIBLE,
                perm: 0,
            };
        }
        if addr >= end {
            return AcnhVmInfo {
                addr: end,
                size: u64::MAX - end + 1,
                state: K_MEM_INACCESSIBLE,
                perm: 0,
            };
        }
    }

    let next = st
        .vm
        .range((Bound::Excluded(addr), Bound::Unbounded))
        .next()
        .map(|(&k, _)| k);
    let mut gap_end = next.unwrap_or(end);

    if let Some((_, &prev)) = st.vm.range(..=addr).next_back() {
        let prev_end = prev.addr.saturating_add(prev.size);
        if addr < prev_end {
            return prev;
        }
        return AcnhVmInfo {
            addr: prev_end,
            size: gap_end.saturating_sub(prev_end),
            state: K_MEM_FREE,
            perm: 0,
        };
    }

    let gap_start = if addr < base { 0 } else { base };
    if addr < base {
        gap_end = base;
    }
    AcnhVmInfo {
        addr: gap_start,
        size: gap_end.saturating_sub(gap_start),
        state: K_MEM_FREE,
        perm: 0,
    }
}

fn atomic_min_u64(atom: &AtomicU64, val: u64) {
    let mut cur = atom.load(Ordering::Relaxed);
    while val < cur {
        match atom.compare_exchange_weak(cur, val, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => break,
            Err(next) => cur = next,
        }
    }
}

fn atomic_max_u64(atom: &AtomicU64, val: u64) {
    let mut cur = atom.load(Ordering::Relaxed);
    while val > cur {
        match atom.compare_exchange_weak(cur, val, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => break,
            Err(next) => cur = next,
        }
    }
}

fn kb_field(path: &str, key: &str) -> u64 {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            return rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
        }
    }
    0
}

fn parse_positive_f64(s: Option<&str>) -> f64 {
    let Some(s) = s else {
        return 0.0;
    };
    match s.trim().parse::<f64>() {
        Ok(v) if v > 0.0 => v,
        _ => 0.0,
    }
}

/// Registers the C++ host callbacks for memory, write watches, and memguard.
///
/// # Safety
/// `host` must be null or point to a valid [`AcnhMemoryHost`] for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn acnh_memory_set_host(host: *const AcnhMemoryHost) {
    if host.is_null() {
        return;
    }
    let h = *host;
    crate::ffi::set_log(h.log);
    *HOST.lock().unwrap() = h;
}

/// Initializes the guest address space reservations and initial VM map.
///
/// # Safety
/// `bounds` must be null or point to a valid [`AcnhMemoryBounds`].
#[no_mangle]
pub unsafe extern "C" fn acnh_memory_init(bounds: *const AcnhMemoryBounds) {
    if bounds.is_null() {
        return;
    }
    let b = *bounds;
    {
        let mut st = VM_STATE.lock().unwrap();
        st.bounds = b;
        st.vm.clear();
        st.kernel_next = b.kernel_base;
    }

    if b.compact_layout {
        if b.module_base == 0 {
            host_fatal("cannot reserve a compact guest window below 2^39");
        }
    } else {
        reserve_or_fatal(b.module_base, b.module_size, "modules");
        reserve_or_fatal(b.heap_base, b.heap_max, "heap");
        reserve_or_fatal(b.alias_base, b.alias_size, "alias");
        reserve_or_fatal(b.stack_base, b.stack_size, "stack");
        reserve_or_fatal(b.kernel_base, b.kernel_size, "kernel");
    }

    if !host_reserve(b.thunk_base, b.thunk_size) {
        host_log(&format!(
            "memory: thunk range {:x} (+{:x}) already in use on this device: not reserved (only an address range for HLE calls, never mapped)",
            b.thunk_base, b.thunk_size
        ));
    }

    let end = b.address_space_base.saturating_add(b.address_space_size);
    let mut st = VM_STATE.lock().unwrap();
    vm_set_locked(
        &mut st.vm,
        b.address_space_base,
        b.alias_base.saturating_sub(b.address_space_base),
        K_MEM_INACCESSIBLE,
        0,
    );
    let alias_end = b.alias_base.saturating_add(b.alias_size);
    if alias_end < b.stack_base {
        vm_set_locked(
            &mut st.vm,
            alias_end,
            b.stack_base - alias_end,
            K_MEM_INACCESSIBLE,
            0,
        );
    }
    let stack_end = b.stack_base.saturating_add(b.stack_size);
    vm_set_locked(
        &mut st.vm,
        stack_end,
        end.saturating_sub(stack_end),
        K_MEM_INACCESSIBLE,
        0,
    );
}

#[no_mangle]
pub extern "C" fn acnh_memory_commit(addr: u64, size: u64) -> bool {
    host_copy().commit.is_some_and(|f| f(addr, size))
}

#[no_mangle]
pub extern "C" fn acnh_memory_decommit(addr: u64, size: u64) {
    if let Some(f) = host_copy().decommit {
        f(addr, size);
    }
}

#[no_mangle]
pub extern "C" fn acnh_memory_kernel_alloc(size: u64, align: u64) -> u64 {
    let mut st = VM_STATE.lock().unwrap();
    let align = if align == 0 { 0x1000 } else { align };
    let a = (st.kernel_next + align - 1) & !(align - 1);
    let size = (size + 0xFFF) & !0xFFFu64;
    if a.saturating_add(size) > st.bounds.kernel_base.saturating_add(st.bounds.kernel_size) {
        drop(st);
        host_fatal("kernel region exhausted");
    }
    st.kernel_next = a + size;
    if !acnh_memory_commit(a, size) {
        drop(st);
        host_fatal("kernel_alloc commit failed");
    }
    a
}

#[no_mangle]
pub extern "C" fn acnh_memory_vm_set(addr: u64, size: u64, state: u32, perm: u32) {
    let mut st = VM_STATE.lock().unwrap();
    vm_set_locked(&mut st.vm, addr, size, state, perm);
}

#[no_mangle]
pub extern "C" fn acnh_memory_vm_query(addr: u64) -> AcnhVmInfo {
    let st = VM_STATE.lock().unwrap();
    vm_query_locked(&st, addr)
}

#[no_mangle]
pub extern "C" fn acnh_write_watch_add(addr: u64, size: u64) -> bool {
    if !write_watch_enabled() || addr == 0 || size == 0 {
        return false;
    }
    let ps = page_size();
    let b = addr & !(ps - 1);
    let e = (addr.saturating_add(size).saturating_add(ps - 1)) & !(ps - 1);
    let Some(mprotect_ro) = host_copy().mprotect_ro else {
        return false;
    };
    for w in &WATCHES {
        if w.addr
            .compare_exchange(0, addr, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            continue;
        }
        w.begin.store(b, Ordering::Release);
        w.end.store(e, Ordering::Release);
        w.size.store(size, Ordering::Release);
        w.written.store(true, Ordering::Release);
        w.lo.store(u64::MAX, Ordering::Release);
        w.hi.store(0, Ordering::Release);
        if !mprotect_ro(b, e - b) {
            w.addr.store(0, Ordering::Release);
            return false;
        }
        return true;
    }
    false
}

#[no_mangle]
pub extern "C" fn acnh_write_watch_remove(addr: u64) {
    if addr == 0 {
        return;
    }
    let mprotect_rw = host_copy().mprotect_rw;
    for w in &WATCHES {
        if w.addr.load(Ordering::Acquire) == addr {
            let b = w.begin.load(Ordering::Acquire);
            let e = w.end.load(Ordering::Acquire);
            if let Some(f) = mprotect_rw {
                f(b, e.saturating_sub(b));
            }
            w.begin.store(0, Ordering::Release);
            w.end.store(0, Ordering::Release);
            w.addr.store(0, Ordering::Release);
        }
    }
}

/// Re-arms the write watch on `addr` and returns whether it was written since the last take.
///
/// # Safety
/// `out_lo` and `out_hi` must be null or point to valid writable `u64` storage.
#[no_mangle]
pub unsafe extern "C" fn acnh_write_watch_take(
    addr: u64,
    out_lo: *mut u64,
    out_hi: *mut u64,
) -> bool {
    if !out_lo.is_null() {
        *out_lo = 0;
    }
    if !out_hi.is_null() {
        *out_hi = u64::MAX;
    }
    if addr == 0 {
        return true;
    }
    let mprotect_ro = host_copy().mprotect_ro;
    for w in &WATCHES {
        if w.addr.load(Ordering::Acquire) == addr {
            let b = w.begin.load(Ordering::Acquire);
            let e = w.end.load(Ordering::Acquire);
            if let Some(f) = mprotect_ro {
                f(b, e.saturating_sub(b));
            }
            let was = w.written.swap(false, Ordering::AcqRel);
            let mut a = w.lo.swap(u64::MAX, Ordering::AcqRel);
            let mut b_hi = w.hi.swap(0, Ordering::AcqRel);
            if a < b_hi {
                a = a.saturating_sub(addr);
                b_hi = b_hi
                    .saturating_sub(addr)
                    .min(w.size.load(Ordering::Acquire));
                if !out_lo.is_null() {
                    *out_lo = a;
                }
                if !out_hi.is_null() {
                    *out_hi = a.max(b_hi);
                }
            } else if !was {
                if !out_lo.is_null() {
                    *out_lo = 0;
                }
                if !out_hi.is_null() {
                    *out_hi = 0;
                }
            }
            return was || a < b_hi;
        }
    }
    true
}

#[no_mangle]
pub extern "C" fn acnh_write_watch_fault(at: u64) -> bool {
    let ps = page_size();
    let page = at & !(ps - 1);
    let mut hit = false;
    for w in &WATCHES {
        if w.addr.load(Ordering::Acquire) != 0
            && at >= w.begin.load(Ordering::Acquire)
            && at < w.end.load(Ordering::Acquire)
        {
            atomic_min_u64(&w.lo, page);
            atomic_max_u64(&w.hi, page.saturating_add(ps));
            w.written.store(true, Ordering::Release);
            hit = true;
        }
    }
    if hit {
        if let Some(mprotect_rw) = host_copy().mprotect_rw {
            mprotect_rw(page, ps);
        }
    }
    hit
}

#[no_mangle]
pub extern "C" fn acnh_memguard_overlap_ns(from: u64, to: u64) -> u64 {
    let mut total = 0u64;
    for r in &MEMGUARD_READS {
        let b = r.begin.load(Ordering::Relaxed);
        let e = r.end.load(Ordering::Relaxed);
        if e == 0 || e <= from || b >= to {
            continue;
        }
        total += e.min(to) - b.max(from);
    }
    total
}

/// Parses `NX_MEM_LIMIT_GB` or falls back to half the machine's RAM (at least 8 GB).
///
/// # Safety
/// `env` must be null or a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn acnh_memguard_limit_gb(env: *const c_char) -> f64 {
    let parsed = if env.is_null() {
        0.0
    } else {
        parse_positive_f64(CStr::from_ptr(env).to_str().ok())
    };
    if parsed > 0.0 {
        return parsed;
    }
    let total = host_copy().memguard_total_ram_bytes.map_or(0, |f| f());
    let half = (total as f64) / GIB / 2.0;
    if half > 8.0 {
        half
    } else {
        8.0
    }
}

#[no_mangle]
pub extern "C" fn acnh_memguard_footprint() -> u64 {
    host_copy().memguard_footprint.map_or(0, |f| f(false))
}

#[no_mangle]
pub extern "C" fn acnh_memguard_start(platform: u32) {
    let env_limit = std::env::var("NX_MEM_LIMIT_GB").ok();
    let parsed_limit = parse_positive_f64(env_limit.as_deref());

    let (limit, gb, mode) = match platform {
        MEMGUARD_ANDROID => {
            let ram = host_copy().memguard_total_ram_bytes.map_or(0, |f| f()) as f64 / GIB;
            let gb = if parsed_limit > 0.0 {
                parsed_limit
            } else if ram > 0.0 {
                ram * 0.6
            } else {
                6.0
            };
            let mode = std::env::var("NX_MEMGUARD").unwrap_or_else(|_| "statm".to_string());
            if mode == "off" {
                host_log("memory: guard off (NX_MEMGUARD=off)");
                return;
            }
            host_log(&format!(
                "memory: guard at {gb:.1} GB (the phone has {ram:.1} GB), source {mode}"
            ));
            ((gb * GIB) as u64, gb, mode)
        }
        MEMGUARD_IOS => {
            let start = host_copy().memguard_footprint.map_or(0, |f| f(false));
            let available = host_copy().memguard_ios_available_bytes.map_or(0, |f| f());
            let margin = 256 * MIB;
            let mut limit = if available == 0 {
                8u64 << 30
            } else {
                start + available.saturating_sub(margin)
            };
            let gb = if parsed_limit > 0.0 {
                limit = (parsed_limit * GIB) as u64;
                parsed_limit
            } else {
                limit as f64 / GIB
            };
            host_log(&format!(
                "memory: {:.2} GB at start, iOS limit {gb:.1} GB ({} MB available, {} MB margin)",
                start as f64 / GIB,
                available >> 20,
                margin >> 20
            ));
            (limit, gb, String::new())
        }
        MEMGUARD_WIN => {
            let gb = if parsed_limit > 0.0 {
                parsed_limit
            } else {
                let total = host_copy().memguard_total_ram_bytes.map_or(0, |f| f());
                let half = (total as f64) / GIB / 2.0;
                if half > 8.0 {
                    half
                } else {
                    8.0
                }
            };
            host_log(&format!(
                "memory: guard on, limit {gb:.1} GB (NX_MEM_LIMIT_GB)"
            ));
            ((gb * GIB) as u64, gb, String::new())
        }
        _ => {
            let gb = if parsed_limit > 0.0 {
                parsed_limit
            } else {
                8.0
            };
            if platform == MEMGUARD_LINUX {
                host_log(&format!(
                    "memory: guard on, limit {gb:.1} GB (NX_MEM_LIMIT_GB)"
                ));
            }
            ((gb * GIB) as u64, gb, String::new())
        }
    };

    let _ = std::thread::Builder::new()
        .name("memguard".to_string())
        .spawn(move || {
            let mut ticks = 0u64;
            let mut peak = 0u64;
            let mut read_ns = 0u64;
            let mut read_max = 0u64;
            let mut smaps = mode == "smaps" || mode == "ab";

            loop {
                std::thread::sleep(Duration::from_millis(250));
                if platform == MEMGUARD_ANDROID {
                    if mode == "ab" && ticks > 0 && ticks.is_multiple_of(160) {
                        smaps = !smaps;
                        host_log(&format!(
                            "memory: guard source {} -> {}",
                            if smaps { "statm" } else { "smaps" },
                            if smaps { "smaps" } else { "statm" }
                        ));
                    }
                    let idx = MEMGUARD_READ_NEXT.fetch_add(1, Ordering::Relaxed) as usize % K_READS;
                    let r = &MEMGUARD_READS[idx];
                    let t0 = host_copy().monotonic_ns.map_or(0, |f| f());
                    r.end.store(0, Ordering::Relaxed);
                    r.begin.store(t0, Ordering::Relaxed);
                    let f = host_copy().memguard_footprint.map_or(0, |fp| fp(smaps));
                    let t1 = host_copy().monotonic_ns.map_or(0, |fp| fp());
                    r.end.store(t1, Ordering::Relaxed);
                    let dt = t1.saturating_sub(t0);
                    read_ns += dt;
                    read_max = read_max.max(dt);
                    if f > peak {
                        peak = f;
                    }
                    ticks += 1;
                    if ticks.is_multiple_of(120) {
                        let kind = if smaps {
                            "Pss (smaps)"
                        } else {
                            "resident (statm)"
                        };
                        host_log(&format!(
                            "memory: {:.2} GB {kind} (peak {:.2} GB, limit {gb:.1} GB), read avg {:.2} ms, max {:.2} ms",
                            f as f64 / GIB,
                            peak as f64 / GIB,
                            read_ns as f64 / 120e6,
                            read_max as f64 / 1e6
                        ));
                        read_ns = 0;
                        read_max = 0;
                        host_log(&format!(
                            "memory: anon {} MB, file {} MB, swapped {} MB (VmSwap) | system: MemAvailable {} MB, SwapFree {} MB",
                            kb_field("/proc/self/status", "RssAnon:") >> 10,
                            kb_field("/proc/self/status", "RssFile:") >> 10,
                            kb_field("/proc/self/status", "VmSwap:") >> 10,
                            kb_field("/proc/meminfo", "MemAvailable:") >> 10,
                            kb_field("/proc/meminfo", "SwapFree:") >> 10
                        ));
                    }
                    if f > limit {
                        host_log(&format!(
                            "memory: {:.2} GB is over the {gb:.1} GB limit (NX_MEM_LIMIT_GB): stopping",
                            f as f64 / GIB
                        ));
                        if let Some(flush) = host_copy().log_flush {
                            flush();
                        }
                        if let Some(term) = host_copy().memguard_terminate {
                            term(3);
                        }
                        std::process::exit(3);
                    }
                } else {
                    let f = host_copy().memguard_footprint.map_or(0, |fp| fp(false));
                    if f > peak {
                        peak = f;
                    }
                    ticks += 1;
                    if ticks.is_multiple_of(120) {
                        host_log(&format!(
                            "memory: {:.2} GB (peak {:.2} GB, limit {gb:.1} GB)",
                            f as f64 / GIB,
                            peak as f64 / GIB
                        ));
                    }
                    if f > limit {
                        let msg = format!(
                            "memory: {:.2} GB is over the {gb:.1} GB limit (NX_MEM_LIMIT_GB): stopping",
                            f as f64 / GIB
                        );
                        host_log(&msg);
                        if let Some(stderr_fn) = host_copy().memguard_stderr {
                            if let Ok(c) = CString::new(msg.as_str()) {
                                stderr_fn(c.as_ptr());
                            }
                        }
                        if let Some(term) = host_copy().memguard_terminate {
                            term(3);
                        }
                        std::process::exit(3);
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    static TEST_LOCK: Mutex<()> = Mutex::new(());
    static RESERVE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static MPROTECT_RO_CALLS: AtomicUsize = AtomicUsize::new(0);
    static MPROTECT_RW_CALLS: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn test_reserve(_addr: u64, _size: u64) -> bool {
        RESERVE_CALLS.fetch_add(1, Ordering::Relaxed);
        true
    }
    extern "C" fn test_commit(_addr: u64, _size: u64) -> bool {
        true
    }
    extern "C" fn test_decommit(_addr: u64, _size: u64) {}
    extern "C" fn test_mprotect_ro(_addr: u64, _size: u64) -> bool {
        MPROTECT_RO_CALLS.fetch_add(1, Ordering::Relaxed);
        true
    }
    extern "C" fn test_mprotect_rw(_addr: u64, _size: u64) -> bool {
        MPROTECT_RW_CALLS.fetch_add(1, Ordering::Relaxed);
        true
    }
    extern "C" fn test_page_size() -> u64 {
        4096
    }
    extern "C" fn test_total_ram() -> u64 {
        32 * (1u64 << 30)
    }

    fn setup_test_host() {
        let host = AcnhMemoryHost {
            reserve: Some(test_reserve),
            commit: Some(test_commit),
            decommit: Some(test_decommit),
            mprotect_ro: Some(test_mprotect_ro),
            mprotect_rw: Some(test_mprotect_rw),
            page_size: Some(test_page_size),
            memguard_total_ram_bytes: Some(test_total_ram),
            ..Default::default()
        };
        // SAFETY: test-only: the host table lives on this test's stack and outlives every call that reads it.
        unsafe { acnh_memory_set_host(&host) };
    }

    fn sample_bounds(compact: bool) -> AcnhMemoryBounds {
        if compact {
            let base = 0x10_0000_0000u64;
            let module_size = 0x2000_0000u64;
            let heap_max = 0xD000_0000u64;
            let alias_size = 0x1000_0000u64;
            let stack_size = 0x1000_0000u64;
            let guard_size = 0x4000u64;
            let kernel_size = 0x1000_0000u64;
            let total = module_size + heap_max + alias_size + stack_size + guard_size + kernel_size;
            AcnhMemoryBounds {
                address_space_base: base,
                address_space_size: total,
                module_base: base,
                module_size,
                heap_base: base + module_size,
                heap_max,
                alias_base: base + module_size + heap_max,
                alias_size,
                stack_base: base + module_size + heap_max + alias_size,
                stack_size,
                kernel_base: base + module_size + heap_max + alias_size + stack_size + guard_size,
                kernel_size,
                thunk_base: base + total,
                thunk_size: 0x10_0000,
                compact_layout: true,
            }
        } else {
            AcnhMemoryBounds {
                address_space_base: 0x0008_0000_0000,
                address_space_size: 0x007F_F800_0000,
                module_base: 0x0008_0000_0000,
                module_size: 0x2000_0000,
                heap_base: 0x0050_0000_0000,
                heap_max: 0x0001_8000_0000,
                alias_base: 0x0058_0000_0000,
                alias_size: 0x0010_0000_0000,
                stack_base: 0x0068_0000_0000,
                stack_size: 0x0001_0000_0000,
                kernel_base: 0x006A_0000_0000,
                kernel_size: 0x0001_0000_0000,
                thunk_base: 0x006F_0000_0000,
                thunk_size: 0x10_0000,
                compact_layout: false,
            }
        }
    }

    #[test]
    fn desktop_layout_vm_query_and_kernel_alloc() {
        let _g = TEST_LOCK.lock().unwrap();
        setup_test_host();
        RESERVE_CALLS.store(0, Ordering::Relaxed);
        let b = sample_bounds(false);
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        unsafe { acnh_memory_init(&b) };
        assert_eq!(RESERVE_CALLS.load(Ordering::Relaxed), 6);

        assert_eq!(
            acnh_memory_vm_query(b.alias_base - 1).state,
            K_MEM_INACCESSIBLE
        );
        assert_eq!(
            acnh_memory_vm_query(b.module_base).state,
            K_MEM_INACCESSIBLE
        );
        assert_eq!(acnh_memory_vm_query(b.alias_base).state, K_MEM_FREE);
        assert_eq!(acnh_memory_vm_query(b.stack_base).state, K_MEM_FREE);
        assert_eq!(
            acnh_memory_vm_query(b.stack_base + b.stack_size).state,
            K_MEM_INACCESSIBLE
        );

        let p1 = acnh_memory_kernel_alloc(0x123, 0x1000);
        let p2 = acnh_memory_kernel_alloc(0x1000, 0x4000);
        assert_eq!(p1, b.kernel_base);
        assert_eq!(p2, (b.kernel_base + 0x1000 + 0x3FFF) & !0x3FFF);
    }

    #[test]
    fn compact_layout_and_interval_splitting() {
        let _g = TEST_LOCK.lock().unwrap();
        setup_test_host();
        let b = sample_bounds(true);
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        unsafe { acnh_memory_init(&b) };
        let end = b.address_space_base + b.address_space_size;

        assert_eq!(
            acnh_memory_vm_query(b.address_space_base - 1).state,
            K_MEM_INACCESSIBLE
        );
        assert_eq!(acnh_memory_vm_query(end).state, K_MEM_INACCESSIBLE);
        let high = acnh_memory_vm_query(u64::MAX);
        assert_eq!(high.state, K_MEM_INACCESSIBLE);
        assert_eq!(high.addr, end);

        let base = b.alias_base;
        acnh_memory_vm_set(base, 0x10000, K_MEM_HEAP, 3);
        acnh_memory_vm_set(base + 0x4000, 0x4000, K_MEM_ALIAS, 1);
        let left = acnh_memory_vm_query(base + 0x1000);
        assert_eq!(
            (left.addr, left.size, left.state, left.perm),
            (base, 0x4000, K_MEM_HEAP, 3)
        );
        let mid = acnh_memory_vm_query(base + 0x5000);
        assert_eq!(
            (mid.addr, mid.size, mid.state, mid.perm),
            (base + 0x4000, 0x4000, K_MEM_ALIAS, 1)
        );
        let right = acnh_memory_vm_query(base + 0x8000);
        assert_eq!(
            (right.addr, right.size, right.state, right.perm),
            (base + 0x8000, 0x8000, K_MEM_HEAP, 3)
        );

        acnh_memory_vm_set(base + 0x4000, 0x4000, K_MEM_FREE, 0);
        let gap = acnh_memory_vm_query(base + 0x5000);
        assert_eq!(
            (gap.addr, gap.size, gap.state),
            (base + 0x4000, 0x4000, K_MEM_FREE)
        );
    }

    #[test]
    fn write_watch_lifecycle() {
        let _g = TEST_LOCK.lock().unwrap();
        setup_test_host();
        MPROTECT_RO_CALLS.store(0, Ordering::Relaxed);
        MPROTECT_RW_CALLS.store(0, Ordering::Relaxed);

        let addr = 0x1000_0200u64;
        let size = 0x3000u64;
        assert!(acnh_write_watch_add(addr, size));

        let mut lo = 0u64;
        let mut hi = 0u64;
        // First take returns written=true (unknown until first take).
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        assert!(unsafe { acnh_write_watch_take(addr, &mut lo, &mut hi) });
        assert_eq!((lo, hi), (0, u64::MAX));

        // Second take with no fault returns false and empty [0, 0).
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        assert!(!unsafe { acnh_write_watch_take(addr, &mut lo, &mut hi) });
        assert_eq!((lo, hi), (0, 0));

        // Simulate fault in second page.
        assert!(acnh_write_watch_fault(addr + 0x1500));
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        assert!(unsafe { acnh_write_watch_take(addr, &mut lo, &mut hi) });
        assert_eq!(lo, 0x1000 - 0x200);
        assert_eq!(hi, 0x2000 - 0x200);

        acnh_write_watch_remove(addr);
        assert!(MPROTECT_RO_CALLS.load(Ordering::Relaxed) >= 3);
        assert!(MPROTECT_RW_CALLS.load(Ordering::Relaxed) >= 2);
    }

    #[test]
    fn memguard_limit_and_overlap() {
        let _g = TEST_LOCK.lock().unwrap();
        setup_test_host();

        // 32 GB RAM -> half is 16 GB (> 8 GB).
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        assert_eq!(unsafe { acnh_memguard_limit_gb(std::ptr::null()) }, 16.0);
        let c5 = CString::new("5.5").unwrap();
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        assert_eq!(unsafe { acnh_memguard_limit_gb(c5.as_ptr()) }, 5.5);
        let czero = CString::new("0").unwrap();
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        assert_eq!(unsafe { acnh_memguard_limit_gb(czero.as_ptr()) }, 16.0);

        MEMGUARD_READS[0].begin.store(100, Ordering::Relaxed);
        MEMGUARD_READS[0].end.store(200, Ordering::Relaxed);
        assert_eq!(acnh_memguard_overlap_ns(50, 150), 50);
        assert_eq!(acnh_memguard_overlap_ns(120, 180), 60);
        assert_eq!(acnh_memguard_overlap_ns(200, 300), 0);
        MEMGUARD_READS[0].begin.store(0, Ordering::Relaxed);
        MEMGUARD_READS[0].end.store(0, Ordering::Relaxed);
    }
}
