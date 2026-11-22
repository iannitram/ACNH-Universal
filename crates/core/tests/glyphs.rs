#[path = "fixtures/font_bytes.rs"]
mod font_bytes;

use acnh_core::glyphs::{decode_bfttf, encode_bfttf, replace_extension};
use acnh_core::sarc::parse;

// Synthetic members: these bytes do not come from a game or a host font.
fn archive() -> Vec<u8> {
    let font = encode_bfttf(b"test font", 0x12345678).unwrap();
    let names = b"ParkExt.bfttf\0\0\0other\0\0\0";
    let base = 0x48 + names.len();
    let mut out = Vec::new();
    out.extend_from_slice(b"SARC\x14\0\xff\xfe");
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(base as u32).to_le_bytes());
    out.extend_from_slice(b"\0\x01\0\0SFAT\x0c\0\x02\0");
    out.extend_from_slice(&101u32.to_le_bytes());
    for (hash, attr, start, end) in [
        (1u32, 0x01000000u32, 0u32, font.len() as u32),
        (2, 0x01000004, 24, 29),
    ] {
        for n in [hash, attr, start, end] {
            out.extend_from_slice(&n.to_le_bytes());
        }
    }
    out.extend_from_slice(b"SFNT\x08\0\0\0");
    out.extend_from_slice(names);
    out.extend_from_slice(&font);
    out.resize(base + 24, 0);
    out.extend_from_slice(b"other");
    let len = out.len() as u32;
    out[8..12].copy_from_slice(&len.to_le_bytes());
    out
}

#[test]
fn bfttf_roundtrips_and_rejects_truncation() {
    for size in 0..12 {
        let original: Vec<u8> = (0..size).collect();
        let encoded = encode_bfttf(&original, 0x12345678).unwrap();
        assert_eq!(decode_bfttf(&encoded).unwrap(), (original, 0x12345678));
        assert!(decode_bfttf(&encoded[..encoded.len() - 1]).is_err());
    }
}

#[test]
fn replacement_retains_other_members_and_key() {
    let input = archive();
    let output = replace_extension(&input, b"replacement").unwrap();
    let parsed = parse(&output).unwrap();
    let members = parsed.members().unwrap();
    assert_eq!(
        decode_bfttf(members[0].data).unwrap(),
        (b"replacement".to_vec(), 0x12345678)
    );
    assert_eq!(members[1].data, b"other");
}

#[test]
fn builds_button_font_from_synthetic_font() {
    use acnh_core::glyphs::{button_font, truetype::Font};
    let source = font_bytes::decode_hex(include_str!("fixtures/synthetic-buttons.hex"));
    let output = button_font::build(&source).unwrap();
    let font = Font::parse(&output).unwrap();
    // Point lists recorded from tools/fonts/make_button_font.py with FontTools.
    for line in include_str!("fixtures/button-oracle.txt")
        .lines()
        .filter(|l| !l.starts_with('#'))
    {
        let mut columns = line.split_whitespace();
        let cp = u32::from_str_radix(columns.next().unwrap(), 16).unwrap();
        let advance: u16 = columns.next().unwrap().parse().unwrap();
        let ends: Vec<usize> = columns
            .next()
            .unwrap()
            .split(',')
            .map(|x| x.parse().unwrap())
            .collect();
        let points: Vec<(i16, i16, bool)> = columns
            .next()
            .unwrap()
            .split(',')
            .map(|p| {
                let mut values = p.split(':');
                (
                    values.next().unwrap().parse().unwrap(),
                    values.next().unwrap().parse().unwrap(),
                    values.next().unwrap() == "1",
                )
            })
            .collect();
        let gid = font.glyph_id(char::from_u32(cp).unwrap()).unwrap();
        assert_eq!(font.advance(gid).unwrap(), advance, "U+{cp:04X}");
        let outline = font.outline(gid).unwrap();
        let mut actual_ends = Vec::new();
        let mut count = 0;
        for c in &outline.contours {
            count += c.len();
            actual_ends.push(count - 1);
        }
        let actual: Vec<_> = outline
            .contours
            .iter()
            .flatten()
            .map(|p| (p.x as i16, p.y as i16, p.on_curve))
            .collect();
        assert_eq!(actual_ends, ends, "U+{cp:04X}");
        assert_eq!(actual, points, "U+{cp:04X}");
    }
}

#[test]
fn composite_host_letters_are_flattened() {
    use acnh_core::glyphs::{button_font, truetype::Font};
    let source = font_bytes::decode_hex(include_str!("fixtures/synthetic-buttons-composite.hex"));
    let output = button_font::build(&source).unwrap();
    let font = Font::parse(&output).unwrap();
    let outline = font.outline(font.glyph_id('\u{e0e1}').unwrap()).unwrap();
    assert_eq!(outline.contours.len(), 3);
    assert!(outline.contours[1].len() > 4);
}
