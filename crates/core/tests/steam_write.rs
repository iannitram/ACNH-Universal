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
