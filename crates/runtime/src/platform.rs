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
use std::ffi::{c_char, c_void, CStr};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::config::{config_generation, config_get, config_get_bool, config_get_int};
use crate::input::{
    AcnhOverlayShape, K_A, K_B, K_DOWN, K_L, K_LEFT, K_MINUS, K_PLUS, K_R, K_RIGHT, K_STICK_L,
    K_STICK_MAX, K_STICK_R, K_UP, K_X, K_Y, K_ZL, K_ZR,
};

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

fn host_log(msg: &str) {
    if let Some(f) = host().log {
        if let Ok(c) = std::ffi::CString::new(msg) {
            f(c.as_ptr());
        }
    }
}

fn host_monotonic_ns() -> u64 {
    if let Some(f) = host().monotonic_ns {
        return f();
    }
    let start = MONO_START.get_or_init(Instant::now);
    start.elapsed().as_nanos() as u64
}

fn cstr_or_empty<'a>(p: *const c_char) -> &'a str {
    if p.is_null() {
        return "";
    }
    // SAFETY: Caller passes a valid NUL-terminated C string or null.
    unsafe { CStr::from_ptr(p) }.to_str().unwrap_or("")
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
pub fn pad_kind_str(vendor: i32, name: &str) -> &'static str {
    let n = name.to_ascii_lowercase();
    if vendor == 0x057e
        || n.contains("nintendo")
        || n.contains("pro controller")
        || n.contains("joy-con")
    {
        "switch"
    } else if vendor == 0x045e || n.contains("xbox") {
        "xbox"
    } else if vendor == 0x054c
        || n.contains("dualsense")
        || n.contains("dualshock")
        || n.contains("playstation")
    {
        "playstation"
    } else {
        "generic"
    }
}

#[no_mangle]
pub extern "C" fn acnh_platform_pad_kind(
    vendor: i32,
    name: *const c_char,
    out: *mut c_char,
    cap: usize,
) -> usize {
    copy_str_to_buf(pad_kind_str(vendor, cstr_or_empty(name)), out, cap)
}

#[no_mangle]
pub extern "C" fn acnh_platform_android_controller_kind(
    connected: bool,
    vendor: i32,
    name: *const c_char,
    out: *mut c_char,
    cap: usize,
) -> usize {
    if !connected {
        return copy_str_to_buf("", out, cap);
    }
    copy_str_to_buf(pad_kind_str(vendor, cstr_or_empty(name)), out, cap)
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

// ---- Thermal Governor & Frame Timing (`thermal.cpp`) -------------------------------------------

#[derive(Clone, Copy)]
struct ThermalModeSpec {
    gpu_target: f64,
    cpu_target: f64,
    late_max: f64,
    gpu_floor_mhz: i64,
    cpu_floor_mhz: i64,
    headroom_hot: f32,
    temp_hot: i32,
    max_drop: i32,
    adpf: f64,
    battery_hot: i32,
    battery_ssrm: i32,
}

const MODES: [ThermalModeSpec; 3] = [
    ThermalModeSpec {
        gpu_target: 1.0,
        cpu_target: 1.0,
        late_max: 1.0,
        gpu_floor_mhz: 0,
        cpu_floor_mhz: 0,
        headroom_hot: 99.0,
        temp_hot: 999,
        max_drop: 0,
        adpf: 0.0,
        battery_hot: 0,
        battery_ssrm: 0,
    },
    ThermalModeSpec {
        gpu_target: 0.85,
        cpu_target: 0.80,
        late_max: 0.03,
        gpu_floor_mhz: 441,
        cpu_floor_mhz: 1200,
        headroom_hot: 0.90,
        temp_hot: 81,
        max_drop: 1,
        adpf: 0.85,
        battery_hot: 42,
        battery_ssrm: 41,
    },
    ThermalModeSpec {
        gpu_target: 0.96,
        cpu_target: 0.88,
        late_max: 0.08,
        gpu_floor_mhz: 305,
        cpu_floor_mhz: 1000,
        headroom_hot: 0.75,
        temp_hot: 68,
        max_drop: 2,
        adpf: 0.95,
        battery_hot: 39,
        battery_ssrm: 0,
    },
];

const K_CPU_CAPS: [i64; 10] = [2400, 2100, 1800, 1600, 1500, 1400, 1300, 1200, 1100, 1000];
const K_CPU_START_LEVEL: usize = 3;
const K_BINS: usize = 80;

static THERMAL_PERIOD_NS: AtomicI64 = AtomicI64::new(0);
static THERMAL_REPLAY_NS: AtomicI64 = AtomicI64::new(0);
static THERMAL_MAIN_NS: AtomicI64 = AtomicI64::new(0);
static THERMAL_FRAMES: AtomicU32 = AtomicU32::new(0);
static THERMAL_LATE: AtomicU32 = AtomicU32::new(0);
static THERMAL_DYN_SCALE_BITS: AtomicU32 = AtomicU32::new(f32::to_bits(1.0));
static THERMAL_BINS: [AtomicU32; K_BINS] = [const { AtomicU32::new(0) }; K_BINS];

pub fn parse_thermal_mode(s: &str) -> i32 {
    match s {
        "performance" => 0,
        "cool" => 2,
        _ => 1,
    }
}

#[no_mangle]
pub extern "C" fn acnh_platform_thermal_parse_mode(s: *const c_char) -> i32 {
    parse_thermal_mode(cstr_or_empty(s))
}

#[no_mangle]
pub extern "C" fn acnh_platform_thermal_mode() -> i32 {
    if let Ok(env) = std::env::var("NX_THERMAL") {
        if !env.is_empty() {
            return parse_thermal_mode(&env);
        }
    }
    static CACHE: OnceLock<Mutex<(u64, i32)>> = OnceLock::new();
    let mu = CACHE.get_or_init(|| Mutex::new((u64::MAX, 1)));
    let mut g = mu.lock().unwrap();
    let gen = config_generation();
    if g.0 != gen {
        g.0 = gen;
        g.1 = parse_thermal_mode(&config_get("video.thermal", "balanced"));
    }
    g.1
}

#[no_mangle]
pub extern "C" fn acnh_platform_thermal_adpf_share(mode: i32) -> f64 {
    MODES[mode.clamp(0, 2) as usize].adpf
}

#[no_mangle]
pub extern "C" fn acnh_platform_thermal_headroom() -> f32 {
    host().thermal_headroom.map(|f| f()).unwrap_or(-1.0)
}

#[no_mangle]
pub extern "C" fn acnh_platform_dynamic_render_scale() -> f32 {
    f32::from_bits(THERMAL_DYN_SCALE_BITS.load(Ordering::Relaxed))
}

#[no_mangle]
pub extern "C" fn acnh_platform_frame_period(ns: i64) {
    if ns > 0 {
        THERMAL_PERIOD_NS.store(ns, Ordering::Relaxed);
    }
}

#[no_mangle]
pub extern "C" fn acnh_platform_frame_period_ns() -> i64 {
    THERMAL_PERIOD_NS.load(Ordering::Relaxed)
}

#[no_mangle]
pub extern "C" fn acnh_platform_frame_replay_work(ns: i64) {
    THERMAL_REPLAY_NS.store(ns, Ordering::Relaxed);
}

#[no_mangle]
pub extern "C" fn acnh_platform_frame_replay_work_ns() -> i64 {
    THERMAL_REPLAY_NS.load(Ordering::Relaxed)
}

#[no_mangle]
pub extern "C" fn acnh_platform_thermal_main_work(ns: i64) {
    THERMAL_MAIN_NS.store(ns, Ordering::Relaxed);
}

#[no_mangle]
pub extern "C" fn acnh_platform_thermal_frame(interval_ns: u64) {
    let p = THERMAL_PERIOD_NS.load(Ordering::Relaxed);
    if p <= 0 || interval_ns > 250_000_000 {
        return;
    }
    THERMAL_FRAMES.fetch_add(1, Ordering::Relaxed);
    if (interval_ns as i64) > p + p / 4 {
        THERMAL_LATE.fetch_add(1, Ordering::Relaxed);
    }
    let work = THERMAL_REPLAY_NS
        .load(Ordering::Relaxed)
        .max(THERMAL_MAIN_NS.load(Ordering::Relaxed));
    let bin = ((work * 40 / p) as isize).clamp(0, (K_BINS - 1) as isize) as usize;
    THERMAL_BINS[bin].fetch_add(1, Ordering::Relaxed);
}

#[derive(Default, Clone)]
struct CpuPolicy {
    id: i32,
    path: String,
    restore: i64,
    written: i64,
    read: i64,
    freqs: Vec<i64>,
    hw_max: i64,
    not_kept: i32,
}

/// Stateful thermal governor for Android rooted clock control + dynamic resolution.
pub struct AcnhThermalGovernor {
    gpu_path: String,
    gpu_in_mhz: bool,
    steps: Vec<i64>,
    gpu_usual: i64,
    gpu_sys_seen: bool,
    gpu_restore: i64,
    gpu_written: i64,
    gpu_read: i64,
    ssrm_low: i64,
    gpu_idx: usize,
    pol: Vec<CpuPolicy>,
    use_limit: bool,
    limit_restore: i64,
    limit_written: i64,
    limit_read: i64,
    cpu_level: usize,
    zones: Vec<String>,
    temp: i32,
    busy_sum: f64,
    clk_sum: f64,
    busy_n: i32,
    drop: i32,
    hot_n: i32,
    cool_n: i32,
    gpu_hold: i32,
    cpu_hold: i32,
    gpu_votes: i32,
    cpu_votes: i32,
    turn_gpu: bool,
    reads_cmd: String,
    restore_sent: String,
    busy: f64,
    clk: f64,
    cpu_p90: f64,
    late: f64,
    battery: i32,
    frames: u32,
    late_n: u32,
    decided_ns: u64,
    dyn_starved: u32,
    dyn_roomy: u32,
    dyn_changed_ns: u64,
    headroom: f32,
}

impl AcnhThermalGovernor {
    pub fn new() -> Self {
        Self {
            gpu_path: String::new(),
            gpu_in_mhz: false,
            steps: Vec::new(),
            gpu_usual: 0,
            gpu_sys_seen: false,
            gpu_restore: 0,
            gpu_written: 0,
            gpu_read: 0,
            ssrm_low: 0,
            gpu_idx: 0,
            pol: Vec::new(),
            use_limit: false,
            limit_restore: 0,
            limit_written: 0,
            limit_read: 0,
            cpu_level: K_CPU_START_LEVEL,
            zones: Vec::new(),
            temp: -1,
            busy_sum: 0.0,
            clk_sum: 0.0,
            busy_n: 0,
            drop: 0,
            hot_n: 0,
            cool_n: 0,
            gpu_hold: 0,
            cpu_hold: 0,
            gpu_votes: 0,
            cpu_votes: 0,
            turn_gpu: true,
            reads_cmd: String::new(),
            restore_sent: String::new(),
            busy: -1.0,
            clk: 0.0,
            cpu_p90: 0.0,
            late: 0.0,
            battery: -1,
            frames: 0,
            late_n: 0,
            decided_ns: 0,
            dyn_starved: 0,
            dyn_roomy: 0,
            dyn_changed_ns: 0,
            headroom: -1.0,
        }
    }

    fn top(&self) -> usize {
        self.steps.len().saturating_sub(1)
    }

    fn floor_idx(&self, m: &ThermalModeSpec) -> usize {
        let mut k = 0;
        let t = self.top();
        while k < t && self.steps[k] < m.gpu_floor_mhz {
            k += 1;
        }
        k
    }

    fn max_level(&self, m: &ThermalModeSpec) -> usize {
        if self.pol.is_empty() {
            return 0;
        }
        let mut l = 0;
        while l < K_CPU_CAPS.len() && K_CPU_CAPS[l] >= m.cpu_floor_mhz {
            l += 1;
        }
        l
    }

    fn gpu_value(&self, mhz: i64) -> i64 {
        if self.gpu_in_mhz {
            mhz
        } else {
            mhz * 1_000_000
        }
    }

    fn gpu_mhz(&self, value: i64) -> i64 {
        if self.gpu_in_mhz {
            value
        } else {
            value / 1_000_000
        }
    }

    fn policy_cap(p: &CpuPolicy, level: usize) -> i64 {
        if level == 0 {
            return p.restore;
        }
        let cap = (K_CPU_CAPS[level - 1] * 1000).min(p.restore);
        let best = p
            .freqs
            .iter()
            .copied()
            .filter(|&f| f <= cap)
            .max()
            .unwrap_or(0);
        if best != 0 {
            best
        } else if let Some(&first) = p.freqs.first() {
            first.min(p.restore)
        } else {
            cap
        }
    }

    fn cpu_cap_mhz(&self, level: usize) -> i64 {
        self.pol
            .iter()
            .map(|p| Self::policy_cap(p, level))
            .max()
            .unwrap_or(0)
            / 1000
    }

    fn cpu_step(&self, level: usize, dir: isize, lo: usize, hi: usize) -> usize {
        let mut l = (level as isize) + dir;
        let cur_mhz = self.cpu_cap_mhz(level);
        while l > lo as isize && l < hi as isize && self.cpu_cap_mhz(l as usize) == cur_mhz {
            l += dir;
        }
        l.clamp(lo as isize, hi as isize) as usize
    }

    pub fn discover_parse(&mut self, output: &str) -> bool {
        let mut sgpu = 0i64;
        let mut kgpu = 0i64;
        let mut uid = -1i64;
        let mut zone_types: Vec<(i32, String)> = Vec::new();
        for line in output.lines() {
            let w: Vec<&str> = line
                .split([' ', '\t', '%'])
                .filter(|s| !s.is_empty())
                .collect();
            if w.is_empty() {
                continue;
            }
            let num = |k: usize| -> i64 { w.get(k).and_then(|s| s.parse().ok()).unwrap_or(0) };
            match w[0] {
                "sgpu" => sgpu = num(1),
                "kgpu" => kgpu = num(1),
                "limit" => self.limit_restore = num(1),
                "id" => uid = num(1),
                "freqs" => {
                    for s in &w[1..] {
                        let mhz = s.parse::<i64>().unwrap_or(0) / 1_000_000;
                        if mhz > 0 && !self.steps.contains(&mhz) {
                            self.steps.push(mhz);
                        }
                    }
                }
                "policy" if w.len() >= 4 => {
                    let id = num(1) as i32;
                    let hw_max = num(2);
                    let restore = num(3);
                    let mut freqs: Vec<i64> = w[4..]
                        .iter()
                        .filter_map(|s| s.parse::<i64>().ok())
                        .collect();
                    freqs.sort_unstable();
                    if restore > 0 {
                        self.pol.push(CpuPolicy {
                            id,
                            path: format!(
                                "/sys/devices/system/cpu/cpufreq/policy{id}/scaling_max_freq"
                            ),
                            restore,
                            freqs,
                            hw_max,
                            ..Default::default()
                        });
                    }
                }
                "zone" if w.len() >= 3 => {
                    zone_types.push((num(1) as i32, w[2].to_string()));
                }
                _ => {}
            }
        }
        if uid != 0 {
            host_log(&format!(
                "thermal: the root shell is not root (uid {uid}): clock control off"
            ));
            return false;
        }
        self.steps.sort_unstable();
        if sgpu > 0 {
            self.gpu_path = "/sys/kernel/gpu/gpu_max_clock".to_string();
            self.gpu_in_mhz = true;
            self.gpu_usual = sgpu;
        } else if kgpu > 0 {
            self.gpu_path = "/sys/class/kgsl/kgsl-3d0/max_gpuclk".to_string();
            self.gpu_usual = kgpu / 1_000_000;
        }
        if self.steps.is_empty() {
            self.gpu_path.clear();
        }
        self.gpu_restore = self.gpu_usual;
        self.gpu_idx = self.top();

        let little_max = self.pol.iter().map(|p| p.hw_max).min().unwrap_or(0);
        self.pol.retain(|p| p.hw_max > little_max);

        for pass in 0..2 {
            if !self.zones.is_empty() {
                break;
            }
            let prefix = if pass == 0 { "cpu-1" } else { "cpu" };
            for (n, ty) in &zone_types {
                if self.zones.len() < 8 && ty.starts_with(prefix) {
                    self.zones
                        .push(format!("/sys/class/thermal/thermal_zone{n}/temp"));
                }
            }
        }

        let mut r = String::from(
            "K=/sys/class/kgsl/kgsl-3d0; nxrd b $K/gpubusy; nxrd bp $K/gpu_busy_percentage; nxrd c $K/gpuclk",
        );
        if !self.gpu_path.is_empty() {
            r.push_str(&format!("; nxrd g {}", self.gpu_path));
        }
        for p in &self.pol {
            r.push_str(&format!("; nxrd \"p {}\" {}", p.id, p.path));
        }
        if self.limit_restore > 0 {
            r.push_str("; nxrd l /sys/power/cpufreq_max_limit");
        }
        for z in &self.zones {
            r.push_str(&format!("; nxrd t {z}"));
        }
        r.push_str("; nxrd bat /sys/class/power_supply/battery/temp");
        self.reads_cmd = r;
        true
    }

    pub fn sample_parse(&mut self, output: &str) {
        let mut busy_got = false;
        let mut t = -1i64;
        for line in output.lines() {
            let w: Vec<&str> = line
                .split([' ', '\t', '%'])
                .filter(|s| !s.is_empty())
                .collect();
            if w.is_empty() {
                continue;
            }
            let num = |k: usize| -> i64 { w.get(k).and_then(|s| s.parse().ok()).unwrap_or(0) };
            match w[0] {
                "b" if num(2) > 0 => {
                    self.busy_sum += (num(1) as f64) / (num(2) as f64);
                    self.busy_n += 1;
                    busy_got = true;
                }
                "bp" if w.len() > 1 && !busy_got => {
                    self.busy_sum += (num(1) as f64) / 100.0;
                    self.busy_n += 1;
                }
                "c" => self.clk_sum += (num(1) as f64) / 1e6,
                "g" => self.gpu_read = self.gpu_mhz(num(1)),
                "l" => self.limit_read = num(1),
                "t" if w.len() > 1 => {
                    let v = num(1);
                    let c = if v > 1000 { v / 1000 } else { v };
                    t = t.max(c);
                }
                "bat" if w.len() > 1 => self.battery = num(1) as i32,
                "p" => {
                    let id = num(1) as i32;
                    let val = num(2);
                    for p in &mut self.pol {
                        if p.id == id {
                            p.read = val;
                        }
                    }
                }
                _ => {}
            }
        }
        self.temp = t as i32;
        if self.gpu_written != 0 && self.gpu_read > 0 && self.gpu_read != self.gpu_written {
            if !self.gpu_sys_seen && self.temp > 0 && self.temp < 65 {
                self.gpu_usual = self.gpu_read;
                self.gpu_restore = self.gpu_read;
            }
            self.gpu_sys_seen = true;
            if self.gpu_read >= self.gpu_usual {
                self.gpu_restore = self.gpu_read;
            }
            let low = if self.gpu_read < self.gpu_usual {
                self.gpu_read
            } else {
                0
            };
            if low != self.ssrm_low {
                host_log(&format!(
                    "thermal: the system's GPU cap {} MHz{}",
                    self.gpu_read,
                    if low != 0 { " (its thermal limit)" } else { "" }
                ));
            }
            self.ssrm_low = low;
        }
        for p in &mut self.pol {
            if p.written != 0 && p.read > p.written && p.written == p.restore {
                p.restore = p.read;
            }
            p.not_kept = if p.written != 0 && p.written < p.restore && p.read > p.written {
                p.not_kept + 1
            } else {
                0
            };
            if p.not_kept >= 3 && !self.use_limit && self.limit_restore > 0 {
                self.use_limit = true;
                host_log(&format!(
                    "thermal: policy{}'s scaling_max_freq is not kept ({} over {}): cpufreq_max_limit used",
                    p.id, p.read, p.written
                ));
            }
        }
        if self.limit_written != 0
            && self.limit_read > 0
            && self.limit_read != self.limit_written
            && self.limit_read > self.limit_restore
        {
            self.limit_restore = self.limit_read;
        }
    }

    pub fn decide(&mut self, mode_idx: i32, now_ns: u64, headroom: f32, dyn_res_on: bool) {
        let mode_idx = mode_idx.clamp(0, 2) as usize;
        let m = &MODES[mode_idx];
        self.frames = THERMAL_FRAMES.swap(0, Ordering::Relaxed);
        self.late_n = THERMAL_LATE.swap(0, Ordering::Relaxed);

        let p = THERMAL_PERIOD_NS.load(Ordering::Relaxed);
        if self.decided_ns != 0 && p > 0 {
            let expected = (now_ns.saturating_sub(self.decided_ns) as f64) / (p as f64);
            if self.frames >= 20
                && (self.frames as f64) >= expected * 0.6
                && expected - (self.frames as f64) > (self.late_n as f64)
            {
                self.late_n = (expected - (self.frames as f64)) as u32;
            }
        }
        self.decided_ns = now_ns;

        let mut seen = 0u32;
        let mut p90 = 0usize;
        let mut bins = [0u32; K_BINS];
        for (k, slot) in THERMAL_BINS.iter().enumerate() {
            bins[k] = slot.swap(0, Ordering::Relaxed);
        }
        for (k, &cnt) in bins.iter().enumerate() {
            p90 = k;
            seen += cnt;
            if k == K_BINS - 1 || seen * 10 >= self.frames * 9 {
                break;
            }
        }
        self.cpu_p90 = if self.frames != 0 {
            ((p90 + 1) as f64) / 40.0
        } else {
            0.0
        };
        self.late = if self.frames != 0 {
            (self.late_n as f64) / (self.frames as f64)
        } else {
            0.0
        };
        self.busy = if self.busy_n != 0 {
            self.busy_sum / (self.busy_n as f64)
        } else {
            -1.0
        };
        self.clk = if self.busy_n != 0 {
            self.clk_sum / (self.busy_n as f64)
        } else {
            0.0
        };
        self.busy_sum = 0.0;
        self.clk_sum = 0.0;
        self.busy_n = 0;
        self.headroom = headroom;

        if mode_idx == 0 {
            self.gpu_idx = self.top();
            self.cpu_level = 0;
            self.drop = 0;
            return;
        }

        let have_temp = self.temp > 0;
        let mut hot = if have_temp {
            self.temp >= m.temp_hot
        } else {
            headroom >= m.headroom_hot
        };
        let mut cooled = if have_temp {
            self.temp < m.temp_hot - 4
        } else {
            headroom < m.headroom_hot - 0.1
        };
        if self.battery > 0 && m.battery_hot > 0 {
            hot = hot || self.battery >= m.battery_hot * 10;
            cooled = cooled && self.battery < m.battery_hot * 10 - 10;
        }
        self.hot_n = if hot { self.hot_n + 1 } else { 0 };
        self.cool_n = if cooled { self.cool_n + 1 } else { 0 };
        if self.hot_n >= 5 && self.drop < m.max_drop {
            self.drop += 1;
            self.hot_n = 0;
        }
        if self.cool_n >= 10 && self.drop > 0 {
            self.drop -= 1;
            self.cool_n = 0;
        }
        self.gpu_hold = (self.gpu_hold - 1).max(0);
        self.cpu_hold = (self.cpu_hold - 1).max(0);
        if self.frames < 20 {
            return;
        }

        let gpu = !self.gpu_path.is_empty();
        let lo = if gpu { self.floor_idx(m) } else { 0 };
        let hi = if gpu {
            lo.max(self.top().saturating_sub(self.drop as usize))
        } else {
            0
        };
        let max_l = self.max_level(m);
        let min_l = if self.drop > 0 {
            (K_CPU_START_LEVEL + (self.drop as usize) - 1).min(max_l)
        } else {
            0
        };
        let work = if gpu && self.busy >= 0.0 && self.clk > 0.0 {
            self.busy * self.clk
        } else {
            -1.0
        };
        let share_at = |idx: usize| -> f64 { work / (self.steps[idx] as f64) };

        if self.late > m.late_max && self.late_n >= 2 {
            let gpu_full = work >= 0.0 && share_at(self.gpu_idx.min(hi)) > 0.9;
            let cpu_full = self.cpu_p90 > m.cpu_target;
            let cpu_top = self.cpu_level <= min_l;
            let raise_gpu = gpu && (gpu_full || cpu_top || !cpu_full);
            let raise_cpu = !cpu_top && (cpu_full || (!gpu && self.cpu_p90 > m.cpu_target * 0.75));
            if raise_gpu {
                self.gpu_idx += 1;
                self.gpu_hold = 5;
            }
            if raise_cpu {
                self.cpu_level = self.cpu_step(self.cpu_level, -1, min_l, max_l);
                self.cpu_hold = 5;
            }
            self.gpu_votes = 0;
            let next = self.cpu_step(self.cpu_level, 1, min_l, max_l);
            let cpu_spare = next > self.cpu_level
                && self.cpu_p90 * (self.cpu_cap_mhz(self.cpu_level) as f64)
                    / (self.cpu_cap_mhz(next) as f64)
                    < m.cpu_target;
            self.cpu_votes = if !raise_cpu && gpu_full && cpu_spare && self.cpu_hold == 0 {
                self.cpu_votes + 1
            } else {
                0
            };
            if self.cpu_votes >= 3 {
                self.cpu_level = next;
                self.cpu_votes = 0;
            }
        } else {
            if work >= 0.0 {
                let mut need = lo;
                while need < self.top() && share_at(need) > m.gpu_target {
                    need += 1;
                }
                if need > self.gpu_idx {
                    self.gpu_idx = need;
                    self.gpu_hold = 5;
                    self.gpu_votes = 0;
                } else {
                    self.gpu_votes = if need < self.gpu_idx && self.gpu_hold == 0 {
                        self.gpu_votes + 1
                    } else {
                        0
                    };
                }
            }
            if self.cpu_level > 0 && self.cpu_p90 > m.cpu_target + 0.08 {
                self.cpu_level = self.cpu_step(self.cpu_level, -1, min_l, max_l);
                self.cpu_hold = 5;
                self.cpu_votes = 0;
            } else {
                let next = self.cpu_step(self.cpu_level, 1, min_l, max_l);
                let fits = next > self.cpu_level
                    && self.cpu_p90 * (self.cpu_cap_mhz(self.cpu_level) as f64)
                        / (self.cpu_cap_mhz(next) as f64)
                        < m.cpu_target;
                self.cpu_votes = if fits && self.cpu_hold == 0 {
                    self.cpu_votes + 1
                } else {
                    0
                };
            }
            let gpu_ready = self.gpu_votes >= 3;
            let cpu_ready = self.cpu_votes >= 3;
            if gpu_ready && (self.turn_gpu || !cpu_ready) {
                self.gpu_idx = self.gpu_idx.saturating_sub(1);
                self.gpu_votes = 0;
                self.turn_gpu = false;
            } else if cpu_ready {
                self.cpu_level = self.cpu_step(self.cpu_level, 1, min_l, max_l);
                self.cpu_votes = 0;
                self.turn_gpu = true;
            }
        }
        self.gpu_idx = if gpu {
            self.gpu_idx.clamp(lo, self.top())
        } else {
            0
        };
        self.cpu_level = self.cpu_level.clamp(min_l, max_l);

        // Dynamic resolution
        if !dyn_res_on || m.gpu_target >= 1.0 {
            THERMAL_DYN_SCALE_BITS.store(1.0f32.to_bits(), Ordering::Relaxed);
            return;
        }
        let low = acnh_platform_dynamic_render_scale() < 1.0;
        let step_hi = self.steps.get(self.gpu_idx.min(hi)).copied().unwrap_or(0);
        let ceiling = self.gpu_idx >= hi || (self.ssrm_low != 0 && self.ssrm_low <= step_hi);
        let starved = self.late > 0.04 && self.late_n >= 3 && self.busy >= 0.75 && ceiling;
        self.dyn_starved = if starved { self.dyn_starved + 1 } else { 0 };
        let roomy = self.ssrm_low == 0
            && self.late_n == 0
            && self.busy >= 0.0
            && !self.steps.is_empty()
            && share_at(hi) * 1.35 < m.gpu_target;
        self.dyn_roomy = if roomy { self.dyn_roomy + 1 } else { 0 };
        if self.dyn_changed_ns != 0 && now_ns.saturating_sub(self.dyn_changed_ns) < 60_000_000_000 {
            return;
        }
        if !low && self.dyn_starved >= 2 {
            THERMAL_DYN_SCALE_BITS.store((5.0f32 / 6.0f32).to_bits(), Ordering::Relaxed);
            self.dyn_changed_ns = now_ns;
            self.dyn_starved = 0;
            self.dyn_roomy = 0;
        } else if low && self.dyn_roomy >= 10 {
            THERMAL_DYN_SCALE_BITS.store(1.0f32.to_bits(), Ordering::Relaxed);
            self.dyn_changed_ns = now_ns;
            self.dyn_starved = 0;
            self.dyn_roomy = 0;
        }
    }

    pub fn build_apply(&mut self, mode_idx: i32) -> (String, String) {
        let mode_idx = mode_idx.clamp(0, 2) as usize;
        let m = &MODES[mode_idx];
        let mut w_cmds: Vec<String> = Vec::new();
        if !self.gpu_path.is_empty() {
            let idx = self.gpu_idx.min(
                self.floor_idx(m)
                    .max(self.top().saturating_sub(self.drop as usize)),
            );
            let mut want = self.steps[idx];
            let keep_ssrm =
                m.battery_ssrm <= 0 || self.battery <= 0 || self.battery >= m.battery_ssrm * 10;
            if mode_idx != 0 && self.ssrm_low != 0 && keep_ssrm {
                want = want.min(self.ssrm_low);
            }
            if self.gpu_read != want {
                w_cmds.push(format!("echo {} > {}", self.gpu_value(want), self.gpu_path));
            }
            self.gpu_written = want;
        }
        let mut limit_want = 0i64;
        for p in &mut self.pol {
            let want = Self::policy_cap(p, self.cpu_level);
            limit_want = limit_want.max(want);
            if p.written == 0 && want >= p.read {
                continue;
            }
            if p.written != 0 && want == p.restore && p.read == p.restore {
                p.written = 0;
                continue;
            }
            if p.read != want && !(p.read < want && p.read != p.written) {
                w_cmds.push(format!("echo {want} > {}", p.path));
            }
            p.written = want;
        }
        if self.use_limit {
            let want = limit_want.min(self.limit_restore);
            if self.limit_read != want
                && !(self.limit_read < want && self.limit_read != self.limit_written)
            {
                w_cmds.push(format!("echo {want} > /sys/power/cpufreq_max_limit"));
            }
            self.limit_written = want;
        }

        let mut r_cmds: Vec<String> = Vec::new();
        if self.gpu_written != 0 {
            r_cmds.push(format!(
                "echo {} > {}",
                self.gpu_value(self.gpu_restore),
                self.gpu_path
            ));
        }
        for p in &self.pol {
            if p.written != 0 {
                r_cmds.push(format!("echo {} > {}", p.restore, p.path));
            }
        }
        if self.limit_written != 0 {
            r_cmds.push(format!(
                "echo {} > /sys/power/cpufreq_max_limit",
                self.limit_restore
            ));
        }
        let r_str = r_cmds.join("; ");
        let restore_cmd = if !r_str.is_empty() && r_str != self.restore_sent {
            self.restore_sent = r_str.clone();
            format!("R='{r_str}'")
        } else {
            String::new()
        };
        (restore_cmd, w_cmds.join("; "))
    }

    pub fn mark_restored(&mut self) {
        self.gpu_written = 0;
        for p in &mut self.pol {
            p.written = 0;
        }
        self.limit_written = 0;
    }

    pub fn reset_on_resume(&mut self) {
        self.gpu_idx = self.top();
        self.cpu_level = K_CPU_START_LEVEL;
        self.drop = 0;
        self.gpu_votes = 0;
        self.cpu_votes = 0;
        THERMAL_FRAMES.store(0, Ordering::Relaxed);
        THERMAL_LATE.store(0, Ordering::Relaxed);
        for b in &THERMAL_BINS {
            b.store(0, Ordering::Relaxed);
        }
    }
}

impl Default for AcnhThermalGovernor {
    fn default() -> Self {
        Self::new()
    }
}

#[no_mangle]
pub extern "C" fn acnh_platform_governor_new() -> *mut AcnhThermalGovernor {
    Box::into_raw(Box::new(AcnhThermalGovernor::new()))
}

/// Free an `AcnhThermalGovernor` allocated by `acnh_platform_governor_new`.
///
/// # Safety
/// `g` must be null or a valid pointer returned by `acnh_platform_governor_new` not yet freed.
#[no_mangle]
pub unsafe extern "C" fn acnh_platform_governor_free(g: *mut AcnhThermalGovernor) {
    if !g.is_null() {
        // SAFETY: Allocated by `acnh_platform_governor_new`.
        unsafe { drop(Box::from_raw(g)) };
    }
}

/// Parse initial sysfs discovery output and write the per-tick read command to `out_reads`.
///
/// # Safety
/// `g` must be a valid governor pointer; `output` and `out_reads` must be valid C buffers.
#[no_mangle]
pub unsafe extern "C" fn acnh_platform_governor_discover_parse(
    g: *mut AcnhThermalGovernor,
    output: *const c_char,
    out_reads: *mut c_char,
    cap: usize,
) -> bool {
    if g.is_null() {
        return false;
    }
    // SAFETY: Valid pointer per caller contract.
    let gov = unsafe { &mut *g };
    if !gov.discover_parse(cstr_or_empty(output)) {
        return false;
    }
    copy_str_to_buf(&gov.reads_cmd, out_reads, cap);
    true
}

/// Parse one tick's sysfs sample output.
///
/// # Safety
/// `g` must be a valid governor pointer; `output` must be null or a NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn acnh_platform_governor_sample_parse(
    g: *mut AcnhThermalGovernor,
    output: *const c_char,
) {
    if g.is_null() {
        return;
    }
    // SAFETY: Valid pointer per caller contract.
    unsafe { (&mut *g).sample_parse(cstr_or_empty(output)) };
}

/// Run one governor decision step.
///
/// # Safety
/// `g` must be null or a valid governor pointer.
#[no_mangle]
pub unsafe extern "C" fn acnh_platform_governor_decide(
    g: *mut AcnhThermalGovernor,
    mode_idx: i32,
    headroom: f32,
) {
    if g.is_null() {
        return;
    }
    let dyn_res = std::env::var("NX_DYNAMIC_RES")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| config_get("video.dynamic_resolution", "on"));
    let dyn_on = dyn_res == "on" || dyn_res == "1";
    // SAFETY: Valid pointer per caller contract.
    unsafe { (&mut *g).decide(mode_idx, host_monotonic_ns(), headroom, dyn_on) };
}

/// Build shell commands for `$R` restore and sysfs writes.
///
/// # Safety
/// `g` must be a valid governor pointer; `out_restore` and `out_write` must be valid buffers.
#[no_mangle]
pub unsafe extern "C" fn acnh_platform_governor_build_apply(
    g: *mut AcnhThermalGovernor,
    mode_idx: i32,
    out_restore: *mut c_char,
    restore_cap: usize,
    out_write: *mut c_char,
    write_cap: usize,
) {
    if g.is_null() {
        return;
    }
    // SAFETY: Valid pointer per caller contract.
    let (r, w) = unsafe { (&mut *g).build_apply(mode_idx) };
    copy_str_to_buf(&r, out_restore, restore_cap);
    copy_str_to_buf(&w, out_write, write_cap);
}

/// Mark all written sysfs caps as restored.
///
/// # Safety
/// `g` must be null or a valid governor pointer.
#[no_mangle]
pub unsafe extern "C" fn acnh_platform_governor_mark_restored(g: *mut AcnhThermalGovernor) {
    if !g.is_null() {
        // SAFETY: Valid pointer per caller contract.
        unsafe { (&mut *g).mark_restored() };
    }
}

/// Reset governor state when resuming from background.
///
/// # Safety
/// `g` must be null or a valid governor pointer.
#[no_mangle]
pub unsafe extern "C" fn acnh_platform_governor_reset_on_resume(g: *mut AcnhThermalGovernor) {
    if !g.is_null() {
        // SAFETY: Valid pointer per caller contract.
        unsafe { (&mut *g).reset_on_resume() };
    }
}

// ---- Performance Hints / ADPF / Clock Keeper (`perf_hint.cpp`) ---------------------------------

struct AdpfState {
    tids: Vec<i32>,
    session: usize,
    dirty: bool,
    target: i64,
    target_at: u64,
    preferred_rate_ns: i64,
    last_report_at: u64,
    pending_max_ns: i64,
    efficiency: i32,
}

impl Default for AdpfState {
    fn default() -> Self {
        Self {
            tids: Vec::new(),
            session: 0,
            dirty: false,
            target: 0,
            target_at: 0,
            preferred_rate_ns: 0,
            last_report_at: 0,
            pending_max_ns: 0,
            efficiency: -1,
        }
    }
}

static ADPF_MU: OnceLock<Mutex<AdpfState>> = OnceLock::new();
static LAST_FRAME_NS: AtomicU64 = AtomicU64::new(0);

fn adpf_mu() -> &'static Mutex<AdpfState> {
    ADPF_MU.get_or_init(|| Mutex::new(AdpfState::default()))
}

#[no_mangle]
pub extern "C" fn acnh_platform_perf_hint_thread(tid: i32) {
    let mut g = adpf_mu().lock().unwrap();
    if g.tids.len() < 8 && !g.tids.contains(&tid) {
        g.tids.push(tid);
        g.dirty = true;
    }
}

#[no_mangle]
pub extern "C" fn acnh_platform_perf_hint_last_frame_ns() -> u64 {
    LAST_FRAME_NS.load(Ordering::Relaxed)
}

#[no_mangle]
pub extern "C" fn acnh_platform_perf_hint_frame(ns: i64) {
    let now = host_monotonic_ns();
    LAST_FRAME_NS.store(now, Ordering::Relaxed);
    if ns <= 0 {
        return;
    }
    let h = host();
    let (Some(create), Some(close), Some(report)) = (
        h.adpf_create_session,
        h.adpf_close_session,
        h.adpf_report_actual,
    ) else {
        return;
    };

    let mode = acnh_platform_thermal_mode();
    let fixed = match std::env::var("NX_FPS_CAP")
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
    {
        Some(v) if v > 0 && v <= 30 => 33_333_333i64,
        _ => 16_666_667i64,
    };
    let period = acnh_platform_frame_period_ns();
    let target = if mode != 0 && period > 0 {
        ((period as f64) * acnh_platform_thermal_adpf_share(mode)) as i64
    } else {
        fixed
    };

    let mut g = adpf_mu().lock().unwrap();
    if g.dirty && !g.tids.is_empty() {
        if g.session != 0 {
            close(g.session as *mut c_void);
        }
        let sess = create(g.tids.as_ptr(), g.tids.len(), target);
        g.session = sess as usize;
        g.preferred_rate_ns = h.adpf_preferred_rate_ns.map(|f| f()).unwrap_or(0);
        g.last_report_at = 0;
        g.pending_max_ns = 0;
        g.dirty = false;
        g.target = target;
        g.efficiency = -1;
    }
    if g.session == 0 {
        return;
    }
    if target != g.target && now.saturating_sub(g.target_at) > 1_000_000_000 {
        if let Some(update) = h.adpf_update_target {
            update(g.session as *mut c_void, target);
        }
        g.target = target;
        g.target_at = now;
    }
    let eff = i32::from(mode == 2);
    if eff != g.efficiency {
        if let Some(set_eff) = h.adpf_set_efficiency {
            set_eff(g.session as *mut c_void, eff != 0);
        }
        g.efficiency = eff;
    }
    let report_ns = if mode == 0 {
        ns
    } else {
        ns.max(acnh_platform_frame_replay_work_ns())
    };
    if g.preferred_rate_ns > 0
        && g.last_report_at != 0
        && now.saturating_sub(g.last_report_at) < g.preferred_rate_ns as u64
    {
        g.pending_max_ns = g.pending_max_ns.max(report_ns);
        return;
    }
    let send_ns = report_ns.max(g.pending_max_ns);
    g.pending_max_ns = 0;
    g.last_report_at = now;
    report(g.session as *mut c_void, send_ns);
}

#[no_mangle]
pub extern "C" fn acnh_platform_gpu_turbo() {
    let turbo = std::env::var("NX_GPU_TURBO")
        .map(|v| v.starts_with('1'))
        .unwrap_or(false);
    if let Some(f) = host().kgsl_set_pwrctrl {
        f(!turbo);
    }
}

/// Parse `NX_CLOCK_KEEPER` comma-separated CPU indices (max 64).
///
/// # Safety
/// `out` must be null or point to `max` writable `i32` elements.
#[no_mangle]
pub unsafe extern "C" fn acnh_platform_parse_keeper_cpus(
    spec: *const c_char,
    out: *mut i32,
    max: usize,
) -> usize {
    let s = cstr_or_empty(spec);
    if s.is_empty() || s == "0" || s == "1" {
        return 0;
    }
    let mut seen = HashSet::new();
    let mut count = 0usize;
    for part in s.split(',') {
        if count >= max {
            break;
        }
        if let Ok(cpu) = part.trim().parse::<i32>() {
            if (0..1024).contains(&cpu) && seen.insert(cpu) {
                if !out.is_null() {
                    // SAFETY: Caller guarantees `out` holds `max` `i32`s.
                    unsafe { *out.add(count) = cpu };
                }
                count += 1;
            }
        }
    }
    count
}

// ---- Unit tests --------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn pad_kind_classification() {
        assert_eq!(pad_kind_str(0x057e, ""), "switch");
        assert_eq!(pad_kind_str(0, "Pro Controller"), "switch");
        assert_eq!(pad_kind_str(0x045e, ""), "xbox");
        assert_eq!(pad_kind_str(0, "Xbox Wireless Controller"), "xbox");
        assert_eq!(pad_kind_str(0x054c, ""), "playstation");
        assert_eq!(
            pad_kind_str(0, "DualSense Wireless Controller"),
            "playstation"
        );
        assert_eq!(pad_kind_str(0x2dc8, "8BitDo Pro 2"), "generic");
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
