//! Split from the old `input.rs` (issue #356).

#[allow(unused_imports)]
use super::*;

// ---- Input script (input_script.cpp) ----

pub(crate) struct InputScriptPress {
    pub(crate) at: f64,
    pub(crate) hold: f64,
    pub(crate) bits: u64,
    pub(crate) lx: i32,
    pub(crate) ly: i32,
}

pub(crate) const SCRIPT_NAMES: [(&str, u64, i32, i32); 14] = [
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

pub(crate) fn parse_input_script(s: &str) -> Vec<InputScriptPress> {
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

pub(crate) struct InputScriptRunner {
    pub(crate) has_env: bool,
    pub(crate) recording: bool,
    pub(crate) start_ns: Option<u64>,
    pub(crate) script: Vec<InputScriptPress>,
    pub(crate) rec_down: [f64; 14],
    pub(crate) rec_init: bool,
    pub(crate) rec_file: Option<File>,
}

pub(crate) static INPUT_SCRIPT_MU: OnceLock<Mutex<InputScriptRunner>> = OnceLock::new();

pub(crate) fn record_input(runner: &mut InputScriptRunner, p: &AcnhPadState, t: f64) {
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

pub(crate) const K_ACNH_ASPECT: f32 = 16.0 / 9.0;
pub(crate) const K_ACNH_COUNT: usize = 15;

pub(crate) const K_WONDER: [AcnhTouchButton; 9] = [
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

pub(crate) const K_WONDER_NAMES: [&str; 9] = ["A", "B", "Y", "X", "ZR", "R", "L", "PLUS", "MINUS"];

pub(crate) const K_ACNH: [AcnhTouchButton; K_ACNH_COUNT] = [
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

pub(crate) const K_ACNH_GROUPS: [&str; K_ACNH_COUNT] = [
    "FACE", "FACE", "FACE", "FACE", "R", "ZR", "L", "ZL", "PLUS", "MINUS", "GEAR", "DPAD", "DPAD",
    "DPAD", "DPAD",
];

pub(crate) const K_ACNH_GROUP_CSTRS: [*const c_char; K_ACNH_COUNT] = [
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

pub(crate) const K_ACNH_LABEL_CSTRS: [*const c_char; K_ACNH_COUNT] = [
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

pub(crate) fn touch_size_scale() -> f32 {
    static ENV: OnceLock<i32> = OnceLock::new();
    let env = *ENV.get_or_init(|| env_int("NX_TOUCH_SIZE", 0));
    let pct = if env != 0 {
        env
    } else {
        config_get_int("input.touch_size", 100)
    };
    pct.clamp(50, 150) as f32 / 100.0
}

pub(crate) fn fit_acnh(b: AcnhTouchButton, aspect: f32, s: f32) -> AcnhTouchButton {
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
pub(crate) struct TouchOffset {
    pub(crate) group: String,
    pub(crate) dx: f32,
    pub(crate) dy: f32,
}

pub(crate) fn parse_layout(all: &str) -> Vec<TouchOffset> {
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

pub(crate) fn clamp_offset(group: &str, aspect: f32, s: f32, dx: &mut f32, dy: &mut f32) {
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

pub(crate) static G_FACE_SWAPPED: AtomicBool = AtomicBool::new(false);

pub(crate) struct AcnhButtonsCache {
    pub(crate) buf: [[AcnhTouchButton; K_ACNH_COUNT]; 2],
    pub(crate) cur: usize,
    pub(crate) initialized: bool,
    pub(crate) made_aspect: f32,
    pub(crate) made_scale: f32,
    pub(crate) made_layout: String,
    pub(crate) made_swap: bool,
}

pub(crate) static ACNH_BUTTONS_MU: OnceLock<Mutex<AcnhButtonsCache>> = OnceLock::new();

pub(crate) fn acnh_buttons(mut aspect: f32) -> *const AcnhTouchButton {
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

pub(crate) fn wonder_buttons() -> &'static Vec<AcnhTouchButton> {
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

pub(crate) fn current_game_kind() -> (bool, bool) {
    static KIND: OnceLock<(bool, bool)> = OnceLock::new();
    *KIND.get_or_init(|| {
        let id = host_game_id();
        (id == "wonder", id == "acnh")
    })
}
