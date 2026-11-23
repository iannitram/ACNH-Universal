//! Setting up save sync in the guided first run and in Settings, driven the way the window
//! drives it (`FirstRun` and `RelaySettings` with Nav presses and pumps), over a scripted setup
//! (`FakeSetup`) or save-relay's real adapter over a fake Nextcloud. No network, no window.
#![cfg(not(any(target_os = "android", target_os = "ios")))]

#[path = "../../crates/cli/tests/support/fake_dav.rs"]
mod fake_dav;

use acnh_app::first_run::FirstRun;
use acnh_app::play::{Runner, Session};
use acnh_app::relay::{choose, opened_each_step, SharedRelay};
use acnh_app::relay_setup::fake::FakeSetup;
use acnh_app::relay_setup::{RelaySettings, RelaySetup, SystemSetup};
use acnh_app::vm::continue_page::ContinueState;
use acnh_app::vm::first_run::{Event, Step};
use acnh_app::vm::relay_setup::{Phase, Prompt, Service};
use acnh_app::vm::text_entry::{Key, COLUMNS};
use acnh_app::vm::{ContinuePageVm, Nav};
use acnh_core::launch::Game;
use acnh_core::platform::Vars;
use acnh_core::savecard::SaveCard;
use acnh_core::Result;
use fake_dav::FakeDav;
use save_relay::creds::{CredentialStore, MemoryStore, Secret};
use save_relay::http::Transport;
use save_relay::platform::AppDirs;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PASSWORD: &str = "app-password";
const WRONG: &str = "Wr0ng-Passw0rd";

fn vars() -> Vars {
    let root = PathBuf::from("/fake");
    Vars {
        home: root.clone(),
        data: root.clone(),
        config: root,
    }
}

fn google_prompt() -> Prompt {
    Prompt {
        url: "https://example.test/device".into(),
        code: Some("ABCD-EFGH".into()),
        needs_code: false,
        browser: false,
    }
}

/// A first run that has just finished the build.
fn first_run_at_the_relay_step(setup: Arc<dyn RelaySetup>) -> FirstRun {
    let mut first = FirstRun::with_relay(vars(), setup);
    first.vm.step = Step::GetBuild;
    first.vm.event(Event::Finished(Ok(())));
    first
}

/// Pump, as the window's timer does, until `done`.
fn until(first: &mut FirstRun, what: &str, done: impl Fn(&FirstRun) -> bool) {
    let started = Instant::now();
    while !done(first) {
        assert!(started.elapsed() < Duration::from_secs(20), "{what}");
        first.pump();
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn walk_to(first: &mut FirstRun, index: usize) {
    for _ in 0..200 {
        let at = first.vm.relay.entry().expect("keyboard open").focus();
        if at == index {
            return;
        }
        let nav = if at / COLUMNS < index / COLUMNS {
            Nav::Down
        } else if at / COLUMNS > index / COLUMNS {
            Nav::Up
        } else if at < index {
            Nav::Right
        } else {
            Nav::Left
        };
        first.nav(nav);
    }
    panic!("focus never reached key {index}");
}

fn press(first: &mut FirstRun, key: Key) {
    let index = first
        .vm
        .relay
        .entry()
        .unwrap()
        .keys()
        .iter()
        .position(|k| *k == key)
        .unwrap();
    walk_to(first, index);
    first.nav(Nav::Accept);
}

/// Type with D-pad and Accept only, switching layers as a person would.
fn pad_type(first: &mut FirstRun, text: &str) {
    for c in text.chars() {
        for _ in 0..3 {
            let entry = first.vm.relay.entry().expect("keyboard open");
            if let Some(index) = entry.keys().iter().position(|k| *k == Key::Char(c)) {
                walk_to(first, index);
                first.nav(Nav::Accept);
                break;
            }
            let switch =
                if c.is_alphabetic() && entry.layer() != acnh_app::vm::text_entry::Layer::Symbols {
                    Key::Shift
                } else {
                    Key::Layers
                };
            press(first, switch);
        }
    }
}

/// Fill one form row with the controller: Accept on it, type, Done.
fn fill_row(first: &mut FirstRun, row: usize, text: &str) {
    while first.vm.relay.focus != row {
        first.nav(if first.vm.relay.focus < row {
            Nav::Down
        } else {
            Nav::Up
        });
    }
    first.nav(Nav::Accept);
    pad_type(first, text);
    press(first, Key::Done);
}

fn connect(first: &mut FirstRun) {
    while first.vm.relay.focus != 3 {
        first.nav(if first.vm.relay.focus < 3 {
            Nav::Down
        } else {
            Nav::Up
        });
    }
    first.nav(Nav::Accept);
}

#[test]
fn nextcloud_is_set_up_with_the_controller_and_a_wrong_password_keeps_the_fields() {
    let fake = FakeSetup::new(PASSWORD, google_prompt());
    let mut first = first_run_at_the_relay_step(fake.clone());
    assert_eq!(first.vm.step, Step::Relay);
    assert!(first.vm.relay_available && !first.vm.relay_set_up);

    first.nav(Nav::Accept); // Nextcloud
    assert_eq!(first.vm.relay.phase, Phase::Form);
    // The server box starts with https://, so only the rest is typed.
    first.nav(Nav::Accept);
    pad_type(&mut first, "cloud.test");
    press(&mut first, Key::Done);
    fill_row(&mut first, 1, "alex");
    fill_row(&mut first, 2, WRONG);
    assert!(first
        .vm
        .options()
        .contains(&"App password: entered".to_string()));
    connect(&mut first);

    until(&mut first, "wrong password answered", |f| {
        f.vm.relay.phase == Phase::Form && !f.vm.relay.message.is_empty()
    });
    assert_eq!(
        first.vm.relay.message,
        "The server did not accept that user name and app password."
    );
    let options = first.vm.options();
    assert_eq!(options[0], "Server address: https://cloud.test");
    assert_eq!(options[1], "User name: alex");
    assert_eq!(options[2], "App password: not entered");
    assert_eq!(first.vm.step, Step::Relay);
    assert!(!first.vm.relay_set_up);
    // The password is in no field of the model, in what the page draws, or in what the fake saw.
    let seen = format!(
        "{:?}{:?}{}",
        first.vm.relay,
        first.vm.options(),
        first.vm.title()
    );
    assert!(!seen.contains(WRONG), "{seen}");
    assert!(fake.calls().iter().all(|call| !call.contains(WRONG)));

    // The right password goes on to the device and Steam step.
    fill_row(&mut first, 2, PASSWORD);
    connect(&mut first);
    until(&mut first, "set up", |f| f.vm.step == Step::DeviceAndSteam);
    assert!(first.vm.relay_set_up);
    assert_eq!(fake.calls().len(), 2);
    let after = format!("{:?}", first.vm.relay);
    assert!(!after.contains(PASSWORD), "{after}");
}

#[test]
fn skip_goes_on_to_add_to_steam_with_save_sync_off() {
    let fake = FakeSetup::new(PASSWORD, google_prompt());
    let mut first = first_run_at_the_relay_step(fake.clone());
    for _ in 0..3 {
        first.nav(Nav::Down);
    }
    first.nav(Nav::Accept);
    assert_eq!(first.vm.step, Step::DeviceAndSteam);
    assert!(fake.account().is_none());
    assert!(fake.calls().is_empty());
}

#[test]
fn physical_keyboard_keys_edit_the_field_and_enter_and_esc_follow_the_brief() {
    let mut first = first_run_at_the_relay_step(FakeSetup::new(PASSWORD, google_prompt()));
    first.nav(Nav::Accept);
    first.nav(Nav::Down);
    first.nav(Nav::Accept); // User name
    assert!(first.vm.keypad_open());
    for key in ["a", "l", " ", "x", "\u{8}"] {
        assert!(first.text_key(key));
    }
    // Backspace key of Slint is U+0008; Space typed a space.
    assert!(first.vm.title().contains("al |"), "{}", first.vm.title());
    assert!(first.text_key("\n"), "Enter submits");
    assert!(!first.vm.keypad_open());
    assert_eq!(first.vm.options()[1], "User name: al");
    first.nav(Nav::Accept);
    assert!(first.vm.keypad_open());
    assert!(first.text_key("\u{1b}"), "Esc goes back");
    assert!(!first.vm.keypad_open());
    assert_eq!(first.vm.step, Step::Relay);
    // Outside a text field those keys are the controller map's.
    assert!(!first.text_key("\n"));
}

#[test]
fn google_drive_in_game_mode_shows_the_code_and_url_and_moves_on_by_itself() {
    let fake = FakeSetup::new(PASSWORD, google_prompt());
    let mut first = first_run_at_the_relay_step(fake.clone());
    first.click(1);
    until(&mut first, "code shown", |f| {
        f.vm.relay.phase == Phase::Waiting
    });
    let title = first.vm.title();
    assert!(title.contains("ABCD-EFGH"), "{title}");
    assert!(title.contains("https://example.test/device"), "{title}");
    assert_eq!(first.vm.options(), ["Cancel"]);
    assert_eq!(first.vm.step, Step::Relay);

    // The fake sign-in finishes on its own: no press.
    fake.release();
    until(&mut first, "signed in", |f| {
        f.vm.step == Step::DeviceAndSteam
    });
    assert!(first.vm.relay_set_up);
    assert_eq!(
        fake.account().unwrap().summary(),
        "Google Drive, name@example.test"
    );
}

#[test]
fn back_cancels_the_sign_in_and_a_late_finish_changes_nothing() {
    let fake = FakeSetup::new(PASSWORD, google_prompt());
    let mut first = first_run_at_the_relay_step(fake.clone());
    first.click(2);
    until(&mut first, "waiting", |f| {
        f.vm.relay.phase == Phase::Waiting
    });
    first.nav(Nav::Back);
    assert_eq!(first.vm.relay.phase, Phase::Choose);
    assert_eq!(first.vm.step, Step::Relay);
    let started = Instant::now();
    while !fake.saw_cancel() {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "cancel never seen"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    fake.release();
    for _ in 0..20 {
        first.pump();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(first.vm.step, Step::Relay);
    assert!(fake.account().is_none(), "nothing was set up");
    // Back once more leaves for the step before; save sync stays off.
    first.nav(Nav::Back);
    assert_eq!(first.vm.step, Step::FindGame);
}

#[test]
fn dropbox_in_game_mode_takes_the_typed_code() {
    let prompt = Prompt {
        url: "https://example.test/dropbox".into(),
        code: None,
        needs_code: true,
        browser: false,
    };
    let fake = FakeSetup::new(PASSWORD, prompt);
    let mut first = first_run_at_the_relay_step(fake.clone());
    first.click(2);
    until(&mut first, "waiting", |f| {
        f.vm.relay.phase == Phase::Waiting
    });
    assert_eq!(first.vm.options(), ["Enter the code", "Cancel"]);
    fake.release();
    first.nav(Nav::Accept);
    pad_type(&mut first, "Zq9");
    press(&mut first, Key::Done);
    until(&mut first, "signed in", |f| {
        f.vm.step == Step::DeviceAndSteam
    });
    assert!(fake
        .calls()
        .contains(&"finish Dropbox typed=true".to_string()));
}

#[test]
fn settings_shows_the_account_signs_in_again_and_turns_off() {
    let fake = FakeSetup::new(PASSWORD, google_prompt());
    fake.setup_nextcloud("https://cloud.test", "alex", PASSWORD)
        .unwrap();
    let mut settings = RelaySettings::new(fake.clone());
    assert_eq!(
        settings.vm.summary(),
        "Save sync: Nextcloud, alex at https://cloud.test"
    );
    assert_eq!(settings.vm.sign_in_label(), "Sign in again");

    // Sign in again: Google Drive in Game Mode, back to Settings with the new account.
    settings.press_sign_in();
    assert!(settings.flow_open());
    assert_eq!(settings.vm.flow.as_ref().unwrap().options()[3], "Cancel");
    settings.click(1);
    let started = Instant::now();
    while settings
        .vm
        .flow
        .as_ref()
        .is_some_and(|f| f.phase != Phase::Waiting)
    {
        assert!(started.elapsed() < Duration::from_secs(20));
        settings.pump();
        std::thread::sleep(Duration::from_millis(2));
    }
    fake.release();
    while settings.flow_open() {
        assert!(started.elapsed() < Duration::from_secs(20));
        settings.pump();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        settings.vm.summary(),
        "Save sync: Google Drive, name@example.test"
    );

    // Turn off asks first, then forgets.
    settings.press_turn_off();
    assert!(settings.vm.confirming_off);
    assert!(settings.vm.account.is_some());
    settings.press_turn_off();
    assert!(settings.vm.account.is_none());
    assert!(fake.account().is_none());
    assert_eq!(
        settings.vm.summary(),
        "Save sync is off. This device plays without syncing."
    );
    assert_eq!(settings.vm.sign_in_label(), "Set up save sync");
}

#[test]
fn cancel_in_settings_changes_nothing() {
    let fake = FakeSetup::new(PASSWORD, google_prompt());
    let mut settings = RelaySettings::new(fake.clone());
    settings.press_sign_in();
    settings.nav(Nav::Down);
    settings.nav(Nav::Down);
    settings.nav(Nav::Down);
    settings.nav(Nav::Accept); // Cancel
    assert!(!settings.flow_open());
    assert!(fake.calls().is_empty());
    assert!(settings.vm.account.is_none());
}

// The real adapter over a fake Nextcloud server.

struct Shared(Arc<MemoryStore>);

impl CredentialStore for Shared {
    fn get(&self, account: &str) -> save_relay::Result<Option<Secret>> {
        self.0.get(account)
    }
    fn set(&self, account: &str, secret: &Secret) -> save_relay::Result<()> {
        self.0.set(account, secret)
    }
    fn delete(&self, account: &str) -> save_relay::Result<()> {
        self.0.delete(account)
    }
    fn kind(&self) -> &'static str {
        "memory"
    }
}

struct Quits(Arc<AtomicBool>);

impl Game for Quits {
    fn run(&self, _env: &[(String, String)]) -> Result<i32> {
        self.0.store(true, Ordering::SeqCst);
        Ok(0)
    }
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Machine {
    root: PathBuf,
    dirs: AppDirs,
    creds: Arc<MemoryStore>,
    dav: Arc<FakeDav>,
}

impl Machine {
    fn new() -> Machine {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("acnh-app-setup-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dirs = AppDirs {
            config: root.join("config"),
            data: root.join("data"),
        };
        fs::create_dir_all(&dirs.config).unwrap();
        fs::create_dir_all(&dirs.data).unwrap();
        let saves = root.join("saves");
        fs::create_dir_all(saves.join("Villager0")).unwrap();
        fs::write(saves.join("main.dat"), "main").unwrap();
        fs::write(saves.join("mainHeader.dat"), "header").unwrap();
        fs::write(saves.join("Villager0/personal.dat"), "villager").unwrap();
        Machine {
            root,
            dirs,
            creds: Arc::new(MemoryStore::new()),
            dav: Arc::new(FakeDav::new(PASSWORD)),
        }
    }

    fn setup(&self) -> Arc<SystemSetup> {
        let (creds, dav) = (self.creds.clone(), self.dav.clone());
        Arc::new(SystemSetup::with(
            self.dirs.clone(),
            Arc::new(move || {
                (
                    Box::new(Shared(creds.clone())) as Box<dyn CredentialStore>,
                    dav.clone() as Arc<dyn Transport>,
                )
            }),
            Arc::new(|| true),
            Arc::new(|_| false),
            "desktop".into(),
        ))
    }

    fn relay(&self) -> SharedRelay {
        let (dirs, creds, dav) = (self.dirs.clone(), self.creds.clone(), self.dav.clone());
        opened_each_step(move || {
            let transport: Arc<dyn Transport> = dav.clone();
            choose(&dirs, &*creds, transport)
        })
    }

    fn text_on_disk(&self) -> String {
        fn walk(dir: &Path, out: &mut String) {
            for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if let Ok(text) = fs::read_to_string(&path) {
                    out.push_str(&text);
                }
            }
        }
        let mut out = String::new();
        walk(&self.dirs.config, &mut out);
        walk(&self.dirs.data, &mut out);
        out
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_wrong_password_to_the_real_adapter_is_a_plain_error_and_nothing_is_stored() {
    let machine = Machine::new();
    let setup = machine.setup();
    let error = setup
        .setup_nextcloud("https://cloud.test", "alex", WRONG)
        .unwrap_err();
    assert_eq!(
        error,
        "The server did not accept that user name and app password."
    );
    assert!(setup.account().is_none());
    assert!(!machine.text_on_disk().contains(WRONG));
    let transport: Arc<dyn Transport> = machine.dav.clone();
    assert!(
        acnh_core::relay::open(&machine.dirs, &*machine.creds, transport)
            .unwrap()
            .is_none()
    );
}

#[test]
fn turn_off_brings_open_back_to_none_and_the_next_play_starts_the_game_without_sync() {
    let machine = Machine::new();
    let setup = machine.setup();
    let account = setup
        .setup_nextcloud("https://cloud.test", "alex", PASSWORD)
        .unwrap();
    assert_eq!(account.summary(), "Nextcloud, alex at https://cloud.test");
    // The password is in the credential store only: no file in either folder holds it.
    assert!(!machine.text_on_disk().contains(PASSWORD));

    let mut settings = RelaySettings::new(setup);
    assert_eq!(
        settings.vm.summary(),
        "Save sync: Nextcloud, alex at https://cloud.test"
    );
    settings.press_turn_off();
    settings.press_turn_off();
    assert!(settings.vm.account.is_none());

    let transport: Arc<dyn Transport> = machine.dav.clone();
    assert!(
        acnh_core::relay::open(&machine.dirs, &*machine.creds, transport)
            .unwrap()
            .is_none()
    );

    // The next Play: sync off, and the game starts with no network at all.
    let before = machine.dav.sent().len();
    let played = Arc::new(AtomicBool::new(false));
    let card = SaveCard {
        saved_unix: 1000,
        device: "desktop".into(),
        play_seconds: 60,
        version: "3.0.2".into(),
        picture: PathBuf::new(),
    };
    let runner = Runner {
        relay: machine.relay(),
        game: Arc::new(Quits(played.clone())),
        save_dir: machine.root.join("saves"),
        env: Vec::new(),
        backup: Arc::new(|_: &Path| Ok(())),
    };
    let mut session = Session::new(
        ContinuePageVm::new(Some(card), None),
        runner,
        PathBuf::new(),
    );
    let started = Instant::now();
    while session.vm().state() == &ContinueState::CheckingStatus {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "still checking"
        );
        session.pump();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(session.vm().state(), &ContinueState::SyncOff);
    assert!(session.nav(Nav::Accept));
    while !played.load(Ordering::SeqCst) {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the game never started"
        );
        session.pump();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        machine.dav.sent().len(),
        before,
        "no network once it is off"
    );
}

#[test]
fn the_service_names_are_the_ones_the_brief_uses() {
    let labels: Vec<_> = Service::ALL.iter().map(|s| s.label()).collect();
    assert_eq!(labels, ["Nextcloud", "Google Drive", "Dropbox"]);
}
