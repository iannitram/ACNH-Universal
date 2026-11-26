//! Split from the old `input.rs` (issue #356).

#[allow(unused_imports)]
use super::*;

// ---- Scripted & live input (hle_hid.cpp) ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Scripted {
    pub(crate) frame: u64,
    pub(crate) buttons: u64,
    pub(crate) lx: i32,
    pub(crate) ly: i32,
    pub(crate) touch: bool,
    pub(crate) move_mouse: bool,
    pub(crate) tx: u32,
    pub(crate) ty: u32,
}

pub(crate) fn parse_script(s: &str, base: u64) -> Vec<Scripted> {
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

pub(crate) struct ScriptState {
    pub(crate) entries: Vec<Scripted>,
    pub(crate) next_check: u64,
    pub(crate) last_trace_buttons: u64,
}

pub(crate) static SCRIPT_MU: OnceLock<Mutex<ScriptState>> = OnceLock::new();

pub(crate) fn script_lock() -> &'static Mutex<ScriptState> {
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

pub(crate) fn poll_live(state: &mut ScriptState) {
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

pub(crate) static G_SAMPLING: AtomicU64 = AtomicU64::new(0);
pub(crate) static G_HELD_BUTTONS: AtomicU64 = AtomicU64::new(0);
pub(crate) static G_HELD_FROM: AtomicU64 = AtomicU64::new(0);
pub(crate) static G_HELD_UNTIL: AtomicU64 = AtomicU64::new(0);

pub(crate) fn stick_buttons(p: &mut AcnhPadState) {
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

pub(crate) fn read_pad_with_script() -> AcnhPadState {
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

pub(crate) fn read_player(id: u32) -> Option<AcnhPadState> {
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

pub(crate) static CONNECTED_WAS: OnceLock<Mutex<[bool; K_MAX_PLAYERS]>> = OnceLock::new();

pub(crate) fn connected(id: u32) -> bool {
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

pub(crate) static SEEN_NPAD_READS: OnceLock<Mutex<HashSet<(u32, String)>>> = OnceLock::new();
pub(crate) static FIRST_PRESSED: [AtomicBool; K_MAX_PLAYERS] = [
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
];

pub(crate) fn get_npad_state_inner(out: *mut u8, id: u32) {
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

pub(crate) static FULL_KEY_CALLS: AtomicU64 = AtomicU64::new(0);
pub(crate) static HISTORIES_MU: OnceLock<Mutex<[VecDeque<[u8; 0x28]>; K_MAX_PLAYERS]>> =
    OnceLock::new();
pub(crate) static SEEN_OTHER_READS: OnceLock<Mutex<HashSet<(u32, String)>>> = OnceLock::new();
pub(crate) static STYLE_SET_LOGGED: AtomicI32 = AtomicI32::new(0);

// ---- Keyboard state ----

pub(crate) struct KbdState {
    pub(crate) mods: u64,
    pub(crate) keys: [u8; 32],
    pub(crate) probe_ms: i64,
}

pub(crate) static KBD_MU: OnceLock<Mutex<KbdState>> = OnceLock::new();

pub(crate) fn kbd_lock() -> &'static Mutex<KbdState> {
    KBD_MU.get_or_init(|| {
        Mutex::new(KbdState {
            mods: 0,
            keys: [0; 32],
            probe_ms: 0,
        })
    })
}

pub(crate) static G_TYPING: AtomicBool = AtomicBool::new(false);
pub(crate) static G_TYPING_KNOWN: AtomicBool = AtomicBool::new(false);
pub(crate) static G_KEYCODE_MS: AtomicI64 = AtomicI64::new(0);
pub(crate) static G_ENTRY_FRAME: AtomicU64 = AtomicU64::new(0);

pub(crate) fn set_text_entry(open: bool) {
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
pub(crate) struct TouchState {
    pub(crate) down: bool,
    pub(crate) x: u32,
    pub(crate) y: u32,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct HoverState {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) moves: u64,
    pub(crate) scripted_until: u64,
    pub(crate) taken: u64,
}

pub(crate) struct TouchInputState {
    pub(crate) box_rect: [f32; 4],
    pub(crate) mouse: TouchState,
    pub(crate) mouse_tap: bool,
    pub(crate) hover: HoverState,
}

pub(crate) static TOUCH_MU: OnceLock<Mutex<TouchInputState>> = OnceLock::new();

pub(crate) fn touch_lock() -> &'static Mutex<TouchInputState> {
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
pub(crate) struct TouchSample {
    pub(crate) sampling: u64,
    pub(crate) attributes: u32,
    pub(crate) listed: bool,
    pub(crate) x: u32,
    pub(crate) y: u32,
}

pub(crate) struct TouchSampleState {
    pub(crate) frame: u64,
    pub(crate) cur: TouchSample,
}

pub(crate) static TOUCH_SAMPLE_MU: OnceLock<Mutex<TouchSampleState>> = OnceLock::new();

#[derive(Clone, Copy, Default)]
pub(crate) struct TapState {
    pub(crate) pending: bool,
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) frame: u64,
}

pub(crate) static TAP_MU: OnceLock<Mutex<TapState>> = OnceLock::new();

pub(crate) fn current_touch() -> TouchState {
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

pub(crate) fn touch_sample() -> TouchSample {
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

pub(crate) unsafe fn fill_touch(out: *mut u8, y_bias: u32) {
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

pub(crate) static TRACE_TOUCH_1: AtomicI32 = AtomicI32::new(0);
pub(crate) static TRACE_TOUCH_16: AtomicI32 = AtomicI32::new(0);
