//! save-relay inside the iPad's player (issue #172, ADR 0006): the app asks save-relay for the
//! island before the game boots and pushes it when the game quits or the app goes to the
//! background. The adapter is `acnh_core::relay` (save-relay's Rust API), as on the Mac and PC.
//!
//! This file holds everything that does not touch UIKit or the Keychain, so the tests run it on
//! every runner (`tests/apple_relay.rs` includes it with `#[path]`): the pure decision
//! (`decide`: a pull's answer in, an action out), the prompt and connector traits, and the
//! loop (`Player::before_game`, `Player::after_game`). The UIKit alerts, the Keychain store and
//! the C exports `acnh_relay_before_game` / `acnh_relay_after_game` are in `apple_ios.rs`, iOS
//! only. Nothing here may use `crate::`: the tests compile this file as one of their modules.
//!
//! The rules (the owner's answers of 2026-11-18, "Refuse until synced" and "Show sign-in
//! again"): the game boots only after a pull has succeeded, with save sync not set up, or after
//! Play anyway on a held island. The app password is only ever in the credential store; no
//! file, log line or message holds it.

use acnh_core::config::Config;
use acnh_core::launch::{Keep, Relay, Sync};
use acnh_core::relay::SaveRelay;
use acnh_core::savecard::SaveCard;
use acnh_core::{saves, Error, Result};
use save_relay::creds::CredentialStore;
use save_relay::local::LocalStore;
use save_relay::platform::AppDirs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(target_os = "ios")]
#[path = "apple_ios.rs"]
pub mod ios;

/// The name this iPad goes by in save-relay's history ("ipad"; never the owner's device name).
pub const DEVICE: &str = "iPad";

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

/// What the person did with the sign-in form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInAnswer {
    Submit {
        server: String,
        user: String,
        password: String,
    },
    /// First start only: leave save sync off, and do not ask again (`relay.ask = 0`).
    NotNow,
    /// Close the form (only the sign-in-again form has no Not now).
    Cancel,
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
}

/// What the person chose on a held island.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldAnswer {
    PlayAnyway,
    CheckAgain,
}

/// The screens. UIKit alerts on the iPad (`apple_ios.rs`); a fake in the tests. Every method
/// blocks until it is answered, on the game's thread.
pub trait Prompt: std::marker::Sync {
    fn sign_in(&self, form: &SignInForm) -> SignInAnswer;
    /// Both islands side by side; `Keep::Local` is this iPad's.
    fn conflict(&self, server: &SaveCard, local: &SaveCard) -> Keep;
    fn held(&self, device: &str, since: &str) -> HeldAnswer;
    /// The game does not boot; offer Retry (and Sign in again when `SignInCancelled`).
    fn blocked(&self, why: &Blocked) -> BlockedAnswer;
}

/// How the app reaches save-relay: `Live` over ureq and rustls; the tests bring an in-memory
/// remote.
pub trait Connector: std::marker::Sync {
    /// `acnh_core::relay::open`: nothing set up is `Ok(None)`.
    fn open(&self, dirs: &AppDirs, creds: &dyn CredentialStore) -> Result<Option<SaveRelay>>;
    /// `acnh_core::relay::setup_nextcloud`: the password goes to `creds` only.
    fn setup(
        &self,
        server: &str,
        user: &str,
        password: &str,
        dirs: &AppDirs,
        creds: &dyn CredentialStore,
    ) -> Result<()>;
}

/// The real network: ureq with rustls through save-relay's `http` feature.
pub struct Live;

impl Connector for Live {
    fn open(&self, dirs: &AppDirs, creds: &dyn CredentialStore) -> Result<Option<SaveRelay>> {
        acnh_core::relay::open(
            dirs,
            creds,
            Arc::new(save_relay::http::UreqTransport::new()),
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
            Arc::new(save_relay::http::UreqTransport::new()),
        )
    }
}

/// Where the player keeps things. `data_root` is the app's Documents folder (saves, settings,
/// logs: what Files shows). save-relay's own config and state go under `library`, never into
/// Documents.
#[derive(Debug, Clone)]
pub struct Roots {
    pub data_root: PathBuf,
    pub library: PathBuf,
}

impl Roots {
    /// The iPad's: `<container>/Documents` and `<container>/Library`.
    pub fn for_data_root(data_root: &Path) -> Roots {
        let library = data_root
            .parent()
            .map_or_else(|| data_root.join("Library"), |p| p.join("Library"));
        Roots {
            data_root: data_root.to_path_buf(),
            library,
        }
    }

    /// save-relay's folders, both under `Library/save-relay`.
    pub fn dirs(&self) -> AppDirs {
        let base = self.library.join("save-relay");
        AppDirs {
            config: base.join("config"),
            data: base.join("data"),
        }
    }

    fn config_ini(&self) -> PathBuf {
        self.data_root.join("config.ini")
    }

    /// A push that did not go (offline, a conflict, an error) leaves this file; the next start
    /// sends it before it pulls.
    fn push_waiting(&self) -> PathBuf {
        self.dirs().data.join("push-waiting")
    }
}

/// Everything the loop needs, borrowed.
pub struct Player<'a> {
    pub roots: Roots,
    pub creds: &'a (dyn CredentialStore + std::marker::Sync),
    pub connector: &'a dyn Connector,
    pub prompt: &'a dyn Prompt,
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
        let mut resolve = None;
        let mut anyway = false;
        loop {
            match self.opened() {
                Err(error) => {
                    let message = self.say(&error, None);
                    self.log(&format!("save sync could not be opened: {message}"));
                    self.prompt.blocked(&Blocked::Error(message));
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

    /// Push the island: when the game quits, and when the app goes to the background. A push
    /// that does not go (offline, a conflict, any error) is left waiting for the next start.
    pub fn after_game(&self) {
        let _one_at_a_time = PUSH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let relay = match self.opened() {
            Ok(Opened::Sync(relay)) => relay,
            Ok(_) => return,
            Err(error) => {
                let message = self.say(&error, None);
                self.log(&format!(
                    "push skipped, save sync could not be opened: {message}"
                ));
                return;
            }
        };
        let dir = saves::save_dir(&self.roots.data_root);
        let result = relay.push(&dir, None);
        match decide(&result) {
            Action::Play | Action::PlaySyncOff => {
                self.set_waiting(false);
                self.log("pushed the island");
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
        } else if *anyway {
            relay.pull_anyway(&dir)
        } else {
            relay.pull(&dir, *resolve)
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
                self.prompt.blocked(&Blocked::Error(
                    "save-relay answered that another device has the island open.".into(),
                ));
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
                self.prompt.blocked(&Blocked::Error(message));
                Attempt::Again
            }
        }
    }

    /// The first start's form (with Not now). True once set up, so the loop opens again.
    fn first_start(&self) -> bool {
        if self.ini().get("relay.ask") == Some("0") {
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
                    self.remember_not_now();
                    return false;
                }
                // Closed without an answer: sync stays off for this start, and it asks again.
                SignInAnswer::Cancel => return false,
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
            message: Some(
                "The server refused the sign-in. Enter a new app password to play.".into(),
            ),
        };
        loop {
            match self.prompt.sign_in(&form) {
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

    fn ini(&self) -> Config {
        std::fs::read_to_string(self.roots.config_ini())
            .ok()
            .and_then(|text| Config::parse(&text).ok())
            .unwrap_or_default()
    }

    /// `relay.ask = 0` in config.ini: Not now is remembered.
    fn remember_not_now(&self) {
        let mut ini = self.ini();
        ini.set("relay.ask", "0");
        let path = self.roots.config_ini();
        let partial = path.with_extension("ini.partial");
        if std::fs::create_dir_all(&self.roots.data_root).is_ok()
            && std::fs::write(&partial, ini.to_text()).is_ok()
            && std::fs::rename(&partial, &path).is_err()
        {
            let _ = std::fs::remove_file(&partial);
        }
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
