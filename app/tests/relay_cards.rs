//! Exercise the Continue chooser with a conflict reported by save-relay itself.

use acnh_app::play;
use acnh_app::vm::{ContinuePageVm, Nav};
use acnh_core::launch::{Game, Relay, Sync};
use acnh_core::relay::SaveRelay;
use save_relay::{local::LocalStore, platform::AppDirs, remote::MemoryRemote};
use std::cell::RefCell;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

struct MustNotRun;

impl Game for MustNotRun {
    fn run(&self, _: &[(String, String)]) -> acnh_core::Result<i32> {
        panic!("the island conflict must hold the game")
    }
}

#[test]
fn real_conflict_reports_supply_the_chooser_cards() {
    let temp = tempfile::tempdir().unwrap();
    let remote = Arc::new(MemoryRemote::new());
    let time = Arc::new(AtomicU64::new(1_700_000_000));
    let relay = |device: &str| {
        let base = temp.path().join(device);
        let dirs = AppDirs {
            config: base.join("config"),
            data: base.join("data"),
        };
        let clock = Arc::clone(&time);
        let save = base.join("root/device_save");
        std::fs::create_dir_all(&save).unwrap();
        (
            SaveRelay::new(remote.clone(), LocalStore::new(dirs), device)
                .with_clock(move || clock.load(Ordering::SeqCst)),
            save,
        )
    };
    let (server, their_save) = relay("mac-relay");
    let (local, our_save) = relay("local-client");
    std::fs::write(their_save.join("main.dat"), "our original island").unwrap();
    std::fs::write(their_save.join("mainHeader.dat"), "original header").unwrap();
    server.push(&their_save, None).unwrap();
    local.pull(&our_save, None).unwrap();
    local.push(&our_save, None).unwrap();
    time.fetch_add(10, Ordering::SeqCst);
    server.pull(&their_save, None).unwrap();
    std::fs::write(their_save.join("main.dat"), "relay change").unwrap();
    time.fetch_add(120, Ordering::SeqCst);
    assert!(matches!(
        server.push(&their_save, None).unwrap(),
        Sync::Done(_)
    ));
    std::fs::write(our_save.join("main.dat"), "local change").unwrap();

    let mut page = ContinuePageVm::new(
        acnh_core::savecard::read_card(&our_save, Path::new("")).ok(),
        None,
    );
    let events = RefCell::new(Vec::new());
    play::play(&local, &MustNotRun, &our_save, &[], &|event| {
        events.borrow_mut().push(event);
    });
    for event in events.into_inner() {
        page.on_play_event(event);
    }
    assert!(page.view().chooser.is_none());
    page.handle(Nav::Accept);
    let (local, relay) = page.view().chooser.expect("the chooser opened");
    assert!(local.saved.starts_with("local-client"));
    assert!(relay.saved.starts_with("mac-relay"));
    assert_eq!(relay.played, "0 h 02 min played");
}
