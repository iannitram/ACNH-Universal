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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

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
        self.dirs().data.join("push-waiting")
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

/// Everything the loop needs, borrowed.
pub struct Player<'a> {
    pub roots: Roots,
    pub creds: &'a (dyn CredentialStore + std::marker::Sync),
    pub connector: &'a dyn Connector,
    pub prompt: &'a dyn Prompt,
    /// False until `before_game` has returned; `after_game` does nothing before that (the player
    /// is not playing yet, and its island may be unsynced).
    pub booted: &'a AtomicBool,
    /// Where `relay.ask` lives.
    pub settings: &'a dyn Settings,
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
        loop {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.sync_first())) {
                Ok(()) => break,
                Err(_) => {
                    // Refuse until synced: a panic never boots the game. Show it and go again;
                    // with no prompt to show it on, stop the app rather than boot.
                    self.log("save sync stopped by a panic");
                    let shown = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        self.prompt
                            .blocked(&Blocked::Error("Save sync stopped unexpectedly.".into()))
                    }));
                    if shown.is_err() {
                        std::process::abort();
                    }
                }
            }
        }
        self.booted.store(true, Ordering::SeqCst);
    }

    fn sync_first(&self) {
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
                        return;
                    }
                }
                Ok(Opened::NeedsSignIn { server, user }) => self.sign_in_again(&server, &user),
                Ok(Opened::Sync(relay)) => match self.attempt(&relay, &mut resolve, &mut anyway) {
                    Attempt::Boot => return,
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

    fn push_island(&self, checkpoint: bool) {
        if !self.booted.load(Ordering::SeqCst) {
            self.log("no push yet: the game has not booted");
            return;
        }
        let _one_at_a_time = PUSH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let relay = match self.opened() {
            Ok(Opened::Sync(relay)) => relay,
            Ok(Opened::Off) => return,
            // Set up, but the password cannot be read (the credential store is out of reach, or
            // the item is gone): the push waits for the next start, which sends it first.
            Ok(Opened::NeedsSignIn { .. }) => {
                self.set_waiting(true);
                self.log("the push is waiting: the app password cannot be read");
                return;
            }
            Err(error) => {
                let message = self.say(&error, None);
                self.set_waiting(true);
                self.log(&format!(
                    "the push is waiting, save sync could not be opened: {message}"
                ));
                return;
            }
        };
        let dir = saves::save_dir(&self.roots.data_root);
        let result = if checkpoint {
            relay.checkpoint(&dir)
        } else {
            relay.push(&dir, None)
        };
        match decide(&result) {
            Action::Play | Action::PlaySyncOff => {
                self.set_waiting(false);
                self.log(if checkpoint {
                    "pushed the island (the game goes on)"
                } else {
                    "pushed the island"
                });
            }
            other => {
                self.set_waiting(true);
                self.log(&format!(
                    "the push is waiting for the next start: {other:?}"
                ));
            }
        }
    }

    fn attempt(&self, relay: &SaveRelay, resolve: &mut Option<Keep>, anyway: &mut bool) -> Attempt {
        let dir = saves::save_dir(&self.roots.data_root);
        let waiting = self.roots.push_waiting().exists();
        // A push still waiting from the last game goes first; then the pull.
        let result = if waiting {
            relay.push(&dir, *resolve)
        } else {
            // After Play anyway a conflict can still come: its answer goes with `anyway`.
            relay.pull_with(&dir, *resolve, *anyway)
        };
        *resolve = None;
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
                    self.log("save sync is set up");
                    return true;
                }
                SignInAnswer::Submit {
                    server,
                    user,
                    password,
                } => match self.setup(&server, &user, &password) {
                    Ok(()) => {
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
                    self.log("signed in again");
                    return;
                }
                SignInAnswer::Submit {
                    server,
                    user,
                    password,
                } => match self.setup(&server, &user, &password) {
                    Ok(()) => {
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
        let path = self.roots.push_waiting();
        if waiting {
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

// ---- the Android app's parts that need no JNI (relay_android.rs uses them) -------------------

/// save-relay's `CredentialStore` as one JSON file (`{account: secret}`) in the Android app's
/// private files folder: Android's per-app sandbox keeps other apps out, and the file is
/// `0600` besides. It is the only place the Dropbox refresh token is written. Every change is a
/// whole new file renamed over the old, so a crash leaves the old one or the new one. A file
/// that does not read as that JSON is an error, never quietly emptied. `Debug` shows the path
/// only.
pub struct FileStore {
    path: PathBuf,
}

/// One change at a time within the process (the sign-in on the home screen, a push on the
/// game's background worker).
static FILE_STORE_LOCK: Mutex<()> = Mutex::new(());

impl std::fmt::Debug for FileStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStore")
            .field("path", &self.path)
            .finish()
    }
}

fn store_error(what: &str) -> save_relay::Error {
    save_relay::Error::Credentials(format!("the app's credential file {what}"))
}

impl FileStore {
    pub fn new(path: impl Into<PathBuf>) -> FileStore {
        FileStore { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Removes the whole file (every secret in it): "Turn off save sync" when the file cannot be
    /// read, so a damaged file does not keep save sync on. A missing file is fine.
    pub fn remove(&self) -> save_relay::Result<()> {
        let _one = FILE_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(store_error("cannot be removed")),
        }
    }

    fn load(&self) -> save_relay::Result<BTreeMap<String, String>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| store_error("does not read")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(_) => Err(store_error("cannot be read")),
        }
    }

    fn save(&self, all: &BTreeMap<String, String>) -> save_relay::Result<()> {
        use std::io::Write;
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(|_| store_error("folder cannot be made"))?;
        let partial = self.path.with_extension("json.partial");
        let _ = std::fs::remove_file(&partial);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let written = (|| {
            let mut file = options.open(&partial)?;
            let text = serde_json::to_vec(all).map_err(std::io::Error::other)?;
            file.write_all(&text)?;
            file.sync_all()?;
            std::fs::rename(&partial, &self.path)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&partial);
            return Err(store_error("cannot be written"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

impl CredentialStore for FileStore {
    fn get(&self, account: &str) -> save_relay::Result<Option<Secret>> {
        let _one = FILE_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        Ok(self.load()?.remove(account).map(Secret::new))
    }

    fn set(&self, account: &str, secret: &Secret) -> save_relay::Result<()> {
        let _one = FILE_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.load()?;
        all.insert(account.to_string(), secret.expose().to_string());
        self.save(&all)
    }

    fn delete(&self, account: &str) -> save_relay::Result<()> {
        let _one = FILE_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.load()?;
        if all.remove(account).is_none() {
            return Ok(());
        }
        self.save(&all)
    }

    fn kind(&self) -> &'static str {
        "android-app-file"
    }
}

/// Where the Android app keeps the Dropbox refresh token for the game download alone, once save
/// sync is turned off (the same `FileStore`): one sign-in serves both, and turning sync off
/// stops the syncing only.
pub const DOWNLOAD_ACCOUNT: &str = "download:dropbox";

/// The Android app's "Turn off save sync" (the home screen's button and the blocked screens'):
/// save sync stops on this device, the Dropbox sign-in stays for the game download. The token
/// moves from save-relay's account to `DOWNLOAD_ACCOUNT` first (when it can be read), then
/// `acnh_core::relay::forget` runs, best effort. When a secret could not be deleted (the credential
/// file does not read), the whole credential file goes: it holds save-relay's and the download's
/// secrets only, and a file that does not read serves neither. An error comes back only when
/// something is still left behind.
pub fn forget_android(dirs: &AppDirs, creds: &FileStore) -> Result<()> {
    keep_for_download(dirs, creds);
    forget_android_all(dirs, creds)
}

/// "Sign out of Dropbox": save sync and the game download both end on this device (the
/// download's token goes too).
pub fn sign_out_android(dirs: &AppDirs, creds: &FileStore) -> Result<()> {
    let forgot = forget_android_all(dirs, creds);
    let download = creds
        .delete(DOWNLOAD_ACCOUNT)
        .or_else(|_| creds.remove())
        .map_err(|e| Error::Saves(e.to_string()));
    forgot.and(download)
}

/// save-relay's Dropbox refresh token, kept for the download under `DOWNLOAD_ACCOUNT`.
fn keep_for_download(dirs: &AppDirs, creds: &FileStore) {
    let Ok(Some(config)) = LocalStore::new(dirs.clone()).load_config() else {
        return;
    };
    let Ok(service) = save_relay::service::Service::from_server(&config.server) else {
        return;
    };
    if service != save_relay::service::Service::Dropbox {
        return;
    }
    let Ok(account) = save_relay::service::token_account(service, &config.user) else {
        return;
    };
    if let Ok(Some(token)) = creds.get(&account) {
        let _ = creds.set(DOWNLOAD_ACCOUNT, &token);
    }
}

fn forget_android_all(dirs: &AppDirs, creds: &FileStore) -> Result<()> {
    let Err(error) = acnh_core::relay::forget(dirs, creds) else {
        return Ok(());
    };
    creds
        .remove()
        .map_err(|e| Error::Saves(format!("{error}; {e}")))?;
    match std::fs::metadata(dirs.config.join("config.json")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(error),
    }
}

/// The home screen's side of `BridgePrompt`: a request's JSON in, the answer's JSON out.
type Ask<'a> = dyn Fn(&str) -> Option<String> + std::marker::Sync + 'a;

/// The prompts as one JSON request each, answered by the app's own screens (Android: dialogs on
/// the home screen, `Relay.ask` in android/java, reached through JNI). `ask` blocks until the
/// person answers and gives the answer's JSON, or `None` when the screens cannot be reached;
/// nothing is then decided for the person (the request is asked again a moment later, or the
/// harmless answer is taken: Cancel, Check again, Retry). Requests never hold a secret, the
/// account's user name or a path.
///
/// - `{"kind": "sign_in", "again": bool, "allow_not_now": bool, "message": text or null}`:
///   `signed_in`, `not_now`, `cancel`
/// - `{"kind": "conflict", "server": text, "local": text}` (`describe_card`): `server`, `local`
/// - `{"kind": "held", "device": text, "since": text}`: `play_anyway`, `check_again`
/// - `{"kind": "blocked", "why": "offline" or "error" or "sign_in_cancelled", "message": text
///   or null, "can_turn_off": bool}`: `retry`, `sign_in_again` (after a cancelled sign-in),
///   `turn_off` (where `can_turn_off`)
///
/// The reply is `{"answer": "<one of those>"}`.
pub struct BridgePrompt<'a> {
    ask: Box<Ask<'a>>,
    /// How long to wait before asking again when the screens could not be reached.
    pub pause: std::time::Duration,
}

impl<'a> BridgePrompt<'a> {
    pub fn new(ask: impl Fn(&str) -> Option<String> + std::marker::Sync + 'a) -> BridgePrompt<'a> {
        BridgePrompt {
            ask: Box::new(ask),
            pause: std::time::Duration::from_secs(1),
        }
    }

    /// The `answer` field of the reply to `request`, or `None`.
    fn answer(&self, request: &serde_json::Value) -> Option<String> {
        let reply = (self.ask)(&request.to_string())?;
        let reply: serde_json::Value = serde_json::from_str(&reply).ok()?;
        reply
            .get("answer")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    }
}

impl Prompt for BridgePrompt<'_> {
    fn sign_in(&self, form: &SignInForm) -> SignInAnswer {
        let request = serde_json::json!({
            "kind": "sign_in",
            "again": !form.allow_not_now,
            "allow_not_now": form.allow_not_now,
            "message": form.message,
        });
        match self.answer(&request).as_deref() {
            Some("signed_in") => SignInAnswer::SignedIn,
            Some("not_now") if form.allow_not_now => SignInAnswer::NotNow,
            _ => SignInAnswer::Cancel,
        }
    }

    fn conflict(&self, server: &SaveCard, local: &SaveCard) -> Keep {
        let request = serde_json::json!({
            "kind": "conflict",
            "server": describe_card(server),
            "local": describe_card(local),
        });
        // Which island to keep is the person's choice alone: ask until one is picked.
        loop {
            match self.answer(&request).as_deref() {
                Some("server") => return Keep::Server,
                Some("local") => return Keep::Local,
                _ => std::thread::sleep(self.pause),
            }
        }
    }

    fn held(&self, device: &str, since: &str) -> HeldAnswer {
        let request = serde_json::json!({"kind": "held", "device": device, "since": since});
        match self.answer(&request).as_deref() {
            Some("play_anyway") => HeldAnswer::PlayAnyway,
            Some(_) => HeldAnswer::CheckAgain,
            None => {
                std::thread::sleep(self.pause);
                HeldAnswer::CheckAgain
            }
        }
    }

    fn blocked(&self, why: &Blocked) -> BlockedAnswer {
        let (kind, message) = match why {
            Blocked::Offline => ("offline", None),
            Blocked::Error(message) => ("error", Some(message.as_str())),
            Blocked::SignInCancelled => ("sign_in_cancelled", None),
        };
        let can_turn_off = *why != Blocked::Offline;
        let request = serde_json::json!({"kind": "blocked", "why": kind, "message": message,
                                         "can_turn_off": can_turn_off});
        match self.answer(&request).as_deref() {
            Some("sign_in_again") if *why == Blocked::SignInCancelled => BlockedAnswer::SignInAgain,
            Some("turn_off") if *why != Blocked::Offline => BlockedAnswer::TurnOff,
            Some(_) => BlockedAnswer::Retry,
            None => {
                std::thread::sleep(self.pause);
                BlockedAnswer::Retry
            }
        }
    }
}

/// A request's headers as save-relay's C interface hands them to the app's `http` callback:
/// `Name: value` lines, each ended by `\n`.
pub fn header_lines(request: &save_relay::http::Request) -> String {
    let mut lines = String::new();
    for (name, value) in &request.headers {
        lines.push_str(name);
        lines.push_str(": ");
        lines.push_str(value);
        lines.push('\n');
    }
    lines
}

/// The app's answer to one request in save-relay's Android wire format (`SaveRelayResponse.toWire`
/// in its bindings): the status and the headers' length as big-endian `u32`s, the `Name: value`
/// header lines, then the body. Anything shorter or malformed is `Unreachable`, as a network
/// failure is.
pub fn parse_wire(wire: &[u8]) -> save_relay::Result<save_relay::http::Response> {
    let bad = || save_relay::Error::Unreachable("the app's network answer is malformed".into());
    if wire.len() < 8 {
        return Err(bad());
    }
    let status = u32::from_be_bytes([wire[0], wire[1], wire[2], wire[3]]);
    let header_len = u32::from_be_bytes([wire[4], wire[5], wire[6], wire[7]]) as usize;
    let status = u16::try_from(status).map_err(|_| bad())?;
    let rest = &wire[8..];
    if rest.len() < header_len {
        return Err(bad());
    }
    let headers = std::str::from_utf8(&rest[..header_len]).map_err(|_| bad())?;
    let mut response = save_relay::http::Response::new(status, rest[header_len..].to_vec());
    for line in headers.lines() {
        if let Some((name, value)) = line.split_once(':') {
            response = response.header(name.trim(), value.trim());
        }
    }
    Ok(response)
}

/// A request as Android's `HttpURLConnection` must send it. A POST without a `Content-Type` (save-
/// relay's Dropbox download and list calls: an empty body, the arguments in a header) would go out
/// with `application/x-www-form-urlencoded`, which Dropbox refuses with 400 ("Bad HTTP
/// Content-Type header"): it gets `application/octet-stream`. Every request asks for
/// `Accept-Encoding: identity`, so the body comes back as sent and its length is the real one.
pub fn for_android(request: &save_relay::http::Request) -> save_relay::http::Request {
    let mut request = request.clone();
    if request.method.eq_ignore_ascii_case("POST") && request.header_value("Content-Type").is_none()
    {
        request = request.header("Content-Type", "application/octet-stream");
    }
    if request.header_value("Accept-Encoding").is_none() {
        request = request.header("Accept-Encoding", "identity");
    }
    request
}

/// save-relay's `Transport` over the app's own network call (Android: the Java host's `http`,
/// the same one save-relay's C interface uses for the sign-in), given the request and its
/// `header_lines`, answering in `parse_wire`'s format, or `None` for a connection failure.
pub struct CallbackTransport<F>(pub F);

impl<F> save_relay::http::Transport for CallbackTransport<F>
where
    F: Fn(&save_relay::http::Request, &str) -> Option<Vec<u8>> + Send + std::marker::Sync,
{
    fn send(
        &self,
        request: &save_relay::http::Request,
    ) -> save_relay::Result<save_relay::http::Response> {
        let request = &for_android(request);
        let wire = (self.0)(request, &header_lines(request)).ok_or_else(|| {
            save_relay::Error::Unreachable("the app's network call failed".into())
        })?;
        let response = parse_wire(&wire)?;
        match request.max_response_bytes {
            Some(max) if response.body.len() > max => Err(save_relay::Error::Unreachable(
                "the answer is larger than allowed".into(),
            )),
            _ => Ok(response),
        }
    }
}
