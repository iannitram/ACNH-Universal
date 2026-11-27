//! The thermal governor and frame timing (`thermal.cpp`).

use super::*;

// ---- Thermal Governor & Frame Timing (`thermal.cpp`) -------------------------------------------

#[derive(Clone, Copy)]
pub(super) struct ThermalModeSpec {
    pub(super) gpu_target: f64,
    pub(super) cpu_target: f64,
    pub(super) late_max: f64,
    pub(super) gpu_floor_mhz: i64,
    pub(super) cpu_floor_mhz: i64,
    pub(super) headroom_hot: f32,
    pub(super) temp_hot: i32,
    pub(super) max_drop: i32,
    pub(super) adpf: f64,
    pub(super) battery_hot: i32,
    pub(super) battery_ssrm: i32,
}

pub(super) const MODES: [ThermalModeSpec; 3] = [
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

pub(super) const K_CPU_CAPS: [i64; 10] =
    [2400, 2100, 1800, 1600, 1500, 1400, 1300, 1200, 1100, 1000];
pub(super) const K_CPU_START_LEVEL: usize = 3;
pub(super) const K_BINS: usize = 80;

pub(super) static THERMAL_PERIOD_NS: AtomicI64 = AtomicI64::new(0);
pub(super) static THERMAL_REPLAY_NS: AtomicI64 = AtomicI64::new(0);
pub(super) static THERMAL_MAIN_NS: AtomicI64 = AtomicI64::new(0);
pub(super) static THERMAL_FRAMES: AtomicU32 = AtomicU32::new(0);
pub(super) static THERMAL_LATE: AtomicU32 = AtomicU32::new(0);
pub(super) static THERMAL_DYN_SCALE_BITS: AtomicU32 = AtomicU32::new(f32::to_bits(1.0));
pub(super) static THERMAL_BINS: [AtomicU32; K_BINS] = [const { AtomicU32::new(0) }; K_BINS];

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
pub(super) struct CpuPolicy {
    pub(super) id: i32,
    pub(super) path: String,
    pub(super) restore: i64,
    pub(super) written: i64,
    pub(super) read: i64,
    pub(super) freqs: Vec<i64>,
    pub(super) hw_max: i64,
    pub(super) not_kept: i32,
}

/// Stateful thermal governor for Android rooted clock control + dynamic resolution.
pub struct AcnhThermalGovernor {
    pub(super) gpu_path: String,
    pub(super) gpu_in_mhz: bool,
    pub(super) steps: Vec<i64>,
    pub(super) gpu_usual: i64,
    pub(super) gpu_sys_seen: bool,
    pub(super) gpu_restore: i64,
    pub(super) gpu_written: i64,
    pub(super) gpu_read: i64,
    pub(super) ssrm_low: i64,
    pub(super) gpu_idx: usize,
    pub(super) pol: Vec<CpuPolicy>,
    pub(super) use_limit: bool,
    pub(super) limit_restore: i64,
    pub(super) limit_written: i64,
    pub(super) limit_read: i64,
    pub(super) cpu_level: usize,
    pub(super) zones: Vec<String>,
    pub(super) temp: i32,
    pub(super) busy_sum: f64,
    pub(super) clk_sum: f64,
    pub(super) busy_n: i32,
    pub(super) drop: i32,
    pub(super) hot_n: i32,
    pub(super) cool_n: i32,
    pub(super) gpu_hold: i32,
    pub(super) cpu_hold: i32,
    pub(super) gpu_votes: i32,
    pub(super) cpu_votes: i32,
    pub(super) turn_gpu: bool,
    pub(super) reads_cmd: String,
    pub(super) restore_sent: String,
    pub(super) busy: f64,
    pub(super) clk: f64,
    pub(super) cpu_p90: f64,
    pub(super) late: f64,
    pub(super) battery: i32,
    pub(super) frames: u32,
    pub(super) late_n: u32,
    pub(super) decided_ns: u64,
    pub(super) dyn_starved: u32,
    pub(super) dyn_roomy: u32,
    pub(super) dyn_changed_ns: u64,
    pub(super) headroom: f32,
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

    pub(super) fn top(&self) -> usize {
        self.steps.len().saturating_sub(1)
    }

    pub(super) fn floor_idx(&self, m: &ThermalModeSpec) -> usize {
        let mut k = 0;
        let t = self.top();
        while k < t && self.steps[k] < m.gpu_floor_mhz {
            k += 1;
        }
        k
    }

    pub(super) fn max_level(&self, m: &ThermalModeSpec) -> usize {
        if self.pol.is_empty() {
            return 0;
        }
        let mut l = 0;
        while l < K_CPU_CAPS.len() && K_CPU_CAPS[l] >= m.cpu_floor_mhz {
            l += 1;
        }
        l
    }

    pub(super) fn gpu_value(&self, mhz: i64) -> i64 {
        if self.gpu_in_mhz {
            mhz
        } else {
            mhz * 1_000_000
        }
    }

    pub(super) fn gpu_mhz(&self, value: i64) -> i64 {
        if self.gpu_in_mhz {
            value
        } else {
            value / 1_000_000
        }
    }

    pub(super) fn policy_cap(p: &CpuPolicy, level: usize) -> i64 {
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

    pub(super) fn cpu_cap_mhz(&self, level: usize) -> i64 {
        self.pol
            .iter()
            .map(|p| Self::policy_cap(p, level))
            .max()
            .unwrap_or(0)
            / 1000
    }

    pub(super) fn cpu_step(&self, level: usize, dir: isize, lo: usize, hi: usize) -> usize {
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
