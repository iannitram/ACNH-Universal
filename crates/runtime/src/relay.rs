//! save-relay inside the iPad's player (issue #172, ADR 0006) and the Android app (issue #81):
//! the app asks save-relay for the island before the game boots and pushes it when the game
//! quits or the app goes to the background. The adapter is `acnh_core::relay` (save-relay's Rust
//! API), as on the Mac and PC.
//!
//! This file holds everything that does not touch UIKit, the Keychain or JNI, so the tests run
//! it on every runner (`tests/ios_relay.rs` and `tests/android_relay.rs` include it with
//! `#[path]`): the pure decision (`decide`: a pull's answer in, an action out), the prompt and
//! connector traits, the loop (`Player::before_game`, `Player::after_game`), and the Android
//! app's parts that need no JNI (`FileStore`, the prompt bridge's JSON in `BridgePrompt`, the
//! wire format of its network answers in `parse_wire`). The UIKit alerts, the Keychain store and
//! the iPad's C exports are in `relay_ios.rs`; the Android C exports in `relay_android.rs`.
//! Nothing here may use `crate::`: the tests compile this file as one of their modules.
//!
//! The rules (the owner's answers of 2026-11-18, "Refuse until synced" and "Show sign-in
//! again"): the game boots only after a pull has succeeded, with save sync not set up, or after
//! Play anyway on a held island. The app password and the Dropbox tokens are only ever in the
//! credential store; no other file, log line or message holds them.

use acnh_core::config::Config;
use acnh_core::launch::{Keep, Relay, Sync};
use acnh_core::relay::SaveRelay;
use acnh_core::savecard::SaveCard;
use acnh_core::{saves, Error, Result};
use save_relay::creds::{CredentialStore, Secret};
use save_relay::local::LocalStore;
use save_relay::platform::AppDirs;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Mutex, TryLockError};

#[path = "relay_android_parts.rs"]
mod android_parts;
#[allow(unused_imports)]
pub use android_parts::*;

#[cfg(target_os = "ios")]
#[path = "relay_ios.rs"]
pub mod ios;

#[cfg(target_os = "android")]
#[path = "relay_android.rs"]
pub mod android;

/// The name this device goes by in save-relay's history ("ipad", "android"; never the owner's
/// device name).
#[cfg(not(target_os = "android"))]
pub const DEVICE: &str = "iPad";
/// The name this device goes by in save-relay's history ("ipad", "android"; never the owner's
/// device name).
#[cfg(target_os = "android")]
pub const DEVICE: &str = "Android";
/// The Android app's name in save-relay's history, on every target (the tests check it).
pub const ANDROID_DEVICE: &str = "Android";

/// What one pull or push means for the app. Pure: `decide` makes it, the loop acts on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Pulled, pushed, adopted or nothing to do: the game may boot.
    Play,
    /// Save sync is not set up (or off): the game boots with sync off.
    PlaySyncOff,
    /// save-relay could not be reached: the game does not boot; offer Retry.
    RefuseOffline,
    /// Both islands changed: show both, boot nothing until one is picked.
    PickConflict { server: SaveCard, local: SaveCard },
    /// Another device has the island open: name it; boot only on Play anyway.
    AskHeld { device: String, since: String },
    /// The server refused the sign-in (401 or 403): show the Nextcloud sign-in again.
    SignInAgain,
    /// Any other error: show its message (redacted by the caller) and offer Retry.
    ShowError(String),
}

/// A pull's or push's answer, as an action.
pub fn decide(result: &Result<Sync>) -> Action {
    match result {
        Ok(Sync::Done(_) | Sync::Resolved { .. }) => Action::Play,
        Ok(Sync::Off) => Action::PlaySyncOff,
        Ok(Sync::Offline) => Action::RefuseOffline,
        Ok(Sync::Conflict { server, local, .. }) => Action::PickConflict {
            server: server.clone(),
            local: local.clone(),
        },
        Ok(Sync::Held { device, since }) => Action::AskHeld {
            device: device.clone(),
            since: since.clone(),
        },
        Err(Error::RelaySignIn) => Action::SignInAgain,
        Err(Error::RelayUnreachable) => Action::RefuseOffline,
        Err(other) => Action::ShowError(other.to_string()),
    }
}

/// The Nextcloud sign-in form: the first start's (with Not now) or the one that comes back when
/// the server refuses the sign-in (server and user filled in, no Not now). The password field is
/// always empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInForm {
    pub server: String,
    pub user: String,
    pub allow_not_now: bool,
    /// Why the form is shown again, when it is (a refused password, an unreachable server).
    pub message: Option<String>,
}

/// What the person did with the sign-in form. `Debug` hides the password.
#[derive(Clone, PartialEq, Eq)]
pub enum SignInAnswer {
    Submit {
        server: String,
        user: String,
        password: String,
    },
    /// The app's own sign-in finished and set save sync up itself (Android: Dropbox in the
    /// browser, through save-relay's `save_relay_sign_in_start` / `_finish`). No secret passes
    /// through here: save-relay filed the refresh token in the credential store.
    SignedIn,
    /// The person chose Dropbox (the iPad: Nextcloud is the form's other choice). The player runs
    /// the code flow: `Connector::dropbox_start`, `Prompt::dropbox_code`, `Connector::dropbox_finish`.
    Dropbox,
    /// First start only: leave save sync off, and do not ask again (`relay.ask = 0`).
    NotNow,
    /// Close the form (only the sign-in-again form has no Not now).
    Cancel,
}

impl std::fmt::Debug for SignInAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignInAnswer::Submit { server, user, .. } => f
                .debug_struct("Submit")
                .field("server", server)
                .field("user", user)
                .field("password", &"***")
                .finish(),
            SignInAnswer::SignedIn => f.write_str("SignedIn"),
            SignInAnswer::Dropbox => f.write_str("Dropbox"),
            SignInAnswer::NotNow => f.write_str("NotNow"),
            SignInAnswer::Cancel => f.write_str("Cancel"),
        }
    }
}

/// Why the game is not booting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocked {
    /// save-relay could not be reached.
    Offline,
    /// Any other error, redacted.
    Error(String),
    /// The sign-in form was cancelled while save sync is set up.
    SignInCancelled,
}

/// What the person chose on a blocked screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedAnswer {
    Retry,
    /// Only offered after a cancelled sign-in.
    SignInAgain,
    /// Turn save sync off on this device and boot with sync off (`relay.ask = 0` too). Offered
    /// after a cancelled sign-in and on an error (an unreadable credential store, a token that is
    /// gone), never when offline: the person chose it, nothing chose it for them.
    TurnOff,
}

/// How the Dropbox code sign-in ended.
enum DropboxOutcome {
    Done,
    /// The person backed out of the code screen: the sign-in form is shown again.
    Back,
    /// The sign-in could not start (offline): the form again, saying why.
    Failed(String),
}

/// What the person chose on a held island.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldAnswer {
    PlayAnyway,
    CheckAgain,
}

/// The screens. UIKit alerts on the iPad (`relay_ios.rs`), the home screen's dialogs on Android
/// (`BridgePrompt`); a fake in the tests. Every method blocks until it is answered, on the game's
/// thread (Android: the home screen's worker).
pub trait Prompt: std::marker::Sync {
    fn sign_in(&self, form: &SignInForm) -> SignInAnswer;
    /// Dropbox's code flow, second half: show where to sign in (`url`, opened in the browser) and
    /// ask for the code Dropbox shows. `None`: the person backed out. Only the iPad offers it.
    fn dropbox_code(&self, url: &str) -> Option<String> {
        let _ = url;
        None
    }
    /// Both islands side by side; `Keep::Local` is this iPad's.
    fn conflict(&self, server: &SaveCard, local: &SaveCard) -> Keep;
    fn held(&self, device: &str, since: &str) -> HeldAnswer;
    /// The game does not boot; offer Retry (and Sign in again when `SignInCancelled`; Turn off
    /// save sync when `SignInCancelled` or `Error`, where the platform offers it).
    fn blocked(&self, why: &Blocked) -> BlockedAnswer;
}

/// How the app reaches save-relay: `Live` over ureq and rustls; the tests bring an in-memory
/// remote.
pub trait Connector: std::marker::Sync {
    /// `acnh_core::relay::open`: nothing set up is `Ok(None)`.
    fn open(&self, dirs: &AppDirs, creds: &dyn CredentialStore) -> Result<Option<SaveRelay>>;
    /// `acnh_core::relay::setup_nextcloud`: the password goes to `creds` only. The Android app
    /// signs in to Dropbox instead (`SignInAnswer::SignedIn`) and keeps this refusal.
    fn setup(
        &self,
        server: &str,
        user: &str,
        password: &str,
        dirs: &AppDirs,
        creds: &dyn CredentialStore,
    ) -> Result<()> {
        let _ = (server, user, password, dirs, creds);
        Err(Error::Saves(
            "this app signs in to its save sync in the browser, not with an app password".into(),
        ))
    }

    /// Start the Dropbox code sign-in (save-relay's Dropbox app, as the game download uses): the
    /// page to open in the browser. The iPad's Dropbox choice; the Android app signs in itself.
    fn dropbox_start(&self, dirs: &AppDirs, creds: &dyn CredentialStore) -> Result<String> {
        let _ = (dirs, creds);
        Err(Error::Saves("this app has no Dropbox code sign-in".into()))
    }

    /// Finish it with the code the person typed: save-relay's config and the refresh token (in
    /// `creds` only) are written before this returns.
    fn dropbox_finish(
        &self,
        code: &str,
        dirs: &AppDirs,
        creds: &dyn CredentialStore,
    ) -> Result<()> {
        let _ = (code, dirs, creds);
        Err(Error::Saves("this app has no Dropbox code sign-in".into()))
    }

    /// "Turn off save sync": `acnh_core::relay::forget` (best effort: the config goes even when a
    /// secret could not be deleted). The Android app also clears a credential file it cannot read.
    fn forget(&self, dirs: &AppDirs, creds: &dyn CredentialStore) -> Result<()> {
        acnh_core::relay::forget(dirs, creds)
    }
}

/// The real network: ureq with rustls through save-relay's `http` feature.
#[cfg(not(target_os = "android"))]
pub struct Live;

#[cfg(not(target_os = "android"))]
impl Connector for Live {
    fn open(&self, dirs: &AppDirs, creds: &dyn CredentialStore) -> Result<Option<SaveRelay>> {
        acnh_core::relay::open(
            dirs,
            creds,
            std::sync::Arc::new(save_relay::http::UreqTransport::new()),
        )
    }

    fn setup(
        &self,
        server: &str,
        user: &str,
        password: &str,
        dirs: &AppDirs,
        creds: &dyn CredentialStore,
    ) -> Result<()> {
        acnh_core::relay::setup_nextcloud(
            server,
            user,
            password,
            DEVICE,
            dirs,
            creds,
            std::sync::Arc::new(save_relay::http::UreqTransport::new()),
        )
    }

    fn dropbox_start(&self, _dirs: &AppDirs, creds: &dyn CredentialStore) -> Result<String> {
        acnh_core::relay::start_code_sign_in(
            save_relay::service::Service::Dropbox,
            creds,
            std::sync::Arc::new(save_relay::http::UreqTransport::new()),
        )
        .map(|prompt| prompt.url)
    }

    fn dropbox_finish(
        &self,
        code: &str,
        dirs: &AppDirs,
        creds: &dyn CredentialStore,
    ) -> Result<()> {
        acnh_core::relay::finish_sign_in(
            save_relay::service::Service::Dropbox,
            dirs,
            creds,
            std::sync::Arc::new(save_relay::http::UreqTransport::new()),
            code.trim(),
        )
    }
}

/// Where the player keeps things. `data_root` is the game folder (saves, settings, logs: on the
/// iPad the app's Documents folder, what Files shows). save-relay's own config and state go under
/// `library`, never into the data root.
#[derive(Debug, Clone)]
pub struct Roots {
    pub data_root: PathBuf,
    pub library: PathBuf,
    relay: AppDirs,
}

/// The folders the Android app hands save-relay's C interface as its `config` and `data`
/// (`SaveRelayHost`), under the app's private files folder: save-relay keeps its config and
/// state in `<these>/save-relay` (`AppDirs::from_vars`), where `Roots::android` looks too.
pub fn android_vars(files: &Path) -> (PathBuf, PathBuf) {
    (files.join("relay-config"), files.join("relay-data"))
}

/// The file a push that did not go leaves in save-relay's data folder; the next start sends it
/// before it pulls, unless save sync was turned off or signed in to anew since (both remove it:
/// the first sync after a sign-in is always a pull).
pub fn push_waiting_file(dirs: &AppDirs) -> PathBuf {
    dirs.data.join("push-waiting")
}

/// The Android app's "Not now" (`relay.ask = 0`), kept in its private files folder: it works
/// before any game folder is known (the home screen's Turn off), and the game folder's
/// `config.ini` gets the same key as well when one is known.
pub fn android_ask_off_file(files: &Path) -> PathBuf {
    files.join("relay-ask-off")
}

/// Whether the Android game screen (`NativeActivity`) may boot in this process without bouncing
/// back to `Home`.
///
/// - When `before_game` has already run in this process (`booted`), the game may boot (either a
///   pull succeeded, or sync is off and the first-start offer was already handled).
/// - Otherwise (Android restored `NativeActivity` in a new process, bypassing `Home` — for
///   example after an app update from a download-only build), only boot straight into the game
///   when save sync is plainly off (`state == 0`) AND the player has already opted out of the
///   first-start offer (`ask_off`). Otherwise return `false` so `NativeActivity` hands off to
///   `Home` to pull or show the first-start sync offer.
pub fn android_ready(booted: bool, state: i32, ask_off: bool) -> bool {
    booted || (state == 0 && ask_off)
}

/// The Android app's credential store file (`FileStore`), in its private files folder.
pub fn android_credentials(files: &Path) -> PathBuf {
    files.join("save-relay-credentials.json")
}

impl Roots {
    /// The iPad's: `<container>/Documents` and `<container>/Library`; save-relay's folders are
    /// both under `Library/save-relay`.
    pub fn for_data_root(data_root: &Path) -> Roots {
        let library = data_root
            .parent()
            .map_or_else(|| data_root.join("Library"), |p| p.join("Library"));
        let base = library.join("save-relay");
        Roots {
            data_root: data_root.to_path_buf(),
            relay: AppDirs {
                config: base.join("config"),
                data: base.join("data"),
            },
            library,
        }
    }

    /// The Android app's: the game folder it plays from, and its private files folder
    /// (`Context.getFilesDir()`), where save-relay's folders are the ones its C interface uses
    /// (`android_vars`), so a sign-in through that interface is what the loop opens.
    pub fn android(data_root: &Path, files: &Path) -> Roots {
        let (config, data) = android_vars(files);
        Roots {
            data_root: data_root.to_path_buf(),
            library: files.to_path_buf(),
            relay: AppDirs {
                config: config.join("save-relay"),
                data: data.join("save-relay"),
            },
        }
    }

    /// save-relay's folders.
    pub fn dirs(&self) -> AppDirs {
        self.relay.clone()
    }

    /// A push that did not go (offline, a conflict, an error) leaves this file; the next start
    /// sends it before it pulls.
    fn push_waiting(&self) -> PathBuf {
        push_waiting_file(&self.dirs())
    }

    /// A checkpoint pushed but could not mark this device as playing again; `Player::resume`
    /// retries it.
    fn mark_waiting(&self) -> PathBuf {
        self.dirs().data.join("mark-waiting")
    }
}

/// The one `relay.ask` setting (`0`: Not now was chosen, do not ask again). On the iPad it is the
/// player's own settings (`nx::config_set`, `nx::config_save`), so the C++ side stays the only
/// writer of `config.ini`: a file Rust wrote behind its back would be dropped by the player's
/// next `config_save`, which rewrites the file from what it loaded. `FileSettings` reads and
/// writes the file directly, for the tests.
pub trait Settings: std::marker::Sync {
    /// `relay.ask = 0`.
    fn ask_off(&self) -> bool;
    /// Set `relay.ask = 0` and save; false when it could not be saved.
    fn remember_not_now(&self) -> bool;
}

/// `Settings` straight on `<data root>/config.ini` (tests). A file that does not parse or read is
/// left exactly as it is.
#[allow(dead_code)] // the iPad uses the player's settings; only the tests use this
pub struct FileSettings(pub PathBuf);

impl FileSettings {
    fn load(&self) -> Option<Config> {
        match std::fs::read_to_string(&self.0) {
            Ok(text) => Config::parse(&text).ok(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(Config::default()),
            Err(_) => None,
        }
    }
}

impl Settings for FileSettings {
    fn ask_off(&self) -> bool {
        self.load()
            .is_some_and(|ini| ini.get("relay.ask") == Some("0"))
    }

    fn remember_not_now(&self) -> bool {
        let Some(mut ini) = self.load() else {
            return false;
        };
        ini.set("relay.ask", "0");
        let partial = self.0.with_extension("ini.partial");
        if self
            .0
            .parent()
            .is_some_and(|p| std::fs::create_dir_all(p).is_err())
            || std::fs::write(&partial, ini.to_text()).is_err()
        {
            return false;
        }
        if std::fs::rename(&partial, &self.0).is_err() {
            let _ = std::fs::remove_file(&partial);
            return false;
        }
        true
    }
}

/// What the Saves page's Sync now came to (issue #260). It only ever uploads, as the background
/// checkpoint does (`Player::sync_now`); the codes are what the C exports hand the settings
/// screen (`plat::SaveSync`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncNow {
    /// A new revision of the island went up.
    Synced = 1,
    /// The server already had this island: nothing new to upload.
    NothingNew = 2,
    /// The server has a newer island from another device (it moved on, both changed, or another
    /// device has it open): nothing was overwritten, and the next start shows the choice.
    Newer = 3,
    /// The island went up, but another device has opened it since (Play anyway there).
    TakenOver = 4,
    /// Save sync is off on this device, or this game started with it off.
    Off = 5,
    /// Save sync is set up, but its sign-in is gone or refused: the next start signs in again.
    SignedOut = 6,
    /// The server could not be reached: the push waits for the next start.
    Offline = 7,
    /// Any other error: the push waits for the next start.
    Error = 8,
}

impl SyncNow {
    /// The code the C exports hand over.
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// Whether the Saves page's Sync now can run now; the codes are the C exports' (`plat::SaveSync`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncReady {
    Ready = 0,
    /// Save sync is not set up (or its settings do not read).
    Off = 1,
    /// This game started with save sync off: it never pushes (the next start pulls first).
    StartedOff = 2,
    /// A push (the background one, the quit's, or a Sync now) is running.
    Busy = 3,
    /// Set up, but the sign-in is gone: pressing it says so.
    SignedOut = 4,
    /// The game has not started yet.
    NotBooted = 5,
}

impl SyncReady {
    /// The code the C exports hand over.
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// True while a sync holds the process's one-at-a-time lock (a push, a pull, a mark written back).
pub fn sync_running() -> bool {
    matches!(PUSH_LOCK.try_lock(), Err(TryLockError::WouldBlock))
}

/// The one final push of the process (the game's end, `SDL_EVENT_QUIT`, `SDL_EVENT_TERMINATING`
/// can all ask for it): the first caller claims it, every other caller finds it claimed and
/// returns at once, so no thread ever waits on a push another one runs.
pub struct FinalPush {
    claimed: AtomicBool,
}

/// What a final push came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bounded {
    /// The push finished (inside the bound, when there was one).
    Done,
    /// It was not done in time. The worker is left running, but the process may end any moment
    /// (iOS ends it when `applicationWillTerminate:` returns), so the island stays marked
    /// waiting (`Player::push_island` marks it before it waits for its turn) and the next start
    /// pushes it.
    Late,
    /// Another caller had claimed the final push (running or done): nothing was started.
    Taken,
}

impl FinalPush {
    pub const fn new() -> FinalPush {
        FinalPush {
            claimed: AtomicBool::new(false),
        }
    }

    /// True once a caller has claimed it.
    pub fn is_claimed(&self) -> bool {
        self.claimed.load(Ordering::SeqCst)
    }

    /// True for the first caller only.
    pub fn claim(&self) -> bool {
        !self.claimed.swap(true, Ordering::SeqCst)
    }

    /// `work` to the end on the calling thread, unless the push is claimed already (then
    /// `Taken`, at once).
    pub fn run(&self, work: impl FnOnce()) -> Bounded {
        if !self.claim() {
            return Bounded::Taken;
        }
        work();
        Bounded::Done
    }

    /// `work` on a worker thread, waited for at most `bound`: for a thread that must not block
    /// (the main thread while iOS ends the app, which gives it about five seconds). `Late`
    /// leaves the worker running; nothing promises it the time to finish.
    pub fn run_bounded(
        &self,
        bound: std::time::Duration,
        work: impl FnOnce() + Send + 'static,
    ) -> Bounded {
        if !self.claim() {
            return Bounded::Taken;
        }
        let (done, finished) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("relay final push".into())
            .spawn(move || {
                work();
                let _ = done.send(());
            });
        if spawned.is_err() {
            return Bounded::Late;
        }
        match finished.recv_timeout(bound) {
            // A worker that panicked drops its sender: also over.
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Bounded::Done,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Bounded::Late,
        }
    }
}

impl Default for FinalPush {
    fn default() -> Self {
        FinalPush::new()
    }
}

/// One Sync now at a time, run on a worker thread, and its last outcome for the screen to poll.
pub struct SyncNowSlot {
    running: AtomicBool,
    /// -1 while running, 0 before the first, else a `SyncNow` code.
    outcome: AtomicI32,
}

impl Default for SyncNowSlot {
    fn default() -> Self {
        SyncNowSlot::new()
    }
}

impl SyncNowSlot {
    pub const fn new() -> SyncNowSlot {
        SyncNowSlot {
            running: AtomicBool::new(false),
            outcome: AtomicI32::new(0),
        }
    }

    pub fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// `work` on a new thread, unless a Sync now is already running. False when it did not start
    /// (one is running, or the thread could not be made: then the outcome is `Error`).
    pub fn start(&'static self, work: impl FnOnce() -> SyncNow + Send + 'static) -> bool {
        if self.running.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.outcome.store(-1, Ordering::SeqCst);
        let spawned = std::thread::Builder::new()
            .name("relay sync now".into())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                    .unwrap_or(SyncNow::Error);
                self.outcome.store(outcome.code(), Ordering::SeqCst);
                self.running.store(false, Ordering::SeqCst);
            });
        if spawned.is_err() {
            self.outcome.store(SyncNow::Error.code(), Ordering::SeqCst);
            self.running.store(false, Ordering::SeqCst);
            return false;
        }
        true
    }

    /// -1 while running, 0 before the first, else the last `SyncNow` code.
    pub fn poll(&self) -> i32 {
        self.outcome.load(Ordering::SeqCst)
    }
}

/// The status C exports' out-parameters (`acnh_relay_android_sync_status`, `acnh_relay_sync_status`).
pub struct StatusOut {
    pub ready: *mut std::ffi::c_int,
    pub outcome: *mut std::ffi::c_int,
    pub last: *mut u64,
    /// `cap` bytes for the service's name, NUL-terminated (cut short when it does not fit).
    pub service: *mut std::ffi::c_char,
    pub cap: usize,
}

impl StatusOut {
    /// Each one written where its pointer is not null.
    ///
    /// # Safety
    /// Each pointer is null or valid for a write; `service` holds `cap` bytes.
    pub unsafe fn write(&self, ready: i32, outcome: i32, last: u64, service: &str) {
        if !self.ready.is_null() {
            *self.ready = ready;
        }
        if !self.outcome.is_null() {
            *self.outcome = outcome;
        }
        if !self.last.is_null() {
            *self.last = last;
        }
        if !self.service.is_null() && self.cap > 0 {
            let bytes = service.as_bytes();
            let n = bytes.len().min(self.cap - 1);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.service.cast::<u8>(), n);
            *self.service.add(n) = 0;
        }
    }
}

/// Now, in Unix seconds.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Everything the loop needs, borrowed.
pub struct Player<'a> {
    pub roots: Roots,
    pub creds: &'a (dyn CredentialStore + std::marker::Sync),
    pub connector: &'a dyn Connector,
    pub prompt: &'a dyn Prompt,
    /// False until `before_game` has returned; `after_game` does nothing before that (the player
    /// is not playing yet, and its island may be unsynced).
    pub booted: &'a AtomicBool,
    /// True when `before_game` let the game start after a pull (sync on), false when it started
    /// with sync off. Only a game that was pulled pushes: one started with sync off and set up
    /// meanwhile (the home screen's sign-in while it runs) syncs from its next start, which
    /// pulls first.
    pub pulled: &'a AtomicBool,
    /// Where `relay.ask` lives.
    pub settings: &'a dyn Settings,
    /// When the island last synced in this process (Unix seconds, 0: not yet): the pull that let
    /// the game start, then every push that went or found nothing new. The Saves page shows it.
    pub synced: &'a AtomicU64,
    /// One line per event, never holding a password.
    pub log: Box<dyn Fn(&str) + std::marker::Sync + 'a>,
}

enum Opened {
    Sync(Box<SaveRelay>),
    /// Never set up.
    Off,
    /// Set up (a config is there) but the password is gone from the credential store.
    NeedsSignIn {
        server: String,
        user: String,
    },
}

enum Attempt {
    /// The game may boot.
    Boot,
    /// Go round again (a retry, a picked conflict side, a held answer, a push that went).
    Again,
    SignIn {
        server: String,
        user: String,
    },
}

static PUSH_LOCK: Mutex<()> = Mutex::new(());

impl Player<'_> {
    /// Returns only when the game may boot: after a pull has succeeded, with save sync not set
    /// up, or after Play anyway on a held island. Every other answer comes back to the prompt.
    pub fn before_game(&self) {
        let pulled = loop {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.sync_first())) {
                Ok(pulled) => break pulled,
                Err(_) => {
                    // Refuse until synced: a panic never boots the game. Show it and go again;
                    // with no prompt to show it on, stop the app rather than boot.
                    self.log("save sync stopped by a panic");
                    let shown = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        self.prompt
                            .blocked(&Blocked::Error("Save sync stopped unexpectedly.".into()))
                    }));
                    match shown {
                        Ok(BlockedAnswer::TurnOff) => {
                            // The way out of a setup that panics every time: forget it, so the
                            // next round finds sync off. A panic in the forgetting changes
                            // nothing; the alert comes back.
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                self.turn_off()
                            }));
                        }
                        Ok(_) => {}
                        Err(_) => std::process::abort(),
                    }
                }
            }
        };
        if pulled {
            self.synced.store(unix_now(), Ordering::SeqCst);
        }
        self.pulled.store(pulled, Ordering::SeqCst);
        self.booted.store(true, Ordering::SeqCst);
    }

    /// True once a pull let the game start, false when it starts with sync off.
    fn sync_first(&self) -> bool {
        let mut resolve = None;
        let mut anyway = false;
        loop {
            match self.opened() {
                Err(error) => {
                    let message = self.say(&error, None);
                    self.log(&format!("save sync could not be opened: {message}"));
                    let answer = self.prompt.blocked(&Blocked::Error(message));
                    self.answered(answer);
                }
                Ok(Opened::Off) => {
                    if !self.first_start() {
                        self.log("save sync is off");
                        return false;
                    }
                }
                Ok(Opened::NeedsSignIn { server, user }) => self.sign_in_again(&server, &user),
                Ok(Opened::Sync(relay)) => match self.attempt(&relay, &mut resolve, &mut anyway) {
                    Attempt::Boot => return true,
                    Attempt::Again => {}
                    Attempt::SignIn { server, user } => self.sign_in_again(&server, &user),
                },
            }
        }
    }

    /// What a blocked screen's answer asks beyond going round again.
    fn answered(&self, answer: BlockedAnswer) {
        if answer == BlockedAnswer::TurnOff {
            self.turn_off();
        }
    }

    /// The person turned save sync off on a blocked screen: save-relay's config and secrets go
    /// (best effort), a waiting push is dropped, and Not now is remembered so the first start's
    /// offer does not come straight back. The loop then finds sync off and boots.
    fn turn_off(&self) {
        match self.connector.forget(&self.roots.dirs(), self.creds) {
            Ok(()) => self.log("save sync turned off on this device"),
            Err(error) => {
                let message = self.say(&error, None);
                self.log(&format!("save sync turned off, not cleanly: {message}"));
            }
        }
        self.set_waiting(false);
        if !self.settings.remember_not_now() {
            self.log("Not now could not be remembered");
        }
    }

    /// Push the island when the game quits. A push that does not go (offline, a conflict, any
    /// error) is left waiting for the next start.
    pub fn after_game(&self) {
        self.push_island(false);
    }

    /// Push the island while the game keeps running (the app went to the background): as
    /// `after_game`, but this device stays marked as the one playing and its play session goes
    /// on (`SaveRelay::checkpoint`), so the held-island guard holds and the quit's card counts
    /// the whole session.
    pub fn checkpoint(&self) {
        self.push_island(true);
    }

    /// The Saves page's Sync now (issue #260): the background checkpoint's push, on demand, while
    /// the game runs. It only uploads (never a pull mid-game), this device stays marked as the
    /// one playing, and a newer island on the server is left for the next start to settle.
    pub fn sync_now(&self) -> SyncNow {
        self.log("Sync now");
        // The game is live: a save folder without an island would make save-relay's push pull
        // the server's island into it (its plan for "local empty, server not"), under a running
        // game. Nothing is sent or fetched until the game has written its first save.
        let dir = saves::save_dir(&self.roots.data_root);
        if !saves::REQUIRED.iter().all(|f| dir.join(f).is_file()) {
            self.log("no Sync now: the game has not saved an island yet");
            return SyncNow::NothingNew;
        }
        self.push_island(true)
    }

    /// Whether Sync now can run: the game started after a pull, save sync is still set up, and
    /// no push is running (`busy`: the caller's Sync now, or `sync_running`). Reads the config
    /// and the credential store only, no network.
    pub fn sync_now_ready(&self, busy: bool) -> SyncReady {
        if !self.booted.load(Ordering::SeqCst) {
            return SyncReady::NotBooted;
        }
        if !self.pulled.load(Ordering::SeqCst) {
            return SyncReady::StartedOff;
        }
        if busy {
            return SyncReady::Busy;
        }
        let config = match LocalStore::new(self.roots.dirs()).load_config() {
            Ok(Some(config)) => config,
            Ok(None) | Err(_) => return SyncReady::Off,
        };
        let signed_in = save_relay::service::credential_accounts(&config)
            .ok()
            .and_then(|accounts| accounts.first().cloned())
            .is_some_and(|account| matches!(self.creds.get(&account), Ok(Some(_))));
        if signed_in {
            SyncReady::Ready
        } else {
            SyncReady::SignedOut
        }
    }

    /// The service save sync uses here, as a person reads it ("Dropbox", "Nextcloud"), when its
    /// config reads.
    pub fn service_label(&self) -> Option<&'static str> {
        let (server, _) = self.sync_config()?;
        save_relay::service::Service::from_server(&server)
            .ok()
            .map(|service| service.label())
    }

    fn push_island(&self, checkpoint: bool) -> SyncNow {
        if !self.booted.load(Ordering::SeqCst) {
            self.log("no push yet: the game has not booted");
            return SyncNow::Off;
        }
        if !self.pulled.load(Ordering::SeqCst) {
            // Sync was off when this game started (and may have been set up since): its island
            // was never pulled, so it is not pushed either; the next start pulls first.
            self.log("no push: this game started with save sync off");
            return SyncNow::Off;
        }
        // The final push is marked waiting before it queues behind a running checkpoint or Sync
        // now (PUSH_LOCK), so a process that ends while it waits still leaves the mark.
        let was_waiting = self.roots.push_waiting().exists();
        if !checkpoint {
            self.set_waiting(true);
        }
        let _one_at_a_time = PUSH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let relay = match self.opened() {
            Ok(Opened::Sync(relay)) => relay,
            Ok(Opened::Off) => {
                // Nothing to send: the mark is as it was before this push.
                self.set_waiting(was_waiting);
                return SyncNow::Off;
            }
            // Set up, but the password cannot be read (the credential store is out of reach, or
            // the item is gone): the push waits for the next start, which sends it first.
            Ok(Opened::NeedsSignIn { .. }) => {
                self.set_waiting(true);
                self.log("the push is waiting: the app password cannot be read");
                return SyncNow::SignedOut;
            }
            Err(error) => {
                let message = self.say(&error, None);
                self.set_waiting(true);
                self.log(&format!(
                    "the push is waiting, save sync could not be opened: {message}"
                ));
                return SyncNow::Error;
            }
        };
        let dir = saves::save_dir(&self.roots.data_root);
        // The final push stays marked waiting (set above) until it went: a process that is
        // killed mid-upload (iOS ends the app while it runs) leaves the mark, so the next start
        // sends the island first.
        // What a push that went comes to: a new revision, nothing new, or (a checkpoint only) an
        // island that moved on or was taken over meanwhile.
        let mut went = SyncNow::Synced;
        let result = if checkpoint {
            relay.checkpoint(&dir).map(|done| {
                self.remarked(&done.remark);
                went = match done.remark {
                    acnh_core::relay::Remark::MovedOn { .. } => SyncNow::Newer,
                    acnh_core::relay::Remark::Elsewhere { .. } => SyncNow::TakenOver,
                    _ if done.pushed => SyncNow::Synced,
                    _ => SyncNow::NothingNew,
                };
                done.sync
            })
        } else {
            self.set_flag(&self.roots.mark_waiting(), false);
            relay.push(&dir, None)
        };
        match decide(&result) {
            Action::Play => {
                self.set_waiting(false);
                if went != SyncNow::Newer {
                    self.synced.store(unix_now(), Ordering::SeqCst);
                }
                self.log(if checkpoint {
                    "pushed the island (the game goes on)"
                } else {
                    "pushed the island"
                });
                went
            }
            Action::PlaySyncOff => {
                self.set_waiting(false);
                self.log("no push: save sync is off");
                SyncNow::Off
            }
            other => {
                self.set_waiting(true);
                self.log(&format!(
                    "the push is waiting for the next start: {other:?}"
                ));
                match other {
                    Action::PickConflict { .. } | Action::AskHeld { .. } => SyncNow::Newer,
                    Action::RefuseOffline => SyncNow::Offline,
                    Action::SignInAgain => SyncNow::SignedOut,
                    _ => SyncNow::Error,
                }
            }
        }
    }

    /// What became of this device's "playing here" mark after a checkpoint.
    fn remarked(&self, remark: &acnh_core::relay::Remark) {
        use acnh_core::relay::Remark;
        match remark {
            Remark::Kept | Remark::Untouched => self.set_flag(&self.roots.mark_waiting(), false),
            Remark::Elsewhere { device, since } => {
                self.set_flag(&self.roots.mark_waiting(), false);
                self.log(&format!(
                    "another device ({device}, since {since}) has taken the island over while this game runs"
                ));
            }
            Remark::MovedOn { revision } => {
                self.set_flag(&self.roots.mark_waiting(), false);
                self.log(&format!(
                    "the island moved on on the server (revision {revision}) while this game was in the background: this device is no longer marked as playing, and its next push meets the newer island"
                ));
            }
            Remark::Failed(why) => {
                self.set_flag(&self.roots.mark_waiting(), true);
                let why = self.redact(why, None);
                self.log(&format!(
                    "pushed, but this device could not be marked as playing again ({why}): retried when the app is back"
                ));
            }
        }
    }

    /// The app is back in front: a "playing here" mark a checkpoint could not write back is
    /// written now (only where no other device's is). Nothing else happens here.
    pub fn resume(&self) {
        if !self.booted.load(Ordering::SeqCst)
            || !self.pulled.load(Ordering::SeqCst)
            || !self.roots.mark_waiting().exists()
        {
            return;
        }
        let _one_at_a_time = PUSH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Again under the lock: a quit's push that got it first has ended the session, and its
        // mark-waiting flag with it; no mark comes after that.
        if !self.roots.mark_waiting().exists() {
            return;
        }
        if let Ok(Opened::Sync(relay)) = self.opened() {
            match relay.remark() {
                Ok(remark) => self.remarked(&remark),
                Err(error) => {
                    let message = self.say(&error, None);
                    self.log(&format!("the mark is still waiting: {message}"));
                }
            }
        }
    }

    fn attempt(&self, relay: &SaveRelay, resolve: &mut Option<Keep>, anyway: &mut bool) -> Attempt {
        let dir = saves::save_dir(&self.roots.data_root);
        let waiting = self.roots.push_waiting().exists();
        // A push still waiting from the last game goes first; then the pull.
        // One sync at a time in the process: a quit's push still running (a restart) finishes first.
        let one_at_a_time = PUSH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let result = if waiting {
            relay.push(&dir, *resolve)
        } else {
            // After Play anyway a conflict can still come: its answer goes with `anyway`.
            relay.pull_with(&dir, *resolve, *anyway)
        };
        *resolve = None;
        drop(one_at_a_time);
        match decide(&result) {
            Action::Play | Action::PlaySyncOff if waiting => {
                self.set_waiting(false);
                self.log("sent the push that was waiting");
                Attempt::Again
            }
            Action::Play | Action::PlaySyncOff => {
                self.log("pulled: the game may boot");
                Attempt::Boot
            }
            Action::RefuseOffline => {
                self.log("save-relay could not be reached: the game waits");
                self.prompt.blocked(&Blocked::Offline);
                Attempt::Again
            }
            Action::PickConflict { server, local } => {
                self.log("both islands changed: asking which to keep");
                *resolve = Some(self.prompt.conflict(&server, &local));
                Attempt::Again
            }
            Action::AskHeld { device, since } if !waiting => {
                self.log("another device has the island open");
                if self.prompt.held(&device, &since) == HeldAnswer::PlayAnyway {
                    *anyway = true;
                }
                Attempt::Again
            }
            Action::AskHeld { .. } => {
                let answer = self.prompt.blocked(&Blocked::Error(
                    "save-relay answered that another device has the island open.".into(),
                ));
                self.answered(answer);
                Attempt::Again
            }
            Action::SignInAgain => {
                self.log("the server refused the sign-in");
                match self.sync_config() {
                    Some((server, user)) => Attempt::SignIn { server, user },
                    None => Attempt::SignIn {
                        server: String::new(),
                        user: String::new(),
                    },
                }
            }
            Action::ShowError(text) => {
                let message = self.redact(&text, None);
                self.log(&format!("save sync error: {message}"));
                let answer = self.prompt.blocked(&Blocked::Error(message));
                self.answered(answer);
                Attempt::Again
            }
        }
    }

    /// The first start's form (with Not now). True once set up, so the loop opens again.
    fn first_start(&self) -> bool {
        if self.settings.ask_off() {
            return false;
        }
        let mut form = SignInForm {
            server: "https://".into(),
            user: String::new(),
            allow_not_now: true,
            message: None,
        };
        loop {
            match self.prompt.sign_in(&form) {
                SignInAnswer::NotNow => {
                    if !self.settings.remember_not_now() {
                        self.log("Not now could not be remembered");
                    }
                    return false;
                }
                // Closed without an answer: sync stays off for this start, and it asks again.
                SignInAnswer::Cancel => return false,
                SignInAnswer::SignedIn => {
                    // A new sign-in starts with a pull, whatever push an older setup left.
                    self.set_waiting(false);
                    self.log("save sync is set up");
                    return true;
                }
                SignInAnswer::Dropbox => match self.dropbox() {
                    DropboxOutcome::Done => {
                        self.set_waiting(false);
                        self.log("save sync is set up (Dropbox)");
                        return true;
                    }
                    DropboxOutcome::Back => {}
                    DropboxOutcome::Failed(message) => form.message = Some(message),
                },
                SignInAnswer::Submit {
                    server,
                    user,
                    password,
                } => match self.setup(&server, &user, &password) {
                    Ok(()) => {
                        self.set_waiting(false);
                        self.log("save sync is set up");
                        return true;
                    }
                    Err(error) => {
                        form = SignInForm {
                            server,
                            user,
                            allow_not_now: true,
                            message: Some(self.say(&error, Some(&password))),
                        };
                    }
                },
            }
        }
    }

    /// The sign-in form again, with the server and user filled in and no Not now. Returns when
    /// a new password is set up, or when the person cancels and picks Retry.
    fn sign_in_again(&self, server: &str, user: &str) {
        let mut form = SignInForm {
            server: server.to_string(),
            user: user.to_string(),
            allow_not_now: false,
            message: Some(sign_in_again_message(server)),
        };
        loop {
            match self.prompt.sign_in(&form) {
                SignInAnswer::SignedIn => {
                    self.set_waiting(false);
                    self.log("signed in again");
                    return;
                }
                SignInAnswer::Dropbox => match self.dropbox() {
                    DropboxOutcome::Done => {
                        self.set_waiting(false);
                        self.log("signed in to Dropbox again");
                        return;
                    }
                    DropboxOutcome::Back => {}
                    DropboxOutcome::Failed(message) => form.message = Some(message),
                },
                SignInAnswer::Submit {
                    server,
                    user,
                    password,
                } => match self.setup(&server, &user, &password) {
                    Ok(()) => {
                        self.set_waiting(false);
                        self.log("a new app password is set up");
                        return;
                    }
                    Err(error) => {
                        form = SignInForm {
                            server,
                            user,
                            allow_not_now: false,
                            message: Some(self.say(&error, Some(&password))),
                        };
                    }
                },
                SignInAnswer::NotNow | SignInAnswer::Cancel => {
                    match self.prompt.blocked(&Blocked::SignInCancelled) {
                        BlockedAnswer::Retry => return,
                        BlockedAnswer::SignInAgain => {}
                        BlockedAnswer::TurnOff => {
                            self.turn_off();
                            return;
                        }
                    }
                }
            }
        }
    }

    /// Dropbox's code sign-in: the page, then the code. A code Dropbox refuses (or a page that
    /// cannot start, offline) goes back to the form with the reason, and choosing Dropbox again
    /// starts a new page: a used code is gone, so asking for another on the old page would fail.
    fn dropbox(&self) -> DropboxOutcome {
        let dirs = self.roots.dirs();
        let url = match self.connector.dropbox_start(&dirs, self.creds) {
            Ok(url) => url,
            Err(error) => return DropboxOutcome::Failed(self.say(&error, None)),
        };
        let Some(code) = self.prompt.dropbox_code(&url) else {
            return DropboxOutcome::Back;
        };
        let code = code.trim().to_string();
        match self.connector.dropbox_finish(&code, &dirs, self.creds) {
            Ok(()) => DropboxOutcome::Done,
            Err(error) => DropboxOutcome::Failed(self.say(&error, Some(&code))),
        }
    }

    fn setup(&self, server: &str, user: &str, password: &str) -> Result<()> {
        self.connector
            .setup(server, user, password, &self.roots.dirs(), self.creds)
    }

    fn opened(&self) -> Result<Opened> {
        let dirs = self.roots.dirs();
        let config = LocalStore::new(dirs.clone())
            .load_config()
            .map_err(|e| Error::Saves(e.to_string()))?;
        match self.connector.open(&dirs, self.creds)? {
            Some(relay) => Ok(Opened::Sync(Box::new(relay))),
            None => Ok(match config {
                Some(config) => Opened::NeedsSignIn {
                    server: config.server,
                    user: config.user,
                },
                None => Opened::Off,
            }),
        }
    }

    fn sync_config(&self) -> Option<(String, String)> {
        let config = LocalStore::new(self.roots.dirs()).load_config().ok()??;
        Some((config.server, config.user))
    }

    fn set_waiting(&self, waiting: bool) {
        self.set_flag(&self.roots.push_waiting(), waiting);
    }

    fn set_flag(&self, path: &Path, on: bool) {
        let path = path.to_path_buf();
        if on {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, b"1");
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }

    /// An error as the person reads it: no home path, and never the password just typed.
    fn say(&self, error: &Error, typed: Option<&str>) -> String {
        self.redact(&error.to_string(), typed)
    }

    fn redact(&self, text: &str, typed: Option<&str>) -> String {
        let mut text = text.to_string();
        if let Some(password) = typed.filter(|p| !p.is_empty()) {
            text = text.replace(password, "***");
        }
        let home = self.roots.library.parent().unwrap_or(&self.roots.library);
        acnh_core::prepare::redact(&text, home)
    }

    fn log(&self, line: &str) {
        (self.log)(&format!("relay: {line}"));
    }
}

/// A card as the conflict alert shows it: the device, when it was saved (UTC) and the play time.
pub fn describe_card(card: &SaveCard) -> String {
    let device = if card.device.is_empty() {
        "unknown device"
    } else {
        &card.device
    };
    let hours = card.play_seconds / 3600;
    let minutes = card.play_seconds % 3600 / 60;
    let played = if card.play_seconds == 0 {
        "play time unknown".to_string()
    } else {
        format!("played {hours} h {minutes} min")
    };
    format!("{device}, saved {}, {played}", utc(card.saved_unix))
}

/// `2026-11-18 07:15 UTC` from Unix seconds (civil-from-days, no time library).
fn utc(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let rest = unix % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        rest / 3600,
        rest % 3600 / 60
    )
}

/// What the sign-in form says when the service refused this device's sign-in: an app password
/// for Nextcloud (the iPad), a new browser sign-in for Dropbox or Google Drive (Android).
pub fn sign_in_again_message(server: &str) -> String {
    match save_relay::service::Service::from_server(server) {
        Ok(service) if service.uses_oauth() => format!(
            "{} refused this device's sign-in. Sign in again to play.",
            service.label()
        ),
        _ => "The server refused the sign-in. Enter a new app password to play.".into(),
    }
}
