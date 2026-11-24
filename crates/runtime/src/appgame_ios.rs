//! The iOS half of the game download (issue #231; SPEC.md M18): the Dropbox sign-in and its token,
//! and the C exports the player's Objective-C++ (`runtime/src/appgame_ios.mm`) calls. Rust plans,
//! checks and signs in; the Objective-C++ moves the bytes (a background `URLSession`) and shows the
//! first-start screen. The plan and the checks are `appgame.rs` (over `acnh_core::appgame`), which the tests run on every runner.
//!
//! The sign-in is save-relay's Dropbox app (an App-folder app, the owner's choice of 2026-11-23:
//! reuse save-relay's app), by Dropbox's code flow: the app opens the Dropbox page in Safari, the
//! person pastes the code it shows into an alert, and save-relay's `oauth::finish_code` turns it
//! into a refresh token. The token goes to the Keychain (`KeychainStore`, after first unlock, so a
//! download resumed by the system with the iPad locked can still read it) under an account of its
//! own, apart from the Nextcloud app password. Not verified on a device: the sign-in, and a
//! download resumed after the access token (about 4 hours) has expired.
//!
//! Strings returned to C are allocated here and freed with `acnh_appgame_free`.

use super::appgame::{api_arg, done_json, finish_json, plan_json};
use super::relay::ios::KeychainStore;
use save_relay::clients;
use save_relay::creds::{CredentialStore, Secret};
use save_relay::http::UreqTransport;
use save_relay::oauth::{self, Pending, TokenSource};
use save_relay::service::Service;
use save_relay::stamp;
use serde_json::{json, Value};
use std::ffi::{c_char, CStr, CString};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The Keychain account of the download's Dropbox refresh token.
const ACCOUNT: &str = "dropbox:game-download";

static PENDING_SIGN_IN: Mutex<Option<Pending>> = Mutex::new(None);

fn text(raw: *const c_char) -> Option<String> {
    if raw.is_null() {
        return None;
    }
    // SAFETY: the player passes a NUL-terminated string that lives for the call.
    unsafe { CStr::from_ptr(raw) }
        .to_str()
        .ok()
        .map(str::to_string)
}

fn out(value: Value) -> *mut c_char {
    CString::new(value.to_string().replace('\0', " "))
        .map(CString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

fn error(why: &str) -> *mut c_char {
    out(json!({"ok": false, "code": "io", "error": why}))
}

/// Free a string this module returned.
///
/// # Safety
/// `raw` is null or a string this module returned, not yet freed.
#[no_mangle]
pub unsafe extern "C" fn acnh_appgame_free(raw: *mut c_char) {
    if !raw.is_null() {
        // SAFETY: only strings made by `out` (CString::into_raw) are passed back.
        drop(unsafe { CString::from_raw(raw) });
    }
}

/// What is left to fetch: see `appgame::plan_json`. `root` is the app's private game folder
/// (`Library/Application Support`), `manifest` the downloaded `manifest.json`.
///
/// # Safety
/// `manifest` points to `manifest_len` readable bytes; the strings are null or NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn acnh_appgame_plan(
    root: *const c_char,
    manifest: *const u8,
    manifest_len: usize,
    abi: *const c_char,
    builds: *const c_char,
    free_bytes: u64,
) -> *mut c_char {
    let (Some(root), true) = (text(root), !manifest.is_null()) else {
        return error("no folder or no file list");
    };
    // SAFETY: the player passes `manifest_len` readable bytes.
    let manifest = unsafe { std::slice::from_raw_parts(manifest, manifest_len) };
    let abi = text(abi).unwrap_or_default();
    let builds = text(builds).unwrap_or_default();
    out(plan_json(
        &PathBuf::from(root),
        manifest,
        &abi,
        &builds,
        free_bytes,
    ))
}

/// A file finished: checks its hash (`{"ok":true,"verified":bool}`).
#[no_mangle]
pub extern "C" fn acnh_appgame_file_done(root: *const c_char, path: *const c_char) -> *mut c_char {
    let (Some(root), Some(path)) = (text(root), text(path)) else {
        return error("no folder or no file name");
    };
    out(done_json(&PathBuf::from(root), &path))
}

/// Every file checked: installs `game/` (read-only).
#[no_mangle]
pub extern "C" fn acnh_appgame_finish(root: *const c_char) -> *mut c_char {
    let Some(root) = text(root) else {
        return error("no folder");
    };
    out(finish_json(&PathBuf::from(root)))
}

/// The `Dropbox-API-Arg` header value for a file of the manifest (or `manifest.json`).
#[no_mangle]
pub extern "C" fn acnh_appgame_api_arg(path: *const c_char) -> *mut c_char {
    match text(path) {
        Some(path) => out(json!(api_arg(&path))),
        None => error("no file name"),
    }
}

fn provider() -> &'static oauth::Provider {
    oauth::provider(Service::Dropbox).expect("Dropbox is an OAuth provider")
}

fn stored() -> Option<Secret> {
    KeychainStore.get(ACCOUNT).ok().flatten()
}

/// True when a Dropbox refresh token is in the Keychain.
#[no_mangle]
pub extern "C" fn acnh_appgame_signed_in() -> bool {
    stored().is_some()
}

/// Begin the sign-in: `{"ok":true,"url":...}`, the page to open in Safari.
#[no_mangle]
pub extern "C" fn acnh_appgame_sign_in_start() -> *mut c_char {
    match oauth::start_code(
        provider(),
        clients::DROPBOX,
        &UreqTransport::new(),
        stamp::now(),
    ) {
        Ok(pending) => {
            let url = pending.url.clone();
            *PENDING_SIGN_IN.lock().unwrap_or_else(|e| e.into_inner()) = Some(pending);
            out(json!({"ok": true, "url": url}))
        }
        Err(e) => error(&e.to_string()),
    }
}

/// Finish it with the code the person pasted: stores the refresh token.
#[no_mangle]
pub extern "C" fn acnh_appgame_sign_in_finish(code: *const c_char) -> *mut c_char {
    let Some(code) = text(code) else {
        return error("no code");
    };
    let Some(pending) = PENDING_SIGN_IN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    else {
        return error("the sign-in did not start: try again");
    };
    match oauth::finish_code(
        provider(),
        clients::DROPBOX,
        &pending,
        Some(&code),
        &UreqTransport::new(),
        stamp::now,
    ) {
        Ok(tokens) => match tokens.refresh {
            Some(refresh) => match KeychainStore.set(ACCOUNT, &refresh) {
                Ok(()) => {
                    *PENDING_SIGN_IN.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    out(json!({"ok": true}))
                }
                Err(e) => error(&e.to_string()),
            },
            None => error("Dropbox sent no refresh token: try again"),
        },
        Err(e) => error(&e.to_string()),
    }
}

/// An access token for a request: `{"ok":true,"token":...}`, or `{"ok":false,"reauth":true}` when
/// Dropbox no longer accepts the stored sign-in (it is forgotten).
#[no_mangle]
pub extern "C" fn acnh_appgame_token() -> *mut c_char {
    // A new token source each time: one refresh request, no state to share between threads.
    let Some(refresh) = stored() else {
        return out(json!({"ok": false, "reauth": true, "error": "Sign in to Dropbox first."}));
    };
    let source = TokenSource::new(
        provider(),
        clients::DROPBOX,
        refresh,
        Arc::new(UreqTransport::new()),
        stamp::now,
    );
    let answer = source.bearer();
    if let Some(rotated) = source.rotated() {
        let _ = KeychainStore.set(ACCOUNT, &rotated);
    }
    match answer {
        Ok(token) => out(json!({"ok": true, "token": token.expose()})),
        Err(save_relay::Error::ReauthNeeded(_)) => {
            let _ = KeychainStore.delete(ACCOUNT);
            out(
                json!({"ok": false, "reauth": true, "error": "Dropbox needs you to sign in again."}),
            )
        }
        Err(e) => out(json!({"ok": false, "reauth": false, "error": e.to_string()})),
    }
}

/// Forget the Dropbox sign-in on this device.
#[no_mangle]
pub extern "C" fn acnh_appgame_sign_out() {
    let _ = KeychainStore.delete(ACCOUNT);
}
