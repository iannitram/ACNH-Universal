// SPDX-License-Identifier: MIT
// Copyright (c) 2026 iannitram
// Adapted from iannitram/totk-universal at 80148e16e60b5ca5d34ca87d3ef3360567490352,
// crates/core/tests/hub_transfer.rs. See NOTICE for the MIT notice.
#![cfg(feature = "hub")]

use acnh_core::bundle::{self, Bundle, BundleFile};
use acnh_core::error::{Error, Result};
use acnh_core::hub::{self, MemoryTransport, Peer, TcpTransport, Transport, CHUNK_SIZE};
use acnh_core::platform::{AppDirs, Os, Vars};
use chacha20poly1305::{aead::AeadInOut, ChaCha20Poly1305, KeyInit};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Folders(PathBuf);

impl Folders {
    fn new() -> Self {
        // Keep all test writes inside this worktree, in the ignored build folder.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/hub-transfer-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn old_install(&self) -> PathBuf {
        let root = self.path("installed");
        fs::create_dir_all(root.join("root")).unwrap();
        fs::write(root.join("game.bin"), b"old build").unwrap();
        fs::write(root.join("root/save.sav"), b"old save").unwrap();
        root
    }

    fn check_old(&self) {
        assert_eq!(
            fs::read(self.path("installed/game.bin")).unwrap(),
            b"old build"
        );
        assert_eq!(
            fs::read(self.path("installed/root/save.sav")).unwrap(),
            b"old save"
        );
        assert!(!self.path("installed.old").exists());
    }
}

impl Drop for Folders {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn peer() -> Peer {
    Peer {
        device: "paired test device".into(),
        key_hex: "42".repeat(32),
    }
}

fn paired(folders: &Folders) -> (Peer, Peer) {
    let dirs = |name| {
        AppDirs::new(&Vars {
            home: folders.0.clone(),
            data: folders.path(name),
            config: folders.path(name),
        })
    };
    let (a, b) = (dirs("hub"), dirs("client"));
    let (mut server, mut client) = MemoryTransport::pair();
    thread::scope(|scope| {
        let a = scope.spawn(move || {
            hub::pair_in(
                &mut server,
                &mut hub::PairingSession::new("123456").unwrap(),
                "client",
                &a,
            )
        });
        let b = hub::pair_in(
            &mut client,
            &mut hub::PairingSession::new("123456").unwrap(),
            "hub",
            &b,
        )
        .unwrap();
        (a.join().unwrap().unwrap(), b)
    })
}

// Generate and hash synthetic builds one chunk at a time, including the 64 MiB fixture.
fn publish(folders: &Folders, name: &str, target: Os, size: u64) -> Bundle {
    let root = folders.path("builds").join(name);
    fs::create_dir_all(root.join("shader-cache")).unwrap();
    let mut game = File::create(root.join("game.bin")).unwrap();
    let mut hash = Sha256::new();
    let mut remaining = size;
    let mut part = 0_u8;
    let mut bytes = vec![0; CHUNK_SIZE];
    while remaining != 0 {
        bytes.fill(part);
        let length = remaining.min(CHUNK_SIZE as u64) as usize;
        game.write_all(&bytes[..length]).unwrap();
        hash.update(&bytes[..length]);
        remaining -= length as u64;
        part = part.wrapping_add(1);
    }
    fs::write(root.join("empty.bin"), []).unwrap();
    let shader = b"prepared shader cache";
    fs::write(root.join("shader-cache/shader-cache.pack"), shader).unwrap();
    let files = vec![
        BundleFile {
            path: "empty.bin".into(),
            size: 0,
            sha256: format!("{:x}", Sha256::digest([])),
        },
        BundleFile {
            path: "game.bin".into(),
            size,
            sha256: format!("{:x}", hash.finalize()),
        },
        BundleFile {
            path: "shader-cache/shader-cache.pack".into(),
            size: shader.len() as u64,
            sha256: format!("{:x}", Sha256::digest(shader)),
        },
    ];
    let lines: String = files
        .iter()
        .map(|f| format!("file {} {} {}\n", f.size, f.sha256, f.path))
        .collect();
    let b = Bundle {
        target,
        version: "3.0.2".into(),
        commit: "synthetic-pin".into(),
        build_id: format!("{:x}", Sha256::digest(lines.as_bytes()))[..16].into(),
        files,
    };
    fs::write(root.join("bundle.txt"), bundle::to_text(&b)).unwrap();
    b
}

#[derive(Default)]
struct Metrics {
    chunks: usize,
    payload_bytes: usize,
    frames: usize,
    max_io: usize,
    first_chunk: Vec<u8>,
}

enum Fault {
    None,
    Cut(usize),
    Flip(usize),
    Replay(usize, Vec<u8>),
    Swap,
}

// Instrument the server's stream, without buffering the build. The protocol writes
// 72 handshake bytes, then length-prefixed AEAD frames (manifest first, final ack last).
struct Inspect<T> {
    inner: T,
    handshake: usize,
    payload: bool,
    fault: Fault,
    metrics: Metrics,
}

impl<T> Inspect<T> {
    fn new(inner: T, fault: Fault) -> Self {
        Self {
            inner,
            handshake: 0,
            payload: false,
            fault,
            metrics: Metrics::default(),
        }
    }
}

impl<T: Read> Read for Inspect<T> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.metrics.max_io = self.metrics.max_io.max(bytes.len());
        self.inner.read(bytes)
    }
}

impl<T: Write> Write for Inspect<T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.metrics.max_io = self.metrics.max_io.max(bytes.len());
        if self.handshake < 72 {
            self.inner.write_all(bytes)?;
            self.handshake += bytes.len();
        } else if !self.payload {
            assert_eq!(bytes.len(), 4);
            self.inner.write_all(bytes)?;
            self.payload = true;
        } else {
            self.payload = false;
            let chunk = self.metrics.frames > 0 && bytes.len() > 17;
            self.metrics.frames += 1;
            if chunk {
                self.metrics.chunks += 1;
                self.metrics.payload_bytes += bytes.len() - 16;
                if self.metrics.chunks == 1 {
                    self.metrics.first_chunk = bytes.to_vec();
                }
            }
            match &self.fault {
                Fault::Cut(at) if chunk && *at == self.metrics.chunks => {
                    self.inner.write_all(&bytes[..bytes.len() / 2])?;
                    return Err(io::ErrorKind::ConnectionReset.into());
                }
                Fault::Flip(at) if chunk && *at == self.metrics.chunks => {
                    let mut damaged = bytes.to_vec();
                    damaged[0] ^= 1;
                    self.inner.write_all(&damaged)?;
                }
                Fault::Replay(at, saved) if chunk && *at == self.metrics.chunks => {
                    assert_eq!(saved.len(), bytes.len());
                    self.inner.write_all(saved)?;
                }
                Fault::Swap if chunk && self.metrics.chunks == 2 => {
                    self.inner.write_all(&self.metrics.first_chunk)?;
                }
                _ => self.inner.write_all(bytes)?,
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<T: Read + Write> Transport for Inspect<T> {}

fn transfer(
    folders: &Folders,
    server_peer: &Peer,
    client_peer: &Peer,
    fault: Fault,
) -> (Result<Bundle>, Result<Bundle>, Metrics) {
    let (server, mut client) = MemoryTransport::pair();
    thread::scope(|scope| {
        let server = scope.spawn(move || {
            let mut server = Inspect::new(server, fault);
            let result = hub::serve(&mut server, server_peer, &folders.path("builds"), Os::Mac);
            (result, server.metrics)
        });
        let result = hub::pull(&mut client, client_peer, Os::Mac, &folders.path("staged"));
        // Closing either end on error lets the other side finish without hanging.
        drop(client);
        let (served, metrics) = server.join().unwrap();
        (served, result, metrics)
    })
}

fn assert_hub<T>(result: Result<T>) {
    assert!(matches!(result, Err(Error::Hub(_))));
}

#[test]
fn paired_full_transfer_installs_verifies_and_keeps_prepared_shaders() {
    let folders = Folders::new();
    let install = folders.old_install();
    let expected = publish(&folders, "new", Os::Mac, (CHUNK_SIZE * 3 + 123) as u64);
    let (server_peer, client_peer) = paired(&folders);
    let (served, pulled, metrics) = transfer(&folders, &server_peer, &client_peer, Fault::None);
    assert_eq!(served.unwrap(), expected);
    let pulled = pulled.unwrap();
    assert_eq!(pulled, expected);
    assert_eq!(metrics.chunks, 5);
    assert!(metrics.max_io <= CHUNK_SIZE + 16);
    folders.check_old();
    bundle::verify(&pulled, &folders.path("staged")).unwrap();
    assert!(!folders.path("staged/.hub-transfer").exists());
    bundle::install(&pulled, &folders.path("staged"), &install).unwrap();
    bundle::verify(&pulled, &install).unwrap();
    assert_eq!(
        fs::read(install.join("shader-cache/shader-cache.pack")).unwrap(),
        b"prepared shader cache"
    );
    assert_eq!(
        fs::read(install.join("root/save.sav")).unwrap(),
        b"old save"
    );
}

fn cached_files(folders: &Folders) -> Vec<PathBuf> {
    let cache = fs::read_dir(folders.path("staged/.hub-transfer"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut paths: Vec<_> = fs::read_dir(cache)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    paths.sort();
    paths
}

#[test]
fn cut_halfway_resumes_only_missing_chunks_and_ignores_partial_temp_files() {
    let folders = Folders::new();
    folders.old_install();
    let b = publish(&folders, "new", Os::Mac, (CHUNK_SIZE * 8 + 123) as u64);
    let (served, pulled, first) = transfer(&folders, &peer(), &peer(), Fault::Cut(5));
    assert_hub(served);
    assert_hub(pulled);
    assert_eq!(first.chunks, 5);
    let cached = cached_files(&folders);
    assert_eq!(cached.len(), 4);
    for path in &cached {
        assert_eq!(fs::metadata(path).unwrap().len(), CHUNK_SIZE as u64 + 32);
    }
    let partial = cached[0].parent().unwrap().join(".hub-leftover.tmp");
    fs::write(partial, b"interrupted write").unwrap();
    folders.check_old();
    let (served, pulled, resumed) = transfer(&folders, &peer(), &peer(), Fault::None);
    assert_eq!(served.unwrap(), b);
    assert_eq!(pulled.unwrap(), b);
    assert_eq!(resumed.chunks, 6);
    assert_eq!(
        resumed.payload_bytes,
        CHUNK_SIZE * 4 + 123 + b"prepared shader cache".len()
    );
    bundle::verify(&b, &folders.path("staged")).unwrap();
    folders.check_old();
}

#[test]
fn damaged_or_unsigned_disk_chunks_are_requested_again() {
    let folders = Folders::new();
    publish(&folders, "new", Os::Mac, (CHUNK_SIZE * 8 + 123) as u64);
    let (_, result, _) = transfer(&folders, &peer(), &peer(), Fault::Cut(5));
    assert_hub(result);
    let cached = cached_files(&folders);
    // A complete-size file without the valid receipt is also untrusted.
    fs::write(&cached[0], vec![0; CHUNK_SIZE + 32]).unwrap();
    fs::write(&cached[1], b"torn").unwrap();
    let (_, result, metrics) = transfer(&folders, &peer(), &peer(), Fault::None);
    let b = result.unwrap();
    assert_eq!(metrics.chunks, 8);
    bundle::verify(&b, &folders.path("staged")).unwrap();
}

#[test]
fn flipped_chunk_fails_authentication_and_installs_nothing() {
    let folders = Folders::new();
    folders.old_install();
    publish(&folders, "new", Os::Mac, (CHUNK_SIZE * 4) as u64);
    let (served, pulled, metrics) = transfer(&folders, &peer(), &peer(), Fault::Flip(2));
    assert_hub(served);
    let error = pulled.unwrap_err();
    assert_eq!(
        error,
        Error::Hub("a transfer chunk failed authentication".into())
    );
    assert_eq!(metrics.chunks, 2);
    assert_eq!(cached_files(&folders).len(), 1);
    assert!(!folders.path("staged/game.bin").exists());
    assert!(!format!("{error:?} {error}").contains(&peer().key_hex));
    folders.check_old();
}

#[test]
fn swapped_positions_and_replayed_chunks_from_another_transfer_are_refused() {
    let folders = Folders::new();
    folders.old_install();
    publish(&folders, "new", Os::Mac, (CHUNK_SIZE * 4) as u64);
    let (_, pulled, first) = transfer(&folders, &peer(), &peer(), Fault::Swap);
    assert_hub(pulled);
    fs::remove_dir_all(folders.path("staged")).unwrap();
    let (_, pulled, second) = transfer(
        &folders,
        &peer(),
        &peer(),
        Fault::Replay(1, first.first_chunk.clone()),
    );
    assert_hub(pulled);
    assert_ne!(first.first_chunk, second.first_chunk);
    folders.check_old();
}

#[test]
fn unpaired_peer_gets_no_manifest_or_file_bytes() {
    let folders = Folders::new();
    folders.old_install();
    publish(&folders, "new", Os::Mac, CHUNK_SIZE as u64);
    let wrong = Peer {
        device: "unpaired".into(),
        key_hex: "99".repeat(32),
    };
    let (served, pulled, metrics) = transfer(&folders, &peer(), &wrong, Fault::None);
    assert_hub(served);
    assert_hub(pulled);
    assert_eq!(metrics.frames, 0);
    assert_eq!(metrics.chunks, 0);
    assert!(!folders.path("staged").exists());
    folders.check_old();
    assert!(!format!("{:?}", peer()).contains(&peer().key_hex));
}

#[test]
fn missing_pairing_key_is_refused_before_transport_use() {
    let folders = Folders::new();
    let (mut transport, other) = MemoryTransport::pair();
    drop(other);
    let missing = Peer {
        device: "unpaired".into(),
        key_hex: String::new(),
    };
    for result in [
        hub::serve(&mut transport, &missing, &folders.path("builds"), Os::Mac),
        hub::pull(&mut transport, &missing, Os::Mac, &folders.path("staged")),
    ] {
        assert_eq!(
            result.unwrap_err(),
            Error::Hub("this device is not paired".into())
        );
    }
}

#[test]
fn forbidden_bundle_is_refused_by_server_before_manifest_or_files_move() {
    let folders = Folders::new();
    folders.old_install();
    let mut b = publish(&folders, "new", Os::Mac, CHUNK_SIZE as u64);
    b.files.push(BundleFile {
        path: "prod.keys".into(),
        size: 7,
        sha256: "0".repeat(64),
    });
    fs::write(folders.path("builds/new/bundle.txt"), bundle::to_text(&b)).unwrap();
    let (served, pulled, metrics) = transfer(&folders, &peer(), &peer(), Fault::None);
    assert_hub(served);
    assert_hub(pulled);
    assert_eq!(metrics.frames, 0);
    assert!(!folders.path("staged").exists());
    folders.check_old();
}

fn keyed(key: &[u8], label: &[u8], data: &[u8]) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).unwrap();
    mac.update(b"acnh-universal/hub/transfer/v1/");
    mac.update(label);
    mac.update(data);
    mac
}

// A paired but misbehaving hub bypasses serve's validation. Authenticate its manifest
// properly so the client refusal exercises its own policy, rather than AEAD corruption.
fn malicious_manifest(mut transport: MemoryTransport, text: String) {
    let key = [0x42; 32];
    let salt = [0x17; 32];
    transport.write_all(b"ACNHHUB1").unwrap();
    transport.write_all(&salt).unwrap();
    let mut client_salt = [0; 32];
    transport.read_exact(&mut client_salt).unwrap();
    let mut transcript = b"ACNHHUB1".to_vec();
    transcript.extend_from_slice(&salt);
    transcript.extend_from_slice(&client_salt);
    transcript.extend_from_slice(b"mac");
    let mut proof = [0; 32];
    transport.read_exact(&mut proof).unwrap();
    keyed(&key, b"client-proof", &transcript)
        .verify_slice(&proof)
        .unwrap();
    transport
        .write_all(
            &keyed(&key, b"server-proof", &transcript)
                .finalize()
                .into_bytes(),
        )
        .unwrap();
    let transfer_key: [u8; 32] = keyed(&key, b"server-key", &transcript)
        .finalize()
        .into_bytes()
        .into();
    let cipher = ChaCha20Poly1305::new(&transfer_key.into());
    let mut bytes = text.into_bytes();
    cipher
        .encrypt_in_place(&[0; 12].into(), b"manifest/v1", &mut bytes)
        .unwrap();
    transport
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .unwrap();
    transport.write_all(&bytes).unwrap();
}

#[test]
fn client_refuses_authenticated_forbidden_or_unsafe_paths_before_staging() {
    let folders = Folders::new();
    folders.old_install();
    for path in [
        "prod.keys",
        "../escaped",
        "/absolute",
        "C:/escape",
        "a\\b",
        "a/../prod.keys",
        ".hub-transfer/file",
        "ROOT/save",
        "a/CON",
    ] {
        let b = Bundle {
            target: Os::Mac,
            version: "3.0.2".into(),
            commit: "synthetic-pin".into(),
            build_id: "malicious".into(),
            files: vec![BundleFile {
                path: path.into(),
                size: 1,
                sha256: "0".repeat(64),
            }],
        };
        let (server, mut client) = MemoryTransport::pair();
        thread::scope(|scope| {
            let server = scope.spawn(move || malicious_manifest(server, bundle::to_text(&b)));
            assert_hub(hub::pull(
                &mut client,
                &peer(),
                Os::Mac,
                &folders.path("staged"),
            ));
            drop(client);
            server.join().unwrap();
        });
        assert!(!folders.path("staged").exists());
        folders.check_old();
    }
}

#[test]
fn newest_matching_target_is_selected_by_publication_time() {
    let folders = Folders::new();
    publish(&folders, "z-old", Os::Mac, 123);
    let expected = publish(&folders, "a-new", Os::Mac, 456);
    publish(&folders, "other-target", Os::Linux, 789);
    let old_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    File::options()
        .write(true)
        .open(folders.path("builds/z-old/bundle.txt"))
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(old_time))
        .unwrap();
    let (served, pulled, _) = transfer(&folders, &peer(), &peer(), Fault::None);
    assert_eq!(served.unwrap(), expected);
    assert_eq!(pulled.unwrap(), expected);
}

#[test]
fn unreadable_or_invalid_newer_bundle_text_does_not_block_a_good_build() {
    use std::os::unix::fs::symlink;
    let folders = Folders::new();
    let expected = publish(&folders, "good", Os::Mac, 123);
    let old_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    File::options()
        .write(true)
        .open(folders.path("builds/good/bundle.txt"))
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(old_time))
        .unwrap();
    fs::create_dir(folders.path("builds/garbage")).unwrap();
    fs::write(
        folders.path("builds/garbage/bundle.txt"),
        b"not a bundle \xff",
    )
    .unwrap();
    fs::create_dir(folders.path("builds/oversized")).unwrap();
    fs::write(
        folders.path("builds/oversized/bundle.txt"),
        vec![b'x'; 8 << 20],
    )
    .unwrap();
    fs::create_dir(folders.path("builds/linked")).unwrap();
    symlink(
        folders.path("builds/good/bundle.txt"),
        folders.path("builds/linked/bundle.txt"),
    )
    .unwrap();
    fs::create_dir(folders.path("builds/dirmanifest")).unwrap();
    fs::create_dir(folders.path("builds/dirmanifest/bundle.txt")).unwrap();
    let (served, pulled, _) = transfer(&folders, &peer(), &peer(), Fault::None);
    assert_eq!(served.unwrap(), expected);
    assert_eq!(pulled.unwrap(), expected);
}

#[test]
fn authenticated_wrong_file_hash_leaves_old_install_and_can_retry() {
    let folders = Folders::new();
    folders.old_install();
    let b = publish(&folders, "new", Os::Mac, 123);
    fs::write(folders.path("builds/new/game.bin"), vec![0x88; 123]).unwrap();
    let (served, pulled, _) = transfer(&folders, &peer(), &peer(), Fault::None);
    assert_hub(served);
    assert_hub(pulled);
    folders.check_old();
    fs::write(folders.path("builds/new/game.bin"), vec![0; 123]).unwrap();
    let (_, pulled, _) = transfer(&folders, &peer(), &peer(), Fault::None);
    assert_eq!(pulled.unwrap(), b);
}

#[cfg(unix)]
#[test]
fn symlink_in_build_or_staging_is_refused_without_touching_its_target() {
    use std::os::unix::fs::symlink;
    let folders = Folders::new();
    folders.old_install();
    publish(&folders, "new", Os::Mac, 123);
    let outside = folders.path("untouched");
    fs::write(&outside, vec![0; 123]).unwrap();
    let source = folders.path("builds/new/game.bin");
    fs::remove_file(&source).unwrap();
    symlink(&outside, &source).unwrap();
    let (served, pulled, metrics) = transfer(&folders, &peer(), &peer(), Fault::None);
    assert_hub(served);
    assert_hub(pulled);
    assert_eq!(metrics.frames, 0);
    fs::remove_file(source).unwrap();
    fs::write(folders.path("builds/new/game.bin"), vec![0; 123]).unwrap();
    fs::create_dir(folders.path("staged")).unwrap();
    symlink(&outside, folders.path("staged/game.bin")).unwrap();
    let (_, pulled, metrics) = transfer(&folders, &peer(), &peer(), Fault::None);
    assert_hub(pulled);
    assert_eq!(metrics.chunks, 0);
    assert_eq!(fs::read(outside).unwrap(), vec![0; 123]);
    folders.check_old();
}

#[test]
fn non_home_tcp_address_is_refused() {
    assert_hub(TcpTransport::connect("8.8.8.8:1".parse().unwrap()));
}

#[test]
fn loopback_tcp_streams_a_64_mib_build_with_bounded_buffers() {
    let folders = Folders::new();
    let size = 64 * 1024 * 1024;
    let expected = publish(&folders, "large", Os::Mac, size);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::scope(|scope| {
        let server = scope.spawn(|| {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(60)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(60)))
                .unwrap();
            stream.set_nodelay(true).unwrap();
            let mut transport =
                Inspect::new(TcpTransport::from_stream(stream).unwrap(), Fault::None);
            let b = hub::serve(&mut transport, &peer(), &folders.path("builds"), Os::Mac).unwrap();
            (b, transport.metrics)
        });
        let mut client = TcpTransport::connect(address).unwrap();
        let b = hub::pull(&mut client, &peer(), Os::Mac, &folders.path("staged")).unwrap();
        drop(client);
        let (served, metrics) = server.join().unwrap();
        assert_eq!(b, expected);
        assert_eq!(served, expected);
        assert_eq!(metrics.chunks, 1025);
        assert_eq!(
            metrics.payload_bytes as u64,
            size + b"prepared shader cache".len() as u64
        );
        assert!(metrics.max_io <= CHUNK_SIZE + 16);
    });
    // Check the received large file by streaming too; bundle::verify currently reads it all.
    let mut source = File::open(folders.path("staged/game.bin")).unwrap();
    assert_eq!(source.metadata().unwrap().len(), size);
    let mut hash = Sha256::new();
    io::copy(&mut source, &mut HashWriter(&mut hash)).unwrap();
    assert_eq!(format!("{:x}", hash.finalize()), expected.files[1].sha256);
}

struct HashWriter<'a>(&'a mut Sha256);

impl Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
