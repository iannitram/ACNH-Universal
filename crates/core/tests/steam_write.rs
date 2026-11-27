// Tests for steam::write_shortcut with temporary userdata folders.
use acnh_core::artwork::{self, Art};
use acnh_core::steam::{write_shortcut, Shortcut};
use acnh_core::vdf::{self, Value};
use acnh_core::Error;
use std::path::PathBuf;

fn shortcut(name: &str) -> Shortcut {
    Shortcut {
        name: name.into(),
        exe: "/bin/acnh-universal".into(),
        start_dir: "/home/u".into(),
        launch_options: "play".into(),
        icon: String::new(),
    }
}

fn setup() -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "acnh-steam-write-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let user = root.join("1234");
    std::fs::create_dir_all(user.join("config")).unwrap();
    (root, user)
}

fn count(file: &std::path::Path) -> usize {
    let doc = vdf::read(&std::fs::read(file).unwrap()).unwrap();
    match &doc[0].1 {
        Value::Map(m) => m.len(),
        _ => 0,
    }
}

#[test]
fn new_file_and_artwork() {
    let (root, user) = setup();
    let s = shortcut("ACNH");
    let images = vec![(Art::Grid, vec![1u8, 2, 3])];
    let out = write_shortcut(&root, &s, &images, &|| false).unwrap();
    assert_eq!(out, vec![user.join("config/shortcuts.vdf")]);
    assert_eq!(count(&out[0]), 1);
    assert!(!user.join("config/shortcuts.vdf.bak").exists());
    let paths = artwork::paths(&user, s.app_id());
    assert_eq!(std::fs::read(&paths[0].1).unwrap(), vec![1u8, 2, 3]);
    assert!(!paths[1].1.exists());
}

#[test]
fn existing_file_keeps_others_and_backs_up_then_replaces() {
    let (root, user) = setup();
    let file = user.join("config/shortcuts.vdf");
    write_shortcut(&root, &shortcut("Other"), &[], &|| false).unwrap();
    let old = std::fs::read(&file).unwrap();
    let s = shortcut("ACNH");
    write_shortcut(&root, &s, &[], &|| false).unwrap();
    assert_eq!(
        std::fs::read(user.join("config/shortcuts.vdf.bak")).unwrap(),
        old
    );
    assert_eq!(count(&file), 2);
    write_shortcut(&root, &s, &[], &|| false).unwrap();
    assert_eq!(count(&file), 2);
}

#[test]
fn steam_running_writes_nothing() {
    let (root, user) = setup();
    let err = write_shortcut(&root, &shortcut("ACNH"), &[], &|| true).unwrap_err();
    assert!(matches!(err, Error::Steam(_)));
    assert!(!user.join("config/shortcuts.vdf").exists());
    assert!(!user.join("config/grid").exists());
}

#[test]
fn nul_in_shortcut_field_returns_vdf_error_and_writes_nothing() {
    let (root, user) = setup();
    let err = write_shortcut(&root, &shortcut("AC\0NH"), &[], &|| false).unwrap_err();
    assert_eq!(
        err,
        Error::Steam("shortcuts.vdf text can't hold a NUL".into())
    );
    assert!(!user.join("config/shortcuts.vdf").exists());
}

#[test]
fn nul_refusal_leaves_existing_file_and_backup_alone() {
    let (root, user) = setup();
    let file = user.join("config/shortcuts.vdf");
    let bak = user.join("config/shortcuts.vdf.bak");
    write_shortcut(&root, &shortcut("First"), &[], &|| false).unwrap();
    write_shortcut(&root, &shortcut("Second"), &[], &|| false).unwrap();
    let (file_before, bak_before) = (std::fs::read(&file).unwrap(), std::fs::read(&bak).unwrap());
    assert!(write_shortcut(&root, &shortcut("Th\0ird"), &[], &|| false).is_err());
    assert_eq!(std::fs::read(&file).unwrap(), file_before);
    assert_eq!(std::fs::read(&bak).unwrap(), bak_before);
}

fn packed(name: &str) -> Shortcut {
    Shortcut {
        exe: "/home/u/Applications/ACNH-Universal/acnh-app".into(),
        ..shortcut(name)
    }
}

fn appimage(name: &str) -> Shortcut {
    Shortcut {
        exe: "/home/u/Applications/ACNH.AppImage".into(),
        ..shortcut(name)
    }
}

fn images() -> Vec<(Art, Vec<u8>)> {
    artwork::ALL
        .into_iter()
        .map(|art| (art, vec![7u8]))
        .collect()
}

#[test]
fn re_adding_after_the_launcher_moves_leaves_one_entry_and_no_stale_artwork() {
    let (root, user) = setup();
    let file = user.join("config/shortcuts.vdf");
    write_shortcut(&root, &shortcut("Other"), &[], &|| false).unwrap();
    let old = appimage("ACNH");
    write_shortcut(&root, &old, &images(), &|| false).unwrap();
    write_shortcut(&root, &shortcut("Another"), &[], &|| false).unwrap();
    assert_eq!(count(&file), 3);
    let new = packed("ACNH");
    assert_ne!(old.app_id(), new.app_id());

    write_shortcut(&root, &new, &images(), &|| false).unwrap();

    // The old entry became the new one, where it was; the others stayed.
    let doc = vdf::read(&std::fs::read(&file).unwrap()).unwrap();
    let Value::Map(entries) = &doc[0].1 else {
        panic!("no shortcuts map")
    };
    let ids: Vec<_> = entries
        .iter()
        .map(|(_, entry)| match entry {
            Value::Map(fields) => fields
                .iter()
                .find_map(|(key, value)| (key == "appid").then(|| value.clone()))
                .unwrap(),
            _ => panic!("entry is not a map"),
        })
        .collect();
    assert_eq!(
        ids,
        vec![
            Value::Int(shortcut("Other").app_id()),
            Value::Int(new.app_id()),
            Value::Int(shortcut("Another").app_id()),
        ]
    );
    for (_, path) in artwork::paths(&user, old.app_id()) {
        assert!(!path.exists(), "{}", path.display());
    }
    for (_, path) in artwork::paths(&user, new.app_id()) {
        assert!(path.is_file(), "{}", path.display());
    }
}

#[test]
fn another_game_with_the_same_name_is_not_replaced() {
    let (root, user) = setup();
    let file = user.join("config/shortcuts.vdf");
    let other = Shortcut {
        exe: "/opt/other/game".into(),
        ..shortcut("ACNH")
    };
    write_shortcut(&root, &other, &images(), &|| false).unwrap();
    write_shortcut(&root, &packed("ACNH"), &[], &|| false).unwrap();
    assert_eq!(count(&file), 2);
    assert!(artwork::paths(&user, other.app_id())[0].1.is_file());
}

#[test]
fn the_first_backup_is_kept() {
    let (root, user) = setup();
    let file = user.join("config/shortcuts.vdf");
    write_shortcut(&root, &shortcut("Other"), &[], &|| false).unwrap();
    let first = std::fs::read(&file).unwrap();
    write_shortcut(&root, &appimage("ACNH"), &[], &|| false).unwrap();
    write_shortcut(&root, &packed("ACNH"), &[], &|| false).unwrap();
    write_shortcut(&root, &packed("ACNH"), &[], &|| false).unwrap();
    assert_eq!(
        std::fs::read(user.join("config/shortcuts.vdf.bak")).unwrap(),
        first
    );
}

#[test]
fn an_empty_shortcuts_file_reads_as_empty() {
    let (root, user) = setup();
    let file = user.join("config/shortcuts.vdf");
    std::fs::write(&file, []).unwrap();
    write_shortcut(&root, &shortcut("ACNH"), &[], &|| false).unwrap();
    assert_eq!(count(&file), 1);
    // Nothing worth keeping was there, so it is not kept as the backup.
    assert!(!user.join("config/shortcuts.vdf.bak").exists());
}

#[cfg(unix)]
mod running {
    use acnh_core::steam::steam_running_on;
    use std::os::unix::fs::PermissionsExt;

    fn path_with_pgrep(script: Option<&str>) -> (tempfile::TempDir, std::ffi::OsString) {
        let dir = tempfile::tempdir().unwrap();
        if let Some(script) = script {
            let pgrep = dir.path().join("pgrep");
            std::fs::write(&pgrep, script).unwrap();
            std::fs::set_permissions(&pgrep, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = dir.path().as_os_str().to_owned();
        (dir, path)
    }

    #[test]
    fn a_missing_pgrep_reads_as_running() {
        let (_dir, path) = path_with_pgrep(None);
        assert!(steam_running_on(Some(&path)));
    }

    #[test]
    fn pgrep_that_finds_nothing_reads_as_not_running() {
        let (_dir, path) = path_with_pgrep(Some("#!/bin/sh\nexit 1\n"));
        assert!(!steam_running_on(Some(&path)));
    }

    #[test]
    fn pgrep_that_finds_steam_reads_as_running() {
        let (_dir, path) = path_with_pgrep(Some("#!/bin/sh\nexit 0\n"));
        assert!(steam_running_on(Some(&path)));
    }

    #[test]
    fn pgrep_that_fails_reads_as_running() {
        let (_dir, path) = path_with_pgrep(Some("#!/bin/sh\nexit 2\n"));
        assert!(steam_running_on(Some(&path)));
    }
}
