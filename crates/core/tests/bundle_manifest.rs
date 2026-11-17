// Locked tests for bundle::manifest and bundle::verify (issue 1.18; M11, M14).
use acnh_core::bundle::{manifest, verify, Bundle, BundleFile};
use acnh_core::platform::Os;
use std::fs;
use std::path::{Path, PathBuf};

const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const HELLO: &str = "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03";

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("acnh-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn put(root: &Path, rel: &str, data: &[u8]) {
    let p = root.join(rel); // "/" is accepted on every OS
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, data).unwrap();
}

#[rustfmt::skip]
fn f(path: &str, size: u64, sha256: &str) -> BundleFile {
    BundleFile { path: path.to_string(), size, sha256: sha256.to_string() }
}

#[rustfmt::skip]
fn bundle(files: Vec<BundleFile>) -> Bundle {
    Bundle { target: Os::Linux, version: "3.0.2".into(), commit: "c".into(), build_id: "x".into(), files }
}

#[test]
fn manifest_lists_sorted_files_with_hashes_and_build_id() {
    let d = tmp("man-ok");
    put(&d, "sub/deep/c.txt", b"hello\n");
    put(&d, "a.txt", b"abc");
    put(&d, "sub/b.bin", b"");
    let b = manifest(&d, Os::Mac, "3.0.3", "f469c5a").unwrap();
    fs::remove_dir_all(&d).unwrap();
    assert_eq!(b.target, Os::Mac);
    assert_eq!(
        (b.version.as_str(), b.commit.as_str()),
        ("3.0.3", "f469c5a")
    );
    assert_eq!(b.build_id, "d4b09c8646368afc");
    let want = [
        f("a.txt", 3, ABC),
        f("sub/b.bin", 0, EMPTY),
        f("sub/deep/c.txt", 6, HELLO),
    ];
    assert_eq!(b.files, want);
}

#[test]
fn manifest_of_an_empty_folder() {
    let d = tmp("man-none");
    let none = manifest(&d, Os::Linux, "3.0.2", "c").unwrap();
    fs::remove_dir_all(&d).unwrap();
    assert!(none.files.is_empty());
    assert_eq!(none.build_id, "e3b0c44298fc1c14");
}

#[test]
fn manifest_skips_device_owned() {
    let d = tmp("man-skip");
    put(&d, "a.txt", b"abc");
    put(&d, "root/user/device_save/main.dat", b"x");
    put(&d, "root/config.ini", b"y");
    put(&d, "root/mods/ptbr/a.msbt", b"m");
    put(&d, "root/extract/romfs/a.bin", b"g");
    put(&d, "logs/last.log", b"z");
    let b = manifest(&d, Os::Linux, "3.0.2", "c").unwrap();
    fs::remove_dir_all(&d).unwrap();
    assert_eq!(b.files, vec![f("a.txt", 3, ABC)]);
    assert_eq!(b.build_id, "ef5d55bcc75826dc");
}

#[test]
fn manifest_refuses_a_forbidden_file() {
    for rel in [
        "acnh.NSP",
        "sub/prod.keys",
        "romfs.pak",
        "libacnh_game.so",
        "gen/main.cpp",
        "x/exefs/main",
    ] {
        let d = tmp("man-bad");
        put(&d, "a.txt", b"abc");
        put(&d, rel, b"q");
        let err = manifest(&d, Os::Linux, "3.0.2", "c")
            .unwrap_err()
            .to_string();
        fs::remove_dir_all(&d).unwrap();
        let tail = "game data and saves never go in a bundle";
        assert_eq!(err, format!("Build hub: refusing to bundle {rel}: {tail}"));
    }
}

#[test]
fn verify_accepts_matching_files_and_ignores_extras() {
    let d = tmp("ver-ok");
    put(&d, "a.txt", b"abc");
    put(&d, "sub/deep/c.txt", b"hello\n");
    put(&d, "extra.txt", b"not listed");
    let ok = bundle(vec![f("a.txt", 3, ABC), f("sub/deep/c.txt", 6, HELLO)]);
    let (r, empty) = (verify(&ok, &d), verify(&bundle(vec![]), &d));
    fs::remove_dir_all(&d).unwrap();
    assert_eq!((r, empty), (Ok(()), Ok(())));
}

#[test]
fn verify_names_a_missing_file() {
    let d = tmp("ver-miss");
    put(&d, "a.txt", b"abc");
    let b = bundle(vec![f("a.txt", 3, ABC), f("sub/gone.bin", 0, EMPTY)]);
    let err = verify(&b, &d).unwrap_err().to_string();
    fs::remove_dir_all(&d).unwrap();
    assert_eq!(err, "Build hub: sub/gone.bin does not match the bundle");
}

// Error text of verifying a folder with only a.txt = data against (a.txt, size, ABC).
fn bad_a(name: &str, size: u64, data: &[u8]) -> String {
    let d = tmp(name);
    put(&d, "a.txt", data);
    let err = verify(&bundle(vec![f("a.txt", size, ABC)]), &d).unwrap_err();
    fs::remove_dir_all(&d).unwrap();
    err.to_string()
}

#[test]
fn verify_catches_size_mismatch_even_when_the_hash_matches() {
    assert_eq!(
        bad_a("ver-size", 4, b"abc"),
        "Build hub: a.txt does not match the bundle"
    );
}

#[test]
fn verify_catches_hash_mismatch_at_the_same_size() {
    assert_eq!(
        bad_a("ver-hash", 3, b"abd"),
        "Build hub: a.txt does not match the bundle"
    );
}

#[test]
fn verify_reports_the_first_bad_file_in_order() {
    let d = tmp("ver-first");
    put(&d, "b.txt", b"abd");
    put(&d, "c.txt", b"abd");
    let b = bundle(vec![f("b.txt", 3, ABC), f("c.txt", 3, ABC)]);
    let err = verify(&b, &d).unwrap_err().to_string();
    fs::remove_dir_all(&d).unwrap();
    assert_eq!(err, "Build hub: b.txt does not match the bundle");
}
