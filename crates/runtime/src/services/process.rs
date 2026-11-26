//! Split from the old `services.rs` (issue #356).

#[allow(unused_imports)]
use super::*;

// ---- 1. Process SVCs (svc_process.cpp) ------------------------------------------------------------------------------

pub(crate) struct ProcessState {
    pub(crate) tls_template: Vec<u8>,
    pub(crate) user_exc_ctx: u64,
}

pub(crate) static PROCESS: Mutex<ProcessState> = Mutex::new(ProcessState {
    tls_template: Vec::new(),
    user_exc_ctx: 0,
});

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_svc_get_info(
    info_type: u32,
    _handle: u32,
    sub: u64,
    out_rc: *mut u32,
) -> u64 {
    if !out_rc.is_null() {
        // SAFETY: the C++ caller passes a writable pointer to a u32 or u64 slot (or null where the code checks first), valid for this call.
        unsafe { *out_rc = SVC_OK };
    }
    // Layout array: [alias_base, alias_size, heap_base, heap_max, addr_base, addr_size, stack_base, stack_size]
    let mut layout = [0u64; 8];
    if let Some(Some(f)) = with_host(|h| h.get_mem_layout) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f(layout.as_mut_ptr(), layout.len()) };
    }
    // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
    let heap_sz = with_host(|h| h.heap_size.map(|f| unsafe { f() }))
        .flatten()
        .unwrap_or(0);
    let mut sys_res = 0u64;
    let mut prog_id = 0u64;
    if let Some(Some(f)) = with_host(|h| h.get_game_info) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f(&mut sys_res, &mut prog_id) };
    }

    match info_type {
        0 => 0xF,
        1 => u64::MAX,
        2 => layout[0],
        3 => layout[1],
        4 => layout[2],
        5 => layout[3],
        6 => 0xCD50_0000,
        7 => heap_sz + 0x0400_0000,
        8 | 9 | 10 | 17 | 27 | 28 | 0xF000_0002 => 0,
        11 => 0x1234_5678_90AB_CDEFu64 ^ sub,
        12 => layout[4],
        13 => layout[5],
        14 => layout[6],
        15 => layout[7],
        16 => sys_res,
        18 => prog_id,
        20 => {
            let mut p = PROCESS.lock().unwrap();
            if p.user_exc_ctx == 0 {
                p.user_exc_ctx = host_kernel_alloc(0x1000);
            }
            p.user_exc_ctx
        }
        21 => 0xCD50_0000,
        22 => heap_sz + 0x0400_0000,
        23 => 1,
        24 => 512,
        26 => 1,
        _ => {
            host_log(&format!("svcGetInfo: unknown type {info_type} sub {sub}"));
            if !out_rc.is_null() {
                // SAFETY: the C++ caller passes a writable pointer to a u32 or u64 slot (or null where the code checks first), valid for this call.
                unsafe { *out_rc = SVC_ERR_GENERIC };
            }
            0
        }
    }
}

pub fn pick_ideal_core(ideal: i32, mask: u64) -> Option<i32> {
    if ideal == -3 {
        return None;
    }
    let mut core = if ideal == -2 { 0 } else { ideal };
    if core < 0 || ((mask >> (core as u32)) & 1) == 0 {
        for k in 0..4 {
            if ((mask >> k) & 1) != 0 {
                core = k;
                break;
            }
        }
    }
    if core >= 0 {
        Some(core)
    } else {
        None
    }
}

#[no_mangle]
pub extern "C" fn acnh_svc_set_thread_core_mask(handle: u32, ideal: i32, mask: u64) -> u32 {
    if let Some(core) = pick_ideal_core(ideal, mask) {
        if let Some(Some(f)) = with_host(|h| h.set_thread_core) {
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            unsafe { f(handle, core) };
        }
    }
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_svc_map_transfer_memory(handle: u32, addr: u64, size: u64) -> u32 {
    let mut tm_addr = 0u64;
    let mut tm_size = 0u64;
    let found = with_host(|h| {
        h.get_transfer_memory
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            .map(|f| unsafe { f(handle, &mut tm_addr, &mut tm_size) })
            .unwrap_or(false)
    })
    .unwrap_or(false);
    if !found {
        return SVC_ERR_INVALID_HANDLE;
    }
    if let Some(Some(f)) = with_host(|h| h.mem_commit) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f(addr, size) };
    }
    let n = size.min(tm_size) as usize;
    if n > 0 && addr != 0 && tm_addr != 0 {
        // SAFETY: the range is guest or caller memory of the length passed, mapped for the life of the process; the C++ dispatcher is stopped while this call runs, so nothing else writes it.
        unsafe { std::ptr::copy_nonoverlapping(tm_addr as *const u8, addr as *mut u8, n) };
    }
    if let Some(Some(f)) = with_host(|h| h.vm_set) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f(addr, size, 3, 3) }; // kMemAlias, perm 3
    }
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_svc_unmap_transfer_memory(handle: u32, addr: u64, size: u64) -> u32 {
    let mut tm_addr = 0u64;
    let mut tm_size = 0u64;
    let found = with_host(|h| {
        h.get_transfer_memory
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            .map(|f| unsafe { f(handle, &mut tm_addr, &mut tm_size) })
            .unwrap_or(false)
    })
    .unwrap_or(false);
    if found {
        let n = size.min(tm_size) as usize;
        if n > 0 && addr != 0 && tm_addr != 0 {
            // SAFETY: the range is guest or caller memory of the length passed, mapped for the life of the process; the C++ dispatcher is stopped while this call runs, so nothing else writes it.
            unsafe { std::ptr::copy_nonoverlapping(addr as *const u8, tm_addr as *mut u8, n) };
        }
    }
    if let Some(Some(f)) = with_host(|h| h.mem_decommit) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f(addr, size) };
    }
    if let Some(Some(f)) = with_host(|h| h.vm_set) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f(addr, size, 0, 0) }; // kMemFree, perm 0
    }
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_svc_set_tls_template(start: u64, end: u64) {
    let mut p = PROCESS.lock().unwrap();
    if end > start && start != 0 {
        let len = (end - start) as usize;
        let slice = std::slice::from_raw_parts(start as *const u8, len);
        p.tls_template.clear();
        p.tls_template.extend_from_slice(slice);
    } else {
        p.tls_template.clear();
    }
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_svc_init_tls_block(block: u64) {
    let p = PROCESS.lock().unwrap();
    if !p.tls_template.is_empty() && block != 0 {
        std::ptr::copy_nonoverlapping(
            p.tls_template.as_ptr(),
            (block + 0x10) as *mut u8,
            p.tls_template.len(),
        );
    }
}
