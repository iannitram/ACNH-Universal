// Locked tests for gamefolder::check (issue 1.04: the game folder layout, SPEC.md M1). The
// folders are made up here: empty files and a made-up NSO header; no game data.
use acnh_core::gamefolder::{check, EXEFS_FILES};
use acnh_core::version::Version;
use acnh_core::Error;
use std::fs;
use std::path::{Path, PathBuf};

// The 3.0.2 build id as header bytes 0x40..0x50.
const ID_302: [u8; 16] = [
    0xFC, 0xD2, 0xBB, 0x23, 0x8A, 0xBE, 0x99, 0xE9, 0x25, 0xB4, 0xE4, 0x52, 0xDF, 0x1F, 0x41, 0xF1,
];

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("acnh-game-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn main_header(id: [u8; 16]) -> Vec<u8> {
    let mut h = vec![0u8; 0x60];
    h[..4].copy_from_slice(b"NSO0");
    h[0x40..0x50].copy_from_slice(&id);
    h
}

// A folder that passes: exefs with all four files, romfs/, main for 3.0.2.
fn good(root: &Path) {
    fs::create_dir_all(root.join("exefs")).unwrap();
    for name in EXEFS_FILES {
        fs::write(root.join("exefs").join(name), b"").unwrap();
    }
    fs::write(root.join("exefs/main"), main_header(ID_302)).unwrap();
    fs::create_dir_all(root.join("romfs")).unwrap();
}

fn bad(root: &Path, msg: &str) {
    assert_eq!(check(root), Err(Error::BadGameFolder(msg.to_string())));
}

#[test]
fn exefs_files_in_order() {
    assert_eq!(EXEFS_FILES, ["main", "sdk", "subsdk0", "main.npdm"]);
}

#[test]
fn a_good_folder() {
    let d = tmp("good");
    good(&d);
    let g = check(&d);
    fs::remove_dir_all(&d).unwrap();
    let g = g.unwrap();
    assert_eq!(g.root, d);
    assert_eq!(g.exefs, d.join("exefs"));
    assert_eq!(g.romfs, d.join("romfs"));
    assert!(g.aoc.is_empty());
    assert!(!g.from_importer);
    assert_eq!(g.version, Version::V3_0_2);
}

#[test]
fn extract_romfs_aoc_and_importer() {
    let d = tmp("extra");
    good(&d);
    fs::remove_dir_all(d.join("romfs")).unwrap();
    fs::create_dir_all(d.join("extract/romfs")).unwrap();
    for a in ["0002", "0001"] {
        fs::create_dir_all(d.join("aoc").join(a)).unwrap();
    }
    fs::write(d.join("aoc/readme.txt"), b"").unwrap();
    fs::create_dir_all(d.join(".import")).unwrap();
    let g = check(&d);
    fs::remove_dir_all(&d).unwrap();
    let g = g.unwrap();
    assert_eq!(g.romfs, d.join("extract/romfs"));
    assert_eq!(
        g.aoc,
        [d.join("aoc").join("0001"), d.join("aoc").join("0002")]
    );
    assert!(g.from_importer);
}

#[test]
fn romfs_wins_over_extract_romfs() {
    let d = tmp("both");
    good(&d);
    fs::create_dir_all(d.join("extract/romfs")).unwrap();
    let g = check(&d);
    fs::remove_dir_all(&d).unwrap();
    assert_eq!(g.unwrap().romfs, d.join("romfs"));
}

#[test]
fn not_a_folder() {
    let d = tmp("nofolder");
    let file = d.join("game.txt");
    fs::write(&file, b"").unwrap();
    let missing = d.join("nope");
    bad(&file, &format!("{} is not a folder", file.display()));
    bad(&missing, &format!("{} is not a folder", missing.display()));
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn problems_in_order() {
    let d = tmp("order");
    bad(&d, "exefs/ is missing");
    fs::create_dir_all(d.join("exefs")).unwrap();
    for name in EXEFS_FILES {
        bad(&d, &format!("exefs/{name} is missing"));
        fs::write(d.join("exefs").join(name), b"").unwrap();
    }
    bad(&d, "romfs/ is missing");
    fs::create_dir_all(d.join("romfs")).unwrap();
    // main is empty: the header check comes last and passes its error through.
    let short = check(&d);
    fs::write(d.join("exefs/main"), main_header([0x00; 16])).unwrap();
    let other = check(&d);
    fs::remove_dir_all(&d).unwrap();
    assert_eq!(
        short,
        Err(Error::NotAcnh(
            "exefs/main is too short to be a Switch executable".into()
        ))
    );
    assert_eq!(
        other,
        Err(Error::WrongVersion {
            found: "0".repeat(32)
        })
    );
}

#[test]
fn a_main_folder_is_not_a_main_file() {
    let d = tmp("dir");
    good(&d);
    fs::remove_file(d.join("exefs/sdk")).unwrap();
    fs::create_dir_all(d.join("exefs/sdk")).unwrap();
    let r = check(&d);
    fs::remove_dir_all(&d).unwrap();
    assert_eq!(r, Err(Error::BadGameFolder("exefs/sdk is missing".into())));
}
