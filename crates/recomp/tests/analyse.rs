//! Analyse pass on hand-written AArch64 with eh_frame data (no game code).

use acnh_recomp::analyse::{analyse, Function};
use acnh_recomp::ehframe;
use acnh_recomp::load::Module;
use std::collections::BTreeMap;

const NOP: u32 = 0xD503_201F;
const RET: u32 = 0xD65F_03C0;
const HDR: usize = 0x2000;
const CIE: usize = 0x2100;

fn put32(img: &mut [u8], at: usize, v: u32) {
    img[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// A module whose text is at 0x1000 and whose eh_frame holds one FDE per `(start, size)`.
fn module(text: &[(u64, u32)], text_size: u64, fdes: &[(u64, u32)]) -> Module {
    let mut image = vec![0u8; 0x2400];
    for &(a, w) in text {
        put32(&mut image, a as usize, w);
    }
    // CIE: zR, code align 1, data align -8, RA 30, R = pcrel sdata4.
    put32(&mut image, CIE, 16);
    image[CIE + 8..CIE + 20].copy_from_slice(&[1, b'z', b'R', 0, 1, 0x78, 30, 1, 0x1B, 0, 0, 0]);
    let mut fde_at = Vec::new();
    let mut p = CIE + 20;
    for &(start, size) in fdes {
        put32(&mut image, p, 16);
        put32(&mut image, p + 4, (p + 4 - CIE) as u32);
        put32(&mut image, p + 8, (start as i64 - (p as i64 + 8)) as u32);
        put32(&mut image, p + 12, size);
        fde_at.push((start, p));
        p += 20;
    }
    image[HDR..HDR + 4].copy_from_slice(&[1, 0x1B, 0x03, 0x3B]);
    put32(&mut image, HDR + 4, (CIE as i64 - (HDR as i64 + 4)) as u32);
    put32(&mut image, HDR + 8, fdes.len() as u32);
    for (i, &(start, at)) in fde_at.iter().enumerate() {
        put32(
            &mut image,
            HDR + 12 + 8 * i,
            (start as i64 - HDR as i64) as u32,
        );
        put32(&mut image, HDR + 16 + 8 * i, (at - HDR) as u32);
    }
    Module {
        name: "t".into(),
        image,
        text: (0x1000, text_size),
        ro: (0, 0),
        data: (0, 0),
        bss: (0, 0),
        symbols: vec![],
        relocs: vec![],
        dynamic: BTreeMap::new(),
        init_array: vec![],
        fini_array: vec![],
        eh_frame_hdr: (HDR as u64, 0x40),
    }
}

fn f(start: u64, end: u64, fde: bool) -> Function {
    Function {
        start,
        end,
        fde,
        name: String::new(),
    }
}

#[test]
fn parses_fdes() {
    let m = module(&[], 0x30, &[(0x1010, 0x10), (0x1000, 0x10)]);
    let fdes = ehframe::parse(&m).unwrap();
    // The header table is read in order; parse sorts by start.
    let starts: Vec<(u64, u64)> = fdes.iter().map(|d| (d.start, d.size)).collect();
    assert_eq!(starts, vec![(0x1000, 0x10), (0x1010, 0x10)]);
}

#[test]
fn functions_from_fdes_calls_and_branches() {
    let text = [
        (0x1000, 0x9400_0008), // bl 0x1020
        (0x1004, RET),
        (0x1008, NOP),
        (0x100C, NOP),
        (0x1010, 0x1400_0006), // b 0x1028 (into the middle of the unwound-less C)
        (0x1014, RET),
        (0x1018, NOP),
        (0x101C, NOP),
        (0x1020, NOP),
        (0x1024, NOP),
        (0x1028, RET),
        (0x102C, NOP),
    ];
    let m = module(&text, 0x30, &[(0x1000, 0x10), (0x1010, 0x10)]);
    let mut lines = Vec::new();
    let an = analyse(&m, &mut |s| lines.push(s.to_string()), &[]).unwrap();
    assert_eq!(
        an.functions,
        vec![
            f(0x1000, 0x1010, true),
            f(0x1010, 0x1020, true),
            f(0x1020, 0x1028, false),
            f(0x1028, 0x1030, false),
        ]
    );
    assert!(an.jump_tables.is_empty());
    assert!(an.unresolved_br.is_empty());
    assert_eq!(an.function_at(0x1024).map(|x| x.start), Some(0x1020));
    assert!(an.function_at(0x1030).is_none());
    assert!(lines[0].contains("2 FDE"));
}

#[test]
fn compressed_jump_table() {
    let text = [
        (0x1000, 0x7100_081F), // cmp w0, #2
        (0x1004, 0x5400_0128), // b.hi 0x1028
        (0x1008, 0x9000_0008), // adrp x8, 0x1000
        (0x100C, 0x9104_0108), // add x8, x8, #0x100
        (0x1010, 0x3860_6909), // ldrb w9, [x8, x0]
        (0x1014, 0x1000_004A), // adr x10, 0x101c
        (0x1018, 0x8B09_094A), // add x10, x10, x9, lsl #2
        (0x101C, 0xD61F_0140), // br x10
        (0x1020, RET),
        (0x1024, RET),
        (0x1028, RET),
    ];
    let mut m = module(&text, 0x2C, &[(0x1000, 0x2C)]);
    m.image[0x1100..0x1103].copy_from_slice(&[1, 2, 3]);
    let an = analyse(&m, &mut |_| {}, &[]).unwrap();
    assert_eq!(an.functions, vec![f(0x1000, 0x102C, true)]);
    assert_eq!(
        an.jump_tables.get(&0x101C),
        Some(&vec![0x1020, 0x1024, 0x1028])
    );
    assert!(an.unresolved_br.is_empty());
}

/// `add x8, x8, #1, lsl #12` moves the table base by 0x1000; the unshifted field would read the wrong bytes.
#[test]
fn jump_table_base_with_shifted_add_immediate() {
    let text = [
        (0x1000, 0x7100_081F), // cmp w0, #2
        (0x1004, 0x5400_0128), // b.hi 0x1028
        (0x1008, 0xB000_0008), // adrp x8, 0x2000
        (0x100C, 0x9140_0508), // add x8, x8, #1, lsl #12
        (0x1010, 0x3860_6909), // ldrb w9, [x8, x0]
        (0x1014, 0x1000_004A), // adr x10, 0x101c
        (0x1018, 0x8B09_094A), // add x10, x10, x9, lsl #2
        (0x101C, 0xD61F_0140), // br x10
        (0x1020, RET),
        (0x1024, RET),
        (0x1028, RET),
    ];
    let mut m = module(&text, 0x2C, &[(0x1000, 0x2C)]);
    m.image.resize(0x4000, 0);
    m.image[0x3000..0x3003].copy_from_slice(&[1, 2, 3]);
    let an = analyse(&m, &mut |_| {}, &[]).unwrap();
    assert_eq!(
        an.jump_tables.get(&0x101C),
        Some(&vec![0x1020, 0x1024, 0x1028])
    );
}

/// An FDE whose size reads as -1 must not stretch the function past the text (the walk over it never ends).
#[test]
fn hostile_fde_size_is_clamped_to_the_text() {
    let text = [(0x1000, NOP), (0x1004, RET)];
    let m = module(&text, 8, &[(0x1000, 0xFFFF_FFFF)]);
    let an = analyse(&m, &mut |_| {}, &[]).unwrap();
    assert!(an.functions.iter().all(|f| f.end <= 0x1008));
}

/// A frame header or CIE pointer outside the image is an error, not a panic.
#[test]
fn eh_frame_pointers_outside_the_image_are_errors() {
    let mut m = module(&[], 0x30, &[(0x1000, 0x10)]);
    m.eh_frame_hdr.0 = u64::MAX - 2;
    assert!(ehframe::parse(&m).is_err());
    let mut m = module(&[], 0x30, &[(0x1000, 0x10)]);
    // The FDE's back pointer to its CIE.
    put32(&mut m.image, CIE + 20 + 4, 0xFFFF_FFF0);
    assert!(ehframe::parse(&m).is_err());
}
