// Locked tests for steam: CRC-32, shortcut ids and Steam's user folders (SPEC.md M2).
use acnh_core::platform::Vars;
use acnh_core::steam::{crc32, rungameid, shortcut_app_id, userdata_roots, Shortcut};
use std::path::PathBuf;

#[test]
fn crc32_check_values() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(b""), 0);
    assert_eq!(
        crc32(b"The quick brown fox jumps over the lazy dog"),
        0x414F_A339
    );
}

const EXE: &str = "\"/home/u/.local/bin/acnh-universal\"";
const NAME: &str = "Animal Crossing: New Horizons";

#[test]
fn app_id_sets_the_high_bit() {
    // crc32 of EXE + NAME is 0xA57E4FE0 (its high bit is already set)
    assert_eq!(shortcut_app_id(EXE, NAME), 0xA57E_4FE0);
    assert!(shortcut_app_id("", "") & 0x8000_0000 != 0);
}

#[test]
fn rungameid_is_app_id_shifted() {
    assert_eq!(rungameid(0xA57E_4FE0), 11_925_056_686_848_475_136);
    assert_eq!(rungameid(0x8000_0000), 0x8000_0000_0200_0000);
}

#[test]
fn shortcut_quotes_exe() {
    let s = Shortcut {
        name: NAME.into(),
        exe: "/home/u/.local/bin/acnh-universal".into(),
        start_dir: "/home/u".into(),
        launch_options: "play".into(),
        icon: String::new(),
    };
    assert_eq!(s.quoted_exe(), EXE);
    assert_eq!(s.app_id(), 0xA57E_4FE0);
}

#[test]
fn userdata_roots_in_order() {
    let v = Vars {
        home: PathBuf::from("/home/u"),
        data: PathBuf::from("/home/u/.local/share"),
        config: PathBuf::from("/home/u/.config"),
    };
    let h = PathBuf::from("/home/u");
    let want = if cfg!(target_os = "windows") {
        vec![PathBuf::from(r"C:\Program Files (x86)\Steam\userdata")]
    } else if cfg!(target_os = "macos") {
        vec![h.join("Library/Application Support/Steam/userdata")]
    } else {
        vec![
            h.join(".local/share/Steam/userdata"),
            h.join(".steam/steam/userdata"),
            h.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/userdata"),
        ]
    };
    assert_eq!(userdata_roots(&v), want);
}
