#![cfg(feature = "relay")]

use acnh_core::launch::{Keep, Relay, Sync};
use acnh_core::relay::{map_sync, SaveRelay};
use save_relay::api::{Action, Held, Side, SyncData, SyncReport};
use save_relay::local::LocalStore;
use save_relay::platform::AppDirs;
use save_relay::remote::MemoryRemote;
use save_relay::transfer::Counts;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

fn temp_fixture_dir(name: &str) -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "acnh-core-relay-test-{}-{}-{}",
        std::process::id(),
        name,
        id
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn create_store(root: &std::path::Path) -> LocalStore {
    let config = root.join("config");
    let data = root.join("data");
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&data).unwrap();
    LocalStore::new(AppDirs { config, data })
}

fn populate_island(save_dir: &std::path::Path, marker: &str) {
    fs::create_dir_all(save_dir.join("Villager0")).unwrap();
    fs::write(
        save_dir.join("main.dat"),
        format!("main island data: {marker}"),
    )
    .unwrap();
    fs::write(
        save_dir.join("mainHeader.dat"),
        format!("mainHeader: {marker}"),
    )
    .unwrap();
    fs::write(
        save_dir.join("Villager0/personal.dat"),
        format!("villager: {marker}"),
    )
    .unwrap();
}

#[test]
fn push_and_pull_between_devices() {
    let root = temp_fixture_dir("push-pull");
    let remote = Arc::new(MemoryRemote::new());

    // Device A (desktop) creates an island and pushes it
    let desktop_dir = root.join("desktop");
    let desktop_saves = desktop_dir.join("saves");
    populate_island(&desktop_saves, "v1-desktop");
    let desktop_store = create_store(&desktop_dir);
    let desktop_relay = SaveRelay::new(remote.clone(), desktop_store, "desktop");

    let push_result = desktop_relay.push(&desktop_saves, None).unwrap();
    match push_result {
        Sync::Done(msg) => {
            assert!(
                msg.contains("Pushed revision 1") || msg.contains("desktop"),
                "{msg}"
            );
        }
        other => panic!("expected Sync::Done, got {other:?}"),
    }

    // Device B (handheld) pulls the island
    let handheld_dir = root.join("handheld");
    let handheld_saves = handheld_dir.join("saves");
    let handheld_store = create_store(&handheld_dir);
    let handheld_relay = SaveRelay::new(remote, handheld_store, "handheld");

    let pull_result = handheld_relay.pull(&handheld_saves, None).unwrap();
    match pull_result {
        Sync::Done(msg) => {
            assert!(
                msg.contains("Pulled revision 1") || msg.contains("Adopted"),
                "{msg}"
            );
        }
        other => panic!("expected Sync::Done, got {other:?}"),
    }

    // Verify pulled files match
    let content = fs::read_to_string(handheld_saves.join("main.dat")).unwrap();
    assert_eq!(content, "main island data: v1-desktop");
    let villager = fs::read_to_string(handheld_saves.join("Villager0/personal.dat")).unwrap();
    assert_eq!(villager, "villager: v1-desktop");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn offline_pull_and_push() {
    let root = temp_fixture_dir("offline");
    let remote = Arc::new(MemoryRemote::new());

    let device_dir = root.join("device");
    let saves_dir = device_dir.join("saves");
    populate_island(&saves_dir, "offline-test");
    let store = create_store(&device_dir);
    let relay = SaveRelay::new(remote.clone(), store, "handheld");

    // Make server unreachable
    remote.set_reachable(false);

    // Pull offline maps to Sync::Offline
    let pull_result = relay.pull(&saves_dir, None).unwrap();
    assert_eq!(pull_result, Sync::Offline);

    // Push offline maps to Sync::Offline
    let push_result = relay.push(&saves_dir, None).unwrap();
    assert_eq!(push_result, Sync::Offline);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn conflict_detected_and_resolved() {
    let root = temp_fixture_dir("conflict");
    let remote = Arc::new(MemoryRemote::new());

    // Step 1: Device A creates initial island and pushes it
    let dev_a_dir = root.join("dev_a");
    let dev_a_saves = dev_a_dir.join("saves");
    populate_island(&dev_a_saves, "initial");
    let store_a = create_store(&dev_a_dir);
    let relay_a = SaveRelay::new(remote.clone(), store_a, "device-a");
    assert!(matches!(
        relay_a.push(&dev_a_saves, None).unwrap(),
        Sync::Done(_)
    ));

    // Step 2: Device B pulls initial island
    let dev_b_dir = root.join("dev_b");
    let dev_b_saves = dev_b_dir.join("saves");
    let store_b = create_store(&dev_b_dir);
    let relay_b = SaveRelay::new(remote.clone(), store_b, "device-b");
    assert!(matches!(
        relay_b.pull(&dev_b_saves, None).unwrap(),
        Sync::Done(_)
    ));

    // Step 3: Device A makes changes and pushes revision 2
    fs::write(
        dev_a_saves.join("main.dat"),
        "main island data: revision-2-a",
    )
    .unwrap();
    assert!(matches!(
        relay_a.push(&dev_a_saves, None).unwrap(),
        Sync::Done(_)
    ));

    // Step 4: Device B also makes changes locally without pulling
    fs::write(
        dev_b_saves.join("main.dat"),
        "main island data: conflicting-b",
    )
    .unwrap();

    // Step 5: Device B tries to pull -> Conflict!
    let conflict = relay_b.pull(&dev_b_saves, None).unwrap();
    match &conflict {
        Sync::Conflict { server, local } => {
            assert!(!server.is_empty(), "server description should not be empty");
            assert!(!local.is_empty(), "local description should not be empty");
        }
        other => panic!("expected Sync::Conflict, got {other:?}"),
    }

    // Step 6: Device B resolves conflict with Keep::Server
    let resolved = relay_b.pull(&dev_b_saves, Some(Keep::Server)).unwrap();
    assert!(matches!(resolved, Sync::Done(_)));

    // Verify Device B now has Device A's changes
    let content = fs::read_to_string(dev_b_saves.join("main.dat")).unwrap();
    assert_eq!(content, "main island data: revision-2-a");

    let _ = fs::remove_dir_all(&root);
}

struct DummyGame;
impl acnh_core::launch::Game for DummyGame {
    fn run(&self, _env: &[(String, String)]) -> acnh_core::Result<i32> {
        Ok(0)
    }
}

#[test]
fn play_flow_with_save_relay() {
    let root = temp_fixture_dir("play-flow");
    let remote = Arc::new(MemoryRemote::new());

    let dev_dir = root.join("device");
    let saves_dir = dev_dir.join("saves");
    populate_island(&saves_dir, "play-flow-v1");
    let store = create_store(&dev_dir);
    let relay = SaveRelay::new(remote, store, "player-device");
    let game = DummyGame;

    let played = acnh_core::launch::play(&relay, &game, &saves_dir, &[], &|_| {
        panic!("choose should not be called")
    })
    .unwrap();

    assert!(matches!(played.pulled, Sync::Done(_)));
    assert_eq!(played.exit_code, 0);
    assert!(matches!(played.pushed, Sync::Done(_)));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn map_sync_actions() {
    let base_data = SyncData {
        game: "acnh".into(),
        action: Action::Noop,
        revision: Some(1),
        server: Some(Side {
            device: "deck".into(),
            time: "2026-11-17T12:00:00Z".into(),
            revision: Some(1),
        }),
        local: Some(Side {
            device: "pc".into(),
            time: "2026-11-17T11:00:00Z".into(),
            revision: None,
        }),
        backup: None,
        counts: Counts::default(),
        message: "Already in sync.".into(),
    };

    // Done actions
    for action in [
        Action::Noop,
        Action::Adopted,
        Action::Pulled,
        Action::Pushed,
    ] {
        let mut data = base_data.clone();
        data.action = action;
        data.message = format!("action {action:?}");
        let report = SyncReport {
            sync: data,
            held: None,
            server_card: None,
            local_card: None,
        };
        assert_eq!(map_sync(report), Sync::Done(format!("action {action:?}")));
    }

    // Offline / Deferred
    for action in [Action::Offline, Action::Deferred] {
        let mut data = base_data.clone();
        data.action = action;
        let report = SyncReport {
            sync: data,
            held: None,
            server_card: None,
            local_card: None,
        };
        assert_eq!(map_sync(report), Sync::Offline);
    }

    // Conflict
    let mut conflict_data = base_data.clone();
    conflict_data.action = Action::Conflict;
    let conflict_report = SyncReport {
        sync: conflict_data,
        held: None,
        server_card: None,
        local_card: None,
    };
    match map_sync(conflict_report) {
        Sync::Conflict { server, local } => {
            assert!(server.contains("deck"));
            assert!(local.contains("pc"));
        }
        other => panic!("expected Sync::Conflict, got {other:?}"),
    }

    // Held
    let mut held_data = base_data;
    held_data.action = Action::Held;
    let held_report = SyncReport {
        sync: held_data,
        held: Some(Held {
            device: "switch".into(),
            since: "2026-11-17T10:00:00Z".into(),
        }),
        server_card: None,
        local_card: None,
    };
    match map_sync(held_report) {
        Sync::Conflict { server, .. } => {
            assert!(server.contains("switch"));
        }
        other => panic!("expected Sync::Conflict, got {other:?}"),
    }
}
