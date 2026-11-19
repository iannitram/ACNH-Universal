// Locked tests for backup (issue 1.10: island backups before an update, SPEC.md M7).
use acnh_core::backup::{backup_saves, list, prune, stamp, KEEP};
use acnh_core::Error;
use std::fs;
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("acnh-backup-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn put(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

#[test]
fn keep_is_ten() {
    assert_eq!(KEEP, 10);
}

#[test]
fn stamps_are_utc() {
    assert_eq!(stamp(0), "19700101-000000");
    assert_eq!(stamp(1_762_041_600), "20251102-000000");
    assert_eq!(stamp(1_793_620_799), "20261102-115959");
    assert_eq!(stamp(951_782_400), "20000229-000000");
}

#[test]
fn copies_synced_files_only() {
    let d = scratch("copy");
    let (saves, backups) = (d.join("device_save"), d.join("backups"));
    put(&saves, "main.dat", "m");
    put(&saves, "mainHeader.dat", "h");
    put(&saves, "Villager0/personal.dat", "v0");
    put(&saves, "main.dat.tmp", "no");
    put(&saves, "Villager1/photo.bak", "no");
    put(&saves, "backups/old/main.dat", "no");
    let made = backup_saves(&saves, &backups, "20261102-120000").unwrap();
    assert_eq!(made, backups.join("20261102-120000"));
    assert_eq!(fs::read_to_string(made.join("main.dat")).unwrap(), "m");
    assert_eq!(
        fs::read_to_string(made.join("mainHeader.dat")).unwrap(),
        "h"
    );
    assert_eq!(
        fs::read_to_string(made.join("Villager0/personal.dat")).unwrap(),
        "v0"
    );
    assert!(!made.join("main.dat.tmp").exists());
    assert!(!made.join("Villager1/photo.bak").exists());
    assert!(!made.join("backups").exists());
    assert_eq!(fs::read_to_string(saves.join("main.dat")).unwrap(), "m");
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn existing_stamp_is_refused() {
    let d = scratch("exists");
    let (saves, backups) = (d.join("device_save"), d.join("backups"));
    put(&saves, "main.dat", "new");
    put(&backups, "S1/main.dat", "old");
    assert_eq!(
        backup_saves(&saves, &backups, "S1"),
        Err(Error::Saves("backup S1 exists".into()))
    );
    assert_eq!(
        fs::read_to_string(backups.join("S1/main.dat")).unwrap(),
        "old"
    );
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn missing_save_dir() {
    let d = scratch("missing");
    let saves = d.join("nope");
    assert_eq!(
        backup_saves(&saves, &d.join("backups"), "S1"),
        Err(Error::Saves(format!(
            "no save folder at {}",
            saves.display()
        )))
    );
    assert!(!d.join("backups").join("S1").exists());
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn list_oldest_first_folders_only_missing_is_empty() {
    let d = scratch("list");
    let backups = d.join("backups");
    assert_eq!(list(&backups), Ok(vec![]));
    for s in ["20261031-120000", "20261030-090000", "20261101-000000"] {
        fs::create_dir_all(backups.join(s)).unwrap();
    }
    put(&backups, "readme.txt", "not a backup");
    assert_eq!(
        list(&backups).unwrap(),
        ["20261030-090000", "20261031-120000", "20261101-000000"]
    );
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn prune_keeps_newest() {
    let d = scratch("prune");
    let backups = d.join("backups");
    for s in ["A3", "A1", "A4", "A2"] {
        put(&backups, &format!("{s}/main.dat"), s);
    }
    assert_eq!(prune(&backups, 2).unwrap(), ["A1", "A2"]);
    assert_eq!(list(&backups).unwrap(), ["A3", "A4"]);
    assert_eq!(prune(&backups, 5).unwrap(), Vec::<String>::new());
    fs::remove_dir_all(&d).unwrap();
}
