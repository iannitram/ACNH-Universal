//! `nn::hid` HLE, scripted/recorded input, and drawn touch-control layout, ported from
//! `runtime/src/hle_hid.cpp`, `runtime/src/input_script.cpp`, and `runtime/src/touch_layout.cpp`.
//!
//! `runtime/src/input_wrapper.cpp` provides the thin C++ host callbacks (`frame`, `data_root`,
//! `log`, `describe`, `hle_current_name`, `live_files`, `read_pad`, `read_player_pad`, `rumble`,
//! `monotonic_ns`, `game_id`, `touch_y_bias`), registers the `NX_HLE` entries, and forwards the
//! `nx::` / `nx::plat::` C++ entry points.

#[allow(unused_imports)]
use std::collections::{HashSet, VecDeque};
#[allow(unused_imports)]
use std::ffi::{c_char, CStr};
#[allow(unused_imports)]
use std::fs::File;
#[allow(unused_imports)]
use std::io::Write;
#[allow(unused_imports)]
use std::path::PathBuf;
#[allow(unused_imports)]
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU64, Ordering};
#[allow(unused_imports)]
use std::sync::{Mutex, OnceLock};
#[allow(unused_imports)]
use std::time::Instant;

#[allow(unused_imports)]
use crate::config::{config_get, config_get_bool, config_get_int, config_set};

// ---- NpadButton bits & constants (platform.h / hle_hid.cpp) ----

pub const K_A: u64 = 1 << 0;
pub const K_B: u64 = 1 << 1;
pub const K_X: u64 = 1 << 2;
pub const K_Y: u64 = 1 << 3;
pub const K_STICK_L: u64 = 1 << 4;
pub const K_STICK_R: u64 = 1 << 5;
pub const K_L: u64 = 1 << 6;
pub const K_R: u64 = 1 << 7;
pub const K_ZL: u64 = 1 << 8;
pub const K_ZR: u64 = 1 << 9;
pub const K_PLUS: u64 = 1 << 10;
pub const K_MINUS: u64 = 1 << 11;
pub const K_LEFT: u64 = 1 << 12;
pub const K_UP: u64 = 1 << 13;
pub const K_RIGHT: u64 = 1 << 14;
pub const K_DOWN: u64 = 1 << 15;
pub const K_STICK_L_LEFT: u64 = 1 << 16;
pub const K_STICK_L_UP: u64 = 1 << 17;
pub const K_STICK_L_RIGHT: u64 = 1 << 18;
pub const K_STICK_L_DOWN: u64 = 1 << 19;
pub const K_STICK_R_LEFT: u64 = 1 << 20;
pub const K_STICK_R_UP: u64 = 1 << 21;
pub const K_STICK_R_RIGHT: u64 = 1 << 22;
pub const K_STICK_R_DOWN: u64 = 1 << 23;
pub const K_SETTINGS_BUTTON: u64 = 1 << 63;

pub const K_STICK_MAX: i32 = 0x7FFF;
pub const K_MAX_PLAYERS: usize = 4;
pub const K_STYLE_FULL_KEY: u32 = 1 << 0;
pub(crate) const K_TYPING_PROBE_MS: i64 = 250;

// ---- C-ABI structs ----

/// Host pad state (`nx::plat::PadState`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AcnhPadState {
    pub buttons: u64,
    pub lx: i32,
    pub ly: i32,
    pub rx: i32,
    pub ry: i32,
}

/// Drawn touch button (`nx::plat::TouchButton`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AcnhTouchButton {
    pub x: f32,
    pub y: f32,
    pub r: f32,
    pub button: u64,
}

/// Drawn overlay circle (`nx::plat::OverlayShape`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AcnhOverlayShape {
    pub x: f32,
    pub y: f32,
    pub r: f32,
    pub alpha: f32,
    pub label: u32,
}

/// Host callbacks provided by `runtime/src/input_wrapper.cpp`.
///
/// When unset (e.g. in standalone Rust unit tests), `frame` defaults to `0`, `monotonic_ns`
/// uses a process-local `Instant`, `game_id` defaults to `"acnh"`, and `read_pad` leaves the
/// pad zeroed.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct AcnhInputHost {
    pub frame: Option<extern "C" fn() -> u64>,
    pub data_root: Option<extern "C" fn(out: *mut c_char, cap: usize) -> usize>,
    pub log: Option<extern "C" fn(msg: *const c_char)>,
    pub describe: Option<extern "C" fn(addr: u64, out: *mut c_char, cap: usize) -> usize>,
    pub hle_current_name: Option<extern "C" fn(out: *mut c_char, cap: usize) -> usize>,
    pub live_files: Option<extern "C" fn() -> bool>,
    pub read_pad: Option<extern "C" fn(out: *mut AcnhPadState)>,
    pub read_player_pad: Option<extern "C" fn(index: i32, out: *mut AcnhPadState) -> bool>,
    pub rumble: Option<extern "C" fn(low_freq: f32, high_freq: f32, duration_ms: u32)>,
    pub monotonic_ns: Option<extern "C" fn() -> u64>,
    pub game_id: Option<extern "C" fn(out: *mut c_char, cap: usize) -> usize>,
    pub touch_y_bias: Option<extern "C" fn() -> u32>,
}

// SAFETY: `AcnhInputHost` only holds plain function pointers.
unsafe impl Send for AcnhInputHost {}
// SAFETY: `AcnhInputHost` only holds plain function pointers.
unsafe impl Sync for AcnhInputHost {}

pub(crate) static HOST: OnceLock<Mutex<AcnhInputHost>> = OnceLock::new();
pub(crate) static MONO_START: OnceLock<Instant> = OnceLock::new();

pub(crate) fn host_copy() -> AcnhInputHost {
    HOST.get()
        .and_then(|m| m.lock().ok().map(|g| *g))
        .unwrap_or_default()
}

pub(crate) fn host_frame() -> u64 {
    host_copy().frame.map(|f| f()).unwrap_or(0)
}

pub(crate) fn call_string_cb(
    cb: Option<extern "C" fn(*mut c_char, usize) -> usize>,
) -> Option<String> {
    let f = cb?;
    let mut buf = vec![0u8; 512];
    let n = f(buf.as_mut_ptr().cast::<c_char>(), buf.len());
    if n >= buf.len() {
        buf.resize(n + 1, 0);
        let _ = f(buf.as_mut_ptr().cast::<c_char>(), buf.len());
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
}

pub(crate) fn host_data_root() -> String {
    call_string_cb(host_copy().data_root).unwrap_or_default()
}

pub(crate) fn host_log(msg: &str) {
    if let Some(f) = host_copy().log {
        let mut bytes = Vec::with_capacity(msg.len() + 1);
        for &b in msg.as_bytes() {
            bytes.push(if b == 0 { b' ' } else { b });
        }
        bytes.push(0);
        f(bytes.as_ptr().cast::<c_char>());
    }
}

pub(crate) fn host_describe(addr: u64) -> String {
    let h = host_copy();
    if let Some(f) = h.describe {
        let mut buf = vec![0u8; 256];
        let n = f(addr, buf.as_mut_ptr().cast::<c_char>(), buf.len());
        if n >= buf.len() {
            buf.resize(n + 1, 0);
            let _ = f(addr, buf.as_mut_ptr().cast::<c_char>(), buf.len());
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).into_owned()
    } else {
        format!("{addr:#x}")
    }
}

pub(crate) fn host_hle_current_name() -> String {
    call_string_cb(host_copy().hle_current_name).unwrap_or_default()
}

pub(crate) fn host_live_files() -> bool {
    host_copy().live_files.map(|f| f()).unwrap_or(false)
}

pub(crate) fn host_read_pad(p: &mut AcnhPadState) {
    if let Some(f) = host_copy().read_pad {
        f(p as *mut AcnhPadState);
    }
}

pub(crate) fn host_read_player_pad(index: i32, p: &mut AcnhPadState) -> bool {
    host_copy()
        .read_player_pad
        .map(|f| f(index, p as *mut AcnhPadState))
        .unwrap_or(false)
}

pub(crate) fn host_rumble(low: f32, high: f32, duration_ms: u32) {
    if let Some(f) = host_copy().rumble {
        f(low, high, duration_ms);
    }
}

pub(crate) fn host_monotonic_ns() -> u64 {
    if let Some(f) = host_copy().monotonic_ns {
        f()
    } else {
        let start = MONO_START.get_or_init(Instant::now);
        start.elapsed().as_nanos() as u64
    }
}

pub(crate) fn host_game_id() -> String {
    call_string_cb(host_copy().game_id).unwrap_or_else(|| String::from("acnh"))
}

pub(crate) fn host_touch_y_bias() -> u32 {
    host_copy().touch_y_bias.map(|f| f()).unwrap_or(0)
}

pub(crate) fn now_ms() -> i64 {
    let start = MONO_START.get_or_init(Instant::now);
    start.elapsed().as_millis() as i64
}

pub(crate) fn key_types(usage: i32) -> bool {
    (4..=39).contains(&usage) || (44..=56).contains(&usage)
}

pub(crate) fn env_int(name: &str, fallback: i32) -> i32 {
    std::env::var(name)
        .ok()
        .filter(|s| !s.is_empty())
        .and_then(|s| parse_c_int(&s))
        .unwrap_or(fallback)
}

pub(crate) fn parse_c_int(s: &str) -> Option<i32> {
    let s = s.trim_start();
    let (neg, rest) = if let Some(r) = s.strip_prefix('-') {
        (true, r)
    } else if let Some(r) = s.strip_prefix('+') {
        (false, r)
    } else {
        (false, s)
    };
    let digits_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if digits_end == 0 {
        return None;
    }
    let val: i64 = rest[..digits_end].parse().ok()?;
    Some((if neg { -val } else { val }) as i32)
}

pub(crate) fn parse_c_u64(s: &str) -> u64 {
    let s = s.trim_start();
    let digits_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if digits_end == 0 {
        return 0;
    }
    s[..digits_end].parse().unwrap_or(0)
}

pub(crate) fn parse_two_u32_x(s: &str) -> Option<(u32, u32)> {
    let (a, b) = s.split_once('x')?;
    let x_end = a.find(|c: char| !c.is_ascii_digit()).unwrap_or(a.len());
    let y_end = b.find(|c: char| !c.is_ascii_digit()).unwrap_or(b.len());
    if x_end == 0 || y_end == 0 {
        return None;
    }
    Some((a[..x_end].parse().ok()?, b[..y_end].parse().ok()?))
}

pub(crate) unsafe fn write_u32(ptr: *mut u8, off: usize, val: u32) {
    ptr.add(off).cast::<u32>().write_unaligned(val);
}

pub(crate) unsafe fn write_i32(ptr: *mut u8, off: usize, val: i32) {
    ptr.add(off).cast::<i32>().write_unaligned(val);
}

pub(crate) unsafe fn write_u64(ptr: *mut u8, off: usize, val: u64) {
    ptr.add(off).cast::<u64>().write_unaligned(val);
}

pub(crate) unsafe fn write_i64(ptr: *mut u8, off: usize, val: i64) {
    ptr.add(off).cast::<i64>().write_unaligned(val);
}

pub(crate) unsafe fn write_i8(ptr: *mut u8, off: usize, val: i8) {
    ptr.add(off).cast::<i8>().write_unaligned(val);
}

mod pad;
#[allow(unused_imports)]
pub use pad::*;
mod script_touch;
#[allow(unused_imports)]
pub use script_touch::*;
mod api;
#[allow(unused_imports)]
pub use api::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    static TEST_LOCK: StdMutex<()> = StdMutex::new(());
    static TEST_FRAME: AtomicU64 = AtomicU64::new(0);

    extern "C" fn test_frame() -> u64 {
        TEST_FRAME.load(Ordering::Relaxed)
    }

    extern "C" fn test_game_id(out: *mut c_char, cap: usize) -> usize {
        let s = b"acnh";
        if !out.is_null() && cap > 0 {
            let n = s.len().min(cap - 1);
            // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
            unsafe {
                std::ptr::copy_nonoverlapping(s.as_ptr(), out.cast::<u8>(), n);
                *out.add(n) = 0;
            }
        }
        s.len()
    }

    fn install_test_host() {
        let host = AcnhInputHost {
            frame: Some(test_frame),
            game_id: Some(test_game_id),
            ..Default::default()
        };
        // SAFETY: test-only: the host table lives on this test's stack and outlives every call that reads it.
        unsafe { acnh_input_set_host(&host as *const AcnhInputHost) };
    }

    #[test]
    fn parse_script_buttons_sticks_and_touch() {
        let parsed = parse_script("10:A,20:LUp,30:T640x360,40:M100x200\n", 100);
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[0].frame, 110);
        assert_eq!(parsed[0].buttons, K_A);
        assert_eq!(parsed[1].frame, 120);
        assert_eq!(parsed[1].ly, K_STICK_MAX);
        assert_eq!(parsed[2].frame, 130);
        assert!(parsed[2].touch);
        assert_eq!((parsed[2].tx, parsed[2].ty), (640, 360));
        assert_eq!(parsed[3].frame, 140);
        assert!(parsed[3].move_mouse);
        assert_eq!((parsed[3].tx, parsed[3].ty), (100, 200));
    }

    #[test]
    fn parse_input_script_semicolon_format() {
        let parsed = parse_input_script("1.5:A:0.25;2.0:LEFT:0.5;bad;3.0:UNKNOWN:1.0;");
        assert_eq!(parsed.len(), 2);
        assert!((parsed[0].at - 1.5).abs() < 1e-6);
        assert!((parsed[0].hold - 0.25).abs() < 1e-6);
        assert_eq!(parsed[0].bits, K_A);
        assert_eq!(parsed[1].bits, K_LEFT);
        assert_eq!(parsed[1].lx, -K_STICK_MAX);
    }

    #[test]
    fn stick_buttons_thresholds() {
        let mut p = AcnhPadState {
            buttons: 0,
            lx: -K_STICK_MAX,
            ly: K_STICK_MAX,
            rx: K_STICK_MAX,
            ry: -K_STICK_MAX,
        };
        stick_buttons(&mut p);
        assert_ne!(p.buttons & K_STICK_L_LEFT, 0);
        assert_ne!(p.buttons & K_STICK_L_UP, 0);
        assert_ne!(p.buttons & K_STICK_R_RIGHT, 0);
        assert_ne!(p.buttons & K_STICK_R_DOWN, 0);
    }

    #[test]
    fn npad_state_and_histories() {
        let _g = TEST_LOCK.lock().unwrap();
        install_test_host();
        TEST_FRAME.store(10, Ordering::Relaxed);

        let mut buf = [0u8; 0x28 * 4];
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        let count = unsafe { acnh_input_get_npad_states_full_key(buf.as_mut_ptr(), 4, 0, 0) };
        assert!(count >= 1);
        let is_connected = u32::from_le_bytes(buf[32..36].try_into().unwrap());
        assert_eq!(is_connected, 1);

        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        let other = unsafe { acnh_input_get_npad_states_other(buf.as_mut_ptr(), 2, 0) };
        assert_eq!(other, 1);
        assert_eq!(
            acnh_input_get_npad_style_set(0),
            u64::from(K_STYLE_FULL_KEY)
        );
        assert_eq!(acnh_input_get_npad_style_set(1), 0);
    }

    #[test]
    fn touch_tap_and_mouse_hover_lifecycle() {
        let _g = TEST_LOCK.lock().unwrap();
        install_test_host();
        acnh_input_set_picture_box(0.0, 0.0, 1.0, 1.0);

        TEST_FRAME.store(100, Ordering::Relaxed);
        acnh_input_set_mouse_touch(true, 0.5, 0.5);
        let mut ts = [0u8; 0x10 + 0x28];
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        unsafe { acnh_input_get_touch_screen_state1(ts.as_mut_ptr(), 0) };
        assert_eq!(i32::from_le_bytes(ts[8..12].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(ts[0x18..0x1C].try_into().unwrap()), 1); // Start

        TEST_FRAME.store(101, Ordering::Relaxed);
        acnh_input_set_mouse_touch(false, 0.6, 0.6);
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        unsafe { acnh_input_get_touch_screen_state1(ts.as_mut_ptr(), 0) };
        assert_eq!(i32::from_le_bytes(ts[8..12].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(ts[0x18..0x1C].try_into().unwrap()), 2); // End

        let (mut tx, mut ty, mut tf) = (0, 0, 0);
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        assert!(unsafe { acnh_input_touch_take_tap(&mut tx, &mut ty, &mut tf) });
        assert_eq!((tx, ty, tf), (640, 360, 101));
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        assert!(!unsafe { acnh_input_touch_take_tap(&mut tx, &mut ty, &mut tf) });

        let (mut hx, mut hy) = (0, 0);
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        assert!(unsafe { acnh_input_mouse_take_hover(&mut hx, &mut hy) });
        assert_eq!((hx, hy), (768, 432));
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        assert!(!unsafe { acnh_input_mouse_take_hover(&mut hx, &mut hy) });
    }

    #[test]
    fn touch_layout_and_hit_testing() {
        let _g = TEST_LOCK.lock().unwrap();
        install_test_host();

        let mut count = 0usize;
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        let ptr = unsafe { acnh_input_touch_buttons(&mut count, 16.0 / 9.0) };
        assert_eq!(count, K_ACNH_COUNT);
        assert!(!ptr.is_null());

        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        let first = unsafe { *ptr };
        assert_eq!(acnh_input_touch_hit(first.x, first.y, 16.0 / 9.0), K_A);
        assert_eq!(acnh_input_touch_hit(0.5, 0.5, 16.0 / 9.0), 0);

        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        let group = unsafe { CStr::from_ptr(acnh_input_touch_group(0)) }
            .to_str()
            .unwrap();
        assert_eq!(group, "FACE");
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        let label = unsafe { CStr::from_ptr(acnh_input_touch_label(0)) }
            .to_str()
            .unwrap();
        assert_eq!(label, "A");
    }

    #[test]
    fn keyboard_typing_state_transitions() {
        let _g = TEST_LOCK.lock().unwrap();
        install_test_host();

        TEST_FRAME.store(50, Ordering::Relaxed);
        acnh_input_keyboard_text_entry(true);
        assert!(acnh_input_keyboard_state_known());
        assert!(acnh_input_keyboard_typing());

        TEST_FRAME.store(54, Ordering::Relaxed);
        assert!(!acnh_input_keyboard_typing());
    }
}
