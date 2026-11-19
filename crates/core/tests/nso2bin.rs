// Tests for nso2bin (issue 1.28): made-up NSOs and the flat image `tools/nso2bin.py` writes.
// The bytes below are invented here; no game data.
use acnh_core::nso2bin::{self, SEGMENTS};
use acnh_core::Error;
use std::fs;
use std::path::PathBuf;

const TEXT: &[u8] = b"TEXT";
const RO: &[u8] = b"RO-RO-RO!";
const DATA: &[u8] = b"DATA!";

// text: plain at 0x08; ro: LZ4 at 0x10 (the 0x90 token is nine literals); data: plain at
// 0x20. Every segment's bytes follow the 0x6C-byte header.
const EXPECTED: [u8; 0x25] = [
    0, 0, 0, 0, 0, 0, 0, 0, b'T', b'E', b'X', b'T', 0, 0, 0, 0, b'R', b'O', b'-', b'R', b'O', b'-',
    b'R', b'O', b'!', 0, 0, 0, 0, 0, 0, 0, b'D', b'A', b'T', b'A', b'!',
];

fn le32(value: u32) -> [u8; 4] {
    value.to_le_bytes()
}

fn nso() -> Vec<u8> {
    let mut raw = vec![0u8; 0x6C];
    raw[..4].copy_from_slice(b"NSO0");
    raw[0x0C..0x10].copy_from_slice(&le32(0b010));
    raw[0x3C..0x40].copy_from_slice(&le32(0x40));

    let segments = [(0x6Cu32, 0x08u32, 4u32), (0x70, 0x10, 9), (0x7A, 0x20, 5)];
    for (i, (file_offset, memory_offset, size)) in segments.iter().enumerate() {
        let at = 0x10 + i * 0x10;
        raw[at..at + 4].copy_from_slice(&le32(*file_offset));
        raw[at + 4..at + 8].copy_from_slice(&le32(*memory_offset));
        raw[at + 8..at + 12].copy_from_slice(&le32(*size));
    }
    for (i, compressed_size) in [4u32, 10, 5].iter().enumerate() {
        let at = 0x60 + i * 4;
        raw[at..at + 4].copy_from_slice(&le32(*compressed_size));
    }

    raw.extend_from_slice(TEXT);
    raw.push(0x90);
    raw.extend_from_slice(RO);
    raw.extend_from_slice(DATA);
    raw
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("acnh-nso2bin-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn flat_image_matches_nso2bin() {
    assert_eq!(nso2bin::image(&nso()).unwrap(), EXPECTED);
}

#[test]
fn parse_exposes_segments_and_bss() {
    let parsed = nso2bin::parse(&nso()).unwrap();
    assert_eq!(SEGMENTS, 3);
    assert_eq!(parsed.bss_size, 0x40);
    assert_eq!(
        parsed
            .segments
            .iter()
            .map(|segment| (segment.memory_offset, segment.bytes.as_slice()))
            .collect::<Vec<_>>(),
        vec![(0x08, TEXT), (0x10, RO), (0x20, DATA)]
    );
    assert_eq!(parsed.image().unwrap(), EXPECTED);
}

#[test]
fn a_truncated_segment_is_not_acnh() {
    let mut raw = nso();
    raw.pop();
    assert!(matches!(nso2bin::image(&raw), Err(Error::NotAcnh(_))));
}

#[test]
fn a_header_without_the_segment_table_is_not_acnh() {
    assert!(matches!(
        nso2bin::image(&nso()[..0x6B]),
        Err(Error::NotAcnh(_))
    ));
}

#[test]
fn wrong_magic_is_not_acnh() {
    let mut raw = nso();
    raw[..4].copy_from_slice(b"NSO1");
    assert!(matches!(nso2bin::image(&raw), Err(Error::NotAcnh(_))));
}

#[test]
fn bad_lz4_data_is_not_acnh() {
    let mut raw = nso();
    raw[0x70..0x7A].fill(0);
    assert!(matches!(nso2bin::image(&raw), Err(Error::NotAcnh(_))));
}

#[test]
fn writes_the_flat_image_to_a_file() {
    let dir = temp("write");
    let input = dir.join("main");
    let output = dir.join("main.bin");
    fs::write(&input, nso()).unwrap();

    nso2bin::write(&input, &output).unwrap();

    assert_eq!(fs::read(&output).unwrap(), EXPECTED);
    fs::remove_dir_all(&dir).unwrap();
}
