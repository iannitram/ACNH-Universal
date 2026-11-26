//! Play refuses to run on an older island (2.06a, #168), tested end to end: the Continue page's
//! session over the real save-relay adapter, on save-relay's `MemoryRemote`. Switching the
//! remote off is the transport going away (`set_reachable(false)`). The fake player writes a
//! marker file when it starts, so "the game must not start" is an absent file. No network and
//! no window: temporary folders only.
#![cfg(not(any(target_os = "android", target_os = "ios")))]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use acnh_app::play::{RelayOff, Runner, Session};
use acnh_app::relay::{opened_each_step, SharedRelay};
use acnh_app::vm::continue_page::{ConflictPhase, ContinueState, FocusTarget};
use acnh_app::vm::{ContinuePageVm, Nav};
use acnh_core::launch::{Game, Relay, RelayStatus};
use acnh_core::relay::SaveRelay;
use acnh_core::savecard::SaveCard;
use acnh_core::Result;
use save_relay::local::LocalStore;
use save_relay::platform::AppDirs;
use save_relay::remote::{MemoryRemote, Remote};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// The island both devices start from (the same bytes, so the second device adopts it).
const ISLAND: [(&str, &str); 3] = [
    ("main.dat", "main"),
    ("mainHeader.dat", "header"),
    ("Villager0/personal.dat", "villager"),
];

/// Everything the devices of one test share: the relay and the folders to remove afterwards.
struct Fixture {
    root: PathBuf,
    remote: Arc<MemoryRemote>,
}

impl Fixture {
    fn new(test: &str) -> Fixture {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "acnh-no-offline-{test}-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Fixture {
            root,
            remote: Arc::new(MemoryRemote::new()),
        }
    }

    fn device(&self, name: &'static str) -> Device {
        let base = self.root.join(name);
        let dirs = AppDirs {
            config: base.join("config"),
            data: base.join("data"),
        };
        fs::create_dir_all(&dirs.config).unwrap();
        fs::create_dir_all(&dirs.data).unwrap();
        let saves = base.join("saves");
        for (file, text) in ISLAND {
            let path = saves.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        Device {
            name,
            saves,
            marker: base.join("game-started"),
            dirs,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// One device: its save folder, its save-relay folders and the marker its player writes.
struct Device {
    name: &'static str,
    saves: PathBuf,
    marker: PathBuf,
    dirs: AppDirs,
}

impl Device {
    /// save-relay as this device runs it: a fresh adapter per step over the shared remote.
    fn relay(&self, remote: &Arc<MemoryRemote>) -> SharedRelay {
        let remote = Arc::clone(remote);
        let dirs = self.dirs.clone();
        let name = self.name;
        opened_each_step(move || {
            let remote: Arc<dyn Remote> = remote.clone();
            let relay: Box<dyn Relay> =
                Box::new(SaveRelay::new(remote, LocalStore::new(dirs.clone()), name));
            relay
        })
    }

    fn island(&self) -> String {
        fs::read_to_string(self.saves.join("main.dat")).unwrap()
    }

    fn started(&self) -> bool {
        self.marker.exists()
    }

    fn forget_start(&self) {
        let _ = fs::remove_file(&self.marker);
    }

    /// The game a play on this device runs: it writes its marker, may change the island, and
    /// may cut the relay off as it quits.
    fn game(
        &self,
        remote: &Arc<MemoryRemote>,
        change: Option<&'static str>,
        off_at_quit: bool,
    ) -> Arc<dyn Game + Send + Sync> {
        Arc::new(Player {
            marker: self.marker.clone(),
            saves: self.saves.clone(),
            remote: Arc::clone(remote),
            change,
            off_at_quit,
        })
    }

    /// The page for this device, opened (its status check starts at once).
    fn open(&self, remote: &Arc<MemoryRemote>, game: Arc<dyn Game + Send + Sync>) -> Session {
        self.open_with(self.relay(remote), game)
    }

    fn open_with(&self, relay: SharedRelay, game: Arc<dyn Game + Send + Sync>) -> Session {
        let card = SaveCard {
            saved_unix: 1000,
            device: self.name.into(),
            play_seconds: 60,
            version: "3.0.2".into(),
            picture: PathBuf::new(),
        };
        let runner = Runner {
            relay,
            game,
            save_dir: self.saves.clone(),
            env: Vec::new(),
            backup: Arc::new(|_: &Path| Ok(())),
        };
        Session::new(
            ContinuePageVm::new(Some(card), None),
            runner,
            PathBuf::new(),
        )
    }

    /// Write this device's island as it would be after a game, then push it directly.
    fn change_and_push(&self, remote: &Arc<MemoryRemote>, text: &str) {
        fs::write(self.saves.join("main.dat"), text).unwrap();
        let relay = self.relay(remote);
        relay.push(&self.saves, None).unwrap();
    }

    /// Pull this device's island directly, as a Play's pull would.
    fn pull(&self, remote: &Arc<MemoryRemote>) {
        let relay = self.relay(remote);
        relay.pull(&self.saves, None).unwrap();
    }
}

struct Player {
    marker: PathBuf,
    saves: PathBuf,
    remote: Arc<MemoryRemote>,
    change: Option<&'static str>,
    off_at_quit: bool,
}

impl Game for Player {
    fn run(&self, _env: &[(String, String)]) -> Result<i32> {
        fs::write(&self.marker, "started").unwrap();
        if let Some(text) = self.change {
            fs::write(self.saves.join("main.dat"), text).unwrap();
        }
        if self.off_at_quit {
            self.remote.set_reachable(false);
        }
        Ok(0)
    }
}

fn busy(state: &ContinueState) -> bool {
    matches!(
        state,
        ContinueState::CheckingStatus
            | ContinueState::Syncing(_)
            | ContinueState::Playing
            | ContinueState::Saving
    )
}

/// Pump, as the window's timer does, until nothing runs. Returns the states on the way.
fn settle(session: &mut Session) -> Vec<ContinueState> {
    let mut seen = vec![session.vm().state().clone()];
    let started = Instant::now();
    while busy(session.vm().state()) {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "still {:?}",
            session.vm().state()
        );
        session.pump();
        let state = session.vm().state().clone();
        if seen.last() != Some(&state) {
            seen.push(state);
        }
        thread::sleep(Duration::from_millis(2));
    }
    seen
}

/// A first play with the relay up: the device adopts the island and the page is Ready. A launcher
/// open only reads the status, so the last sync is recorded by a play (its pull and push).
fn first_open(dev: &Device, fx: &Fixture) {
    let mut session = dev.open(&fx.remote, dev.game(&fx.remote, None, false));
    assert_eq!(settle(&mut session).last(), Some(&ContinueState::Ready));
    assert!(session.nav(Nav::Accept));
    assert_eq!(settle(&mut session).last(), Some(&ContinueState::Ready));
    dev.forget_start();
}

/// The game runs on a device (already set up by `first_open`) whose relay is cut off as it
/// quits: the push waits. Returns the page, in `PushFailed`, with the relay still off.
fn leave_waiting_push(dev: &Device, fx: &Fixture) -> Session {
    let mut session = dev.open(&fx.remote, dev.game(&fx.remote, Some("after game"), true));
    assert_eq!(settle(&mut session).last(), Some(&ContinueState::Ready));
    dev.forget_start();
    assert!(session.nav(Nav::Accept));
    settle(&mut session);
    assert!(
        matches!(session.vm().state(), ContinueState::PushFailed(_)),
        "{:?}",
        session.vm().state()
    );
    assert!(dev.started(), "the game ran");
    session
}

#[test]
fn a_game_whose_push_finds_the_transport_off_leaves_a_waiting_push() {
    let fx = Fixture::new("waiting");
    let dev = fx.device("amd-handheld");
    first_open(&dev, &fx);
    let session = leave_waiting_push(&dev, &fx);
    assert_eq!(session.vm().view().status, "Not synced yet");
    assert_eq!(dev.island(), "after game", "the island is safe here");
}

#[test]
fn at_the_next_open_with_the_relay_unreachable_retry_never_starts_the_game() {
    let fx = Fixture::new("unreachable-open");
    let dev = fx.device("amd-handheld");
    first_open(&dev, &fx);
    drop(leave_waiting_push(&dev, &fx));
    dev.forget_start();

    let mut session = dev.open(&fx.remote, dev.game(&fx.remote, None, false));
    assert_eq!(
        settle(&mut session).last(),
        Some(&ContinueState::RelayUnavailable)
    );
    let view = session.vm().view();
    assert_eq!(view.status, "Not synced yet");
    assert_eq!(view.button, "Retry");
    assert!(
        view.message.contains("could not be reached"),
        "{}",
        view.message
    );
    assert!(!dev.started(), "opening never starts the game");

    assert!(session.nav(Nav::Accept), "Retry is pressed");
    assert_eq!(
        settle(&mut session).last(),
        Some(&ContinueState::RelayUnavailable)
    );
    assert!(!dev.started(), "Retry never starts the game");
}

#[test]
fn at_the_next_open_with_the_relay_back_the_waiting_push_runs_first_and_the_other_device_gets_it() {
    let fx = Fixture::new("back");
    let island = fx.device("amd-handheld");
    let other = fx.device("strix-tablet");
    // Both devices hold the same island on the relay before the game changes it here.
    first_open(&island, &fx);
    first_open(&other, &fx);
    drop(leave_waiting_push(&island, &fx));

    fx.remote.set_reachable(true);
    island.forget_start();
    let mut session = island.open(&fx.remote, island.game(&fx.remote, None, false));
    let states = settle(&mut session);
    let pushing = states
        .iter()
        .position(|state| {
            state == &ContinueState::Syncing(acnh_app::vm::continue_page::SyncStep::Push)
        })
        .unwrap_or_else(|| panic!("no waiting push was sent: {states:?}"));
    let ready = states
        .iter()
        .position(|state| state == &ContinueState::Ready);
    assert!(
        ready.is_some_and(|at| pushing < at),
        "Play is enabled only after the push: {states:?}"
    );
    assert!(!island.started(), "the push never starts the game");
    assert!(
        session.vm().view().button_enabled,
        "Play is enabled after the push"
    );

    // The other device's pull now gets the island the game left.
    other.pull(&fx.remote);
    assert_eq!(other.island(), "after game");
}

#[test]
fn with_the_transport_off_after_open_play_s_pull_cannot_reach_the_relay_and_the_game_does_not_start(
) {
    let fx = Fixture::new("off-after-open");
    let dev = fx.device("amd-handheld");
    first_open(&dev, &fx);

    let mut session = dev.open(&fx.remote, dev.game(&fx.remote, None, false));
    assert_eq!(settle(&mut session).last(), Some(&ContinueState::Ready));
    fx.remote.set_reachable(false);
    dev.forget_start();

    assert!(session.nav(Nav::Accept), "Play is pressed");
    assert_eq!(
        settle(&mut session).last(),
        Some(&ContinueState::RelayUnavailable)
    );
    assert!(!dev.started(), "Play's own pull could not reach the relay");
    assert_eq!(session.vm().view().status, "Not synced yet");
}

#[test]
fn with_save_sync_off_play_starts_the_game_without_sync() {
    let fx = Fixture::new("sync-off");
    let dev = fx.device("amd-handheld");
    let mut session = dev.open_with(Arc::new(RelayOff), dev.game(&fx.remote, None, false));
    assert_eq!(settle(&mut session).last(), Some(&ContinueState::SyncOff));
    dev.forget_start();

    assert!(session.nav(Nav::Accept));
    assert_eq!(settle(&mut session).last(), Some(&ContinueState::SyncOff));
    assert!(dev.started(), "sync off still plays");
    assert!(fx.remote.file_paths().is_empty(), "no sync without a setup");
}

#[test]
fn sync_now_sends_the_waiting_push_and_the_other_device_gets_it() {
    let fx = Fixture::new("sync-now-push");
    let island = fx.device("amd-handheld");
    let other = fx.device("strix-tablet");
    first_open(&island, &fx);
    first_open(&other, &fx);
    let mut session = leave_waiting_push(&island, &fx);

    fx.remote.set_reachable(true);
    session.click(FocusTarget::Saves);
    assert!(session.vm().saves_open(), "the Saves page opens");
    assert!(session.vm().view().sync_now, "Sync now is offered");
    assert!(session.sync_now());
    assert_eq!(settle(&mut session).last(), Some(&ContinueState::Ready));
    assert!(
        session.vm().saves_open(),
        "a clean sync keeps the Saves page"
    );

    other.pull(&fx.remote);
    assert_eq!(other.island(), "after game");
}

#[test]
fn sync_now_pulls_the_newer_island_once() {
    let fx = Fixture::new("sync-now-pull");
    let here = fx.device("amd-handheld");
    let other = fx.device("strix-tablet");
    first_open(&here, &fx);
    first_open(&other, &fx);

    other.change_and_push(&fx.remote, "from the tablet");
    let mut session = here.open(&fx.remote, here.game(&fx.remote, None, false));
    assert_eq!(
        settle(&mut session).last(),
        Some(&ContinueState::RelayNewer)
    );

    session.click(FocusTarget::Saves);
    assert!(session.sync_now());
    assert_eq!(settle(&mut session).last(), Some(&ContinueState::Ready));
    assert_eq!(here.island(), "from the tablet");
    // The card the page shows is the pulled island's, read again after the pull.
    assert_eq!(
        session.vm().local_card().map(|card| card.device.as_str()),
        Some("strix-tablet")
    );
    // The pull marked this device as playing; the exit push cleared it, so the other device
    // sees the island as ready, not held.
    assert_eq!(
        other.relay(&fx.remote).status(&other.saves),
        Ok(RelayStatus::Ready)
    );
}

#[test]
fn sync_now_with_both_sides_changed_opens_the_chooser() {
    let fx = Fixture::new("sync-now-conflict");
    let island = fx.device("amd-handheld");
    let other = fx.device("strix-tablet");
    first_open(&island, &fx);
    first_open(&other, &fx);
    let mut session = leave_waiting_push(&island, &fx);

    fx.remote.set_reachable(true);
    other.change_and_push(&fx.remote, "from the tablet");

    session.click(FocusTarget::Saves);
    assert!(session.sync_now());
    assert_eq!(settle(&mut session).last(), Some(&ContinueState::Conflict));
    let conflict = session.vm().conflict().expect("the chooser has both cards");
    assert_eq!(conflict.phase, ConflictPhase::Pending);
    assert!(
        !session.vm().saves_open(),
        "the chooser is on the Continue page"
    );
    assert_eq!(island.island(), "after game", "nothing was overwritten");
}

#[test]
fn sync_now_does_nothing_while_save_sync_is_off() {
    let fx = Fixture::new("sync-now-off");
    let dev = fx.device("amd-handheld");
    let mut session = dev.open_with(Arc::new(RelayOff), dev.game(&fx.remote, None, false));
    assert_eq!(settle(&mut session).last(), Some(&ContinueState::SyncOff));
    session.click(FocusTarget::Saves);
    assert!(!session.vm().view().sync_now);
    assert!(!session.sync_now());
    assert_eq!(session.vm().state(), &ContinueState::SyncOff);
}

#[test]
fn saves_relay_line_is_the_one_the_state_asks_for() {
    // Each state the Saves page shows, from the page's own status path.
    let fx = Fixture::new("saves-line");
    let dev = fx.device("amd-handheld");
    first_open(&dev, &fx);
    let session = dev.open(&fx.remote, dev.game(&fx.remote, None, false));
    let mut session = session;
    settle(&mut session);
    assert!(
        session
            .vm()
            .view()
            .saves_relay
            .starts_with("Last sync: amd-handheld"),
        "{}",
        session.vm().view().saves_relay
    );
}

#[test]
fn a_relay_error_names_the_reason_on_the_saves_page() {
    let fx = Fixture::new("saves-error");
    let dev = fx.device("amd-handheld");
    let relay: SharedRelay = Arc::new(acnh_app::relay::RelayFailed(acnh_core::Error::Saves(
        "the credential store is locked".into(),
    )));
    let mut session = dev.open_with(relay, dev.game(&fx.remote, None, false));
    settle(&mut session);
    assert!(matches!(session.vm().state(), ContinueState::RelayError(_)));
    assert!(session
        .vm()
        .view()
        .saves_relay
        .contains("credential store is locked"));
}
