// Locked tests for backup (issue 1.10: island backups before an update, SPEC.md M7).
use acnh_core::backup::{
    backup_before_replacing, backup_saves, backup_saves_pruned, is_stamp, list, prune, stamp, KEEP,
};
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
    for s in [
        "20260103-000000",
        "20260101-000000",
        "20260104-000000",
        "20260102-000000",
    ] {
        put(&backups, &format!("{s}/main.dat"), s);
    }
    assert_eq!(
        prune(&backups, 2).unwrap(),
        ["20260101-000000", "20260102-000000"]
    );
    assert_eq!(
        list(&backups).unwrap(),
        ["20260103-000000", "20260104-000000"]
    );
    assert_eq!(prune(&backups, 5).unwrap(), Vec::<String>::new());
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn only_stamp_shaped_names_count() {
    assert!(is_stamp("20261102-115959"));
    for bad in [
        "1st Isle 00000000",
        "save-link-fix",
        "20261102-115959.auto",
        "20261102-115959-2",
        "2026110-2115959",
        "20261102_115959",
        "",
    ] {
        assert!(!is_stamp(bad), "{bad}");
    }
}

#[test]
fn prune_never_touches_island_folders_or_other_folders() {
    let d = scratch("island");
    let backups = d.join("backups");
    // Names that byte-sort before every stamp: the old prune deleted them first.
    let others = [
        "1st Isle 00000000",
        "!odd 12345678",
        "0 Isle ffffffff",
        "save-link-fix",
    ];
    for o in others {
        put(&backups, &format!("{o}/20260101-000000/main.dat"), "island");
    }
    for i in 0..12 {
        put(
            &backups,
            &format!("202601{:02}-000000/main.dat", i + 1),
            "s",
        );
    }
    let removed = prune(&backups, KEEP).unwrap();
    assert_eq!(removed, ["20260101-000000", "20260102-000000"]);
    assert_eq!(list(&backups).unwrap().len(), KEEP);
    for o in others {
        assert!(
            backups.join(o).join("20260101-000000/main.dat").is_file(),
            "{o}"
        );
    }
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn backup_saves_pruned_keeps_the_newest_ten() {
    let d = scratch("pruned");
    let save = d.join("save");
    put(&save, "main.dat", "x");
    let backups = d.join("backups");
    for i in 0..12u64 {
        backup_saves_pruned(&save, &backups, &stamp(1_762_041_600 + i * 60)).unwrap();
    }
    let names = list(&backups).unwrap();
    assert_eq!(names.len(), KEEP);
    assert_eq!(names.last().unwrap(), &stamp(1_762_041_600 + 11 * 60));
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn replacing_with_no_save_folder_backs_nothing_up() {
    // A Mac that has never played takes the island from the relay with Play anyway.
    let d = scratch("replace-none");
    let (saves, backups) = (d.join("device_save"), d.join("backups"));
    assert_eq!(
        backup_before_replacing(&saves, &backups, "20261126-160000"),
        Ok(None)
    );
    assert!(!backups.exists());
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn replacing_an_island_backs_it_up_first() {
    let d = scratch("replace-some");
    let (saves, backups) = (d.join("device_save"), d.join("backups"));
    put(&saves, "main.dat", "here");
    let made = backup_before_replacing(&saves, &backups, "20261126-160000").unwrap();
    assert_eq!(made, Some(backups.join("20261126-160000")));
    assert_eq!(
        fs::read_to_string(backups.join("20261126-160000/main.dat")).unwrap(),
        "here"
    );
    fs::remove_dir_all(&d).unwrap();
}
