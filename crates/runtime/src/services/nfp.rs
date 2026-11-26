//! Split from the old `services.rs` (issue #356).

#[allow(unused_imports)]
use super::*;

// ---- 8. NFP / NFC / HID NFC (hle_nfp.cpp, svc_nfp.cpp, hle_hid_system_nfc.cpp) --------------------------------------

pub(crate) static NFP_HLE_INIT: AtomicBool = AtomicBool::new(false);
pub(crate) static NFP_IPC_INIT: AtomicBool = AtomicBool::new(false);
pub(crate) static NFC_IPC_INIT: AtomicBool = AtomicBool::new(false);

pub(crate) fn nfp_has_tag() -> bool {
    // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
    with_host(|h| h.nfp_has_tag.map(|f| unsafe { f() }).unwrap_or(false)).unwrap_or(false)
}

pub(crate) fn nfp_device_state() -> u32 {
    // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
    with_host(|h| h.nfp_device_state.map(|f| unsafe { f() }).unwrap_or(5)).unwrap_or(5)
}

pub(crate) fn ensure_nfp_hle_init() {
    if !NFP_HLE_INIT.swap(true, Ordering::AcqRel) {
        if let Some(Some(f)) = with_host(|h| h.nfp_on_initialize) {
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            unsafe { f() };
        }
    }
}

pub(crate) fn ensure_nfp_ipc_init() {
    if !NFP_IPC_INIT.swap(true, Ordering::AcqRel) {
        NFC_IPC_INIT.store(true, Ordering::Release);
        if let Some(Some(f)) = with_host(|h| h.nfp_on_initialize) {
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            unsafe { f() };
        }
    }
}

pub(crate) fn nfp_handle_ok(h: u64) -> bool {
    h == NFP_VIRTUAL_DEVICE_HANDLE || (h as u32) == (NFP_VIRTUAL_DEVICE_HANDLE as u32)
}

#[no_mangle]
pub extern "C" fn acnh_nfp_initialize(is_ipc: bool) -> u32 {
    if is_ipc {
        ensure_nfp_ipc_init();
    } else {
        NFP_HLE_INIT.store(true, Ordering::Release);
        if let Some(Some(f)) = with_host(|h| h.nfp_on_initialize) {
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            unsafe { f() };
        }
    }
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_nfp_finalize(is_ipc: bool) -> u32 {
    if is_ipc {
        NFP_IPC_INIT.store(false, Ordering::Release);
    } else {
        NFP_HLE_INIT.store(false, Ordering::Release);
    }
    if let Some(Some(f)) = with_host(|h| h.nfp_on_finalize) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f() };
    }
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_nfp_get_state(is_ipc: bool) -> u32 {
    let init = if is_ipc {
        NFP_IPC_INIT.load(Ordering::Acquire)
    } else {
        NFP_HLE_INIT.load(Ordering::Acquire)
    };
    u32::from(init)
}

#[no_mangle]
pub extern "C" fn acnh_nfp_get_device_state(is_ipc: bool) -> u32 {
    if is_ipc {
        if !NFP_IPC_INIT.load(Ordering::Acquire) {
            return 5; // Unavailable
        }
        return nfp_device_state();
    }
    if !NFP_HLE_INIT.load(Ordering::Acquire) && nfp_has_tag() {
        ensure_nfp_hle_init();
    }
    if !NFP_HLE_INIT.load(Ordering::Acquire) {
        return 5;
    }
    nfp_device_state()
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_nfp_list_devices(out_handles: u64, out_count: u64, max: i32) -> u32 {
    ensure_nfp_hle_init();
    let count = i32::from(max > 0);
    if out_count != 0 {
        std::ptr::write_unaligned(out_count as *mut i32, count);
    }
    if out_handles != 0 && max > 0 {
        std::ptr::write_unaligned(out_handles as *mut u64, NFP_VIRTUAL_DEVICE_HANDLE);
    }
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_nfp_get_npad_id(out_npad: u64, handle_ptr: u64) -> u32 {
    ensure_nfp_hle_init();
    let handle = if handle_ptr != 0 {
        std::ptr::read_unaligned(handle_ptr as *const u64)
    } else {
        u64::MAX
    };
    if handle != NFP_VIRTUAL_DEVICE_HANDLE {
        return NFP_ERR_DEVICE_NOT_FOUND;
    }
    if out_npad != 0 {
        std::ptr::write_unaligned(out_npad as *mut u32, NFP_VIRTUAL_NPAD_ID);
    }
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_nfp_mount(model_type: u32, mount_target: u32, check_target: bool) -> u32 {
    ensure_nfp_hle_init();
    if check_target && (mount_target & 3) == 0 {
        return NFP_ERR_WRONG_ARGUMENT;
    }
    if model_type != 0 {
        return NFP_ERR_WRONG_ARGUMENT;
    }
    if !nfp_has_tag() || nfp_device_state() != 2 {
        return NFP_ERR_WRONG_DEVICE_STATE;
    }
    if let Some(Some(f)) = with_host(|h| h.nfp_on_mount) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f() };
    }
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_nfp_unmount() -> u32 {
    if nfp_device_state() != 3 {
        return NFP_ERR_WRONG_DEVICE_STATE;
    }
    if let Some(Some(f)) = with_host(|h| h.nfp_on_unmount) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f() };
    }
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_nfp_fill_info_checked(
    kind: u32,
    out_ptr: u64,
    allow_found: bool,
) -> u32 {
    if allow_found {
        ensure_nfp_hle_init();
    }
    let st = nfp_device_state();
    let valid = if allow_found {
        st == 2 || st == 3
    } else {
        st == 3
    };
    if !valid {
        return NFP_ERR_WRONG_DEVICE_STATE;
    }
    if allow_found && !nfp_has_tag() {
        return NFP_ERR_WRONG_DEVICE_STATE;
    }
    if out_ptr != 0 {
        if let Some(Some(f)) = with_host(|h| h.nfp_fill_info) {
            f(kind, out_ptr as *mut c_void);
        }
    }
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_nfp_exists_app_area(out_ptr: u64) -> u32 {
    ensure_nfp_hle_init();
    let has = with_host(|h| h.nfp_tag_has_app_area.map(|f| f()).unwrap_or(false)).unwrap_or(false);
    let st = nfp_device_state();
    let exists = has && (st == 2 || st == 3);
    if out_ptr != 0 {
        std::ptr::write_unaligned(out_ptr as *mut u8, u8::from(exists));
    }
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_nfp_open_app_area(access_id: u32) -> u32 {
    if nfp_device_state() != 3 {
        return NFP_ERR_WRONG_DEVICE_STATE;
    }
    let ok = with_host(|h| {
        h.nfp_on_open_app_area
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            .map(|f| unsafe { f(access_id) })
            .unwrap_or(false)
    })
    .unwrap_or(false);
    if ok {
        SVC_OK
    } else {
        NFP_ERR_APP_AREA_MISSING
    }
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_nfp_get_app_area(out_ptr: u64, out_size_ptr: u64, size: u32) -> u32 {
    if nfp_device_state() != 3 || out_ptr == 0 {
        return NFP_ERR_WRONG_DEVICE_STATE;
    }
    let ok = with_host(|h| {
        h.nfp_read_app_area
            .map(|f| f(out_ptr as *mut c_void, size))
            .unwrap_or(false)
    })
    .unwrap_or(false);
    if !ok {
        return NFP_ERR_WRONG_DEVICE_STATE;
    }
    if out_size_ptr != 0 {
        std::ptr::write_unaligned(out_size_ptr as *mut u64, u64::from(size));
    }
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_nfp_set_app_area(in_ptr: u64, size: u32) -> u32 {
    if nfp_device_state() != 3 {
        return NFP_ERR_WRONG_DEVICE_STATE;
    }
    if let Some(Some(f)) = with_host(|h| h.nfp_write_app_area) {
        f(in_ptr as *const c_void, size);
    }
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_svc_nfp_ipc_handle_ok(h: u64) -> bool {
    nfp_handle_ok(h)
}

#[no_mangle]
pub extern "C" fn acnh_svc_nfc_ipc_set_init(init: bool) {
    NFC_IPC_INIT.store(init, Ordering::Release);
    if init {
        NFP_IPC_INIT.store(true, Ordering::Release);
    }
}

#[no_mangle]
pub extern "C" fn acnh_svc_nfc_ipc_is_init() -> bool {
    NFC_IPC_INIT.load(Ordering::Acquire)
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_hid_nfc_get_abstracted_pad_id(out_ptr: u64, npad: u32) -> u32 {
    if npad != NFP_VIRTUAL_NPAD_ID {
        return HID_ERR_INVALID_ARGUMENT;
    }
    if out_ptr != 0 {
        std::ptr::write_unaligned(out_ptr as *mut u32, 0);
    }
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_hid_nfc_get_npads(out_ptr: u64, max: i32) -> u32 {
    if max > 0 && out_ptr != 0 {
        std::ptr::write_unaligned(out_ptr as *mut u32, NFP_VIRTUAL_NPAD_ID);
        1
    } else {
        0
    }
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_hid_nfc_detail_get_npads(
    count_ptr: u64,
    pads_ptr: u64,
    max: i32,
) -> u32 {
    let count = i32::from(max > 0);
    if count_ptr != 0 {
        std::ptr::write_unaligned(count_ptr as *mut i32, count);
    }
    if count > 0 && pads_ptr != 0 {
        std::ptr::write_unaligned(pads_ptr as *mut u32, NFP_VIRTUAL_NPAD_ID);
    }
    SVC_OK
}
