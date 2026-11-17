// Locked tests for the bundle text format and the never-bundled rule (issue 1.17; M11, M14).
use acnh_core::bundle::{from_text, is_forbidden, to_text, Bundle, BundleFile, DEVICE_OWNED};
use acnh_core::error::Error;
use acnh_core::platform::{Os, ALL_OS};

const H1: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const H2: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
const HEAD: &str = "acnh-bundle 1\ntarget mac\nversion 3.0.2\ncommit c\nbuild b\n";

fn file(path: &str, size: u64, sha256: &str) -> BundleFile {
    let (path, sha256) = (path.to_string(), sha256.to_string());
    BundleFile { path, size, sha256 }
}

fn sample() -> (Bundle, String) {
    let b = Bundle {
        target: Os::Mac,
        version: "3.0.2".to_string(),
        commit: "abc123".to_string(),
        build_id: "0011223344556677".to_string(),
        files: vec![
            file("ACNH.app/acnh", 12, H1),
            file("assets/my font.ttf", 0, H2),
        ],
    };
    let t = "acnh-bundle 1\ntarget mac\nversion 3.0.2\ncommit abc123\nbuild 0011223344556677\n";
    (
        b,
        format!("{t}file 12 {H1} ACNH.app/acnh\nfile 0 {H2} assets/my font.ttf\n"),
    )
}

fn bad(text: &str, n: usize, line: &str) {
    let want = Error::Hub(format!("bad bundle line {n}: {line}"));
    assert_eq!(from_text(text).unwrap_err(), want, "{text:?}");
}

#[test]
fn device_owned_entries() {
    assert_eq!(DEVICE_OWNED, &["root", "logs"]);
}

#[test]
fn text_is_exact_and_reads_back() {
    let (b, t) = sample();
    assert_eq!(to_text(&b), t);
    assert_eq!(from_text(&t).unwrap(), b);
}

#[test]
fn no_files_is_five_lines() {
    let b = from_text(HEAD).unwrap();
    assert_eq!((b.target, b.files.len()), (Os::Mac, 0));
    assert_eq!(
        (b.version.as_str(), b.commit.as_str(), b.build_id.as_str()),
        ("3.0.2", "c", "b")
    );
    assert_eq!(to_text(&b), HEAD);
}

#[test]
fn every_os_key_reads() {
    for os in ALL_OS {
        let t = HEAD.replace("target mac", &format!("target {}", os.key()));
        assert_eq!(from_text(&t).unwrap().target, os);
    }
}

#[test]
fn wrong_first_line() {
    bad("acnh-bundle 2\ntarget mac\n", 1, "acnh-bundle 2");
    bad("totk-bundle 1\ntarget mac\n", 1, "totk-bundle 1");
    bad("", 1, "");
}

#[test]
fn header_lines_missing_or_out_of_order() {
    bad(
        "acnh-bundle 1\nversion 3.0.2\ntarget mac\n",
        2,
        "version 3.0.2",
    );
    bad(
        "acnh-bundle 1\ntarget mac\ncommit c\nversion 3.0.2\n",
        3,
        "commit c",
    );
    bad(
        "acnh-bundle 1\ntarget mac\nversion 3.0.2\ncommit c\n",
        5,
        "",
    );
    bad(
        "acnh-bundle 1\ntarget mac\nversion \ncommit c\nbuild b\n",
        3,
        "version ",
    );
}

#[test]
fn unknown_os_key() {
    bad(
        "acnh-bundle 1\ntarget beos\nversion 3.0.2\n",
        2,
        "target beos",
    );
    bad(
        "acnh-bundle 1\ntarget Mac\nversion 3.0.2\n",
        2,
        "target Mac",
    );
}

#[test]
fn file_lines_must_parse() {
    for line in [
        format!("file big {H1} a"),
        format!("file -1 {H1} a"),
        format!("file 1 {H1}"),
        format!("files 1 {H1} a"),
    ] {
        bad(&format!("{HEAD}{line}\n"), 6, &line);
    }
}

#[test]
fn sha256_must_be_64_lowercase_hex() {
    let (up, long) = (H1.to_uppercase(), format!("{H1}0"));
    let g = format!("g{}", &H1[1..]);
    for h in [up.as_str(), &H1[..63], &long, &g] {
        let line = format!("file 1 {h} a");
        bad(&format!("{HEAD}{line}\n"), 6, &line);
    }
}

#[test]
fn forbidden_path_is_refused() {
    let t = format!("{HEAD}file 1 {H1} ok\nfile 1 {H1} root/user/device_save/main.dat\n");
    let want = "refusing to bundle root/user/device_save/main.dat: game data and saves never go in a bundle";
    assert_eq!(from_text(&t).unwrap_err(), Error::Hub(want.to_string()));
}

fn all(list: &str, want: bool) {
    for p in list.split(' ') {
        assert_eq!(is_forbidden(p), want, "{p}");
    }
}

#[test]
fn forbidden_suffixes_in_any_case() {
    all(
        "a.nsp a.xci a.nca a.nsz a.xcz a.nso a.pak a.keys d/e/prod.keys",
        true,
    );
    all(
        "G.NSP g.Xci x.NcA x.NSZ x.XcZ main.NSO romfs.PAK prod.KEYS",
        true,
    );
    all("nsp a.nsp.txt keys a.nspx a.key a.xc a.nso2 a.paks", false);
}

#[test]
fn forbidden_game_library_by_name() {
    all(
        "libacnh_game.so libacnh_game.dylib bin/libacnh_game.dll libx_game.a",
        true,
    );
    all("acnh_game.so mylib_game.so libacnh.so libacnh_gamer", false);
}

#[test]
fn forbidden_parts_anywhere() {
    all(
        "exefs/main romfs/Model/a.bfres aoc/x gen/main.cpp game/x",
        true,
    );
    all(
        "a/b/exefs/c x/device_save/main.dat user/backups/s/main.dat",
        true,
    );
    all(
        "exefs2/a myromfs/a generated/a games/a device_saves/a backup/a",
        false,
    );
}

#[test]
fn forbidden_device_owned_first_part_only() {
    all(
        "root root/a root/config.ini root/mods/ptbr/x logs logs/x",
        true,
    );
    all("sub/root/a sub/logs/a roots/a logs2/a log/x", false);
}

#[test]
fn a_player_build_is_allowed() {
    all(
        "acnh acnh.exe ACNH.app/Contents/MacOS/acnh assets/acnhrecomp.png",
        false,
    );
}
