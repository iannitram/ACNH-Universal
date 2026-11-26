//! The iPad's save-relay flow (issue #172) with fakes, on every runner: save-relay's in-memory
//! remote and credential store, a temporary data root, and a prompt that blocks until the test
//! answers it, so "the game has not booted" is checked as "`before_game` has not returned".
//! Only the Keychain, UIKit and the C exports are iOS-only (`relay_ios.rs`).
#![cfg(not(any(target_os = "ios", target_os = "android")))]

#[allow(dead_code)] // `Live` is the iPad's connector; the tests bring a fake
#[path = "../src/relay.rs"]
mod relay;

use acnh_core::launch::{Keep, Relay, Sync as RelaySync};
use acnh_core::relay::SaveRelay;
use acnh_core::savecard::SaveCard;
use acnh_core::{saves, Error};
use relay::{
    decide, describe_card, Action, Blocked, BlockedAnswer, Bounded, Connector, FileSettings,
    FinalPush, HeldAnswer, Player, Prompt, Roots, SignInAnswer, SignInForm, SyncNow, SyncReady,
};
use save_relay::creds::{self, CredentialStore, MemoryStore, Secret};
use save_relay::local::{Config, LocalStore};
use save_relay::platform::AppDirs;
use save_relay::remote::{MemoryRemote, Precondition, Remote, RemoteEntry};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SERVER: &str = "https://cloud.test";
const USER: &str = "alex";
const PASSWORD: &str = "first-app-password-4711";
const SECOND: &str = "second-app-password-8142";
const THIRD: &str = "third-app-password-5593";
const DROPBOX_URL: &str = "https://dropbox.test/oauth2/authorize?pending=1";
const DROPBOX_CODE: &str = "dropbox-code-7731";
const DROPBOX_TOKEN: &str = "dropbox-refresh-token-9042";

static NEXT: AtomicUsize = AtomicUsize::new(0);

// ---- the fake server -------------------------------------------------------------------------

/// The in-memory remote behind a password check and a "broken" switch: a wrong or revoked app
/// password answers `AuthFailed` (a 401 on the wire), a broken server answers `Server`.
struct Gate {
    inner: Arc<MemoryRemote>,
    password_ok: bool,
    broken: Arc<AtomicBool>,
    /// No network: every call answers `Unreachable`.
    offline: Arc<AtomicBool>,
    /// The next puts of `playing.json` fail as a dropped connection.
    fail_mark_puts: Arc<AtomicUsize>,
    /// Every put waits while this is set (a slow server, a big island).
    stall: Arc<AtomicBool>,
}

impl Gate {
    fn check(&self) -> save_relay::Result<()> {
        if self.offline.load(Ordering::SeqCst) {
            return Err(save_relay::Error::Unreachable("no network".into()));
        }
        if !self.password_ok {
            return Err(save_relay::Error::AuthFailed);
        }
        if self.broken.load(Ordering::SeqCst) {
            return Err(save_relay::Error::Server("the server fell over".into()));
        }
        Ok(())
    }
}

impl Remote for Gate {
    fn list(&self, dir: &str) -> save_relay::Result<Vec<RemoteEntry>> {
        self.check()?;
        self.inner.list(dir)
    }
    fn get(&self, path: &str) -> save_relay::Result<(Vec<u8>, String)> {
        self.check()?;
        self.inner.get(path)
    }
    fn put(&self, path: &str, bytes: &[u8], pre: &Precondition) -> save_relay::Result<String> {
        while self.stall.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let failing = self.fail_mark_puts.load(Ordering::SeqCst);
        if path.ends_with("playing.json")
            && failing > 0
            && self
                .fail_mark_puts
                .compare_exchange(failing, failing - 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            return Err(save_relay::Error::Unreachable(
                "the connection dropped".into(),
            ));
        }
        self.check()?;
        self.inner.put(path, bytes, pre)
    }
    fn mkcol(&self, dir: &str) -> save_relay::Result<()> {
        self.check()?;
        self.inner.mkcol(dir)
    }
    fn copy(&self, from: &str, to: &str) -> save_relay::Result<()> {
        self.check()?;
        self.inner.copy(from, to)
    }
    fn move_to(&self, from: &str, to: &str) -> save_relay::Result<()> {
        self.check()?;
        self.inner.move_to(from, to)
    }
    fn delete(&self, path: &str) -> save_relay::Result<()> {
        self.check()?;
        self.inner.delete(path)
    }
}

/// The credential store, which can be made unreadable (a locked Keychain).
struct Creds {
    inner: MemoryStore,
    unreadable: AtomicBool,
}

impl CredentialStore for Creds {
    fn get(&self, account: &str) -> save_relay::Result<Option<Secret>> {
        if self.unreadable.load(Ordering::SeqCst) {
            return Err(save_relay::Error::Credentials(
                "the Keychain is locked".into(),
            ));
        }
        self.inner.get(account)
    }
    fn set(&self, account: &str, secret: &Secret) -> save_relay::Result<()> {
        self.inner.set(account, secret)
    }
    fn delete(&self, account: &str) -> save_relay::Result<()> {
        self.inner.delete(account)
    }
    fn kind(&self) -> &'static str {
        "test"
    }
}

struct FakeConnector {
    /// The next `open` panics (a bug in the adapter).
    panic_once: AtomicBool,
    remote: Arc<MemoryRemote>,
    accepted: Mutex<String>,
    broken: Arc<AtomicBool>,
    offline: Arc<AtomicBool>,
    fail_mark_puts: Arc<AtomicUsize>,
    stall: Arc<AtomicBool>,
    /// A Dropbox sign-in was started and is waiting for its code.
    dropbox_started: AtomicBool,
}

impl Connector for FakeConnector {
    fn open(
        &self,
        dirs: &AppDirs,
        creds: &dyn CredentialStore,
    ) -> acnh_core::Result<Option<SaveRelay>> {
        if self.panic_once.swap(false, Ordering::SeqCst) {
            panic!("a bug in the adapter");
        }
        let store = LocalStore::new(dirs.clone());
        let Some(config) = store
            .load_config()
            .map_err(|e| Error::Saves(e.to_string()))?
        else {
            return Ok(None);
        };
        let dropbox = config.server == "dropbox";
        let account = if dropbox {
            save_relay::service::token_account(save_relay::service::Service::Dropbox, &config.user)
                .unwrap()
        } else {
            creds::account(&config.server, &config.user).unwrap()
        };
        let Some(secret) = creds
            .get(&account)
            .map_err(|e| Error::Saves(e.to_string()))?
        else {
            return Ok(None);
        };
        let gate = Gate {
            inner: self.remote.clone(),
            password_ok: dropbox || secret.expose() == *self.accepted.lock().unwrap(),
            broken: self.broken.clone(),
            offline: self.offline.clone(),
            fail_mark_puts: self.fail_mark_puts.clone(),
            stall: self.stall.clone(),
        };
        Ok(Some(
            SaveRelay::new(Arc::new(gate), store, config.device).with_root(config.root),
        ))
    }

    fn dropbox_start(
        &self,
        _dirs: &AppDirs,
        _creds: &dyn CredentialStore,
    ) -> acnh_core::Result<String> {
        if self.offline.load(Ordering::SeqCst) {
            return Err(Error::Saves("Dropbox could not be reached".into()));
        }
        self.dropbox_started.store(true, Ordering::SeqCst);
        Ok(DROPBOX_URL.into())
    }

    fn dropbox_finish(
        &self,
        code: &str,
        dirs: &AppDirs,
        creds: &dyn CredentialStore,
    ) -> acnh_core::Result<()> {
        if !self.dropbox_started.swap(false, Ordering::SeqCst) || code != DROPBOX_CODE {
            return Err(Error::Saves("Dropbox refused the code".into()));
        }
        // What `acnh_core::relay::finish_sign_in` leaves: the token in the credential store
        // under the service's account, the config naming the service.
        let account =
            save_relay::service::token_account(save_relay::service::Service::Dropbox, "me")
                .map_err(|e| Error::Saves(e.to_string()))?;
        creds
            .set(&account, &Secret::new(DROPBOX_TOKEN))
            .map_err(|e| Error::Saves(e.to_string()))?;
        LocalStore::new(dirs.clone())
            .save_config(&Config {
                format: save_relay::local::FORMAT,
                server: "dropbox".into(),
                user: "me".into(),
                root: save_relay::cli::DEFAULT_ROOT.to_string(),
                device: save_relay::layout::device_slug(relay::DEVICE),
            })
            .map_err(|e| Error::Saves(e.to_string()))
    }

    fn setup(
        &self,
        server: &str,
        user: &str,
        password: &str,
        dirs: &AppDirs,
        creds: &dyn CredentialStore,
    ) -> acnh_core::Result<()> {
        if password != *self.accepted.lock().unwrap() {
            return Err(Error::RelaySignIn);
        }
        let account = creds::account(server, user).map_err(|e| Error::Saves(e.to_string()))?;
        creds
            .set(&account, &Secret::new(password))
            .map_err(|e| Error::Saves(e.to_string()))?;
        LocalStore::new(dirs.clone())
            .save_config(&Config {
                format: save_relay::local::FORMAT,
                server: server.to_string(),
                user: user.to_string(),
                root: save_relay::cli::DEFAULT_ROOT.to_string(),
                device: save_relay::layout::device_slug(relay::DEVICE),
            })
            .map_err(|e| Error::Saves(e.to_string()))
    }
}

// ---- the fake prompt -------------------------------------------------------------------------

#[derive(Debug)]
enum Shown {
    SignIn(SignInForm),
    Conflict(SaveCard, SaveCard),
    Held(String, String),
    Blocked(Blocked),
    DropboxCode(String),
}

#[derive(Debug)]
enum Reply {
    Code(Option<String>),
    SignIn(SignInAnswer),
    Keep(Keep),
    Held(HeldAnswer),
    Blocked(BlockedAnswer),
}

/// What the fake prompts got wrong (a wrong reply, an unanswered screen, a screen that should not
/// have been shown). The fakes never panic: a panic inside `before_game` is caught by its
/// recovery, which would hide it or abort the binary. They record, answer harmlessly, and
/// `Run::finish`, `run` and `Fx`'s drop assert that nothing failed.
type Failures = Arc<Mutex<Vec<String>>>;

fn fail(failures: &Failures, what: String) {
    let mut all = failures.lock().unwrap();
    all.push(what);
    // A harmless answer can loop (Retry, Cancel): stop the binary, loudly, rather than hang.
    if all.len() > 40 {
        eprintln!("too many prompt failures:\n{}", all.join("\n"));
        std::process::exit(101);
    }
}

fn no_failures(failures: &Failures) {
    let all = failures.lock().unwrap();
    assert!(all.is_empty(), "the fake prompts failed: {all:?}");
}

/// Shows each screen to the test and blocks until the test answers it.
struct ChannelPrompt {
    shown: Mutex<Sender<Shown>>,
    replies: Mutex<Receiver<Reply>>,
    failures: Failures,
}

impl ChannelPrompt {
    /// The test's reply, or None (recorded) when it gave none.
    fn ask(&self, shown: Shown) -> Option<Reply> {
        let _ = self.shown.lock().unwrap().send(shown);
        match self
            .replies
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(20))
        {
            Ok(reply) => Some(reply),
            Err(_) => {
                fail(&self.failures, "the test did not answer the prompt".into());
                None
            }
        }
    }

    fn wrong(&self, got: Reply) {
        fail(&self.failures, format!("wrong reply {got:?}"));
    }
}

impl Prompt for ChannelPrompt {
    fn sign_in(&self, form: &SignInForm) -> SignInAnswer {
        match self.ask(Shown::SignIn(form.clone())) {
            Some(Reply::SignIn(answer)) => answer,
            Some(other) => {
                self.wrong(other);
                SignInAnswer::Cancel
            }
            None => SignInAnswer::Cancel,
        }
    }
    fn dropbox_code(&self, url: &str) -> Option<String> {
        match self.ask(Shown::DropboxCode(url.into())) {
            Some(Reply::Code(code)) => code,
            Some(other) => {
                self.wrong(other);
                None
            }
            None => None,
        }
    }
    fn conflict(&self, server: &SaveCard, local: &SaveCard) -> Keep {
        match self.ask(Shown::Conflict(server.clone(), local.clone())) {
            Some(Reply::Keep(keep)) => keep,
            Some(other) => {
                self.wrong(other);
                Keep::Local
            }
            None => Keep::Local,
        }
    }
    fn held(&self, device: &str, since: &str) -> HeldAnswer {
        match self.ask(Shown::Held(device.into(), since.into())) {
            Some(Reply::Held(answer)) => answer,
            Some(other) => {
                self.wrong(other);
                HeldAnswer::PlayAnyway
            }
            None => HeldAnswer::PlayAnyway,
        }
    }
    fn blocked(&self, why: &Blocked) -> BlockedAnswer {
        match self.ask(Shown::Blocked(why.clone())) {
            Some(Reply::Blocked(answer)) => answer,
            Some(other) => {
                self.wrong(other);
                BlockedAnswer::Retry
            }
            None => BlockedAnswer::Retry,
        }
    }
}

// ---- the fixture -----------------------------------------------------------------------------

struct Fx {
    root: PathBuf,
    roots: Roots,
    remote: Arc<MemoryRemote>,
    creds: Creds,
    booted: AtomicBool,
    pulled: AtomicBool,
    synced: AtomicU64,
    settings: FileSettings,
    failures: Failures,
    connector: FakeConnector,
    log: Mutex<Vec<String>>,
}

impl Fx {
    fn new(name: &str) -> Fx {
        let root = std::env::temp_dir().join(format!(
            "acnh-apple-relay-{}-{name}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        // The app's container: Documents (the data root) and Library beside it.
        let data_root = root.join("Documents");
        fs::create_dir_all(&data_root).unwrap();
        let remote = Arc::new(MemoryRemote::new());
        Fx {
            roots: Roots::for_data_root(&data_root),
            connector: FakeConnector {
                panic_once: AtomicBool::new(false),
                remote: remote.clone(),
                accepted: Mutex::new(PASSWORD.into()),
                broken: Arc::new(AtomicBool::new(false)),
                offline: Arc::new(AtomicBool::new(false)),
                fail_mark_puts: Arc::new(AtomicUsize::new(0)),
                stall: Arc::new(AtomicBool::new(false)),
                dropbox_started: AtomicBool::new(false),
            },
            remote,
            creds: Creds {
                inner: MemoryStore::new(),
                unreadable: AtomicBool::new(false),
            },
            booted: AtomicBool::new(false),
            pulled: AtomicBool::new(false),
            synced: AtomicU64::new(0),
            settings: FileSettings(data_root.join("config.ini")),
            failures: Failures::default(),
            log: Mutex::new(Vec::new()),
            root,
        }
    }

    fn saves(&self) -> PathBuf {
        saves::save_dir(&self.roots.data_root)
    }

    fn set_up(&self) {
        self.connector
            .setup(SERVER, USER, PASSWORD, &self.roots.dirs(), &self.creds)
            .unwrap();
    }

    fn player<'a>(&'a self, prompt: &'a dyn Prompt) -> Player<'a> {
        Player {
            roots: self.roots.clone(),
            creds: &self.creds,
            connector: &self.connector,
            prompt,
            booted: &self.booted,
            pulled: &self.pulled,
            settings: &self.settings,
            synced: &self.synced,
            log: Box::new(|line| self.log.lock().unwrap().push(line.to_string())),
        }
    }

    /// A second device on the same server.
    fn other(&self, name: &str) -> (SaveRelay, PathBuf) {
        let dir = self.root.join(name);
        let store = LocalStore::new(AppDirs {
            config: dir.join("config"),
            data: dir.join("data"),
        });
        (
            SaveRelay::new(self.remote.clone(), store, name),
            dir.join("saves"),
        )
    }

    /// Everything in the temporary folders, as text.
    fn all_text(&self) -> String {
        text_of(&self.root)
    }

    /// save-relay's history for the island: (device, current).
    fn history(&self) -> Vec<(String, bool)> {
        let manifest = save_relay::manifest::Manifest::from_json(&saves::relay_manifest()).unwrap();
        let session = save_relay::sync::Session {
            remote: &*self.remote,
            layout: save_relay::layout::Layout::new(save_relay::cli::DEFAULT_ROOT).unwrap(),
            device: "ipad".into(),
            now: 0,
        };
        save_relay::sync::history(&session, &manifest.id)
            .unwrap()
            .entries
            .iter()
            .map(|e| (e.device.clone(), e.current))
            .collect()
    }

    /// `after_game` shows no screen: a screen it shows is recorded as a failure.
    fn no_prompt(&self) -> NoPrompt {
        NoPrompt(self.failures.clone())
    }

    fn log_text(&self) -> String {
        self.log.lock().unwrap().join("\n")
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            no_failures(&self.failures);
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn text_of(dir: &Path) -> String {
    let mut text = String::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in fs::read_dir(&next).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                text.push_str(&String::from_utf8_lossy(&fs::read(&path).unwrap()));
            }
        }
    }
    text
}

struct NoPrompt(Failures);

impl Prompt for NoPrompt {
    fn sign_in(&self, _: &SignInForm) -> SignInAnswer {
        fail(&self.0, "a sign-in screen where none belongs".into());
        SignInAnswer::Cancel
    }
    fn conflict(&self, _: &SaveCard, _: &SaveCard) -> Keep {
        fail(&self.0, "a conflict screen where none belongs".into());
        Keep::Local
    }
    fn held(&self, _: &str, _: &str) -> HeldAnswer {
        fail(&self.0, "a held screen where none belongs".into());
        HeldAnswer::PlayAnyway
    }
    fn blocked(&self, _: &Blocked) -> BlockedAnswer {
        fail(&self.0, "a blocked screen where none belongs".into());
        BlockedAnswer::Retry
    }
}

fn island(dir: &Path, marker: &str) {
    fs::create_dir_all(dir.join("Villager0")).unwrap();
    fs::write(dir.join("main.dat"), format!("main {marker}")).unwrap();
    fs::write(dir.join("mainHeader.dat"), format!("header {marker}")).unwrap();
    fs::write(
        dir.join("Villager0/personal.dat"),
        format!("villager {marker}"),
    )
    .unwrap();
}

fn read_main(dir: &Path) -> String {
    fs::read_to_string(dir.join("main.dat")).unwrap()
}

/// `before_game` on its own thread, with the test answering each screen.
struct Run<'s> {
    shown: Receiver<Shown>,
    replies: Sender<Reply>,
    handle: std::thread::ScopedJoinHandle<'s, ()>,
    failures: Failures,
}

impl Run<'_> {
    /// The next screen the game's thread shows.
    fn shown(&self) -> Shown {
        self.shown
            .recv_timeout(Duration::from_secs(20))
            .expect("no prompt was shown")
    }

    fn reply(&self, reply: Reply) {
        self.replies.send(reply).unwrap();
    }

    fn returned(&self) -> bool {
        self.handle.is_finished()
    }

    /// `before_game` returns (the game may boot), with no screen left unanswered.
    fn finish(&self) {
        let start = Instant::now();
        while !self.returned() {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "before_game did not return"
            );
            if let Ok(shown) = self.shown.try_recv() {
                panic!("an unexpected screen: {shown:?}");
            }
            no_failures(&self.failures);
            std::thread::sleep(Duration::from_millis(5));
        }
        no_failures(&self.failures);
        assert!(self.shown.try_recv().is_err(), "an unanswered screen");
    }
}

fn run<R>(fx: &Fx, test: impl FnOnce(&Run<'_>) -> R) -> R {
    let (shown_tx, shown) = channel();
    let (replies, replies_rx) = channel();
    let prompt = ChannelPrompt {
        shown: Mutex::new(shown_tx),
        replies: Mutex::new(replies_rx),
        failures: fx.failures.clone(),
    };
    let player = fx.player(&prompt);
    std::thread::scope(|scope| {
        let handle = scope.spawn(|| player.before_game());
        let harness = Run {
            shown,
            replies,
            handle,
            failures: fx.failures.clone(),
        };
        let result = test(&harness);
        no_failures(&fx.failures);
        result
    })
}

fn submit(password: &str) -> Reply {
    Reply::SignIn(SignInAnswer::Submit {
        server: SERVER.into(),
        user: USER.into(),
        password: password.into(),
    })
}

fn account() -> String {
    creds::account(SERVER, USER).unwrap()
}

// ---- the pure decision -----------------------------------------------------------------------

fn card(device: &str, saved: u64, play: u64) -> SaveCard {
    SaveCard {
        saved_unix: saved,
        device: device.into(),
        play_seconds: play,
        version: String::new(),
        picture: PathBuf::new(),
    }
}

#[test]
fn each_answer_gives_its_action() {
    for done in [
        RelaySync::Done("pulled".into()),
        RelaySync::Resolved {
            message: "kept".into(),
            backup: "b".into(),
        },
    ] {
        assert_eq!(decide(&Ok(done)), Action::Play);
    }
    assert_eq!(decide(&Ok(RelaySync::Off)), Action::PlaySyncOff);
    assert_eq!(decide(&Ok(RelaySync::Offline)), Action::RefuseOffline);
    let (server, local) = (card("desktop", 10, 60), card("iPad", 20, 120));
    assert_eq!(
        decide(&Ok(RelaySync::Conflict {
            server: server.clone(),
            local: local.clone(),
            backup: None
        })),
        Action::PickConflict { server, local }
    );
    assert_eq!(
        decide(&Ok(RelaySync::Held {
            device: "desktop".into(),
            since: "2026-11-18T07:15:00Z".into()
        })),
        Action::AskHeld {
            device: "desktop".into(),
            since: "2026-11-18T07:15:00Z".into()
        }
    );
    assert_eq!(decide(&Err(Error::RelaySignIn)), Action::SignInAgain);
    assert_eq!(
        decide(&Err(Error::Saves("the server fell over".into()))),
        Action::ShowError("Saves: the server fell over".into())
    );
}

#[test]
fn cards_are_described_with_device_time_and_play_time() {
    assert_eq!(
        describe_card(&card("desktop", 1_763_450_100, 3_900)),
        "desktop, saved 2025-11-18 07:15 UTC, played 1 h 5 min"
    );
    assert_eq!(
        describe_card(&card("", 0, 0)),
        "unknown device, saved 1970-01-01 00:00 UTC, play time unknown"
    );
}

// ---- the flows -------------------------------------------------------------------------------

#[test]
fn a_successful_pull_boots_without_a_screen() {
    let fx = Fx::new("pull-ok");
    fx.set_up();
    island(&fx.saves(), "v1");
    run(&fx, |game| game.finish());
    assert!(fx.log_text().contains("the game may boot"));
}

#[test]
fn not_set_up_asks_once_and_not_now_is_remembered_in_config_ini() {
    let fx = Fx::new("not-now");
    island(&fx.saves(), "v1");
    run(&fx, |game| {
        let Shown::SignIn(form) = game.shown() else {
            panic!("expected the sign-in form")
        };
        assert!(form.allow_not_now);
        assert!(form.message.is_none());
        assert!(!game.returned());
        game.reply(Reply::SignIn(SignInAnswer::NotNow));
        game.finish();
    });
    let ini = fs::read_to_string(fx.roots.data_root.join("config.ini")).unwrap();
    assert!(ini.contains("relay.ask = 0"), "{ini}");
    // The next start plays with sync off and asks nothing.
    run(&fx, |game| game.finish());
    assert!(!fx.creds.get(&account()).unwrap().is_some());
}

#[test]
fn a_setup_through_the_prompt_keeps_the_password_in_the_credential_store_only() {
    let fx = Fx::new("setup");
    island(&fx.saves(), "v1");
    run(&fx, |game| {
        let Shown::SignIn(_) = game.shown() else {
            panic!("expected the sign-in form")
        };
        // A wrong password: the form comes back, its message never holds the password.
        game.reply(submit(THIRD));
        let Shown::SignIn(form) = game.shown() else {
            panic!("expected the form again")
        };
        let message = form.message.expect("why");
        assert!(!message.contains(THIRD), "{message}");
        assert!(form.allow_not_now);
        assert!(!game.returned());
        game.reply(submit(PASSWORD));
        game.finish();
    });
    assert_eq!(
        fx.creds.get(&account()).unwrap().unwrap().expose(),
        PASSWORD
    );
    assert!(
        !fx.all_text().contains(PASSWORD),
        "the password reached a file"
    );
    assert!(
        !fx.all_text().contains(THIRD),
        "the wrong password reached a file"
    );
    assert!(!fx.log_text().contains(PASSWORD));
    assert!(!fx.log_text().contains(THIRD));
    // save-relay's own config is under Library, never in Documents (Files shows Documents).
    assert!(fx
        .roots
        .library
        .join("save-relay/config/config.json")
        .exists());
    assert!(!text_of(&fx.roots.data_root).contains(SERVER));
}

#[test]
fn a_conflict_shows_both_islands_and_nothing_plays_until_one_is_picked() {
    for keep in [Keep::Local, Keep::Server] {
        let fx = Fx::new("conflict");
        fx.set_up();
        island(&fx.saves(), "ipad-1");
        run(&fx, |game| game.finish()); // the first start
        fx.player(&fx.no_prompt()).after_game(); // the quit: pushes the island, clears the mark

        let (desktop, desktop_saves) = fx.other("desktop");
        assert!(matches!(
            desktop.pull(&desktop_saves, None).unwrap(),
            RelaySync::Done(_)
        ));
        fs::write(desktop_saves.join("main.dat"), "main desktop-2").unwrap();
        desktop.push(&desktop_saves, None).unwrap();
        // This iPad played on its own after the last sync.
        fs::write(fx.saves().join("main.dat"), "main ipad-2").unwrap();
        // (The iPad's own push at the last quit was not sent: its mark and record are old.)

        run(&fx, |game| {
            let Shown::Conflict(server, local) = game.shown() else {
                panic!("expected the conflict screen")
            };
            assert_eq!(server.device, "desktop");
            assert_ne!(server, local);
            assert!(!game.returned(), "nothing plays until one is picked");
            game.reply(Reply::Keep(keep));
            game.finish();
        });
        let expected = match keep {
            Keep::Server => "main desktop-2",
            Keep::Local => "main ipad-2",
        };
        assert_eq!(read_main(&fx.saves()), expected);
        // The side not kept is in save-relay's history.
        let history = fx.history();
        let kept_ipad = ("ipad".to_string(), true);
        let kept_desktop = ("desktop".to_string(), true);
        match keep {
            // The desktop's entry stays, no longer current; this iPad's island is the current one.
            Keep::Local => {
                assert!(history.contains(&kept_ipad), "{history:?}");
                assert!(
                    history.contains(&("desktop".to_string(), false)),
                    "{history:?}"
                );
            }
            // This iPad's island is uploaded as a history entry first; the desktop's stays current.
            Keep::Server => {
                assert!(history.contains(&kept_desktop), "{history:?}");
                let ipad_entries = history.iter().filter(|(d, c)| d == "ipad" && !c).count();
                assert!(ipad_entries >= 2, "{history:?}");
            }
        }
    }
}

#[test]
fn a_held_island_plays_only_after_play_anyway() {
    let fx = Fx::new("held");
    fx.set_up();
    island(&fx.saves(), "ipad-1");
    run(&fx, |game| game.finish());
    fx.player(&fx.no_prompt()).after_game();
    // The desktop starts a game: its pull marks the island open there.
    let (desktop, desktop_saves) = fx.other("desktop");
    assert!(matches!(
        desktop.pull(&desktop_saves, None).unwrap(),
        RelaySync::Done(_)
    ));

    run(&fx, |game| {
        let Shown::Held(device, _since) = game.shown() else {
            panic!("expected the held screen")
        };
        assert_eq!(device, "desktop");
        assert!(!game.returned());
        game.reply(Reply::Held(HeldAnswer::CheckAgain));
        let Shown::Held(..) = game.shown() else {
            panic!("expected the held screen again")
        };
        assert!(!game.returned());
        game.reply(Reply::Held(HeldAnswer::PlayAnyway));
        game.finish();
    });
}

#[test]
fn an_offline_start_refuses_until_retry_sends_the_waiting_push_and_pulls() {
    let fx = Fx::new("offline");
    fx.set_up();
    island(&fx.saves(), "v1");
    run(&fx, |game| game.finish());

    // The game ran and quit with the server unreachable: the push is deferred.
    fs::write(fx.saves().join("main.dat"), "main played-offline").unwrap();
    fx.remote.set_reachable(false);
    fx.player(&fx.no_prompt()).after_game();
    assert!(fx.roots.dirs().data.join("push-waiting").exists());

    run(&fx, |game| {
        // Next start, still unreachable: the game does not boot and the prompt says why.
        let Shown::Blocked(Blocked::Offline) = game.shown() else {
            panic!("expected the offline screen")
        };
        assert!(!game.returned());
        game.reply(Reply::Blocked(BlockedAnswer::Retry));
        // Retry with the server still down: the same screen, still not booted.
        let Shown::Blocked(Blocked::Offline) = game.shown() else {
            panic!("expected the offline screen again")
        };
        assert!(!game.returned());
        // The server is back: Retry sends the waiting push, pulls, and only then returns.
        fx.remote.set_reachable(true);
        game.reply(Reply::Blocked(BlockedAnswer::Retry));
        game.finish();
    });
    assert!(!fx.roots.dirs().data.join("push-waiting").exists());
    fx.player(&fx.no_prompt()).after_game();
    let (desktop, desktop_saves) = fx.other("desktop");
    desktop.pull(&desktop_saves, None).unwrap();
    assert_eq!(read_main(&desktop_saves), "main played-offline");
}

#[test]
fn any_other_pull_error_shows_its_message_with_retry_and_does_not_boot() {
    let fx = Fx::new("error");
    fx.set_up();
    island(&fx.saves(), "v1");
    fx.connector.broken.store(true, Ordering::SeqCst);
    run(&fx, |game| {
        let Shown::Blocked(Blocked::Error(message)) = game.shown() else {
            panic!("expected the error screen")
        };
        assert!(message.contains("fell over"), "{message}");
        assert!(!message.contains(PASSWORD));
        assert!(!game.returned());
        fx.connector.broken.store(false, Ordering::SeqCst);
        game.reply(Reply::Blocked(BlockedAnswer::Retry));
        game.finish();
    });
}

#[test]
fn a_refused_sign_in_shows_the_form_again_and_boots_only_after_a_new_password_pulls() {
    let fx = Fx::new("sign-in");
    fx.set_up();
    island(&fx.saves(), "v1");
    // The app password is revoked and a new one made on the server.
    *fx.connector.accepted.lock().unwrap() = SECOND.into();
    run(&fx, |game| {
        let Shown::SignIn(form) = game.shown() else {
            panic!("expected the sign-in form")
        };
        assert_eq!((form.server.as_str(), form.user.as_str()), (SERVER, USER));
        assert!(!form.allow_not_now, "no Not now: save sync is set up");
        assert!(!game.returned());
        // A wrong password again: the form comes back, with no password in its message.
        game.reply(submit(THIRD));
        let Shown::SignIn(form) = game.shown() else {
            panic!("expected the form again")
        };
        assert!(!form.message.unwrap_or_default().contains(THIRD));
        assert!(!form.allow_not_now);
        assert!(!game.returned());
        game.reply(submit(SECOND));
        game.finish();
    });
    // The new password replaced the old one, and is in no file or log line.
    assert_eq!(fx.creds.get(&account()).unwrap().unwrap().expose(), SECOND);
    for secret in [PASSWORD, SECOND, THIRD] {
        assert!(!fx.all_text().contains(secret), "a password reached a file");
        assert!(
            !fx.log_text().contains(secret),
            "a password reached the log"
        );
    }
}

#[test]
fn cancelling_the_sign_in_offers_retry_and_sign_in_again_and_a_retry_that_gets_401_asks_again() {
    let fx = Fx::new("sign-in-cancel");
    fx.set_up();
    island(&fx.saves(), "v1");
    *fx.connector.accepted.lock().unwrap() = SECOND.into();
    run(&fx, |game| {
        let Shown::SignIn(_) = game.shown() else {
            panic!("expected the form")
        };
        game.reply(Reply::SignIn(SignInAnswer::Cancel));
        let Shown::Blocked(Blocked::SignInCancelled) = game.shown() else {
            panic!("expected the cancelled screen")
        };
        assert!(!game.returned());
        // Retry: the pull gets 401 again, so the form comes back.
        game.reply(Reply::Blocked(BlockedAnswer::Retry));
        let Shown::SignIn(form) = game.shown() else {
            panic!("expected the form again")
        };
        assert!(!form.allow_not_now);
        game.reply(Reply::SignIn(SignInAnswer::Cancel));
        let Shown::Blocked(Blocked::SignInCancelled) = game.shown() else {
            panic!("expected the cancelled screen")
        };
        game.reply(Reply::Blocked(BlockedAnswer::SignInAgain));
        let Shown::SignIn(_) = game.shown() else {
            panic!("expected the form")
        };
        assert!(!game.returned());
        game.reply(submit(SECOND));
        game.finish();
    });
}

/// The way out of a broken setup (the iPad's blocked alert offers "Turn off save sync" on an
/// error and a cancelled sign-in, never offline): save-relay's config and secret go, Not now is
/// remembered, and the game boots with sync off and with nothing more asked.
fn turned_off(fx: &Fx) {
    assert!(fx.creds.get(&account()).unwrap().is_none());
    assert!(!fx
        .roots
        .library
        .join("save-relay/config/config.json")
        .exists());
    let ini = fs::read_to_string(fx.roots.data_root.join("config.ini")).unwrap();
    assert!(ini.contains("relay.ask = 0"), "{ini}");
    assert!(!fx.pulled.load(Ordering::SeqCst));
    assert!(fx.booted.load(Ordering::SeqCst));
    // The next start asks nothing and boots with sync off.
    run(fx, |game| game.finish());
    assert!(!fx.pulled.load(Ordering::SeqCst));
}

#[test]
fn turn_off_on_an_error_screen_boots_with_sync_off_and_asks_nothing_again() {
    let fx = Fx::new("turn-off-error");
    fx.set_up();
    island(&fx.saves(), "v1");
    fx.connector.broken.store(true, Ordering::SeqCst);
    run(&fx, |game| {
        let Shown::Blocked(Blocked::Error(_)) = game.shown() else {
            panic!("expected the error screen")
        };
        assert!(!game.returned());
        game.reply(Reply::Blocked(BlockedAnswer::TurnOff));
        game.finish();
    });
    turned_off(&fx);
    assert_eq!(read_main(&fx.saves()), "main v1");
}

#[test]
fn turn_off_after_a_cancelled_sign_in_boots_with_sync_off() {
    let fx = Fx::new("turn-off-cancel");
    fx.set_up();
    island(&fx.saves(), "v1");
    *fx.connector.accepted.lock().unwrap() = SECOND.into();
    run(&fx, |game| {
        let Shown::SignIn(_) = game.shown() else {
            panic!("expected the form")
        };
        game.reply(Reply::SignIn(SignInAnswer::Cancel));
        let Shown::Blocked(Blocked::SignInCancelled) = game.shown() else {
            panic!("expected the cancelled screen")
        };
        game.reply(Reply::Blocked(BlockedAnswer::TurnOff));
        game.finish();
    });
    turned_off(&fx);
}

#[test]
fn a_blank_ipad_set_up_from_the_prompt_pulls_the_macs_island_without_a_conflict_or_held_screen() {
    // #76 check 4: another device ("mac") has pushed an island; this iPad has no island, no
    // user/device_save and no save-relay config yet.
    let fx = Fx::new("blank-ipad");
    let (mac, mac_saves) = fx.other("mac");
    island(&mac_saves, "mac-1");
    mac.push(&mac_saves, None).unwrap();
    assert!(!fx.saves().exists());
    run(&fx, |game| {
        let Shown::SignIn(form) = game.shown() else {
            panic!("expected the sign-in form")
        };
        assert!(form.allow_not_now);
        game.reply(submit(PASSWORD));
        // No conflict screen and no held-island screen: `finish` fails on any other screen.
        game.finish();
    });
    assert!(fx.pulled.load(Ordering::SeqCst));
    assert_eq!(read_main(&fx.saves()), "main mac-1");
    assert_eq!(
        fs::read_to_string(fx.saves().join("Villager0/personal.dat")).unwrap(),
        "villager mac-1"
    );
}

#[test]
fn a_synced_quit_pushes_the_island_and_clears_the_waiting_mark() {
    let fx = Fx::new("quit");
    fx.set_up();
    island(&fx.saves(), "v1");
    run(&fx, |game| game.finish());
    fs::write(fx.saves().join("main.dat"), "main after-play").unwrap();
    fx.player(&fx.no_prompt()).after_game();
    assert!(!fx.roots.dirs().data.join("push-waiting").exists());
    let (desktop, desktop_saves) = fx.other("desktop");
    desktop.pull(&desktop_saves, None).unwrap();
    assert_eq!(read_main(&desktop_saves), "main after-play");
}

#[test]
fn after_game_before_the_game_has_booted_changes_nothing() {
    let fx = Fx::new("early-push");
    fx.set_up();
    island(&fx.saves(), "ipad-1");
    run(&fx, |game| game.finish());
    fx.player(&fx.no_prompt()).after_game();
    let (desktop, desktop_saves) = fx.other("desktop");
    desktop.pull(&desktop_saves, None).unwrap();

    // The next start: the island is held, and the held screen waits.
    fx.booted.store(false, Ordering::SeqCst);
    fs::write(fx.saves().join("main.dat"), "main unsynced").unwrap();
    run(&fx, |game| {
        let Shown::Held(device, _) = game.shown() else {
            panic!("expected the held screen")
        };
        assert_eq!(device, "desktop");
        // The app goes to the background while the held screen is up: nothing is pushed.
        let before = fx.history();
        fx.player(&fx.no_prompt()).after_game();
        assert_eq!(fx.history(), before);
        assert!(!fx.roots.dirs().data.join("push-waiting").exists());
        assert!(!game.returned());
        game.reply(Reply::Held(HeldAnswer::CheckAgain));
        // The same screen comes back, and Play anyway still boots.
        let Shown::Held(device, _) = game.shown() else {
            panic!("expected the held screen again")
        };
        assert_eq!(device, "desktop");
        game.reply(Reply::Held(HeldAnswer::PlayAnyway));
        game.finish();
    });
    assert!(fx.log_text().contains("has not booted"));
}

#[test]
fn a_conflict_after_play_anyway_goes_through_the_conflict_screen_and_resolves() {
    let fx = Fx::new("anyway-conflict");
    fx.set_up();
    island(&fx.saves(), "ipad-1");
    run(&fx, |game| game.finish());
    fx.player(&fx.no_prompt()).after_game();
    // The desktop plays and quits (pushes), then starts again: it holds the island.
    let (desktop, desktop_saves) = fx.other("desktop");
    desktop.pull(&desktop_saves, None).unwrap();
    fs::write(desktop_saves.join("main.dat"), "main desktop-2").unwrap();
    desktop.push(&desktop_saves, None).unwrap();
    desktop.pull(&desktop_saves, None).unwrap();
    // And the iPad changed its own island meanwhile.
    fs::write(fx.saves().join("main.dat"), "main ipad-2").unwrap();

    fx.booted.store(false, Ordering::SeqCst);
    run(&fx, |game| {
        let Shown::Held(..) = game.shown() else {
            panic!("expected the held screen")
        };
        game.reply(Reply::Held(HeldAnswer::PlayAnyway));
        // Play anyway does not skip the conflict: it is asked, once, and settled.
        let Shown::Conflict(..) = game.shown() else {
            panic!("expected the conflict screen")
        };
        assert!(!game.returned());
        game.reply(Reply::Keep(Keep::Server));
        game.finish();
    });
    assert_eq!(read_main(&fx.saves()), "main desktop-2");
}

#[test]
fn a_panic_shows_the_error_screen_and_never_boots_without_syncing() {
    let fx = Fx::new("panic");
    fx.set_up();
    island(&fx.saves(), "v1");
    fx.connector.panic_once.store(true, Ordering::SeqCst);
    run(&fx, |game| {
        let Shown::Blocked(Blocked::Error(_)) = game.shown() else {
            panic!("expected the error screen")
        };
        assert!(!game.returned());
        assert!(!fx.booted.load(Ordering::SeqCst));
        game.reply(Reply::Blocked(BlockedAnswer::Retry));
        game.finish();
    });
    assert!(fx.booted.load(Ordering::SeqCst));
}

#[test]
fn turn_off_on_the_panic_screen_forgets_the_setup_and_boots_with_sync_off() {
    let fx = Fx::new("panic-turn-off");
    fx.set_up();
    island(&fx.saves(), "v1");
    fx.connector.panic_once.store(true, Ordering::SeqCst);
    run(&fx, |game| {
        let Shown::Blocked(Blocked::Error(_)) = game.shown() else {
            panic!("expected the error screen")
        };
        assert!(!game.returned());
        game.reply(Reply::Blocked(BlockedAnswer::TurnOff));
        game.finish();
    });
    turned_off(&fx);
}

#[test]
fn not_now_leaves_a_config_ini_that_does_not_parse_exactly_as_it_was() {
    let fx = Fx::new("bad-ini");
    island(&fx.saves(), "v1");
    let ini = fx.roots.data_root.join("config.ini");
    fs::write(&ini, "a line with no equals sign\nvideo.fullscreen = 1\n").unwrap();
    run(&fx, |game| {
        let Shown::SignIn(_) = game.shown() else {
            panic!("expected the sign-in form")
        };
        game.reply(Reply::SignIn(SignInAnswer::NotNow));
        game.finish();
    });
    assert_eq!(
        fs::read_to_string(&ini).unwrap(),
        "a line with no equals sign\nvideo.fullscreen = 1\n"
    );
}

#[test]
fn a_push_that_cannot_read_the_credential_is_left_waiting() {
    let fx = Fx::new("locked");
    fx.set_up();
    island(&fx.saves(), "v1");
    run(&fx, |game| game.finish());
    fx.creds.unreadable.store(true, Ordering::SeqCst);
    fx.player(&fx.no_prompt()).after_game();
    assert!(fx.roots.dirs().data.join("push-waiting").exists());
    assert!(!fx.log_text().contains(PASSWORD));
}

#[test]
fn the_sign_in_answer_never_prints_its_password() {
    let answer = SignInAnswer::Submit {
        server: SERVER.into(),
        user: USER.into(),
        password: PASSWORD.into(),
    };
    let shown = format!("{answer:?} {:?}", Reply::SignIn(answer.clone()));
    assert!(!shown.contains(PASSWORD), "{shown}");
}

// ---- the background push (a checkpoint): the iPad stays the one playing ------------------------

fn ipad_mark(fx: &Fx) -> Option<String> {
    let manifest = save_relay::manifest::Manifest::from_json(&saves::relay_manifest()).unwrap();
    let layout = save_relay::layout::Layout::new(save_relay::cli::DEFAULT_ROOT).unwrap();
    let (bytes, _) = fx.remote.get(&layout.playing_path(&manifest.id)).ok()?;
    save_relay::presence::Mark::from_json(&String::from_utf8_lossy(&bytes))
        .ok()
        .map(|mark| mark.device)
}

#[test]
fn a_background_push_keeps_the_ipad_playing_and_leaves_another_devices_mark_alone() {
    let fx = Fx::new("checkpoint");
    fx.set_up();
    island(&fx.saves(), "v1");
    run(&fx, |game| game.finish());
    assert_eq!(ipad_mark(&fx).as_deref(), Some("ipad"));
    // To the background: pushed, still marked.
    fs::write(fx.saves().join("main.dat"), "main paused").unwrap();
    fx.player(&fx.no_prompt()).checkpoint();
    assert_eq!(ipad_mark(&fx).as_deref(), Some("ipad"));
    let (desktop, desktop_saves) = fx.other("desktop");
    assert!(matches!(
        desktop.pull(&desktop_saves, None).unwrap(),
        RelaySync::Held { .. }
    ));
    // The desktop takes it over with Play anyway; the iPad's next background push leaves its mark.
    desktop.pull_anyway(&desktop_saves, None).unwrap();
    assert_eq!(read_main(&desktop_saves), "main paused");
    fx.player(&fx.no_prompt()).checkpoint();
    assert_eq!(ipad_mark(&fx).as_deref(), Some("desktop"));
    assert!(fx.log_text().contains("another device (desktop"));
    // The quit clears only the iPad's own mark: the desktop's stays.
    fx.player(&fx.no_prompt()).after_game();
    assert_eq!(ipad_mark(&fx).as_deref(), Some("desktop"));
}

#[test]
fn a_failed_mark_after_a_good_background_push_reports_the_push_and_comes_back_on_foreground() {
    let fx = Fx::new("ios-remark");
    fx.set_up();
    island(&fx.saves(), "v1");
    run(&fx, |game| game.finish());
    fs::write(fx.saves().join("main.dat"), "main paused").unwrap();
    fx.connector.fail_mark_puts.store(2, Ordering::SeqCst);
    fx.player(&fx.no_prompt()).checkpoint();
    assert!(fx
        .log_text()
        .contains("could not be marked as playing again"));
    assert!(!fx.roots.dirs().data.join("push-waiting").exists());
    assert_eq!(ipad_mark(&fx), None);
    assert!(fx.roots.dirs().data.join("mark-waiting").exists());
    // Back to the front (acnh_relay_foreground): marked again, and the push did go.
    fx.player(&fx.no_prompt()).resume();
    assert_eq!(ipad_mark(&fx).as_deref(), Some("ipad"));
    assert!(!fx.roots.dirs().data.join("mark-waiting").exists());
    let (desktop, desktop_saves) = fx.other("desktop");
    assert!(matches!(
        desktop.pull(&desktop_saves, None).unwrap(),
        RelaySync::Held { .. }
    ));
    desktop.pull_anyway(&desktop_saves, None).unwrap();
    assert_eq!(read_main(&desktop_saves), "main paused");
}

#[test]
fn a_background_push_with_nothing_new_keeps_the_mark_and_one_against_a_newer_island_overwrites_nothing(
) {
    let fx = Fx::new("ios-noop-newer");
    fx.set_up();
    island(&fx.saves(), "v1");
    run(&fx, |game| game.finish());
    fx.player(&fx.no_prompt()).checkpoint();
    assert_eq!(ipad_mark(&fx).as_deref(), Some("ipad"));
    // The desktop plays anyway and pushes a newer island; the iPad changed its own too.
    let (desktop, desktop_saves) = fx.other("desktop");
    desktop.pull_anyway(&desktop_saves, None).unwrap();
    fs::write(desktop_saves.join("main.dat"), "main desktop newer").unwrap();
    desktop.push(&desktop_saves, None).unwrap();
    fs::write(fx.saves().join("main.dat"), "main ipad also").unwrap();
    fx.player(&fx.no_prompt()).checkpoint();
    assert!(fx.log_text().contains("the push is waiting"));
    let (laptop, laptop_saves) = fx.other("laptop");
    laptop.pull_anyway(&laptop_saves, None).unwrap();
    assert_eq!(read_main(&laptop_saves), "main desktop newer");
}

// ---- the Saves page's Sync now (issue #260): the checkpoint's push, on demand ----------------

/// A booted game, set up and pulled, its island "v1".
fn playing(name: &str) -> Fx {
    let fx = Fx::new(name);
    fx.set_up();
    island(&fx.saves(), "v1");
    run(&fx, |game| game.finish());
    assert!(fx.pulled.load(Ordering::SeqCst));
    fx
}

#[test]
fn sync_now_uploads_keeps_the_ipad_playing_and_says_synced_then_nothing_new() {
    let fx = playing("ios-sync-now");
    let prompt = fx.no_prompt();
    let player = fx.player(&prompt);
    let pulled_at = fx.synced.load(Ordering::SeqCst);
    assert!(pulled_at > 0, "the start's pull is the first sync");
    assert_eq!(player.sync_now_ready(false), SyncReady::Ready);
    assert_eq!(player.service_label(), Some("Nextcloud"));
    fs::write(fx.saves().join("main.dat"), "main played").unwrap();
    assert_eq!(player.sync_now(), SyncNow::Synced);
    assert!(fx.synced.load(Ordering::SeqCst) >= pulled_at);
    assert_eq!(
        ipad_mark(&fx).as_deref(),
        Some("ipad"),
        "still the one playing"
    );
    assert!(!fx.roots.dirs().data.join("push-waiting").exists());
    assert_eq!(player.sync_now(), SyncNow::NothingNew);
    assert_eq!(ipad_mark(&fx).as_deref(), Some("ipad"));
    let (desktop, desktop_saves) = fx.other("desktop");
    assert!(matches!(
        desktop.pull(&desktop_saves, None).unwrap(),
        RelaySync::Held { .. }
    ));
    desktop.pull_anyway(&desktop_saves, None).unwrap();
    assert_eq!(read_main(&desktop_saves), "main played");
    assert!(!fx.log_text().contains(PASSWORD));
}

#[test]
fn sync_now_with_no_island_saved_yet_pulls_nothing_under_the_running_game() {
    let fx = playing("ios-sync-now-empty");
    let prompt = fx.no_prompt();
    let player = fx.player(&prompt);
    fs::remove_dir_all(fx.saves()).unwrap();
    fs::create_dir_all(fx.saves()).unwrap();
    assert_eq!(player.sync_now(), SyncNow::NothingNew);
    assert!(
        !fx.saves().join("main.dat").exists(),
        "the server's island is not pulled mid-game"
    );
}

#[test]
fn sync_now_against_a_newer_island_overwrites_nothing_never_pulls_and_waits_for_the_next_start() {
    let fx = playing("ios-sync-now-newer");
    let prompt = fx.no_prompt();
    let player = fx.player(&prompt);
    let (desktop, desktop_saves) = fx.other("desktop");
    desktop.pull_anyway(&desktop_saves, None).unwrap();
    fs::write(desktop_saves.join("main.dat"), "main desktop newer").unwrap();
    desktop.push(&desktop_saves, None).unwrap();
    fs::write(fx.saves().join("main.dat"), "main ipad also").unwrap();
    assert_eq!(player.sync_now(), SyncNow::Newer);
    assert_eq!(read_main(&fx.saves()), "main ipad also", "nothing pulled");
    assert!(fx.roots.dirs().data.join("push-waiting").exists());
    let (laptop, laptop_saves) = fx.other("laptop");
    laptop.pull_anyway(&laptop_saves, None).unwrap();
    assert_eq!(read_main(&laptop_saves), "main desktop newer");

    // The island moved on and the iPad's did not change: no mark is claimed over it.
    let fx = playing("ios-sync-now-moved-on");
    let prompt = fx.no_prompt();
    let player = fx.player(&prompt);
    let (desktop, desktop_saves) = fx.other("desktop");
    desktop.pull_anyway(&desktop_saves, None).unwrap();
    fs::write(desktop_saves.join("main.dat"), "main desktop newer").unwrap();
    desktop.push(&desktop_saves, None).unwrap();
    assert_eq!(player.sync_now(), SyncNow::Newer);
    assert_eq!(ipad_mark(&fx), None);
    assert_eq!(read_main(&fx.saves()), "main v1");
}

#[test]
fn sync_now_when_another_device_took_the_island_over_leaves_its_mark() {
    let fx = playing("ios-sync-now-taken");
    let prompt = fx.no_prompt();
    let player = fx.player(&prompt);
    let (desktop, desktop_saves) = fx.other("desktop");
    desktop.pull_anyway(&desktop_saves, None).unwrap();
    assert_eq!(player.sync_now(), SyncNow::TakenOver);
    assert_eq!(ipad_mark(&fx).as_deref(), Some("desktop"));
}

#[test]
fn sync_now_is_off_for_a_game_started_with_sync_off() {
    let fx = Fx::new("ios-sync-now-off");
    island(&fx.saves(), "v1");
    assert_eq!(
        fx.player(&fx.no_prompt()).sync_now_ready(false),
        SyncReady::NotBooted
    );
    run(&fx, |game| {
        let Shown::SignIn(_) = game.shown() else {
            panic!("expected the sign-in form")
        };
        game.reply(Reply::SignIn(SignInAnswer::NotNow));
        game.finish();
    });
    assert_eq!(fx.synced.load(Ordering::SeqCst), 0);
    // Set up while the game runs: still no push from this game.
    fx.set_up();
    let prompt = fx.no_prompt();
    let player = fx.player(&prompt);
    assert_eq!(player.sync_now_ready(false), SyncReady::StartedOff);
    assert_eq!(player.sync_now(), SyncNow::Off);
    assert_eq!(ipad_mark(&fx), None);
    assert_eq!(player.sync_now_ready(true), SyncReady::StartedOff);
}

#[test]
fn sync_now_with_a_refused_or_unreadable_password_never_asks_mid_game() {
    let fx = playing("ios-sync-now-signed-out");
    let prompt = fx.no_prompt();
    let player = fx.player(&prompt);
    assert_eq!(player.sync_now_ready(true), SyncReady::Busy);
    // The server refuses the app password: signed out, the push waits.
    *fx.connector.accepted.lock().unwrap() = SECOND.into();
    fs::write(fx.saves().join("main.dat"), "main played").unwrap();
    assert_eq!(player.sync_now(), SyncNow::SignedOut);
    assert!(fx.roots.dirs().data.join("push-waiting").exists());
    // The Keychain cannot be read (locked): the button says signed out, a press is an error.
    fx.creds.unreadable.store(true, Ordering::SeqCst);
    assert_eq!(player.sync_now_ready(false), SyncReady::SignedOut);
    assert_eq!(player.sync_now(), SyncNow::Error);
    // The password is gone from the Keychain.
    fx.creds.unreadable.store(false, Ordering::SeqCst);
    fx.creds.delete(&account()).unwrap();
    assert_eq!(player.sync_now_ready(false), SyncReady::SignedOut);
    assert_eq!(player.sync_now(), SyncNow::SignedOut);
    assert!(!fx.log_text().contains(PASSWORD));
    assert!(!fx.log_text().contains(SECOND));
}

#[test]
fn sync_now_offline_or_on_a_broken_server_waits_for_the_next_start() {
    let fx = playing("ios-sync-now-offline");
    let prompt = fx.no_prompt();
    let player = fx.player(&prompt);
    fs::write(fx.saves().join("main.dat"), "main played").unwrap();
    fx.connector.offline.store(true, Ordering::SeqCst);
    assert_eq!(player.sync_now(), SyncNow::Offline);
    assert!(fx.roots.dirs().data.join("push-waiting").exists());
    fx.connector.offline.store(false, Ordering::SeqCst);
    fx.connector.broken.store(true, Ordering::SeqCst);
    assert_eq!(player.sync_now(), SyncNow::Error);
    assert!(fx.roots.dirs().data.join("push-waiting").exists());
    fx.connector.broken.store(false, Ordering::SeqCst);
    assert_eq!(player.sync_now(), SyncNow::Synced);
    assert!(!fx.roots.dirs().data.join("push-waiting").exists());
    // Save sync turned off meanwhile (its config gone): off.
    fs::remove_file(fx.roots.dirs().config.join("config.json")).unwrap();
    assert_eq!(player.sync_now_ready(false), SyncReady::Off);
    assert_eq!(player.sync_now(), SyncNow::Off);
}

// ---- the final push on terminate (issue #76) -------------------------------------------------

#[test]
fn a_second_final_push_while_one_runs_returns_at_once_and_runs_nothing() {
    static FINAL: FinalPush = FinalPush::new();
    let (release, held) = channel::<()>();
    let (started_tx, started) = channel::<()>();
    let first = std::thread::spawn(move || {
        FINAL.run(|| {
            started_tx.send(()).unwrap();
            let _ = held.recv_timeout(Duration::from_secs(20));
        })
    });
    started.recv_timeout(Duration::from_secs(20)).unwrap();
    let ran = AtomicBool::new(false);
    let begun = Instant::now();
    // Both entry points, the blocking one and the bounded one, find it taken.
    assert_eq!(
        FINAL.run(|| ran.store(true, Ordering::SeqCst)),
        Bounded::Taken
    );
    let bounded = FINAL.run_bounded(Duration::from_secs(10), || panic!("must not run"));
    assert_eq!(bounded, Bounded::Taken);
    assert!(begun.elapsed() < Duration::from_secs(1), "it waited");
    assert!(!ran.load(Ordering::SeqCst));
    release.send(()).unwrap();
    assert_eq!(first.join().unwrap(), Bounded::Done);
    // Done is taken too: a late caller does not push twice.
    assert_eq!(FINAL.run(|| panic!("must not run")), Bounded::Taken);
}

#[test]
fn a_final_push_over_the_bound_returns_late_leaves_the_island_waiting_and_goes_on() {
    static FINAL: FinalPush = FinalPush::new();
    let fx = Arc::new(Fx::new("terminate-late"));
    fx.set_up();
    island(&fx.saves(), "v1");
    run(&fx, |game| game.finish());
    fs::write(fx.saves().join("main.dat"), "main after-play").unwrap();
    fx.connector.stall.store(true, Ordering::SeqCst);

    let worker = fx.clone();
    let begun = Instant::now();
    let outcome = FINAL.run_bounded(Duration::from_millis(300), move || {
        worker.player(&worker.no_prompt()).after_game();
    });
    assert_eq!(outcome, Bounded::Late);
    assert!(begun.elapsed() < Duration::from_secs(5), "it blocked");
    // Still uploading, and already marked: a kill now leaves the push for the next start.
    assert!(fx.roots.dirs().data.join("push-waiting").exists());
    assert_eq!(
        FINAL.run_bounded(Duration::from_millis(10), || panic!("must not run")),
        Bounded::Taken
    );

    // The server answers: the push finishes on its worker and clears the mark.
    fx.connector.stall.store(false, Ordering::SeqCst);
    let start = Instant::now();
    while fx.roots.dirs().data.join("push-waiting").exists() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the push never finished"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let (desktop, desktop_saves) = fx.other("desktop");
    desktop.pull(&desktop_saves, None).unwrap();
    assert_eq!(read_main(&desktop_saves), "main after-play");
}

#[test]
fn a_final_push_inside_the_bound_is_done_and_clears_the_mark() {
    static FINAL: FinalPush = FinalPush::new();
    let fx = Arc::new(Fx::new("terminate-done"));
    fx.set_up();
    island(&fx.saves(), "v1");
    run(&fx, |game| game.finish());
    fs::write(fx.saves().join("main.dat"), "main after-play").unwrap();
    let worker = fx.clone();
    let outcome = FINAL.run_bounded(Duration::from_secs(20), move || {
        worker.player(&worker.no_prompt()).after_game();
    });
    assert_eq!(outcome, Bounded::Done);
    assert!(!fx.roots.dirs().data.join("push-waiting").exists());
}

// ---- Dropbox on the iPad (issue #361) ---------------------------------------------------------

fn dropbox_config(fx: &Fx) -> Option<Config> {
    LocalStore::new(fx.roots.dirs()).load_config().unwrap()
}

#[test]
fn the_first_start_can_sign_in_to_dropbox_with_the_code_flow() {
    let fx = Fx::new("dropbox");
    island(&fx.saves(), "v1");
    run(&fx, |game| {
        let Shown::SignIn(form) = game.shown() else {
            panic!("expected the sign-in form")
        };
        assert!(form.allow_not_now);
        game.reply(Reply::SignIn(SignInAnswer::Dropbox));
        let Shown::DropboxCode(url) = game.shown() else {
            panic!("expected the code screen")
        };
        assert_eq!(url, DROPBOX_URL);
        assert!(!game.returned());
        game.reply(Reply::Code(Some(DROPBOX_CODE.into())));
        game.finish();
    });
    let config = dropbox_config(&fx).expect("save sync is set up");
    assert_eq!(config.server, "dropbox");
    let account =
        save_relay::service::token_account(save_relay::service::Service::Dropbox, "me").unwrap();
    assert_eq!(
        fx.creds.get(&account).unwrap().unwrap().expose(),
        DROPBOX_TOKEN
    );
    // The refresh token is only in the credential store, never in a file or the log.
    assert!(!fx.all_text().contains(DROPBOX_TOKEN));
    assert!(!fx.log_text().contains(DROPBOX_TOKEN));
    assert!(!fx.log_text().contains(DROPBOX_CODE));
}

#[test]
fn backing_out_of_the_dropbox_code_screen_returns_to_the_form_and_sets_nothing_up() {
    let fx = Fx::new("dropbox-back");
    island(&fx.saves(), "v1");
    run(&fx, |game| {
        let Shown::SignIn(_) = game.shown() else {
            panic!("expected the sign-in form")
        };
        game.reply(Reply::SignIn(SignInAnswer::Dropbox));
        let Shown::DropboxCode(_) = game.shown() else {
            panic!("expected the code screen")
        };
        game.reply(Reply::Code(None));
        let Shown::SignIn(form) = game.shown() else {
            panic!("expected the form again")
        };
        assert!(form.allow_not_now);
        game.reply(Reply::SignIn(SignInAnswer::NotNow));
        game.finish();
    });
    assert!(dropbox_config(&fx).is_none());
}

#[test]
fn a_refused_dropbox_code_and_an_unreachable_dropbox_show_the_form_again_with_the_reason() {
    let fx = Fx::new("dropbox-refused");
    island(&fx.saves(), "v1");
    run(&fx, |game| {
        let Shown::SignIn(_) = game.shown() else {
            panic!("expected the sign-in form")
        };
        game.reply(Reply::SignIn(SignInAnswer::Dropbox));
        let Shown::DropboxCode(_) = game.shown() else {
            panic!("expected the code screen")
        };
        game.reply(Reply::Code(Some("wrong-code-1".into())));
        let Shown::SignIn(form) = game.shown() else {
            panic!("expected the form again")
        };
        let message = form.message.expect("why");
        assert!(message.contains("refused"), "{message}");
        assert!(!message.contains("wrong-code-1"), "{message}");
        // Offline: the page cannot start, the form says so.
        fx.connector.offline.store(true, Ordering::SeqCst);
        game.reply(Reply::SignIn(SignInAnswer::Dropbox));
        let Shown::SignIn(form) = game.shown() else {
            panic!("expected the form again")
        };
        assert!(form.message.expect("why").contains("could not be reached"));
        fx.connector.offline.store(false, Ordering::SeqCst);
        // Then Dropbox again, and the right code.
        game.reply(Reply::SignIn(SignInAnswer::Dropbox));
        let Shown::DropboxCode(_) = game.shown() else {
            panic!("expected the code screen")
        };
        game.reply(Reply::Code(Some(DROPBOX_CODE.into())));
        game.finish();
    });
    assert_eq!(dropbox_config(&fx).unwrap().server, "dropbox");
}

#[test]
fn nextcloud_still_works_beside_dropbox() {
    // The same form: choosing Nextcloud (Submit) sets up the app password as before.
    let fx = Fx::new("dropbox-nextcloud");
    island(&fx.saves(), "v1");
    run(&fx, |game| {
        let Shown::SignIn(_) = game.shown() else {
            panic!("expected the sign-in form")
        };
        game.reply(submit(PASSWORD));
        game.finish();
    });
    assert_eq!(dropbox_config(&fx).unwrap().server, SERVER);
}

#[test]
fn a_lost_dropbox_token_asks_for_a_dropbox_sign_in_again() {
    let fx = Fx::new("dropbox-again");
    island(&fx.saves(), "v1");
    // Set up as Dropbox, then the token is gone (the Keychain item was removed).
    fx.connector.dropbox_started.store(true, Ordering::SeqCst);
    fx.connector
        .dropbox_finish(DROPBOX_CODE, &fx.roots.dirs(), &fx.creds)
        .unwrap();
    let account =
        save_relay::service::token_account(save_relay::service::Service::Dropbox, "me").unwrap();
    fx.creds.delete(&account).unwrap();
    run(&fx, |game| {
        // The config names Dropbox but its token is gone: the form comes back for Dropbox.
        let Shown::SignIn(form) = game.shown() else {
            panic!("expected the sign-in form")
        };
        assert_eq!(form.server, "dropbox");
        assert!(!form.allow_not_now);
        assert!(form.message.expect("why").contains("Dropbox"));
        game.reply(Reply::SignIn(SignInAnswer::Dropbox));
        let Shown::DropboxCode(_) = game.shown() else {
            panic!("expected the code screen")
        };
        game.reply(Reply::Code(Some(DROPBOX_CODE.into())));
        game.finish();
    });
}
