//! Island clock, `nn::time` HLE helpers, and `time:u`/`time:a`/`time:s` shared-memory/IPC helpers,
//! ported from `runtime/src/game_clock.cpp`, `runtime/src/hle_time.cpp`, and `runtime/src/svc_time.cpp`.
//!
//! `runtime/src/time_wrapper.cpp` provides the thin C++ host callbacks (online state, monotonic
//! clock, steady ticks, host localtime/mktime) and registers the `NX_HLE` / `ServiceRegistrar`
//! entries against the guest CPU/IPC dispatchers.

use std::ffi::c_char;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config;

/// Steady-clock source UUID (low 64 bits), matching `svc_time.cpp`.
pub const SOURCE_UUID_LO: u64 = 0x4E58_5254_4352_4543;
/// Steady-clock source UUID (high 64 bits), matching `svc_time.cpp`.
pub const SOURCE_UUID_HI: u64 = 0x4841_4445_5354_494D;
/// Switch steady clock frequency in Hz (`19.2 MHz`).
pub const STEADY_CLOCK_HZ: u64 = 19_200_000;
/// Size of the `time` shared-memory page (`0x1000`).
pub const TIME_SHM_SIZE: usize = 0x1000;

/// Horizon `nn::time::CalendarTime` (8 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AcnhCalendarTime {
    pub year: i16,
    pub month: i8,
    pub day: i8,
    pub hour: i8,
    pub minute: i8,
    pub second: i8,
    pub _pad: u8,
}

/// Horizon `nn::time::CalendarAdditionalInfo` (0x18 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AcnhCalendarAdditionalInfo {
    pub day_of_week: i32,
    pub day_of_year: i32,
    pub tz_name: [u8; 8],
    pub is_dst: i32,
    pub utc_offset: i32,
}

/// Host callbacks provided by `runtime/src/time_wrapper.cpp`.
///
/// When unset (e.g. in standalone Rust unit tests), `online_services_active` defaults to `false`,
/// `monotonic_ns` uses a process-local `Instant`, `steady_ticks` derives from `monotonic_ns`,
/// `local_from_posix` / `posix_from_local` fall back to pure-Rust UTC calendar math, and
/// `utc_offset_west` defaults to `0`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct AcnhTimeHost {
    pub online_services_active: Option<extern "C" fn() -> bool>,
    pub monotonic_ns: Option<extern "C" fn() -> u64>,
    pub steady_ticks: Option<extern "C" fn() -> u64>,
    pub local_from_posix: Option<
        extern "C" fn(
            posix: i64,
            out_cal: *mut AcnhCalendarTime,
            out_add: *mut AcnhCalendarAdditionalInfo,
        ) -> bool,
    >,
    pub posix_from_local:
        Option<extern "C" fn(cal: *const AcnhCalendarTime, out_posix: *mut i64) -> bool>,
    pub utc_offset_west: Option<extern "C" fn() -> i32>,
}

// SAFETY: `AcnhTimeHost` only holds plain function pointers.
unsafe impl Send for AcnhTimeHost {}
// SAFETY: `AcnhTimeHost` only holds plain function pointers.
unsafe impl Sync for AcnhTimeHost {}

#[derive(Default)]
struct ClockState {
    dirty: bool,
    shm_ptr: usize,
    ticker_started: bool,
}

#[derive(Default)]
struct GettimeofdayAnchor {
    base_s: i64,
    base_ns: u64,
}

static HOST: OnceLock<Mutex<AcnhTimeHost>> = OnceLock::new();
static CLOCK_MU: OnceLock<Mutex<ClockState>> = OnceLock::new();
static GETTIMEOFDAY_MU: OnceLock<Mutex<GettimeofdayAnchor>> = OnceLock::new();
static MONO_START: OnceLock<Instant> = OnceLock::new();
static CLOCK_START_NS: OnceLock<u64> = OnceLock::new();

static G_OFFSET: AtomicI64 = AtomicI64::new(0);
static G_SPEED: AtomicI32 = AtomicI32::new(1);
static G_FROZEN: AtomicI64 = AtomicI64::new(0);
static G_SHM_LIVE: AtomicBool = AtomicBool::new(false);

fn host_lock() -> &'static Mutex<AcnhTimeHost> {
    HOST.get_or_init(|| Mutex::new(AcnhTimeHost::default()))
}

fn current_host() -> AcnhTimeHost {
    *host_lock().lock().unwrap_or_else(|e| e.into_inner())
}

fn clock_mu() -> &'static Mutex<ClockState> {
    CLOCK_MU.get_or_init(|| Mutex::new(ClockState::default()))
}

fn gettimeofday_mu() -> &'static Mutex<GettimeofdayAnchor> {
    GETTIMEOFDAY_MU.get_or_init(|| Mutex::new(GettimeofdayAnchor::default()))
}

fn fallback_monotonic_ns() -> u64 {
    let start = MONO_START.get_or_init(Instant::now);
    start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
}

fn host_online_services_active() -> bool {
    current_host()
        .online_services_active
        .map(|f| f())
        .unwrap_or(false)
}

fn host_monotonic_ns() -> u64 {
    current_host()
        .monotonic_ns
        .map(|f| f())
        .unwrap_or_else(fallback_monotonic_ns)
}

fn host_steady_ticks() -> u64 {
    if let Some(f) = current_host().steady_ticks {
        return f();
    }
    let ns = u128::from(fallback_monotonic_ns());
    ((ns * u128::from(STEADY_CLOCK_HZ)) / 1_000_000_000) as u64
}

fn host_utc_offset_west() -> i32 {
    current_host().utc_offset_west.map(|f| f()).unwrap_or(0)
}

pub fn steady_seconds() -> i64 {
    (host_steady_ticks() / STEADY_CLOCK_HZ) as i64
}

pub fn game_clock_real_unix_seconds() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

pub fn game_clock_unix_seconds() -> i64 {
    // Stopped: return the frozen moment itself, not real time + an offset up to a second old.
    if G_SPEED.load(Ordering::Acquire) == 0 {
        return G_FROZEN.load(Ordering::Relaxed);
    }
    game_clock_real_unix_seconds() + G_OFFSET.load(Ordering::Relaxed)
}

pub fn network_unix_seconds() -> i64 {
    if host_online_services_active() {
        game_clock_real_unix_seconds()
    } else {
        game_clock_unix_seconds()
    }
}

fn parse_i64_like(s: &str) -> i64 {
    let s = s.trim_start();
    let (neg, rest) = if let Some(r) = s.strip_prefix('-') {
        (true, r)
    } else if let Some(r) = s.strip_prefix('+') {
        (false, r)
    } else {
        (false, s)
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return 0;
    }
    let val = digits.parse::<i64>().unwrap_or(i64::MAX);
    if neg {
        val.saturating_neg()
    } else {
        val
    }
}

fn env_override(name: &str) -> Option<String> {
    if config::config_is_env_from_config(name) {
        return None;
    }
    let val = std::env::var(name).ok()?;
    if val.is_empty() {
        None
    } else {
        Some(val)
    }
}

fn note_offset(st: &mut ClockState) -> bool {
    if !st.dirty {
        return false;
    }
    config::config_set(
        "game.time_travel_offset",
        &G_OFFSET.load(Ordering::Relaxed).to_string(),
    );
    if G_SPEED.load(Ordering::Relaxed) == 0 {
        config::config_set(
            "game.clock_frozen_at",
            &G_FROZEN.load(Ordering::Relaxed).to_string(),
        );
    }
    st.dirty = false;
    true
}

/// Write an unaligned little-endian `i64`/`u64` into the mapped `time` shared-memory page.
///
/// # Safety
/// `base` must point to a writable `TIME_SHM_SIZE`-byte shared-memory region, and `off + 8 <= TIME_SHM_SIZE`.
unsafe fn write_shm_i64(base: *mut u8, off: usize, val: i64) {
    debug_assert!(off + 8 <= TIME_SHM_SIZE);
    std::ptr::write_unaligned(base.add(off).cast::<i64>(), val);
}

/// # Safety
/// `base` must point to a writable `TIME_SHM_SIZE`-byte shared-memory region, and `off + 8 <= TIME_SHM_SIZE`.
unsafe fn write_shm_u64(base: *mut u8, off: usize, val: u64) {
    debug_assert!(off + 8 <= TIME_SHM_SIZE);
    std::ptr::write_unaligned(base.add(off).cast::<u64>(), val);
}

fn refresh_shared_clocks_at(shm_ptr: usize) {
    if shm_ptr == 0 || !G_SHM_LIVE.load(Ordering::Acquire) {
        return;
    }
    let p = shm_ptr as *mut u8;
    let steady = steady_seconds();
    let local_off = game_clock_unix_seconds() - steady;
    let net_off = network_unix_seconds() - steady;
    for base in [0x38usize, 0x80usize] {
        let val = if base == 0x80 { net_off } else { local_off };
        for k in 0..2 {
            // SAFETY: `p` was registered by `acnh_time_init_shared_memory` with at least `TIME_SHM_SIZE` bytes,
            // and `0x80 + 8 + 1 * 0x20 + 8 = 0xb0 <= 0x1000`.
            unsafe {
                write_shm_i64(p, base + 8 + k * 0x20, val);
            }
        }
    }
}

pub fn refresh_shared_clocks() {
    let shm_ptr = clock_mu().lock().unwrap_or_else(|e| e.into_inner()).shm_ptr;
    refresh_shared_clocks_at(shm_ptr);
}

/// Initialize the `time` shared-memory page (`0x1000` bytes) and retain its host pointer for
/// live updates.
///
/// # Safety
/// `shm_ptr` must be valid for writes of `shm_size` bytes for the lifetime of the process.
pub unsafe fn init_shared_memory(shm_ptr: *mut u8, shm_size: usize) {
    if shm_ptr.is_null() || shm_size < TIME_SHM_SIZE {
        return;
    }
    // LockFreeAtomicType<SteadyClockContext> at 0x0: u32 counter, pad, value[2] {u64 internal_offset; uuid}
    for k in 0..2 {
        write_shm_u64(shm_ptr, 0x8 + k * 0x18, 0);
        write_shm_u64(shm_ptr, 0x10 + k * 0x18, SOURCE_UUID_LO);
        write_shm_u64(shm_ptr, 0x18 + k * 0x18, SOURCE_UUID_HI);
    }
    let steady = steady_seconds();
    let local_off = game_clock_unix_seconds() - steady;
    let net_off = network_unix_seconds() - steady;
    // Local (0x38) and network (0x80) system clocks: {s64 offset; {s64 time_point; uuid}}
    for base in [0x38usize, 0x80usize] {
        let val = if base == 0x80 { net_off } else { local_off };
        for k in 0..2 {
            let v = base + 8 + k * 0x20;
            write_shm_i64(shm_ptr, v, val);
            write_shm_i64(shm_ptr, v + 8, 0);
            write_shm_u64(shm_ptr, v + 16, SOURCE_UUID_LO);
            write_shm_u64(shm_ptr, v + 24, SOURCE_UUID_HI);
        }
    }
    let mut st = clock_mu().lock().unwrap_or_else(|e| e.into_inner());
    st.shm_ptr = shm_ptr as usize;
    G_SHM_LIVE.store(true, Ordering::Release);
}

fn speed_ticker() {
    let mut tick: i64 = 1;
    loop {
        let now = host_monotonic_ns();
        let next_sec_ns = (now / 1_000_000_000 + 1) * 1_000_000_000 + 100_000;
        let sleep_ns = next_sec_ns.saturating_sub(now);
        std::thread::sleep(Duration::from_nanos(sleep_ns));

        let save = {
            let mut st = clock_mu().lock().unwrap_or_else(|e| e.into_inner());
            let k = G_SPEED.load(Ordering::Acquire);
            let online = host_online_services_active();
            if k == 0 {
                let real = game_clock_real_unix_seconds();
                if online {
                    G_FROZEN.store(real + G_OFFSET.load(Ordering::Relaxed), Ordering::Relaxed);
                } else {
                    G_OFFSET.store(G_FROZEN.load(Ordering::Relaxed) - real, Ordering::Relaxed);
                    refresh_shared_clocks_at(st.shm_ptr);
                }
                st.dirty = true;
            } else if k != 1 && !online {
                G_OFFSET.fetch_add(i64::from(k - 1), Ordering::Relaxed);
                st.dirty = true;
                refresh_shared_clocks_at(st.shm_ptr);
            }
            if tick % 10 == 0 || k == 1 {
                note_offset(&mut st)
            } else {
                false
            }
        };
        if save {
            config::config_save();
        }
        tick = tick.wrapping_add(1);
    }
}

pub fn game_clock_save() {
    let save = {
        let mut st = clock_mu().lock().unwrap_or_else(|e| e.into_inner());
        note_offset(&mut st)
    };
    if save {
        config::config_save();
    }
}

pub fn game_clock_set_offset(offset: i64) {
    {
        let mut st = clock_mu().lock().unwrap_or_else(|e| e.into_inner());
        G_OFFSET.store(offset, Ordering::Relaxed);
        st.dirty = false;
        config::config_set("game.time_travel_offset", &offset.to_string());
        if G_SPEED.load(Ordering::Relaxed) == 0 {
            let frozen = game_clock_real_unix_seconds() + offset;
            G_FROZEN.store(frozen, Ordering::Relaxed);
            config::config_set("game.clock_frozen_at", &frozen.to_string());
        }
        refresh_shared_clocks_at(st.shm_ptr);
    }
    config::config_save();
}

pub fn game_clock_apply_config() {
    let override_val = env_override("NX_TIME_OFFSET");
    let mut st = clock_mu().lock().unwrap_or_else(|e| e.into_inner());
    if G_SPEED.load(Ordering::Relaxed) == 0 {
        G_OFFSET.store(
            G_FROZEN.load(Ordering::Relaxed) - game_clock_real_unix_seconds(),
            Ordering::Relaxed,
        );
        st.dirty = true;
    }
    note_offset(&mut st);

    let mut offset: i64 = 0;
    let mut speed: i32 = 1;
    if let Some(ref ov) = override_val {
        offset = parse_i64_like(ov);
    } else if !config::config_get_bool("game.time_travel_enabled", true) {
        offset = config::config_get_int64("game.time_travel_offset", 0);
        speed = config::config_get_int("game.clock_speed", 1).clamp(0, 60);
    }

    if speed == 0 {
        let at = config::config_get("game.clock_frozen_at", "");
        let real = game_clock_real_unix_seconds();
        let frozen = if at.is_empty() {
            real + offset
        } else {
            parse_i64_like(&at)
        };
        G_FROZEN.store(frozen, Ordering::Relaxed);
        offset = frozen - real;
        config::config_set("game.clock_frozen_at", &frozen.to_string());
    } else if override_val.is_none() && !config::config_get("game.clock_frozen_at", "").is_empty() {
        config::config_set("game.clock_frozen_at", "");
    }

    G_OFFSET.store(offset, Ordering::Relaxed);
    G_SPEED.store(speed, Ordering::Release);
    if speed != 1 && !st.ticker_started {
        st.ticker_started = true;
        std::thread::spawn(speed_ticker);
    }
    refresh_shared_clocks_at(st.shm_ptr);
}

pub fn gettimeofday_pair() -> (i64, i64) {
    let mut anchor = gettimeofday_mu().lock().unwrap_or_else(|e| e.into_inner());
    let now_ns = host_monotonic_ns();
    let wall = game_clock_unix_seconds();
    let mut us = ((now_ns.saturating_sub(anchor.base_ns)) / 1000) as i64;
    if anchor.base_ns == 0 || (anchor.base_s + us / 1_000_000 - wall).abs() > 2 {
        anchor.base_s = wall;
        anchor.base_ns = now_ns;
        us = 0;
    }
    (anchor.base_s + us / 1_000_000, us % 1_000_000)
}

pub fn clock_micros() -> u64 {
    let start = *CLOCK_START_NS.get_or_init(host_monotonic_ns);
    host_monotonic_ns().saturating_sub(start) / 1000
}

pub fn device_location_name_bytes() -> [u8; 0x24] {
    let mut out = [0u8; 0x24];
    let name = match std::env::var("TZ") {
        Ok(tz) if !tz.is_empty() => tz.strip_prefix(':').unwrap_or(&tz).to_string(),
        _ => "UTC".to_string(),
    };
    let bytes = name.as_bytes();
    let copy_len = bytes.len().min(out.len() - 1);
    out[..copy_len].copy_from_slice(&bytes[..copy_len]);
    out
}

// ---- Pure-Rust UTC calendar math (Howard Hinnant's civil/days algorithms) ----------------------

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// Days since 1970-01-01 to `(year, month 1..=12, day 1..=31)`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = year - if month <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `(year, month 1..=12, day 1..=31)` from days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

pub fn calendar_from_unix_utc(
    posix: i64,
    utc_offset_secs: i32,
) -> (AcnhCalendarTime, AcnhCalendarAdditionalInfo) {
    let shifted = posix.saturating_add(i64::from(utc_offset_secs));
    let days = shifted.div_euclid(86_400);
    let secs = shifted.rem_euclid(86_400);
    let hour = (secs / 3600) as i8;
    let minute = ((secs % 3600) / 60) as i8;
    let second = (secs % 60) as i8;
    let (year, month, day) = civil_from_days(days);
    // 1970-01-01 was Thursday (4).
    let day_of_week = (days + 4).rem_euclid(7) as i32;
    let jan1_days = days_from_civil(year, 1, 1);
    let day_of_year = (days - jan1_days) as i32;
    let _ = is_leap_year(year);

    let cal = AcnhCalendarTime {
        year: year.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16,
        month: month as i8,
        day: day as i8,
        hour,
        minute,
        second,
        _pad: 0,
    };
    let mut tz_name = [0u8; 8];
    tz_name[..3].copy_from_slice(b"UTC");
    let add = AcnhCalendarAdditionalInfo {
        day_of_week,
        day_of_year,
        tz_name,
        is_dst: 0,
        utc_offset: utc_offset_secs,
    };
    (cal, add)
}

pub fn unix_from_calendar_utc(cal: &AcnhCalendarTime, utc_offset_secs: i32) -> i64 {
    let days = days_from_civil(
        i64::from(cal.year),
        i64::from(cal.month),
        i64::from(cal.day),
    );
    let secs = i64::from(cal.hour) * 3600 + i64::from(cal.minute) * 60 + i64::from(cal.second);
    days * 86_400 + secs - i64::from(utc_offset_secs)
}

pub fn to_calendar_local(posix: i64) -> (AcnhCalendarTime, AcnhCalendarAdditionalInfo) {
    if let Some(f) = current_host().local_from_posix {
        let mut cal = AcnhCalendarTime::default();
        let mut add = AcnhCalendarAdditionalInfo::default();
        if f(posix, &mut cal, &mut add) {
            return (cal, add);
        }
    }
    calendar_from_unix_utc(posix, 0)
}

pub fn to_posix_local(cal: &AcnhCalendarTime) -> i64 {
    if let Some(f) = current_host().posix_from_local {
        let mut out = 0i64;
        if f(cal, &mut out) {
            return out;
        }
    }
    unix_from_calendar_utc(cal, 0)
}

pub fn to_calendar_ipc(posix: i64) -> (AcnhCalendarTime, AcnhCalendarAdditionalInfo) {
    let offset_west = host_utc_offset_west();
    calendar_from_unix_utc(posix, -offset_west)
}

pub fn to_posix_ipc(cal: &AcnhCalendarTime) -> i64 {
    let offset_west = host_utc_offset_west();
    unix_from_calendar_utc(cal, -offset_west)
}

// ---- C FFI exports -----------------------------------------------------------------------------

/// Register host callbacks used by `acnh_time_*`. Passing null clears callbacks to defaults.
///
/// # Safety
/// If `host` is non-null, it must point to a valid `AcnhTimeHost`.
#[no_mangle]
pub unsafe extern "C" fn acnh_time_set_host(host: *const AcnhTimeHost) {
    let val = if host.is_null() {
        AcnhTimeHost::default()
    } else {
        *host
    };
    *host_lock().lock().unwrap_or_else(|e| e.into_inner()) = val;
}

#[no_mangle]
pub extern "C" fn acnh_time_real_unix_seconds() -> i64 {
    game_clock_real_unix_seconds()
}

#[no_mangle]
pub extern "C" fn acnh_time_unix_seconds() -> i64 {
    game_clock_unix_seconds()
}

#[no_mangle]
pub extern "C" fn acnh_time_network_unix_seconds() -> i64 {
    network_unix_seconds()
}

#[no_mangle]
pub extern "C" fn acnh_time_steady_seconds() -> i64 {
    steady_seconds()
}

#[no_mangle]
pub extern "C" fn acnh_time_apply_config() {
    game_clock_apply_config();
}

#[no_mangle]
pub extern "C" fn acnh_time_set_offset(offset: i64) {
    game_clock_set_offset(offset);
}

#[no_mangle]
pub extern "C" fn acnh_time_save() {
    game_clock_save();
}

/// Initialize the `time` shared-memory page (`0x1000` bytes).
///
/// # Safety
/// `shm_ptr` must be valid for writes of `shm_size` bytes for the lifetime of the process.
#[no_mangle]
pub unsafe extern "C" fn acnh_time_init_shared_memory(shm_ptr: *mut u8, shm_size: usize) {
    init_shared_memory(shm_ptr, shm_size);
}

#[no_mangle]
pub extern "C" fn acnh_time_refresh_shared_clocks() {
    refresh_shared_clocks();
}

/// Compute `gettimeofday` seconds and microseconds anchored to the game clock.
///
/// # Safety
/// `out_sec` and `out_usec`, if non-null, must be valid writable pointers.
#[no_mangle]
pub unsafe extern "C" fn acnh_time_gettimeofday(out_sec: *mut i64, out_usec: *mut i64) {
    let (sec, usec) = gettimeofday_pair();
    if !out_sec.is_null() {
        *out_sec = sec;
    }
    if !out_usec.is_null() {
        *out_usec = usec;
    }
}

#[no_mangle]
pub extern "C" fn acnh_time_clock_us() -> u64 {
    clock_micros()
}

/// Copy the `0x24`-byte device location name (from `TZ`, defaulting to `"UTC"`) into `out`.
///
/// # Safety
/// `out` must point to at least `cap` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn acnh_time_device_location_name(out: *mut c_char, cap: usize) {
    if out.is_null() || cap == 0 {
        return;
    }
    let name = device_location_name_bytes();
    let n = cap.min(name.len());
    std::ptr::copy_nonoverlapping(name.as_ptr(), out.cast::<u8>(), n);
}

/// Convert a POSIX timestamp to local `AcnhCalendarTime` and optional `AcnhCalendarAdditionalInfo`.
///
/// # Safety
/// `out_cal` (and `out_add` if non-null) must be valid writable pointers.
#[no_mangle]
pub unsafe extern "C" fn acnh_time_to_calendar(
    posix_time: i64,
    out_cal: *mut AcnhCalendarTime,
    out_add: *mut AcnhCalendarAdditionalInfo,
) {
    let (cal, add) = to_calendar_local(posix_time);
    if !out_cal.is_null() {
        *out_cal = cal;
    }
    if !out_add.is_null() {
        *out_add = add;
    }
}

/// Convert a local `AcnhCalendarTime` to a POSIX timestamp.
///
/// # Safety
/// `cal` must be a valid readable pointer.
#[no_mangle]
pub unsafe extern "C" fn acnh_time_to_posix(cal: *const AcnhCalendarTime) -> i64 {
    if cal.is_null() {
        return 0;
    }
    to_posix_local(&*cal)
}

/// Convert a POSIX timestamp to `ITimeZoneService` calendar time + additional info.
///
/// # Safety
/// `out_cal` (and `out_add` if non-null) must be valid writable pointers.
#[no_mangle]
pub unsafe extern "C" fn acnh_time_to_calendar_ipc(
    posix_time: i64,
    out_cal: *mut AcnhCalendarTime,
    out_add: *mut AcnhCalendarAdditionalInfo,
) {
    let (cal, add) = to_calendar_ipc(posix_time);
    if !out_cal.is_null() {
        *out_cal = cal;
    }
    if !out_add.is_null() {
        *out_add = add;
    }
}

/// Convert an `ITimeZoneService` `AcnhCalendarTime` to a POSIX timestamp.
///
/// # Safety
/// `cal` must be a valid readable pointer.
#[no_mangle]
pub unsafe extern "C" fn acnh_time_to_posix_ipc(cal: *const AcnhCalendarTime) -> i64 {
    if cal.is_null() {
        return 0;
    }
    to_posix_ipc(&*cal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn lock_tests() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn reset_time_state() {
        // SAFETY: clearing test host callbacks to defaults.
        unsafe { acnh_time_set_host(std::ptr::null()) };
        G_OFFSET.store(0, Ordering::Relaxed);
        G_SPEED.store(1, Ordering::Relaxed);
        G_FROZEN.store(0, Ordering::Relaxed);
        G_SHM_LIVE.store(false, Ordering::Release);
        let mut st = clock_mu().lock().unwrap_or_else(|e| e.into_inner());
        st.dirty = false;
        st.shm_ptr = 0;
        std::env::remove_var("NX_TIME_OFFSET");
        std::env::remove_var("TZ");
    }

    #[test]
    fn nx_time_offset_overrides_config_in_apply_config() {
        let _g = lock_tests();
        reset_time_state();
        config::config_set("game.time_travel_enabled", "0");
        config::config_set("game.time_travel_offset", "123");
        std::env::set_var("NX_TIME_OFFSET", "3600");

        game_clock_apply_config();
        let before = game_clock_real_unix_seconds();
        let game_now = game_clock_unix_seconds();
        let after = game_clock_real_unix_seconds();

        assert!(game_now >= before + 3600 && game_now <= after + 3600);
        assert_eq!(G_OFFSET.load(Ordering::Relaxed), 3600);
        reset_time_state();
    }

    #[test]
    fn game_clock_apply_config_reads_offset_and_frozen_moment() {
        let _g = lock_tests();
        reset_time_state();

        // Time travel disabled (`game.time_travel_enabled` = true means real-time sync, i.e. offset 0).
        config::config_set("game.time_travel_enabled", "1");
        config::config_set("game.time_travel_offset", "9999");
        game_clock_apply_config();
        assert_eq!(G_OFFSET.load(Ordering::Relaxed), 0);

        // Custom time enabled (`game.time_travel_enabled` = 0) with frozen clock (`speed = 0`).
        config::config_set("game.time_travel_enabled", "0");
        config::config_set("game.time_travel_offset", "500");
        config::config_set("game.clock_speed", "0");
        config::config_set("game.clock_frozen_at", "1700000000");
        game_clock_apply_config();
        assert_eq!(G_SPEED.load(Ordering::Relaxed), 0);
        assert_eq!(game_clock_unix_seconds(), 1_700_000_000);

        // Switching back to normal speed clears `game.clock_frozen_at`.
        config::config_set("game.clock_speed", "1");
        game_clock_apply_config();
        assert_eq!(G_SPEED.load(Ordering::Relaxed), 1);
        assert_eq!(config::config_get("game.clock_frozen_at", "nonempty"), "");
        reset_time_state();
    }

    static FAKE_TICKS: AtomicU64 = AtomicU64::new(0);
    static FAKE_NS: AtomicU64 = AtomicU64::new(0);

    extern "C" fn fake_steady_ticks() -> u64 {
        FAKE_TICKS.load(Ordering::Relaxed)
    }

    extern "C" fn fake_monotonic_ns() -> u64 {
        FAKE_NS.load(Ordering::Relaxed)
    }

    #[test]
    fn clock_ticks_and_shared_memory_refresh() {
        let _g = lock_tests();
        reset_time_state();
        FAKE_TICKS.store(10 * STEADY_CLOCK_HZ, Ordering::Relaxed);
        FAKE_NS.store(10_000_000_000, Ordering::Relaxed);

        let host = AcnhTimeHost {
            steady_ticks: Some(fake_steady_ticks),
            monotonic_ns: Some(fake_monotonic_ns),
            ..Default::default()
        };
        // SAFETY: `host` is a valid stack struct.
        unsafe { acnh_time_set_host(&host) };

        assert_eq!(steady_seconds(), 10);

        let mut shm = vec![0u8; TIME_SHM_SIZE];
        // SAFETY: `shm` has `TIME_SHM_SIZE` writable bytes.
        unsafe { acnh_time_init_shared_memory(shm.as_mut_ptr(), shm.len()) };

        // Check SteadyClockContext UUIDs at 0x10 and 0x18.
        let lo = u64::from_le_bytes(shm[0x10..0x18].try_into().unwrap());
        let hi = u64::from_le_bytes(shm[0x18..0x20].try_into().unwrap());
        assert_eq!((lo, hi), (SOURCE_UUID_LO, SOURCE_UUID_HI));

        // Update offset and refresh shared memory; verify local system clock offset advances by 42s.
        let before = i64::from_le_bytes(shm[0x40..0x48].try_into().unwrap());
        game_clock_set_offset(42);
        let after = i64::from_le_bytes(shm[0x40..0x48].try_into().unwrap());
        assert!(after >= before + 42 && after <= before + 44);

        // Verify `clock_micros` advances with monotonic_ns.
        let t0 = clock_micros();
        FAKE_NS.fetch_add(250_000, Ordering::Relaxed);
        let t1 = clock_micros();
        assert_eq!(t1 - t0, 250);
        reset_time_state();
    }

    #[test]
    fn calendar_conversion_round_trip_and_leap_year() {
        let _g = lock_tests();
        reset_time_state();

        // 2024-02-29 23:59:59 UTC (leap day, Thursday = 4, day_of_year = 59).
        let leap_posix = 1_709_251_199;
        let (cal, add) = calendar_from_unix_utc(leap_posix, 0);
        assert_eq!(cal.year, 2024);
        assert_eq!(cal.month, 2);
        assert_eq!(cal.day, 29);
        assert_eq!(cal.hour, 23);
        assert_eq!(cal.minute, 59);
        assert_eq!(cal.second, 59);
        assert_eq!(add.day_of_week, 4);
        assert_eq!(add.day_of_year, 59);
        assert_eq!(add.utc_offset, 0);
        assert_eq!(&add.tz_name[..4], b"UTC\0");
        assert_eq!(unix_from_calendar_utc(&cal, 0), leap_posix);

        // With UTC-5 offset (-18000s): 2024-02-29 18:59:59.
        let (cal_est, add_est) = calendar_from_unix_utc(leap_posix, -18_000);
        assert_eq!(cal_est.year, 2024);
        assert_eq!(cal_est.month, 2);
        assert_eq!(cal_est.day, 29);
        assert_eq!(cal_est.hour, 18);
        assert_eq!(add_est.utc_offset, -18_000);
        assert_eq!(unix_from_calendar_utc(&cal_est, -18_000), leap_posix);

        // Epoch 1970-01-01 00:00:00 UTC (Thursday = 4, day_of_year = 0).
        let (epoch_cal, epoch_add) = calendar_from_unix_utc(0, 0);
        assert_eq!(epoch_cal.year, 1970);
        assert_eq!(epoch_cal.month, 1);
        assert_eq!(epoch_cal.day, 1);
        assert_eq!(epoch_add.day_of_week, 4);
        assert_eq!(epoch_add.day_of_year, 0);
        assert_eq!(unix_from_calendar_utc(&epoch_cal, 0), 0);
        reset_time_state();
    }
}
