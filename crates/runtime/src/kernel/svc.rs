//! Split from the old `kernel.rs` (issue #356).

use super::*;
#[allow(unused_imports)]
use crate::sync::{CondvarExt, LockExt};

// ---- SVC implementations -----------------------------------------------------------------------

pub(crate) fn svc_arbitrate_lock(owner: u32, addr: u64, tag: u32) -> u32 {
    let _kl = KERNEL_LOCK.lock();
    let Some(self_t) = current_thread() else {
        return RC_INVALID_STATE;
    };
    // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
    if unsafe { load32(addr) } != (owner | HANDLE_WAIT_MASK) {
        return RC_OK;
    }
    {
        let mut d = self_t.data.lock_or_recover();
        d.wake = false;
        d.wait_mutex = addr;
        d.wait_tag = tag;
    }
    kstate()
        .lock_or_recover()
        .mutex_waiters
        .push(Arc::clone(&self_t));
    block_thread(&self_t, -1);
    let res = self_t.data.lock_or_recover().wait_result;
    res
}

pub(crate) fn svc_arbitrate_unlock(addr: u64) -> u32 {
    let _wakes = DeferredWakeScope;
    let _kl = KERNEL_LOCK.lock();
    let mut ks = kstate().lock_or_recover();
    unlock_mutex_locked(&mut ks, addr);
    RC_OK
}

pub(crate) fn svc_wait_process_wide_key(mutex: u64, key: u64, tag: u32, timeout: i64) -> u32 {
    let _wakes = DeferredWakeScope;
    let _kl = KERNEL_LOCK.lock();
    let Some(self_t) = current_thread() else {
        return RC_INVALID_STATE;
    };
    {
        let mut d = self_t.data.lock_or_recover();
        d.wake = false;
        d.wait_mutex = mutex;
        d.wait_key = key;
        d.wait_tag = tag;
        d.wait_result = 0;
    }
    {
        let mut ks = kstate().lock_or_recover();
        ks.cv_waiters.push(Arc::clone(&self_t));
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        unsafe { store32(key, 1) };
        unlock_mutex_locked(&mut ks, mutex);
    }
    if block_thread(&self_t, timeout) {
        return self_t.data.lock_or_recover().wait_result;
    }
    let mut ks = kstate().lock_or_recover();
    remove_thread(&mut ks.cv_waiters, &self_t);
    remove_thread(&mut ks.mutex_waiters, &self_t);
    let others = ks
        .cv_waiters
        .iter()
        .any(|t| t.data.lock_or_recover().wait_key == key);
    if !others {
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        unsafe { store32(key, 0) };
    }
    RC_TIMEOUT
}

pub(crate) fn svc_signal_process_wide_key(key: u64, count: i32) -> u32 {
    let _wakes = DeferredWakeScope;
    let _kl = KERNEL_LOCK.lock();
    let mut ks = kstate().lock_or_recover();
    let mut n = 0i32;
    let mut i = 0usize;
    while i < ks.cv_waiters.len() && (count <= 0 || n < count) {
        let match_key = ks.cv_waiters[i].data.lock_or_recover().wait_key == key;
        if !match_key {
            i += 1;
            continue;
        }
        let t = ks.cv_waiters.remove(i);
        n += 1;
        let (wait_mutex, wait_tag) = {
            let d = t.data.lock_or_recover();
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
        .any(|t| t.data.lock_or_recover().wait_key == key);
    if !others {
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        unsafe { store32(key, 0) };
    }
    RC_OK
}

pub(crate) fn svc_wait_for_address(addr: u64, wait_type: u32, value: i32, timeout: i64) -> u32 {
    let self_t = current_thread();
    if addr == trace_addr() && addr != 0 {
        let h = self_t
            .as_ref()
            .map_or(0, |t| t.data.lock_or_recover().handle);
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
        let mut d = self_t.data.lock_or_recover();
        d.wake = false;
        d.wait_addr = addr;
        d.wait_result = 0;
    }
    kstate()
        .lock_or_recover()
        .addr_waiters
        .push(Arc::clone(&self_t));
    if !block_thread(&self_t, timeout) {
        let mut ks = kstate().lock_or_recover();
        remove_thread(&mut ks.addr_waiters, &self_t);
        return RC_TIMEOUT;
    }
    let res = self_t.data.lock_or_recover().wait_result;
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

pub(crate) fn svc_signal_to_address(addr: u64, sig_type: u32, value: i32, count: i32) -> u32 {
    if addr == trace_addr() && addr != 0 {
        let h = current_thread().map_or(0, |t| t.data.lock_or_recover().handle);
        host_log(&format!(
            "arb: thread {h:x} signals {addr:x} type {sig_type} value {value} count {count} (memory {})",
            // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
            unsafe { load32(addr) as i32 }
        ));
    }
    let _wakes = DeferredWakeScope;
    let _kl = KERNEL_LOCK.lock();
    let mut ks = kstate().lock_or_recover();
    let waiters = ks
        .addr_waiters
        .iter()
        .filter(|t| t.data.lock_or_recover().wait_addr == addr)
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
        if ks.addr_waiters[i].data.lock_or_recover().wait_addr != addr {
            i += 1;
            continue;
        }
        let t = ks.addr_waiters.remove(i);
        wake_thread(&t);
        n += 1;
    }
    RC_OK
}

pub(crate) fn svc_wait_synchronization(
    c: &mut AcnhCpu,
    handles_ptr: u64,
    count: i32,
    timeout: i64,
) -> u32 {
    let _kl = KERNEL_LOCK.lock();
    let mut handles = Vec::with_capacity(count.max(0) as usize);
    for i in 0..count.max(0) {
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        let h = unsafe { ld_u32(handles_ptr + 4 * (i as u64)) };
        let sig = (host().obj_signaled)(h);
        if sig < 0 {
            let first_time = kstate().lock_or_recover().invalid_wait_handles.insert(h);
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
        .lock_or_recover()
        .sync_waiters
        .push(Arc::clone(&self_t));
    struct LeaveGuard(Arc<KThread>);
    impl Drop for LeaveGuard {
        fn drop(&mut self) {
            let mut ks = kstate().lock_or_recover();
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
            let mut d = self_t.data.lock_or_recover();
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
            let d = self_t.data.lock_or_recover();
            let (_g, res) = self_t.cv.wait_timeout_or_recover(d, dl - now);
            let timed_out = res.timed_out();
            drop(_g);
            KERNEL_LOCK.relock_all(depth);
            if timed_out && ready_idx(&handles).is_none() {
                return RC_TIMEOUT;
            }
        } else {
            let d = self_t.data.lock_or_recover();
            let _g = self_t.cv.wait_or_recover(d);
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
        let mut d = t.data.lock_or_recover();
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
    let mut ctx_copy = t.data.lock_or_recover().ctx;
    (host().call_guest)(&mut ctx_copy, entry);
    t.data.lock_or_recover().ctx = ctx_copy;

    let _kl = KERNEL_LOCK.lock();
    let handle = {
        let mut d = t.data.lock_or_recover();
        d.exited = true;
        d.handle
    };
    (host().on_thread_exited)(handle);
    notify_all_locked();
}

pub(crate) fn new_thread(entry: u64, arg: u64, sp: u64, prio: i32, core: i32) -> Arc<KThread> {
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
            .lock_or_recover()
            .threads
            .insert(handle, Arc::clone(&t));
    }
    all_threads().lock_or_recover().push(Arc::clone(&t));
    (host().on_thread_created)(handle, id, entry, tls, prio, core);
    t
}

pub(crate) fn alloc_handle() -> u32 {
    let _kl = KERNEL_LOCK.lock();
    let mut ks = kstate().lock_or_recover();
    let h = ks.next_handle;
    ks.next_handle = ks.next_handle.wrapping_add(1);
    h
}

pub(crate) fn notify_all_locked() {
    let waiters: Vec<Arc<KThread>> = kstate().lock_or_recover().sync_waiters.clone();
    for t in waiters {
        t.cv.notify_one();
    }
}

pub(crate) fn get_thread(handle: u32) -> Option<Arc<KThread>> {
    if handle == PSEUDO_CURRENT_THREAD {
        return current_thread();
    }
    let _kl = KERNEL_LOCK.lock();
    kstate().lock_or_recover().threads.get(&handle).cloned()
}

// ---- SVC Dispatch ------------------------------------------------------------------------------

pub(crate) fn write_meminfo(p: u64, addr: u64, size: u64, state: u32, perm: u32) {
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

pub(crate) fn svc_impl(c: &mut AcnhCpu, imm: u32, pc: u64) {
    if (host().get_trace)() {
        host_log(&format!("svc {imm:#x} from {}", host_describe(pc)));
    }
    let mut rc = RC_OK;
    if imm != 0x0B {
        if let Some(t) = current_thread() {
            t.data.lock_or_recover().yb_run = 0;
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
                let prev = kstate().lock_or_recover().heap_size;
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
                kstate().lock_or_recover().heap_size = size;
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
            let h = t.data.lock_or_recover().handle;
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
                t.data.lock_or_recover().started = true;
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
                    let mut d = t.data.lock_or_recover();
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
                    t.data.lock_or_recover().yb_run = 0;
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
                    let mut d = t.data.lock_or_recover();
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
                let mut d = t.data.lock_or_recover();
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
            t.data.lock_or_recover().last_yield = (host().monotonic_ns)();
            return;
        }
        0x0C => {
            // GetThreadPriority(handle) -> prio
            c.x[1] = get_thread(c.x[1] as u32)
                .map_or(44u64, |t| t.data.lock_or_recover().prio as u32 as u64);
        }
        0x0D => {
            // SetThreadPriority(handle, prio)
            if let Some(t) = get_thread(c.x[0] as u32) {
                t.data.lock_or_recover().prio = c.x[1] as i32;
            }
        }
        0x0E => {
            // GetThreadCoreMask(handle) -> core, mask
            c.x[1] = get_thread(c.x[2] as u32)
                .map_or(0u64, |t| t.data.lock_or_recover().core as u32 as u64);
            c.x[2] = 0xF;
        }
        0x0F => {
            // SetThreadCoreMask(handle, ideal core, mask)
            let h = c.x[0] as u32;
            let mut core_out = 0i32;
            rc = (host().set_thread_core_mask)(h, c.x[1] as i32, c.x[2], &mut core_out);
            if let Some(t) = get_thread(h) {
                t.data.lock_or_recover().core = core_out;
            }
        }
        0x10 => {
            // GetCurrentProcessorNumber
            c.x[0] = current_thread().map_or(0u64, |t| t.data.lock_or_recover().core.max(0) as u64);
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
                t.data.lock_or_recover().cancel = true;
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
            let tls = current_thread().map_or(0, |t| t.data.lock_or_recover().tls);
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
            c.x[1] = get_thread(c.x[1] as u32).map_or(0, |t| t.data.lock_or_recover().id);
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

pub(crate) fn close_handle(h: u32) {
    {
        let _kl = KERNEL_LOCK.lock();
        kstate().lock_or_recover().threads.remove(&h);
    }
    ipc_close_session(h);
    (host().on_handle_close)(h);
}
