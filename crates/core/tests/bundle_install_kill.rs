// Interrupted installs (issue 1.42): a kill between any two renames of `bundle::install`
// must leave the island in the old install or the new one, and the next run recovers.
use acnh_core::bundle::{install, recover_install, Bundle, BundleFile};
use acnh_core::platform::Os;
use std::fs;
use std::path::{Path, PathBuf};

const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
const SAVE: &str = "root/user/device_save/main.dat";

fn bundle() -> Bundle {
    let f = BundleFile {
        path: "acnh".into(),
        size: 3,
        sha256: ABC.into(),
    };
    Bundle {
        target: Os::Linux,
        version: "3.0.2".into(),
        commit: "c".into(),
        build_id: "x".into(),
        files: vec![f],
    }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("acnh-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn put(dir: &Path, rel: &str, data: &str) {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, data).unwrap();
}

fn get(dir: &Path, rel: &str) -> Option<String> {
    fs::read_to_string(dir.join(rel)).ok()
}

/// The folders as they are when `install` was killed after `step` of its renames
/// (0: none, 1: install -> install.old, 2: staged -> install, 3: root moved across).
fn killed_after(base: &Path, step: u32) -> PathBuf {
    let (staged, dir, old) = (
        base.join("staged"),
        base.join("install"),
        base.join("install.old"),
    );
    put(&staged, "acnh", "abc");
    match step {
        0 => {
            put(&dir, "acnh", "old build");
            put(&dir, SAVE, "island");
            put(&dir, "logs/last.log", "log");
        }
        1 => {
            put(&old, "acnh", "old build");
            put(&old, SAVE, "island");
            put(&old, "logs/last.log", "log");
        }
        2 => {
            fs::rename(&staged, &dir).unwrap();
            put(&old, "acnh", "old build");
            put(&old, SAVE, "island");
            put(&old, "logs/last.log", "log");
        }
        _ => {
            fs::rename(&staged, &dir).unwrap();
            put(&dir, SAVE, "island");
            put(&old, "acnh", "old build");
            put(&old, "logs/last.log", "log");
        }
    }
    dir
}

#[test]
fn a_kill_between_any_two_renames_loses_nothing_and_the_next_install_recovers() {
    for step in 0..=3 {
        let base = tmp(&format!("kill-{step}"));
        let dir = killed_after(&base, step);
        let staged = base.join("staged");
        if !staged.exists() {
            // The new build was already swapped in; the next pull stages it again.
            put(&staged, "acnh", "abc");
        }
        install(&bundle(), &staged, &dir).unwrap();
        let seen = (
            get(&dir, "acnh"),
            get(&dir, SAVE),
            get(&dir, "logs/last.log"),
            base.join("install.old").exists(),
        );
        fs::remove_dir_all(&base).unwrap();
        let want = (
            Some("abc".to_string()),
            Some("island".to_string()),
            Some("log".to_string()),
            false,
        );
        assert_eq!(seen, want, "step {step}");
    }
}

#[test]
fn recover_install_without_a_new_install_restores_the_old_one() {
    let base = tmp("rec-restore");
    let dir = killed_after(&base, 1);
    recover_install(&dir).unwrap();
    let seen = (get(&dir, "acnh"), get(&dir, SAVE));
    let old = base.join("install.old").exists();
    fs::remove_dir_all(&base).unwrap();
    assert_eq!(seen, (Some("old build".into()), Some("island".into())));
    assert!(!old);
}

#[test]
fn recover_install_moves_the_island_into_the_new_install() {
    let base = tmp("rec-move");
    let dir = killed_after(&base, 2);
    recover_install(&dir).unwrap();
    let seen = (
        get(&dir, "acnh"),
        get(&dir, SAVE),
        get(&dir, "logs/last.log"),
    );
    let old = base.join("install.old").exists();
    fs::remove_dir_all(&base).unwrap();
    let want = (
        Some("abc".to_string()),
        Some("island".to_string()),
        Some("log".to_string()),
    );
    assert_eq!(seen, want);
    assert!(!old);
}

#[test]
fn an_island_in_both_is_kept_not_deleted() {
    let base = tmp("rec-both");
    let dir = killed_after(&base, 2);
    put(&dir, SAVE, "fresh island made after the kill");
    recover_install(&dir).unwrap();
    let seen = (
        get(&dir, SAVE),
        get(&dir, "root.recovered-1/user/device_save/main.dat"),
    );
    fs::remove_dir_all(&base).unwrap();
    let want = (
        Some("fresh island made after the kill".to_string()),
        Some("island".to_string()),
    );
    assert_eq!(seen, want);
}

#[test]
fn recover_without_an_old_folder_does_nothing() {
    let base = tmp("rec-none");
    let dir = base.join("install");
    put(&dir, "acnh", "x");
    recover_install(&dir).unwrap();
    let got = get(&dir, "acnh");
    fs::remove_dir_all(&base).unwrap();
    assert_eq!(got.as_deref(), Some("x"));
}
