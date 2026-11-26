//! Host OS services (HLE + IPC services): the Services group (issue #57).
//! Replaces `svc_am.cpp`, `svc_set.cpp`, `svc_misc.cpp`, `svc_process.cpp`, `svc_hid.cpp`,
//! `svc_nfp.cpp`, `hle_ro.cpp`, `hle_vi.cpp`, `hle_hid_system_nfc.cpp`, `hle_net.cpp`,
//! `hle_web.cpp`, `hle_pl.cpp`, `hle_album.cpp`, `hle_jpeg.cpp`, `hle_nfp.cpp`, `hle_bcat.cpp`,
//! `hle_movie.cpp`, `hle_friends.cpp`, `hle_ngc.cpp`, and `hle_ldn.cpp`.

#[allow(unused_imports)]
use crate::sync::LockExt;
use std::collections::{BTreeMap, VecDeque};
#[allow(unused_imports)]
use std::ffi::{c_char, c_void, CStr, CString};
#[allow(unused_imports)]
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
#[allow(unused_imports)]
use std::sync::{Condvar, Mutex, OnceLock};
#[allow(unused_imports)]
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

pub(crate) static HOST: Mutex<Option<AcnhServicesHost>> = Mutex::new(None);

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_services_set_host(host: *const AcnhServicesHost) {
    if host.is_null() {
        return;
    }
    let mut g = HOST.lock_or_recover();
    *g = Some(std::ptr::read(host));
}

pub(crate) fn with_host<R>(f: impl FnOnce(&AcnhServicesHost) -> R) -> Option<R> {
    let g = HOST.lock_or_recover();
    g.as_ref().map(f)
}

pub(crate) fn host_log(msg: &str) {
    if let Some(Some(f)) = with_host(|h| h.log) {
        if let Ok(cstr) = CString::new(msg) {
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            unsafe { f(cstr.as_ptr()) };
        }
    }
}

pub(crate) fn host_is_handheld() -> bool {
    with_host(|h| {
        h.config_is_handheld
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            .map(|f| unsafe { f() })
            .unwrap_or(false)
    })
    .unwrap_or(false)
}

pub(crate) fn host_config_bool(key: &str, default: bool) -> bool {
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

pub(crate) fn host_monotonic_ns() -> u64 {
    // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
    with_host(|h| h.monotonic_ns.map(|f| unsafe { f() }))
        .flatten()
        .unwrap_or_else(|| {
            static START: OnceLock<Instant> = OnceLock::new();
            START.get_or_init(Instant::now).elapsed().as_nanos() as u64
        })
}

pub(crate) fn host_sleep_ns(ns: i64) {
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

pub(crate) fn host_kernel_alloc(size: u64) -> u64 {
    // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
    with_host(|h| h.kernel_alloc.map(|f| unsafe { f(size) }))
        .flatten()
        .unwrap_or(0)
}

pub(crate) fn host_event_create(signaled: bool) -> u32 {
    // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
    with_host(|h| h.event_create.map(|f| unsafe { f(signaled) }))
        .flatten()
        .unwrap_or(0)
}

pub(crate) fn host_event_signal(ev: u32, signaled: bool) {
    if ev == 0 {
        return;
    }
    if let Some(Some(f)) = with_host(|h| h.event_signal) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f(ev, signaled) };
    }
}

pub(crate) fn host_notify_all() {
    if let Some(Some(f)) = with_host(|h| h.notify_all) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f() };
    }
}

mod process;
#[allow(unused_imports)]
pub use process::*;
mod am_set;
#[allow(unused_imports)]
pub use am_set::*;
mod fonts_vi;
#[allow(unused_imports)]
pub use fonts_vi::*;
mod net;
#[allow(unused_imports)]
pub use net::*;
mod nfp;
#[allow(unused_imports)]
pub use nfp::*;
mod ldn;
#[allow(unused_imports)]
pub use ldn::*;
mod movie;
#[allow(unused_imports)]
pub use movie::*;

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
