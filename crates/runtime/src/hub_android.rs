//! Android hub discovery, pairing, and prepared-game pull into `<files>/game/` (#80).
//!
//! All hub state (`peers.json`, `device-id.txt`, `launcher.ini`) lives under `<files>/hub/`,
//! and the pulled game is installed strictly through `acnh_core::appgame::install` into
//! `<files>/game/` (never outside `<files>/game/`).

use acnh_core::config::Config;
use acnh_core::hub;
use acnh_core::platform::{atomic_write, AppDirs, Os};
use acnh_core::Result;
use std::ffi::{c_char, CStr, CString};
use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

fn hub_dirs(files: &Path) -> AppDirs {
    let base = files.join("hub");
    AppDirs {
        config: base.join("config"),
        data: base.join("data"),
    }
}

fn device_id(dirs: &AppDirs) -> Result<String> {
    let path = dirs.config.join("device-id.txt");
    if let Ok(existing) = fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_owned());
        }
    }
    fs::create_dir_all(&dirs.config)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = format!("android-{:x}-{}", std::process::id(), now);
    atomic_write(&path, format!("{id}\n").as_bytes())?;
    Ok(id)
}

fn save_hub_config(dirs: &AppDirs, hub_name: &str, addr: &str) -> Result<()> {
    fs::create_dir_all(&dirs.config)?;
    let path = dirs.config.join("launcher.ini");
    let mut cfg = match fs::read_to_string(&path) {
        Ok(text) => Config::parse(&text)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => return Err(e.into()),
    };
    cfg.set("hub_name", hub_name);
    cfg.set("hub_address", addr);
    atomic_write(&path, cfg.to_text().as_bytes())
}

fn cstr(ptr: *const c_char) -> std::result::Result<String, String> {
    if ptr.is_null() {
        return Err("missing string argument".into());
    }
    // SAFETY: caller provides a NUL-terminated UTF-8 string for the duration of the call.
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .map(str::to_owned)
        .map_err(|_| "invalid UTF-8 string".into())
}

fn into_c_string(text: String) -> *mut c_char {
    CString::new(text.replace('\0', " "))
        .unwrap_or_else(|_| CString::new("").expect("empty CString"))
        .into_raw()
}

fn catch(f: impl FnOnce() -> String + std::panic::UnwindSafe) -> *mut c_char {
    let out = std::panic::catch_unwind(f).unwrap_or_else(|_| "unexpected internal error".into());
    into_c_string(out)
}

/// Discover paired-capable hubs on the home network for `Os::Android`.
/// Returns a JSON array of `{"hub": "...", "addr": "...", "port": N}`.
#[no_mangle]
pub extern "C" fn acnh_hub_android_discover(timeout_ms: u32) -> *mut c_char {
    catch(|| match hub::discover(timeout_ms as u64) {
        Ok(found) => {
            let items: Vec<serde_json::Value> = found
                .into_iter()
                .filter(|h| h.targets.contains(&Os::Android))
                .map(|h| {
                    serde_json::json!({
                        "hub": h.hub,
                        "addr": h.addr.to_string(),
                        "port": h.port,
                    })
                })
                .collect();
            serde_json::to_string(&items).unwrap_or_else(|_| "[]".into())
        }
        Err(_) => "[]".into(),
    })
}

/// Read the currently paired hub configuration under `<files>/hub/`.
/// Returns JSON `{"hub_name": "...", "hub_address": "...", "peers": ["..."]}`.
#[no_mangle]
pub extern "C" fn acnh_hub_android_paired(files: *const c_char) -> *mut c_char {
    catch(|| {
        let Ok(files) = cstr(files) else {
            return "{}".into();
        };
        let dirs = hub_dirs(Path::new(&files));
        let peers: Vec<String> = hub::read_peers(&dirs)
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.device)
            .collect();
        let cfg = fs::read_to_string(dirs.config.join("launcher.ini"))
            .ok()
            .and_then(|t| Config::parse(&t).ok())
            .unwrap_or_default();
        let hub_name = cfg.get("hub_name").unwrap_or("");
        let hub_address = cfg.get("hub_address").unwrap_or("");
        serde_json::json!({
            "hub_name": hub_name,
            "hub_address": hub_address,
            "peers": peers,
        })
        .to_string()
    })
}

/// Pair with a hub at `addr` (`ip:port`) using `code` and save the peer under `<files>/hub/`.
/// Returns `""` on success, or a human-readable error message.
#[no_mangle]
pub extern "C" fn acnh_hub_android_pair(
    files: *const c_char,
    addr: *const c_char,
    hub_name: *const c_char,
    code: *const c_char,
) -> *mut c_char {
    catch(|| {
        let res = (|| -> std::result::Result<(), String> {
            let files = cstr(files)?;
            let addr_str = cstr(addr)?;
            let hub_name = cstr(hub_name)?;
            let code = cstr(code)?;
            let sock: SocketAddr = addr_str
                .trim()
                .parse()
                .map_err(|_| format!("invalid hub address: {addr_str}"))?;
            let dirs = hub_dirs(Path::new(&files));
            let id = device_id(&dirs).map_err(|e| e.to_string())?;
            hub::pair_hub(sock, &dirs, &id, hub_name.trim(), code.trim(), Os::Android)
                .map_err(|e| e.to_string())?;
            save_hub_config(&dirs, hub_name.trim(), &sock.to_string())
                .map_err(|e| e.to_string())?;
            Ok(())
        })();
        match res {
            Ok(()) => String::new(),
            Err(e) => e,
        }
    })
}

/// Pull prepared Android game library and data from a paired hub and install it into
/// `<files>/game/` through `acnh_core::appgame::install`.
/// Returns `""` on success, or a human-readable error message.
#[no_mangle]
pub extern "C" fn acnh_hub_android_pull(
    files: *const c_char,
    addr: *const c_char,
    hub_name: *const c_char,
    free_bytes: u64,
) -> *mut c_char {
    catch(|| {
        let res = (|| -> std::result::Result<(), String> {
            let files = cstr(files)?;
            let addr_str = cstr(addr)?;
            let hub_name = cstr(hub_name)?;
            let sock: SocketAddr = addr_str
                .trim()
                .parse()
                .map_err(|_| format!("invalid hub address: {addr_str}"))?;
            let private_dir = Path::new(&files);
            let dirs = hub_dirs(private_dir);
            let id = device_id(&dirs).map_err(|e| e.to_string())?;
            let game_dir = hub::pull_hub_prepared_app(
                sock,
                &dirs,
                &id,
                hub_name.trim(),
                Os::Android,
                private_dir,
                free_bytes,
            )
            .map_err(|e| e.to_string())?;
            for req in [
                "analysis/main.bin",
                "romfs.pak",
                "spirv.pak",
                "libacnh_game.so",
            ] {
                if !game_dir.join(req).is_file() {
                    return Err(format!("installed game verification failed: {req} missing"));
                }
            }
            Ok(())
        })();
        match res {
            Ok(()) => String::new(),
            Err(e) => e,
        }
    })
}

/// Free a string returned by `acnh_hub_android_*`.
///
/// # Safety
/// `ptr` must be null or a string this module returned, freed only once.
#[no_mangle]
pub unsafe extern "C" fn acnh_hub_android_free(ptr: *mut c_char) {
    if !ptr.is_null() {
        // SAFETY: `ptr` was created by `CString::into_raw` in this module.
        unsafe {
            drop(CString::from_raw(ptr));
        }
    }
}
