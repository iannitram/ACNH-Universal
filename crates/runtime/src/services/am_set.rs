//! Split from the old `services.rs` (issue #356).

use super::*;
#[allow(unused_imports)]
use crate::sync::LockExt;

// ---- 2. Set / Misc / AM Services (svc_set.cpp, svc_misc.cpp, svc_am.cpp) --------------------------------------------

pub(crate) fn pack_lang_code_ascii(tag: &str) -> u64 {
    let mut buf = [0u8; 8];
    let bytes = tag.as_bytes();
    let n = bytes.len().min(8);
    buf[..n].copy_from_slice(&bytes[..n]);
    u64::from_le_bytes(buf)
}

pub(crate) const LANG_TAGS: [&str; 18] = [
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

pub(crate) struct AmState {
    pub(crate) messages: VecDeque<u32>,
    pub(crate) message_event: u32,
    pub(crate) popped_launch_param: bool,
    pub(crate) display_res: Option<(u32, u32)>,
}

pub(crate) static AM_STATE: Mutex<Option<AmState>> = Mutex::new(None);

pub(crate) fn with_am<R>(f: impl FnOnce(&mut AmState) -> R) -> R {
    let mut g = AM_STATE.lock_or_recover();
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

pub(crate) const VIB_DURATION_MS: u32 = 60;
pub(crate) const VIB_STRENGTH: f32 = 3.0;
pub(crate) const VIB_LEFT: usize = 0;
pub(crate) const VIB_RIGHT: usize = 1;
pub(crate) const VIB_COMBINED: usize = 2;

#[derive(Clone, Copy, Default)]
pub(crate) struct Vibration {
    pub(crate) low: f32,
    pub(crate) high: f32,
    pub(crate) until_ns: u64,
}

pub(crate) static VIB_DEVICES: Mutex<[Vibration; 3]> = Mutex::new(
    [Vibration {
        low: 0.0,
        high: 0.0,
        until_ns: 0,
    }; 3],
);

pub(crate) fn clean_amp(v: f32) -> f32 {
    if v.is_finite() {
        v.max(0.0)
    } else {
        0.0
    }
}

pub(crate) fn mix_vibrations_at(devices: &[Vibration; 3], now_ns: u64) -> (f32, f32) {
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
    let mut devs = VIB_DEVICES.lock_or_recover();
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

pub(crate) static ALBUM_STAMP_STATE: Mutex<(String, i32)> = Mutex::new((String::new(), 0));

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
        let mut st = ALBUM_STAMP_STATE.lock_or_recover();
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
