//! Full fonts stage with a made-up host font and a generated archive.

use acnh_core::glyphs::truetype::{Font, Outline, Point};
use acnh_core::glyphs::{decode_bfttf, encode_bfttf, stage};
use acnh_core::sarc;
use std::{fs, process::Command};

const ARCHIVE: &str = "Font/ScalableFont.sarc.zs";
const KEY: u32 = 0x235a_e017;
const ICONS: [char; 13] = [
    '\u{e0e0}', '\u{e0a0}', '\u{e0e1}', '\u{e0a1}', '\u{e0e2}', '\u{e0a2}', '\u{e0a4}', '\u{e0e6}',
    '\u{e0b3}', '\u{e0f2}', '\u{e0aa}', '\u{e23f}', '\u{e27f}',
];

fn fixture() -> Vec<u8> {
    include_str!("fixtures/synthetic-buttons.hex")
        .split_whitespace()
        .flat_map(|line| {
            (0..line.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&line[i..i + 2], 16).unwrap())
        })
        .collect()
}

fn outline(index: usize) -> Outline {
    let width = 700.0 + 11.0 * index as f64;
    let height = 600.0 + 13.0 * index as f64;
    let shift = index as f64 * 3.0;
    let points = if index % 4 < 2 {
        [(0.0, 0.0), (0.0, height), (width, height), (width, 0.0)]
    } else {
        [
            (0.0, 0.0),
            (width * 0.2, height),
            (width, height),
            (width, 0.0),
        ]
    };
    Outline {
        contours: vec![points
            .into_iter()
            .map(|(x, y)| Point {
                x: x + shift,
                y: y - 80.0,
                on_curve: true,
            })
            .collect()],
    }
}

fn archive(members: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut header = b"SARC\x14\0\xff\xfe\0\0\0\0\0\0\0\0\0\x01\0\0SFAT\x0c\0".to_vec();
    header.extend((members.len() as u16).to_le_bytes());
    header.extend(101u32.to_le_bytes());
    let mut names = Vec::new();
    let mut data = Vec::new();
    for (name, bytes) in members {
        let hash = name
            .bytes()
            .fold(0u32, |h, b| h.wrapping_mul(101).wrapping_add(b.into()));
        header.extend(hash.to_le_bytes());
        header.extend((0x0100_0000 | (names.len() as u32 / 4)).to_le_bytes());
        names.extend(name.bytes());
        names.push(0);
        names.resize(names.len().next_multiple_of(4), 0);
        data.resize(data.len().next_multiple_of(32), 0);
        header.extend((data.len() as u32).to_le_bytes());
        data.extend(bytes);
        header.extend((data.len() as u32).to_le_bytes());
    }
    header.extend(b"SFNT\x08\0\0\0");
    header.extend(names);
    header.resize(header.len().next_multiple_of(32), 0);
    let size = (header.len() + data.len()) as u32;
    let data_offset = header.len() as u32;
    header[8..12].copy_from_slice(&size.to_le_bytes());
    header[12..16].copy_from_slice(&data_offset.to_le_bytes());
    header.extend(data);
    header
}

fn shape(font: &Font, code: char) -> Vec<(i16, i16, bool)> {
    font.outline(font.glyph_id(code).unwrap())
        .unwrap()
        .contours
        .into_iter()
        .flatten()
        .map(|p| (p.x as i16, p.y as i16, p.on_curve))
        .collect()
}

#[test]
fn writes_each_archive_with_original_metrics_and_unrelated_members() {
    if Command::new("zstd").arg("--version").output().is_err() {
        return; // The archive stage uses the same zstd CLI as the other SARC stages.
    }
    let temp = tempfile::tempdir().unwrap();
    let host = fixture();
    let host_path = temp.path().join("host.ttf");
    fs::write(&host_path, &host).unwrap();

    let mut labels = Font::parse(&host).unwrap();
    for (i, c) in "HABXYLRT12".chars().enumerate() {
        if labels.glyph_id(c).is_none() {
            labels.add(c, outline(i), 800).unwrap();
        }
    }
    let label_bytes = encode_bfttf(&labels.finish().unwrap(), 13).unwrap();
    let mut source = Font::parse(&host).unwrap();
    for (i, cp) in ICONS.into_iter().enumerate() {
        source.add(cp, outline(i), 900 + i as u16).unwrap();
    }
    let original = source.finish().unwrap();
    let source = Font::parse(&original).unwrap();
    let raw = temp.path().join("synthetic.archive");
    fs::write(
        &raw,
        archive(&[
            ("ParkExt.bfttf", encode_bfttf(&original, KEY).unwrap()),
            ("Example-P_Seurat-B.bfttf", label_bytes.clone()),
            ("untouched.txt", b"unchanged member".to_vec()),
        ]),
    )
    .unwrap();
    let romfs = temp.path().join("romfs");
    let input = romfs.join(ARCHIVE);
    fs::create_dir_all(input.parent().unwrap()).unwrap();
    assert!(Command::new("zstd")
        .args(["-q", "--no-check"])
        .arg(raw)
        .arg("-o")
        .arg(input)
        .status()
        .unwrap()
        .success());

    let out = temp.path().join("fonts");
    stage(&romfs, &out, Some(&host_path)).unwrap();
    assert_eq!(fs::read(out.join("shared.ttf")).unwrap(), host);
    assert!(out.join("shared_5.ttf").is_file());
    for (style, layout) in [
        ("xbox", "console"),
        ("xbox", "xbox"),
        ("playstation", "console"),
        ("playstation", "xbox"),
        ("switch", "xbox"),
    ] {
        let base = out.join(format!("glyphs_{style}")).join(layout);
        let file = base.join("romfs").join(ARCHIVE);
        let raw = sarc::read(&file).unwrap();
        let parsed = sarc::parse(&raw).unwrap();
        let members = parsed.members().unwrap();
        let (font_bytes, key) = decode_bfttf(members[0].data).unwrap();
        assert_eq!(key, KEY);
        assert_eq!(fs::read(base.join("ParkExt.ttf")).unwrap(), font_bytes);
        assert_eq!(members[1].data, label_bytes);
        assert_eq!(members[2].data, b"unchanged member");
        let font = Font::parse(&font_bytes).unwrap();
        for code in ICONS {
            let before = source.glyph_id(code).unwrap();
            let after = font.glyph_id(code).unwrap();
            assert_eq!(
                source.advance(before).unwrap(),
                font.advance(after).unwrap()
            );
            let old = source.bounds(before).unwrap().unwrap();
            let new = font.bounds(after).unwrap().unwrap();
            assert_eq!(
                [old.x_min, old.y_min, old.x_max, old.y_max],
                [new.x_min, new.y_min, new.x_max, new.y_max],
                "{style}/{layout} U+{:04X}",
                code as u32
            );
        }
        assert_ne!(shape(&source, '\u{e0e0}'), shape(&font, '\u{e0e0}'));
        assert_eq!(shape(&source, '\u{e27f}'), shape(&font, '\u{e27f}'));

        // A single-segment frame, or a nonzero flag in the top two bits,
        // records the decompressed size. Bit 2 is the checksum flag.
        let frame = fs::read(file).unwrap();
        assert_eq!(&frame[..4], &[0x28, 0xb5, 0x2f, 0xfd]);
        assert_ne!(frame[4] & 0xe0, 0);
        assert_eq!(frame[4] & 0x04, 0);
    }
    assert!(!out.join("glyphs_switch/console").exists());
    let merged = out.join("glyphs_playstation/console/romfs").join(ARCHIVE);
    fs::remove_file(&merged).unwrap();
    stage(&romfs, &out, Some(&host_path)).unwrap();
    assert!(merged.is_file());
}
