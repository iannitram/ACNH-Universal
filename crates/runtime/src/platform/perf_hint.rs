//! Performance hints: ADPF, KGSL and the clock keeper (`perf_hint.cpp`).

use super::*;

// ---- Performance Hints / ADPF / Clock Keeper (`perf_hint.cpp`) ---------------------------------

pub(super) struct AdpfState {
    pub(super) tids: Vec<i32>,
    pub(super) session: usize,
    pub(super) dirty: bool,
    pub(super) target: i64,
    pub(super) target_at: u64,
    pub(super) preferred_rate_ns: i64,
    pub(super) last_report_at: u64,
    pub(super) pending_max_ns: i64,
    pub(super) efficiency: i32,
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

pub(super) static ADPF_MU: OnceLock<Mutex<AdpfState>> = OnceLock::new();
pub(super) static LAST_FRAME_NS: AtomicU64 = AtomicU64::new(0);

pub(super) fn adpf_mu() -> &'static Mutex<AdpfState> {
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
