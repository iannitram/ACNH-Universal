// SPDX-License-Identifier: MIT
// Copyright (c) 2026 iannitram
// Adapted from iannitram/totk-universal at 80148e16e60b5ca5d34ca87d3ef3360567490352,
// crates/core/tests/hub_pair.rs. See NOTICE for the MIT notice.
#![cfg(feature = "hub")]

use acnh_core::error::{Error, Result};
use acnh_core::hub::{
    pair_in, read_peers, remove_peer, Clock, MemoryTransport, PairingSession, Peer, CODE_LIFETIME,
    MAX_FAILED_ATTEMPTS,
};
use acnh_core::platform::{AppDirs, Vars};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Folders(PathBuf);

impl Folders {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "acnh-hub-pair-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn dirs(&self, side: &str) -> AppDirs {
        AppDirs::new(&Vars {
            home: self.0.clone(),
            data: self.0.join(side),
            config: self.0.join(side),
        })
    }
}

impl Drop for Folders {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn run_pair(a: &AppDirs, b: &AppDirs, b_code: &'static str) -> (Result<Peer>, Result<Peer>) {
    let (mut left, mut right) = MemoryTransport::pair();
    thread::scope(|scope| {
        let first = scope.spawn(|| {
            pair_in(
                &mut left,
                &mut PairingSession::new("012345").unwrap(),
                "client",
                a,
            )
        });
        let second = scope.spawn(|| {
            pair_in(
                &mut right,
                &mut PairingSession::new(b_code).unwrap(),
                "hub",
                b,
            )
        });
        (first.join().unwrap(), second.join().unwrap())
    })
}

#[test]
fn matching_code_confirms_both_directions_and_round_trips_peers() {
    let folders = Folders::new();
    let (a, b) = (folders.dirs("a"), folders.dirs("b"));
    let (left, right) = run_pair(&a, &b, "012345");
    let (left, right) = (left.unwrap(), right.unwrap());
    assert_eq!(left.key_hex.len(), 64);
    assert!(left.key_hex.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(left.key_hex == right.key_hex, "paired keys differ");
    assert_eq!(left.device, "client");
    assert_eq!(right.device, "hub");
    assert_eq!(read_peers(&a).unwrap(), vec![left]);
    assert_eq!(read_peers(&b).unwrap(), vec![right]);
    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(a.config.join("peers.json")).unwrap()).unwrap();
    assert_eq!(json[0]["device"], "client");
    assert!(remove_peer(&a, "client").unwrap());
    assert!(read_peers(&a).unwrap().is_empty());
    assert!(!remove_peer(&a, "client").unwrap());
    assert_eq!(read_peers(&b).unwrap().len(), 1);
    assert_eq!(fs::read_dir(&a.config).unwrap().count(), 1);
}

#[test]
fn pairing_again_replaces_the_remembered_key() {
    let folders = Folders::new();
    let (a, b) = (folders.dirs("a"), folders.dirs("b"));
    let (first, _) = run_pair(&a, &b, "012345");
    let (left, right) = run_pair(&a, &b, "012345");
    let (left, right) = (left.unwrap(), right.unwrap());
    assert!(
        left.key_hex != first.unwrap().key_hex,
        "pairing reused the old key"
    );
    assert!(left.key_hex == right.key_hex, "paired keys differ");
    assert_eq!(read_peers(&a).unwrap(), vec![left]);
    assert_eq!(read_peers(&b).unwrap(), vec![right]);
}

#[test]
fn wrong_code_fails_on_both_sides_without_writing() {
    let folders = Folders::new();
    let (a, b) = (folders.dirs("a"), folders.dirs("b"));
    let (left, right) = run_pair(&a, &b, "012346");
    let mismatch = Error::Hub("the pairing code did not match".into());
    assert_eq!(left.unwrap_err(), mismatch);
    assert_eq!(right.unwrap_err(), mismatch);
    assert!(!a.config.exists());
    assert!(!b.config.exists());
    assert!(read_peers(&a).unwrap().is_empty());
    assert!(read_peers(&b).unwrap().is_empty());
}

#[test]
fn peer_debug_redacts_the_key() {
    let peer = Peer {
        device: "handheld".into(),
        key_hex: "0123456789abcdef".repeat(4),
    };
    for formatted in [format!("{peer:?}"), format!("{peer:#?}")] {
        assert!(!formatted.contains(&peer.key_hex));
        assert!(formatted.contains("[redacted]"));
        assert!(formatted.contains("handheld"));
    }
}

#[test]
fn malformed_peers_file_error_does_not_echo_secrets() {
    let folders = Folders::new();
    let dirs = folders.dirs("a");
    fs::create_dir_all(&dirs.config).unwrap();
    let secret = "ab".repeat(32);
    fs::write(
        dirs.config.join("peers.json"),
        format!(r#"[{{"device":"client","key_hex":{{"{secret}":0}}}}]"#),
    )
    .unwrap();
    let error = read_peers(&dirs).unwrap_err();
    assert!(!format!("{error:?} {error}").contains(&secret));
}

#[test]
fn damaged_confirmation_in_one_direction_is_rejected_by_both_sides() {
    use acnh_core::hub::Transport;
    use std::io::{self, Read, Write};

    struct DamageConfirmation {
        inner: MemoryTransport,
        writes: usize,
    }

    impl Read for DamageConfirmation {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.read(buf)
        }
    }

    impl Write for DamageConfirmation {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            let mut bytes = buf.to_vec();
            if self.writes == 2 {
                bytes[0] ^= 1;
            }
            self.inner.write(&bytes)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    impl Transport for DamageConfirmation {}

    let folders = Folders::new();
    let (a, b) = (folders.dirs("a"), folders.dirs("b"));
    let (mut left, right) = MemoryTransport::pair();
    let mut right = DamageConfirmation {
        inner: right,
        writes: 0,
    };
    thread::scope(|scope| {
        let first = scope.spawn(|| {
            pair_in(
                &mut left,
                &mut PairingSession::new("012345").unwrap(),
                "client",
                &a,
            )
        });
        let second = scope.spawn(|| {
            pair_in(
                &mut right,
                &mut PairingSession::new("012345").unwrap(),
                "hub",
                &b,
            )
        });
        for result in [first.join().unwrap(), second.join().unwrap()] {
            assert_eq!(
                result.unwrap_err(),
                Error::Hub("the pairing code did not match".into())
            );
        }
    });
    assert!(!a.config.exists());
    assert!(!b.config.exists());
}

const SPENT: &str = "this pairing code is used up: start pairing again with a new code";
const EXPIRED: &str = "this pairing code has expired: start pairing again with a new code";

fn mismatch() -> Error {
    Error::Hub("the pairing code did not match".into())
}

/// A clock the test moves by hand; the session holds a clone of it.
#[derive(Clone)]
struct FakeClock(Arc<Mutex<Instant>>);

impl FakeClock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }

    fn advance(&self, by: Duration) {
        *self.0.lock().unwrap() += by;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
}

/// One attempt of `session` against a peer that types `peer_code`. Both sides run the exchange.
fn attempt(
    session: &mut PairingSession<FakeClock>,
    a: &AppDirs,
    b: &AppDirs,
    peer_code: &str,
) -> Result<Peer> {
    let (mut left, mut right) = MemoryTransport::pair();
    let mut peer = PairingSession::new(peer_code).unwrap();
    thread::scope(|scope| {
        let first = scope.spawn(|| pair_in(&mut left, session, "client", a));
        let second = scope.spawn(|| pair_in(&mut right, &mut peer, "hub", b));
        let result = first.join().unwrap();
        second.join().unwrap().ok();
        result
    })
}

/// An attempt that must be refused before any bytes move: the peer's end is already gone, so
/// any transport use would fail with an I/O error instead of the expected message.
fn refused(session: &mut PairingSession<FakeClock>, a: &AppDirs) -> Error {
    let (mut left, right) = MemoryTransport::pair();
    drop(right);
    pair_in(&mut left, session, "client", a).unwrap_err()
}

#[test]
fn two_wrong_codes_leave_the_code_usable() {
    let folders = Folders::new();
    let (a, b) = (folders.dirs("a"), folders.dirs("b"));
    let mut session = PairingSession::with_clock("012345", FakeClock::new()).unwrap();
    for _ in 1..MAX_FAILED_ATTEMPTS {
        assert_eq!(
            attempt(&mut session, &a, &b, "012346").unwrap_err(),
            mismatch()
        );
    }
    assert!(attempt(&mut session, &a, &b, "012345").is_ok());
}

#[test]
fn third_wrong_code_ends_the_session_and_a_correct_code_is_refused_after() {
    let folders = Folders::new();
    let (a, b) = (folders.dirs("a"), folders.dirs("b"));
    let mut session = PairingSession::with_clock("012345", FakeClock::new()).unwrap();
    for _ in 0..MAX_FAILED_ATTEMPTS {
        assert_eq!(
            attempt(&mut session, &a, &b, "012346").unwrap_err(),
            mismatch()
        );
    }
    assert_eq!(refused(&mut session, &a), Error::Hub(SPENT.into()));
    assert!(read_peers(&a).unwrap().is_empty());
    assert!(read_peers(&b).unwrap().is_empty());
}

#[test]
fn a_code_expires_five_minutes_after_it_is_made() {
    let folders = Folders::new();
    let (a, b) = (folders.dirs("a"), folders.dirs("b"));
    let clock = FakeClock::new();
    let mut session = PairingSession::with_clock("012345", clock.clone()).unwrap();
    clock.advance(CODE_LIFETIME - Duration::from_secs(1));
    assert!(attempt(&mut session, &a, &b, "012345").is_ok());

    let mut expired = PairingSession::with_clock("012345", clock.clone()).unwrap();
    clock.advance(CODE_LIFETIME);
    assert_eq!(refused(&mut expired, &a), Error::Hub(EXPIRED.into()));
    assert_eq!(read_peers(&a).unwrap().len(), 1);
}

#[test]
fn dropped_connections_count_as_failed_tries() {
    let folders = Folders::new();
    let a = folders.dirs("a");
    let mut session = PairingSession::with_clock("012345", FakeClock::new()).unwrap();
    for _ in 0..MAX_FAILED_ATTEMPTS {
        // The peer's end is gone, so the exchange ends in a transport error, not a mismatch.
        let (mut left, right) = MemoryTransport::pair();
        drop(right);
        let error = pair_in(&mut left, &mut session, "client", &a).unwrap_err();
        assert_ne!(error, Error::Hub(SPENT.into()));
    }
    assert_eq!(refused(&mut session, &a), Error::Hub(SPENT.into()));
}
