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

#[test]
fn download_replaces_an_older_game_and_handles_empty_files() {
    let tmp = TempDir::new().unwrap();
    let private = tmp.path().join("private");
    let dest = private.join(GAME_DIR);
    let mut first = FakeTransport::from_files(BUILD_ID, ABI, decoy_entries());
    download(&private, &dest, 1 << 30, BUILD_ID, ABI, &mut first).unwrap();

    let mut newer = decoy_entries();
    newer[3] = ("romfs.pak", vec![b'N'; 300]);
    newer.push(("aoc/empty.bin", Vec::new()));
    let mut second = FakeTransport::from_files(BUILD_ID, ABI, newer);
    download(&private, &dest, 1 << 30, BUILD_ID, ABI, &mut second).unwrap();
    assert_eq!(fs::read(dest.join("romfs.pak")).unwrap(), vec![b'N'; 300]);
    assert_eq!(fs::metadata(dest.join("aoc/empty.bin")).unwrap().len(), 0);
    assert!(!private.join(PARTIAL_DIR).exists());
}

#[test]
fn bad_manifests_are_refused() {
    let hash = "a".repeat(64);
    let entry = |path: &str, sha: &str| format!(r#"{{"path":"{path}","size":1,"sha256":"{sha}"}}"#);
    let manifest = |files: String| {
        format!(
            r#"{{"version":1,"build_id":"{BUILD_ID}","nx_game_abi":"{ABI}","files":[{files}]}}"#
        )
    };
    for bad in [
        manifest(entry("../x", &hash)),
        manifest(entry("/abs", &hash)),
        manifest(entry("", &hash)),
        manifest(entry("a.bin", "zz")),
        manifest(format!(
            "{},{}",
            entry("a.bin", &hash),
            entry("a.bin", &hash)
        )),
    ] {
        let parsed = Manifest::from_json(bad.as_bytes()).and_then(|m| m.validate(BUILD_ID, ABI));
        assert!(parsed.is_err(), "{bad}");
    }
    assert!(Manifest::from_json(b"not json").is_err());
    // No build id or ABI: parses (they default to empty) but never validates.
    assert!(Manifest::from_json(br#"{"version":1,"files":[]}"#)
        .and_then(|m| m.validate(BUILD_ID, ABI))
        .is_err());
    let ok = manifest(entry("a.bin", &hash));
    Manifest::from_json(ok.as_bytes())
        .unwrap()
        .validate(BUILD_ID, ABI)
        .unwrap();
}

fn item(path: &str, data: &[u8]) -> ManifestFile {
    ManifestFile {
        path: path.to_owned(),
        size: data.len() as u64,
        sha256: sha256_hex(data),
    }
}

#[test]
fn the_stepwise_api_applies_the_same_resume_rule() {
    let tmp = TempDir::new().unwrap();
    let private = tmp.path().join("private");
    let partial = private.join(PARTIAL_DIR);
    let whole = vec![7u8; 100];
    let (a, b, c) = (
        item("a.bin", &whole),
        item("sub/b.bin", &whole),
        item("c.bin", &whole),
    );

    // Nothing yet: 0, and game.partial/ exists.
    assert_eq!(acnh_core::appgame::partial_bytes(&private, &a).unwrap(), 0);
    assert!(partial.is_dir());
    fs::create_dir_all(partial.join("sub")).unwrap();
    // A shorter file continues from its length; a whole one with the right hash is complete;
    // a whole one with the wrong hash, and a longer one, start again (and are deleted).
    fs::write(partial.join("a.bin"), &whole[..40]).unwrap();
    fs::write(partial.join("sub/b.bin"), &whole).unwrap();
    fs::write(partial.join("c.bin"), vec![9u8; 100]).unwrap();
    assert_eq!(acnh_core::appgame::partial_bytes(&private, &a).unwrap(), 40);
    assert_eq!(
        acnh_core::appgame::partial_bytes(&private, &b).unwrap(),
        100
    );
    assert_eq!(acnh_core::appgame::partial_bytes(&private, &c).unwrap(), 0);
    assert!(!partial.join("c.bin").exists());
    fs::write(partial.join("c.bin"), vec![9u8; 150]).unwrap();
    assert_eq!(acnh_core::appgame::partial_bytes(&private, &c).unwrap(), 0);
    assert!(!partial.join("c.bin").exists());

    // verify_partial: right is true; wrong is false and deletes the file.
    fs::write(partial.join("a.bin"), &whole).unwrap();
    assert!(acnh_core::appgame::verify_partial(&private, &a).unwrap());
    fs::write(partial.join("c.bin"), vec![9u8; 100]).unwrap();
    assert!(!acnh_core::appgame::verify_partial(&private, &c).unwrap());
    assert!(!partial.join("c.bin").exists());

    // install_partial refuses while a file is missing, then installs read-only with the manifest.
    let manifest = Manifest {
        version: 1,
        build_id: BUILD_ID.to_owned(),
        nx_game_abi: ABI.to_owned(),
        files: vec![a.clone(), b.clone(), c.clone()],
    }
    .to_json()
    .into_bytes();
    let dest = private.join(GAME_DIR);
    assert!(acnh_core::appgame::install_partial(&private, &dest, &manifest).is_err());
    assert!(!dest.exists());
    fs::write(partial.join("c.bin"), &whole).unwrap();
    acnh_core::appgame::install_partial(&private, &dest, &manifest).unwrap();
    assert!(!partial.exists());
    assert_eq!(fs::read(dest.join("manifest.json")).unwrap(), manifest);
    assert_eq!(fs::read(dest.join("sub/b.bin")).unwrap(), whole);
    assert!(fs::metadata(dest.join("a.bin"))
        .unwrap()
        .permissions()
        .readonly());
    // A destination outside the private folder is refused.
    assert!(acnh_core::appgame::install_partial(&private, tmp.path(), &manifest).is_err());
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

#[test]
fn manifest_allows_omitted_build_id_and_abi() {
    let raw = br#"{"version":1,"files":[{"path":"romfs.pak","size":3,"sha256":"0000000000000000000000000000000000000000000000000000000000000000"}]}"#;
    let parsed =
        Manifest::from_json(raw).expect("manifest without build_id/nx_game_abi should parse");
    assert!(parsed.build_id.is_empty());
    assert!(parsed.nx_game_abi.is_empty());
    parsed
        .validate("", "")
        .expect("empty expected build_id/abi validates");
}

#[test]
fn scan_and_write_make_a_manifest_with_a_real_size_and_hash() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("fonts")).unwrap();
    std::fs::write(root.join("romfs.pak"), b"romfs decoy").unwrap();
    std::fs::write(root.join("fonts/a b\u{e9}.ttf"), b"font").unwrap();
    std::fs::write(root.join(".DS_Store"), b"junk").unwrap();
    std::fs::write(root.join("manifest.json"), b"old").unwrap();

    let manifest = Manifest::scan(root, BUILD_ID, ABI).unwrap();
    manifest.write_to(root).unwrap();

    let raw = std::fs::read(root.join("manifest.json")).unwrap();
    let parsed = Manifest::from_json(&raw).unwrap();
    assert_eq!(parsed, manifest);
    parsed.validate(BUILD_ID, ABI).unwrap();
    let paths: Vec<_> = parsed.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["fonts/a b\u{e9}.ttf", "romfs.pak"]);
    let romfs = parsed.files.iter().find(|f| f.path == "romfs.pak").unwrap();
    assert_eq!(romfs.size, 11);
    assert_eq!(romfs.sha256, sha256_hex(b"romfs decoy"));
    assert!(!root.join("manifest.json.tmp").exists());
}

#[test]
fn a_manifest_without_a_build_id_or_abi_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("romfs.pak"), b"x").unwrap();
    assert!(Manifest::scan(dir.path(), "", ABI).is_err());
    assert!(Manifest::scan(dir.path(), BUILD_ID, " ").is_err());
    assert!(Manifest::scan(dir.path(), BUILD_ID, ABI).is_ok());
    let empty = tempfile::tempdir().unwrap();
    assert!(Manifest::scan(empty.path(), BUILD_ID, ABI).is_err());
    assert!(!dir.path().join("manifest.json").exists());
}

#[test]
fn write_refuses_a_manifest_that_does_not_match_the_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("romfs.pak"), b"abc").unwrap();
    let mut manifest = Manifest::scan(dir.path(), BUILD_ID, ABI).unwrap();
    manifest.files[0].size = 99;
    assert!(manifest.write_to(dir.path()).is_err());
    assert!(!dir.path().join("manifest.json").exists());
}
