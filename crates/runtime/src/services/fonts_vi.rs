//! Split from the old `services.rs` (issue #356).

#[allow(unused_imports)]
use super::*;

// ---- 5. Shared System Fonts (hle_pl.cpp) ----------------------------------------------------------------------------

#[derive(Clone, Copy, Default)]
pub(crate) struct SharedFont {
    pub(crate) addr: u64,
    pub(crate) size: u64,
}

pub(crate) static SHARED_FONTS: Mutex<[SharedFont; 6]> =
    Mutex::new([SharedFont { addr: 0, size: 0 }; 6]);

pub(crate) fn host_game_root() -> String {
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
pub(crate) fn linux_font_candidates() -> Vec<String> {
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

pub(crate) fn ensure_shared_font(font_type: u32) -> SharedFont {
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

pub(crate) static VI_VBLANKS: AtomicU64 = AtomicU64::new(0);
pub(crate) static VI_PER_TICK: AtomicI32 = AtomicI32::new(0);
pub(crate) static VI_TICK_PERIOD: AtomicU64 = AtomicU64::new(0);
pub(crate) static VI_PRESENT_INTERVAL: AtomicI32 = AtomicI32::new(2);
pub(crate) static VI_LAST_DUE: AtomicU64 = AtomicU64::new(0);
pub(crate) static VI_VSYNC_EVENT: AtomicU32 = AtomicU32::new(0);
pub(crate) static VI_VSYNC_STARTED: AtomicBool = AtomicBool::new(false);
pub(crate) static VI_VB_SYNC: (Mutex<()>, Condvar) = (Mutex::new(()), Condvar::new());

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

pub(crate) fn run_real_vsync() {
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
