// Locked tests for bundle::install (issue 1.19; M7, M11): verify first, keep the device's
// entries (the island in root/ among them), swap the folders.
use acnh_core::bundle::{install, Bundle, BundleFile};
use acnh_core::platform::Os;
use std::fs;
use std::path::{Path, PathBuf};

const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

#[rustfmt::skip]
fn bundle() -> Bundle {
    let f = BundleFile { path: "acnh".into(), size: 3, sha256: ABC.into() };
    Bundle { target: Os::Linux, version: "3.0.2".into(), commit: "c".into(), build_id: "x".into(), files: vec![f] }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("acnh-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn put(dir: &Path, parts: &[&str], data: &str) {
    let mut p = dir.to_path_buf();
    for x in parts {
        p.push(x);
    }
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, data).unwrap();
}

fn get(dir: &Path, parts: &[&str]) -> Option<String> {
    let mut p = dir.to_path_buf();
    for x in parts {
        p.push(x);
    }
    fs::read_to_string(p).ok()
}

#[test]
fn with_no_old_install_staged_is_just_renamed() {
    let base = tmp("inst-fresh");
    let (staged, dir) = (base.join("staged"), base.join("install"));
    put(&staged, &["acnh"], "abc");
    let r = install(&bundle(), &staged, &dir);
    let got = get(&dir, &["acnh"]);
    let (staged_left, old_left) = (staged.exists(), base.join("install.old").exists());
    fs::remove_dir_all(&base).unwrap();
    assert!(r.is_ok());
    assert_eq!(got.as_deref(), Some("abc"));
    assert!(!staged_left && !old_left);
}

#[test]
fn replaces_the_old_build_and_keeps_device_owned_entries() {
    let base = tmp("inst-swap");
    let (staged, dir) = (base.join("staged"), base.join("install"));
    put(&staged, &["acnh"], "abc");
    put(&dir, &["acnh"], "old build");
    put(&dir, &["stale.bin"], "gone");
    put(
        &dir,
        &["root", "user", "device_save", "main.dat"],
        "my island",
    );
    put(&dir, &["root", "config.ini"], "video.frame_cap = 30");
    put(&dir, &["root", "mods", "ptbr", "a.msbt"], "mod");
    put(&dir, &["logs", "last.log"], "log");
    let r = install(&bundle(), &staged, &dir);
    let seen = [
        get(&dir, &["acnh"]),
        get(&dir, &["stale.bin"]),
        get(&dir, &["root", "user", "device_save", "main.dat"]),
        get(&dir, &["root", "config.ini"]),
        get(&dir, &["root", "mods", "ptbr", "a.msbt"]),
        get(&dir, &["logs", "last.log"]),
    ];
    let (staged_left, old_left) = (staged.exists(), base.join("install.old").exists());
    fs::remove_dir_all(&base).unwrap();
    assert!(r.is_ok());
    let want = [
        Some("abc"),
        None,
        Some("my island"),
        Some("video.frame_cap = 30"),
        Some("mod"),
        Some("log"),
    ];
    assert_eq!(seen.each_ref().map(|s| s.as_deref()), want);
    assert!(!staged_left && !old_left);
}

#[test]
fn an_old_install_missing_some_device_owned_entries_is_fine() {
    let base = tmp("inst-part");
    let (staged, dir) = (base.join("staged"), base.join("install"));
    put(&staged, &["acnh"], "abc");
    put(&dir, &["root", "user", "device_save", "main.dat"], "s");
    let r = install(&bundle(), &staged, &dir);
    let seen = [
        get(&dir, &["root", "user", "device_save", "main.dat"]),
        get(&dir, &["logs", "last.log"]),
    ];
    fs::remove_dir_all(&base).unwrap();
    assert!(r.is_ok());
    assert_eq!(seen.each_ref().map(|s| s.as_deref()), [Some("s"), None]);
}

#[test]
fn a_leftover_old_folder_is_removed_first() {
    let base = tmp("inst-left");
    let (staged, dir) = (base.join("staged"), base.join("install"));
    put(&staged, &["acnh"], "abc");
    put(&dir, &["acnh"], "old build");
    put(&base.join("install.old"), &["junk"], "from an earlier run");
    let r = install(&bundle(), &staged, &dir);
    let (got, old_left) = (get(&dir, &["acnh"]), base.join("install.old").exists());
    fs::remove_dir_all(&base).unwrap();
    assert!(r.is_ok());
    assert_eq!(got.as_deref(), Some("abc"));
    assert!(!old_left);
}

#[test]
fn a_failed_verify_changes_nothing() {
    let base = tmp("inst-bad");
    let (staged, dir) = (base.join("staged"), base.join("install"));
    put(&staged, &["acnh"], "abd");
    put(&dir, &["acnh"], "old build");
    put(&dir, &["root", "user", "device_save", "main.dat"], "s");
    let err = install(&bundle(), &staged, &dir).unwrap_err();
    let seen = [
        get(&staged, &["acnh"]),
        get(&dir, &["acnh"]),
        get(&dir, &["root", "user", "device_save", "main.dat"]),
    ];
    let old_left = base.join("install.old").exists();
    fs::remove_dir_all(&base).unwrap();
    assert_eq!(err.to_string(), "Build hub: acnh does not match the bundle");
    assert_eq!(
        seen.each_ref().map(|s| s.as_deref()),
        [Some("abd"), Some("old build"), Some("s")]
    );
    assert!(!old_left);
}
