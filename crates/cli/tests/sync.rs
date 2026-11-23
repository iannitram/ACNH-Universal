//! `play` and `sync` against save-relay's in-memory remote and a fake Nextcloud, in process
//! through `acnh_cli::run`: no network, a temporary save-relay config folder, a fake player.
#![cfg(not(any(target_os = "android", target_os = "ios")))]

#[path = "support/fake_dav.rs"]
mod fake_dav;

use acnh_cli::sync::SyncEnv;
use acnh_cli::{run, Context, RelaySource};
use acnh_core::launch::{Game, Relay};
use acnh_core::platform::Os;
use acnh_core::relay::{open, SaveRelay};
use acnh_core::{saves, Result};
use fake_dav::FakeDav;
use save_relay::creds::{self, CredentialStore, MemoryStore};
use save_relay::fake::{FakeDrive, FakeDropbox, REFRESH_TOKEN, SIGN_IN_CODE};
use save_relay::http::Transport;
use save_relay::local::LocalStore;
use save_relay::platform::AppDirs;
use save_relay::remote::MemoryRemote;
use save_relay::service::{self, Service};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const PASSWORD: &str = "app-password-4821";
const SERVER: &str = "https://cloud.test";

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A fake player: it writes `marker` when it starts, so a test can tell it never did.
struct FakePlayer {
    marker: PathBuf,
    env: Mutex<Vec<(String, String)>>,
}

impl Game for FakePlayer {
    fn run(&self, env: &[(String, String)]) -> Result<i32> {
        fs::write(&self.marker, "started").unwrap();
        *self.env.lock().unwrap() = env.to_vec();
        Ok(0)
    }
}

/// One machine: its folders, its credential store, a fake Nextcloud and a fake player.
struct Machine {
    root: PathBuf,
    dirs: AppDirs,
    creds: MemoryStore,
    dav: Arc<FakeDav>,
    player: FakePlayer,
}

impl Machine {
    fn new(name: &str) -> Machine {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("acnh-cli-sync-{name}-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dirs = AppDirs {
            config: root.join("relay-config"),
            data: root.join("relay-data"),
        };
        fs::create_dir_all(&dirs.config).unwrap();
        fs::create_dir_all(&dirs.data).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        let player = FakePlayer {
            marker: root.join("player-started"),
            env: Mutex::new(Vec::new()),
        };
        Machine {
            root,
            dirs,
            creds: MemoryStore::new(),
            dav: Arc::new(FakeDav::new(PASSWORD)),
            player,
        }
    }

    fn data_root(&self) -> PathBuf {
        self.root
            .join("home/.local/share/acnh-universal/install/root")
    }

    fn save_dir(&self) -> PathBuf {
        saves::save_dir(&self.data_root())
    }

    fn started(&self) -> bool {
        self.player.marker.exists()
    }

    /// Run `args` the way the binary would, with `stdin`, against this machine.
    fn exec(&self, args: &[&str], stdin: &str, relay: RelaySource<'_>) -> (i32, String, String) {
        let home = self.root.join("home").display().to_string();
        let var = move |name: &str| (name == "HOME").then(|| home.clone());
        let transport: Arc<dyn Transport> = self.dav.clone();
        let no_browser = |_: &str, _: &mut dyn std::io::Write| {
            Err(acnh_core::Error::Saves("no browser in this test".into()))
        };
        let mut input = Cursor::new(stdin.as_bytes().to_vec());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut cx = Context {
            os: Os::Linux,
            var: &var,
            game: Some(&self.player),
            relay,
            stdin: &mut input,
            out: &mut out,
            err: &mut err,
            sync: SyncEnv {
                dirs: Ok(self.dirs.clone()),
                creds: &self.creds,
                transport,
                redirect: &no_browser,
            },
        };
        let code = run(args.iter().map(|arg| arg.to_string()).collect(), &mut cx);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn setup(&self) {
        let (code, out, err) = self.exec(
            &["sync", "setup", "--server", SERVER, "--user", "alex"],
            &format!("{PASSWORD}\n"),
            RelaySource::Open,
        );
        assert_eq!(code, 0, "{out}{err}");
    }

    fn open_is_some(&self) -> bool {
        let transport: Arc<dyn Transport> = self.dav.clone();
        open(&self.dirs, &self.creds, transport).unwrap().is_some()
    }

    /// A relay for the island on this machine, over `remote`.
    fn relay_over(&self, remote: &Arc<MemoryRemote>, device: &str, clock: u64) -> SaveRelay {
        let dirs = AppDirs {
            config: self.root.join(format!("{device}-config")),
            data: self.root.join(format!("{device}-data")),
        };
        fs::create_dir_all(&dirs.config).unwrap();
        fs::create_dir_all(&dirs.data).unwrap();
        SaveRelay::new(remote.clone(), LocalStore::new(dirs), device).with_clock(move || clock)
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn island(dir: &Path, marker: &str) {
    fs::create_dir_all(dir.join("Villager0")).unwrap();
    fs::write(dir.join("main.dat"), format!("main island data: {marker}")).unwrap();
    fs::write(dir.join("mainHeader.dat"), format!("mainHeader: {marker}")).unwrap();
    fs::write(
        dir.join("Villager0/personal.dat"),
        format!("villager: {marker}"),
    )
    .unwrap();
}

fn every_file_text(dir: &Path) -> String {
    let mut text = String::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in fs::read_dir(&next).into_iter().flatten().flatten() {
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

#[test]
fn play_with_no_setup_and_after_sync_off_plays_and_says_sync_is_off() {
    let machine = Machine::new("off");
    island(&machine.save_dir(), "here");

    let (code, out, err) = machine.exec(&["play"], "", RelaySource::Open);
    assert_eq!(code, 0, "{out}{err}");
    assert!(machine.started());
    assert!(out.contains("save sync is off"), "{out}");
    assert!(machine.dav.sent().is_empty(), "no network");
    assert_eq!(
        machine.player.env.lock().unwrap()[0].0,
        "NX_DATA_ROOT".to_string()
    );

    // Set up, then off again: the same.
    fs::remove_file(&machine.player.marker).unwrap();
    machine.setup();
    assert!(machine.open_is_some());
    let (code, out, _) = machine.exec(&["sync", "off"], "", RelaySource::Open);
    assert_eq!(code, 0, "{out}");
    assert!(
        !machine.open_is_some(),
        "sync off makes open Ok(None) again"
    );
    let (code, out, err) = machine.exec(&["play"], "", RelaySource::Open);
    assert_eq!(code, 0, "{out}{err}");
    assert!(machine.started());
    assert!(out.contains("save sync is off"), "{out}");
}

/// Device A pushed, this machine pulled, A pushed again, and this machine changed too.
fn conflict(machine: &Machine, remote: &Arc<MemoryRemote>) -> SaveRelay {
    let a = machine.relay_over(remote, "device-a", 1000);
    let a_saves = machine.root.join("a-saves");
    island(&a_saves, "initial");
    a.push(&a_saves, None).unwrap();
    let here = machine.relay_over(remote, "device-b", 1010);
    here.pull(&machine.save_dir(), None).unwrap();
    fs::write(a_saves.join("main.dat"), "main island data: from-a").unwrap();
    a.push(&a_saves, None).unwrap();
    fs::write(
        machine.save_dir().join("main.dat"),
        "main island data: from-b",
    )
    .unwrap();
    here
}

#[test]
fn a_conflict_exits_3_prints_both_cards_and_never_starts_the_game() {
    let machine = Machine::new("conflict");
    let remote = Arc::new(MemoryRemote::new());
    let relay = conflict(&machine, &remote);

    let (code, out, err) = machine.exec(&["play"], "", RelaySource::Fixed(&relay));
    assert_eq!(code, 3, "{out}{err}");
    assert!(!machine.started());
    assert!(out.contains("relay: "), "{out}");
    assert!(out.contains("here: "), "{out}");
    assert!(out.contains("device-a"), "{out}");
    assert!(out.contains("--keep"), "{out}");
    // Nothing was answered for the player.
    assert_eq!(
        fs::read_to_string(machine.save_dir().join("main.dat")).unwrap(),
        "main island data: from-b"
    );

    let (code, out, _) = machine.exec(&["--json", "play"], "", RelaySource::Fixed(&relay));
    assert_eq!(code, 3);
    assert_eq!(out.trim(), "{\"code\":\"saves\"}");
}

#[test]
fn keep_relay_backs_the_island_here_up_first_and_then_plays() {
    let machine = Machine::new("keep-relay");
    let remote = Arc::new(MemoryRemote::new());
    let relay = conflict(&machine, &remote);

    let (code, out, err) =
        machine.exec(&["play", "--keep", "relay"], "", RelaySource::Fixed(&relay));
    assert_eq!(code, 0, "{out}{err}");
    assert!(machine.started());
    let backups = saves::backups_dir(&machine.data_root());
    let stamps: Vec<_> = fs::read_dir(&backups).unwrap().flatten().collect();
    assert_eq!(stamps.len(), 1, "one backup");
    assert_eq!(
        fs::read_to_string(stamps[0].path().join("main.dat")).unwrap(),
        "main island data: from-b"
    );
    assert_eq!(
        fs::read_to_string(machine.save_dir().join("main.dat")).unwrap(),
        "main island data: from-a"
    );
}

#[test]
fn keep_here_plays_with_the_island_here() {
    let machine = Machine::new("keep-here");
    let remote = Arc::new(MemoryRemote::new());
    let relay = conflict(&machine, &remote);

    let (code, out, err) = machine.exec(&["play", "--keep=here"], "", RelaySource::Fixed(&relay));
    assert_eq!(code, 0, "{out}{err}");
    assert!(machine.started());
    assert_eq!(
        fs::read_to_string(machine.save_dir().join("main.dat")).unwrap(),
        "main island data: from-b"
    );
    let (code, _, _) = machine.exec(&["play", "--keep", "nowhere"], "", RelaySource::Open);
    assert_eq!(code, 2, "an unknown --keep is a usage error");
}

#[test]
fn a_held_island_exits_4_and_anyway_plays() {
    let machine = Machine::new("held");
    let remote = Arc::new(MemoryRemote::new());
    let a = machine.relay_over(&remote, "device-a", 1000);
    let a_saves = machine.root.join("a-saves");
    island(&a_saves, "initial");
    a.push(&a_saves, None).unwrap();
    let here = machine.relay_over(&remote, "device-b", 1010);
    here.pull(&machine.save_dir(), None).unwrap();
    fs::write(machine.save_dir().join("main.dat"), "changed here").unwrap();
    here.push(&machine.save_dir(), None).unwrap();
    // Device A takes the island: it is open there.
    a.pull(&a_saves, None).unwrap();

    let (code, out, err) = machine.exec(&["play"], "", RelaySource::Fixed(&here));
    assert_eq!(code, 4, "{out}{err}");
    assert!(!machine.started());
    assert!(err.contains("device-a"), "{err}");

    let (code, out, err) = machine.exec(&["play", "--anyway"], "", RelaySource::Fixed(&here));
    assert_eq!(code, 0, "{out}{err}");
    assert!(machine.started());
}

#[test]
fn a_relay_that_cannot_be_reached_exits_5_and_never_starts_the_game() {
    let machine = Machine::new("unreachable");
    island(&machine.save_dir(), "here");
    machine.setup();
    machine.dav.set_online(false);

    let (code, out, err) = machine.exec(&["play"], "", RelaySource::Open);
    assert_eq!(code, 5, "{out}{err}");
    assert!(!machine.started());
    assert!(err.contains("could not be reached"), "{err}");

    let (code, out, _) = machine.exec(&["--json", "play"], "", RelaySource::Open);
    assert_eq!(code, 5);
    assert_eq!(out.trim(), "{\"code\":\"relay_unreachable\"}");
    // No flag plays anyway.
    let (code, _, _) = machine.exec(&["play", "--anyway"], "", RelaySource::Open);
    assert_eq!(code, 5);
    assert!(!machine.started());

    // The same through an adapter over an offline memory remote.
    let remote = Arc::new(MemoryRemote::new());
    let relay = machine.relay_over(&remote, "device-b", 1010);
    remote.set_reachable(false);
    let (code, _, _) = machine.exec(&["play"], "", RelaySource::Fixed(&relay));
    assert_eq!(code, 5);
    assert!(!machine.started());
}

#[test]
fn setup_files_the_password_in_the_credential_store_only() {
    let machine = Machine::new("password");
    machine.setup();

    let account = creds::account(SERVER, "alex").unwrap();
    assert_eq!(
        machine.creds.get(&account).unwrap().unwrap().expose(),
        PASSWORD
    );
    let files = every_file_text(&machine.root);
    assert!(!files.contains(PASSWORD), "the password reached a file");
    assert!(machine.open_is_some());

    let (code, out, err) = machine.exec(&["sync", "status"], "", RelaySource::Open);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("save sync is on"), "{out}");
    assert!(out.contains("alex"), "{out}");
    assert!(!format!("{out}{err}").contains(PASSWORD));

    // Passing it as an argument is not a way in; a wrong one files nothing.
    let other = Machine::new("password-wrong");
    let (code, out, err) = other.exec(
        &["sync", "setup", "--server", SERVER, "--user", "alex"],
        "not-the-password\n",
        RelaySource::Open,
    );
    assert_ne!(code, 0);
    assert!(!format!("{out}{err}").contains("not-the-password"));
    assert!(!other.open_is_some());
    assert!(!every_file_text(&other.root).contains("not-the-password"));
    let (code, _, err) = other.exec(
        &["sync", "setup", "--server", SERVER, "--user", "alex"],
        "",
        RelaySource::Open,
    );
    assert_eq!(code, 1);
    assert!(err.contains("standard input"), "{err}");

    let (code, out, _) = machine.exec(&["sync", "status"], "", RelaySource::Open);
    assert_eq!(code, 0);
    assert!(out.contains("save sync is on"));
    let (code, out, _) = machine.exec(&["sync", "off"], "", RelaySource::Open);
    assert_eq!(code, 0, "{out}");
    assert!(machine.creds.get(&account).unwrap().is_none());
    let (_, out, _) = machine.exec(&["sync", "status"], "", RelaySource::Open);
    assert!(out.contains("save sync is off"), "{out}");
}

fn query_param(url: &str, name: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| value.to_string())
    })
}

#[test]
#[allow(clippy::arc_with_non_send_sync)]
fn sign_in_prints_the_code_in_game_mode_and_follows_the_browser_on_a_desktop() {
    let machine = Machine::new("sign-in");
    let home = machine.root.join("home").display().to_string();
    let var = move |name: &str| (name == "HOME").then(|| home.clone());

    let run_with = |transport: Arc<dyn Transport>,
                    redirect: &dyn Fn(&str, &mut dyn std::io::Write) -> Result<String>,
                    args: &[&str],
                    stdin: &str| {
        let mut input = Cursor::new(stdin.as_bytes().to_vec());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut cx = Context {
            os: Os::Linux,
            var: &var,
            game: Some(&machine.player),
            relay: RelaySource::Open,
            stdin: &mut input,
            out: &mut out,
            err: &mut err,
            sync: SyncEnv {
                dirs: Ok(machine.dirs.clone()),
                creds: &machine.creds,
                transport,
                redirect,
            },
        };
        let code = run(args.iter().map(|arg| arg.to_string()).collect(), &mut cx);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    };
    let never = |_: &str, _: &mut dyn std::io::Write| -> Result<String> {
        panic!("Game Mode has no browser to wait for")
    };

    std::env::set_var("XDG_CURRENT_DESKTOP", "gamescope");
    let (code, out, err) = run_with(
        Arc::new(FakeDrive::new()),
        &never,
        &["sync", "sign-in", "gdrive"],
        "",
    );
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("google.com/device"), "{out}");
    assert!(out.contains("code: "), "{out}");
    let google = service::token_account(Service::Gdrive, "fake@example.com").unwrap();
    assert_eq!(
        machine.creds.get(&google).unwrap().unwrap().expose(),
        REFRESH_TOKEN
    );
    assert!(LocalStore::new(machine.dirs.clone())
        .load_config()
        .unwrap()
        .is_some());

    std::env::remove_var("XDG_CURRENT_DESKTOP");
    std::env::remove_var("SteamGamepadUI");
    let browser = |url: &str, _: &mut dyn std::io::Write| -> Result<String> {
        let state = query_param(url, "state").unwrap();
        Ok(format!("/?code={SIGN_IN_CODE}&state={state}"))
    };
    let (code, out, err) = run_with(
        Arc::new(FakeDropbox::new()),
        &browser,
        &["sync", "sign-in", "dropbox"],
        "",
    );
    assert_eq!(code, 0, "{out}{err}");
    let dropbox = service::token_account(Service::Dropbox, "fake@example.com").unwrap();
    assert_eq!(
        machine.creds.get(&dropbox).unwrap().unwrap().expose(),
        REFRESH_TOKEN
    );

    let (code, _, _) = run_with(
        Arc::new(FakeDropbox::new()),
        &browser,
        &["sync", "sign-in", "icloud"],
        "",
    );
    assert_eq!(code, 1);
}
