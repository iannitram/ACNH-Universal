// Locked tests for nso (issue 1.03: the build id from an NSO header, SPEC.md M1). The headers
// are made up here: magic, zeros, and a chosen id; no game bytes.
use acnh_core::nso::{build_id, read_build_id, HEADER_LEN};
use acnh_core::Error;
use std::fs;

fn header(id: [u8; 16], len: usize) -> Vec<u8> {
    let mut h = vec![0u8; len.max(0x50)];
    h[..4].copy_from_slice(b"NSO0");
    h[0x40..0x50].copy_from_slice(&id);
    // The second half of the module id is not part of the build id.
    for b in h.iter_mut().take(0x60.min(len)).skip(0x50) {
        *b = 0xEE;
    }
    h.truncate(len);
    h
}

const ID: [u8; 16] = [
    0xFC, 0xD2, 0xBB, 0x23, 0x8A, 0xBE, 0x99, 0xE9, 0x25, 0xB4, 0xE4, 0x52, 0xDF, 0x1F, 0x41, 0xF1,
];

#[test]
fn header_len_is_0x60() {
    assert_eq!(HEADER_LEN, 0x60);
}

#[test]
fn reads_16_bytes_at_0x40_as_upper_hex() {
    assert_eq!(
        build_id(&header(ID, 0x60)).unwrap(),
        "FCD2BB238ABE99E925B4E452DF1F41F1"
    );
    assert_eq!(
        build_id(&header([0x0a; 16], 0x100)).unwrap(),
        "0A".repeat(16)
    );
}

#[test]
fn too_short_is_not_acnh() {
    for len in [0, 4, 0x50, 0x5F] {
        assert_eq!(
            build_id(&header(ID, len.max(4))[..len]),
            Err(Error::NotAcnh(
                "exefs/main is too short to be a Switch executable".into()
            )),
            "{len}"
        );
    }
}

#[test]
fn wrong_magic_is_not_acnh() {
    for magic in [b"NSO1", b"nso0", b"NRO0", b"\0\0\0\0"] {
        let mut h = header(ID, 0x60);
        h[..4].copy_from_slice(magic);
        assert_eq!(
            build_id(&h),
            Err(Error::NotAcnh(
                "exefs/main is not a Switch executable (NSO)".into()
            ))
        );
    }
}

#[test]
fn reads_from_a_file() {
    let d = std::env::temp_dir().join(format!("acnh-nso-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    let mut long = header(ID, 0x60);
    long.extend(std::iter::repeat_n(0x11, 4096));
    fs::write(d.join("main"), &long).unwrap();
    fs::write(d.join("short"), &long[..0x20]).unwrap();
    let got = read_build_id(&d.join("main"));
    let short = read_build_id(&d.join("short"));
    let missing = read_build_id(&d.join("nope"));
    fs::remove_dir_all(&d).unwrap();
    assert_eq!(got.unwrap(), "FCD2BB238ABE99E925B4E452DF1F41F1");
    assert_eq!(short.unwrap_err().code(), "not_acnh");
    assert_eq!(missing.unwrap_err().code(), "io");
}
