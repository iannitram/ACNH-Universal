//! The iPhone and iPad game download's plan and checks (issue #231) on every runner, with decoy
//! files (made-up bytes, never game data): what `appgame_ios.rs` returns to the player's
//! Objective-C++, minus the Keychain, Dropbox and the `URLSession`. The manifest, resume rule and
//! install are `acnh_core::appgame`'s (tested in `crates/core/tests/appgame_download.rs`); this
//! file tests the stepwise driving that is iOS's own.
#![cfg(not(any(target_os = "ios", target_os = "android")))]

#[allow(dead_code)]
#[path = "../src/appgame.rs"]
mod appgame;

use appgame::{api_arg, done_json, finish_json, installed, plan_json, PENDING};
use sha2::{Digest, Sha256};
use std::fs;

const ABI: &str = "0123456789ab";
const BUILD: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn manifest(files: &[(&str, Vec<u8>)], build: &str, abi: &str) -> Vec<u8> {
    let list: Vec<String> = files
        .iter()
        .map(|(p, b)| {
            format!(
                r#"{{"path":"{p}","size":{},"sha256":"{:x}"}}"#,
                b.len(),
                Sha256::digest(b)
            )
        })
        .collect();
    format!(
        r#"{{"version":1,"build_id":"{build}","nx_game_abi":"{abi}","files":[{}]}}"#,
        list.join(",")
    )
    .into_bytes()
}

fn decoys() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("analysis/main.bin", vec![1u8; 100]),
        ("romfs.pak", vec![2u8; 400]),
        ("spirv.pak", Vec::new()),
    ]
}

#[test]
fn plan_check_and_install_the_way_the_player_drives_them() {
    let root = tempfile::tempdir().unwrap();
    let files = decoys();
    let bytes = manifest(&files, BUILD, ABI);

    let plan = plan_json(root.path(), &bytes, ABI, BUILD, 1 << 30);
    assert_eq!(plan["ok"], true, "{plan}");
    assert_eq!(plan["installed"], false);
    assert_eq!(plan["total"], 500);
    // An empty file needs no request, and is not listed.
    assert_eq!(plan["remaining"], 500);
    let fetch = plan["fetch"].as_array().unwrap();
    assert_eq!(fetch.len(), 2);
    assert_eq!(fetch[0]["path"], "analysis/main.bin");
    assert!(
        !root.path().join(PENDING).exists(),
        "a plan made to ask the question keeps nothing: only the person's yes does"
    );
    // The player writes it once the person says Download.
    fs::write(root.path().join(PENDING), &bytes).unwrap();

    // The session wrote romfs.pak whole and half of the other; it checks, the other resumes.
    let partial = root.path().join("game.partial");
    fs::create_dir_all(partial.join("analysis")).unwrap();
    fs::write(partial.join("romfs.pak"), &files[1].1).unwrap();
    fs::write(partial.join("analysis/main.bin"), &files[0].1[..40]).unwrap();
    assert_eq!(done_json(root.path(), "romfs.pak")["verified"], true);
    let again = plan_json(root.path(), &bytes, ABI, BUILD, 60);
    assert_eq!(
        again["ok"], true,
        "only 60 bytes are left, and 60 are free: {again}"
    );
    assert_eq!(again["fetch"][0]["offset"], 40);
    assert_eq!(again["remaining"], 60);

    // Not all checked: no game/.
    assert_eq!(finish_json(root.path())["ok"], false);
    assert!(!installed(root.path()));
    fs::write(partial.join("analysis/main.bin"), &files[0].1).unwrap();
    assert_eq!(
        done_json(root.path(), "analysis/main.bin")["verified"],
        true
    );
    // The empty file is checked by its plan (a hash of nothing), the way a session never reports it.
    let last = plan_json(root.path(), &bytes, ABI, BUILD, 0);
    assert_eq!(last["remaining"], 0, "{last}");
    assert_eq!(finish_json(root.path())["ok"], true);
    assert!(installed(root.path()));
    assert!(!root.path().join(PENDING).exists());
    assert!(!root.path().join("game.verified.txt").exists());
    // Now it is the installed game, and `game/` holds its manifest.
    assert_eq!(
        plan_json(root.path(), &bytes, ABI, BUILD, 0)["installed"],
        true
    );
    assert_eq!(
        fs::read(root.path().join("game/manifest.json")).unwrap(),
        bytes
    );
    // game/ is read-only, so removing it needs the files writable again (as Android's does).
    assert!(fs::metadata(root.path().join("game/romfs.pak"))
        .unwrap()
        .permissions()
        .readonly());
}

#[test]
fn a_wrong_hash_is_deleted_and_fetched_again_from_zero() {
    let root = tempfile::tempdir().unwrap();
    let files = decoys();
    let bytes = manifest(&files, BUILD, ABI);
    assert_eq!(
        plan_json(root.path(), &bytes, ABI, BUILD, 1 << 30)["ok"],
        true
    );
    fs::write(root.path().join(PENDING), &bytes).unwrap();
    let partial = root.path().join("game.partial");
    fs::write(partial.join("romfs.pak"), vec![9u8; 400]).unwrap();
    assert_eq!(done_json(root.path(), "romfs.pak")["verified"], false);
    assert!(!partial.join("romfs.pak").exists());
    let again = plan_json(root.path(), &bytes, ABI, BUILD, 1 << 30);
    assert_eq!(again["fetch"][1]["offset"], 0);
    assert_eq!(done_json(root.path(), "nope.bin")["ok"], false);
}

#[test]
fn refusals_name_both_sides_and_write_nothing() {
    let root = tempfile::tempdir().unwrap();
    let bytes = manifest(
        &decoys(),
        "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
        "ffffffffffff",
    );
    let plan = plan_json(root.path(), &bytes, ABI, BUILD, 1 << 30);
    assert_eq!(plan["ok"], false);
    assert_eq!(plan["code"], "refused");
    let why = plan["error"].as_str().unwrap();
    for part in [
        "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
        "ffffffffffff",
        BUILD,
        ABI,
    ] {
        assert!(why.contains(part), "{why}");
    }
    assert!(!root.path().join("game.partial").exists());

    // Same build, another ABI: refused too, naming both. Any case, and a list of builds, match.
    let abi_only = manifest(&decoys(), BUILD, "ffffffffffff");
    let plan = plan_json(root.path(), &abi_only, ABI, BUILD, 1 << 30);
    assert_eq!(plan["code"], "refused");
    assert!(plan["error"].as_str().unwrap().contains("ffffffffffff"));
    let ok = manifest(&decoys(), BUILD, ABI);
    let lower = format!("cccccccccccccccccccccccccccccccc, {}", BUILD.to_lowercase());
    assert_eq!(
        plan_json(root.path(), &ok, ABI, &lower, 1 << 30)["ok"],
        true
    );

    let space = plan_json(root.path(), &ok, ABI, BUILD, 10);
    assert_eq!(space["code"], "no_space");
    assert!(
        !root.path().join(PENDING).exists(),
        "nothing waits after a refusal"
    );
    let bad = plan_json(root.path(), b"not a manifest", ABI, BUILD, 1 << 30);
    assert_eq!(bad["code"], "manifest");
}

#[test]
fn the_dropbox_arg_is_ascii_and_names_the_ios_folder() {
    assert_eq!(
        api_arg("romfs.pak"),
        r#"{"path":"/acnh-universal-game/ios/romfs.pak"}"#
    );
    assert!(api_arg("fonts/é.ttf").is_ascii());
}
