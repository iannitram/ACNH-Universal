// Locked tests for saves (issue 1.08: where the island lives, what syncs, the save-relay
// manifest; SPEC.md M6).
use acnh_core::saves::{backups_dir, check_island, is_synced, relay_manifest, save_dir, REQUIRED};
use acnh_core::Error;
use std::fs;
use std::path::Path;

#[test]
fn folders_under_the_data_root() {
    let root = Path::new("/d/root");
    assert_eq!(save_dir(root), root.join("user").join("device_save"));
    assert_eq!(backups_dir(root), root.join("user").join("backups"));
}

#[test]
fn required_files_in_order() {
    assert_eq!(
        REQUIRED,
        ["main.dat", "mainHeader.dat", "Villager0/personal.dat"]
    );
}

#[test]
fn what_syncs() {
    for p in [
        "main.dat",
        "mainHeader.dat",
        "Villager0/personal.dat",
        "Villager7/photo_studio_island.dat",
        "a.dat.tmp2",
    ] {
        assert!(is_synced(p), "{p}");
    }
    for p in [
        "",
        "/main.dat",
        "main.dat.tmp",
        "Villager0/x.bak",
        "main.dat~",
        "backups/x/main.dat",
        "a/backups/b",
        "../main.dat",
        "Villager0/../main.dat",
    ] {
        assert!(!is_synced(p), "{p}");
    }
}

#[test]
fn check_island_in_order() {
    let d = std::env::temp_dir().join(format!("acnh-saves-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    let missing = check_island(&d);
    fs::create_dir_all(d.join("Villager0")).unwrap();
    let mut steps = vec![check_island(&d)];
    fs::write(d.join("main.dat"), b"").unwrap();
    steps.push(check_island(&d));
    fs::write(d.join("mainHeader.dat"), b"").unwrap();
    steps.push(check_island(&d));
    fs::write(d.join("Villager0/personal.dat"), b"").unwrap();
    steps.push(check_island(&d));
    fs::remove_dir_all(&d).unwrap();
    assert_eq!(
        missing,
        Err(Error::Saves(format!("no island at {}", d.display())))
    );
    assert_eq!(
        steps,
        [
            Err(Error::Saves("main.dat missing".into())),
            Err(Error::Saves("mainHeader.dat missing".into())),
            Err(Error::Saves("Villager0/personal.dat missing".into())),
            Ok(()),
        ]
    );
}

#[test]
fn relay_manifest_is_exact() {
    assert_eq!(
        relay_manifest(),
        concat!(
            r#"{"format":1,"id":"acnh","name":"Animal Crossing: New Horizons","saves":{},"#,
            r#""saves_from":"launcher","include":["**"],"#,
            r#""exclude":["**/*.tmp","**/*.bak","**/*~","backups/**"]}"#,
            "\n"
        )
    );
}
