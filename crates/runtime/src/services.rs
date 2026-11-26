//! Host OS services (HLE + IPC services): the Services group (issue #57).
//! Replaces `svc_am.cpp`, `svc_set.cpp`, `svc_misc.cpp`, `svc_process.cpp`, `svc_hid.cpp`,
//! `svc_nfp.cpp`, `hle_ro.cpp`, `hle_vi.cpp`, `hle_hid_system_nfc.cpp`, `hle_net.cpp`,
//! `hle_web.cpp`, `hle_pl.cpp`, `hle_album.cpp`, `hle_jpeg.cpp`, `hle_nfp.cpp`, `hle_bcat.cpp`,
//! `hle_movie.cpp`, `hle_friends.cpp`, `hle_ngc.cpp`, and `hle_ldn.cpp`.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

// ---- Constants ------------------------------------------------------------------------------------------------------

pub const SVC_OK: u32 = 0;
pub const SVC_ERR_GENERIC: u32 = 0xF001;
pub const SVC_ERR_INVALID_HANDLE: u32 = 0xE401;

pub const AM_ERR_NO_MESSAGE: u32 = 0x680;
pub const AM_ERR_NO_STORAGE: u32 = 0x2A80;
pub const AM_MSG_FOCUS_STATE_CHANGED: u32 = 15;
pub const AM_MSG_OPERATION_MODE_CHANGED: u32 = 30;
pub const AM_MSG_PERFORMANCE_MODE_CHANGED: u32 = 31;

pub const SET_LANGUAGE_COUNT: i32 = 18;

pub const BCAT_ERR_NOT_FOUND: u32 = 122 | (30 << 9);
pub const BCAT_ERR_NO_INTERNET: u32 = 122 | (2 << 9);

pub const RO_ERR_NOT_LOADED: u32 = (2 << 9) | 22;

pub const NIFM_ERR_NO_NETWORK: u32 = (3400 << 9) | 110;
pub const NET_ERR_GENERIC_FAIL: u32 = (1 << 9) | 110;
pub const NET_EAI_FAIL: u32 = 4;

pub const FRIENDS_ERR_NOT_LINKED: u32 = 121 | (4 << 9);
pub const FRIENDS_RECORD_SIZE: usize = 0x200;

pub const HID_ERR_INVALID_ARGUMENT: u32 = 124 | (10 << 9);

pub const NFP_ERR_DEVICE_NOT_FOUND: u32 = 115 | (64 << 9);
pub const NFP_ERR_WRONG_DEVICE_STATE: u32 = 115 | (73 << 9);
pub const NFP_ERR_WRONG_ARGUMENT: u32 = 115 | (10 << 9);
pub const NFP_ERR_APP_AREA_MISSING: u32 = 115 | (128 << 9);
pub const NFP_VIRTUAL_DEVICE_HANDLE: u64 = 0;
pub const NFP_VIRTUAL_NPAD_ID: u32 = 0;
pub const NFP_APP_AREA_SIZE: u32 = 0xD8;

pub const LDN_ERR_DEVICE_DISABLED: u32 = (22 << 9) | 203;
pub const LDN_ERR_INVALID_STATE: u32 = (22 << 9) | 32;
pub const LDN_INFO_SIZE: usize = 0x480;
pub const LDN_ADVERTISE_MAX: usize = 0x180;

pub const MOVIE_OK: u32 = 0;
pub const MOVIE_ERR_INVALID: u32 = 0x100b;
pub const MOVIE_ERR_NO_OUTPUT_BUFFER: u32 = 0x100e;
pub const MOVIE_ERR_NO_BUFFER: u32 = 0x1011;
pub const MOVIE_FLAG_EOS: u32 = 4;
pub const MOVIE_CODEC_VP8: i32 = 6;
pub const MOVIE_CODEC_VP9: i32 = 7;
pub const MOVIE_CODEC_VORBIS: i32 = 11;

// ---- Host Callback Table --------------------------------------------------------------------------------------------

#[repr(C)]
pub struct AcnhFriendInfo {
    pub nsa: u64,
    pub pid: u64,
    pub status: u32,
    pub favorite: u8,
    pub _pad: [u8; 3],
    pub app_id: [c_char; 32],
    pub name: [c_char; 33],
    pub _pad2: [u8; 7],
}

#[repr(C)]
pub struct AcnhServicesHost {
    pub log: Option<unsafe extern "C" fn(*const c_char)>,
    pub config_is_handheld: Option<unsafe extern "C" fn() -> bool>,
    pub config_get_bool: Option<unsafe extern "C" fn(*const c_char, bool) -> bool>,
    pub desired_language: Option<unsafe extern "C" fn() -> u64>,
    pub language_code_for_enum: Option<unsafe extern "C" fn(u32) -> u64>,

    pub monotonic_ns: Option<unsafe extern "C" fn() -> u64>,
    pub sleep_ns: Option<unsafe extern "C" fn(i64)>,
    pub wait_vblank: Option<unsafe extern "C" fn() -> u64>,
    pub create_window: Option<unsafe extern "C" fn() -> *mut c_void>,
    pub release_window: Option<unsafe extern "C" fn(*mut c_void)>,
    pub game_root: Option<unsafe extern "C" fn(*mut c_char, usize) -> usize>,
    pub data_root: Option<unsafe extern "C" fn(*mut c_char, usize) -> usize>,
    pub rumble: Option<unsafe extern "C" fn(f32, f32, u32)>,
    pub wifi_multicast: Option<unsafe extern "C" fn(bool)>,

    pub kernel_alloc: Option<unsafe extern "C" fn(u64) -> u64>,
    pub mem_commit: Option<unsafe extern "C" fn(u64, u64)>,
    pub mem_decommit: Option<unsafe extern "C" fn(u64, u64)>,
    pub vm_set: Option<unsafe extern "C" fn(u64, u64, u32, u32)>,
    pub heap_size: Option<unsafe extern "C" fn() -> u64>,
    pub get_mem_layout: Option<unsafe extern "C" fn(*mut u64, usize)>,
    pub get_game_info: Option<unsafe extern "C" fn(*mut u64, *mut u64)>,

    pub set_thread_core: Option<unsafe extern "C" fn(u32, i32) -> bool>,
    pub get_transfer_memory: Option<unsafe extern "C" fn(u32, *mut u64, *mut u64) -> bool>,

    pub event_create: Option<unsafe extern "C" fn(bool) -> u32>,
    pub event_signal: Option<unsafe extern "C" fn(u32, bool)>,
    pub notify_all: Option<unsafe extern "C" fn()>,
    pub attach_system_event: Option<unsafe extern "C" fn(*mut c_void, u64, u32)>,

    pub user_create_storage: Option<unsafe extern "C" fn(u32, *mut u8, usize) -> u32>,
    pub preselected_user_storage: Option<unsafe extern "C" fn() -> u32>,

    pub nvn_wait_finish: Option<unsafe extern "C" fn()>,
    pub nvn_worker_finish: Option<unsafe extern "C" fn()>,
    pub nvn_cpu_read_after_write_backs: Option<unsafe extern "C" fn(*const u8, u64) -> bool>,
    pub nvn_album_capture: Option<unsafe extern "C" fn(u64, *const c_char)>,
    pub nvn_screenshot_save_rgba: Option<unsafe extern "C" fn(*const u8, u32, u32, *const c_char)>,
    pub nvn_texture_at_memory: Option<unsafe extern "C" fn(u64) -> u64>,
    pub nvn_textures_written: Option<unsafe extern "C" fn(u64, usize)>,

    pub save_island_display_name: Option<unsafe extern "C" fn(*mut c_char, usize) -> usize>,

    pub nfp_has_tag: Option<unsafe extern "C" fn() -> bool>,
    pub nfp_device_state: Option<unsafe extern "C" fn() -> u32>,
    pub nfp_on_initialize: Option<unsafe extern "C" fn()>,
    pub nfp_on_finalize: Option<unsafe extern "C" fn()>,
    pub nfp_on_start_detection: Option<unsafe extern "C" fn()>,
    pub nfp_on_stop_detection: Option<unsafe extern "C" fn()>,
    pub nfp_on_mount: Option<unsafe extern "C" fn()>,
    pub nfp_on_unmount: Option<unsafe extern "C" fn()>,
    pub nfp_tag_has_app_area: Option<unsafe extern "C" fn() -> bool>,
    pub nfp_on_open_app_area: Option<unsafe extern "C" fn(u32) -> bool>,
    pub nfp_read_app_area: Option<unsafe extern "C" fn(*mut c_void, u32) -> bool>,
    pub nfp_write_app_area: Option<unsafe extern "C" fn(*const c_void, u32)>,
    pub nfp_fill_info: Option<unsafe extern "C" fn(u32, *mut c_void)>,
    pub nfp_tag_path: Option<unsafe extern "C" fn(*mut c_char, usize) -> usize>,
    pub nfp_bind_system_event: Option<unsafe extern "C" fn(*mut c_void, u64, i32)>,
    pub nfp_signal_availability: Option<unsafe extern "C" fn()>,
    pub nfp_ensure_ready: Option<unsafe extern "C" fn(*mut c_void)>,

    pub nextendo_online_active: Option<unsafe extern "C" fn() -> bool>,
    pub nextendo_is_nextendo: Option<unsafe extern "C" fn() -> bool>,
    pub nextendo_signed_in: Option<unsafe extern "C" fn() -> bool>,
    pub nextendo_version_ok: Option<unsafe extern "C" fn() -> bool>,
    pub nextendo_account_nsa: Option<unsafe extern "C" fn() -> u64>,
    pub nextendo_ldn_wanted: Option<unsafe extern "C" fn() -> bool>,
    pub nextendo_friends_count: Option<unsafe extern "C" fn() -> usize>,
    pub nextendo_friends_get: Option<unsafe extern "C" fn(usize, *mut AcnhFriendInfo) -> bool>,
    pub nextendo_friends_refresh: Option<unsafe extern "C" fn()>,
    pub nextendo_presence_playing: Option<unsafe extern "C" fn(bool)>,
    pub time_refresh_clocks: Option<unsafe extern "C" fn()>,
    pub ui_open_online: Option<unsafe extern "C" fn()>,

    pub hostnet_active: Option<unsafe extern "C" fn() -> bool>,
    pub hostnet_local_ip: Option<unsafe extern "C" fn() -> u32>,

    pub ldn_link_host_start: Option<unsafe extern "C" fn()>,
    pub ldn_link_stop: Option<unsafe extern "C" fn()>,
    pub ldn_link_scan: Option<unsafe extern "C" fn(*mut u8, i32, *const u8) -> i32>,
    pub ldn_link_join: Option<unsafe extern "C" fn(*const u8, *const u8, *mut u8) -> i32>,
    pub ldn_local_address: Option<unsafe extern "C" fn() -> u32>,
    pub ldn_island_name: Option<unsafe extern "C" fn(*const u8, *mut c_char, usize) -> usize>,
    pub vsock_reset: Option<unsafe extern "C" fn()>,

    pub movie_codec_open: Option<unsafe extern "C" fn(i32, i32, *const u8, usize) -> *mut c_void>,
    pub movie_codec_close: Option<unsafe extern "C" fn(*mut c_void, i32)>,
    pub movie_codec_decode_video: Option<
        unsafe extern "C" fn(
            *mut c_void,
            i32,
            *const u8,
            usize,
            *mut u8,
            *mut usize,
            *mut i32,
            *mut i32,
        ) -> bool,
    >,
    pub movie_codec_decode_audio: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const u8,
            usize,
            *mut u8,
            *mut usize,
            *mut i32,
            *mut i32,
        ) -> bool,
    >,
}

// Safety: host function pointers are thread-safe C callbacks set once at boot.
unsafe impl Send for AcnhServicesHost {}
// SAFETY: the host table holds only plain function pointers installed once and never mutated afterwards, so sharing it across threads is sound.
unsafe impl Sync for AcnhServicesHost {}

static HOST: Mutex<Option<AcnhServicesHost>> = Mutex::new(None);

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_services_set_host(host: *const AcnhServicesHost) {
    if host.is_null() {
        return;
    }
    let mut g = HOST.lock().unwrap();
    *g = Some(std::ptr::read(host));
}

fn with_host<R>(f: impl FnOnce(&AcnhServicesHost) -> R) -> Option<R> {
    let g = HOST.lock().unwrap();
    g.as_ref().map(f)
}

fn host_log(msg: &str) {
    if let Some(Some(f)) = with_host(|h| h.log) {
        if let Ok(cstr) = CString::new(msg) {
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            unsafe { f(cstr.as_ptr()) };
        }
    }
}

fn host_is_handheld() -> bool {
    with_host(|h| {
        h.config_is_handheld
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            .map(|f| unsafe { f() })
            .unwrap_or(false)
    })
    .unwrap_or(false)
}

fn host_config_bool(key: &str, default: bool) -> bool {
    with_host(|h| {
        if let Some(f) = h.config_get_bool {
            if let Ok(c) = CString::new(key) {
                // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
                return unsafe { f(c.as_ptr(), default) };
            }
        }
        default
    })
    .unwrap_or(default)
}

fn host_monotonic_ns() -> u64 {
    // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
    with_host(|h| h.monotonic_ns.map(|f| unsafe { f() }))
        .flatten()
        .unwrap_or_else(|| {
            static START: OnceLock<Instant> = OnceLock::new();
            START.get_or_init(Instant::now).elapsed().as_nanos() as u64
        })
}

fn host_sleep_ns(ns: i64) {
    if ns <= 0 {
        return;
    }
    if let Some(Some(f)) = with_host(|h| h.sleep_ns) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f(ns) };
    } else {
        std::thread::sleep(Duration::from_nanos(ns as u64));
    }
}

fn host_kernel_alloc(size: u64) -> u64 {
    // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
    with_host(|h| h.kernel_alloc.map(|f| unsafe { f(size) }))
        .flatten()
        .unwrap_or(0)
}

fn host_event_create(signaled: bool) -> u32 {
    // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
    with_host(|h| h.event_create.map(|f| unsafe { f(signaled) }))
        .flatten()
        .unwrap_or(0)
}

fn host_event_signal(ev: u32, signaled: bool) {
    if ev == 0 {
        return;
    }
    if let Some(Some(f)) = with_host(|h| h.event_signal) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f(ev, signaled) };
    }
}

fn host_notify_all() {
    if let Some(Some(f)) = with_host(|h| h.notify_all) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f() };
    }
}

// ---- 1. Process SVCs (svc_process.cpp) ------------------------------------------------------------------------------

struct ProcessState {
    tls_template: Vec<u8>,
    user_exc_ctx: u64,
}

static PROCESS: Mutex<ProcessState> = Mutex::new(ProcessState {
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

// ---- 2. Set / Misc / AM Services (svc_set.cpp, svc_misc.cpp, svc_am.cpp) --------------------------------------------

fn pack_lang_code_ascii(tag: &str) -> u64 {
    let mut buf = [0u8; 8];
    let bytes = tag.as_bytes();
    let n = bytes.len().min(8);
    buf[..n].copy_from_slice(&bytes[..n]);
    u64::from_le_bytes(buf)
}

const LANG_TAGS: [&str; 18] = [
    "ja", "en-US", "fr", "de", "it", "es", "zh-CN", "ko", "nl", "pt", "ru", "zh-TW", "en-GB",
    "fr-CA", "es-419", "zh-Hans", "zh-Hant", "pt-BR",
];

#[no_mangle]
pub extern "C" fn acnh_svc_set_language_code_for_enum(idx: u32) -> u64 {
    if let Some(Some(f)) = with_host(|h| h.language_code_for_enum) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        return unsafe { f(idx) };
    }
    let tag = LANG_TAGS.get(idx as usize).copied().unwrap_or("en-US");
    pack_lang_code_ascii(tag)
}

#[no_mangle]
pub extern "C" fn acnh_svc_set_desired_language() -> u64 {
    // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
    with_host(|h| h.desired_language.map(|f| unsafe { f() }))
        .flatten()
        .unwrap_or_else(|| pack_lang_code_ascii("en-US"))
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_svc_set_push_available_languages(
    out_addr: u64,
    out_size: usize,
) -> i32 {
    if out_addr != 0 && out_size >= (SET_LANGUAGE_COUNT as usize) * 8 {
        let out = std::slice::from_raw_parts_mut(out_addr as *mut u64, SET_LANGUAGE_COUNT as usize);
        for (k, slot) in out.iter_mut().enumerate() {
            *slot = acnh_svc_set_language_code_for_enum(k as u32);
        }
    }
    SET_LANGUAGE_COUNT
}

#[no_mangle]
pub extern "C" fn acnh_svc_set_language_count() -> i32 {
    SET_LANGUAGE_COUNT
}

#[no_mangle]
pub extern "C" fn acnh_svc_set_region_code() -> i32 {
    1 // Americas
}

#[no_mangle]
pub extern "C" fn acnh_svc_apm_performance_mode() -> u32 {
    if host_is_handheld() {
        0
    } else {
        1
    }
}

struct AmState {
    messages: VecDeque<u32>,
    message_event: u32,
    popped_launch_param: bool,
    display_res: Option<(u32, u32)>,
}

static AM_STATE: Mutex<Option<AmState>> = Mutex::new(None);

fn with_am<R>(f: impl FnOnce(&mut AmState) -> R) -> R {
    let mut g = AM_STATE.lock().unwrap();
    let st = g.get_or_insert_with(|| {
        let mut q = VecDeque::new();
        q.push_back(AM_MSG_FOCUS_STATE_CHANGED);
        q.push_back(AM_MSG_OPERATION_MODE_CHANGED);
        q.push_back(AM_MSG_PERFORMANCE_MODE_CHANGED);
        AmState {
            messages: q,
            message_event: 0,
            popped_launch_param: false,
            display_res: None,
        }
    });
    f(st)
}

pub fn parse_render_res(env_val: Option<&str>, handheld: bool) -> (u32, u32) {
    let (mut w, mut h) = if handheld { (1280, 720) } else { (1920, 1080) };
    if let Some(s) = env_val {
        if let Some((ws, hs)) = s.split_once('x') {
            if let (Ok(rw), Ok(rh)) = (ws.trim().parse::<u32>(), hs.trim().parse::<u32>()) {
                if rw >= 320 && rh >= 180 {
                    w = rw;
                    h = rh;
                }
            }
        }
    }
    (w, h)
}

#[no_mangle]
pub extern "C" fn acnh_svc_am_get_message_event() -> u32 {
    with_am(|st| {
        if st.message_event == 0 {
            st.message_event = host_event_create(!st.messages.is_empty());
        } else {
            host_event_signal(st.message_event, !st.messages.is_empty());
        }
        st.message_event
    })
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_svc_am_receive_message(out_msg: *mut u32) -> u32 {
    with_am(|st| {
        if let Some(msg) = st.messages.pop_front() {
            if !out_msg.is_null() {
                *out_msg = msg;
            }
            if st.messages.is_empty() && st.message_event != 0 {
                host_event_signal(st.message_event, false);
            }
            SVC_OK
        } else {
            if st.message_event != 0 {
                host_event_signal(st.message_event, false);
            }
            AM_ERR_NO_MESSAGE
        }
    })
}

#[no_mangle]
pub extern "C" fn acnh_svc_am_get_operation_mode() -> u8 {
    if host_is_handheld() {
        0
    } else {
        1
    }
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_svc_am_get_default_display_resolution(
    out_w: *mut u32,
    out_h: *mut u32,
) {
    let (w, h) = with_am(|st| {
        if let Some(res) = st.display_res {
            res
        } else {
            let env = std::env::var("NX_RENDER_RES").ok();
            let res = parse_render_res(env.as_deref(), host_is_handheld());
            st.display_res = Some(res);
            res
        }
    });
    if !out_w.is_null() {
        *out_w = w;
    }
    if !out_h.is_null() {
        *out_h = h;
    }
}

#[no_mangle]
pub extern "C" fn acnh_svc_am_pop_launch_parameter(kind: u32) -> u32 {
    with_am(|st| {
        if kind != 2 || st.popped_launch_param {
            return 0;
        }
        st.popped_launch_param = true;
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        with_host(|h| h.preselected_user_storage.map(|f| unsafe { f() }))
            .flatten()
            .unwrap_or(0)
    })
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_svc_am_player_select(
    in_data: *const u8,
    in_len: usize,
    out_buf: *mut u8,
    out_cap: usize,
) -> usize {
    if out_buf.is_null() || out_cap < 0x18 {
        return 0;
    }
    let out = std::slice::from_raw_parts_mut(out_buf, 0x18);
    out.fill(0);
    let ui = if !in_data.is_null() && in_len >= 4 {
        let b = std::slice::from_raw_parts(in_data, 4);
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    } else {
        0
    };
    let mut uid = [0u8; 16];
    let ok = with_host(|h| {
        h.user_create_storage
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            .map(|f| unsafe { f(ui, uid.as_mut_ptr(), uid.len()) })
            .unwrap_or(0)
    })
    .unwrap_or(0);
    if ok != 0 {
        out[8..24].copy_from_slice(&uid);
    } else {
        let cancelled: u64 = 2;
        out[0..8].copy_from_slice(&cancelled.to_le_bytes());
    }
    0x18
}

// ---- 3. HID Vibration Mixer (svc_hid.cpp) ---------------------------------------------------------------------------

const VIB_DURATION_MS: u32 = 60;
const VIB_STRENGTH: f32 = 3.0;
const VIB_LEFT: usize = 0;
const VIB_RIGHT: usize = 1;
const VIB_COMBINED: usize = 2;

#[derive(Clone, Copy, Default)]
struct Vibration {
    low: f32,
    high: f32,
    until_ns: u64,
}

static VIB_DEVICES: Mutex<[Vibration; 3]> = Mutex::new(
    [Vibration {
        low: 0.0,
        high: 0.0,
        until_ns: 0,
    }; 3],
);

fn clean_amp(v: f32) -> f32 {
    if v.is_finite() {
        v.max(0.0)
    } else {
        0.0
    }
}

fn mix_vibrations_at(devices: &[Vibration; 3], now_ns: u64) -> (f32, f32) {
    let mut low = 0.0f32;
    let mut high = 0.0f32;
    if now_ns < devices[VIB_RIGHT].until_ns {
        low = devices[VIB_RIGHT].low * 0.85 + devices[VIB_RIGHT].high * 0.15;
    }
    if now_ns < devices[VIB_LEFT].until_ns {
        high = devices[VIB_LEFT].low * 0.15 + devices[VIB_LEFT].high * 0.85;
    }
    if now_ns < devices[VIB_COMBINED].until_ns {
        low = low.max(devices[VIB_COMBINED].low);
        high = high.max(devices[VIB_COMBINED].high);
    }
    (
        (low * VIB_STRENGTH).min(1.0),
        (high * VIB_STRENGTH).min(1.0),
    )
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_svc_hid_send_vibration(
    handles_addr: u64,
    handles_size: u64,
    values_addr: u64,
    values_size: u64,
) -> u32 {
    const HANDLE_SIZE: u64 = 4;
    const VALUE_SIZE: u64 = 16;
    if handles_addr == 0
        || values_addr == 0
        || !handles_size.is_multiple_of(HANDLE_SIZE)
        || !values_size.is_multiple_of(VALUE_SIZE)
        || values_size / VALUE_SIZE != handles_size / HANDLE_SIZE
    {
        return SVC_OK;
    }
    if !host_config_bool("input.rumble", true) {
        return SVC_OK;
    }
    let count = (handles_size / HANDLE_SIZE) as usize;
    let now_ns = host_monotonic_ns();
    let until_ns = now_ns + u64::from(VIB_DURATION_MS) * 1_000_000;
    let mut player_one = false;
    let mut devs = VIB_DEVICES.lock().unwrap();
    for i in 0..count {
        let handle =
            std::ptr::read_unaligned((handles_addr + (i as u64) * HANDLE_SIZE) as *const u32);
        if ((handle >> 8) & 0xff) != 0 {
            continue;
        }
        let device = ((handle >> 16) & 0xff) as usize;
        if device >= 3 {
            continue;
        }
        player_one = true;
        let val_ptr = (values_addr + (i as u64) * VALUE_SIZE) as *const f32;
        let low = clean_amp(std::ptr::read_unaligned(val_ptr));
        let high = clean_amp(std::ptr::read_unaligned(val_ptr.add(2)));
        devs[device] = Vibration {
            low,
            high,
            until_ns,
        };
    }
    if player_one {
        let (low, high) = mix_vibrations_at(&devs, now_ns);
        let dur = if low > 0.001 || high > 0.001 {
            VIB_DURATION_MS
        } else {
            0
        };
        if let Some(Some(f)) = with_host(|h| h.rumble) {
            f(low, high, dur);
        }
    }
    SVC_OK
}

// ---- 4. Small HLE Modules (bcat, ngc, ro, web, jpeg, album) ---------------------------------------------------------

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_hle_bcat_directory_read(count_ptr: u64) -> u32 {
    if count_ptr != 0 {
        std::ptr::write_unaligned(count_ptr as *mut i32, 0);
    }
    BCAT_ERR_NOT_FOUND
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_hle_bcat_file_read(read_ptr: u64) -> u32 {
    if read_ptr != 0 {
        std::ptr::write_unaligned(read_ptr as *mut u64, 0);
    }
    BCAT_ERR_NOT_FOUND
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_hle_bcat_get_digest(out_ptr: u64) {
    if out_ptr != 0 {
        std::ptr::write_unaligned(out_ptr as *mut u64, 0);
        std::ptr::write_unaligned((out_ptr + 8) as *mut u64, 0);
    }
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_hle_ngc_check_words(results_ptr: u64, count: u64) -> u32 {
    if results_ptr != 0 {
        for k in 0..count {
            std::ptr::write_unaligned((results_ptr + 4 * k) as *mut u32, 0);
        }
    }
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_hle_ngc_mask_words(masked_ptr: u64) -> u32 {
    if masked_ptr != 0 {
        std::ptr::write_unaligned(masked_ptr as *mut i32, 0);
    }
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_hle_ro_get_buffer_size(out_ptr: u64) -> u32 {
    if out_ptr != 0 {
        std::ptr::write_unaligned(out_ptr as *mut u64, 0x1000);
    }
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_hle_ro_load_module() -> u32 {
    host_log("nn::ro::LoadModule: NRO not supported (EOS SDK); reporting failure");
    RO_ERR_NOT_LOADED
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_hle_web_show_share_page(ret_ptr: u64) -> u32 {
    if ret_ptr != 0 {
        std::ptr::write_unaligned(ret_ptr as *mut u64, 1);
    }
    host_log("web: ShowSharePage answered as closed (the share page is not emulated)");
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_hle_jpeg_set_pixel_data(pixels: u64, dim_ptr: u64, align: i32) {
    if pixels == 0 || dim_ptr == 0 {
        return;
    }
    let width = std::ptr::read_unaligned(dim_ptr as *const i32);
    let height = std::ptr::read_unaligned((dim_ptr + 4) as *const i32);
    if width > 0 && height > 0 && width <= 8192 && height <= 8192 {
        let a = if align > 0 { align as u64 } else { 1 };
        let size = (width as u64).div_ceil(a) * a * 4 * (height as u64);
        if let Some(Some(f)) = with_host(|h| h.nvn_wait_finish) {
            f();
        }
        if let Some(Some(f)) = with_host(|h| h.nvn_worker_finish) {
            f();
        }
        let waited = with_host(|h| {
            h.nvn_cpu_read_after_write_backs
                .map(|f| f(pixels as *const u8, size))
                .unwrap_or(false)
        })
        .unwrap_or(false);
        let suffix = if waited {
            ": waited for the GPU's copy to land"
        } else {
            ""
        };
        host_log(&format!(
            "jpeg: SetPixelData {width}x{height} at {pixels:x}{suffix}"
        ));
    }
}

pub fn sanitize_file_name_part(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if (ch as u32) < 0x20 || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        {
            continue;
        }
        out.push(ch);
    }
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    if out.is_empty() {
        String::from("Island")
    } else {
        out
    }
}

static ALBUM_STAMP_STATE: Mutex<(String, i32)> = Mutex::new((String::new(), 0));

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_hle_album_save_screenshot(
    data: u64,
    size: u64,
    image_size: u32,
    stamp_cstr: *const c_char,
) -> u32 {
    const WIDTH: u32 = 1280;
    const HEIGHT: u32 = 720;
    if data == 0 || image_size != 0 || size < u64::from(WIDTH) * u64::from(HEIGHT) * 4 {
        host_log(&format!(
            "album: SaveScreenshot(size {size}, image size {image_size}) not saved"
        ));
        return SVC_OK;
    }
    let visited = ldn_visited_island_string();
    let island = if !visited.is_empty() {
        format!("Visit_{}", sanitize_file_name_part(&visited))
    } else {
        let mut buf = [0 as c_char; 128];
        let n = with_host(|h| {
            h.save_island_display_name
                .map(|f| f(buf.as_mut_ptr(), buf.len()))
                .unwrap_or(0)
        })
        .unwrap_or(0);
        let raw = if n > 0 {
            CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
        } else {
            String::new()
        };
        sanitize_file_name_part(&raw)
    };
    let stamp = if stamp_cstr.is_null() {
        String::from("0000-00-00_00-00-00")
    } else {
        CStr::from_ptr(stamp_cstr).to_string_lossy().into_owned()
    };
    let mut stem = format!("{island}_{stamp}");
    {
        let mut st = ALBUM_STAMP_STATE.lock().unwrap();
        if st.0 == stem {
            st.1 += 1;
        } else {
            st.0 = stem.clone();
            st.1 = 1;
        }
        if st.1 > 1 {
            stem.push_str(&format!("_{}", st.1));
        }
    }
    if let Ok(cstem) = CString::new(stem) {
        let tex = with_host(|h| h.nvn_texture_at_memory.map(|f| f(data)))
            .flatten()
            .unwrap_or(0);
        if tex != 0 {
            if let Some(Some(f)) = with_host(|h| h.nvn_album_capture) {
                f(tex, cstem.as_ptr());
            }
        } else if let Some(Some(f)) = with_host(|h| h.nvn_screenshot_save_rgba) {
            f(data as *const u8, WIDTH, HEIGHT, cstem.as_ptr());
        }
    }
    SVC_OK
}

// ---- 5. Shared System Fonts (hle_pl.cpp) ----------------------------------------------------------------------------

#[derive(Clone, Copy, Default)]
struct SharedFont {
    addr: u64,
    size: u64,
}

static SHARED_FONTS: Mutex<[SharedFont; 6]> = Mutex::new([SharedFont { addr: 0, size: 0 }; 6]);

fn host_game_root() -> String {
    let mut buf = [0 as c_char; 1024];
    let n = with_host(|h| {
        h.game_root
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            .map(|f| unsafe { f(buf.as_mut_ptr(), buf.len()) })
            .unwrap_or(0)
    })
    .unwrap_or(0);
    if n == 0 {
        return String::new();
    }
    // SAFETY: the buffer is NUL-terminated: it is a fixed-size array we zero-initialised and the host callback filled, or a C string the caller passed.
    unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(all(target_os = "linux", not(target_os = "android")))]
fn linux_font_candidates() -> Vec<String> {
    #[cfg(all(target_os = "linux", not(target_os = "android")))]
    {
        let mut out = Vec::new();
        if let Ok(output) = std::process::Command::new("fc-match")
            .args(["-s", "-f", "%{file}\n", "sans-serif:fontformat=TrueType"])
            .output()
        {
            if output.status.success() {
                for line in String::from_utf8_lossy(&output.stdout).lines().take(8) {
                    let p = line.trim();
                    if p.ends_with(".ttf") || p.ends_with(".TTF") {
                        out.push(p.to_string());
                    }
                }
            }
        }
        let mut dirs = vec![
            String::from("/usr/share/fonts"),
            String::from("/usr/local/share/fonts"),
        ];
        if let Ok(home) = std::env::var("HOME") {
            dirs.push(format!("{home}/.local/share/fonts"));
            dirs.push(format!("{home}/.fonts"));
            dirs.push(format!("{home}/.local/share/Steam/clientui/fonts"));
            dirs.push(format!("{home}/.steam/steam/clientui/fonts"));
        }
        let mut good = Vec::new();
        let mut other = Vec::new();
        fn walk(
            dir: &std::path::Path,
            budget: &mut usize,
            good: &mut Vec<String>,
            other: &mut Vec<String>,
        ) {
            if *budget == 0 {
                return;
            }
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in rd.flatten() {
                if *budget == 0 {
                    break;
                }
                *budget -= 1;
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, budget, good, other);
                } else if let Some(s) = path.to_str() {
                    if s.ends_with(".ttf") || s.ends_with(".TTF") {
                        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        let regular = name.contains("Regular") || name.contains("Sans.ttf");
                        let styled = name.contains("Bold")
                            || name.contains("Italic")
                            || name.contains("Mono")
                            || name.contains("Emoji");
                        if regular && !styled {
                            good.push(s.to_string());
                        } else {
                            other.push(s.to_string());
                        }
                    }
                }
            }
        }
        let mut budget = 20_000usize;
        for d in &dirs {
            walk(std::path::Path::new(d), &mut budget, &mut good, &mut other);
        }
        good.sort();
        other.sort();
        out.extend(good);
        out.extend(other);
        out
    }
    #[cfg(not(all(target_os = "linux", not(target_os = "android"))))]
    {
        Vec::new()
    }
}

fn ensure_shared_font(font_type: u32) -> SharedFont {
    let idx = if font_type < 6 { font_type as usize } else { 0 };
    let mut fonts = SHARED_FONTS.lock().unwrap();
    if fonts[idx].addr != 0 {
        return fonts[idx];
    }
    let root = format!("{}/fonts/", host_game_root());
    let mut candidates = vec![
        format!("{root}shared_{font_type}.ttf"),
        format!("{root}shared.ttf"),
    ];
    if let Ok(env_font) = std::env::var("NX_SHARED_FONT") {
        candidates.push(env_font);
    }
    #[cfg(target_os = "macos")]
    {
        candidates.push(String::from(
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        ));
        candidates.push(String::from("/System/Library/Fonts/Supplemental/Arial.ttf"));
    }
    #[cfg(target_os = "android")]
    {
        candidates.push(String::from("/system/fonts/Roboto-Regular.ttf"));
    }
    #[cfg(target_os = "windows")]
    {
        candidates.push(String::from("C:/Windows/Fonts/arialuni.ttf"));
        candidates.push(String::from("C:/Windows/Fonts/arial.ttf"));
    }
    #[cfg(all(target_os = "linux", not(target_os = "android")))]
    {
        candidates.extend([
            String::from("/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf"),
            String::from("/usr/share/fonts/liberation-sans/LiberationSans-Regular.ttf"),
            String::from("/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf"),
            String::from("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"),
            String::from("/usr/share/fonts/liberation/LiberationSans-Regular.ttf"),
            String::from("/usr/share/fonts/noto/NotoSans-Regular.ttf"),
            String::from("/usr/share/fonts/TTF/LiberationSans-Regular.ttf"),
            String::from("/usr/share/fonts/TTF/DejaVuSans.ttf"),
            String::from("/usr/share/fonts/google-noto/NotoSans-Regular.ttf"),
            String::from("/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf"),
        ]);
        candidates.extend(linux_font_candidates());
    }

    let mut ttf = Vec::new();
    for path in &candidates {
        if let Ok(bytes) = std::fs::read(path) {
            if !bytes.is_empty() {
                host_log(&format!(
                    "pl: shared font {font_type} <- {path} ({} bytes)",
                    bytes.len()
                ));
                ttf = bytes;
                break;
            }
        }
    }
    if ttf.is_empty() {
        host_log(&format!(
            "pl: no font for shared font {font_type} (put one in {root})"
        ));
    }
    let size = (ttf.len() + 8) as u64;
    let addr = host_kernel_alloc(size);
    if addr != 0 {
        // SAFETY: the range is guest or caller memory of the length passed, mapped for the life of the process; the C++ dispatcher is stopped while this call runs, so nothing else writes it.
        unsafe {
            std::ptr::write_unaligned(addr as *mut u32, 0x1802_9A7F);
            std::ptr::write_unaligned((addr + 4) as *mut u32, ttf.len() as u32);
            if !ttf.is_empty() {
                std::ptr::copy_nonoverlapping(ttf.as_ptr(), (addr + 8) as *mut u8, ttf.len());
            }
        }
    }
    fonts[idx] = SharedFont { addr, size };
    fonts[idx]
}

#[no_mangle]
pub extern "C" fn acnh_hle_pl_get_shared_font_address(font_type: u32) -> u64 {
    ensure_shared_font(font_type).addr
}

#[no_mangle]
pub extern "C" fn acnh_hle_pl_get_shared_font_size(font_type: u32) -> u64 {
    ensure_shared_font(font_type).size
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_hle_pl_get_shared_font(
    count_ptr: u64,
    out_ptr: u64,
    max: i32,
) -> u32 {
    let mut n = 0i32;
    for t in 0..6u32 {
        if n >= max {
            break;
        }
        let f = ensure_shared_font(t);
        if out_ptr != 0 {
            let base = out_ptr + 0x18 * (n as u64);
            std::ptr::write_unaligned(base as *mut u32, t);
            std::ptr::write_unaligned((base + 4) as *mut u32, 0);
            std::ptr::write_unaligned((base + 8) as *mut u64, f.addr);
            std::ptr::write_unaligned((base + 0x10) as *mut u64, f.size);
        }
        n += 1;
    }
    if count_ptr != 0 {
        std::ptr::write_unaligned(count_ptr as *mut i32, n);
    }
    SVC_OK
}

// ---- 6. Display / VSync (hle_vi.cpp) --------------------------------------------------------------------------------

static VI_VBLANKS: AtomicU64 = AtomicU64::new(0);
static VI_PER_TICK: AtomicI32 = AtomicI32::new(0);
static VI_TICK_PERIOD: AtomicU64 = AtomicU64::new(0);
static VI_PRESENT_INTERVAL: AtomicI32 = AtomicI32::new(2);
static VI_LAST_DUE: AtomicU64 = AtomicU64::new(0);
static VI_VSYNC_EVENT: AtomicU32 = AtomicU32::new(0);
static VI_VSYNC_STARTED: AtomicBool = AtomicBool::new(false);
static VI_VB_SYNC: (Mutex<()>, Condvar) = (Mutex::new(()), Condvar::new());

pub fn parse_display_period_ns(env_val: Option<&str>) -> u64 {
    if let Some(s) = env_val {
        if let Ok(hz) = s.trim().parse::<i32>() {
            if (30..=360).contains(&hz) {
                return 1_000_000_000u64 / (hz as u64);
            }
        }
    }
    16_666_667
}

#[no_mangle]
pub extern "C" fn acnh_vi_display_period_ns() -> u64 {
    static PERIOD: OnceLock<u64> = OnceLock::new();
    *PERIOD.get_or_init(|| {
        let env = std::env::var("NX_DISPLAY_HZ").ok();
        let p = parse_display_period_ns(env.as_deref());
        if p != 16_666_667 {
            let hz = 1_000_000_000u64 / p;
            host_log(&format!("vi: display at {hz} Hz (NX_DISPLAY_HZ)"));
        }
        p
    })
}

#[no_mangle]
pub extern "C" fn acnh_vi_tick_period_ns() -> u64 {
    let p = VI_TICK_PERIOD.load(Ordering::Relaxed);
    if p != 0 {
        p
    } else {
        acnh_vi_display_period_ns()
    }
}

#[no_mangle]
pub extern "C" fn acnh_vi_set_present_interval(interval: i32) {
    VI_PRESENT_INTERVAL.store(interval, Ordering::Relaxed);
}

#[no_mangle]
pub extern "C" fn acnh_vi_vblank_hold() {
    let k = VI_PER_TICK.load(Ordering::Relaxed);
    let interval = VI_PRESENT_INTERVAL.load(Ordering::Relaxed);
    if k <= 0 || interval <= 0 {
        return;
    }
    let seen = VI_VBLANKS.load(Ordering::Acquire);
    let prev = VI_LAST_DUE.load(Ordering::Relaxed);
    let mut due = prev.saturating_add((interval as u64) * (k as u64));
    if prev == 0 || due <= seen {
        due = seen + 1;
    }
    if due - 1 > seen {
        let mut lk = VI_VB_SYNC.0.lock().unwrap();
        let deadline = Instant::now() + Duration::from_millis(100);
        while VI_VBLANKS.load(Ordering::Acquire) < due - 1 {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (guard, res) = VI_VB_SYNC.1.wait_timeout(lk, deadline - now).unwrap();
            lk = guard;
            if res.timed_out() {
                break;
            }
        }
    }
    VI_LAST_DUE.store(due, Ordering::Relaxed);
}

fn run_real_vsync() {
    if std::env::var_os("NX_DISPLAY_HZ").is_some() {
        return;
    }
    if std::env::var("NX_REAL_VSYNC").as_deref() == Ok("0") {
        return;
    }
    let Some(wait_vb) = with_host(|h| h.wait_vblank).flatten() else {
        return;
    };
    // SAFETY: `wait_vb` is the host's wait-for-vblank callback from acnh_services_set_host; it takes no arguments and is valid for the process lifetime.
    let t0 = unsafe { wait_vb() };
    if t0 == 0 {
        return;
    }
    let mut t = t0;
    for _ in 0..30 {
        // SAFETY: `wait_vb` is the host's wait-for-vblank callback from acnh_services_set_host; it takes no arguments and is valid for the process lifetime.
        t = unsafe { wait_vb() };
        if t == 0 {
            return;
        }
    }
    let p = (t - t0) as f64 / 30.0;
    let r = 16_666_667.0 / p;
    let k = r.round() as i32;
    if !(1..=4).contains(&k) || (r - k as f64).abs() > 0.03 * (k as f64) {
        host_log(&format!(
            "vi: the display refreshes at {:.2} Hz, not a multiple of 60: timed vsync",
            1e9 / p
        ));
        return;
    }
    host_log(&format!(
        "vi: vsync from the display's vblanks ({:.2} Hz, {k} per 60 Hz tick)",
        1e9 / p
    ));
    let mut period = p;
    let mut prev = t;
    VI_TICK_PERIOD.store((period * k as f64) as u64, Ordering::Relaxed);
    VI_PER_TICK.store(k, Ordering::Relaxed);
    let mut n = 0u64;
    loop {
        // SAFETY: `wait_vb` is the host's wait-for-vblank callback from acnh_services_set_host; it takes no arguments and is valid for the process lifetime.
        let now = unsafe { wait_vb() };
        if now == 0 {
            break;
        }
        let d = (now - prev) as f64;
        prev = now;
        if d > 0.5 * p && d < 1.5 * p {
            period = period * 0.99 + d * 0.01;
        }
        {
            let _lk = VI_VB_SYNC.0.lock().unwrap();
            VI_VBLANKS.fetch_add(1, Ordering::Release);
        }
        VI_VB_SYNC.1.notify_all();
        n += 1;
        if !n.is_multiple_of(k as u64) {
            continue;
        }
        VI_TICK_PERIOD.store((period * k as f64) as u64, Ordering::Relaxed);
        let ev = VI_VSYNC_EVENT.load(Ordering::Relaxed);
        host_event_signal(ev, true);
        host_notify_all();
    }
    VI_PER_TICK.store(0, Ordering::Relaxed);
    VI_TICK_PERIOD.store(0, Ordering::Relaxed);
    host_log("vi: the display's vblanks stopped: timed vsync");
}

#[no_mangle]
pub extern "C" fn acnh_vi_get_vsync_event_and_start() -> u32 {
    let mut ev = VI_VSYNC_EVENT.load(Ordering::Acquire);
    if ev == 0 {
        let created = host_event_create(false);
        match VI_VSYNC_EVENT.compare_exchange(0, created, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => ev = created,
            Err(existing) => ev = existing,
        }
    }
    if !VI_VSYNC_STARTED.swap(true, Ordering::AcqRel) {
        std::thread::Builder::new()
            .name(String::from("vsync"))
            .spawn(|| {
                run_real_vsync();
                let mut next = host_monotonic_ns();
                loop {
                    next += acnh_vi_display_period_ns();
                    let wait = (next as i64) - (host_monotonic_ns() as i64);
                    if wait > 0 {
                        host_sleep_ns(wait);
                    }
                    let ev = VI_VSYNC_EVENT.load(Ordering::Relaxed);
                    host_event_signal(ev, true);
                    host_notify_all();
                }
            })
            .ok();
    }
    ev
}

// ---- 7. Networking & Friends (hle_net.cpp, hle_friends.cpp) ---------------------------------------------------------

static NET_LAST_AVAIL: AtomicI32 = AtomicI32::new(-1);

fn net_up() -> bool {
    with_host(|h| {
        h.nextendo_online_active
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            .map(|f| unsafe { f() })
            .unwrap_or(false)
    })
    .unwrap_or(false)
}

#[no_mangle]
pub extern "C" fn acnh_net_is_network_available() -> u64 {
    let up = net_up();
    let up_i = i32::from(up);
    if NET_LAST_AVAIL.swap(up_i, Ordering::Relaxed) != up_i {
        if let Some(Some(f)) = with_host(|h| h.time_refresh_clocks) {
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            unsafe { f() };
        }
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        let nextendo = with_host(|h| h.nextendo_is_nextendo.map(|f| unsafe { f() }))
            .flatten()
            .unwrap_or(false);
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        let signed = with_host(|h| h.nextendo_signed_in.map(|f| unsafe { f() }))
            .flatten()
            .unwrap_or(false);
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        let ver_ok = with_host(|h| h.nextendo_version_ok.map(|f| unsafe { f() }))
            .flatten()
            .unwrap_or(false);
        host_log(&format!(
            "nifm: network {} (Nextendo mode {}, signed in {}, version ok {})",
            if up { "available" } else { "unavailable" },
            i32::from(nextendo),
            i32::from(signed),
            i32::from(ver_ok)
        ));
    }
    u64::from(up)
}

#[no_mangle]
pub extern "C" fn acnh_net_is_any_internet_request_accepted() -> u64 {
    u64::from(net_up())
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_net_get_current_proxy_setting(out_ptr: u64) -> u32 {
    if !net_up() {
        return NIFM_ERR_NO_NETWORK;
    }
    if out_ptr != 0 {
        std::ptr::write_bytes(out_ptr as *mut u8, 0, 0xAA);
    }
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_net_handle_request_error() -> u32 {
    if net_up() {
        SVC_OK
    } else {
        NIFM_ERR_NO_NETWORK
    }
}

pub fn expand_nsd_fqdn(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        if out.len() >= 0xF0 {
            break;
        }
        if ch == '%' {
            out.push_str("lp1");
        } else {
            out.push(ch);
        }
    }
    out
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_net_resolve_ex(out_ptr: u64, in_ptr: u64) -> u32 {
    if !net_up() || in_ptr == 0 || out_ptr == 0 {
        return NET_ERR_GENERIC_FAIL;
    }
    let input = CStr::from_ptr(in_ptr as *const c_char).to_string_lossy();
    let fqdn = expand_nsd_fqdn(&input);
    let dst = std::slice::from_raw_parts_mut(out_ptr as *mut u8, 0x100);
    let bytes = fqdn.as_bytes();
    let n = bytes.len().min(0xFF);
    dst[..n].copy_from_slice(&bytes[..n]);
    dst[n] = 0;
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_net_get_current_primary_ip(out_ptr: u64) -> u32 {
    let ip = if net_up() {
        with_host(|h| h.hostnet_local_ip.map(|f| f()))
            .flatten()
            .unwrap_or(0)
    } else {
        0
    };
    if ip == 0 {
        return NIFM_ERR_NO_NETWORK;
    }
    if out_ptr != 0 {
        std::ptr::write_unaligned(out_ptr as *mut u32, ip);
    }
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_net_get_current_ip_config_info(
    out0: u64,
    out1: u64,
    out2: u64,
    out3: u64,
    out4: u64,
) -> u32 {
    let ip = if net_up() {
        with_host(|h| h.hostnet_local_ip.map(|f| f()))
            .flatten()
            .unwrap_or(0)
    } else {
        0
    };
    if ip == 0 {
        return NIFM_ERR_NO_NETWORK;
    }
    let mask = 0xFFFF_FF00u32.to_be();
    let gw = (ip & mask) | 1u32.to_be();
    let outs = [out0, out1, out2, out3, out4];
    let vals = [ip, mask, gw, gw, gw];
    for (o, v) in outs.into_iter().zip(vals) {
        if o != 0 {
            std::ptr::write_unaligned(o as *mut u32, v);
        }
    }
    SVC_OK
}

const ACNH_TITLE_ID: u64 = 0x0100_6F80_0232_6000;

fn friend_announced_id(f: &AcnhFriendInfo, own_nsa: u64) -> u64 {
    if f.nsa != 0 && own_nsa != 0 {
        f.nsa
    } else {
        f.pid
    }
}

fn friend_same_game(f: &AcnhFriendInfo) -> bool {
    if f.status == 0 {
        return false;
    }
    // SAFETY: the buffer is NUL-terminated: it is a fixed-size array we zero-initialised and the host callback filled, or a C string the caller passed.
    let app = unsafe { CStr::from_ptr(f.app_id.as_ptr()) }.to_string_lossy();
    u64::from_str_radix(app.trim().trim_start_matches("0x"), 16).unwrap_or(0) == ACNH_TITLE_ID
}

unsafe fn write_friend_record(out: u64, uid: u64, f: &AcnhFriendInfo, own_nsa: u64) {
    std::ptr::write_bytes(out as *mut u8, 0, FRIENDS_RECORD_SIZE);
    if uid != 0 {
        std::ptr::copy_nonoverlapping(uid as *const u8, out as *mut u8, 16);
        std::ptr::copy_nonoverlapping(uid as *const u8, (out + 0x40) as *mut u8, 16);
    }
    std::ptr::write_unaligned((out + 0x10) as *mut u64, friend_announced_id(f, own_nsa));
    let name = CStr::from_ptr(f.name.as_ptr()).to_bytes();
    let n = name.len().min(0x20);
    std::ptr::copy_nonoverlapping(name.as_ptr(), (out + 0x18) as *mut u8, n);
    let status = match f.status {
        2 => 2u32,
        1 => 1u32,
        _ => 0u32,
    };
    std::ptr::write_unaligned((out + 0x58) as *mut u32, status);
    std::ptr::write_unaligned((out + 0x5C) as *mut u8, u8::from(friend_same_game(f)));
    std::ptr::write_unaligned((out + 0x120) as *mut u8, u8::from(f.favorite != 0));
    std::ptr::write_unaligned((out + 0x128) as *mut u8, 1);
}

unsafe fn friend_passes_filter(filter: u64, f: &AcnhFriendInfo) -> bool {
    if filter == 0 {
        return true;
    }
    let presence = std::ptr::read_unaligned(filter as *const u32);
    if presence == 1 && f.status != 1 {
        return false;
    }
    if presence == 2 && f.status != 2 {
        return false;
    }
    if presence == 3 && f.status == 0 {
        return false;
    }
    if std::ptr::read_unaligned((filter + 4) as *const u8) != 0 && f.favorite == 0 {
        return false;
    }
    if std::ptr::read_unaligned((filter + 5) as *const u8) != 0 && !friend_same_game(f) {
        return false;
    }
    true
}

fn collect_friends() -> (u64, Vec<AcnhFriendInfo>) {
    // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
    let own_nsa = with_host(|h| h.nextendo_account_nsa.map(|f| unsafe { f() }))
        .flatten()
        .unwrap_or(0);
    // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
    let count = with_host(|h| h.nextendo_friends_count.map(|f| unsafe { f() }))
        .flatten()
        .unwrap_or(0);
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut info = AcnhFriendInfo {
            nsa: 0,
            pid: 0,
            status: 0,
            favorite: 0,
            _pad: [0; 3],
            app_id: [0; 32],
            name: [0; 33],
            _pad2: [0; 7],
        };
        let ok = with_host(|h| {
            h.nextendo_friends_get
                // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
                .map(|f| unsafe { f(i, &mut info) })
                .unwrap_or(false)
        })
        .unwrap_or(false);
        if ok {
            out.push(info);
        }
    }
    (own_nsa, out)
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_hle_friends_get_list(
    out_count: u64,
    out: u64,
    uid: u64,
    offset: i32,
    max: i32,
    filter: u64,
) -> u32 {
    if !net_up() {
        if out_count != 0 {
            std::ptr::write_unaligned(out_count as *mut i32, 0);
        }
        return SVC_OK;
    }
    let off = offset.max(0) as usize;
    let mx = max.max(0) as usize;
    let (own_nsa, all) = collect_friends();
    let filtered: Vec<&AcnhFriendInfo> = all
        .iter()
        .filter(|f| friend_passes_filter(filter, f))
        .collect();
    let mut written = 0usize;
    for f in filtered.into_iter().skip(off).take(mx) {
        if out != 0 {
            write_friend_record(
                out + (written * FRIENDS_RECORD_SIZE) as u64,
                uid,
                f,
                own_nsa,
            );
        }
        written += 1;
    }
    if out_count != 0 {
        std::ptr::write_unaligned(out_count as *mut i32, written as i32);
    }
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_hle_friends_update_info(
    out: u64,
    uid: u64,
    ids: u64,
    count: i32,
) -> u32 {
    if out == 0 || ids == 0 || count <= 0 {
        return SVC_OK;
    }
    let (own_nsa, list) = if net_up() {
        collect_friends()
    } else {
        (0, Vec::new())
    };
    for i in 0..(count as usize) {
        let id = std::ptr::read_unaligned((ids + (i as u64) * 8) as *const u64);
        let dst = out + (i * FRIENDS_RECORD_SIZE) as u64;
        std::ptr::write_bytes(dst as *mut u8, 0, FRIENDS_RECORD_SIZE);
        for f in &list {
            if friend_announced_id(f, own_nsa) == id {
                write_friend_record(dst, uid, f, own_nsa);
            }
        }
    }
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_hle_friends_ensure_available() -> u32 {
    if !net_up() {
        return FRIENDS_ERR_NOT_LINKED;
    }
    if let Some(Some(f)) = with_host(|h| h.nextendo_friends_refresh) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f() };
    }
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_hle_friends_online_ok() -> u32 {
    if net_up() {
        SVC_OK
    } else {
        FRIENDS_ERR_NOT_LINKED
    }
}

#[no_mangle]
pub extern "C" fn acnh_hle_friends_declare_playing(playing: bool) -> u32 {
    if let Some(Some(f)) = with_host(|h| h.nextendo_presence_playing) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f(playing) };
    }
    SVC_OK
}

// ---- 8. NFP / NFC / HID NFC (hle_nfp.cpp, svc_nfp.cpp, hle_hid_system_nfc.cpp) --------------------------------------

static NFP_HLE_INIT: AtomicBool = AtomicBool::new(false);
static NFP_IPC_INIT: AtomicBool = AtomicBool::new(false);
static NFC_IPC_INIT: AtomicBool = AtomicBool::new(false);

fn nfp_has_tag() -> bool {
    // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
    with_host(|h| h.nfp_has_tag.map(|f| unsafe { f() }).unwrap_or(false)).unwrap_or(false)
}

fn nfp_device_state() -> u32 {
    // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
    with_host(|h| h.nfp_device_state.map(|f| unsafe { f() }).unwrap_or(5)).unwrap_or(5)
}

fn ensure_nfp_hle_init() {
    if !NFP_HLE_INIT.swap(true, Ordering::AcqRel) {
        if let Some(Some(f)) = with_host(|h| h.nfp_on_initialize) {
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            unsafe { f() };
        }
    }
}

fn ensure_nfp_ipc_init() {
    if !NFP_IPC_INIT.swap(true, Ordering::AcqRel) {
        NFC_IPC_INIT.store(true, Ordering::Release);
        if let Some(Some(f)) = with_host(|h| h.nfp_on_initialize) {
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            unsafe { f() };
        }
    }
}

fn nfp_handle_ok(h: u64) -> bool {
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

// ---- 9. LDN (hle_ldn.cpp) -------------------------------------------------------------------------------------------

#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LdnState {
    None = 0,
    Initialized = 1,
    AccessPoint = 2,
    AccessPointCreated = 3,
    Station = 4,
    StationConnected = 5,
    Error = 6,
}

impl LdnState {
    fn name(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Initialized => "Initialized",
            Self::AccessPoint => "AccessPoint",
            Self::AccessPointCreated => "AccessPointCreated",
            Self::Station => "Station",
            Self::StationConnected => "StationConnected",
            Self::Error => "Error",
        }
    }
}

const NI_SESSION_ID: usize = 0x10;
const NI_BSSID: usize = 0x20;
const NI_SSID: usize = 0x26;
const NI_CHANNEL: usize = 0x48;
const NI_LINK_LEVEL: usize = 0x4A;
const NI_NETWORK_TYPE: usize = 0x4B;
const NI_SEC_PARAM: usize = 0x50;
const NI_SEC_MODE: usize = 0x60;
const NI_ACCEPT_POLICY: usize = 0x62;
const NI_NODE_MAX: usize = 0x66;
const NI_NODE_COUNT: usize = 0x67;
const NI_NODES: usize = 0x68;
const NI_ADV_SIZE: usize = 0x26A;
const NI_ADV: usize = 0x26C;
const NI_AUTH_ID: usize = 0x478;

const NODE_IP: usize = 0;
const NODE_MAC: usize = 4;
const NODE_ID: usize = 0xA;
const NODE_CONNECTED: usize = 0xB;
const NODE_NAME: usize = 0xC;
const NODE_VERSION: usize = 0x2E;

struct LdnInner {
    state: LdnState,
    state_event: u32,
    advertise: [u8; LDN_ADVERTISE_MAX],
    advertise_size: usize,
    info: [u8; LDN_INFO_SIZE],
    mac: [u8; 6],
    accept_policy: u8,
    disconnect_reason: u32,
    rng_state: u64,
}

impl LdnInner {
    const fn new() -> Self {
        Self {
            state: LdnState::None,
            state_event: 0,
            advertise: [0; LDN_ADVERTISE_MAX],
            advertise_size: 0,
            info: [0; LDN_INFO_SIZE],
            mac: [0; 6],
            accept_policy: 0,
            disconnect_reason: 0,
            rng_state: 0x9E37_79B9_7F4A_7C15,
        }
    }

    fn random_bytes(&mut self, out: &mut [u8]) {
        if self.rng_state == 0x9E37_79B9_7F4A_7C15 {
            self.rng_state ^= host_monotonic_ns().wrapping_add(1);
        }
        for b in out.iter_mut() {
            let mut x = self.rng_state;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.rng_state = x;
            *b = (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8;
        }
    }

    fn ensure_mac(&mut self) {
        if self.mac[0] == 0 && self.mac[1] == 0 {
            let mut m = [0u8; 6];
            self.random_bytes(&mut m);
            m[0] = (m[0] | 2) & 0xFE;
            self.mac = m;
        }
    }

    fn signal_change(&self) {
        if self.state_event != 0 {
            host_event_signal(self.state_event, true);
            host_notify_all();
        }
    }

    fn set_state(&mut self, next: LdnState) {
        if self.state == next {
            return;
        }
        host_log(&format!(
            "ldn: state {} -> {}",
            self.state.name(),
            next.name()
        ));
        self.state = next;
        self.signal_change();
    }
}

static LDN_SESSION: AtomicBool = AtomicBool::new(false);
static LDN_OWN_IP: AtomicU32 = AtomicU32::new(0);
static LDN_LOCK: Mutex<LdnInner> = Mutex::new(LdnInner::new());

fn ldn_wanted() -> bool {
    with_host(|h| {
        h.nextendo_ldn_wanted
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            .map(|f| unsafe { f() })
            .unwrap_or(false)
    })
    .unwrap_or(false)
}

pub fn ldn_info_is_valid(info: &mut [u8]) -> bool {
    if info.len() < LDN_INFO_SIZE {
        return false;
    }
    let adv = u16::from_le_bytes([info[NI_ADV_SIZE], info[NI_ADV_SIZE + 1]]) as usize;
    let max = info[NI_NODE_MAX];
    let count = info[NI_NODE_COUNT];
    if info[NI_SSID] > 32
        || !(1..=8).contains(&max)
        || count < 1
        || count > max
        || adv > LDN_ADVERTISE_MAX
    {
        return false;
    }
    info[NI_SSID + 0x21] = 0;
    for i in 0..8usize {
        let node = &mut info[NI_NODES + i * 0x40..NI_NODES + (i + 1) * 0x40];
        if node[NODE_CONNECTED] == 0 {
            node.fill(0);
            continue;
        }
        node[NODE_CONNECTED] = 1;
        node[NODE_ID] = i as u8;
        node[NODE_NAME + 0x20] = 0;
    }
    true
}

fn ldn_visited_island_string() -> String {
    let g = LDN_LOCK.lock().unwrap();
    if g.state != LdnState::StationConnected {
        return String::new();
    }
    let mut buf = [0 as c_char; 128];
    let n = with_host(|h| {
        h.ldn_island_name
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            .map(|f| unsafe { f(g.info.as_ptr(), buf.as_mut_ptr(), buf.len()) })
            .unwrap_or(0)
    })
    .unwrap_or(0);
    if n == 0 {
        String::new()
    } else {
        // SAFETY: the buffer is NUL-terminated: it is a fixed-size array we zero-initialised and the host callback filled, or a C string the caller passed.
        unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_visited_island(out: *mut c_char, cap: usize) -> usize {
    if out.is_null() || cap == 0 {
        return 0;
    }
    let s = ldn_visited_island_string();
    let bytes = s.as_bytes();
    let n = bytes.len().min(cap - 1);
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.cast::<u8>(), n);
    *out.add(n) = 0;
    n
}

#[no_mangle]
pub extern "C" fn acnh_ldn_enabled() -> bool {
    LDN_SESSION.load(Ordering::Acquire)
}

#[no_mangle]
pub extern "C" fn acnh_ldn_net_up() -> bool {
    LDN_OWN_IP.load(Ordering::Acquire) != 0
}

#[no_mangle]
pub extern "C" fn acnh_ldn_own_ip() -> u32 {
    LDN_OWN_IP.load(Ordering::Acquire)
}

#[no_mangle]
pub extern "C" fn acnh_ldn_broadcast_ip() -> u32 {
    LDN_OWN_IP.load(Ordering::Acquire) | 0xFF
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_info_valid(info_ptr: *mut u8) -> bool {
    if info_ptr.is_null() {
        return false;
    }
    ldn_info_is_valid(std::slice::from_raw_parts_mut(info_ptr, LDN_INFO_SIZE))
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_link_info(out_ptr: *mut u8) -> bool {
    if out_ptr.is_null() {
        return false;
    }
    let g = LDN_LOCK.lock().unwrap();
    if g.state != LdnState::AccessPointCreated {
        return false;
    }
    std::ptr::copy_nonoverlapping(g.info.as_ptr(), out_ptr, LDN_INFO_SIZE);
    true
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_link_add_node(station_ptr: *const u8, own_ip: u32) -> i32 {
    if station_ptr.is_null() {
        return -1;
    }
    let mut g = LDN_LOCK.lock().unwrap();
    if g.state != LdnState::AccessPointCreated {
        return -1;
    }
    if own_ip != 0 && g.info[NI_NODE_COUNT] == 1 {
        g.info[NI_NODES + NODE_IP..NI_NODES + NODE_IP + 4].copy_from_slice(&own_ip.to_le_bytes());
        LDN_OWN_IP.store(own_ip, Ordering::Release);
    }
    let max = (g.info[NI_NODE_MAX] as usize).min(8);
    let station = std::slice::from_raw_parts(station_ptr, 0x40);
    for i in 1..max {
        let base = NI_NODES + i * 0x40;
        if g.info[base + NODE_CONNECTED] != 0 {
            continue;
        }
        g.info[base..base + 0x40].copy_from_slice(station);
        g.info[base + NODE_ID] = i as u8;
        g.info[base + NODE_CONNECTED] = 1;
        g.info[base + NODE_NAME + 0x20] = 0;
        g.info[NI_NODE_COUNT] += 1;
        g.signal_change();
        return i as i32;
    }
    -1
}

#[no_mangle]
pub extern "C" fn acnh_ldn_link_remove_node(n: i32) {
    if !(0..8).contains(&n) {
        return;
    }
    let mut g = LDN_LOCK.lock().unwrap();
    let base = NI_NODES + (n as usize) * 0x40;
    if g.info[base + NODE_CONNECTED] == 0 {
        return;
    }
    g.info[base..base + 0x40].fill(0);
    g.info[NI_NODE_COUNT] = g.info[NI_NODE_COUNT].saturating_sub(1);
    g.signal_change();
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_peer_ips(out_ptr: *mut u32) -> i32 {
    if out_ptr.is_null() {
        return 0;
    }
    let g = LDN_LOCK.lock().unwrap();
    if g.state != LdnState::AccessPointCreated && g.state != LdnState::StationConnected {
        return 0;
    }
    let own = LDN_OWN_IP.load(Ordering::Acquire);
    let mut n = 0i32;
    for i in 0..8usize {
        let base = NI_NODES + i * 0x40;
        let ip = u32::from_le_bytes([
            g.info[base + NODE_IP],
            g.info[base + NODE_IP + 1],
            g.info[base + NODE_IP + 2],
            g.info[base + NODE_IP + 3],
        ]);
        if g.info[base + NODE_CONNECTED] != 0 && ip != 0 && ip != own {
            *out_ptr.add(n as usize) = ip;
            n += 1;
        }
    }
    n
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_link_set_info(info_ptr: *const u8) {
    if info_ptr.is_null() {
        return;
    }
    let mut g = LDN_LOCK.lock().unwrap();
    if g.state != LdnState::StationConnected {
        return;
    }
    std::ptr::copy_nonoverlapping(info_ptr, g.info.as_mut_ptr(), LDN_INFO_SIZE);
    g.signal_change();
}

#[no_mangle]
pub extern "C" fn acnh_ldn_link_lost() {
    let mut g = LDN_LOCK.lock().unwrap();
    if g.state != LdnState::StationConnected {
        return;
    }
    LDN_OWN_IP.store(0, Ordering::Release);
    g.disconnect_reason = 6;
    g.set_state(LdnState::Error);
}

#[no_mangle]
pub extern "C" fn acnh_ldn_disabled() -> u32 {
    if ldn_wanted() {
        SVC_OK
    } else {
        LDN_ERR_DEVICE_DISABLED
    }
}

#[no_mangle]
pub extern "C" fn acnh_ldn_initialize() -> u32 {
    if !ldn_wanted() {
        if let Some(Some(f)) = with_host(|h| h.ui_open_online) {
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            unsafe { f() };
        }
        return LDN_ERR_DEVICE_DISABLED;
    }
    LDN_SESSION.store(true, Ordering::Release);
    if let Some(Some(f)) = with_host(|h| h.wifi_multicast) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f(true) };
    }
    let mut g = LDN_LOCK.lock().unwrap();
    if g.state == LdnState::None {
        g.set_state(LdnState::Initialized);
    }
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_ldn_finalize() -> u32 {
    if LDN_SESSION.load(Ordering::Acquire) {
        if let Some(Some(f)) = with_host(|h| h.ldn_link_stop) {
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            unsafe { f() };
        }
        if let Some(Some(f)) = with_host(|h| h.vsock_reset) {
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            unsafe { f() };
        }
    }
    if let Some(Some(f)) = with_host(|h| h.wifi_multicast) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f(false) };
    }
    let mut g = LDN_LOCK.lock().unwrap();
    LDN_SESSION.store(false, Ordering::Release);
    LDN_OWN_IP.store(0, Ordering::Release);
    g.state = LdnState::None;
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_ldn_get_state() -> u32 {
    let g = LDN_LOCK.lock().unwrap();
    if LDN_SESSION.load(Ordering::Acquire) {
        g.state as u32
    } else {
        LdnState::None as u32
    }
}

#[no_mangle]
pub extern "C" fn acnh_ldn_set_state_event(ev: u32) {
    let mut g = LDN_LOCK.lock().unwrap();
    g.state_event = ev;
}

#[no_mangle]
pub extern "C" fn acnh_ldn_open_access_point() -> u32 {
    let mut g = LDN_LOCK.lock().unwrap();
    if g.state != LdnState::Initialized {
        return LDN_ERR_INVALID_STATE;
    }
    g.set_state(LdnState::AccessPoint);
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_ldn_close_access_point() -> u32 {
    if let Some(Some(f)) = with_host(|h| h.ldn_link_stop) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f() };
    }
    let mut g = LDN_LOCK.lock().unwrap();
    LDN_OWN_IP.store(0, Ordering::Release);
    g.set_state(LdnState::Initialized);
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_ldn_open_station() -> u32 {
    let mut g = LDN_LOCK.lock().unwrap();
    if g.state != LdnState::Initialized {
        return LDN_ERR_INVALID_STATE;
    }
    g.set_state(LdnState::Station);
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_ldn_close_station() -> u32 {
    if let Some(Some(f)) = with_host(|h| h.ldn_link_stop) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f() };
    }
    let mut g = LDN_LOCK.lock().unwrap();
    LDN_OWN_IP.store(0, Ordering::Release);
    g.set_state(LdnState::Initialized);
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_create_network(
    cfg_ptr: *const u8,
    sec_ptr: *const u8,
    sec_param_ptr: *const u8,
    user_ptr: *const u8,
) -> u32 {
    let mut g = LDN_LOCK.lock().unwrap();
    if g.state != LdnState::AccessPoint
        || cfg_ptr.is_null()
        || sec_ptr.is_null()
        || user_ptr.is_null()
    {
        return LDN_ERR_INVALID_STATE;
    }
    let cfg = std::slice::from_raw_parts(cfg_ptr, 0x20);
    let sec = std::slice::from_raw_parts(sec_ptr, 0x44);
    let user = std::slice::from_raw_parts(user_ptr, 0x30);
    g.ensure_mac();
    g.info.fill(0);
    g.info[0..0x10].copy_from_slice(&cfg[0..0x10]);
    if !sec_param_ptr.is_null() {
        let sp = std::slice::from_raw_parts(sec_param_ptr, 0x20);
        g.info[NI_SEC_PARAM..NI_SEC_PARAM + 0x10].copy_from_slice(&sp[0..0x10]);
        g.info[NI_SESSION_ID..NI_SESSION_ID + 0x10].copy_from_slice(&sp[0x10..0x20]);
    } else {
        let mut r1 = [0u8; 16];
        let mut r2 = [0u8; 16];
        g.random_bytes(&mut r1);
        g.random_bytes(&mut r2);
        g.info[NI_SEC_PARAM..NI_SEC_PARAM + 0x10].copy_from_slice(&r1);
        g.info[NI_SESSION_ID..NI_SESSION_ID + 0x10].copy_from_slice(&r2);
    }
    let mac = g.mac;
    g.info[NI_BSSID..NI_BSSID + 6].copy_from_slice(&mac);
    let mut r = [0u8; 8];
    g.random_bytes(&mut r);
    let ssid = format!("ACNHRecomp{:02x}{:02x}{:02x}{:02x}", r[0], r[1], r[2], r[3]);
    let sb = ssid.as_bytes();
    g.info[NI_SSID] = sb.len() as u8;
    g.info[NI_SSID + 1..NI_SSID + 1 + sb.len()].copy_from_slice(sb);
    let ch = u16::from_le_bytes([cfg[0x10], cfg[0x11]]);
    let ch_val = if ch != 0 { ch } else { 6 };
    g.info[NI_CHANNEL..NI_CHANNEL + 2].copy_from_slice(&ch_val.to_le_bytes());
    g.info[NI_LINK_LEVEL] = 3;
    g.info[NI_NETWORK_TYPE] = 2;
    g.info[NI_SEC_MODE..NI_SEC_MODE + 2].copy_from_slice(&sec[0..2]);
    g.info[NI_ACCEPT_POLICY] = g.accept_policy;
    let max = cfg[0x12];
    g.info[NI_NODE_MAX] = if (1..=8).contains(&max) { max } else { 8 };
    g.info[NI_NODE_COUNT] = 1;
    let ip = with_host(|h| h.ldn_local_address.map(|f| f()))
        .flatten()
        .unwrap_or(0);
    LDN_OWN_IP.store(ip, Ordering::Release);
    g.info[NI_NODES + NODE_IP..NI_NODES + NODE_IP + 4].copy_from_slice(&ip.to_le_bytes());
    g.info[NI_NODES + NODE_MAC..NI_NODES + NODE_MAC + 6].copy_from_slice(&mac);
    g.info[NI_NODES + NODE_ID] = 0;
    g.info[NI_NODES + NODE_CONNECTED] = 1;
    g.info[NI_NODES + NODE_NAME..NI_NODES + NODE_NAME + 0x20].copy_from_slice(&user[0..0x20]);
    g.info[NI_NODES + NODE_VERSION..NI_NODES + NODE_VERSION + 2].copy_from_slice(&cfg[0x14..0x16]);
    let adv_sz = g.advertise_size;
    g.info[NI_ADV_SIZE..NI_ADV_SIZE + 2].copy_from_slice(&(adv_sz as u16).to_le_bytes());
    let adv = g.advertise;
    g.info[NI_ADV..NI_ADV + adv_sz].copy_from_slice(&adv[..adv_sz]);
    g.info[NI_AUTH_ID..NI_AUTH_ID + 8].copy_from_slice(&r);
    if let Some(Some(f)) = with_host(|h| h.ldn_link_host_start) {
        f();
    }
    g.set_state(LdnState::AccessPointCreated);
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_ldn_destroy_network() -> u32 {
    if let Some(Some(f)) = with_host(|h| h.ldn_link_stop) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f() };
    }
    let mut g = LDN_LOCK.lock().unwrap();
    LDN_OWN_IP.store(0, Ordering::Release);
    if g.state == LdnState::AccessPointCreated {
        g.set_state(LdnState::AccessPoint);
    }
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_set_advertise_data(data_ptr: *const u8, size: u64) -> u32 {
    let mut g = LDN_LOCK.lock().unwrap();
    let n = size as usize;
    if n > LDN_ADVERTISE_MAX {
        return LDN_ERR_INVALID_STATE;
    }
    if n > 0 && !data_ptr.is_null() {
        let src = std::slice::from_raw_parts(data_ptr, n);
        g.advertise[..n].copy_from_slice(src);
    }
    g.advertise_size = n;
    if g.state == LdnState::AccessPointCreated {
        g.info[NI_ADV_SIZE..NI_ADV_SIZE + 2].copy_from_slice(&(n as u16).to_le_bytes());
        g.info[NI_ADV..NI_ADV + LDN_ADVERTISE_MAX].fill(0);
        let adv = g.advertise;
        g.info[NI_ADV..NI_ADV + n].copy_from_slice(&adv[..n]);
    }
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_ldn_set_station_accept_policy(policy: u32) -> u32 {
    let mut g = LDN_LOCK.lock().unwrap();
    g.accept_policy = policy as u8;
    g.info[NI_ACCEPT_POLICY] = g.accept_policy;
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_connect(
    net_ptr: *const u8,
    user_ptr: *const u8,
    version: u16,
) -> u32 {
    let mut g = LDN_LOCK.lock().unwrap();
    if g.state != LdnState::Station || net_ptr.is_null() || user_ptr.is_null() {
        return LDN_ERR_INVALID_STATE;
    }
    g.ensure_mac();
    let mut me = [0u8; 0x40];
    me[NODE_MAC..NODE_MAC + 6].copy_from_slice(&g.mac);
    me[NODE_CONNECTED] = 1;
    let user = std::slice::from_raw_parts(user_ptr, 0x20);
    me[NODE_NAME..NODE_NAME + 0x20].copy_from_slice(user);
    me[NODE_VERSION..NODE_VERSION + 2].copy_from_slice(&version.to_le_bytes());
    let mut info = [0u8; LDN_INFO_SIZE];
    let node = with_host(|h| {
        h.ldn_link_join
            .map(|f| f(net_ptr.add(NI_BSSID), me.as_ptr(), info.as_mut_ptr()))
            .unwrap_or(-1)
    })
    .unwrap_or(-1);
    if node < 0 {
        return LDN_ERR_INVALID_STATE;
    }
    g.info = info;
    let base = NI_NODES + (node as usize) * 0x40 + NODE_IP;
    let ip = u32::from_le_bytes([info[base], info[base + 1], info[base + 2], info[base + 3]]);
    LDN_OWN_IP.store(ip, Ordering::Release);
    g.disconnect_reason = 0;
    g.set_state(LdnState::StationConnected);
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_ldn_disconnect() -> u32 {
    if let Some(Some(f)) = with_host(|h| h.ldn_link_stop) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f() };
    }
    let mut g = LDN_LOCK.lock().unwrap();
    LDN_OWN_IP.store(0, Ordering::Release);
    if g.state == LdnState::StationConnected {
        g.set_state(LdnState::Station);
    }
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_get_network_info(out_ptr: *mut u8) -> u32 {
    let g = LDN_LOCK.lock().unwrap();
    let up = g.state == LdnState::AccessPointCreated || g.state == LdnState::StationConnected;
    if !out_ptr.is_null() {
        if up {
            std::ptr::copy_nonoverlapping(g.info.as_ptr(), out_ptr, LDN_INFO_SIZE);
        } else {
            std::ptr::write_bytes(out_ptr, 0, LDN_INFO_SIZE);
        }
    }
    if up {
        SVC_OK
    } else {
        LDN_ERR_INVALID_STATE
    }
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_get_network_config(out_ptr: *mut u8) -> u32 {
    let g = LDN_LOCK.lock().unwrap();
    let up = g.state == LdnState::AccessPointCreated || g.state == LdnState::StationConnected;
    if !out_ptr.is_null() {
        std::ptr::write_bytes(out_ptr, 0, 0x20);
        if up {
            let out = std::slice::from_raw_parts_mut(out_ptr, 0x20);
            out[0..0x10].copy_from_slice(&g.info[0..0x10]);
            out[0x10..0x12].copy_from_slice(&g.info[NI_CHANNEL..NI_CHANNEL + 2]);
            out[0x12] = g.info[NI_NODE_MAX];
            out[0x14..0x16]
                .copy_from_slice(&g.info[NI_NODES + NODE_VERSION..NI_NODES + NODE_VERSION + 2]);
        }
    }
    if up {
        SVC_OK
    } else {
        LDN_ERR_INVALID_STATE
    }
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_get_security_parameter(out_ptr: *mut u8) -> u32 {
    let g = LDN_LOCK.lock().unwrap();
    let up = g.state == LdnState::AccessPointCreated || g.state == LdnState::StationConnected;
    if !out_ptr.is_null() {
        std::ptr::write_bytes(out_ptr, 0, 0x20);
        if up {
            let out = std::slice::from_raw_parts_mut(out_ptr, 0x20);
            out[0..0x10].copy_from_slice(&g.info[NI_SEC_PARAM..NI_SEC_PARAM + 0x10]);
            out[0x10..0x20].copy_from_slice(&g.info[NI_SESSION_ID..NI_SESSION_ID + 0x10]);
        }
    }
    if up {
        SVC_OK
    } else {
        LDN_ERR_INVALID_STATE
    }
}

#[no_mangle]
pub extern "C" fn acnh_ldn_get_disconnect_reason() -> u32 {
    LDN_LOCK.lock().unwrap().disconnect_reason
}

// ---- 10. Movie Decoder (hle_movie.cpp) ------------------------------------------------------------------------------

const MOVIE_INPUTS: usize = 8;
const MOVIE_VIDEO_OUTPUTS: usize = 4;
const MOVIE_AUDIO_OUTPUTS: usize = 8;

struct MovieFrame {
    data: Vec<u8>,
    pts: i64,
    flags: u32,
}

struct MovieDecoder {
    codec_type: i32,
    width: i32,
    height: i32,
    channels: i32,
    sample_rate: i32,
    started: bool,
    eos_sent: bool,
    in_event: u32,
    out_event: u32,
    err_event: u32,
    free_inputs: VecDeque<i32>,
    input_buffers: BTreeMap<i32, Vec<u8>>,
    free_outputs: VecDeque<i32>,
    active_outputs: BTreeMap<i32, MovieFrame>,
    decoded: VecDeque<MovieFrame>,
    current_frame: MovieFrame,
    play_start_ns: u64,
    first_pts: i64,
    codec_ctx: usize, // *mut c_void cast to usize for Send
}

impl MovieDecoder {
    fn is_audio(&self) -> bool {
        self.codec_type == MOVIE_CODEC_VORBIS
    }
    fn max_outputs(&self) -> usize {
        if self.is_audio() {
            MOVIE_AUDIO_OUTPUTS
        } else {
            MOVIE_VIDEO_OUTPUTS
        }
    }
}

static MOVIE_DECODERS: Mutex<BTreeMap<u64, MovieDecoder>> = Mutex::new(BTreeMap::new());

#[no_mangle]
pub extern "C" fn acnh_movie_create(self_addr: u64, codec_type: i32) {
    let dec = MovieDecoder {
        codec_type,
        width: 1920,
        height: 1080,
        channels: 2,
        sample_rate: 48000,
        started: false,
        eos_sent: false,
        in_event: 0,
        out_event: 0,
        err_event: 0,
        free_inputs: VecDeque::new(),
        input_buffers: BTreeMap::new(),
        free_outputs: VecDeque::new(),
        active_outputs: BTreeMap::new(),
        decoded: VecDeque::new(),
        current_frame: MovieFrame {
            data: Vec::new(),
            pts: 0,
            flags: 0,
        },
        play_start_ns: 0,
        first_pts: 0,
        codec_ctx: 0,
    };
    MOVIE_DECODERS.lock().unwrap().insert(self_addr, dec);
}

#[no_mangle]
pub extern "C" fn acnh_movie_destroy(self_addr: u64) {
    if let Some(d) = MOVIE_DECODERS.lock().unwrap().remove(&self_addr) {
        if d.codec_ctx != 0 {
            if let Some(Some(f)) = with_host(|h| h.movie_codec_close) {
                // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
                unsafe { f(d.codec_ctx as *mut c_void, d.codec_type) };
            }
        }
    }
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_movie_configure(
    self_addr: u64,
    width: i32,
    height: i32,
    channels: i32,
    sample_rate: i32,
    extra: *const u8,
    extra_size: usize,
) -> u32 {
    let codec_type = {
        let g = MOVIE_DECODERS.lock().unwrap();
        let Some(d) = g.get(&self_addr) else {
            return MOVIE_ERR_INVALID;
        };
        d.codec_type
    };
    let ctx = with_host(|h| {
        h.movie_codec_open
            .map(|f| f(codec_type, channels.max(2), extra, extra_size))
    })
    .flatten()
    .unwrap_or(std::ptr::null_mut());

    let mut g = MOVIE_DECODERS.lock().unwrap();
    let Some(d) = g.get_mut(&self_addr) else {
        return MOVIE_ERR_INVALID;
    };
    if width > 0 {
        d.width = width;
    }
    if height > 0 {
        d.height = height;
    }
    if channels > 0 {
        d.channels = channels;
    }
    if sample_rate > 0 {
        d.sample_rate = sample_rate;
    }
    d.codec_ctx = ctx as usize;
    MOVIE_OK
}

#[no_mangle]
pub extern "C" fn acnh_movie_register_events(
    self_addr: u64,
    in_ev: u32,
    out_ev: u32,
    err_ev: u32,
) -> u32 {
    let mut g = MOVIE_DECODERS.lock().unwrap();
    let Some(d) = g.get_mut(&self_addr) else {
        return MOVIE_ERR_INVALID;
    };
    d.in_event = in_ev;
    d.out_event = out_ev;
    d.err_event = err_ev;
    MOVIE_OK
}

#[no_mangle]
pub extern "C" fn acnh_movie_start(self_addr: u64) -> u32 {
    let in_ev = {
        let mut g = MOVIE_DECODERS.lock().unwrap();
        let Some(d) = g.get_mut(&self_addr) else {
            return MOVIE_ERR_INVALID;
        };
        d.started = true;
        d.eos_sent = false;
        d.free_inputs = (0..MOVIE_INPUTS as i32).collect();
        let max_out = d.max_outputs() as i32;
        d.free_outputs = (0..max_out).collect();
        d.in_event
    };
    host_event_signal(in_ev, true);
    host_notify_all();
    MOVIE_OK
}

#[no_mangle]
pub extern "C" fn acnh_movie_stop(self_addr: u64) -> u32 {
    let mut g = MOVIE_DECODERS.lock().unwrap();
    let Some(d) = g.get_mut(&self_addr) else {
        return MOVIE_ERR_INVALID;
    };
    d.started = false;
    MOVIE_OK
}

#[no_mangle]
pub extern "C" fn acnh_movie_flush(self_addr: u64) -> u32 {
    let in_ev = {
        let mut g = MOVIE_DECODERS.lock().unwrap();
        let Some(d) = g.get_mut(&self_addr) else {
            return MOVIE_ERR_INVALID;
        };
        d.decoded.clear();
        d.active_outputs.clear();
        d.free_inputs = (0..MOVIE_INPUTS as i32).collect();
        let max_out = d.max_outputs() as i32;
        d.free_outputs = (0..max_out).collect();
        d.eos_sent = false;
        d.play_start_ns = 0;
        if d.started {
            d.in_event
        } else {
            0
        }
    };
    if in_ev != 0 {
        host_event_signal(in_ev, true);
        host_notify_all();
    }
    MOVIE_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_movie_acquire_input(
    self_addr: u64,
    out_idx: *mut i32,
    out_rem: *mut i32,
) -> u32 {
    let mut g = MOVIE_DECODERS.lock().unwrap();
    let Some(d) = g.get_mut(&self_addr) else {
        return MOVIE_ERR_INVALID;
    };
    let Some(idx) = d.free_inputs.pop_front() else {
        return MOVIE_ERR_NO_BUFFER;
    };
    d.input_buffers.entry(idx).or_default().clear();
    if !out_idx.is_null() {
        *out_idx = idx;
    }
    if !out_rem.is_null() {
        *out_rem = d.free_inputs.len() as i32;
    }
    MOVIE_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_movie_send_input(
    self_addr: u64,
    idx: i32,
    data: *const u8,
    size: usize,
    pts: i64,
    flags: u32,
) -> u32 {
    let (codec_type, codec_ctx, is_audio) = {
        let g = MOVIE_DECODERS.lock().unwrap();
        let Some(d) = g.get(&self_addr) else {
            return MOVIE_ERR_INVALID;
        };
        (d.codec_type, d.codec_ctx, d.is_audio())
    };
    let mut out_bytes = Vec::new();
    let mut out_w = 0i32;
    let mut out_h = 0i32;
    let mut out_ch = 0i32;
    let mut out_sr = 0i32;

    if (flags & MOVIE_FLAG_EOS) == 0 && size > 0 && !data.is_null() && codec_ctx != 0 {
        if is_audio {
            out_bytes.resize(0x8000, 0);
            let mut written = out_bytes.len();
            let ok = with_host(|h| {
                h.movie_codec_decode_audio
                    .map(|f| {
                        f(
                            codec_ctx as *mut c_void,
                            data,
                            size,
                            out_bytes.as_mut_ptr(),
                            &mut written,
                            &mut out_ch,
                            &mut out_sr,
                        )
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false);
            if ok {
                out_bytes.truncate(written);
            } else {
                out_bytes.clear();
            }
        } else {
            out_bytes.resize(1920 * 1080 * 3 / 2, 0);
            let mut written = out_bytes.len();
            let ok = with_host(|h| {
                h.movie_codec_decode_video
                    .map(|f| {
                        f(
                            codec_ctx as *mut c_void,
                            codec_type,
                            data,
                            size,
                            out_bytes.as_mut_ptr(),
                            &mut written,
                            &mut out_w,
                            &mut out_h,
                        )
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false);
            if ok {
                out_bytes.truncate(written);
            } else {
                out_bytes.clear();
            }
        }
    }

    let (in_ev, out_ev) = {
        let mut g = MOVIE_DECODERS.lock().unwrap();
        let Some(d) = g.get_mut(&self_addr) else {
            return MOVIE_ERR_INVALID;
        };
        if out_w > 0 {
            d.width = out_w;
        }
        if out_h > 0 {
            d.height = out_h;
        }
        if out_ch > 0 {
            d.channels = out_ch;
        }
        if out_sr > 0 {
            d.sample_rate = out_sr;
        }
        if (flags & MOVIE_FLAG_EOS) != 0 {
            d.decoded.push_back(MovieFrame {
                data: Vec::new(),
                pts,
                flags: MOVIE_FLAG_EOS,
            });
        } else if !out_bytes.is_empty() {
            d.decoded.push_back(MovieFrame {
                data: out_bytes,
                pts,
                flags: 0,
            });
        }
        d.input_buffers.remove(&idx);
        d.free_inputs.push_back(idx);
        let sig_in = if d.started { d.in_event } else { 0 };
        let sig_out = if d.started && !d.decoded.is_empty() && !d.free_outputs.is_empty() {
            d.out_event
        } else {
            0
        };
        (sig_in, sig_out)
    };
    if in_ev != 0 {
        host_event_signal(in_ev, true);
    }
    if out_ev != 0 {
        host_event_signal(out_ev, true);
    }
    if in_ev != 0 || out_ev != 0 {
        host_notify_all();
    }
    MOVIE_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_movie_acquire_output(
    self_addr: u64,
    out_idx: *mut i32,
    out_pts: *mut i64,
    out_flags: *mut u32,
    out_rem: *mut i32,
) -> u32 {
    let mut g = MOVIE_DECODERS.lock().unwrap();
    let Some(d) = g.get_mut(&self_addr) else {
        return MOVIE_ERR_INVALID;
    };
    if d.eos_sent || d.free_outputs.is_empty() {
        return MOVIE_ERR_NO_OUTPUT_BUFFER;
    }
    if d.decoded.is_empty() && (d.is_audio() || d.current_frame.data.is_empty()) {
        return MOVIE_ERR_NO_OUTPUT_BUFFER;
    }
    let idx = d.free_outputs.pop_front().unwrap();
    let (pts, flags) =
        if !d.decoded.is_empty() && (d.decoded.front().unwrap().flags & MOVIE_FLAG_EOS) != 0 {
            if !d.is_audio() || d.active_outputs.is_empty() {
                let f = d.decoded.pop_front().unwrap();
                d.eos_sent = true;
                d.active_outputs.insert(
                    idx,
                    MovieFrame {
                        data: Vec::new(),
                        pts: f.pts,
                        flags: MOVIE_FLAG_EOS,
                    },
                );
                (f.pts, MOVIE_FLAG_EOS)
            } else {
                d.free_outputs.push_front(idx);
                return MOVIE_ERR_NO_OUTPUT_BUFFER;
            }
        } else if d.is_audio() {
            let f = d.decoded.pop_front().unwrap();
            let p = f.pts;
            d.active_outputs.insert(idx, f);
            (p, 0)
        } else {
            let p = d
                .decoded
                .front()
                .map(|f| f.pts)
                .unwrap_or(d.current_frame.pts);
            d.active_outputs.insert(
                idx,
                MovieFrame {
                    data: Vec::new(),
                    pts: p,
                    flags: 0,
                },
            );
            (p, 0)
        };
    if !out_idx.is_null() {
        *out_idx = idx;
    }
    if !out_pts.is_null() {
        *out_pts = pts;
    }
    if !out_flags.is_null() {
        *out_flags = flags;
    }
    if !out_rem.is_null() {
        *out_rem = d.free_outputs.len() as i32;
    }
    MOVIE_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_movie_copy_output(
    self_addr: u64,
    idx: i32,
    dst: u64,
    cap: usize,
    out_written: *mut usize,
) -> u32 {
    let mut g = MOVIE_DECODERS.lock().unwrap();
    let Some(d) = g.get_mut(&self_addr) else {
        return MOVIE_ERR_INVALID;
    };
    if !d.active_outputs.contains_key(&idx) {
        return MOVIE_ERR_INVALID;
    }
    let is_audio = d.is_audio();
    let n = if is_audio {
        let f = d.active_outputs.get(&idx).unwrap();
        let n = if cap > 0 {
            f.data.len().min(cap)
        } else {
            f.data.len()
        };
        if n > 0 && dst != 0 {
            std::ptr::copy_nonoverlapping(f.data.as_ptr(), dst as *mut u8, n);
        }
        n
    } else {
        let now = host_monotonic_ns();
        if d.play_start_ns == 0 {
            d.play_start_ns = now;
            if let Some(front) = d.decoded.front() {
                if (front.flags & MOVIE_FLAG_EOS) == 0 {
                    let f = d.decoded.pop_front().unwrap();
                    d.first_pts = f.pts;
                    d.current_frame = f;
                } else {
                    d.first_pts = d.current_frame.pts;
                }
            } else {
                d.first_pts = d.current_frame.pts;
            }
        } else {
            let elapsed_us = d.first_pts + ((now - d.play_start_ns) / 1000) as i64;
            while let Some(front) = d.decoded.front() {
                if (front.flags & MOVIE_FLAG_EOS) != 0 || front.pts > elapsed_us {
                    break;
                }
                d.current_frame = d.decoded.pop_front().unwrap();
            }
        }
        let f = &d.current_frame;
        let n = if cap > 0 {
            f.data.len().min(cap)
        } else {
            f.data.len()
        };
        if n > 0 && dst != 0 {
            std::ptr::copy_nonoverlapping(f.data.as_ptr(), dst as *mut u8, n);
        }
        n
    };
    if !out_written.is_null() {
        *out_written = n;
    }
    if n > 0 && dst != 0 && !is_audio {
        if let Some(Some(f)) = with_host(|h| h.nvn_textures_written) {
            f(dst, n);
        }
    }
    MOVIE_OK
}

#[no_mangle]
pub extern "C" fn acnh_movie_release_output(self_addr: u64, idx: i32) -> u32 {
    let out_ev = {
        let mut g = MOVIE_DECODERS.lock().unwrap();
        let Some(d) = g.get_mut(&self_addr) else {
            return MOVIE_ERR_INVALID;
        };
        if d.active_outputs.remove(&idx).is_some() {
            d.free_outputs.push_back(idx);
        }
        if !d.started {
            return MOVIE_OK;
        }
        let avail = if d.is_audio() {
            !d.eos_sent && !d.decoded.is_empty() && !d.free_outputs.is_empty()
        } else {
            !d.eos_sent
                && (!d.decoded.is_empty() || !d.current_frame.data.is_empty())
                && !d.free_outputs.is_empty()
        };
        if avail {
            d.out_event
        } else {
            0
        }
    };
    if out_ev != 0 {
        host_event_signal(out_ev, true);
        host_notify_all();
    }
    MOVIE_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_movie_get_output_buffer_size(
    self_addr: u64,
    out_size: *mut i32,
    out_count: *mut i32,
) -> u32 {
    let g = MOVIE_DECODERS.lock().unwrap();
    let Some(d) = g.get(&self_addr) else {
        return MOVIE_ERR_INVALID;
    };
    let w = if d.width > 0 { d.width } else { 1920 };
    let h = if d.height > 0 { d.height } else { 1080 };
    let sz = if d.is_audio() {
        0x2000 * d.channels.max(1)
    } else {
        w * h * 3 / 2
    };
    if !out_size.is_null() {
        *out_size = sz;
    }
    if !out_count.is_null() {
        *out_count = d.max_outputs() as i32;
    }
    MOVIE_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_movie_get_output_format(
    self_addr: u64,
    out_is_audio: *mut bool,
    out_w: *mut i32,
    out_h: *mut i32,
    out_ch: *mut i32,
    out_sr: *mut i32,
) -> u32 {
    let g = MOVIE_DECODERS.lock().unwrap();
    let Some(d) = g.get(&self_addr) else {
        return MOVIE_ERR_INVALID;
    };
    if !out_is_audio.is_null() {
        *out_is_audio = d.is_audio();
    }
    if !out_w.is_null() {
        *out_w = d.width;
    }
    if !out_h.is_null() {
        *out_h = d.height;
    }
    if !out_ch.is_null() {
        *out_ch = d.channels;
    }
    if !out_sr.is_null() {
        *out_sr = d.sample_rate;
    }
    MOVIE_OK
}

// ---- Unit Tests -----------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_get_info_constants_and_entropy() {
        let mut rc = 1u32;
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        unsafe {
            assert_eq!(acnh_svc_get_info(0, 0, 0, &mut rc), 0xF);
            assert_eq!(rc, SVC_OK);
            assert_eq!(acnh_svc_get_info(23, 0, 0, &mut rc), 1);
            assert_eq!(acnh_svc_get_info(24, 0, 0, &mut rc), 512);
            assert_eq!(
                acnh_svc_get_info(11, 0, 0x55, &mut rc),
                0x1234_5678_90AB_CDEF ^ 0x55
            );
            assert_eq!(acnh_svc_get_info(999, 0, 0, &mut rc), 0);
            assert_eq!(rc, SVC_ERR_GENERIC);
        }
    }

    #[test]
    fn thread_core_mask_picks_valid_core() {
        assert_eq!(pick_ideal_core(-3, 0xF), None);
        assert_eq!(pick_ideal_core(-2, 0xF), Some(0));
        assert_eq!(pick_ideal_core(2, 0xF), Some(2));
        assert_eq!(pick_ideal_core(0, 0b1100), Some(2));
    }

    #[test]
    fn set_language_table_and_region() {
        assert_eq!(acnh_svc_set_language_count(), 18);
        assert_eq!(acnh_svc_set_region_code(), 1);
        let mut buf = [0u64; 18];
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        let count = unsafe {
            acnh_svc_set_push_available_languages(
                buf.as_mut_ptr() as u64,
                std::mem::size_of_val(&buf),
            )
        };
        assert_eq!(count, 18);
        assert_eq!(buf[0], pack_lang_code_ascii("ja"));
        assert_eq!(buf[1], pack_lang_code_ascii("en-US"));
    }

    #[test]
    fn am_render_res_and_message_queue() {
        assert_eq!(parse_render_res(None, false), (1920, 1080));
        assert_eq!(parse_render_res(None, true), (1280, 720));
        assert_eq!(parse_render_res(Some("2560x1440"), false), (2560, 1440));
        assert_eq!(parse_render_res(Some("100x100"), false), (1920, 1080));

        let mut msg = 0u32;
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        assert_eq!(unsafe { acnh_svc_am_receive_message(&mut msg) }, SVC_OK);
        assert_eq!(msg, AM_MSG_FOCUS_STATE_CHANGED);
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        assert_eq!(unsafe { acnh_svc_am_receive_message(&mut msg) }, SVC_OK);
        assert_eq!(msg, AM_MSG_OPERATION_MODE_CHANGED);
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        assert_eq!(unsafe { acnh_svc_am_receive_message(&mut msg) }, SVC_OK);
        assert_eq!(msg, AM_MSG_PERFORMANCE_MODE_CHANGED);
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_svc_am_receive_message(&mut msg) },
            AM_ERR_NO_MESSAGE
        );
    }

    #[test]
    fn hid_vibration_mixing_combines_and_clamps() {
        let mut devs = [Vibration::default(); 3];
        devs[VIB_LEFT] = Vibration {
            low: 0.2,
            high: 0.4,
            until_ns: 100,
        };
        devs[VIB_RIGHT] = Vibration {
            low: 0.4,
            high: 0.2,
            until_ns: 100,
        };
        let (low, high) = mix_vibrations_at(&devs, 50);
        assert!((low - 1.0).abs() < 1e-5);
        assert!((high - 1.0).abs() < 1e-5);
        let (low_expired, high_expired) = mix_vibrations_at(&devs, 150);
        assert_eq!(low_expired, 0.0);
        assert_eq!(high_expired, 0.0);
    }

    #[test]
    fn small_services_bcat_ngc_ro_web_album() {
        let mut count = -1i32;
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_hle_bcat_directory_read(&mut count as *mut i32 as u64) },
            BCAT_ERR_NOT_FOUND
        );
        assert_eq!(count, 0);

        let mut results = [99u32; 3];
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_hle_ngc_check_words(results.as_mut_ptr() as u64, 3) },
            SVC_OK
        );
        assert_eq!(results, [0, 0, 0]);

        let mut sz = 0u64;
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_hle_ro_get_buffer_size(&mut sz as *mut u64 as u64) },
            SVC_OK
        );
        assert_eq!(sz, 0x1000);
        assert_eq!(acnh_hle_ro_load_module(), RO_ERR_NOT_LOADED);

        let mut ret = 0u64;
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_hle_web_show_share_page(&mut ret as *mut u64 as u64) },
            SVC_OK
        );
        assert_eq!(ret, 1);

        assert_eq!(
            sanitize_file_name_part("  My/Island:Name? . "),
            "  MyIslandName"
        );
        assert_eq!(sanitize_file_name_part("..."), "Island");
    }

    #[test]
    fn vi_display_period_parsing() {
        assert_eq!(parse_display_period_ns(None), 16_666_667);
        assert_eq!(parse_display_period_ns(Some("120")), 8_333_333);
        assert_eq!(parse_display_period_ns(Some("10")), 16_666_667);
        assert_eq!(parse_display_period_ns(Some("999")), 16_666_667);
    }

    #[test]
    fn nsd_fqdn_expansion() {
        assert_eq!(
            expand_nsd_fqdn("api.hac.%.acbaa.srv.nintendo.net"),
            "api.hac.lp1.acbaa.srv.nintendo.net"
        );
    }

    #[test]
    fn ldn_info_validation() {
        let mut info = [0u8; LDN_INFO_SIZE];
        info[NI_SSID] = 4;
        info[NI_NODE_MAX] = 8;
        info[NI_NODE_COUNT] = 1;
        info[NI_NODES + NODE_CONNECTED] = 1;
        assert!(ldn_info_is_valid(&mut info));
        info[NI_NODE_COUNT] = 0;
        assert!(!ldn_info_is_valid(&mut info));
    }

    #[test]
    fn movie_decoder_lifecycle_and_buffers() {
        let addr = 0x1234_5678u64;
        acnh_movie_create(addr, MOVIE_CODEC_VP9);
        assert_eq!(acnh_movie_start(addr), MOVIE_OK);
        let mut idx = -1i32;
        let mut rem = -1i32;
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_movie_acquire_input(addr, &mut idx, &mut rem) },
            MOVIE_OK
        );
        assert_eq!(idx, 0);
        assert_eq!(rem, 7);
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe { acnh_movie_send_input(addr, idx, std::ptr::null(), 0, 1000, MOVIE_FLAG_EOS) },
            MOVIE_OK
        );
        let mut out_idx = -1i32;
        let mut pts = 0i64;
        let mut flags = 0u32;
        assert_eq!(
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe {
                acnh_movie_acquire_output(addr, &mut out_idx, &mut pts, &mut flags, &mut rem)
            },
            MOVIE_OK
        );
        assert_eq!(flags, MOVIE_FLAG_EOS);
        assert_eq!(acnh_movie_release_output(addr, out_idx), MOVIE_OK);
        assert_eq!(acnh_movie_stop(addr), MOVIE_OK);
        acnh_movie_destroy(addr);
    }
}
