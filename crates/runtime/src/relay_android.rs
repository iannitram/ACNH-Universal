//! The Android half of `relay.rs` (issue #81): the C exports the app calls through its JNI glue
//! (`runtime/src/relay_android.cpp`), and the calls back into it.
//!
//! The pieces come from save-relay's own Android bindings where they exist: the sign-in is its
//! C interface's (`save_relay_sign_in_start` / `_finish`, feature `ffi`, with its JNI glue and its
//! host ported to Java in `android/`), and the network is the Java host's `http`, which that
//! interface calls too; the loop here reaches it through `nx_relay_http`, in the same wire format.
//! The credential store is `FileStore` (`relay.rs`), which the Java host's credential calls reach
//! through `acnh_relay_android_cred_*`, so save-relay's sign-in and this loop read the same file.
//! The prompts are `BridgePrompt`'s JSON, answered by the home screen's dialogs (`nx_relay_ask`).
//!
//! Every export takes the game folder (`data_root`) and the app's private files folder (`files`,
//! `Context.getFilesDir()`), catches panics, and never logs a secret, the account or a path.

use super::{
    android_ask_off_file, android_credentials, android_ready, forget_android, sign_out_android,
    BridgePrompt, CallbackTransport, Connector, FileStore, Player, Roots, Settings,
};
use acnh_core::relay::SaveRelay;
use acnh_core::Result;
use save_relay::creds::{CredentialStore, Secret};
use save_relay::local::LocalStore;
use save_relay::platform::AppDirs;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

extern "C" {
    /// `runtime/src/relay_android.cpp`: one line into the app's log (`nx::log`, or logcat before
    /// the game folder is known).
    fn nx_relay_log(line: *const c_char);
    /// `relay.ask` is `0` in the game folder's `config.ini` (the player's own settings).
    fn nx_relay_ask_off() -> bool;
    /// `config_set("relay.ask", "0")` and `config_save()` on the game folder's `config.ini`.
    fn nx_relay_remember_not_now() -> bool;
    /// One `BridgePrompt` request to the home screen (`Relay.ask`); blocks until it is answered.
    /// The answer is `malloc`ed (free it with `free`), or null when the screens cannot be reached.
    fn nx_relay_ask(request: *const c_char) -> *mut c_char;
    /// One request through the Java host's `http` (the one save-relay's C interface uses). The
    /// answer, in `parse_wire`'s format, is `malloc`ed into `*out` (free it with `free`); 0 when
    /// one came back, nonzero for a connection failure.
    fn nx_relay_http(
        method: *const c_char,
        url: *const c_char,
        headers: *const c_char,
        body: *const u8,
        body_len: usize,
        max_response_bytes: usize,
        out: *mut *mut u8,
        out_len: *mut usize,
    ) -> c_int;
    fn free(pointer: *mut c_void);
}

fn log(line: &str) {
    if let Ok(line) = CString::new(line.replace('\0', " ")) {
        // SAFETY: a NUL-terminated string that lives for the call.
        unsafe { nx_relay_log(line.as_ptr()) };
    }
}

fn ask(request: &str) -> Option<String> {
    let request = CString::new(request).ok()?;
    // SAFETY: a NUL-terminated request; the answer is a malloc'ed C string or null.
    unsafe {
        let answer = nx_relay_ask(request.as_ptr());
        if answer.is_null() {
            return None;
        }
        let text = CStr::from_ptr(answer).to_string_lossy().into_owned();
        free(answer.cast());
        Some(text)
    }
}

fn http(request: &save_relay::http::Request, headers: &str) -> Option<Vec<u8>> {
    let method = CString::new(request.method.as_str()).ok()?;
    let url = CString::new(request.url.as_str()).ok()?;
    let headers = CString::new(headers).ok()?;
    let mut out: *mut u8 = std::ptr::null_mut();
    let mut len = 0usize;
    // SAFETY: every pointer lives through the call; the answer is malloc'ed by the callee.
    unsafe {
        let status = nx_relay_http(
            method.as_ptr(),
            url.as_ptr(),
            headers.as_ptr(),
            request.body.as_ptr(),
            request.body.len(),
            request.max_response_bytes.unwrap_or(0),
            &mut out,
            &mut len,
        );
        if out.is_null() {
            return None;
        }
        let wire = std::slice::from_raw_parts(out, len).to_vec();
        free(out.cast());
        (status == 0).then_some(wire)
    }
}

/// The app's network, as save-relay's `Transport`.
fn transport() -> Arc<dyn save_relay::http::Transport> {
    Arc::new(CallbackTransport(http))
}

/// save-relay over the Java host's network; its turn-off also clears a credential file that
/// does not read (`forget_android`).
struct AndroidLive<'a> {
    creds: &'a FileStore,
}

impl Connector for AndroidLive<'_> {
    fn open(&self, dirs: &AppDirs, creds: &dyn CredentialStore) -> Result<Option<SaveRelay>> {
        acnh_core::relay::open(dirs, creds, transport())
    }

    fn forget(&self, dirs: &AppDirs, _creds: &dyn CredentialStore) -> Result<()> {
        forget_android(dirs, self.creds)
    }
}

/// `relay.ask` through the player's settings: the C++ side is the only writer of `config.ini`.
/// `relay.ask` on Android: a file in the app's private folder (`android_ask_off_file`), which the
/// home screen's Turn off can write before any game folder is known, and the game folder's
/// `config.ini` too (the player's own settings, as on the iPad) when there is one.
struct PlayerSettings<'a> {
    files: &'a Path,
}

impl Settings for PlayerSettings<'_> {
    fn ask_off(&self) -> bool {
        // SAFETY: a plain call into the app, no pointers.
        android_ask_off_file(self.files).exists() || unsafe { nx_relay_ask_off() }
    }
    fn remember_not_now(&self) -> bool {
        let kept = remember_not_now(self.files);
        // SAFETY: as above.
        if !unsafe { nx_relay_remember_not_now() } {
            log("relay: relay.ask could not be written to the game folder's config.ini");
        }
        kept
    }
}

/// Not now (or Turn off) remembered in the app's private folder; false when it could not be.
fn remember_not_now(files: &Path) -> bool {
    let written = std::fs::create_dir_all(files)
        .and_then(|()| std::fs::write(android_ask_off_file(files), b"relay.ask = 0\n"));
    if written.is_err() {
        log("relay: Not now could not be remembered in the app's folder");
    }
    written.is_ok()
}

/// True once `before_game` has returned in this process: the push waits for it, and the game
/// screen sends the app back to the home screen without it (`acnh_relay_android_ready`).
static BOOTED: AtomicBool = AtomicBool::new(false);

/// True when that start pulled (sync on); a game started with sync off never pushes.
static PULLED: AtomicBool = AtomicBool::new(false);

fn path(raw: *const c_char) -> Option<PathBuf> {
    text(raw).filter(|t| !t.is_empty()).map(PathBuf::from)
}

fn text(raw: *const c_char) -> Option<String> {
    if raw.is_null() {
        return None;
    }
    // SAFETY: the caller passes a NUL-terminated string.
    let text = unsafe { CStr::from_ptr(raw) }.to_str().ok()?;
    Some(text.to_string())
}

fn store(files: &Path) -> FileStore {
    FileStore::new(android_credentials(files))
}

fn with_player(data_root: &Path, files: &Path, work: impl FnOnce(&Player<'_>)) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let creds = store(files);
        let prompt = BridgePrompt::new(ask);
        let player = Player {
            roots: Roots::android(data_root, files),
            creds: &creds,
            connector: &AndroidLive { creds: &creds },
            prompt: &prompt,
            booted: &BOOTED,
            pulled: &PULLED,
            settings: &PlayerSettings { files },
            log: Box::new(log),
        };
        work(&player);
    }));
    if result.is_err() {
        log("relay: stopped by a panic");
    }
}

/// Before the game starts, on the home screen's worker thread (`Relay.beforeGame`): returns when
/// the game may start (a pull has succeeded, save sync is not set up, or Play anyway was chosen
/// on a held island). Every other answer goes to the home screen's dialogs and back.
///
/// # Safety
/// Both arguments are NUL-terminated paths.
#[no_mangle]
pub unsafe extern "C" fn acnh_relay_android_before_game(
    data_root: *const c_char,
    files: *const c_char,
) {
    match (path(data_root), path(files)) {
        (Some(root), Some(files)) => with_player(&root, &files, |player| player.before_game()),
        _ => {
            // No game folder or files folder, so no saves to protect: nothing to sync.
            log("relay: no game folder");
            BOOTED.store(true, Ordering::SeqCst);
        }
    }
}

/// When the game quits (before the process ends): pushes the island and returns when the push
/// has finished or been left waiting for the next start. Does nothing before the game has booted.
///
/// # Safety
/// As `acnh_relay_android_before_game`.
#[no_mangle]
pub unsafe extern "C" fn acnh_relay_android_after_game(
    data_root: *const c_char,
    files: *const c_char,
) {
    if let (Some(root), Some(files)) = (path(data_root), path(files)) {
        with_player(&root, &files, |player| player.after_game());
    }
}

/// When the app goes to the background (`APP_CMD_PAUSE`): pushes on a worker thread and returns
/// at once. The game goes on, so it is a checkpoint (`Player::checkpoint`): this device stays
/// marked as the one playing and its session goes on. Does nothing before the game has booted.
///
/// # Safety
/// As `acnh_relay_android_before_game`.
#[no_mangle]
pub unsafe extern "C" fn acnh_relay_android_background(
    data_root: *const c_char,
    files: *const c_char,
) {
    let (Some(root), Some(files)) = (path(data_root), path(files)) else {
        return;
    };
    if !BOOTED.load(Ordering::SeqCst) {
        log("relay: no push yet: the game has not booted");
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("relay push".into())
        .spawn(move || with_player(&root, &files, |player| player.checkpoint()));
    if spawned.is_err() {
        log("relay: the background push could not start");
    }
}

/// 0: save sync is not set up; 1: set up, with its token; 2: set up, but the token is gone or
/// cannot be read (the next start asks to sign in again); 3: its config does not read (the next
/// start shows the error, with Turn off). Reads files only, no network.
fn state(files: &Path) -> c_int {
    let dirs = Roots::android(files, files).dirs();
    let config = match LocalStore::new(dirs).load_config() {
        Ok(Some(config)) => config,
        Ok(None) => return 0,
        // A config that does not read is not "off": the game waits for the pull (which shows the
        // error with Turn off), and the home screen keeps Turn off within reach.
        Err(_) => return 3,
    };
    let Ok(accounts) = save_relay::service::credential_accounts(&config) else {
        return 2;
    };
    let creds = store(files);
    match accounts.first().map(|account| creds.get(account)) {
        Some(Ok(Some(_))) => 1,
        _ => 2,
    }
}

/// Whether save sync is set up on this device, for the home screen (`state` above).
///
/// # Safety
/// `files` is a NUL-terminated path.
#[no_mangle]
pub unsafe extern "C" fn acnh_relay_android_state(files: *const c_char) -> c_int {
    let Some(files) = path(files) else { return 0 };
    std::panic::catch_unwind(|| state(&files)).unwrap_or(2)
}

/// Whether the game screen may start the game in this process: after `before_game` returned
/// here, or with save sync off and the first-start offer already opted out of (`relay.ask = 0`).
/// False sends it back to the home screen, which pulls or shows the first-start sync offer
/// (Android can bring the game screen back in a new process, past the home screen, e.g. after an
/// update from a download-only build).
///
/// # Safety
/// `files` is a NUL-terminated path.
#[no_mangle]
pub unsafe extern "C" fn acnh_relay_android_ready(files: *const c_char) -> bool {
    if BOOTED.load(Ordering::SeqCst) || PULLED.load(Ordering::SeqCst) {
        return true;
    }
    let Some(files) = path(files) else {
        return true;
    };
    std::panic::catch_unwind(|| {
        let sync_state = state(&files);
        let ask_off = sync_state == 0 && PlayerSettings { files: &files }.ask_off();
        android_ready(false, sync_state, ask_off)
    })
    .unwrap_or(false)
}

/// The app is back in front (`APP_CMD_RESUME`): a "playing here" mark a background push could
/// not write back is written now, on a worker (`Player::resume`).
///
/// # Safety
/// As `acnh_relay_android_before_game`.
#[no_mangle]
pub unsafe extern "C" fn acnh_relay_android_resume(data_root: *const c_char, files: *const c_char) {
    let (Some(root), Some(files)) = (path(data_root), path(files)) else {
        return;
    };
    if !PULLED.load(Ordering::SeqCst) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("relay mark".into())
        .spawn(move || with_player(&root, &files, |player| player.resume()));
}

/// "Turn off save sync" on the home screen: save-relay's config and every secret it filed here
/// are removed (`acnh_core::relay::forget`). The island on this device stays.
///
/// # Safety
/// `files` is a NUL-terminated path.
#[no_mangle]
pub unsafe extern "C" fn acnh_relay_android_forget(files: *const c_char) -> bool {
    let Some(files) = path(files) else {
        return false;
    };
    std::panic::catch_unwind(|| {
        let dirs = Roots::android(&files, &files).dirs();
        let remembered = remember_not_now(&files);
        match forget_android(&dirs, &store(&files)) {
            Ok(()) => {
                log("relay: save sync turned off on this device");
                remembered
            }
            Err(_) => {
                log("relay: save sync could not be turned off");
                false
            }
        }
    })
    .unwrap_or(false)
}

/// "Sign out of Dropbox" on the home screen: save sync and the game download's sign-in both end
/// here (`sign_out_android`). The island on this device stays.
///
/// # Safety
/// `files` is a NUL-terminated path.
#[no_mangle]
pub unsafe extern "C" fn acnh_relay_android_sign_out(files: *const c_char) -> bool {
    let Some(files) = path(files) else {
        return false;
    };
    std::panic::catch_unwind(|| {
        let dirs = Roots::android(&files, &files).dirs();
        let done = sign_out_android(&dirs, &store(&files)).is_ok();
        log(if done {
            "relay: signed out of Dropbox on this device (save sync and the download)"
        } else {
            "relay: the Dropbox sign-out left something behind"
        });
        done
    })
    .unwrap_or(false)
}

/// The Java host's `credentialGet` (save-relay's `cred_get`): 0 and the secret in `buf` (its
/// length in `*len`), 1 when there is none, 2 for a failure or a buffer too small.
///
/// # Safety
/// `files` and `account` are NUL-terminated; `buf` is writable for `cap` bytes; `len` is valid.
#[no_mangle]
pub unsafe extern "C" fn acnh_relay_android_cred_get(
    files: *const c_char,
    account: *const c_char,
    buf: *mut u8,
    cap: usize,
    len: *mut usize,
) -> c_int {
    let (Some(files), Some(account)) = (path(files), text(account)) else {
        return 2;
    };
    if buf.is_null() || len.is_null() {
        return 2;
    }
    match std::panic::catch_unwind(|| store(&files).get(&account)) {
        Ok(Ok(Some(secret))) => {
            let bytes = secret.expose().as_bytes();
            if bytes.len() > cap {
                return 2;
            }
            // SAFETY: `buf` holds `cap` bytes, and `bytes` fits.
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len());
            *len = bytes.len();
            0
        }
        Ok(Ok(None)) => 1,
        _ => 2,
    }
}

/// The Java host's `credentialSet` (save-relay's `cred_set`): 0 on success.
///
/// # Safety
/// All three are NUL-terminated strings.
#[no_mangle]
pub unsafe extern "C" fn acnh_relay_android_cred_set(
    files: *const c_char,
    account: *const c_char,
    secret: *const c_char,
) -> c_int {
    let (Some(files), Some(account), Some(secret)) = (path(files), text(account), text(secret))
    else {
        return 2;
    };
    match std::panic::catch_unwind(|| store(&files).set(&account, &Secret::new(secret))) {
        Ok(Ok(())) => 0,
        _ => 2,
    }
}

/// The Java host's `credentialDelete` (save-relay's `cred_delete`): 0 on success, a missing
/// account included.
///
/// # Safety
/// Both are NUL-terminated strings.
#[no_mangle]
pub unsafe extern "C" fn acnh_relay_android_cred_delete(
    files: *const c_char,
    account: *const c_char,
) -> c_int {
    let (Some(files), Some(account)) = (path(files), text(account)) else {
        return 2;
    };
    match std::panic::catch_unwind(|| store(&files).delete(&account)) {
        Ok(Ok(())) => 0,
        _ => 2,
    }
}
