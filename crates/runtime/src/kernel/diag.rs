//! Split from the old `kernel.rs` (issue #356).

use super::*;
#[allow(unused_imports)]
use crate::sync::LockExt;

// ---- Kernel watch / diagnostics ----------------------------------------------------------------

pub(crate) fn backtrace_cpu(c: &AcnhCpu) {
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

pub(crate) fn report_threads() {
    let all = all_threads().lock_or_recover().clone();
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
        let ks = kstate().lock_or_recover();
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
            let d = t.data.lock_or_recover();
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
                let all = all_threads().lock_or_recover().clone();
                let mut tids = Vec::new();
                let mut lrs = Vec::new();
                let mut keep = Vec::new();
                for t in all {
                    let (host_tid, exited, id, entry, lr_ptr) = {
                        let d = t.data.lock_or_recover();
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

pub(crate) fn dump_threads() {
    let bounds = region_bounds();
    let all = all_threads().lock_or_recover().clone();
    for t in all {
        let (started, exited, id, ctx) = {
            let d = t.data.lock_or_recover();
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
        let all = all_threads().lock_or_recover().clone();
        for t in all {
            let d = t.data.lock_or_recover();
            if d.started && !d.exited {
                *hits.entry((d.id, d.ctx.x[30])).or_insert(0) += 1;
            }
        }
    }
}

// ---- Profiling ---------------------------------------------------------------------------------

pub(crate) type SvcProfMap = BTreeMap<(i32, u32, u64), (u64, u64)>;
pub(crate) static SVC_PROF_LOCK: Mutex<Option<SvcProfMap>> = Mutex::new(None);
pub(crate) static FRAME_SVC_NS: AtomicU64 = AtomicU64::new(0);
pub(crate) static FRAME_SVC_TOP_NS: AtomicU64 = AtomicU64::new(0);
pub(crate) static FRAME_SVC_TOP_PC: AtomicU64 = AtomicU64::new(0);
pub(crate) static FRAME_SVC_TOP_IMM: AtomicU32 = AtomicU32::new(0);

pub(crate) fn svc_profile_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("NX_SVC_PROFILE").is_some())
}

pub(crate) fn frame_profile_on() -> bool {
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
    kstate().lock_or_recover().heap_size
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
                let mut d = t.data.lock_or_recover();
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
            t.data.lock_or_recover().last_tick_call = (host().monotonic_ns)();
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
        let mut d = t.data.lock_or_recover();
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
        let mut d = t.data.lock_or_recover();
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
        let mut map_guard = SVC_PROF_LOCK.lock_or_recover();
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
    let mut map_guard = SVC_PROF_LOCK.lock_or_recover();
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
