//! Host platform services, thermal governor, performance hints (ADPF / KGSL), audio gain,
//! input/controller helpers, and window/path state, ported from `runtime/src/platform_*.cpp`,
//! `runtime/src/thermal.cpp`, and `runtime/src/perf_hint.cpp`.
//!
//! `runtime/src/platform_wrapper.cpp` and the thin per-OS wrappers (`platform_linux_wrapper.cpp`,
//! `platform_mac_wrapper.cpp`, `platform_win_wrapper.cpp`, `platform_sdl_wrapper.cpp`,
//! `platform_android_wrapper.cpp`, `platform_android_input_wrapper.cpp`,
//! `platform_android_audio_wrapper.cpp`) provide the OS/SDL3/JNI/NDK host callbacks and forward
//! `nx::plat::*` entry points here.

use std::collections::HashSet;
use std::ffi::{c_char, c_void};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::config::{config_generation, config_get, config_get_bool, config_get_int};
use crate::ffi::{cstr_or_empty, host_log};
use crate::input::{
    AcnhOverlayShape, K_A, K_B, K_DOWN, K_L, K_LEFT, K_MINUS, K_PLUS, K_R, K_RIGHT, K_STICK_L,
    K_STICK_MAX, K_STICK_R, K_UP, K_X, K_Y, K_ZL, K_ZR,
};

mod perf_hint;
mod thermal;

use thermal::*;

// ---- Host callbacks ----------------------------------------------------------------------------

/// Host callbacks provided by `runtime/src/platform_wrapper.cpp` and the OS wrappers.
///
/// When unset (e.g. in standalone Rust unit tests), `monotonic_ns` uses a process-local
/// `Instant`, `thermal_headroom` returns `-1.0`, and ADPF/KGSL hooks are no-ops.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct AcnhPlatformHost {
    pub log: Option<extern "C" fn(msg: *const c_char)>,
    pub monotonic_ns: Option<extern "C" fn() -> u64>,
    pub thermal_headroom: Option<extern "C" fn() -> f32>,
    pub adpf_create_session:
        Option<extern "C" fn(tids: *const i32, count: usize, target_ns: i64) -> *mut c_void>,
    pub adpf_close_session: Option<extern "C" fn(session: *mut c_void)>,
    pub adpf_update_target: Option<extern "C" fn(session: *mut c_void, target_ns: i64)>,
    pub adpf_report_actual: Option<extern "C" fn(session: *mut c_void, actual_ns: i64)>,
    pub adpf_set_efficiency: Option<extern "C" fn(session: *mut c_void, prefer_efficiency: bool)>,
    pub adpf_preferred_rate_ns: Option<extern "C" fn() -> i64>,
    pub kgsl_set_pwrctrl: Option<extern "C" fn(enable_normal: bool) -> i32>,
}

// SAFETY: `AcnhPlatformHost` only holds plain function pointers.
unsafe impl Send for AcnhPlatformHost {}
// SAFETY: `AcnhPlatformHost` only holds plain function pointers.
unsafe impl Sync for AcnhPlatformHost {}

static HOST: OnceLock<Mutex<AcnhPlatformHost>> = OnceLock::new();
static MONO_START: OnceLock<Instant> = OnceLock::new();

fn host() -> AcnhPlatformHost {
    *HOST
        .get_or_init(|| Mutex::new(AcnhPlatformHost::default()))
        .lock()
        .unwrap()
}

fn host_monotonic_ns() -> u64 {
    if let Some(f) = host().monotonic_ns {
        return f();
    }
    let start = MONO_START.get_or_init(Instant::now);
    start.elapsed().as_nanos() as u64
}

fn copy_str_to_buf(s: &str, out: *mut c_char, cap: usize) -> usize {
    let bytes = s.as_bytes();
    if !out.is_null() && cap > 0 {
        let n = bytes.len().min(cap - 1);
        // SAFETY: `out` is valid for `cap` bytes per caller contract.
        unsafe {
            if n > 0 {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.cast::<u8>(), n);
            }
            *out.add(n) = 0;
        }
    }
    bytes.len()
}

/// Register host platform callbacks from C++.
///
/// # Safety
/// `h` must be null or point to a valid `AcnhPlatformHost`.
#[no_mangle]
pub unsafe extern "C" fn acnh_platform_set_host(h: *const AcnhPlatformHost) {
    if h.is_null() {
        return;
    }
    // SAFETY: `h` is a valid pointer to `AcnhPlatformHost`.
    let val = unsafe { *h };
    crate::ffi::set_log(val.log);
    *HOST
        .get_or_init(|| Mutex::new(AcnhPlatformHost::default()))
        .lock()
        .unwrap() = val;
}

// ---- Environment, Paths, and Window State ------------------------------------------------------

/// Returns `true` when `NX_HEADLESS` is set in the environment.
#[no_mangle]
pub extern "C" fn acnh_platform_headless() -> bool {
    std::env::var_os("NX_HEADLESS").is_some()
}

/// Returns `true` when live test files in the data folder should be polled.
#[no_mangle]
pub extern "C" fn acnh_platform_live_files(is_android: bool) -> bool {
    if let Ok(v) = std::env::var("NX_LIVE") {
        return v.starts_with('1');
    }
    if is_android {
        std::env::var_os("NX_LIVE_FRAME").is_some()
    } else {
        true
    }
}

/// Returns `true` when `NX_DEBUG_FILES=1`.
#[no_mangle]
pub extern "C" fn acnh_platform_debug_files() -> bool {
    std::env::var("NX_DEBUG_FILES")
        .map(|v| v.starts_with('1'))
        .unwrap_or(false)
}

/// Look up `NX_<UPPER_KEY>` in the environment, writing to `out` and returning full length.
#[no_mangle]
pub extern "C" fn acnh_platform_setting(key: *const c_char, out: *mut c_char, cap: usize) -> usize {
    let key = cstr_or_empty(key);
    let mut env_key = String::with_capacity(3 + key.len());
    env_key.push_str("NX_");
    for ch in key.chars() {
        env_key.push(ch.to_ascii_uppercase());
    }
    let val = std::env::var(&env_key).unwrap_or_default();
    copy_str_to_buf(&val, out, cap)
}

/// Resolve `data_root`: `NX_DATA_ROOT` if set, else `default_root`.
#[no_mangle]
pub extern "C" fn acnh_platform_data_root(
    default_root: *const c_char,
    out: *mut c_char,
    cap: usize,
) -> usize {
    let val =
        std::env::var("NX_DATA_ROOT").unwrap_or_else(|_| cstr_or_empty(default_root).to_string());
    copy_str_to_buf(&val, out, cap)
}

/// Recursively create directories for `path`.
#[no_mangle]
pub extern "C" fn acnh_platform_make_dirs(path: *const c_char) -> bool {
    let p = cstr_or_empty(path);
    !p.is_empty() && std::fs::create_dir_all(Path::new(p)).is_ok()
}

// Window state shared across wrappers.
static WIN_MINIMIZED: AtomicBool = AtomicBool::new(false);
static WIN_W: AtomicU32 = AtomicU32::new(1280);
static WIN_H: AtomicU32 = AtomicU32::new(720);
static WIN_REFRESH_HZ: AtomicI32 = AtomicI32::new(60);

#[no_mangle]
pub extern "C" fn acnh_platform_window_set_minimized(minimized: bool) {
    WIN_MINIMIZED.store(minimized, Ordering::Relaxed);
}

#[no_mangle]
pub extern "C" fn acnh_platform_window_minimized() -> bool {
    WIN_MINIMIZED.load(Ordering::Relaxed)
}

#[no_mangle]
pub extern "C" fn acnh_platform_window_set_size(w: u32, h: u32) {
    WIN_W.store(w, Ordering::Relaxed);
    WIN_H.store(h, Ordering::Relaxed);
}

/// Read the current window pixel dimensions.
///
/// # Safety
/// `out_w` and `out_h` must be null or valid writable pointers.
#[no_mangle]
pub unsafe extern "C" fn acnh_platform_window_size(out_w: *mut u32, out_h: *mut u32) {
    if !out_w.is_null() {
        // SAFETY: Valid pointer per caller contract.
        unsafe { *out_w = WIN_W.load(Ordering::Relaxed) };
    }
    if !out_h.is_null() {
        // SAFETY: Valid pointer per caller contract.
        unsafe { *out_h = WIN_H.load(Ordering::Relaxed) };
    }
}

#[no_mangle]
pub extern "C" fn acnh_platform_window_set_refresh_hz(hz: i32) {
    if hz > 20 {
        WIN_REFRESH_HZ.store(hz, Ordering::Relaxed);
    }
}

#[no_mangle]
pub extern "C" fn acnh_platform_display_refresh_hz() -> i32 {
    WIN_REFRESH_HZ.load(Ordering::Relaxed)
}

// File/folder picker result queue.
#[derive(Default)]
struct PickState {
    done: bool,
    path: String,
}

static PICK_MU: OnceLock<Mutex<PickState>> = OnceLock::new();

fn pick_mu() -> &'static Mutex<PickState> {
    PICK_MU.get_or_init(|| Mutex::new(PickState::default()))
}

#[no_mangle]
pub extern "C" fn acnh_platform_pick_begin() {
    let mut g = pick_mu().lock().unwrap();
    g.done = false;
}

#[no_mangle]
pub extern "C" fn acnh_platform_pick_complete(path: *const c_char) {
    let mut g = pick_mu().lock().unwrap();
    g.path = cstr_or_empty(path).to_string();
    g.done = true;
}

/// Take the completed picker result if ready.
///
/// # Safety
/// `out` must be null or writable for `cap` bytes; `out_len` must be null or writable.
#[no_mangle]
pub unsafe extern "C" fn acnh_platform_pick_take(
    out: *mut c_char,
    cap: usize,
    out_len: *mut usize,
) -> bool {
    let mut g = pick_mu().lock().unwrap();
    if !g.done {
        return false;
    }
    g.done = false;
    let len = copy_str_to_buf(&g.path, out, cap);
    if !out_len.is_null() {
        // SAFETY: Valid pointer per caller contract.
        unsafe { *out_len = len };
    }
    true
}

// ---- Audio Gain --------------------------------------------------------------------------------

static MASTER_VOLUME_BITS: AtomicU32 = AtomicU32::new(f32::to_bits(-1.0));

#[no_mangle]
pub extern "C" fn acnh_platform_set_master_volume(v: f32) {
    let clamped = v.clamp(0.0, 1.0);
    MASTER_VOLUME_BITS.store(clamped.to_bits(), Ordering::Relaxed);
}

#[no_mangle]
pub extern "C" fn acnh_platform_effective_volume(window_focused: bool) -> f32 {
    let mut v = f32::from_bits(MASTER_VOLUME_BITS.load(Ordering::Relaxed));
    if v < 0.0 {
        v = (config_get_int("audio.volume", 100).clamp(0, 100) as f32) / 100.0;
        MASTER_VOLUME_BITS.store(v.to_bits(), Ordering::Relaxed);
    }
    if !window_focused && config_get_bool("audio.mute_background", false) {
        return 0.0;
    }
    v
}

/// Scale `count` signed 16-bit PCM samples in-place by the effective master volume.
///
/// # Safety
/// `pcm` must be null or point to `count` writable `i16` samples.
#[no_mangle]
pub unsafe extern "C" fn acnh_platform_apply_gain(
    pcm: *mut i16,
    count: usize,
    window_focused: bool,
) {
    if pcm.is_null() || count == 0 {
        return;
    }
    let v = acnh_platform_effective_volume(window_focused);
    if v >= 0.999 {
        return;
    }
    // SAFETY: Caller guarantees `pcm` points to `count` writable `i16` samples.
    let samples = unsafe { std::slice::from_raw_parts_mut(pcm, count) };
    for s in samples {
        *s = ((*s as f32) * v) as i16;
    }
}

// ---- Controller Kind & Input Helpers -----------------------------------------------------------

/// Classify a controller into `"switch"`, `"xbox"`, `"playstation"`, or `"generic"`.
///
/// The same rule as the launcher (`app/src/input.rs`) and `runtime/src/controller_kind.h`; all three
/// are held to `tests/data/pad_kinds.tsv`. Maker (USB vendor id) first, then the name.
pub fn pad_kind_str(vendor: i32, product: i32, name: &str) -> &'static str {
    let n = name.to_ascii_lowercase();
    match (vendor, product) {
        (0x057e, _) => return "switch",
        (0x045e, _) => return "xbox",
        (0x054c, _) => return "playstation",
        (0x17ef, _) if n.contains("legion") => return "xbox",
        // 8BitDo pads in Switch mode ("Nintendo" layout) or X-input mode.
        (0x2dc8, 0x6001 | 0x6002 | 0x6006 | 0x6007 | 0x6101 | 0x6102 | 0x9006 | 0x9012) => {
            return "switch"
        }
        (0x2dc8, 0x2000) => return "xbox",
        (0x2dc8, _) if n.contains("switch") => return "switch",
        _ => {}
    }
    if n.contains("nintendo") || n.contains("pro controller") || n.contains("joy-con") {
        "switch"
    } else if n.contains("xbox") {
        "xbox"
    } else if n.contains("dualsense") || n.contains("dualshock") || n.contains("playstation") {
        "playstation"
    } else {
        "generic"
    }
}

#[no_mangle]
pub extern "C" fn acnh_platform_pad_kind(
    vendor: i32,
    product: i32,
    name: *const c_char,
    out: *mut c_char,
    cap: usize,
) -> usize {
    copy_str_to_buf(pad_kind_str(vendor, product, cstr_or_empty(name)), out, cap)
}

#[no_mangle]
pub extern "C" fn acnh_platform_android_controller_kind(
    connected: bool,
    vendor: i32,
    product: i32,
    name: *const c_char,
    out: *mut c_char,
    cap: usize,
) -> usize {
    if !connected {
        return copy_str_to_buf("", out, cap);
    }
    copy_str_to_buf(pad_kind_str(vendor, product, cstr_or_empty(name)), out, cap)
}

/// Resolve `input.layout` (`"xbox"`, `"console"`, or `"auto"`).
#[no_mangle]
pub extern "C" fn acnh_platform_xbox_layout(controller_kind: *const c_char) -> bool {
    static CACHE: OnceLock<Mutex<(u64, String)>> = OnceLock::new();
    let mu = CACHE.get_or_init(|| Mutex::new((u64::MAX, String::new())));
    let mut g = mu.lock().unwrap();
    let gen = config_generation();
    if g.0 != gen {
        g.0 = gen;
        g.1 = config_get("input.layout", "auto");
    }
    match g.1.as_str() {
        "xbox" => true,
        "console" => false,
        _ => cstr_or_empty(controller_kind) != "switch",
    }
}

/// Scale a normalized `[-1.0, 1.0]` axis with deadzone to `[-K_STICK_MAX, K_STICK_MAX]`.
#[no_mangle]
pub extern "C" fn acnh_platform_scale_stick_axis(val: f32, deadzone: f32) -> i32 {
    if val.abs() < deadzone {
        0
    } else {
        (val.clamp(-1.0, 1.0) * (K_STICK_MAX as f32)) as i32
    }
}

/// Map an Android `AKEYCODE_*` to Switch `NpadButton` bits.
#[no_mangle]
pub extern "C" fn acnh_platform_android_key_to_button(keycode: i32, xbox_layout: bool) -> u64 {
    // Android NDK keycodes (stable ABI values from android/keycodes.h).
    const AKEYCODE_BACK: i32 = 4;
    const AKEYCODE_DPAD_UP: i32 = 19;
    const AKEYCODE_DPAD_DOWN: i32 = 20;
    const AKEYCODE_DPAD_LEFT: i32 = 21;
    const AKEYCODE_DPAD_RIGHT: i32 = 22;
    const AKEYCODE_BUTTON_A: i32 = 96;
    const AKEYCODE_BUTTON_B: i32 = 97;
    const AKEYCODE_BUTTON_X: i32 = 99;
    const AKEYCODE_BUTTON_Y: i32 = 100;
    const AKEYCODE_BUTTON_L1: i32 = 102;
    const AKEYCODE_BUTTON_R1: i32 = 103;
    const AKEYCODE_BUTTON_L2: i32 = 104;
    const AKEYCODE_BUTTON_R2: i32 = 105;
    const AKEYCODE_BUTTON_THUMBL: i32 = 106;
    const AKEYCODE_BUTTON_THUMBR: i32 = 107;
    const AKEYCODE_BUTTON_START: i32 = 108;
    const AKEYCODE_BUTTON_SELECT: i32 = 109;

    match keycode {
        AKEYCODE_BUTTON_A => {
            if xbox_layout {
                K_A
            } else {
                K_B
            }
        }
        AKEYCODE_BUTTON_B => {
            if xbox_layout {
                K_B
            } else {
                K_A
            }
        }
        AKEYCODE_BUTTON_X => K_Y,
        AKEYCODE_BUTTON_Y => K_X,
        AKEYCODE_BUTTON_L1 => K_L,
        AKEYCODE_BUTTON_R1 => K_R,
        AKEYCODE_BUTTON_L2 => K_ZL,
        AKEYCODE_BUTTON_R2 => K_ZR,
        AKEYCODE_BUTTON_START => K_PLUS,
        AKEYCODE_BUTTON_SELECT => K_MINUS,
        AKEYCODE_BUTTON_THUMBL => K_STICK_L,
        AKEYCODE_BUTTON_THUMBR => K_STICK_R,
        AKEYCODE_DPAD_LEFT => K_LEFT,
        AKEYCODE_DPAD_UP => K_UP,
        AKEYCODE_DPAD_RIGHT => K_RIGHT,
        AKEYCODE_DPAD_DOWN => K_DOWN,
        AKEYCODE_BACK => K_MINUS,
        _ => 0,
    }
}

/// Encode up to 2 glyph characters from `"ABXYRLZ+-"` for `present.frag`.
#[no_mangle]
pub extern "C" fn acnh_platform_touch_label_code(label: *const c_char) -> u32 {
    const GLYPHS: &[u8] = b"ABXYRLZ+-";
    let bytes = cstr_or_empty(label).as_bytes();
    let mut code = 0u32;
    for (c, &b) in bytes.iter().take(2).enumerate() {
        let Some(pos) = GLYPHS.iter().position(|&g| g == b) else {
            return 0;
        };
        code |= ((pos as u32) + 1) << (8 * c);
    }
    code
}

/// Evaluate virtual stick auto-run hysteresis (`kRunOn = 1.3`, `kRunOff = 1.1`, `250 ms` delay).
///
/// # Safety
/// `was_run` and `past_ns` must be valid writable pointers when non-null.
#[no_mangle]
pub unsafe extern "C" fn acnh_platform_touch_stick_run(
    dx: f32,
    dy: f32,
    can_run: bool,
    now_ns: u64,
    was_run: *mut bool,
    past_ns: *mut u64,
) -> bool {
    const K_RUN_ON: f32 = 1.3;
    const K_RUN_OFF: f32 = 1.1;
    const K_RUN_DELAY_NS: u64 = 250_000_000;
    if was_run.is_null() || past_ns.is_null() {
        return false;
    }
    // SAFETY: Valid pointers per caller contract.
    let (run_ref, past_ref) = unsafe { (&mut *was_run, &mut *past_ns) };
    let dist = dx.hypot(dy);
    let inside = if *run_ref {
        dist < K_RUN_OFF
    } else {
        dist <= K_RUN_ON
    };
    if !can_run || inside {
        *run_ref = false;
        *past_ref = 0;
    } else if !*run_ref {
        if *past_ref == 0 {
            *past_ref = now_ns;
        }
        *run_ref = now_ns.saturating_sub(*past_ref) >= K_RUN_DELAY_NS;
    }
    *run_ref
}

/// Build an `AcnhOverlayShape` from normalized layout coordinates.
#[no_mangle]
pub extern "C" fn acnh_platform_make_overlay_shape(
    x: f32,
    y: f32,
    r: f32,
    alpha: f32,
    label: *const c_char,
) -> AcnhOverlayShape {
    AcnhOverlayShape {
        x,
        y,
        r,
        alpha: alpha.min(1.0),
        label: acnh_platform_touch_label_code(label),
    }
}

// ---- Unit tests --------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::perf_hint::*;
    use super::*;
    use std::ffi::CString;

    #[test]
    fn pad_kind_classification() {
        assert_eq!(pad_kind_str(0x057e, 0, ""), "switch");
        assert_eq!(pad_kind_str(0, 0, "Pro Controller"), "switch");
        assert_eq!(pad_kind_str(0x045e, 0, ""), "xbox");
        assert_eq!(pad_kind_str(0, 0, "Xbox Wireless Controller"), "xbox");
        assert_eq!(pad_kind_str(0x054c, 0, ""), "playstation");
        assert_eq!(
            pad_kind_str(0, 0, "DualSense Wireless Controller"),
            "playstation"
        );
        assert_eq!(pad_kind_str(0x2dc8, 0x6006, "8BitDo Pro 2"), "switch");
    }

    /// The shared list the launcher and the C++ runtime are held to as well.
    #[test]
    fn pad_kind_matches_the_shared_table() {
        let table = include_str!("../../../../tests/data/pad_kinds.tsv");
        let mut rows = 0;
        for line in table
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
        {
            let f: Vec<&str> = line.split('\t').collect();
            assert_eq!(f.len(), 4, "{line}");
            let hex = |s: &str| i32::from_str_radix(s.trim_start_matches("0x"), 16).unwrap();
            let want = if f[3] == "deck" { "generic" } else { f[3] };
            assert_eq!(pad_kind_str(hex(f[0]), hex(f[1]), f[2]), want, "{line}");
            rows += 1;
        }
        assert!(rows > 20);
    }

    #[test]
    fn android_key_mapping_respects_layout() {
        assert_eq!(acnh_platform_android_key_to_button(96, true), K_A);
        assert_eq!(acnh_platform_android_key_to_button(96, false), K_B);
        assert_eq!(acnh_platform_android_key_to_button(97, true), K_B);
        assert_eq!(acnh_platform_android_key_to_button(97, false), K_A);
        assert_eq!(acnh_platform_android_key_to_button(108, false), K_PLUS);
        assert_eq!(acnh_platform_android_key_to_button(4, false), K_MINUS);
    }

    #[test]
    fn touch_label_and_stick_run_hysteresis() {
        let ab = CString::new("ZR").unwrap();
        let code = acnh_platform_touch_label_code(ab.as_ptr());
        assert_ne!(code, 0);
        let bad = CString::new("?").unwrap();
        assert_eq!(acnh_platform_touch_label_code(bad.as_ptr()), 0);

        let mut was_run = false;
        let mut past_ns = 0u64;
        // SAFETY: Test passes valid pointers to stack variables.
        unsafe {
            // Not past kRunOn (1.3): stays false
            assert!(!acnh_platform_touch_stick_run(
                1.2,
                0.0,
                true,
                100,
                &mut was_run,
                &mut past_ns
            ));
            // Past 1.3 at t=100: starts timer, still false
            assert!(!acnh_platform_touch_stick_run(
                1.4,
                0.0,
                true,
                100,
                &mut was_run,
                &mut past_ns
            ));
            assert_eq!(past_ns, 100);
            // After 250 ms: turns true
            assert!(acnh_platform_touch_stick_run(
                1.4,
                0.0,
                true,
                100 + 250_000_000,
                &mut was_run,
                &mut past_ns
            ));
            // Between kRunOff (1.1) and kRunOn (1.3): stays true
            assert!(acnh_platform_touch_stick_run(
                1.2,
                0.0,
                true,
                100 + 300_000_000,
                &mut was_run,
                &mut past_ns
            ));
            // Below kRunOff (1.1): turns false
            assert!(!acnh_platform_touch_stick_run(
                1.0,
                0.0,
                true,
                100 + 350_000_000,
                &mut was_run,
                &mut past_ns
            ));
        }
    }

    #[test]
    fn thermal_modes_and_governor_discover_decide() {
        assert_eq!(parse_thermal_mode("performance"), 0);
        assert_eq!(parse_thermal_mode("balanced"), 1);
        assert_eq!(parse_thermal_mode("cool"), 2);
        assert_eq!(acnh_platform_thermal_adpf_share(1), 0.85);
        assert_eq!(acnh_platform_thermal_adpf_share(2), 0.95);

        let mut gov = AcnhThermalGovernor::new();
        let discover = "\
sgpu 441
freqs 305000000 441000000 587000000
limit 1800000
policy 0 1804800 1804800 1000000 1804800
policy 4 2419200 1804800 1000000 1400000 1804800 2419200
zone 0 cpu-1-0-usr
id 0
";
        assert!(gov.discover_parse(discover));
        assert_eq!(gov.steps, vec![305, 441, 587]);
        assert_eq!(gov.pol.len(), 1);
        assert_eq!(gov.pol[0].id, 4);

        gov.sample_parse("bp 92\nc 441000000\ng 441\np 4 1804800\nt 70000\nbat 360\n");
        assert_eq!(gov.temp, 70);
        assert_eq!(gov.battery, 360);

        let (restore, write) = gov.build_apply(1);
        assert!(restore.contains("R='"));
        let _ = write;
    }

    #[test]
    fn keeper_cpu_parsing_and_audio_gain() {
        let mut cpus = [0i32; 8];
        let spec = CString::new("4, 7, 4, 9999").unwrap();
        // SAFETY: `cpus` has 8 elements.
        let n = unsafe { acnh_platform_parse_keeper_cpus(spec.as_ptr(), cpus.as_mut_ptr(), 8) };
        assert_eq!(n, 2);
        assert_eq!(&cpus[..2], &[4, 7]);

        acnh_platform_set_master_volume(0.5);
        let mut pcm = [1000i16, -2000i16];
        // SAFETY: `pcm` has 2 elements.
        unsafe { acnh_platform_apply_gain(pcm.as_mut_ptr(), pcm.len(), true) };
        assert_eq!(pcm, [500, -1000]);
    }
}
