#![cfg(feature = "hub")]

use acnh_core::bundle;
use acnh_core::hub::{self, MemoryTransport, PairingSession, TcpTransport, CHUNK_SIZE};
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
    let packed = vec![0x65; CHUNK_SIZE + 17];
    fs::write(source.join("romfs.pak"), &packed).unwrap();
    let root = client.data_root();
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
    let build = temp.path().join("player-update");
    fs::create_dir(&build).unwrap();
    fs::write(build.join("player"), b"synthetic player").unwrap();
    let bundle = bundle::manifest(&build, Os::Android, "3.0.2", "synthetic").unwrap();
    bundle::install(&bundle, &build, &client.install()).unwrap();

    assert_eq!(fs::read(root.join("game/romfs.pak")).unwrap(), packed);
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

fn app_fixture(root: &Path) {
    fixture(root);
    fs::create_dir_all(root.join("analysis")).unwrap();
    fs::create_dir_all(root.join("aoc/1")).unwrap();
    fs::write(root.join("analysis/main.bin"), "synthetic analysis").unwrap();
    fs::write(root.join("aoc/1/data"), "synthetic dlc").unwrap();
}

#[test]
fn paired_loopback_pull_prepared_app_installs_read_only_through_appgame_and_verifies() {
    let temp = tempfile::tempdir().unwrap();
    let server = dirs(temp.path(), "server");
    let client = dirs(temp.path(), "client");
    pair(&server, &client);
    let source = temp.path().join("source");
    app_fixture(&source);
    let packed = vec![0x65; CHUNK_SIZE + 17];
    fs::write(source.join("romfs.pak"), &packed).unwrap();

    let private_dir = temp.path().join("files");
    fs::create_dir_all(private_dir.join("root")).unwrap();
    fs::write(private_dir.join("root/config.ini"), "settings").unwrap();
    fs::write(private_dir.join("root/save"), "island").unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let installed = std::thread::scope(|s| {
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
        let out = hub::pull_prepared_app_with(
            &mut transport,
            &client,
            "hub",
            Os::Android,
            &private_dir,
            64 * 1024 * 1024,
            false,
        )
        .unwrap();
        worker.join().unwrap();
        out
    });

    assert_eq!(installed, private_dir.join("game"));
    for present in [
        "analysis/main.bin",
        "romfs.pak",
        "spirv.pak",
        "libacnh_game.so",
        "fonts/shared_0.ttf",
        "aoc/1/data",
    ] {
        let p = installed.join(present);
        assert!(p.is_file(), "expected {present} in <files>/game/");
        assert!(
            fs::metadata(&p).unwrap().permissions().readonly(),
            "expected {present} to be read-only"
        );
    }
    assert_eq!(fs::read(installed.join("romfs.pak")).unwrap(), packed);
    assert!(!installed.join("mods").exists());
    assert!(!private_dir.join("game.partial").exists());
    assert_eq!(
        fs::read(private_dir.join("root/config.ini")).unwrap(),
        b"settings"
    );
    assert_eq!(fs::read(private_dir.join("root/save")).unwrap(), b"island");
}

#[test]
fn pull_prepared_app_refuses_missing_analysis_or_low_free_space() {
    let temp = tempfile::tempdir().unwrap();
    let server = dirs(temp.path(), "server");
    let client = dirs(temp.path(), "client");
    pair(&server, &client);

    let private_dir = temp.path().join("files");
    fs::create_dir_all(private_dir.join("game")).unwrap();
    fs::write(private_dir.join("game/old"), "keep").unwrap();

    // Missing analysis/main.bin fails appgame::install and leaves existing game/ intact.
    let incomplete = temp.path().join("incomplete");
    fixture(&incomplete);
    let (mut a, mut b) = MemoryTransport::pair();
    let server_ref = &server;
    std::thread::scope(|s| {
        let worker = s.spawn(move || {
            hub::serve_prepared(&mut a, server_ref, "client", Os::Android, &incomplete)
        });
        let err = hub::pull_prepared_app_with(
            &mut b,
            &client,
            "hub",
            Os::Android,
            &private_dir,
            64 * 1024 * 1024,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("analysis/main.bin"));
        worker.join().unwrap().unwrap();
    });
    assert_eq!(fs::read(private_dir.join("game/old")).unwrap(), b"keep");

    // Complete source with insufficient free_bytes fails appgame::install and keeps old game/.
    let complete = temp.path().join("complete");
    app_fixture(&complete);
    let (mut a, mut b) = MemoryTransport::pair();
    std::thread::scope(|s| {
        let worker = s.spawn(move || {
            hub::serve_prepared(&mut a, server_ref, "client", Os::Android, &complete)
        });
        let err = hub::pull_prepared_app_with(
            &mut b,
            &client,
            "hub",
            Os::Android,
            &private_dir,
            4,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not enough free space"));
        worker.join().unwrap().unwrap();
    });
    assert_eq!(fs::read(private_dir.join("game/old")).unwrap(), b"keep");
}

fn desktop_fixture(root: &Path) {
    fs::create_dir_all(root.join("analysis")).unwrap();
    fs::create_dir_all(root.join("fonts")).unwrap();
    fs::create_dir_all(root.join("mods/ptbr/romfs/Pack")).unwrap();
    for (path, data) in [
        ("analysis/main.bin", "synthetic analysis"),
        ("romfs.pak", "synthetic romfs"),
        ("spirv.pak", "synthetic shaders"),
        ("libacnh_game.so", "not for a desktop"),
        ("fonts/shared_0.ttf", "synthetic font"),
        ("mods/ptbr/romfs/Pack/test.pack", "synthetic mod"),
        ("mods/ptbr/romfs/secret.keys", "not sent"),
    ] {
        fs::write(root.join(path), data).unwrap();
    }
}

fn serve_once(
    listener: &TcpListener,
    server: &AppDirs,
    target: Os,
    source: &Path,
) -> acnh_core::Result<()> {
    let (stream, _) = listener.accept().unwrap();
    hub::serve_prepared(
        &mut TcpTransport::from_stream(stream).unwrap(),
        server,
        "client",
        target,
        source,
    )
}

#[test]
fn paired_loopback_installs_the_desktop_game_beside_the_player_and_keeps_client_data() {
    for target in [Os::Linux, Os::Windows] {
        let temp = tempfile::tempdir().unwrap();
        let server = dirs(temp.path(), "server");
        let client = dirs(temp.path(), "client");
        pair(&server, &client);
        let source = temp.path().join("source");
        desktop_fixture(&source);
        let packed = vec![0x65; CHUNK_SIZE + 17];
        fs::write(source.join("romfs.pak"), &packed).unwrap();
        // The player is installed first, with the device's own data beside it.
        let install = client.install();
        fs::create_dir_all(install.join("game")).unwrap();
        fs::write(install.join("game/old"), "old").unwrap();
        fs::write(install.join("acnh"), "player").unwrap();
        fs::create_dir_all(client.data_root()).unwrap();
        fs::write(client.data_root().join("save"), "island").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        std::thread::scope(|s| {
            let worker = s.spawn(|| serve_once(&listener, &server, target, &source).unwrap());
            let mut transport = TcpTransport::connect(listener.local_addr().unwrap()).unwrap();
            let dest = hub::pull_prepared_desktop(&mut transport, &client, "hub", target, u64::MAX)
                .unwrap();
            assert_eq!(dest, install.join("game"));
            worker.join().unwrap();
        });
        let game = install.join("game");
        assert_eq!(fs::read(game.join("romfs.pak")).unwrap(), packed);
        assert!(game.join("analysis/main.bin").is_file());
        assert!(game.join("mods/ptbr/romfs/Pack/test.pack").is_file());
        // Never sent: the Android library, a key-like file, and nothing of the old tree stays.
        assert!(!game.join("libacnh_game.so").exists());
        assert!(!game.join("mods/ptbr/romfs/secret.keys").exists());
        assert!(!game.join("old").exists());
        assert_eq!(fs::read(install.join("acnh")).unwrap(), b"player");
        assert_eq!(
            fs::read(client.data_root().join("save")).unwrap(),
            b"island"
        );
        assert!(!client.data.join(".prepared-staging").exists());
        assert!(!client.data.join(".prepared-previous").exists());

        // A player update keeps the pulled game.
        let build = temp.path().join("player-update");
        fs::create_dir(&build).unwrap();
        fs::write(build.join("acnh"), b"new player").unwrap();
        let bundle = bundle::manifest(&build, target, "3.0.2", "synthetic").unwrap();
        bundle::install(&bundle, &build, &install).unwrap();
        assert_eq!(fs::read(install.join("acnh")).unwrap(), b"new player");
        assert!(install.join("game/analysis/main.bin").is_file());
    }
}

#[test]
fn desktop_pull_refuses_a_hub_with_no_analysis_and_keeps_the_old_game() {
    let temp = tempfile::tempdir().unwrap();
    let server = dirs(temp.path(), "server");
    let client = dirs(temp.path(), "client");
    pair(&server, &client);
    let source = temp.path().join("source");
    desktop_fixture(&source);
    fs::remove_file(source.join("analysis/main.bin")).unwrap();
    let install = client.install();
    fs::create_dir_all(install.join("game")).unwrap();
    fs::write(install.join("game/old"), "old").unwrap();
    let (mut a, mut b) = MemoryTransport::pair();
    std::thread::scope(|s| {
        let worker =
            s.spawn(move || hub::serve_prepared(&mut a, &server, "client", Os::Linux, &source));
        assert!(hub::pull_prepared_desktop(&mut b, &client, "hub", Os::Linux, u64::MAX).is_err());
        assert!(worker.join().unwrap().is_err());
    });
    assert_eq!(fs::read(install.join("game/old")).unwrap(), b"old");
}

#[test]
fn desktop_pull_needs_the_free_space_before_writing_and_only_desktop_targets() {
    let temp = tempfile::tempdir().unwrap();
    let server = dirs(temp.path(), "server");
    let client = dirs(temp.path(), "client");
    pair(&server, &client);
    let source = temp.path().join("source");
    desktop_fixture(&source);
    fs::create_dir_all(client.install()).unwrap();
    let (mut a, mut b) = MemoryTransport::pair();
    std::thread::scope(|s| {
        let server = &server;
        let source = &source;
        let worker =
            s.spawn(move || hub::serve_prepared(&mut a, server, "client", Os::Linux, source));
        let error = hub::pull_prepared_desktop(&mut b, &client, "hub", Os::Linux, 10).unwrap_err();
        assert!(error.to_string().contains("free space"), "{error}");
        drop(b);
        let _ = worker.join().unwrap();
    });
    assert!(!client.install().join("game").exists());
    assert!(!client.data.join(".prepared-staging").exists());
    // Android has its own pull; the Apple targets have none.
    for target in [Os::Android, Os::Mac, Os::Ios] {
        let (_a, mut b) = MemoryTransport::pair();
        assert!(hub::pull_prepared_desktop(&mut b, &client, "hub", target, u64::MAX).is_err());
    }
    // A hub never builds a manifest for the Apple targets either.
    for target in [Os::Mac, Os::Ios] {
        let (mut a, _b) = MemoryTransport::pair();
        assert!(hub::serve_prepared(&mut a, &server, "client", target, &source).is_err());
    }
}

#[test]
fn a_desktop_client_never_accepts_an_android_manifest() {
    let temp = tempfile::tempdir().unwrap();
    let server = dirs(temp.path(), "server");
    let client = dirs(temp.path(), "client");
    pair(&server, &client);
    let source = temp.path().join("source");
    fixture(&source);
    fs::create_dir_all(client.install()).unwrap();
    // The channel is bound to the target, so a hub answering for Android fails the handshake
    // of a Linux client before any manifest is read.
    let (mut a, mut b) = MemoryTransport::pair();
    std::thread::scope(|s| {
        let server = &server;
        let source = &source;
        let worker =
            s.spawn(move || hub::serve_prepared(&mut a, server, "client", Os::Android, source));
        assert!(hub::pull_prepared_desktop(&mut b, &client, "hub", Os::Linux, u64::MAX).is_err());
        drop(b);
        let _ = worker.join().unwrap();
    });
    assert!(!client.install().join("game").exists());
}
