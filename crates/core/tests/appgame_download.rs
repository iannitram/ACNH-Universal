use acnh_core::appgame::{
    download, DownloadTransport, Manifest, ManifestFile, GAME_DIR, PARTIAL_DIR,
};
use acnh_core::error::Error;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use tempfile::TempDir;

const BUILD_ID: &str = "FCD2BB238ABE99E925B4E452DF1F41F1";
const ABI: &str = "0123456789ab";

fn sha256_hex(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

struct FakeTransport {
    manifest: Vec<u8>,
    files: BTreeMap<String, Vec<u8>>,
    /// Optional byte limit for a single `fetch_range` call on a given path, followed by an error.
    fail_after_on: Option<(String, usize)>,
    /// Optional override of bytes returned for a path (simulating corrupt data).
    corrupt_once: Option<(String, Vec<u8>)>,
    corrupt_always: Option<(String, Vec<u8>)>,
    range_calls: Vec<(String, u64)>,
    bytes_sent: u64,
}

impl FakeTransport {
    fn from_files(build_id: &str, abi: &str, entries: Vec<(&str, Vec<u8>)>) -> Self {
        let mut files = BTreeMap::new();
        let mut manifest_files = Vec::new();
        for (path, data) in entries {
            manifest_files.push(ManifestFile {
                path: path.to_owned(),
                size: data.len() as u64,
                sha256: sha256_hex(&data),
            });
            files.insert(path.to_owned(), data);
        }
        let manifest = Manifest {
            version: 1,
            build_id: build_id.to_owned(),
            nx_game_abi: abi.to_owned(),
            files: manifest_files,
        }
        .to_json()
        .into_bytes();
        Self {
            manifest,
            files,
            fail_after_on: None,
            corrupt_once: None,
            corrupt_always: None,
            range_calls: Vec::new(),
            bytes_sent: 0,
        }
    }
}

impl DownloadTransport for FakeTransport {
    fn fetch_manifest(&mut self) -> acnh_core::Result<Vec<u8>> {
        Ok(self.manifest.clone())
    }

    fn fetch_range(
        &mut self,
        path: &str,
        offset: u64,
        out: &mut dyn Write,
    ) -> acnh_core::Result<u64> {
        self.range_calls.push((path.to_owned(), offset));

        if let Some((target, bad)) = &self.corrupt_always {
            if target == path {
                let slice = &bad[offset as usize..];
                out.write_all(slice)?;
                self.bytes_sent += slice.len() as u64;
                return Ok(slice.len() as u64);
            }
        }

        if let Some((target, _)) = &self.corrupt_once {
            if target == path {
                let (_, bad) = self.corrupt_once.take().unwrap();
                let slice = &bad[offset as usize..];
                out.write_all(slice)?;
                self.bytes_sent += slice.len() as u64;
                return Ok(slice.len() as u64);
            }
        }

        let data = self
            .files
            .get(path)
            .ok_or_else(|| Error::Io(format!("missing remote file {path}")))?;
        let slice = &data[offset as usize..];

        if let Some((target, limit)) = &self.fail_after_on {
            if target == path {
                let take = (*limit).min(slice.len());
                out.write_all(&slice[..take])?;
                self.bytes_sent += take as u64;
                self.fail_after_on = None;
                return Err(Error::Io("simulated network drop".into()));
            }
        }

        out.write_all(slice)?;
        self.bytes_sent += slice.len() as u64;
        Ok(slice.len() as u64)
    }
}

fn decoy_entries() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("analysis/main.bin", b"decoy-analysis".to_vec()),
        ("fonts/shared.ttf", b"decoy-font".to_vec()),
        ("libacnh_game.so", b"decoy-elf".to_vec()),
        ("romfs.pak", vec![b'R'; 1024]),
        ("spirv.pak", vec![b'S'; 512]),
    ]
}

#[test]
fn download_full_installs_game_read_only() {
    let tmp = TempDir::new().unwrap();
    let private = tmp.path().join("private");
    let dest = private.join(GAME_DIR);
    let mut transport = FakeTransport::from_files(BUILD_ID, ABI, decoy_entries());

    let out = download(&private, &dest, 1024 * 1024, BUILD_ID, ABI, &mut transport).unwrap();
    assert_eq!(out, dest);
    assert!(!private.join(PARTIAL_DIR).exists());
    assert!(dest.join("manifest.json").is_file());
    for (rel, data) in decoy_entries() {
        let p = dest.join(rel);
        assert_eq!(fs::read(&p).unwrap(), data);
        assert!(fs::metadata(&p).unwrap().permissions().readonly());
    }
}

#[test]
fn download_resumes_after_partial_failure() {
    let tmp = TempDir::new().unwrap();
    let private = tmp.path().join("private");
    let dest = private.join(GAME_DIR);
    let entries = decoy_entries();
    let total_bytes: u64 = entries.iter().map(|(_, d)| d.len() as u64).sum();

    let mut transport = FakeTransport::from_files(BUILD_ID, ABI, entries.clone());
    transport.fail_after_on = Some(("romfs.pak".to_owned(), 300));

    let err = download(&private, &dest, 1024 * 1024, BUILD_ID, ABI, &mut transport).unwrap_err();
    assert!(err.to_string().contains("simulated network drop"));
    assert!(!dest.exists(), "game/ must not exist after partial failure");
    assert!(private.join(PARTIAL_DIR).is_dir());
    let first_bytes = transport.bytes_sent;

    // Second run: should skip completed files and request romfs.pak starting at offset 300.
    transport.range_calls.clear();
    transport.bytes_sent = 0;
    download(&private, &dest, 1024 * 1024, BUILD_ID, ABI, &mut transport).unwrap();

    assert!(dest.is_dir());
    assert!(!private.join(PARTIAL_DIR).exists());
    assert_eq!(first_bytes + transport.bytes_sent, total_bytes);
    assert_eq!(
        transport.range_calls,
        vec![("romfs.pak".to_owned(), 300), ("spirv.pak".to_owned(), 0),]
    );
}

#[test]
fn download_refetches_corrupted_file_and_refuses_persistent_mismatch() {
    let tmp = TempDir::new().unwrap();
    let private = tmp.path().join("private");
    let dest = private.join(GAME_DIR);

    // Corrupt once: recovers on retry.
    let mut recover = FakeTransport::from_files(BUILD_ID, ABI, decoy_entries());
    recover.corrupt_once = Some(("spirv.pak".to_owned(), vec![b'X'; 512]));
    download(&private, &dest, 1024 * 1024, BUILD_ID, ABI, &mut recover).unwrap();
    assert_eq!(fs::read(dest.join("spirv.pak")).unwrap(), vec![b'S'; 512]);
    let spirv_calls: Vec<u64> = recover
        .range_calls
        .iter()
        .filter(|(p, _)| p == "spirv.pak")
        .map(|(_, off)| *off)
        .collect();
    assert_eq!(spirv_calls, vec![0, 0]);

    // Corrupt always: refused after retry, game/ untouched.
    let tmp2 = TempDir::new().unwrap();
    let private2 = tmp2.path().join("private");
    let dest2 = private2.join(GAME_DIR);
    let mut fail = FakeTransport::from_files(BUILD_ID, ABI, decoy_entries());
    fail.corrupt_always = Some(("spirv.pak".to_owned(), vec![b'X'; 512]));
    let err = download(&private2, &dest2, 1024 * 1024, BUILD_ID, ABI, &mut fail).unwrap_err();
    assert!(err.to_string().contains("spirv.pak"));
    assert!(!dest2.exists());
}

#[test]
fn download_refuses_mismatched_build_id_or_abi() {
    let tmp = TempDir::new().unwrap();
    let private = tmp.path().join("private");
    let dest = private.join(GAME_DIR);

    let mut wrong_build = FakeTransport::from_files("OTHER_BUILD", ABI, decoy_entries());
    let err = download(
        &private,
        &dest,
        1024 * 1024,
        BUILD_ID,
        ABI,
        &mut wrong_build,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains(BUILD_ID),
        "should name expected build id: {msg}"
    );
    assert!(
        msg.contains("OTHER_BUILD"),
        "should name actual build id: {msg}"
    );
    assert!(wrong_build.range_calls.is_empty());

    let mut wrong_abi = FakeTransport::from_files(BUILD_ID, "deadbeef0000", decoy_entries());
    let err2 = download(&private, &dest, 1024 * 1024, BUILD_ID, ABI, &mut wrong_abi).unwrap_err();
    let msg2 = err2.to_string();
    assert!(msg2.contains(ABI), "should name expected ABI: {msg2}");
    assert!(
        msg2.contains("deadbeef0000"),
        "should name actual ABI: {msg2}"
    );
    assert!(wrong_abi.range_calls.is_empty());
}

#[test]
fn download_refuses_insufficient_free_space_before_requests() {
    let tmp = TempDir::new().unwrap();
    let private = tmp.path().join("private");
    let dest = private.join(GAME_DIR);
    let mut transport = FakeTransport::from_files(BUILD_ID, ABI, decoy_entries());

    let err = download(&private, &dest, 100, BUILD_ID, ABI, &mut transport).unwrap_err();
    assert!(err.to_string().contains("not enough free space"));
    assert!(transport.range_calls.is_empty());
    assert!(!private.join(PARTIAL_DIR).exists());
    assert!(!dest.exists());
}

#[cfg(feature = "relay")]
#[test]
fn remote_download_transport_downloads_and_reports_missing_manifest() {
    use acnh_core::relay::download_game_from_remote;
    use save_relay::remote::{Precondition, Remote, RemoteEntry};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    struct MemRemote(Mutex<BTreeMap<String, Vec<u8>>>);
    impl Remote for MemRemote {
        fn get(&self, path: &str) -> save_relay::Result<(Vec<u8>, String)> {
            self.0
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .map(|b| (b, "etag".into()))
                .ok_or_else(|| save_relay::Error::NotFound(path.to_owned()))
        }
        fn put(&self, path: &str, data: &[u8], _pre: &Precondition) -> save_relay::Result<String> {
            self.0
                .lock()
                .unwrap()
                .insert(path.to_owned(), data.to_vec());
            Ok("etag".into())
        }
        fn list(&self, _prefix: &str) -> save_relay::Result<Vec<RemoteEntry>> {
            Ok(vec![])
        }
        fn delete(&self, path: &str) -> save_relay::Result<()> {
            self.0.lock().unwrap().remove(path);
            Ok(())
        }
        fn mkcol(&self, _path: &str) -> save_relay::Result<()> {
            Ok(())
        }
        fn copy(&self, _from: &str, _to: &str) -> save_relay::Result<()> {
            Ok(())
        }
        fn move_to(&self, _from: &str, _to: &str) -> save_relay::Result<()> {
            Ok(())
        }
    }

    let tmp = TempDir::new().unwrap();
    let private = tmp.path().join("private");
    let dest = private.join(GAME_DIR);
    let empty_remote: Arc<dyn Remote> = Arc::new(MemRemote(Mutex::new(BTreeMap::new())));
    let err =
        download_game_from_remote(empty_remote, &private, &dest, 1024 * 1024, "linux").unwrap_err();
    assert!(err.to_string().contains("No prepared game"));

    let mut map = BTreeMap::new();
    let mut files = Vec::new();
    for (rel, body) in decoy_entries() {
        let mut hasher = Sha256::new();
        hasher.update(&body);
        let hex: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        files.push(ManifestFile {
            path: rel.to_string(),
            size: body.len() as u64,
            sha256: hex,
        });
        map.insert(format!("acnh-universal-game/linux/{rel}"), body);
    }
    let manifest = Manifest {
        version: 1,
        build_id: BUILD_ID.into(),
        nx_game_abi: ABI.into(),
        files,
    };
    map.insert(
        "acnh-universal-game/linux/manifest.json".into(),
        manifest.to_json().into_bytes(),
    );
    let remote: Arc<dyn Remote> = Arc::new(MemRemote(Mutex::new(map)));
    let installed = download_game_from_remote(remote, &private, &dest, 1024 * 1024, "linux")
        .expect("remote download should succeed");
    assert_eq!(installed, dest);
    assert!(dest.join("analysis/main.bin").is_file());
    assert!(dest.join("libacnh_game.so").is_file());
}
