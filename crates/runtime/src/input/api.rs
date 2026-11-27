//! Split from the old `input.rs` (issue #356).

#[allow(unused_imports)]
use super::*;

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
    crate::ffi::set_log(new_host.log);
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
