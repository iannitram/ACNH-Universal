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
    decide, describe_card, Action, Blocked, BlockedAnswer, Connector, FileSettings, HeldAnswer,
    Player, Prompt, Roots, SignInAnswer, SignInForm,
};
use save_relay::creds::{self, CredentialStore, MemoryStore, Secret};
use save_relay::local::{Config, LocalStore};
use save_relay::platform::AppDirs;
use save_relay::remote::{MemoryRemote, Precondition, Remote, RemoteEntry};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SERVER: &str = "https://cloud.test";
const USER: &str = "alex";
const PASSWORD: &str = "first-app-password-4711";
const SECOND: &str = "second-app-password-8142";
const THIRD: &str = "third-app-password-5593";

static NEXT: AtomicUsize = AtomicUsize::new(0);

// ---- the fake server -------------------------------------------------------------------------

/// The in-memory remote behind a password check and a "broken" switch: a wrong or revoked app
/// password answers `AuthFailed` (a 401 on the wire), a broken server answers `Server`.
struct Gate {
    inner: Arc<MemoryRemote>,
    password_ok: bool,
    broken: Arc<AtomicBool>,
}

impl Gate {
    fn check(&self) -> save_relay::Result<()> {
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
        let account = creds::account(&config.server, &config.user).unwrap();
        let Some(secret) = creds
            .get(&account)
            .map_err(|e| Error::Saves(e.to_string()))?
        else {
            return Ok(None);
        };
        let gate = Gate {
            inner: self.remote.clone(),
            password_ok: secret.expose() == *self.accepted.lock().unwrap(),
            broken: self.broken.clone(),
        };
        Ok(Some(
            SaveRelay::new(Arc::new(gate), store, config.device).with_root(config.root),
        ))
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
}

#[derive(Debug)]
enum Reply {
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
            },
            remote,
            creds: Creds {
                inner: MemoryStore::new(),
                unreadable: AtomicBool::new(false),
            },
            booted: AtomicBool::new(false),
            pulled: AtomicBool::new(false),
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
