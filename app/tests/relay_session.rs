//! The Continue page's session with the real adapter, over a fake Nextcloud: no setup gives
//! `SyncOff`, a set-up machine gives the adapter, a server that does not answer gives
//! `RelayUnavailable` and an error opening gives `RelayError`. No network, temporary folders.
#![cfg(not(any(target_os = "android", target_os = "ios")))]

#[path = "../../crates/cli/tests/support/fake_dav.rs"]
mod fake_dav;

use acnh_app::play::{Runner, Session};
use acnh_app::relay::{choose, opened_each_step, RelayFailed, SharedRelay};
use acnh_app::vm::continue_page::ContinueState;
use acnh_app::vm::{ContinuePageVm, Nav};
use acnh_core::launch::Game;
use acnh_core::relay::setup_nextcloud;
use acnh_core::savecard::SaveCard;
use acnh_core::{Error, Result};
use fake_dav::FakeDav;
use save_relay::creds::MemoryStore;
use save_relay::http::Transport;
use save_relay::platform::AppDirs;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Quits(Arc<AtomicBool>);

impl Game for Quits {
    fn run(&self, _env: &[(String, String)]) -> Result<i32> {
        self.0.store(true, Ordering::SeqCst);
        Ok(0)
    }
}

struct Folders {
    root: PathBuf,
    dirs: AppDirs,
    creds: Arc<MemoryStore>,
    dav: Arc<FakeDav>,
    played: Arc<AtomicBool>,
}

impl Folders {
    fn new(name: &str) -> Folders {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("acnh-app-relay-{name}-{}-{id}", std::process::id()));
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
        Folders {
            root,
            dirs,
            creds: Arc::new(MemoryStore::new()),
            dav: Arc::new(FakeDav::new("app-password")),
            played: Arc::new(AtomicBool::new(false)),
        }
    }

    fn set_up(&self) {
        let transport: Arc<dyn Transport> = self.dav.clone();
        setup_nextcloud(
            "https://cloud.test",
            "alex",
            "app-password",
            "desktop",
            &self.dirs,
            &*self.creds,
            transport,
        )
        .unwrap();
    }

    /// The relay `session()` in `main.rs` builds, over these folders and this fake server.
    fn relay(&self) -> SharedRelay {
        let (dirs, creds, dav) = (self.dirs.clone(), self.creds.clone(), self.dav.clone());
        opened_each_step(move || {
            let transport: Arc<dyn Transport> = dav.clone();
            choose(&dirs, &*creds, transport)
        })
    }

    fn session(&self, relay: SharedRelay) -> Session {
        let card = SaveCard {
            saved_unix: 1000,
            device: "desktop".into(),
            play_seconds: 60,
            version: "3.0.2".into(),
            picture: PathBuf::new(),
        };
        let runner = Runner {
            relay,
            game: Arc::new(Quits(self.played.clone())),
            save_dir: self.root.join("saves"),
            env: Vec::new(),
            backup: Arc::new(|_: &Path| Ok(())),
        };
        Session::new(
            ContinuePageVm::new(Some(card), None),
            runner,
            PathBuf::new(),
        )
    }
}

impl Drop for Folders {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Pump, as the window's timer does, until the page leaves its first status check.
fn settled(session: &mut Session) -> ContinueState {
    let started = Instant::now();
    while session.vm().state() == &ContinueState::CheckingStatus {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "still checking"
        );
        session.pump();
        std::thread::sleep(Duration::from_millis(2));
    }
    session.vm().state().clone()
}

#[test]
fn with_no_setup_the_page_says_save_sync_is_off_and_play_still_goes() {
    let folders = Folders::new("off");
    let mut session = folders.session(folders.relay());
    assert_eq!(settled(&mut session), ContinueState::SyncOff);

    assert!(session.nav(Nav::Accept));
    let started = Instant::now();
    while !folders.played.load(Ordering::SeqCst) {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the game never started"
        );
        session.pump();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(folders.dav.sent().is_empty(), "no network without a setup");
}

#[test]
fn with_a_fake_setup_the_adapter_is_used() {
    let folders = Folders::new("on");
    folders.set_up();
    let mut session = folders.session(folders.relay());
    let state = settled(&mut session);
    assert!(
        matches!(state, ContinueState::Ready | ContinueState::LocalNewer),
        "{state:?}"
    );

    // Play goes through the adapter: the island reaches the fake server.
    assert!(session.nav(Nav::Accept));
    let started = Instant::now();
    while folders.dav.file_paths().is_empty() {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{:?}",
            session.vm().state()
        );
        session.pump();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!folders.dav.sent().is_empty());
}

#[test]
fn a_server_that_does_not_answer_is_relay_unavailable_not_sync_off() {
    let folders = Folders::new("down");
    folders.set_up();
    folders.dav.set_online(false);
    let mut session = folders.session(folders.relay());
    assert_eq!(settled(&mut session), ContinueState::RelayUnavailable);
}

#[test]
fn an_error_opening_save_sync_is_a_relay_error_with_the_reason() {
    let folders = Folders::new("broken");
    let relay: SharedRelay = Arc::new(RelayFailed(Error::Saves(
        "the credential store is locked".into(),
    )));
    let mut session = folders.session(relay);
    match settled(&mut session) {
        ContinueState::RelayError(reason) => {
            assert!(reason.contains("credential store is locked"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
}
