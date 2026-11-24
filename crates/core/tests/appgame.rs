use acnh_core::appgame::{install, install_with, GAME_DIR, PARTIAL_DIR};
use acnh_core::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn touch(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, b"").unwrap();
}

fn make_stub(root: &Path) {
    for rel in [
        "analysis/main.bin",
        "romfs.pak",
        "spirv.pak",
        "fonts/shared.ttf",
        "aoc/1/data",
        "libacnh_game.so",
        "gen/main/table.cpp",
        "build/acnh",
        "exefs/main",
        "extract/romfs/a",
        "spirv/a.spv",
        "mods/a",
        "user/device_save/main.dat",
        "config.ini",
    ] {
        touch(&root.join(rel));
    }
}

#[test]
fn install_copies_only_the_six_names_read_only() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    let private = tmp.path().join("private");
    let dest = private.join(GAME_DIR);
    make_stub(&src);

    let out = install(&src, &private, &dest, 1024 * 1024).unwrap();
    assert_eq!(out, dest);
    assert!(!private.join(PARTIAL_DIR).exists());

    for present in [
        "analysis/main.bin",
        "romfs.pak",
        "spirv.pak",
        "fonts/shared.ttf",
        "aoc/1/data",
        "libacnh_game.so",
    ] {
        let p = dest.join(present);
        assert!(p.is_file(), "expected {present} to be copied");
        assert!(
            fs::metadata(&p).unwrap().permissions().readonly(),
            "expected {present} to be read-only"
        );
    }

    for excluded in [
        "gen",
        "build",
        "exefs",
        "extract",
        "spirv",
        "mods",
        "user",
        "config.ini",
    ] {
        assert!(
            !dest.join(excluded).exists(),
            "did not expect {excluded} in game/"
        );
    }
}

#[test]
fn install_refuses_missing_required_file() {
    for missing in [
        "analysis/main.bin",
        "romfs.pak",
        "spirv.pak",
        "libacnh_game.so",
    ] {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        let private = tmp.path().join("private");
        let dest = private.join(GAME_DIR);
        make_stub(&src);
        fs::remove_file(src.join(missing)).unwrap();

        let err = install(&src, &private, &dest, 1024 * 1024).unwrap_err();
        assert!(
            err.to_string().contains(missing),
            "error '{err}' should name {missing}"
        );
        assert!(!dest.exists(), "game/ must not be left behind");
    }
}

#[test]
fn install_resumes_after_partial_failure() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    let private = tmp.path().join("private");
    let dest = private.join(GAME_DIR);
    make_stub(&src);
    // Give files non-zero sizes so we can verify resume skips finished ones.
    fs::write(src.join("analysis/main.bin"), b"1234").unwrap();
    fs::write(src.join("aoc/1/data"), b"5678").unwrap();
    fs::write(src.join("fonts/shared.ttf"), b"abcd").unwrap();

    let mut first_count = 0usize;
    let mut fail_on_third = |from: &Path, to: &Path| {
        first_count += 1;
        if first_count == 3 {
            return Err(Error::Io("simulated failure on third file".into()));
        }
        fs::copy(from, to)?;
        Ok(())
    };

    let err = install_with(
        &src,
        &private,
        &dest,
        1024 * 1024,
        false,
        &mut fail_on_third,
    )
    .unwrap_err();
    assert!(err.to_string().contains("simulated failure"));
    assert!(!dest.exists(), "incomplete copy must not rename to game/");
    assert!(private.join(PARTIAL_DIR).is_dir());

    let mut second_copied: Vec<PathBuf> = Vec::new();
    let mut resume_copy = |from: &Path, to: &Path| {
        second_copied.push(from.to_path_buf());
        fs::copy(from, to)?;
        Ok(())
    };

    install_with(&src, &private, &dest, 1024 * 1024, false, &mut resume_copy).unwrap();
    assert!(dest.is_dir());
    assert!(!private.join(PARTIAL_DIR).exists());
    // Total files in stub = 6; first 2 succeeded before failure on 3rd, so only 4 remain.
    assert_eq!(second_copied.len(), 4);
}

#[test]
fn install_refuses_insufficient_free_space() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    let private = tmp.path().join("private");
    let dest = private.join(GAME_DIR);
    make_stub(&src);
    fs::write(src.join("romfs.pak"), vec![0u8; 4096]).unwrap();

    let mut copied = false;
    let mut no_copy = |_: &Path, _: &Path| {
        copied = true;
        Ok(())
    };

    let err = install_with(&src, &private, &dest, 1024, false, &mut no_copy).unwrap_err();
    assert!(!copied, "must refuse before anything is copied");
    assert!(
        err.to_string().contains("4096"),
        "error '{err}' should name the size needed (4096)"
    );
    assert!(!dest.exists());
    assert!(!private.join(PARTIAL_DIR).exists());
}

#[test]
fn install_refuses_dest_outside_private_folder() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    let private = tmp.path().join("private");
    let outside = tmp.path().join("outside/game");
    make_stub(&src);

    let err = install(&src, &private, &outside, 1024 * 1024).unwrap_err();
    assert!(err.to_string().contains("outside"));
    assert!(!outside.exists());

    let traversal = private.join("../escape/game");
    let err2 = install(&src, &private, &traversal, 1024 * 1024).unwrap_err();
    assert!(err2.to_string().contains("outside"));
}

#[test]
fn install_refuses_non_empty_decoy_under_ci() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    let private = tmp.path().join("private");
    let dest = private.join(GAME_DIR);
    make_stub(&src);
    fs::write(src.join("romfs.pak"), b"not empty").unwrap();

    let mut copy = |from: &Path, to: &Path| {
        fs::copy(from, to)?;
        Ok(())
    };
    let err = install_with(&src, &private, &dest, 1024 * 1024, true, &mut copy).unwrap_err();
    assert!(err.to_string().contains("romfs.pak"));
    assert!(!dest.exists());
}
