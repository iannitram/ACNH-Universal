//! `appgame::download` (issues #230 and #231): a fake transport and decoy files (made-up bytes,
//! never game data) cover the whole job: install, resume, hash refusal, build and ABI refusal,
//! and the free-space refusal before any request.

use acnh_core::appgame::download::{
    download, install, plan, remove_all, verified, DownloadError, Expect, Manifest, Outcome,
    Transport, GAME_DIR, PARTIAL_DIR,
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

const ABI: &str = "abcdef012345";
const BUILD: &str = "0123456789ABCDEF0123456789ABCDEF";

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The decoy game: name -> bytes.
fn decoys() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("analysis/main.bin", vec![1u8; 3000]),
        ("romfs.pak", (0..9000u32).map(|i| (i % 251) as u8).collect()),
        ("spirv.pak", vec![7u8; 1500]),
        ("fonts/a.ttf", vec![9u8; 10]),
        ("aoc/1/empty", Vec::new()),
    ]
}

fn manifest_json(files: &[(&str, Vec<u8>)], build: Option<&str>, abi: Option<&str>) -> Vec<u8> {
    let list: Vec<String> = files
        .iter()
        .map(|(p, b)| {
            format!(
                r#"{{"path":"{p}","size":{},"sha256":"{}"}}"#,
                b.len(),
                sha(b)
            )
        })
        .collect();
    let mut head = String::from(r#"{"version":1"#);
    if let Some(b) = build {
        head += &format!(r#","build_id":"{b}""#);
    }
    if let Some(a) = abi {
        head += &format!(r#","abi":"{a}""#);
    }
    format!(r#"{head},"files":[{}]}}"#, list.join(",")).into_bytes()
}

struct Fake {
    manifest: Vec<u8>,
    files: HashMap<String, Vec<u8>>,
    /// Requests as (path, offset), and the bytes asked for in all.
    requests: Vec<(String, u64)>,
    bytes_asked: u64,
    /// Stop after writing this many bytes of the named file (a connection that drops).
    fail_after: Option<(String, usize)>,
    /// Serve these bytes instead for the named file, this many times (a corrupted download).
    corrupt: Option<(String, u32)>,
}

impl Fake {
    fn new(files: &[(&str, Vec<u8>)], manifest: Vec<u8>) -> Fake {
        Fake {
            manifest,
            files: files
                .iter()
                .map(|(p, b)| (p.to_string(), b.clone()))
                .collect(),
            requests: Vec::new(),
            bytes_asked: 0,
            fail_after: None,
            corrupt: None,
        }
    }
}

impl Transport for Fake {
    fn manifest(&mut self) -> Result<Vec<u8>, String> {
        Ok(self.manifest.clone())
    }
    fn fetch(&mut self, path: &str, offset: u64, sink: &mut dyn Write) -> Result<(), String> {
        self.requests.push((path.to_string(), offset));
        let mut body = self.files.get(path).ok_or("404")?.clone();
        if let Some((name, times)) = &mut self.corrupt {
            if name == path && *times > 0 {
                *times -= 1;
                if let Some(first) = body.first_mut() {
                    *first ^= 0xff;
                }
            }
        }
        let rest = &body[offset as usize..];
        self.bytes_asked += rest.len() as u64;
        if let Some((name, after)) = &self.fail_after {
            if name == path {
                let cut = (*after).min(rest.len());
                sink.write_all(&rest[..cut]).map_err(|e| e.to_string())?;
                self.fail_after = None;
                return Err("the connection dropped".into());
            }
        }
        sink.write_all(rest).map_err(|e| e.to_string())
    }
}

fn expect() -> Expect {
    Expect {
        build_ids: vec![BUILD.into()],
        abi: ABI.into(),
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                out.push(p.strip_prefix(base).unwrap().to_string_lossy().into_owned());
            }
        }
    }
    walk(dir, dir, &mut out);
    out.sort();
    out
}

#[test]
fn a_full_download_installs_game() {
    let root = tempfile::tempdir().unwrap();
    let files = decoys();
    let mut fake = Fake::new(&files, manifest_json(&files, Some(BUILD), Some(ABI)));
    let out = download(&mut fake, root.path(), &expect(), u64::MAX, true).unwrap();
    assert_eq!(out, Outcome::Installed);
    let game = root.path().join(GAME_DIR);
    assert!(!root.path().join(PARTIAL_DIR).exists());
    for (p, b) in &files {
        assert_eq!(&std::fs::read(game.join(p)).unwrap(), b, "{p}");
    }
    assert!(game.join("manifest.json").is_file());
    // Read-only once complete.
    assert!(std::fs::metadata(game.join("romfs.pak"))
        .unwrap()
        .permissions()
        .readonly());
    // The same manifest again changes nothing and asks for nothing.
    let asked = fake.bytes_asked;
    let again = download(&mut fake, root.path(), &expect(), 0, true).unwrap();
    assert_eq!(again, Outcome::AlreadyInstalled);
    assert_eq!(fake.bytes_asked, asked);
    remove_all(root.path()).unwrap();
}

#[test]
fn a_failure_partway_leaves_no_game_and_the_second_run_fetches_only_what_is_missing() {
    let root = tempfile::tempdir().unwrap();
    let files = decoys();
    let mut fake = Fake::new(&files, manifest_json(&files, Some(BUILD), Some(ABI)));
    fake.fail_after = Some(("romfs.pak".into(), 4000));
    let err = download(&mut fake, root.path(), &expect(), u64::MAX, false).unwrap_err();
    assert!(matches!(err, DownloadError::Transport(_)));
    assert!(
        !root.path().join(GAME_DIR).exists(),
        "no game/ after a failure"
    );
    let first_run_bytes = fake.bytes_asked;
    assert!(first_run_bytes < 3000 + 9000 + 1500 + 10);

    // Second run: what is complete is not asked for again; romfs.pak resumes at 4000.
    let before = fake.requests.len();
    let bytes_before = fake.bytes_asked;
    let out = download(&mut fake, root.path(), &expect(), u64::MAX, false).unwrap();
    assert_eq!(out, Outcome::Installed);
    let second: Vec<_> = fake.requests[before..].to_vec();
    assert!(
        second.contains(&("romfs.pak".to_string(), 4000)),
        "{second:?}"
    );
    assert!(
        !second.iter().any(|(p, _)| p == "analysis/main.bin"),
        "{second:?}"
    );
    // Left to fetch: the other 5000 bytes of romfs.pak, spirv.pak, fonts/a.ttf.
    assert_eq!(fake.bytes_asked - bytes_before, 5000 + 1500 + 10);
    assert!(root.path().join(GAME_DIR).join("romfs.pak").is_file());
}

#[test]
fn a_wrong_hash_is_fetched_again_and_then_refused() {
    let root = tempfile::tempdir().unwrap();
    let files = decoys();
    // Corrupted once: the second fetch is right, the game installs.
    let mut once = Fake::new(&files, manifest_json(&files, Some(BUILD), Some(ABI)));
    once.corrupt = Some(("spirv.pak".into(), 1));
    download(&mut once, root.path(), &expect(), u64::MAX, false).unwrap();
    let spirv: Vec<_> = once
        .requests
        .iter()
        .filter(|(p, _)| p == "spirv.pak")
        .collect();
    assert_eq!(spirv.len(), 2, "fetched again after the wrong hash");
    assert_eq!(spirv[1].1, 0, "from the start");
    remove_all(root.path()).unwrap();

    // Corrupted every time: refused, no game/.
    let root = tempfile::tempdir().unwrap();
    let mut always = Fake::new(&files, manifest_json(&files, Some(BUILD), Some(ABI)));
    always.corrupt = Some(("spirv.pak".into(), 10));
    let err = download(&mut always, root.path(), &expect(), u64::MAX, false).unwrap_err();
    assert_eq!(err, DownloadError::BadHash("spirv.pak".into()));
    assert!(!root.path().join(GAME_DIR).exists());
    assert!(!root.path().join(PARTIAL_DIR).join("spirv.pak").exists());
}

#[test]
fn another_build_id_or_abi_is_refused_naming_both() {
    let root = tempfile::tempdir().unwrap();
    let files = decoys();
    let other_build = "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF";
    let mut fake = Fake::new(
        &files,
        manifest_json(&files, Some(other_build), Some("000000000000")),
    );
    let err = download(&mut fake, root.path(), &expect(), u64::MAX, false).unwrap_err();
    let text = err.to_string();
    for part in [other_build, "000000000000", BUILD, ABI] {
        assert!(text.contains(part), "{text} should name {part}");
    }
    assert!(fake.requests.is_empty(), "no file was requested");
    assert!(!root.path().join(GAME_DIR).exists());

    // Either one alone is enough to refuse.
    let mut abi_only = Fake::new(
        &files,
        manifest_json(&files, Some(BUILD), Some("000000000000")),
    );
    assert!(matches!(
        download(&mut abi_only, root.path(), &expect(), u64::MAX, false),
        Err(DownloadError::Refused { .. })
    ));
    let mut build_only = Fake::new(&files, manifest_json(&files, Some(other_build), Some(ABI)));
    assert!(matches!(
        download(&mut build_only, root.path(), &expect(), u64::MAX, false),
        Err(DownloadError::Refused { .. })
    ));
}

#[test]
fn too_little_free_space_is_refused_before_any_request() {
    let root = tempfile::tempdir().unwrap();
    let files = decoys();
    let mut fake = Fake::new(&files, manifest_json(&files, Some(BUILD), Some(ABI)));
    let err = download(&mut fake, root.path(), &expect(), 100, false).unwrap_err();
    assert_eq!(
        err,
        DownloadError::NoSpace {
            needed: 3000 + 9000 + 1500 + 10,
            free: 100
        }
    );
    assert!(fake.requests.is_empty());
    assert!(!root.path().join(GAME_DIR).exists());
}

#[test]
fn space_counts_only_what_is_still_to_fetch() {
    let root = tempfile::tempdir().unwrap();
    let files = decoys();
    let mut fake = Fake::new(&files, manifest_json(&files, Some(BUILD), Some(ABI)));
    fake.fail_after = Some(("romfs.pak".into(), 8000));
    download(&mut fake, root.path(), &expect(), u64::MAX, false).unwrap_err();
    // Left: 1000 of romfs.pak, spirv.pak 1500 and the 10-byte font: 2510 bytes.
    let mut resume = Fake::new(&files, manifest_json(&files, Some(BUILD), Some(ABI)));
    download(&mut resume, root.path(), &expect(), 2510, false).unwrap();
    assert!(root.path().join(GAME_DIR).join("romfs.pak").is_file());
}

#[test]
fn leftovers_and_bad_partials_are_cleaned_by_the_plan() {
    let root = tempfile::tempdir().unwrap();
    let files = decoys();
    let manifest = Manifest::parse(&manifest_json(&files, None, None)).unwrap();
    let partial = root.path().join(PARTIAL_DIR);
    std::fs::create_dir_all(partial.join("fonts")).unwrap();
    std::fs::write(partial.join("stale.bin"), b"left from another build").unwrap();
    std::fs::create_dir_all(partial.join("analysis")).unwrap();
    // The right size, the wrong bytes: dropped. Too long: dropped. Short: kept.
    std::fs::write(partial.join("analysis/main.bin"), vec![2u8; 3000]).unwrap();
    std::fs::write(partial.join("spirv.pak"), vec![7u8; 9999]).unwrap();
    std::fs::write(partial.join("romfs.pak"), &files[1].1[..100]).unwrap();
    let p = plan(root.path(), &manifest).unwrap();
    let have: HashMap<_, _> = p.items.iter().map(|i| (i.path.as_str(), i.have)).collect();
    assert_eq!(have["analysis/main.bin"], 0);
    assert_eq!(have["spirv.pak"], 0);
    assert_eq!(have["romfs.pak"], 100);
    assert_eq!(have["aoc/1/empty"], 0);
    assert!(p
        .items
        .iter()
        .find(|i| i.path == "aoc/1/empty")
        .unwrap()
        .complete());
    assert!(!partial.join("stale.bin").exists());
    assert_eq!(p.total, 3000 + 9000 + 1500 + 10);
}

#[test]
fn the_steps_install_only_what_was_checked() {
    // The iOS route: the platform moves the bytes, the core checks and installs.
    let root = tempfile::tempdir().unwrap();
    let files = decoys();
    let bytes = manifest_json(&files, Some(BUILD), Some(ABI));
    let manifest = Manifest::parse(&bytes).unwrap();
    plan(root.path(), &manifest).unwrap();
    let partial = root.path().join(PARTIAL_DIR);
    for (p, b) in &files {
        let local = partial.join(p);
        std::fs::create_dir_all(local.parent().unwrap()).unwrap();
        std::fs::write(local, b).unwrap();
    }
    // Nothing is checked yet: refused.
    assert!(install(root.path(), &bytes, false).is_err());
    assert!(!root.path().join(GAME_DIR).exists());
    for f in &manifest.files {
        assert!(verified(root.path(), f).unwrap());
    }
    install(root.path(), &bytes, false).unwrap();
    assert_eq!(
        names(&root.path().join(GAME_DIR)),
        vec![
            "analysis/main.bin",
            "aoc/1/empty",
            "fonts/a.ttf",
            "manifest.json",
            "romfs.pak",
            "spirv.pak"
        ]
    );
    // A wrong file is deleted and reported.
    let root2 = tempfile::tempdir().unwrap();
    plan(root2.path(), &manifest).unwrap();
    let bad = root2.path().join(PARTIAL_DIR).join("spirv.pak");
    std::fs::write(&bad, vec![0u8; 1500]).unwrap();
    let spirv = manifest
        .files
        .iter()
        .find(|f| f.path == "spirv.pak")
        .unwrap();
    assert!(!verified(root2.path(), spirv).unwrap());
    assert!(!bad.exists());
}

#[test]
fn an_update_replaces_the_old_game() {
    let root = tempfile::tempdir().unwrap();
    let files = decoys();
    let mut fake = Fake::new(&files, manifest_json(&files, Some(BUILD), Some(ABI)));
    download(&mut fake, root.path(), &expect(), u64::MAX, true).unwrap();
    let mut newer = decoys();
    newer[2].1 = vec![8u8; 1600];
    let mut fake2 = Fake::new(&newer, manifest_json(&newer, Some(BUILD), Some(ABI)));
    download(&mut fake2, root.path(), &expect(), u64::MAX, true).unwrap();
    assert_eq!(
        std::fs::read(root.path().join(GAME_DIR).join("spirv.pak"))
            .unwrap()
            .len(),
        1600
    );
    assert!(!root.path().join("game.old").exists());
    remove_all(root.path()).unwrap();
}

#[test]
fn bad_manifests_are_refused() {
    for (why, text) in [
        ("not json", "nope".to_string()),
        ("no files", r#"{"version":1}"#.to_string()),
        ("empty list", r#"{"version":1,"files":[]}"#.to_string()),
        (
            "dotdot",
            format!(
                r#"{{"files":[{{"path":"../x","size":1,"sha256":"{}"}}]}}"#,
                sha(b"x")
            ),
        ),
        (
            "absolute",
            format!(
                r#"{{"files":[{{"path":"/x","size":1,"sha256":"{}"}}]}}"#,
                sha(b"x")
            ),
        ),
        (
            "short hash",
            r#"{"files":[{"path":"x","size":1,"sha256":"ab"}]}"#.to_string(),
        ),
    ] {
        assert!(Manifest::parse(text.as_bytes()).is_err(), "{why}");
    }
    // The Android manifest as staged today has no build id or ABI and still parses.
    let files = decoys();
    let m = Manifest::parse(&manifest_json(&files, None, None)).unwrap();
    assert_eq!((m.build_id, m.abi), (None, None));
}
