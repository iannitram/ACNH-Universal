//! `nn::hid` HLE, scripted/recorded input, and drawn touch-control layout, ported from
//! `runtime/src/hle_hid.cpp`, `runtime/src/input_script.cpp`, and `runtime/src/touch_layout.cpp`.
//!
//! `runtime/src/input_wrapper.cpp` provides the thin C++ host callbacks (`frame`, `data_root`,
//! `log`, `describe`, `hle_current_name`, `live_files`, `read_pad`, `read_player_pad`, `rumble`,
//! `monotonic_ns`, `game_id`, `touch_y_bias`), registers the `NX_HLE` entries, and forwards the
//! `nx::` / `nx::plat::` C++ entry points.

use std::collections::{HashSet, VecDeque};
use std::ffi::{c_char, CStr};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

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
const K_TYPING_PROBE_MS: i64 = 250;

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

static HOST: OnceLock<Mutex<AcnhInputHost>> = OnceLock::new();
static MONO_START: OnceLock<Instant> = OnceLock::new();

fn host_copy() -> AcnhInputHost {
    HOST.get()
        .and_then(|m| m.lock().ok().map(|g| *g))
        .unwrap_or_default()
}

fn host_frame() -> u64 {
    host_copy().frame.map(|f| f()).unwrap_or(0)
}

fn call_string_cb(cb: Option<extern "C" fn(*mut c_char, usize) -> usize>) -> Option<String> {
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

fn host_data_root() -> String {
    call_string_cb(host_copy().data_root).unwrap_or_default()
}

fn host_log(msg: &str) {
    if let Some(f) = host_copy().log {
        let mut bytes = Vec::with_capacity(msg.len() + 1);
        for &b in msg.as_bytes() {
            bytes.push(if b == 0 { b' ' } else { b });
        }
        bytes.push(0);
        f(bytes.as_ptr().cast::<c_char>());
    }
}

fn host_describe(addr: u64) -> String {
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

fn host_hle_current_name() -> String {
    call_string_cb(host_copy().hle_current_name).unwrap_or_default()
}

fn host_live_files() -> bool {
    host_copy().live_files.map(|f| f()).unwrap_or(false)
}

fn host_read_pad(p: &mut AcnhPadState) {
    if let Some(f) = host_copy().read_pad {
        f(p as *mut AcnhPadState);
    }
}

fn host_read_player_pad(index: i32, p: &mut AcnhPadState) -> bool {
    host_copy()
        .read_player_pad
        .map(|f| f(index, p as *mut AcnhPadState))
        .unwrap_or(false)
}

fn host_rumble(low: f32, high: f32, duration_ms: u32) {
    if let Some(f) = host_copy().rumble {
        f(low, high, duration_ms);
    }
}

fn host_monotonic_ns() -> u64 {
    if let Some(f) = host_copy().monotonic_ns {
        f()
    } else {
        let start = MONO_START.get_or_init(Instant::now);
        start.elapsed().as_nanos() as u64
    }
}

fn host_game_id() -> String {
    call_string_cb(host_copy().game_id).unwrap_or_else(|| String::from("acnh"))
}

fn host_touch_y_bias() -> u32 {
    host_copy().touch_y_bias.map(|f| f()).unwrap_or(0)
}

fn now_ms() -> i64 {
    let start = MONO_START.get_or_init(Instant::now);
    start.elapsed().as_millis() as i64
}

fn key_types(usage: i32) -> bool {
    (4..=39).contains(&usage) || (44..=56).contains(&usage)
}

fn env_int(name: &str, fallback: i32) -> i32 {
    std::env::var(name)
        .ok()
        .filter(|s| !s.is_empty())
        .and_then(|s| parse_c_int(&s))
        .unwrap_or(fallback)
}

fn parse_c_int(s: &str) -> Option<i32> {
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

fn parse_c_u64(s: &str) -> u64 {
    let s = s.trim_start();
    let digits_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if digits_end == 0 {
        return 0;
    }
    s[..digits_end].parse().unwrap_or(0)
}

fn parse_two_u32_x(s: &str) -> Option<(u32, u32)> {
    let (a, b) = s.split_once('x')?;
    let x_end = a.find(|c: char| !c.is_ascii_digit()).unwrap_or(a.len());
    let y_end = b.find(|c: char| !c.is_ascii_digit()).unwrap_or(b.len());
    if x_end == 0 || y_end == 0 {
        return None;
    }
    Some((a[..x_end].parse().ok()?, b[..y_end].parse().ok()?))
}

unsafe fn write_u32(ptr: *mut u8, off: usize, val: u32) {
    ptr.add(off).cast::<u32>().write_unaligned(val);
}

unsafe fn write_i32(ptr: *mut u8, off: usize, val: i32) {
    ptr.add(off).cast::<i32>().write_unaligned(val);
}

unsafe fn write_u64(ptr: *mut u8, off: usize, val: u64) {
    ptr.add(off).cast::<u64>().write_unaligned(val);
}

unsafe fn write_i64(ptr: *mut u8, off: usize, val: i64) {
    ptr.add(off).cast::<i64>().write_unaligned(val);
}

unsafe fn write_i8(ptr: *mut u8, off: usize, val: i8) {
    ptr.add(off).cast::<i8>().write_unaligned(val);
}

// ---- Scripted & live input (hle_hid.cpp) ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Scripted {
    frame: u64,
    buttons: u64,
    lx: i32,
    ly: i32,
    touch: bool,
    move_mouse: bool,
    tx: u32,
    ty: u32,
}

fn parse_script(s: &str, base: u64) -> Vec<Scripted> {
    let mut v = Vec::new();
    for raw in s.split(',') {
        let item = raw.trim_end_matches(|c: char| c.is_ascii_whitespace());
        let Some((frame_str, b)) = item.split_once(':') else {
            continue;
        };
        let frame = base.saturating_add(parse_c_u64(frame_str));
        let buttons = match b {
            "A" => K_A,
            "B" => K_B,
            "X" => K_X,
            "Y" => K_Y,
            "Plus" => K_PLUS,
            "Minus" => K_MINUS,
            "Up" => K_UP,
            "Down" => K_DOWN,
            "Left" => K_LEFT,
            "Right" => K_RIGHT,
            "L" => K_L,
            "R" => K_R,
            "ZL" => K_ZL,
            "ZR" => K_ZR,
            _ => 0,
        };
        let mut sc = Scripted {
            frame,
            buttons,
            lx: 0,
            ly: 0,
            touch: false,
            move_mouse: false,
            tx: 0,
            ty: 0,
        };
        match b {
            "LUp" => sc.ly = K_STICK_MAX,
            "LDown" => sc.ly = -K_STICK_MAX,
            "LLeft" => sc.lx = -K_STICK_MAX,
            "LRight" => sc.lx = K_STICK_MAX,
            _ => {}
        }
        if let Some(rest) = b.strip_prefix('T') {
            if let Some((tx, ty)) = parse_two_u32_x(rest) {
                sc.touch = true;
                sc.tx = tx;
                sc.ty = ty;
            }
        }
        if let Some(rest) = b.strip_prefix('M') {
            if let Some((tx, ty)) = parse_two_u32_x(rest) {
                sc.move_mouse = true;
                sc.tx = tx;
                sc.ty = ty;
            }
        }
        v.push(sc);
    }
    v
}

struct ScriptState {
    entries: Vec<Scripted>,
    next_check: u64,
    last_trace_buttons: u64,
}

static SCRIPT_MU: OnceLock<Mutex<ScriptState>> = OnceLock::new();

fn script_lock() -> &'static Mutex<ScriptState> {
    SCRIPT_MU.get_or_init(|| {
        let entries = std::env::var("NX_INPUT")
            .map(|e| parse_script(&e, 0))
            .unwrap_or_default();
        Mutex::new(ScriptState {
            entries,
            next_check: 0,
            last_trace_buttons: 0,
        })
    })
}

fn poll_live(state: &mut ScriptState) {
    let frame = host_frame();
    if frame < state.next_check {
        return;
    }
    state.next_check = frame + 15;
    if std::env::var_os("NX_LIVE_FRAME").is_some() {
        let root = host_data_root();
        let out = PathBuf::from(&root).join("live_frame");
        let tmp = PathBuf::from(&root).join("live_frame.tmp");
        if let Ok(mut f) = File::create(&tmp) {
            let _ = writeln!(f, "{frame}");
            drop(f);
            let _ = std::fs::rename(&tmp, &out);
        }
    }
    if !host_live_files() {
        return;
    }
    let path = PathBuf::from(host_data_root()).join("input_live");
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    let _ = std::fs::remove_file(&path);
    let s = String::from_utf8_lossy(&bytes);
    state.entries.retain(|x| x.frame + 8 >= frame);
    let add = parse_script(&s, frame + 1);
    host_log(&format!(
        "hid: live input, {} entries from frame {}",
        add.len(),
        frame
    ));
    state.entries.extend(add);
}

// ---- Pad state & Npad HLE ----

static G_SAMPLING: AtomicU64 = AtomicU64::new(0);
static G_HELD_BUTTONS: AtomicU64 = AtomicU64::new(0);
static G_HELD_FROM: AtomicU64 = AtomicU64::new(0);
static G_HELD_UNTIL: AtomicU64 = AtomicU64::new(0);

fn stick_buttons(p: &mut AcnhPadState) {
    let t = K_STICK_MAX / 2;
    if p.lx < -t {
        p.buttons |= K_STICK_L_LEFT;
    }
    if p.lx > t {
        p.buttons |= K_STICK_L_RIGHT;
    }
    if p.ly > t {
        p.buttons |= K_STICK_L_UP;
    }
    if p.ly < -t {
        p.buttons |= K_STICK_L_DOWN;
    }
    if p.rx < -t {
        p.buttons |= K_STICK_R_LEFT;
    }
    if p.rx > t {
        p.buttons |= K_STICK_R_RIGHT;
    }
    if p.ry > t {
        p.buttons |= K_STICK_R_UP;
    }
    if p.ry < -t {
        p.buttons |= K_STICK_R_DOWN;
    }
}

fn read_pad_with_script() -> AcnhPadState {
    let mut p = AcnhPadState::default();
    let frame = host_frame();
    let mut st = script_lock().lock().unwrap_or_else(|e| e.into_inner());
    poll_live(&mut st);
    for s in &st.entries {
        if frame >= s.frame && frame < s.frame + 8 {
            p.buttons |= s.buttons;
        }
    }
    if std::env::var_os("NX_INPUT_TRACE").is_some() && p.buttons != st.last_trace_buttons {
        let sample = G_SAMPLING.load(Ordering::Relaxed) + 1;
        host_log(&format!(
            "hid: script buttons {:x} at frame {}, sample {}",
            p.buttons, frame, sample
        ));
    }
    st.last_trace_buttons = p.buttons;
    let held_from = G_HELD_FROM.load(Ordering::Relaxed);
    let held_until = G_HELD_UNTIL.load(Ordering::Relaxed);
    if frame >= held_from && frame < held_until {
        p.buttons |= G_HELD_BUTTONS.load(Ordering::Relaxed);
    }
    host_read_pad(&mut p);
    let mut stick = false;
    for s in &st.entries {
        if (s.lx != 0 || s.ly != 0) && frame >= s.frame && frame < s.frame + 8 {
            if !stick {
                p.lx = 0;
                p.ly = 0;
                stick = true;
            }
            if s.lx != 0 {
                p.lx = s.lx;
            }
            if s.ly != 0 {
                p.ly = s.ly;
            }
        }
    }
    stick_buttons(&mut p);
    p
}

fn read_player(id: u32) -> Option<AcnhPadState> {
    if id == 0 {
        return Some(read_pad_with_script());
    }
    if (id as usize) >= K_MAX_PLAYERS {
        return None;
    }
    let mut p = AcnhPadState::default();
    if !host_read_player_pad(id as i32, &mut p) {
        return None;
    }
    stick_buttons(&mut p);
    Some(p)
}

static CONNECTED_WAS: OnceLock<Mutex<[bool; K_MAX_PLAYERS]>> = OnceLock::new();

fn connected(id: u32) -> bool {
    let on = read_player(id).is_some();
    let mu = CONNECTED_WAS.get_or_init(|| Mutex::new([true, false, false, false]));
    let mut was = mu.lock().unwrap_or_else(|e| e.into_inner());
    if (id as usize) < K_MAX_PLAYERS && on != was[id as usize] {
        was[id as usize] = on;
        host_log(&format!(
            "hid: player {} {}",
            id + 1,
            if on { "connected" } else { "disconnected" }
        ));
    }
    on
}

static SEEN_NPAD_READS: OnceLock<Mutex<HashSet<(u32, String)>>> = OnceLock::new();
static FIRST_PRESSED: [AtomicBool; K_MAX_PLAYERS] = [
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
];

fn get_npad_state_inner(out: *mut u8, id: u32) {
    if out.is_null() {
        return;
    }
    // SAFETY: Caller passes a valid 0x28-byte Npad state buffer.
    unsafe {
        std::ptr::write_bytes(out, 0, 0x28);
        let sampling = G_SAMPLING.fetch_add(1, Ordering::Relaxed) + 1;
        write_i64(out, 0, sampling as i64);
    }
    let cur_name = host_hle_current_name();
    {
        let mu = SEEN_NPAD_READS.get_or_init(|| Mutex::new(HashSet::new()));
        let mut seen = mu.lock().unwrap_or_else(|e| e.into_inner());
        if seen.insert((id, cur_name.clone())) {
            host_log(&format!("hid: reads id {id:#x} via {cur_name}"));
        }
    }
    let Some(p) = read_player(id) else {
        return;
    };
    if p.buttons != 0
        && (id as usize) < K_MAX_PLAYERS
        && !FIRST_PRESSED[id as usize].swap(true, Ordering::Relaxed)
    {
        if id == 0 {
            host_log(&format!("hid: first buttons {:x}", p.buttons));
        } else {
            host_log(&format!(
                "hid: player {}'s first buttons {:x}",
                id + 1,
                p.buttons
            ));
        }
    }
    // SAFETY: `out` is non-null and points to a 0x28-byte buffer.
    unsafe {
        write_u64(out, 8, p.buttons);
        write_i32(out, 16, p.lx);
        write_i32(out, 20, p.ly);
        write_i32(out, 24, p.rx);
        write_i32(out, 28, p.ry);
        write_u32(out, 32, 1);
    }
}

static FULL_KEY_CALLS: AtomicU64 = AtomicU64::new(0);
static HISTORIES_MU: OnceLock<Mutex<[VecDeque<[u8; 0x28]>; K_MAX_PLAYERS]>> = OnceLock::new();
static SEEN_OTHER_READS: OnceLock<Mutex<HashSet<(u32, String)>>> = OnceLock::new();
static STYLE_SET_LOGGED: AtomicI32 = AtomicI32::new(0);

// ---- Keyboard state ----

struct KbdState {
    mods: u64,
    keys: [u8; 32],
    probe_ms: i64,
}

static KBD_MU: OnceLock<Mutex<KbdState>> = OnceLock::new();

fn kbd_lock() -> &'static Mutex<KbdState> {
    KBD_MU.get_or_init(|| {
        Mutex::new(KbdState {
            mods: 0,
            keys: [0; 32],
            probe_ms: 0,
        })
    })
}

static G_TYPING: AtomicBool = AtomicBool::new(false);
static G_TYPING_KNOWN: AtomicBool = AtomicBool::new(false);
static G_KEYCODE_MS: AtomicI64 = AtomicI64::new(0);
static G_ENTRY_FRAME: AtomicU64 = AtomicU64::new(0);

fn set_text_entry(open: bool) {
    if G_TYPING.swap(open, Ordering::Relaxed) != open {
        host_log(&format!(
            "hid: the game's text entry {}: the keys {}",
            if open { "opened" } else { "closed" },
            if open { "type" } else { "drive the pad" }
        ));
    }
}

// ---- Touch & mouse state ----

#[derive(Clone, Copy, Default)]
struct TouchState {
    down: bool,
    x: u32,
    y: u32,
}

#[derive(Clone, Copy, Default)]
struct HoverState {
    x: u32,
    y: u32,
    moves: u64,
    scripted_until: u64,
    taken: u64,
}

struct TouchInputState {
    box_rect: [f32; 4],
    mouse: TouchState,
    mouse_tap: bool,
    hover: HoverState,
}

static TOUCH_MU: OnceLock<Mutex<TouchInputState>> = OnceLock::new();

fn touch_lock() -> &'static Mutex<TouchInputState> {
    TOUCH_MU.get_or_init(|| {
        Mutex::new(TouchInputState {
            box_rect: [0.0, 0.0, 1.0, 1.0],
            mouse: TouchState::default(),
            mouse_tap: false,
            hover: HoverState::default(),
        })
    })
}

#[derive(Clone, Copy)]
struct TouchSample {
    sampling: u64,
    attributes: u32,
    listed: bool,
    x: u32,
    y: u32,
}

struct TouchSampleState {
    frame: u64,
    cur: TouchSample,
}

static TOUCH_SAMPLE_MU: OnceLock<Mutex<TouchSampleState>> = OnceLock::new();

#[derive(Clone, Copy, Default)]
struct TapState {
    pending: bool,
    x: u32,
    y: u32,
    frame: u64,
}

static TAP_MU: OnceLock<Mutex<TapState>> = OnceLock::new();

fn current_touch() -> TouchState {
    let frame = host_frame();
    {
        let st = script_lock().lock().unwrap_or_else(|e| e.into_inner());
        for s in &st.entries {
            if s.touch && frame >= s.frame && frame < s.frame + 8 {
                return TouchState {
                    down: true,
                    x: s.tx,
                    y: s.ty,
                };
            }
        }
    }
    let mut ti = touch_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut t = ti.mouse;
    if ti.mouse_tap {
        t.down = true;
        ti.mouse_tap = false;
    }
    t
}

fn touch_sample() -> TouchSample {
    let mu = TOUCH_SAMPLE_MU.get_or_init(|| {
        Mutex::new(TouchSampleState {
            frame: u64::MAX,
            cur: TouchSample {
                sampling: 0,
                attributes: 0,
                listed: false,
                x: 0,
                y: 0,
            },
        })
    });
    let mut st = mu.lock().unwrap_or_else(|e| e.into_inner());
    let frame = host_frame();
    if frame == st.frame {
        return st.cur;
    }
    st.frame = frame;
    let t = current_touch();
    let was_down = st.cur.listed && (st.cur.attributes & 2) == 0;
    st.cur.sampling += 1;
    if t.down {
        st.cur.listed = true;
        st.cur.attributes = if was_down { 0 } else { 1 };
        st.cur.x = t.x;
        st.cur.y = t.y;
    } else if was_down {
        st.cur.attributes = 2;
        let tap_mu = TAP_MU.get_or_init(|| Mutex::new(TapState::default()));
        let mut tap = tap_mu.lock().unwrap_or_else(|e| e.into_inner());
        *tap = TapState {
            pending: true,
            x: st.cur.x,
            y: st.cur.y,
            frame,
        };
    } else {
        st.cur.listed = false;
        st.cur.attributes = 0;
    }
    if std::env::var_os("NX_TRACE_TOUCH").is_some() && st.cur.listed {
        let suffix = match st.cur.attributes {
            1 => " (start)",
            2 => " (end)",
            _ => "",
        };
        host_log(&format!(
            "hid: touch at frame {}: {} x {}{}",
            frame, st.cur.x, st.cur.y, suffix
        ));
    }
    st.cur
}

unsafe fn fill_touch(out: *mut u8, y_bias: u32) {
    let t = touch_sample();
    std::ptr::write_bytes(out, 0, 0x10 + 0x28);
    write_i64(out, 0, t.sampling as i64);
    if t.listed {
        write_i32(out, 8, 1);
        let s = out.add(0x10);
        write_u32(s, 8, t.attributes);
        write_u32(s, 16, t.x);
        write_u32(s, 20, t.y.saturating_add(y_bias).min(719));
        write_u32(s, 24, 15);
        write_u32(s, 28, 15);
    }
}

static TRACE_TOUCH_1: AtomicI32 = AtomicI32::new(0);
static TRACE_TOUCH_16: AtomicI32 = AtomicI32::new(0);

// ---- Input script (input_script.cpp) ----

struct InputScriptPress {
    at: f64,
    hold: f64,
    bits: u64,
    lx: i32,
    ly: i32,
}

const SCRIPT_NAMES: [(&str, u64, i32, i32); 14] = [
    ("A", K_A, 0, 0),
    ("B", K_B, 0, 0),
    ("X", K_X, 0, 0),
    ("Y", K_Y, 0, 0),
    ("L", K_L, 0, 0),
    ("R", K_R, 0, 0),
    ("ZL", K_ZL, 0, 0),
    ("ZR", K_ZR, 0, 0),
    ("PLUS", K_PLUS, 0, 0),
    ("MINUS", K_MINUS, 0, 0),
    ("LEFT", K_LEFT, -K_STICK_MAX, 0),
    ("RIGHT", K_RIGHT, K_STICK_MAX, 0),
    ("UP", K_UP, 0, K_STICK_MAX),
    ("DOWN", K_DOWN, 0, -K_STICK_MAX),
];

fn parse_input_script(s: &str) -> Vec<InputScriptPress> {
    let mut v = Vec::new();
    for item in s.split(';') {
        let Some((at_str, rest)) = item.split_once(':') else {
            continue;
        };
        let Some((button, hold_str)) = rest.split_once(':') else {
            continue;
        };
        let at: f64 = at_str.trim().parse().unwrap_or(0.0);
        let hold: f64 = hold_str.trim().parse().unwrap_or(0.0);
        let mut bits = 0;
        let mut lx = 0;
        let mut ly = 0;
        for &(name, b, sx, sy) in &SCRIPT_NAMES {
            if button == name {
                bits = b;
                lx = sx;
                ly = sy;
            }
        }
        if bits != 0 {
            v.push(InputScriptPress {
                at,
                hold,
                bits,
                lx,
                ly,
            });
        }
    }
    v
}

struct InputScriptRunner {
    has_env: bool,
    recording: bool,
    start_ns: Option<u64>,
    script: Vec<InputScriptPress>,
    rec_down: [f64; 14],
    rec_init: bool,
    rec_file: Option<File>,
}

static INPUT_SCRIPT_MU: OnceLock<Mutex<InputScriptRunner>> = OnceLock::new();

fn record_input(runner: &mut InputScriptRunner, p: &AcnhPadState, t: f64) {
    if !runner.rec_init {
        runner.rec_init = true;
        runner.rec_down = [-1.0; 14];
        let path = match std::env::var("NX_INPUT_RECORD") {
            Ok(e) if e != "1" => PathBuf::from(e),
            _ => PathBuf::from(host_data_root()).join("input_record.txt"),
        };
        runner.rec_file = File::create(path).ok();
    }
    let Some(file) = runner.rec_file.as_mut() else {
        return;
    };
    let half = K_STICK_MAX / 2;
    let mut now = p.buttons;
    if p.lx < -half {
        now |= K_LEFT;
    }
    if p.lx > half {
        now |= K_RIGHT;
    }
    if p.ly > half {
        now |= K_UP;
    }
    if p.ly < -half {
        now |= K_DOWN;
    }
    for (k, &(name, bits, _, _)) in SCRIPT_NAMES.iter().enumerate() {
        let held = (now & bits) != 0;
        if held && runner.rec_down[k] < 0.0 {
            runner.rec_down[k] = t;
        }
        if !held && runner.rec_down[k] >= 0.0 {
            let dur = (t - runner.rec_down[k]).max(0.05);
            let _ = write!(file, "{:.2}:{name}:{dur:.2};", runner.rec_down[k]);
            let _ = file.flush();
            runner.rec_down[k] = -1.0;
        }
    }
}

// ---- Touch layout (touch_layout.cpp) ----

const K_ACNH_ASPECT: f32 = 16.0 / 9.0;
const K_ACNH_COUNT: usize = 15;

const K_WONDER: [AcnhTouchButton; 9] = [
    AcnhTouchButton {
        x: 0.905,
        y: 0.74,
        r: 0.105,
        button: K_A,
    },
    AcnhTouchButton {
        x: 0.775,
        y: 0.86,
        r: 0.085,
        button: K_B,
    },
    AcnhTouchButton {
        x: 0.775,
        y: 0.60,
        r: 0.085,
        button: K_Y,
    },
    AcnhTouchButton {
        x: 0.905,
        y: 0.47,
        r: 0.075,
        button: K_X,
    },
    AcnhTouchButton {
        x: 0.955,
        y: 0.25,
        r: 0.055,
        button: K_ZR,
    },
    AcnhTouchButton {
        x: 0.955,
        y: 0.07,
        r: 0.045,
        button: K_R,
    },
    AcnhTouchButton {
        x: 0.045,
        y: 0.07,
        r: 0.045,
        button: K_L,
    },
    AcnhTouchButton {
        x: 0.545,
        y: 0.07,
        r: 0.040,
        button: K_PLUS,
    },
    AcnhTouchButton {
        x: 0.455,
        y: 0.07,
        r: 0.040,
        button: K_MINUS,
    },
];

const K_WONDER_NAMES: [&str; 9] = ["A", "B", "Y", "X", "ZR", "R", "L", "PLUS", "MINUS"];

const K_ACNH: [AcnhTouchButton; K_ACNH_COUNT] = [
    AcnhTouchButton {
        x: 0.915,
        y: 0.700,
        r: 0.080,
        button: K_A,
    },
    AcnhTouchButton {
        x: 0.835,
        y: 0.845,
        r: 0.080,
        button: K_B,
    },
    AcnhTouchButton {
        x: 0.835,
        y: 0.555,
        r: 0.080,
        button: K_X,
    },
    AcnhTouchButton {
        x: 0.755,
        y: 0.700,
        r: 0.080,
        button: K_Y,
    },
    AcnhTouchButton {
        x: 0.955,
        y: 0.090,
        r: 0.050,
        button: K_R,
    },
    AcnhTouchButton {
        x: 0.880,
        y: 0.090,
        r: 0.050,
        button: K_ZR,
    },
    AcnhTouchButton {
        x: 0.045,
        y: 0.090,
        r: 0.050,
        button: K_L,
    },
    AcnhTouchButton {
        x: 0.120,
        y: 0.090,
        r: 0.050,
        button: K_ZL,
    },
    AcnhTouchButton {
        x: 0.560,
        y: 0.070,
        r: 0.040,
        button: K_PLUS,
    },
    AcnhTouchButton {
        x: 0.440,
        y: 0.070,
        r: 0.040,
        button: K_MINUS,
    },
    AcnhTouchButton {
        x: 0.500,
        y: 0.070,
        r: 0.036,
        button: K_SETTINGS_BUTTON,
    },
    AcnhTouchButton {
        x: 0.075,
        y: 0.700,
        r: 0.040,
        button: K_UP,
    },
    AcnhTouchButton {
        x: 0.075,
        y: 0.900,
        r: 0.040,
        button: K_DOWN,
    },
    AcnhTouchButton {
        x: 0.030,
        y: 0.800,
        r: 0.040,
        button: K_LEFT,
    },
    AcnhTouchButton {
        x: 0.120,
        y: 0.800,
        r: 0.040,
        button: K_RIGHT,
    },
];

const K_ACNH_GROUPS: [&str; K_ACNH_COUNT] = [
    "FACE", "FACE", "FACE", "FACE", "R", "ZR", "L", "ZL", "PLUS", "MINUS", "GEAR", "DPAD", "DPAD",
    "DPAD", "DPAD",
];

const K_ACNH_GROUP_CSTRS: [*const c_char; K_ACNH_COUNT] = [
    c"FACE".as_ptr(),
    c"FACE".as_ptr(),
    c"FACE".as_ptr(),
    c"FACE".as_ptr(),
    c"R".as_ptr(),
    c"ZR".as_ptr(),
    c"L".as_ptr(),
    c"ZL".as_ptr(),
    c"PLUS".as_ptr(),
    c"MINUS".as_ptr(),
    c"GEAR".as_ptr(),
    c"DPAD".as_ptr(),
    c"DPAD".as_ptr(),
    c"DPAD".as_ptr(),
    c"DPAD".as_ptr(),
];

const K_ACNH_LABEL_CSTRS: [*const c_char; K_ACNH_COUNT] = [
    c"A".as_ptr(),
    c"B".as_ptr(),
    c"X".as_ptr(),
    c"Y".as_ptr(),
    c"R".as_ptr(),
    c"ZR".as_ptr(),
    c"L".as_ptr(),
    c"ZL".as_ptr(),
    c"+".as_ptr(),
    c"-".as_ptr(),
    c"".as_ptr(),
    c"".as_ptr(),
    c"".as_ptr(),
    c"".as_ptr(),
    c"".as_ptr(),
];

fn touch_size_scale() -> f32 {
    static ENV: OnceLock<i32> = OnceLock::new();
    let env = *ENV.get_or_init(|| env_int("NX_TOUCH_SIZE", 0));
    let pct = if env != 0 {
        env
    } else {
        config_get_int("input.touch_size", 100)
    };
    pct.clamp(50, 150) as f32 / 100.0
}

fn fit_acnh(b: AcnhTouchButton, aspect: f32, s: f32) -> AcnhTouchButton {
    let mut o = b;
    if b.x < 0.3 {
        let dx = b.x * K_ACNH_ASPECT * s;
        o.x = dx / aspect;
    } else if b.x > 0.7 {
        let dx = (1.0 - b.x) * K_ACNH_ASPECT * s;
        o.x = 1.0 - dx / aspect;
    } else {
        let dx = (b.x - 0.5) * K_ACNH_ASPECT * s;
        o.x = 0.5 + dx / aspect;
    }
    o.y = if b.y < 0.5 {
        b.y * s
    } else {
        1.0 - (1.0 - b.y) * s
    };
    o.r = b.r * s;
    o
}

#[derive(Debug, Clone, PartialEq)]
struct TouchOffset {
    group: String,
    dx: f32,
    dy: f32,
}

fn parse_layout(all: &str) -> Vec<TouchOffset> {
    let mut out = Vec::new();
    for item in all.split(';') {
        if item.is_empty() {
            continue;
        }
        let Some((group, coords)) = item.split_once(':') else {
            continue;
        };
        let Some((dx_str, dy_str)) = coords.split_once(',') else {
            continue;
        };
        let Ok(dx) = dx_str.trim().parse::<f32>() else {
            continue;
        };
        let Ok(dy) = dy_str.trim().parse::<f32>() else {
            continue;
        };
        if !(dx.abs() < 5.0 && dy.abs() < 1.0) {
            continue;
        }
        out.push(TouchOffset {
            group: group.to_string(),
            dx,
            dy,
        });
    }
    out
}

fn clamp_offset(group: &str, aspect: f32, s: f32, dx: &mut f32, dy: &mut f32) {
    let mut lox = -1e9f32;
    let mut hix = 1e9f32;
    let mut loy = -1e9f32;
    let mut hiy = 1e9f32;
    for k in 0..K_ACNH_COUNT {
        if K_ACNH_GROUPS[k] != group {
            continue;
        }
        let b = fit_acnh(K_ACNH[k], aspect, s);
        lox = lox.max((b.r / aspect - b.x) * aspect);
        hix = hix.min((1.0 - b.r / aspect - b.x) * aspect);
        loy = loy.max(b.r - b.y);
        hiy = hiy.min(1.0 - b.r - b.y);
    }
    if lox <= hix {
        *dx = dx.clamp(lox, hix);
    }
    if loy <= hiy {
        *dy = dy.clamp(loy, hiy);
    }
}

static G_FACE_SWAPPED: AtomicBool = AtomicBool::new(false);

struct AcnhButtonsCache {
    buf: [[AcnhTouchButton; K_ACNH_COUNT]; 2],
    cur: usize,
    initialized: bool,
    made_aspect: f32,
    made_scale: f32,
    made_layout: String,
    made_swap: bool,
}

static ACNH_BUTTONS_MU: OnceLock<Mutex<AcnhButtonsCache>> = OnceLock::new();

fn acnh_buttons(mut aspect: f32) -> *const AcnhTouchButton {
    if !(aspect > 0.5 && aspect < 5.0) {
        aspect = K_ACNH_ASPECT;
    }
    let s = touch_size_scale();
    let layout = config_get("input.touch_layout", "");
    let swap = config_get("input.layout", "console") == "xbox";
    let mu = ACNH_BUTTONS_MU.get_or_init(|| {
        Mutex::new(AcnhButtonsCache {
            buf: [K_ACNH; 2],
            cur: 0,
            initialized: false,
            made_aspect: 0.0,
            made_scale: 0.0,
            made_layout: String::new(),
            made_swap: false,
        })
    });
    let mut cache = mu.lock().unwrap_or_else(|e| e.into_inner());
    if cache.initialized
        && (cache.made_aspect - aspect).abs() < 1e-3
        && cache.made_scale == s
        && cache.made_layout == layout
        && cache.made_swap == swap
    {
        return cache.buf[cache.cur].as_ptr();
    }
    let next = if cache.initialized && cache.cur == 0 {
        1
    } else {
        0
    };
    let offsets = parse_layout(&layout);
    for k in 0..K_ACNH_COUNT {
        let mut b = fit_acnh(K_ACNH[k], aspect, s);
        for o in &offsets {
            if o.group == K_ACNH_GROUPS[k] {
                let mut dx = o.dx;
                let mut dy = o.dy;
                clamp_offset(K_ACNH_GROUPS[k], aspect, s, &mut dx, &mut dy);
                b.x += dx / aspect;
                b.y += dy;
            }
        }
        if swap && (b.button == K_A || b.button == K_B) {
            b.button = if b.button == K_A { K_B } else { K_A };
        }
        cache.buf[next][k] = b;
    }
    cache.made_aspect = aspect;
    cache.made_scale = s;
    cache.made_layout = layout;
    cache.made_swap = swap;
    cache.initialized = true;
    cache.cur = next;
    G_FACE_SWAPPED.store(swap, Ordering::Relaxed);
    cache.buf[next].as_ptr()
}

fn wonder_buttons() -> &'static Vec<AcnhTouchButton> {
    static WONDER: OnceLock<Vec<AcnhTouchButton>> = OnceLock::new();
    WONDER.get_or_init(|| {
        let mut out = K_WONDER.to_vec();
        if let Ok(all) = std::env::var("NX_TOUCH_LAYOUT") {
            for item in all.split(';') {
                if item.is_empty() {
                    continue;
                }
                let Some((name, coords)) = item.split_once(':') else {
                    continue;
                };
                let mut parts = coords.split(',');
                let (Some(xs), Some(ys), Some(rs)) = (parts.next(), parts.next(), parts.next())
                else {
                    continue;
                };
                let (Ok(x), Ok(y), Ok(r)) = (
                    xs.trim().parse::<f32>(),
                    ys.trim().parse::<f32>(),
                    rs.trim().parse::<f32>(),
                ) else {
                    continue;
                };
                for (k, b) in out.iter_mut().enumerate() {
                    if name == K_WONDER_NAMES[k]
                        && (0.0..=1.0).contains(&x)
                        && (0.0..=1.0).contains(&y)
                        && r > 0.01
                        && r < 0.3
                    {
                        b.x = x;
                        b.y = y;
                        b.r = r;
                    }
                }
            }
        }
        let scale = env_int("NX_TOUCH_SIZE", 100).clamp(50, 160) as f32 / 100.0;
        for b in &mut out {
            b.r *= scale;
        }
        out
    })
}

fn current_game_kind() -> (bool, bool) {
    static KIND: OnceLock<(bool, bool)> = OnceLock::new();
    *KIND.get_or_init(|| {
        let id = host_game_id();
        (id == "wonder", id == "acnh")
    })
}

// ---- Public extern "C" API ----

/// Installs the host callback table from `runtime/src/input_wrapper.cpp`.
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_set_host(host: *const AcnhInputHost) {
    let new_host = if host.is_null() {
        AcnhInputHost::default()
    } else {
        // SAFETY: Caller passes a valid pointer to `AcnhInputHost`.
        unsafe { *host }
    };
    let mu = HOST.get_or_init(|| Mutex::new(new_host));
    if let Ok(mut guard) = mu.lock() {
        *guard = new_host;
    }
}

/// Fills an `NpadFullKeyState` / `NpadJoyDualState` / `NpadHandheldState` (0x28 bytes).
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_get_npad_state(out: *mut u8, id: u32) {
    get_npad_state_inner(out, id);
}

/// Fills up to `count` `NpadFullKeyState` entries (0x28 bytes each), newest first.
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_get_npad_states_full_key(
    out: *mut u8,
    n: i32,
    id: u32,
    lr: u64,
) -> u64 {
    let calls = FULL_KEY_CALLS.fetch_add(1, Ordering::Relaxed) + 1;
    if calls.is_multiple_of(3000) {
        let desc = host_describe(lr);
        host_log(&format!("hid: {calls} full-key reads (lr {desc})"));
    }
    if n < 1 || out.is_null() {
        return 0;
    }
    let mu = HISTORIES_MU.get_or_init(|| {
        Mutex::new([
            VecDeque::new(),
            VecDeque::new(),
            VecDeque::new(),
            VecDeque::new(),
        ])
    });
    let mut histories = mu.lock().unwrap_or_else(|e| e.into_inner());
    get_npad_state_inner(out, id);
    if (id as usize) >= K_MAX_PLAYERS || !connected(id) {
        if (id as usize) < K_MAX_PLAYERS {
            histories[id as usize].clear();
        }
        return 1;
    }
    let mut now = [0u8; 0x28];
    // SAFETY: `out` is non-null and holds 0x28 bytes written by `get_npad_state_inner`.
    unsafe {
        std::ptr::copy_nonoverlapping(out, now.as_mut_ptr(), 0x28);
    }
    let history = &mut histories[id as usize];
    history.push_front(now);
    if history.len() > 16 {
        history.pop_back();
    }
    let count = (n as usize).min(history.len());
    for (k, entry) in history.iter().enumerate().take(count).skip(1) {
        // SAFETY: `out` has room for `count` 0x28-byte entries.
        unsafe {
            std::ptr::copy_nonoverlapping(entry.as_ptr(), out.add(0x28 * k), 0x28);
        }
    }
    count as u64
}

/// Fills one disconnected `Npad*State` entry (0x28 bytes) for non-full-key styles.
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_get_npad_states_other(out: *mut u8, n: i32, id: u32) -> u64 {
    let cur_name = host_hle_current_name();
    {
        let mu = SEEN_OTHER_READS.get_or_init(|| Mutex::new(HashSet::new()));
        let mut seen = mu.lock().unwrap_or_else(|e| e.into_inner());
        if seen.insert((id, cur_name.clone())) {
            host_log(&format!(
                "hid: reads id {id:#x} via {cur_name} (not connected)"
            ));
        }
    }
    if n < 1 || out.is_null() {
        return 0;
    }
    let sampling = G_SAMPLING.load(Ordering::Relaxed) as i64;
    // SAFETY: `out` is non-null and points to 0x28 bytes.
    unsafe {
        std::ptr::write_bytes(out, 0, 0x28);
        write_i64(out, 0, sampling);
    }
    1
}

/// Fills one disconnected `MouseState` entry (0x28 bytes).
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_get_mouse_states(out: *mut u8, n: i32) -> u64 {
    if n < 1 || out.is_null() {
        return 0;
    }
    let sampling = G_SAMPLING.load(Ordering::Relaxed) as i64;
    // SAFETY: `out` is non-null and points to 0x28 bytes.
    unsafe {
        std::ptr::write_bytes(out, 0, 0x28);
        write_i64(out, 0, sampling);
    }
    1
}

/// Zeroes a `DebugPadState` (0x20 bytes).
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_get_debug_pad_state(out: *mut u8) {
    if out.is_null() {
        return;
    }
    // SAFETY: `out` is non-null and points to 0x20 bytes.
    unsafe {
        std::ptr::write_bytes(out, 0, 0x20);
    }
}

/// Fills a `KeyboardState` (0x30 bytes).
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_get_keyboard_state(out: *mut u8) {
    if out.is_null() {
        return;
    }
    let kbd = kbd_lock().lock().unwrap_or_else(|e| e.into_inner());
    let sampling = G_SAMPLING.load(Ordering::Relaxed) as i64;
    // SAFETY: `out` is non-null and points to 0x30 bytes.
    unsafe {
        write_i64(out, 0, sampling);
        write_u32(out, 8, kbd.mods as u32);
        write_u32(out, 12, 1);
        std::ptr::copy_nonoverlapping(kbd.keys.as_ptr(), out.add(16), 32);
    }
}

/// Records that the game called `nn::hid::GetKeyCode`.
#[no_mangle]
pub extern "C" fn acnh_input_note_keycode() {
    G_KEYCODE_MS.store(now_ms(), Ordering::Relaxed);
    if !G_TYPING_KNOWN.load(Ordering::Relaxed) && !G_TYPING.swap(true, Ordering::Relaxed) {
        host_log("hid: the game types with the keyboard: its keys no longer drive the pad");
    }
}

/// Returns `kStyleFullKey` (`1`) when `id` is connected, `0` otherwise.
#[no_mangle]
pub extern "C" fn acnh_input_get_npad_style_set(id: u32) -> u64 {
    if STYLE_SET_LOGGED.fetch_add(1, Ordering::Relaxed) < 4 {
        host_log(&format!("hid: GetNpadStyleSet id {id:#x}"));
    }
    if connected(id) {
        u64::from(K_STYLE_FULL_KEY)
    } else {
        0
    }
}

/// Writes player 1's vibration device handle when `id == 0` and `count > 0`.
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_get_vibration_device_handles(
    out: *mut u8,
    count: i32,
    id: u32,
) -> u64 {
    if id == 0 && !out.is_null() && count > 0 {
        // SAFETY: `out` is non-null.
        unsafe {
            write_u32(out, 0, 1);
        }
        return 1;
    }
    0
}

/// Fills `VibrationDeviceInfo` (LRA, Left).
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_get_vibration_device_info(out: *mut u8) {
    if out.is_null() {
        return;
    }
    // SAFETY: `out` is non-null and holds 8 bytes.
    unsafe {
        write_u32(out, 0, 1);
        write_u32(out, 4, 1);
    }
}

/// Forwards rumble to the host when `input.rumble` is enabled.
#[no_mangle]
pub extern "C" fn acnh_input_send_vibration_value(amp_low: f32, amp_high: f32) {
    if config_get_bool("input.rumble", true) {
        if amp_low > 0.001 || amp_high > 0.001 {
            host_rumble(amp_low, amp_high, 60);
        } else {
            host_rumble(0.0, 0.0, 0);
        }
    }
}

/// Fills `ControllerSupportResultInfo` with the number of connected players.
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_show_controller_support(out: *mut u8) {
    let mut players: i8 = 0;
    for id in 0..K_MAX_PLAYERS as u32 {
        if connected(id) {
            players += 1;
        }
    }
    host_log(&format!("hid: controller applet: {players} player(s)"));
    if !out.is_null() {
        // SAFETY: `out` is non-null and holds 8 bytes.
        unsafe {
            write_i8(out, 0, players);
            write_u32(out, 4, 0);
        }
    }
}

/// Fills `TouchScreenState<1>`.
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_get_touch_screen_state1(out: *mut u8, lr: u64) {
    if std::env::var_os("NX_TRACE_TOUCH").is_some()
        && TRACE_TOUCH_1.fetch_add(1, Ordering::Relaxed) < 3
    {
        let desc = host_describe(lr);
        host_log(&format!("hid: GetTouchScreenState<1> from {desc}"));
    }
    if out.is_null() {
        return;
    }
    // SAFETY: `out` is non-null and points to `TouchScreenState<1>`.
    unsafe {
        fill_touch(out, 0);
    }
}

/// Fills `TouchScreenState<16>` (returns 1 on success, 0 when `count < 1`).
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_get_touch_screen_states16(
    out: *mut u8,
    count: i32,
    lr: u64,
) -> u64 {
    if std::env::var_os("NX_TRACE_TOUCH").is_some()
        && TRACE_TOUCH_16.fetch_add(1, Ordering::Relaxed) < 3
    {
        let desc = host_describe(lr);
        host_log(&format!(
            "hid: GetTouchScreenStates<16>(count {count}) from {desc}"
        ));
    }
    if count < 1 || out.is_null() {
        return 0;
    }
    let y_bias = host_touch_y_bias();
    // SAFETY: `out` is non-null and points to `TouchScreenState<16>`.
    unsafe {
        std::ptr::write_bytes(out, 0, 0x10 + 0x28 * 16);
        fill_touch(out, y_bias);
    }
    1
}

/// Fills `NpadControllerColor` (dark grey body, black buttons).
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_get_npad_controller_color(p0: *mut u8, p1: *mut u8) {
    for p in [p0, p1] {
        if !p.is_null() {
            // SAFETY: `p` is non-null and holds 8 bytes.
            unsafe {
                write_u32(p, 0, 0xFF2D_2D2D);
                write_u32(p, 4, 0xFF0F_0F0F);
            }
        }
    }
}

/// Returns `true` and the hovered position when a new hover event occurred.
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_mouse_take_hover(out_x: *mut u32, out_y: *mut u32) -> bool {
    let frame = host_frame();
    let mut ti = touch_lock().lock().unwrap_or_else(|e| e.into_inner());
    let scripted_until = ti.hover.scripted_until;
    let moves: Vec<Scripted> = {
        let st = script_lock().lock().unwrap_or_else(|e| e.into_inner());
        st.entries
            .iter()
            .copied()
            .filter(|s| s.move_mouse && s.frame <= frame && s.frame >= scripted_until)
            .collect()
    };
    for s in moves {
        if s.frame >= ti.hover.scripted_until {
            ti.hover.x = s.tx;
            ti.hover.y = s.ty;
            ti.hover.moves += 1;
            ti.hover.scripted_until = s.frame + 1;
        }
    }
    if ti.hover.moves == ti.hover.taken {
        return false;
    }
    ti.hover.taken = ti.hover.moves;
    if !out_x.is_null() {
        // SAFETY: Caller passes valid output pointers.
        unsafe { *out_x = ti.hover.x };
    }
    if !out_y.is_null() {
        // SAFETY: Caller passes valid output pointers.
        unsafe { *out_y = ti.hover.y };
    }
    true
}

/// Returns `true` and the tap coordinates/frame when a touch recently ended.
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_touch_take_tap(
    out_x: *mut u32,
    out_y: *mut u32,
    out_frame: *mut u64,
) -> bool {
    let mu = TAP_MU.get_or_init(|| Mutex::new(TapState::default()));
    let mut tap = mu.lock().unwrap_or_else(|e| e.into_inner());
    if !tap.pending {
        return false;
    }
    tap.pending = false;
    // SAFETY: Caller passes valid output pointers (or null, guarded).
    unsafe {
        if !out_x.is_null() {
            *out_x = tap.x;
        }
        if !out_y.is_null() {
            *out_y = tap.y;
        }
        if !out_frame.is_null() {
            *out_frame = tap.frame;
        }
    }
    true
}

/// Holds `buttons` down for `frames` frames starting on the next frame (or 2 frames after any active hold).
#[no_mangle]
pub extern "C" fn acnh_input_hid_hold_buttons(buttons: u64, frames: u32) {
    let mut from = host_frame() + 1;
    let until = G_HELD_UNTIL.load(Ordering::Relaxed);
    if until + 2 > from {
        from = until + 2;
    }
    G_HELD_UNTIL.store(0, Ordering::Relaxed);
    G_HELD_BUTTONS.store(buttons, Ordering::Relaxed);
    G_HELD_FROM.store(from, Ordering::Relaxed);
    G_HELD_UNTIL.store(from + u64::from(frames), Ordering::Relaxed);
}

/// Updates the USB keyboard state (`modifiers` and 32-byte key bitmap).
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_set_keyboard(modifiers: u64, keys: *const u8) {
    if keys.is_null() {
        return;
    }
    // SAFETY: Caller passes a 32-byte array.
    let new_keys = unsafe { std::slice::from_raw_parts(keys, 32) };
    let mut kbd = kbd_lock().lock().unwrap_or_else(|e| e.into_inner());
    let now = now_ms();
    for u in 0..256 {
        let was = (kbd.keys[u / 8] >> (u % 8)) & 1 != 0;
        let is = (new_keys[u / 8] >> (u % 8)) & 1 != 0;
        if is
            && !was
            && key_types(u as i32)
            && G_TYPING.load(Ordering::Relaxed)
            && kbd.probe_ms == 0
        {
            kbd.probe_ms = now;
        }
    }
    if G_TYPING_KNOWN.load(Ordering::Relaxed) {
        kbd.probe_ms = 0;
    }
    if kbd.probe_ms != 0 && G_KEYCODE_MS.load(Ordering::Relaxed) >= kbd.probe_ms {
        kbd.probe_ms = 0;
    }
    if kbd.probe_ms != 0 && now - kbd.probe_ms > K_TYPING_PROBE_MS {
        kbd.probe_ms = 0;
        if G_TYPING.swap(false, Ordering::Relaxed) {
            host_log("hid: the game no longer types with the keyboard: its keys drive the pad");
        }
    }
    kbd.mods = modifiers;
    kbd.keys.copy_from_slice(new_keys);
}

/// Returns `true` while the game is actively typing with the keyboard.
#[no_mangle]
pub extern "C" fn acnh_input_keyboard_typing() -> bool {
    if G_TYPING_KNOWN.load(Ordering::Relaxed)
        && G_TYPING.load(Ordering::Relaxed)
        && host_frame() > G_ENTRY_FRAME.load(Ordering::Relaxed) + 3
    {
        set_text_entry(false);
    }
    G_TYPING.load(Ordering::Relaxed)
}

/// Reports whether the game's text entry UI is currently open.
#[no_mangle]
pub extern "C" fn acnh_input_keyboard_text_entry(open: bool) {
    G_TYPING_KNOWN.store(true, Ordering::Relaxed);
    G_ENTRY_FRAME.store(host_frame(), Ordering::Relaxed);
    set_text_entry(open);
}

/// Returns `true` once `acnh_input_keyboard_text_entry` has been called.
#[no_mangle]
pub extern "C" fn acnh_input_keyboard_state_known() -> bool {
    G_TYPING_KNOWN.load(Ordering::Relaxed)
}

/// Updates mouse touch / hover state from normalized window coordinates `(u, v)`.
#[no_mangle]
pub extern "C" fn acnh_input_set_mouse_touch(down: bool, u: f32, v: f32) {
    let mut ti = touch_lock().lock().unwrap_or_else(|e| e.into_inner());
    let x = (u - ti.box_rect[0]) / ti.box_rect[2];
    let y = (v - ti.box_rect[1]) / ti.box_rect[3];
    let inside = (0.0..1.0).contains(&x) && (0.0..1.0).contains(&y);
    let px = (x * 1280.0) as u32;
    let py = (y * 720.0) as u32;
    if down && inside {
        if !ti.mouse.down {
            ti.mouse_tap = true;
        }
        ti.mouse = TouchState {
            down: true,
            x: px,
            y: py,
        };
    } else {
        ti.mouse.down = false;
        let dx = px as i32 - ti.hover.x as i32;
        let dy = py as i32 - ti.hover.y as i32;
        if inside && dx * dx + dy * dy >= 4 {
            ti.hover.x = px;
            ti.hover.y = py;
            ti.hover.moves += 1;
        }
    }
}

/// Sets the picture box in normalized window coordinates `(x, y, w, h)`.
#[no_mangle]
pub extern "C" fn acnh_input_set_picture_box(x: f32, y: f32, w: f32, h: f32) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let mut ti = touch_lock().lock().unwrap_or_else(|e| e.into_inner());
    ti.box_rect = [x, y, w, h];
}

/// Applies `NX_INPUT_SCRIPT` and/or records `NX_INPUT_RECORD` on `p`.
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_script(p: *mut AcnhPadState) {
    if p.is_null() {
        return;
    }
    let mu = INPUT_SCRIPT_MU.get_or_init(|| {
        let env = std::env::var("NX_INPUT_SCRIPT").ok();
        let recording = std::env::var_os("NX_INPUT_RECORD").is_some();
        let script = env.as_deref().map(parse_input_script).unwrap_or_default();
        Mutex::new(InputScriptRunner {
            has_env: env.is_some(),
            recording,
            start_ns: None,
            script,
            rec_down: [-1.0; 14],
            rec_init: false,
            rec_file: None,
        })
    });
    let mut runner = mu.lock().unwrap_or_else(|e| e.into_inner());
    if !runner.has_env && !runner.recording {
        return;
    }
    let now_ns = host_monotonic_ns();
    let start_ns = *runner.start_ns.get_or_insert(now_ns);
    let t = (now_ns.saturating_sub(start_ns)) as f64 / 1e9;
    // SAFETY: `p` is non-null.
    let pad = unsafe { &mut *p };
    if runner.recording {
        record_input(&mut runner, pad, t);
    }
    if !runner.has_env {
        return;
    }
    for s in &runner.script {
        if t >= s.at && t < s.at + s.hold {
            pad.buttons |= s.bits;
            if s.lx != 0 {
                pad.lx = s.lx;
            }
            if s.ly != 0 {
                pad.ly = s.ly;
            }
        }
    }
}

/// Returns `true` when on-screen touch controls are enabled.
#[no_mangle]
pub extern "C" fn acnh_input_touch_enabled() -> bool {
    static ENV_ON: OnceLock<bool> = OnceLock::new();
    let env_on = *ENV_ON.get_or_init(|| {
        std::env::var("NX_TOUCH_SHOW")
            .map(|e| !e.starts_with('0'))
            .unwrap_or(true)
    });
    env_on && config_get_bool("input.touch_controls", true)
}

/// Returns `true` when `NX_TOUCH_MIRROR=1` is set.
#[no_mangle]
pub extern "C" fn acnh_input_touch_mirrored() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("NX_TOUCH_MIRROR")
            .map(|e| e.starts_with('1'))
            .unwrap_or(false)
    })
}

/// Returns the touch overlay brightness scale relative to 85% baseline.
#[no_mangle]
pub extern "C" fn acnh_input_touch_opacity_scale() -> f32 {
    static ENV: OnceLock<i32> = OnceLock::new();
    let env = *ENV.get_or_init(|| env_int("NX_TOUCH_OPACITY", 0));
    let pct = if env != 0 {
        env
    } else {
        parse_c_int(&config_get("input.touch_opacity", "60")).unwrap_or(0)
    };
    pct.clamp(10, 100) as f32 / 85.0
}

/// Returns the game's touch button array and writes its length to `out_count`.
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_touch_buttons(
    out_count: *mut usize,
    aspect: f32,
) -> *const AcnhTouchButton {
    let (wonder, acnh) = current_game_kind();
    if acnh {
        if !out_count.is_null() {
            // SAFETY: Caller passes a valid output pointer.
            unsafe { *out_count = K_ACNH_COUNT };
        }
        return acnh_buttons(aspect);
    }
    if wonder {
        let v = wonder_buttons();
        if !out_count.is_null() {
            // SAFETY: Caller passes a valid output pointer.
            unsafe { *out_count = v.len() };
        }
        return v.as_ptr();
    }
    if !out_count.is_null() {
        // SAFETY: Caller passes a valid output pointer.
        unsafe { *out_count = 0 };
    }
    std::ptr::null()
}

/// Computes the resting position and radius of the virtual stick.
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_touch_stick_rest(
    mut aspect: f32,
    mirrored: bool,
    out_x: *mut f32,
    out_y: *mut f32,
    out_r: *mut f32,
) {
    let (_, acnh) = current_game_kind();
    let r = 0.12f32;
    if !(aspect > 0.5 && aspect < 5.0) {
        aspect = K_ACNH_ASPECT;
    }
    let mut dx = 0.15 * K_ACNH_ASPECT;
    let mut cy = 0.72f32;
    if acnh {
        let ptr = acnh_buttons(aspect);
        // SAFETY: `acnh_buttons` returns a pointer to `K_ACNH_COUNT` elements.
        let right = unsafe { *ptr.add(K_ACNH_COUNT - 1) };
        dx = right.x * aspect + right.r + r + 0.04;
        cy = (right.y - 0.14).clamp(r, 1.0 - r);
    }
    let x = if mirrored {
        1.0 - dx / aspect
    } else {
        dx / aspect
    };
    // SAFETY: Caller passes valid output pointers (or null, guarded).
    unsafe {
        if !out_x.is_null() {
            *out_x = x;
        }
        if !out_y.is_null() {
            *out_y = cy;
        }
        if !out_r.is_null() {
            *out_r = r;
        }
    }
}

/// Returns the static group name for touch button `index`, or null if unavailable.
#[no_mangle]
pub extern "C" fn acnh_input_touch_group(index: usize) -> *const c_char {
    let (_, acnh) = current_game_kind();
    if acnh && index < K_ACNH_COUNT {
        K_ACNH_GROUP_CSTRS[index]
    } else {
        std::ptr::null()
    }
}

/// Returns the static label string for touch button `index`.
#[no_mangle]
pub extern "C" fn acnh_input_touch_label(index: usize) -> *const c_char {
    if index >= K_ACNH_COUNT {
        return c"".as_ptr();
    }
    if index < 2 && G_FACE_SWAPPED.load(Ordering::Relaxed) {
        return K_ACNH_LABEL_CSTRS[1 - index];
    }
    K_ACNH_LABEL_CSTRS[index]
}

/// Moves `group` by `(dx, dy)` screen heights and persists `input.touch_layout`.
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_touch_layout_move(
    group: *const c_char,
    dx: f32,
    dy: f32,
    mut aspect: f32,
) {
    if group.is_null() {
        return;
    }
    // SAFETY: Caller passes a valid NUL-terminated string.
    let Ok(group_str) = (unsafe { CStr::from_ptr(group) }).to_str() else {
        return;
    };
    if !(aspect > 0.5 && aspect < 5.0) {
        aspect = K_ACNH_ASPECT;
    }
    let mut offsets = parse_layout(&config_get("input.touch_layout", ""));
    let mut found = false;
    for o in &mut offsets {
        if o.group == group_str {
            o.dx += dx;
            o.dy += dy;
            found = true;
        }
    }
    if !found {
        offsets.push(TouchOffset {
            group: group_str.to_string(),
            dx,
            dy,
        });
    }
    let scale = touch_size_scale();
    for o in &mut offsets {
        if o.group == group_str {
            clamp_offset(group_str, aspect, scale, &mut o.dx, &mut o.dy);
        }
    }
    let mut all = String::new();
    for o in &offsets {
        if !all.is_empty() {
            all.push(';');
        }
        all.push_str(&format!("{}:{:.4},{:.4}", o.group, o.dx, o.dy));
    }
    config_set("input.touch_layout", &all);
}

/// Returns the button under normalized coordinates `(x, y)` on a screen of `aspect`, or `0`.
#[no_mangle]
pub extern "C" fn acnh_input_touch_hit(x: f32, y: f32, aspect: f32) -> u64 {
    let mut n = 0usize;
    // SAFETY: `n` is a live local the callee writes to; the returned table is static.
    let ptr = unsafe { acnh_input_touch_buttons(&mut n as *mut usize, aspect) };
    if ptr.is_null() || n == 0 {
        return 0;
    }
    // SAFETY: `ptr` points to `n` `AcnhTouchButton` elements.
    let buttons = unsafe { std::slice::from_raw_parts(ptr, n) };
    let mut best = 0u64;
    let mut best_d = 1e9f32;
    for b in buttons {
        let d = ((x - b.x) * aspect).hypot(y - b.y) / b.r;
        if d < 1.35 && d < best_d {
            best = b.button;
            best_d = d;
        }
    }
    best
}

/// Fills `out` with up to `max` demo overlay circles when `NX_OVERLAY_DEMO` is set.
#[no_mangle]
///
/// # Safety
///
/// All non-null pointers passed from C++ must be valid for the expected size.
pub unsafe extern "C" fn acnh_input_touch_overlay(out: *mut AcnhOverlayShape, max: usize) -> usize {
    static DEMO: OnceLock<bool> = OnceLock::new();
    if !*DEMO.get_or_init(|| std::env::var_os("NX_OVERLAY_DEMO").is_some()) {
        return 0;
    }
    if out.is_null() || max == 0 {
        return 0;
    }
    let mut n = 0usize;
    let ptr = acnh_input_touch_buttons(&mut n as *mut usize, K_ACNH_ASPECT);
    let mut k = 0usize;
    if !ptr.is_null() && n > 0 {
        // SAFETY: `ptr` points to `n` `AcnhTouchButton` elements and `out` holds `max`.
        let buttons = unsafe { std::slice::from_raw_parts(ptr, n) };
        while k < n && k < max {
            let b = buttons[k];
            // SAFETY: `k < max`.
            unsafe {
                *out.add(k) = AcnhOverlayShape {
                    x: b.x,
                    y: b.y,
                    r: b.r,
                    alpha: 0.22,
                    label: 0,
                };
            }
            k += 1;
        }
    }
    let (mut sx, mut sy, mut sr) = (0.0f32, 0.0f32, 0.0f32);
    acnh_input_touch_stick_rest(
        K_ACNH_ASPECT,
        false,
        &mut sx as *mut f32,
        &mut sy as *mut f32,
        &mut sr as *mut f32,
    );
    if k < max {
        // SAFETY: `k < max`.
        unsafe {
            *out.add(k) = AcnhOverlayShape {
                x: sx,
                y: sy,
                r: sr,
                alpha: 0.10,
                label: 0,
            };
        }
        k += 1;
    }
    k
}

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
