//! Player settings (`<data root>/config.ini`), ported from `runtime/src/config.cpp`.
//!
//! Every setting is a `section.key = value` line. At startup (`config_apply`), each setting that
//! maps to a runtime switch becomes its `NX_*` environment variable unless already set (the
//! environment wins, except on Android where phone locale / explicit `game.language` overrides
//! `NX_LANGUAGE` from a prepared game folder). Settings read live are queried via `config_get*`,
//! and `config_save` rewrites `config.ini` preserving unknown keys.

use std::collections::HashSet;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// Host callbacks provided by the C++ runtime (`runtime/src/config_wrapper.cpp`).
///
/// When unset (e.g. in standalone Rust unit tests), `data_root` falls back to `NX_DATA_ROOT` or
/// `"."`, `device_locale` to `""`, `version_string` to `"dev"`, and `log`/`mods_fetch`/
/// `game_clock_apply_config` are no-ops.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AcnhConfigHost {
    pub data_root: Option<extern "C" fn(*mut c_char, usize) -> usize>,
    pub device_locale: Option<extern "C" fn(*mut c_char, usize) -> usize>,
    pub version_string: *const c_char,
    pub log: Option<extern "C" fn(*const c_char)>,
    pub mods_fetch: Option<extern "C" fn()>,
    pub game_clock_apply_config: Option<extern "C" fn()>,
}

// SAFETY: `AcnhConfigHost` holds function pointers and a static C string pointer (`NX_VERSION_STRING`).
unsafe impl Send for AcnhConfigHost {}
// SAFETY: `AcnhConfigHost` holds function pointers and a static C string pointer (`NX_VERSION_STRING`).
unsafe impl Sync for AcnhConfigHost {}

impl Default for AcnhConfigHost {
    fn default() -> Self {
        Self {
            data_root: None,
            device_locale: None,
            version_string: std::ptr::null(),
            log: None,
            mods_fetch: None,
            game_clock_apply_config: None,
        }
    }
}

pub type AcnhConfigStringCb = extern "C" fn(*const c_char, usize, *mut c_void);
pub type AcnhConfigEntryCb = extern "C" fn(*const c_char, usize, *const c_char, usize, *mut c_void);

#[derive(Default)]
struct ConfigState {
    entries: Vec<(String, String)>,
    loaded: bool,
    env_from_config: HashSet<String>,
    last_save_open_err: Option<i32>,
    last_save_rename_err: Option<i32>,
}

static STATE: OnceLock<Mutex<ConfigState>> = OnceLock::new();
static HOST: OnceLock<Mutex<AcnhConfigHost>> = OnceLock::new();
static GENERATION: AtomicU64 = AtomicU64::new(1);

// Cached state for `config_show_fps`, matching `config.cpp`.
static FPS_LAST_GEN: AtomicU64 = AtomicU64::new(0);
static FPS_CACHED: AtomicBool = AtomicBool::new(false);

fn state() -> &'static Mutex<ConfigState> {
    STATE.get_or_init(|| Mutex::new(ConfigState::default()))
}

fn host_lock() -> &'static Mutex<AcnhConfigHost> {
    HOST.get_or_init(|| Mutex::new(AcnhConfigHost::default()))
}

fn current_host() -> AcnhConfigHost {
    *host_lock().lock().unwrap_or_else(|e| e.into_inner())
}

fn call_host_string(f: Option<extern "C" fn(*mut c_char, usize) -> usize>) -> Option<String> {
    let f = f?;
    let mut buf = vec![0u8; 512];
    let len = f(buf.as_mut_ptr().cast::<c_char>(), buf.len());
    if len >= buf.len() {
        buf.resize(len + 1, 0);
        let len2 = f(buf.as_mut_ptr().cast::<c_char>(), buf.len());
        buf.truncate(len2.min(buf.len()));
    } else {
        buf.truncate(len);
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

fn host_data_root() -> String {
    let h = current_host();
    if let Some(root) = call_host_string(h.data_root) {
        if !root.is_empty() {
            return root;
        }
    }
    std::env::var("NX_DATA_ROOT").unwrap_or_else(|_| ".".to_string())
}

fn host_device_locale() -> String {
    let h = current_host();
    call_host_string(h.device_locale).unwrap_or_default()
}

fn host_version_string() -> String {
    let h = current_host();
    if !h.version_string.is_null() {
        // SAFETY: `version_string` is provided by C++ as a valid NUL-terminated static string.
        if let Ok(s) = unsafe { CStr::from_ptr(h.version_string) }.to_str() {
            return s.to_string();
        }
    }
    "dev".to_string()
}

fn host_log(msg: &str) {
    let h = current_host();
    if let Some(log_fn) = h.log {
        if let Ok(cstr) = CString::new(msg.replace('\0', " ")) {
            log_fn(cstr.as_ptr());
        }
    }
}

fn host_mods_fetch() {
    let h = current_host();
    if let Some(f) = h.mods_fetch {
        f();
    }
}

fn host_game_clock_apply_config() {
    let h = current_host();
    if let Some(f) = h.game_clock_apply_config {
        f();
    }
}

fn config_path(data_root: &str) -> PathBuf {
    Path::new(data_root).join("config.ini")
}

fn trim_ascii(s: &str) -> &str {
    s.trim_matches([' ', '\t', '\r', '\n'])
}

fn find_mut<'a>(
    entries: &'a mut [(String, String)],
    key: &str,
) -> Option<&'a mut (String, String)> {
    entries.iter_mut().find(|(k, _)| k == key)
}

fn find_val<'a>(entries: &'a [(String, String)], key: &str) -> Option<&'a str> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn ini_full_key(section: &str, k: &str) -> (String, bool) {
    if section.is_empty() {
        return (k.to_string(), false);
    }
    if section == "key" || section == "key2" {
        return (format!("input.{section}.{k}"), true);
    }
    if !k.contains('.') {
        return (format!("{section}.{k}"), false);
    }
    if section == "input" && (k.starts_with("key.") || k.starts_with("key2.")) {
        return (format!("input.{k}"), false);
    }
    (k.to_string(), false)
}

// ---- Mappers -----------------------------------------------------------------------------------

type MapFn = fn(&str, &str) -> String;

fn on_off(v: &str, _: &str) -> String {
    if v == "1" || v == "on" || v == "true" {
        "1".to_string()
    } else {
        String::new()
    }
}

fn on_or_zero(v: &str, data_root: &str) -> String {
    if v.is_empty() {
        String::new()
    } else if on_off(v, data_root).is_empty() {
        "0".to_string()
    } else {
        "1".to_string()
    }
}

fn off_is_zero(v: &str, _: &str) -> String {
    if v == "0" || v == "off" || v == "false" {
        "0".to_string()
    } else {
        String::new()
    }
}

fn auto_empty(v: &str, _: &str) -> String {
    if v == "auto" {
        String::new()
    } else {
        v.to_string()
    }
}

fn off_empty(v: &str, _: &str) -> String {
    if v == "off" {
        String::new()
    } else {
        v.to_string()
    }
}

fn game_empty(v: &str, _: &str) -> String {
    if v == "game" {
        String::new()
    } else {
        v.to_string()
    }
}

fn game_language(v: &str, _: &str) -> String {
    if v.is_empty() || v == "auto" {
        String::new()
    } else if v == "pt-BR" {
        "es".to_string()
    } else {
        v.to_string()
    }
}

fn handheld(v: &str, _: &str) -> String {
    if v == "handheld" { "1" } else { "0" }.to_string()
}

fn fsr(v: &str, _: &str) -> String {
    if v == "fsr" { "1" } else { "0" }.to_string()
}

fn fps_cap(v: &str, _: &str) -> String {
    if v == "30" {
        "30".to_string()
    } else {
        String::new()
    }
}

fn speedfix_installed(data_root: &str) -> bool {
    Path::new(data_root).join("mods/speedfix60/romfs").is_dir()
}

fn speedfix(v: &str, data_root: &str) -> String {
    if v == "60" && speedfix_installed(data_root) {
        "1".to_string()
    } else {
        String::new()
    }
}

fn interval(v: &str, data_root: &str) -> String {
    if (v == "60" && speedfix_installed(data_root)) || v == "60native" {
        "1".to_string()
    } else {
        String::new()
    }
}

fn native60(v: &str, _: &str) -> String {
    if v == "60native" {
        "1".to_string()
    } else {
        String::new()
    }
}

fn framegen(v: &str, _: &str) -> String {
    if v == "builtin" {
        "1".to_string()
    } else if v.is_empty() {
        String::new()
    } else {
        "0".to_string()
    }
}

fn ffx_framegen(v: &str, _: &str) -> String {
    if v == "fsr" {
        "1".to_string()
    } else {
        String::new()
    }
}

fn atoi_like(s: &str) -> i32 {
    let s = s.trim_start();
    let (neg, rest) = if let Some(r) = s.strip_prefix('-') {
        (true, r)
    } else if let Some(r) = s.strip_prefix('+') {
        (false, r)
    } else {
        (false, s)
    };
    let mut acc: i64 = 0;
    for b in rest.bytes() {
        if !b.is_ascii_digit() {
            break;
        }
        acc = acc.saturating_mul(10).saturating_add(i64::from(b - b'0'));
        if acc > i64::from(i32::MAX) + 1 {
            break;
        }
    }
    if neg {
        (-acc).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
    } else {
        acc.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
    }
}

fn fsr_sharpness(v: &str, _: &str) -> String {
    if v.is_empty() {
        return String::new();
    }
    let clamped = atoi_like(v).clamp(0, 100);
    format!("{:.2}", f64::from(100 - clamped) / 50.0)
}

struct Mapping {
    key: &'static str,
    env: &'static str,
    map: MapFn,
    restart: bool,
}

const MAPPINGS: &[Mapping] = &[
    Mapping {
        key: "game.language",
        env: "NX_LANGUAGE",
        map: game_language,
        restart: true,
    },
    Mapping {
        key: "video.fullscreen",
        env: "NX_FULLSCREEN",
        map: on_off,
        restart: false,
    },
    Mapping {
        key: "video.show_fps",
        env: "NX_SHOW_FPS",
        map: on_off,
        restart: false,
    },
    Mapping {
        key: "video.window",
        env: "NX_RESOLUTION",
        map: auto_empty,
        restart: true,
    },
    Mapping {
        key: "video.mode",
        env: "NX_HANDHELD",
        map: handheld,
        restart: true,
    },
    Mapping {
        key: "video.upscaler",
        env: "NX_FSR",
        map: fsr,
        restart: true,
    },
    Mapping {
        key: "video.fsr_sharpness_pct",
        env: "NX_FSR_SHARP",
        map: fsr_sharpness,
        restart: true,
    },
    Mapping {
        key: "video.antialiasing",
        env: "NX_AA",
        map: off_empty,
        restart: true,
    },
    Mapping {
        key: "video.temporal",
        env: "NX_TEMPORAL",
        map: off_empty,
        restart: false,
    },
    Mapping {
        key: "video.anisotropy",
        env: "NX_ANISOTROPY",
        map: game_empty,
        restart: true,
    },
    Mapping {
        key: "video.gpu",
        env: "NX_GPU",
        map: auto_empty,
        restart: true,
    },
    Mapping {
        key: "video.frame_rate",
        env: "NX_PRESENT_INTERVAL",
        map: interval,
        restart: true,
    },
    Mapping {
        key: "video.frame_rate",
        env: "NX_NATIVE60",
        map: native60,
        restart: true,
    },
    Mapping {
        key: "video.frame_rate",
        env: "NX_SPEEDFIX",
        map: speedfix,
        restart: true,
    },
    Mapping {
        key: "video.frame_cap",
        env: "NX_FPS_CAP",
        map: fps_cap,
        restart: true,
    },
    Mapping {
        key: "video.frame_gen",
        env: "NX_FRAMEGEN",
        map: framegen,
        restart: true,
    },
    Mapping {
        key: "video.frame_gen",
        env: "NX_FFX_FRAMEGEN",
        map: ffx_framegen,
        restart: false,
    },
    Mapping {
        key: "video.async_shaders",
        env: "NX_ASYNC_PIPELINES",
        map: on_or_zero,
        restart: true,
    },
    Mapping {
        key: "video.vsync",
        env: "NX_VSYNC",
        map: off_is_zero,
        restart: true,
    },
    Mapping {
        key: "video.hdr",
        env: "NX_HDR",
        map: off_empty,
        restart: true,
    },
    Mapping {
        key: "video.shader_prepare",
        env: "NX_PIPE_PREWARM",
        map: off_is_zero,
        restart: true,
    },
    Mapping {
        key: "audio.reverb",
        env: "NX_AUDIO_REVERB",
        map: off_is_zero,
        restart: false,
    },
    Mapping {
        key: "audio.surround",
        env: "NX_AUDIO_SURROUND",
        map: off_is_zero,
        restart: true,
    },
    Mapping {
        key: "video.root_clocks",
        env: "NX_ROOT_CLOCKS",
        map: on_off,
        restart: true,
    },
];

// ---- Presets -----------------------------------------------------------------------------------

const PRESET_QUALITY: &[(&str, &str)] = &[
    ("video.mode", "docked"),
    ("video.resolution", "native"),
    ("video.upscaler", "linear"),
    ("audio.reverb", "1"),
];

const PRESET_BALANCED: &[(&str, &str)] = &[
    ("video.mode", "handheld"),
    ("video.resolution", "native"),
    ("video.upscaler", "fsr"),
    ("audio.reverb", "1"),
];

const PRESET_LOW: &[(&str, &str)] = &[
    ("video.mode", "docked"),
    ("video.resolution", "540"),
    ("video.upscaler", "fsr"),
    ("video.frame_gen", "off"),
    ("audio.reverb", "0"),
];

pub fn preset_named(name: &str) -> &'static [(&'static str, &'static str)] {
    match name {
        "quality" => PRESET_QUALITY,
        "balanced" => PRESET_BALANCED,
        "low" => PRESET_LOW,
        _ => &[],
    }
}

// ---- Environment helpers -----------------------------------------------------------------------

fn set_env_default(env_from_config: &mut HashSet<String>, name: &str, value: &str) {
    if std::env::var_os(name).is_some() {
        return;
    }
    std::env::set_var(name, value);
    env_from_config.insert(name.to_string());
}

fn unset_env(env_from_config: &mut HashSet<String>, name: &str) {
    env_from_config.insert(name.to_string());
    std::env::remove_var(name);
}

#[cfg(any(target_os = "android", test))]
fn set_env_force(env_from_config: &mut HashSet<String>, name: &str, value: &str) {
    std::env::set_var(name, value);
    env_from_config.insert(name.to_string());
}

fn log_environment() {
    let mut items = Vec::new();
    for (k, v) in std::env::vars_os() {
        let k = k.to_string_lossy();
        if k.starts_with("NX_")
            || k.starts_with("TU_")
            || k.starts_with("MESA_")
            || k.starts_with("FD_")
        {
            let v = v.to_string_lossy();
            let mut entry = format!("{k}={v}");
            if entry.len() > 120 {
                let mut end = 117;
                while !entry.is_char_boundary(end) {
                    end -= 1;
                }
                entry.truncate(end);
                entry.push_str("...");
            }
            items.push(entry);
        }
    }
    host_log(&format!("ACNHRecomp {}", host_version_string()));
    if items.is_empty() {
        host_log("config: environment: no NX_* variables");
    } else {
        host_log(&format!("config: environment: {}", items.join(" ")));
    }
}

fn system_game_language(data_root: &str, device_locale: &str) -> String {
    let mut loc = device_locale.to_string();
    if loc.is_empty() {
        for v in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(e) = std::env::var(v) {
                if !e.is_empty() && e != "C" && e != "POSIX" {
                    loc = e;
                    break;
                }
            }
        }
    }
    #[cfg(target_vendor = "apple")]
    if loc.is_empty() {
        loc = "en_US".to_string();
    }
    let loc = loc.split(['.', '@']).next().unwrap_or("");
    let mut lang = String::new();
    let mut script = String::new();
    let mut region = String::new();
    let mut rest = loc;
    let mut part = 0;
    while !rest.is_empty() {
        let (sub, next) = match rest.find(['-', '_']) {
            Some(cut) => (&rest[..cut], &rest[cut + 1..]),
            None => (rest, ""),
        };
        rest = next;
        let sub = sub.to_ascii_lowercase();
        if part == 0 {
            lang = sub;
        } else if sub.len() == 4 && script.is_empty() && region.is_empty() {
            script = sub;
        } else if (sub.len() == 2 || sub.len() == 3) && region.is_empty() {
            region = sub;
        }
        part += 1;
    }
    if lang == "pt" {
        let ptbr = Path::new(data_root).join("mods/ptbr/romfs").is_dir();
        return if ptbr { "pt-BR" } else { "en-US" }.to_string();
    }
    if lang == "en" {
        return if region == "us" || region.is_empty() {
            "en-US"
        } else {
            "en-GB"
        }
        .to_string();
    }
    if lang == "fr" {
        return if region == "ca" { "fr-CA" } else { "fr" }.to_string();
    }
    if lang == "es" {
        return if region == "es" || region.is_empty() {
            "es"
        } else {
            "es-419"
        }
        .to_string();
    }
    if lang == "zh" {
        let traditional =
            script == "hant" || (script != "hans" && matches!(region.as_str(), "tw" | "hk" | "mo"));
        return if traditional { "zh-TW" } else { "zh-CN" }.to_string();
    }
    if matches!(lang.as_str(), "de" | "it" | "nl" | "ru" | "ja" | "ko") {
        return lang;
    }
    "en-US".to_string()
}

// ---- Core Rust API -----------------------------------------------------------------------------

pub fn config_load() {
    let data_root = host_data_root();
    GENERATION.fetch_add(1, Ordering::AcqRel);
    let mut st = state().lock().unwrap_or_else(|e| e.into_inner());
    st.entries.clear();
    st.loaded = true;
    let Ok(bytes) = fs::read(config_path(&data_root)) else {
        return;
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut section = String::new();
    for raw_line in text.lines() {
        let line = trim_ascii(raw_line);
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = trim_ascii(&line[1..line.len() - 1]).to_string();
            continue;
        }
        let Some(eq) = line.find('=') else {
            continue;
        };
        let raw_k = trim_ascii(&line[..eq]);
        let v = trim_ascii(&line[eq + 1..]).to_string();
        let (k, legacy) = ini_full_key(&section, raw_k);
        if legacy && find_val(&st.entries, &k).is_some() {
            continue;
        }
        if let Some(entry) = find_mut(&mut st.entries, &k) {
            entry.1 = v;
        } else {
            st.entries.push((k, v));
        }
    }
    if let Some(preset_name) = find_val(&st.entries, "video.preset").map(str::to_string) {
        for &(pk, pv) in preset_named(&preset_name) {
            if find_val(&st.entries, pk).is_none() {
                st.entries.push((pk.to_string(), pv.to_string()));
            }
        }
    }
}

pub fn config_get(key: &str, fallback: &str) -> String {
    let need_load = !state().lock().unwrap_or_else(|e| e.into_inner()).loaded;
    if need_load {
        config_load();
    }
    let st = state().lock().unwrap_or_else(|e| e.into_inner());
    find_val(&st.entries, key)
        .map(str::to_string)
        .unwrap_or_else(|| fallback.to_string())
}

pub fn config_get_int(key: &str, fallback: i32) -> i32 {
    let v = config_get(key, "");
    if v.is_empty() {
        fallback
    } else {
        atoi_like(&v)
    }
}

pub fn config_get_int64(key: &str, fallback: i64) -> i64 {
    let v = config_get(key, "");
    if v.is_empty() {
        fallback
    } else {
        let s = v.trim_start();
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
}

pub fn config_get_float(key: &str, fallback: f32) -> f32 {
    let v = config_get(key, "");
    if v.is_empty() {
        fallback
    } else {
        v.trim().parse::<f32>().unwrap_or(0.0)
    }
}

pub fn config_get_bool(key: &str, fallback: bool) -> bool {
    let v = config_get(key, "");
    if v.is_empty() {
        return fallback;
    }
    matches!(v.as_str(), "1" | "on" | "true" | "yes")
}

pub fn config_set(key: &str, value: &str) {
    struct Bump;
    impl Drop for Bump {
        fn drop(&mut self) {
            GENERATION.fetch_add(1, Ordering::AcqRel);
        }
    }
    let _bump = Bump;
    let mut st = state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = find_mut(&mut st.entries, key) {
        entry.1 = value.to_string();
    } else {
        st.entries.push((key.to_string(), value.to_string()));
    }
}

pub fn config_all() -> Vec<(String, String)> {
    state()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entries
        .clone()
}

pub fn config_generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

pub fn config_is_env_from_config(name: &str) -> bool {
    state()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .env_from_config
        .contains(name)
}

pub fn config_show_fps() -> bool {
    let gen = config_generation();
    if FPS_LAST_GEN.load(Ordering::Relaxed) != gen {
        let v = config_get("video.show_fps", "");
        let val = if !v.is_empty() {
            matches!(v.as_str(), "1" | "on" | "true" | "yes")
        } else {
            std::env::var("NX_SHOW_FPS")
                .map(|e| matches!(e.as_str(), "1" | "true" | "on"))
                .unwrap_or(false)
        };
        FPS_CACHED.store(val, Ordering::Relaxed);
        FPS_LAST_GEN.store(gen, Ordering::Relaxed);
    }
    FPS_CACHED.load(Ordering::Relaxed)
}

pub fn config_is_handheld() -> bool {
    if let Ok(e) = std::env::var("NX_HANDHELD") {
        return !e.is_empty() && !e.starts_with('0') && e != "false" && e != "off";
    }
    config_get("video.mode", "") == "handheld"
}

pub fn config_needs_restart(key: &str) -> bool {
    for m in MAPPINGS {
        if m.key == key {
            return m.restart;
        }
    }
    matches!(
        key,
        "game.mods" | "video.gpu_driver" | "controls.glyphs" | "input.layout" | "video.preset"
    )
}

fn apply_language_selection(
    data_root: &str,
    device_locale: &str,
    is_android: bool,
    ptbr_enabled: bool,
) -> (String, bool) {
    let mut game_lang = config_get("game.language", "auto");
    let mut language_from_phone = false;
    let file = Path::new(data_root).join("language.txt").exists();
    let automatic = game_lang.is_empty() || game_lang == "auto";
    if is_android {
        if automatic && !device_locale.is_empty() {
            game_lang = system_game_language(data_root, device_locale);
            language_from_phone = true;
            host_log(&format!(
                "config: game language from the phone: {game_lang}"
            ));
        } else if !automatic {
            language_from_phone = true;
        } else if automatic && !file && std::env::var_os("NX_LANGUAGE").is_none() {
            game_lang = system_game_language(data_root, device_locale);
            host_log(&format!(
                "config: game language from the system: {game_lang}"
            ));
        }
    } else if automatic && !file && std::env::var_os("NX_LANGUAGE").is_none() {
        game_lang = system_game_language(data_root, device_locale);
        host_log(&format!(
            "config: game language from the system: {game_lang}"
        ));
    }
    if !ptbr_enabled && game_lang == "pt-BR" {
        game_lang = "en-US".to_string();
    }
    (game_lang, language_from_phone)
}

pub fn config_apply() {
    let need_load = !state().lock().unwrap_or_else(|e| e.into_inner()).loaded;
    if need_load {
        config_load();
    }
    let data_root = host_data_root();
    let device_locale = host_device_locale();
    log_environment();

    let ptbr_enabled = !on_off(&config_get("mods.ptbr", "0"), &data_root).is_empty();
    let speedfix_enabled = !on_off(&config_get("mods.speedfix", "0"), &data_root).is_empty()
        || config_get("video.frame_rate", "") == "60";
    if speedfix_enabled {
        host_mods_fetch();
    }

    let (game_lang, language_from_phone) = apply_language_selection(
        &data_root,
        &device_locale,
        cfg!(target_os = "android"),
        ptbr_enabled,
    );

    let count = {
        let mut st = state().lock().unwrap_or_else(|e| e.into_inner());
        let ConfigState {
            entries,
            env_from_config,
            ..
        } = &mut *st;
        if !speedfix_enabled {
            unset_env(env_from_config, "NX_SPEEDFIX");
        }
        for m in MAPPINGS {
            let value = if m.key == "game.language" {
                game_lang.clone()
            } else {
                find_val(entries, m.key).unwrap_or("").to_string()
            };
            if value.is_empty() {
                continue;
            }
            let mut v = (m.map)(&value, &data_root);
            if !speedfix_enabled
                && value == "60"
                && (m.env == "NX_SPEEDFIX" || m.env == "NX_PRESENT_INTERVAL")
            {
                v.clear();
            }
            if m.env == "NX_HANDHELD" {
                if v == "0" {
                    unset_env(env_from_config, "NX_HANDHELD");
                } else if v == "1" {
                    set_env_default(env_from_config, "NX_HANDHELD", "1");
                }
            } else if m.env == "NX_FSR" {
                if v == "0" {
                    unset_env(env_from_config, "NX_FSR");
                } else if v == "1" {
                    set_env_default(env_from_config, "NX_FSR", "1");
                }
            } else if !v.is_empty() && m.env == "NX_LANGUAGE" && language_from_phone {
                #[cfg(target_os = "android")]
                set_env_force(env_from_config, "NX_LANGUAGE", &v);
            } else if !v.is_empty() {
                set_env_default(env_from_config, m.env, &v);
            }
        }

        let mut mods = find_val(entries, "game.mods").unwrap_or("").to_string();
        if ptbr_enabled && game_lang == "pt-BR" {
            if mods.is_empty() {
                mods = "ptbr".to_string();
            } else if !mods.contains("ptbr") {
                mods.push_str(",ptbr");
            }
        }
        if speedfix_enabled
            && std::env::var("NX_SPEEDFIX")
                .ok()
                .is_some_and(|sf| sf.starts_with('1'))
        {
            mods = if mods.is_empty() {
                "speedfix60".to_string()
            } else {
                format!("{mods},speedfix60")
            };
        }
        if !mods.is_empty() {
            set_env_default(env_from_config, "NX_MODS", &mods);
        }
        entries.len()
    };

    host_game_clock_apply_config();
    let cfg_path = config_path(&data_root);
    host_log(&format!(
        "config: {count} settings from {}",
        cfg_path.display()
    ));
}

pub fn config_save() -> bool {
    let data_root = host_data_root();
    let mut st = state().lock().unwrap_or_else(|e| e.into_inner());
    let mut sections: Vec<String> = Vec::new();
    for (k, _) in &st.entries {
        let s = k.split('.').next().unwrap_or(k.as_str());
        if !sections.iter().any(|existing| existing == s) {
            sections.push(s.to_string());
        }
    }
    let path = config_path(&data_root);
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    let mut file = match fs::File::create(&tmp) {
        Ok(f) => f,
        Err(e) => {
            let code = e.raw_os_error().unwrap_or(-1);
            if st.last_save_open_err != Some(code) {
                st.last_save_open_err = Some(code);
                host_log(&format!(
                    "config: cannot write {} ({}): settings are not saved",
                    tmp.display(),
                    e
                ));
            }
            return false;
        }
    };
    let mut write_ok = writeln!(
        file,
        "# ACNHRecomp settings (edited by the settings screen; F1 or Minus+Plus in game)"
    )
    .is_ok();
    if write_ok {
        'outer: for s in &sections {
            if writeln!(file, "\n[{s}]").is_err() {
                write_ok = false;
                break;
            }
            let prefix = format!("{s}.");
            for (k, v) in &st.entries {
                if let Some(rest) = k.strip_prefix(&prefix) {
                    if writeln!(file, "{rest} = {v}").is_err() {
                        write_ok = false;
                        break 'outer;
                    }
                }
            }
        }
    }
    if write_ok {
        write_ok = file.flush().is_ok();
    }
    drop(file);
    if !write_ok {
        return false;
    }
    match fs::rename(&tmp, &path) {
        Ok(()) => true,
        Err(e) => {
            let code = e.raw_os_error().unwrap_or(-1);
            if st.last_save_rename_err != Some(code) {
                st.last_save_rename_err = Some(code);
                host_log(&format!(
                    "config: cannot replace {} ({}): settings are not saved",
                    path.display(),
                    e
                ));
            }
            false
        }
    }
}

// ---- C FFI exports -----------------------------------------------------------------------------

fn cstr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: Caller guarantees `ptr` is a valid NUL-terminated C string.
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}

/// # Safety
/// `host` must be null or point to a valid `AcnhConfigHost` for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn acnh_config_set_host(host: *const AcnhConfigHost) {
    if host.is_null() {
        return;
    }
    // SAFETY: Caller guarantees `host` points to a valid `AcnhConfigHost`.
    let h = unsafe { *host };
    *host_lock().lock().unwrap_or_else(|e| e.into_inner()) = h;
}

#[no_mangle]
pub extern "C" fn acnh_config_load() {
    config_load();
}

#[no_mangle]
pub extern "C" fn acnh_config_apply() {
    config_apply();
}

#[no_mangle]
pub extern "C" fn acnh_config_save() -> bool {
    config_save()
}

#[no_mangle]
pub extern "C" fn acnh_config_get(
    key: *const c_char,
    fallback: *const c_char,
    cb: AcnhConfigStringCb,
    ctx: *mut c_void,
) {
    let key = cstr_to_str(key).unwrap_or("");
    let fallback = cstr_to_str(fallback).unwrap_or("");
    let val = config_get(key, fallback);
    cb(val.as_ptr().cast::<c_char>(), val.len(), ctx);
}

#[no_mangle]
pub extern "C" fn acnh_config_get_int(key: *const c_char, fallback: c_int) -> c_int {
    let key = cstr_to_str(key).unwrap_or("");
    config_get_int(key, fallback)
}

#[no_mangle]
pub extern "C" fn acnh_config_get_int64(key: *const c_char, fallback: i64) -> i64 {
    let key = cstr_to_str(key).unwrap_or("");
    config_get_int64(key, fallback)
}

#[no_mangle]
pub extern "C" fn acnh_config_get_float(key: *const c_char, fallback: f32) -> f32 {
    let key = cstr_to_str(key).unwrap_or("");
    config_get_float(key, fallback)
}

#[no_mangle]
pub extern "C" fn acnh_config_get_bool(key: *const c_char, fallback: bool) -> bool {
    let key = cstr_to_str(key).unwrap_or("");
    config_get_bool(key, fallback)
}

#[no_mangle]
pub extern "C" fn acnh_config_set(key: *const c_char, value: *const c_char) {
    let key = cstr_to_str(key).unwrap_or("");
    let value = cstr_to_str(value).unwrap_or("");
    config_set(key, value);
}

#[no_mangle]
pub extern "C" fn acnh_config_all(cb: AcnhConfigEntryCb, ctx: *mut c_void) {
    for (k, v) in config_all() {
        cb(
            k.as_ptr().cast::<c_char>(),
            k.len(),
            v.as_ptr().cast::<c_char>(),
            v.len(),
            ctx,
        );
    }
}

#[no_mangle]
pub extern "C" fn acnh_config_generation() -> u64 {
    config_generation()
}

#[no_mangle]
pub extern "C" fn acnh_config_is_env_from_config(name: *const c_char) -> bool {
    let name = cstr_to_str(name).unwrap_or("");
    config_is_env_from_config(name)
}

#[no_mangle]
pub extern "C" fn acnh_config_show_fps() -> bool {
    config_show_fps()
}

#[no_mangle]
pub extern "C" fn acnh_config_is_handheld() -> bool {
    config_is_handheld()
}

#[no_mangle]
pub extern "C" fn acnh_config_preset(name: *const c_char, cb: AcnhConfigEntryCb, ctx: *mut c_void) {
    let name = cstr_to_str(name).unwrap_or("");
    for &(k, v) in preset_named(name) {
        cb(
            k.as_ptr().cast::<c_char>(),
            k.len(),
            v.as_ptr().cast::<c_char>(),
            v.len(),
            ctx,
        );
    }
}

#[no_mangle]
pub extern "C" fn acnh_config_needs_restart(key: *const c_char) -> bool {
    let key = cstr_to_str(key).unwrap_or("");
    config_needs_restart(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    static MODS_FETCH_COUNT: AtomicUsize = AtomicUsize::new(0);
    static CLOCK_APPLY_COUNT: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn test_mods_fetch() {
        MODS_FETCH_COUNT.fetch_add(1, Ordering::SeqCst);
    }

    extern "C" fn test_clock_apply() {
        CLOCK_APPLY_COUNT.fetch_add(1, Ordering::SeqCst);
    }

    struct TestGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        dir: PathBuf,
        saved_env: Vec<(&'static str, Option<String>)>,
    }

    const TRACKED_ENV: &[&str] = &[
        "NX_DATA_ROOT",
        "NX_LANGUAGE",
        "NX_FULLSCREEN",
        "NX_SHOW_FPS",
        "NX_RESOLUTION",
        "NX_HANDHELD",
        "NX_FSR",
        "NX_FSR_SHARP",
        "NX_AA",
        "NX_TEMPORAL",
        "NX_ANISOTROPY",
        "NX_GPU",
        "NX_PRESENT_INTERVAL",
        "NX_NATIVE60",
        "NX_SPEEDFIX",
        "NX_FPS_CAP",
        "NX_FRAMEGEN",
        "NX_FFX_FRAMEGEN",
        "NX_ASYNC_PIPELINES",
        "NX_VSYNC",
        "NX_HDR",
        "NX_PIPE_PREWARM",
        "NX_AUDIO_REVERB",
        "NX_AUDIO_SURROUND",
        "NX_ROOT_CLOCKS",
        "NX_MODS",
        "LC_ALL",
        "LC_MESSAGES",
        "LANG",
    ];

    impl TestGuard {
        fn new(name: &str) -> Self {
            let lock = TEST_LOCK
                .get_or_init(|| Mutex::new(()))
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let dir = std::env::temp_dir()
                .join(format!("acnh_config_test_{name}_{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();

            let mut saved_env = Vec::new();
            for &k in TRACKED_ENV {
                saved_env.push((k, std::env::var(k).ok()));
                std::env::remove_var(k);
            }
            std::env::set_var("NX_DATA_ROOT", &dir);

            *host_lock().lock().unwrap() = AcnhConfigHost {
                mods_fetch: Some(test_mods_fetch),
                game_clock_apply_config: Some(test_clock_apply),
                ..AcnhConfigHost::default()
            };
            MODS_FETCH_COUNT.store(0, Ordering::SeqCst);
            CLOCK_APPLY_COUNT.store(0, Ordering::SeqCst);

            {
                let mut st = state().lock().unwrap();
                st.entries.clear();
                st.loaded = false;
                st.env_from_config.clear();
                st.last_save_open_err = None;
                st.last_save_rename_err = None;
            }
            FPS_LAST_GEN.store(0, Ordering::SeqCst);
            FPS_CACHED.store(false, Ordering::SeqCst);

            Self {
                _lock: lock,
                dir,
                saved_env,
            }
        }
    }

    impl Drop for TestGuard {
        fn drop(&mut self) {
            *host_lock().lock().unwrap() = AcnhConfigHost::default();
            for (k, v) in &self.saved_env {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn config_load_parses_sections_legacy_keys_and_presets() {
        let g = TestGuard::new("load");
        fs::write(
            g.dir.join("config.ini"),
            "\
# comment
; another comment
[key]
move_up = W
jump = Space

[input]
key.move_up = Up

[video]
preset = balanced
upscaler = linear

video.frame_rate = 60
",
        )
        .unwrap();

        config_load();
        assert_eq!(config_get("input.key.move_up", ""), "Up");
        assert_eq!(config_get("input.key.jump", ""), "Space");
        // Explicit upscaler wins over balanced preset's "fsr"; missing keys come from preset.
        assert_eq!(config_get("video.upscaler", ""), "linear");
        assert_eq!(config_get("video.mode", ""), "handheld");
        assert_eq!(config_get("video.resolution", ""), "native");
        assert_eq!(config_get("audio.reverb", ""), "1");
        assert_eq!(config_get("video.frame_rate", ""), "60");
    }

    #[test]
    fn config_save_groups_by_section_and_preserves_unknown_keys() {
        let g = TestGuard::new("save");
        config_load();
        config_set("video.mode", "handheld");
        config_set("custom.unknown_key", "hello");
        config_set("video.upscaler", "fsr");
        config_set("input.key.move_up", "W");

        assert!(config_save());
        let written = fs::read_to_string(g.dir.join("config.ini")).unwrap();
        assert!(written.contains("[video]\nmode = handheld\nupscaler = fsr\n"));
        assert!(written.contains("[custom]\nunknown_key = hello\n"));
        assert!(written.contains("[input]\nkey.move_up = W\n"));

        config_load();
        assert_eq!(config_get("custom.unknown_key", ""), "hello");
        assert_eq!(config_get("input.key.move_up", ""), "W");
    }

    #[test]
    fn presets_match_expected_keys() {
        assert_eq!(
            preset_named("quality"),
            &[
                ("video.mode", "docked"),
                ("video.resolution", "native"),
                ("video.upscaler", "linear"),
                ("audio.reverb", "1"),
            ]
        );
        assert_eq!(
            preset_named("balanced"),
            &[
                ("video.mode", "handheld"),
                ("video.resolution", "native"),
                ("video.upscaler", "fsr"),
                ("audio.reverb", "1"),
            ]
        );
        assert_eq!(
            preset_named("low"),
            &[
                ("video.mode", "docked"),
                ("video.resolution", "540"),
                ("video.upscaler", "fsr"),
                ("video.frame_gen", "off"),
                ("audio.reverb", "0"),
            ]
        );
        assert!(preset_named("custom").is_empty());
        assert!(preset_named("nonexistent").is_empty());
    }

    #[test]
    fn config_apply_sets_env_and_respects_overrides() {
        let _g = TestGuard::new("apply_env");
        std::env::set_var("NX_FULLSCREEN", "0");

        config_load();
        config_set("video.fullscreen", "1");
        config_set("video.mode", "handheld");
        config_set("video.upscaler", "fsr");
        config_set("video.fsr_sharpness_pct", "50");
        config_set("audio.reverb", "off");
        config_apply();

        // Pre-existing env wins and is not marked as from_config.
        assert_eq!(std::env::var("NX_FULLSCREEN").unwrap(), "0");
        assert!(!config_is_env_from_config("NX_FULLSCREEN"));

        assert_eq!(std::env::var("NX_HANDHELD").unwrap(), "1");
        assert!(config_is_env_from_config("NX_HANDHELD"));
        assert_eq!(std::env::var("NX_FSR").unwrap(), "1");
        assert_eq!(std::env::var("NX_FSR_SHARP").unwrap(), "1.00");
        assert_eq!(std::env::var("NX_AUDIO_REVERB").unwrap(), "0");
        assert_eq!(CLOCK_APPLY_COUNT.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn speedfix_60fps_sets_nx_speedfix_when_installed() {
        let g = TestGuard::new("speedfix");
        fs::create_dir_all(g.dir.join("mods/speedfix60/romfs")).unwrap();

        config_load();
        config_set("video.frame_rate", "60");
        config_set("mods.speedfix", "1");
        config_apply();

        assert_eq!(MODS_FETCH_COUNT.load(Ordering::SeqCst), 1);
        assert_eq!(std::env::var("NX_SPEEDFIX").unwrap(), "1");
        assert_eq!(std::env::var("NX_PRESENT_INTERVAL").unwrap(), "1");
        assert_eq!(std::env::var("NX_MODS").unwrap(), "speedfix60");
    }

    #[test]
    fn speedfix_60fps_stays_unset_without_speedfix_romfs() {
        let _g = TestGuard::new("no_speedfix");
        config_load();
        config_set("video.frame_rate", "60");
        config_apply();

        assert_eq!(MODS_FETCH_COUNT.load(Ordering::SeqCst), 1);
        assert!(std::env::var_os("NX_SPEEDFIX").is_none());
        assert!(std::env::var_os("NX_PRESENT_INTERVAL").is_none());
    }

    #[test]
    fn android_phone_locale_and_ptbr_rules() {
        let g = TestGuard::new("android_locale");
        fs::write(g.dir.join("language.txt"), "en-US").unwrap();
        std::env::set_var("NX_LANGUAGE", "en-US");

        config_load();
        config_set("game.language", "auto");
        let (lang, from_phone) =
            apply_language_selection(g.dir.to_str().unwrap(), "zh_Hant_TW", true, false);
        assert_eq!(lang, "zh-TW");
        assert!(from_phone);

        // On desktop, language.txt / NX_LANGUAGE wins over system locale.
        let (desktop_lang, desktop_phone) =
            apply_language_selection(g.dir.to_str().unwrap(), "", false, false);
        assert_eq!(desktop_lang, "auto");
        assert!(!desktop_phone);

        // pt-BR requires mods.ptbr = 1; otherwise falls back to en-US.
        config_set("game.language", "pt-BR");
        let (no_ptbr, _) = apply_language_selection(g.dir.to_str().unwrap(), "pt_BR", true, false);
        assert_eq!(no_ptbr, "en-US");
        let (with_ptbr, _) = apply_language_selection(g.dir.to_str().unwrap(), "pt_BR", true, true);
        assert_eq!(with_ptbr, "pt-BR");

        // Test set_env_force helper used on Android.
        let mut set = HashSet::new();
        set_env_force(&mut set, "NX_LANGUAGE", "zh-TW");
        assert_eq!(std::env::var("NX_LANGUAGE").unwrap(), "zh-TW");
        assert!(set.contains("NX_LANGUAGE"));
    }

    #[test]
    fn generation_show_fps_handheld_and_restart_helpers() {
        let _g = TestGuard::new("helpers");
        config_load();
        let g0 = config_generation();
        config_set("video.show_fps", "1");
        assert!(config_generation() > g0);
        assert!(config_show_fps());

        config_set("video.mode", "handheld");
        assert!(config_is_handheld());
        config_set("video.mode", "docked");
        assert!(!config_is_handheld());

        assert!(config_needs_restart("video.mode"));
        assert!(!config_needs_restart("video.show_fps"));
        assert!(config_needs_restart("video.preset"));
        assert!(!config_needs_restart("unknown.key"));
    }
}
