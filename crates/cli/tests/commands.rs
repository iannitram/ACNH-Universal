use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn temp(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("acnh-cli-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'A'..=b'F' => byte - b'A' + 10,
        b'a'..=b'f' => byte - b'a' + 10,
        _ => 0,
    }
}

fn header(build_id: &str) -> Vec<u8> {
    let mut header = vec![0u8; 0x60];
    header[..4].copy_from_slice(b"NSO0");
    for (index, byte) in build_id.bytes().take(32).enumerate() {
        let shift = if index % 2 == 0 { 4 } else { 0 };
        header[0x40 + index / 2] |= nibble(byte) << shift;
    }
    header
}

fn game_folder(name: &str, build_id: &str) -> PathBuf {
    let root = temp(name);
    let exefs = root.join("exefs");
    fs::create_dir_all(&exefs).unwrap();
    fs::write(exefs.join("main"), header(build_id)).unwrap();
    for file in ["sdk", "subsdk0", "main.npdm"] {
        fs::write(exefs.join(file), b"").unwrap();
    }
    fs::create_dir_all(root.join("romfs")).unwrap();
    root
}

fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_acnh-universal"))
        .args(args)
        .output()
        .unwrap();
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn game_path(root: &Path) -> &str {
    root.to_str().unwrap()
}

#[test]
fn check_accepts_a_known_version() {
    let root = game_folder("known", "FCD2BB238ABE99E925B4E452DF1F41F1");
    let (code, out, err) = run(&["check", "--game", game_path(&root)]);
    fs::remove_dir_all(root).unwrap();
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains("3.0.2"), "{out}");
}

#[test]
fn check_names_the_first_missing_file() {
    let root = temp("missing");
    let exefs = root.join("exefs");
    fs::create_dir_all(&exefs).unwrap();
    fs::write(exefs.join("main"), b"").unwrap();
    fs::write(exefs.join("sdk"), b"").unwrap();
    fs::write(exefs.join("subsdk0"), b"").unwrap();
    fs::create_dir_all(root.join("romfs")).unwrap();
    let (code, _, err) = run(&["check", "--game", game_path(&root)]);
    fs::remove_dir_all(root).unwrap();
    assert_eq!(code, Some(1));
    assert!(err.contains("exefs/main.npdm is missing"), "{err}");
}

#[test]
fn check_refuses_another_version_with_the_four_versions() {
    let root = game_folder("wrong", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
    let (code, _, err) = run(&["check", "--game", game_path(&root)]);
    fs::remove_dir_all(root).unwrap();
    assert_eq!(code, Some(1));
    for version in ["3.0.2", "3.0.3", "2.0.7", "1.0.0"] {
        assert!(err.contains(version), "{version} missing from {err}");
    }
}

#[test]
fn json_prints_the_error_code() {
    let root = game_folder("json", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
    let (code, out, _) = run(&["check", "--json", "--game", game_path(&root)]);
    fs::remove_dir_all(root).unwrap();
    assert_eq!(code, Some(1));
    assert!(out.contains("\"wrong_version\""), "{out}");
}

#[test]
fn unknown_commands_exit_2_with_usage() {
    let (code, out, err) = run(&["frobnicate"]);
    assert_eq!(code, Some(2));
    assert!(
        out.contains("usage: acnh-universal") || err.contains("usage: acnh-universal"),
        "{out}{err}"
    );
}

#[test]
fn no_command_exits_2_with_usage() {
    let (code, out, err) = run(&[]);
    assert_eq!(code, Some(2));
    assert!(
        out.contains("usage: acnh-universal") || err.contains("usage: acnh-universal"),
        "{out}{err}"
    );
}

#[cfg(unix)]
#[test]
fn steam_add_writes_the_appimage_shortcut_with_persistent_artwork() {
    use acnh_core::platform::{AppDirs, Os, Vars};
    use acnh_core::{steam, vdf};
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let appimage = root.join("ACNH Universal.AppImage");
    let mount = root.join("mount");
    let assets = mount.join("usr/share/acnh-universal");
    fs::create_dir_all(&assets).unwrap();
    fs::write(&appimage, []).unwrap();
    fs::write(assets.join("acnhrecomp.png"), b"fake icon").unwrap();

    let home = root.join("user");
    let data = root.join("data");
    let vars = Vars::from_lookup(Os::current(), |name| match name {
        "HOME" => Some(home.display().to_string()),
        "XDG_DATA_HOME" => Some(data.display().to_string()),
        _ => None,
    })
    .unwrap();
    let user = steam::userdata_roots(&vars)[0].join("1");
    fs::create_dir_all(&user).unwrap();
    let pgrep = root.join("pgrep");
    fs::write(&pgrep, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(pgrep, fs::Permissions::from_mode(0o755)).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_acnh-universal"))
        .arg("steam-add")
        .env("HOME", &home)
        .env("XDG_DATA_HOME", &data)
        .env("APPIMAGE", &appimage)
        .env("APPDIR", &mount)
        .env("PATH", root)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    drop(fs::remove_dir_all(&mount));
    let icon = AppDirs::new(&vars).data.join("steam-launcher-icon.png");
    assert_eq!(fs::read(&icon).unwrap(), b"fake icon");
    let file = user.join("config/shortcuts.vdf");
    let expected = steam::launcher_shortcut(
        Path::new("unused"),
        Some(&appimage),
        "Animal Crossing: New Horizons",
        &icon,
    );
    let mut shortcuts = vec![];
    steam::upsert(&mut shortcuts, &expected).unwrap();
    assert_eq!(vdf::read(&fs::read(file).unwrap()).unwrap(), shortcuts);
}
