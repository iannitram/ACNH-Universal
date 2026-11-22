#![cfg(feature = "hub")]

use acnh_core::hub::{self, MemoryTransport, PairingSession, TcpTransport};
use acnh_core::platform::{AppDirs, Os};
use std::fs;
use std::net::TcpListener;
use std::path::Path;

fn dirs(root: &Path, name: &str) -> AppDirs {
    AppDirs {
        config: root.join(name),
        data: root.join(format!("{name}-data")),
    }
}

fn pair(a: &AppDirs, b: &AppDirs) {
    let (mut x, mut y) = MemoryTransport::pair();
    std::thread::scope(|s| {
        let thread = s.spawn(|| {
            hub::pair_in(
                &mut x,
                &mut PairingSession::new("123456").unwrap(),
                "client",
                a,
            )
            .unwrap()
        });
        hub::pair_in(
            &mut y,
            &mut PairingSession::new("123456").unwrap(),
            "hub",
            b,
        )
        .unwrap();
        thread.join().unwrap();
    });
}

fn fixture(root: &Path) {
    fs::create_dir_all(root.join("fonts")).unwrap();
    fs::create_dir_all(root.join("mods/ptbr/romfs/Pack")).unwrap();
    for (path, data) in [
        ("libacnh_game.so", "synthetic library"),
        ("romfs.pak", "synthetic romfs"),
        ("spirv.pak", "synthetic shaders"),
        ("fonts/shared_0.ttf", "synthetic font"),
        ("mods/ptbr/romfs/Pack/test.pack", "synthetic mod"),
        ("mods/ptbr/romfs/secret.keys", "not sent"),
    ] {
        fs::write(root.join(path), data).unwrap();
    }
}

#[test]
fn paired_loopback_installs_only_prepared_files_and_preserves_client_data() {
    let temp = tempfile::tempdir().unwrap();
    let server = dirs(temp.path(), "server");
    let client = dirs(temp.path(), "client");
    pair(&server, &client);
    let source = temp.path().join("source");
    fixture(&source);
    let root = temp.path().join("private");
    fs::create_dir_all(root.join("game")).unwrap();
    fs::write(root.join("game/old"), "old").unwrap();
    fs::write(root.join("config.ini"), "settings").unwrap();
    fs::write(root.join("save"), "island").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    std::thread::scope(|s| {
        let worker = s.spawn(|| {
            let (stream, _) = listener.accept().unwrap();
            hub::serve_prepared(
                &mut TcpTransport::from_stream(stream).unwrap(),
                &server,
                "client",
                Os::Android,
                &source,
            )
            .unwrap();
        });
        let mut transport = TcpTransport::connect(listener.local_addr().unwrap()).unwrap();
        hub::pull_prepared(&mut transport, &client, "hub", Os::Android, &root).unwrap();
        worker.join().unwrap();
    });
    assert_eq!(
        fs::read(root.join("game/romfs.pak")).unwrap(),
        b"synthetic romfs"
    );
    assert_eq!(
        fs::read(root.join("game/mods/ptbr/romfs/Pack/test.pack")).unwrap(),
        b"synthetic mod"
    );
    assert!(!root.join("game/mods/ptbr/romfs/secret.keys").exists());
    assert!(!root.join("game/old").exists());
    assert_eq!(fs::read(root.join("config.ini")).unwrap(), b"settings");
    assert_eq!(fs::read(root.join("save")).unwrap(), b"island");
}

#[test]
fn unremembered_peer_and_wrong_targets_send_nothing() {
    use std::io::Read;
    let temp = tempfile::tempdir().unwrap();
    let dirs = dirs(temp.path(), "server");
    let source = temp.path().join("source");
    fixture(&source);
    for (device, target) in [
        ("unknown", Os::Android),
        ("unknown", Os::Linux),
        ("unknown", Os::Ios),
    ] {
        let (mut server, mut client) = MemoryTransport::pair();
        assert!(hub::serve_prepared(&mut server, &dirs, device, target, &source).is_err());
        drop(server);
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).unwrap();
        assert!(bytes.is_empty());
    }
}

#[test]
fn incomplete_source_cannot_replace_existing_game() {
    let temp = tempfile::tempdir().unwrap();
    let server = dirs(temp.path(), "server");
    let client = dirs(temp.path(), "client");
    pair(&server, &client);
    let source = temp.path().join("source");
    fixture(&source);
    fs::remove_file(source.join("romfs.pak")).unwrap();
    let root = temp.path().join("private");
    fs::create_dir_all(root.join("game")).unwrap();
    fs::write(root.join("game/old"), "old").unwrap();
    let (mut a, mut b) = MemoryTransport::pair();
    std::thread::scope(|s| {
        let worker =
            s.spawn(move || hub::serve_prepared(&mut a, &server, "client", Os::Android, &source));
        assert!(hub::pull_prepared(&mut b, &client, "hub", Os::Android, &root).is_err());
        assert!(worker.join().unwrap().is_err());
    });
    assert_eq!(fs::read(root.join("game/old")).unwrap(), b"old");
}

#[test]
fn unknown_key_receives_only_challenge_and_no_prepared_bytes() {
    use acnh_core::hub::Transport;
    use std::io::{self, Read, Write};
    struct Counted {
        inner: MemoryTransport,
        written: usize,
    }
    impl Read for Counted {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            self.inner.read(b)
        }
    }
    impl Write for Counted {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            let n = self.inner.write(b)?;
            self.written += n;
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }
    impl Transport for Counted {}
    let temp = tempfile::tempdir().unwrap();
    let server = dirs(temp.path(), "server");
    let real_client = dirs(temp.path(), "real-client");
    pair(&server, &real_client);
    let other_server = dirs(temp.path(), "other-server");
    let attacker = dirs(temp.path(), "attacker");
    pair(&other_server, &attacker);
    let source = temp.path().join("source");
    fixture(&source);
    let root = temp.path().join("private");
    fs::create_dir(&root).unwrap();
    let (a, mut b) = MemoryTransport::pair();
    std::thread::scope(|s| {
        let worker = s.spawn(move || {
            let mut transport = Counted {
                inner: a,
                written: 0,
            };
            assert!(
                hub::serve_prepared(&mut transport, &server, "client", Os::Android, &source)
                    .is_err()
            );
            transport.written
        });
        assert!(hub::pull_prepared(&mut b, &attacker, "hub", Os::Android, &root).is_err());
        assert_eq!(worker.join().unwrap(), 40); // protocol marker and random challenge only
    });
    assert!(!root.join("game").exists());
}
