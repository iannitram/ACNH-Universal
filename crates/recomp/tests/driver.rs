//! Tests for the Rust recompiler driver (`driver.rs`) and module stats (`stats.rs`).

use acnh_recomp::driver::{parse_pchtxt, recompile, RecompileOptions};
use acnh_recomp::stats::stats;
use std::collections::BTreeSet;
use std::path::PathBuf;

const WORDS: &[u32] = &[
    0x1400_0002, // b 0x8
    0x0000_1000, // MOD0 pointer
    0x9100_0400, // add x0, x0, #1
    0x9400_0002, // bl 0x14
    0xD65F_03C0, // ret
    0x9100_0800, // add x0, x0, #2 (0x14)
    0xD65F_03C0, // ret
];

fn synthetic_nso() -> Vec<u8> {
    let mut text = Vec::new();
    for &w in WORDS {
        text.extend_from_slice(&w.to_le_bytes());
    }
    let mut ro = vec![0u8; 0x180];
    ro[0..4].copy_from_slice(b"MOD0");
    for (i, v) in [0x40i32, 0x1004, 0x1004, -0x1000, -0x1000]
        .iter()
        .enumerate()
    {
        ro[4 + i * 4..8 + i * 4].copy_from_slice(&v.to_le_bytes());
    }

    let strings = b"\0entry\0callee\0";
    let sym_start = 0x100usize;
    let mut symbols = vec![0u8; 24];
    // entry: offset 8, size 12
    symbols.extend_from_slice(&1u32.to_le_bytes());
    symbols.push(0x12);
    symbols.push(0);
    symbols.extend_from_slice(&1u16.to_le_bytes());
    symbols.extend_from_slice(&8u64.to_le_bytes());
    symbols.extend_from_slice(&12u64.to_le_bytes());
    // callee: offset 0x14, size 8
    symbols.extend_from_slice(&7u32.to_le_bytes());
    symbols.push(0x12);
    symbols.push(0);
    symbols.extend_from_slice(&1u16.to_le_bytes());
    symbols.extend_from_slice(&0x14u64.to_le_bytes());
    symbols.extend_from_slice(&8u64.to_le_bytes());

    let str_start = sym_start + symbols.len();
    ro[sym_start..str_start].copy_from_slice(&symbols);
    ro[str_start..str_start + strings.len()].copy_from_slice(strings);

    for (k, (tag, val)) in [
        (6i64, 0x1000u64 + sym_start as u64),
        (5i64, 0x1000u64 + str_start as u64),
        (0i64, 0u64),
    ]
    .iter()
    .enumerate()
    {
        let at = 0x40 + k * 16;
        ro[at..at + 8].copy_from_slice(&tag.to_le_bytes());
        ro[at + 8..at + 16].copy_from_slice(&val.to_le_bytes());
    }

    let data = vec![0u8; 4];
    let mut header = vec![0u8; 0x100];
    header[0..4].copy_from_slice(b"NSO0");
    let mut offset = header.len() as u32;
    for (k, (addr, seg)) in [(0u32, &text), (0x1000u32, &ro), (0x2000u32, &data)]
        .iter()
        .enumerate()
    {
        let seg_len = seg.len() as u32;
        let base = 0x10 + k * 0x10;
        header[base..base + 4].copy_from_slice(&offset.to_le_bytes());
        header[base + 4..base + 8].copy_from_slice(&addr.to_le_bytes());
        header[base + 8..base + 12].copy_from_slice(&seg_len.to_le_bytes());
        header[0x60 + k * 4..0x64 + k * 4].copy_from_slice(&seg_len.to_le_bytes());
        offset += seg_len;
    }

    let mut out = header;
    out.extend_from_slice(&text);
    out.extend_from_slice(&ro);
    out.extend_from_slice(&data);
    out
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("acnh-recomp-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn parses_pchtxt_enabled_sections_and_stops() {
    let text = "\
@nsobid-1234
// comment
@disabled
00000008 00000000
@enabled
00000008 01040091 // add x1, x0, #1
00000010 2000805200000014
@stop
00000020 FFFFFFFF
";
    let words = parse_pchtxt(text, std::path::Path::new("test.pchtxt")).unwrap();
    assert_eq!(
        words,
        vec![(0x8, 0x9100_0401), (0x10, 0x5280_0020), (0x14, 0x1400_0000),]
    );
}

#[test]
fn recompile_writes_headers_code_table_and_report() {
    let dir = temp_dir("basic");
    let nso = dir.join("main.nso");
    std::fs::write(&nso, synthetic_nso()).unwrap();
    let hooks = dir.join("hooks.txt");
    std::fs::write(&hooks, "0x8 # hook entry\n").unwrap();
    let out = dir.join("out");

    let opts = RecompileOptions {
        nso,
        out: out.clone(),
        name: "fixture".to_owned(),
        base: 0x40_1000_0000,
        jobs: 1,
        only: None,
        hooks: Some(hooks),
        variant: None,
    };
    let mut logs = Vec::new();
    let report = recompile(&opts, &mut |s| logs.push(s.to_owned())).unwrap();
    assert_eq!(report.functions, 3);
    assert_eq!(report.files, 1);
    assert_eq!(report.unhandled_total, 0);

    let funcs_h = std::fs::read_to_string(out.join("funcs.h")).unwrap();
    assert!(funcs_h.contains("#define NXB (NX_BASE + 0x10000000ull)"));
    assert!(funcs_h.contains("void fixture_00000008(nx::CPU& c);"));
    assert!(funcs_h.contains("void fixture_00000014(nx::CPU& c);"));

    let code = std::fs::read_to_string(out.join("code_000.cpp")).unwrap();
    assert!(code.contains("#include \"funcs.h\""));
    assert!(code.contains("nx_hook"));

    let table = std::fs::read_to_string(out.join("table.cpp")).unwrap();
    assert!(table.contains("extern const nx::ModuleInfo nx_module_fixture = {"));
    assert!(table.contains("{0x8, fixture_00000008}"));
    assert!(table.contains("{0x14, fixture_00000014}"));

    let rpt = std::fs::read_to_string(out.join("report.txt")).unwrap();
    assert!(rpt.contains("functions 3, handled"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recompile_with_variant_and_only_filter() {
    let dir = temp_dir("variant");
    let nso = dir.join("main.nso");
    std::fs::write(&nso, synthetic_nso()).unwrap();
    let pchtxt = dir.join("patch.pchtxt");
    // Patch instruction at 0x8 to `add x0, x0, #3` (0x91000c00 -> le 000c0091)
    std::fs::write(&pchtxt, "@enabled\n00000008 000c0091\n").unwrap();
    let out = dir.join("out");

    let mut only = BTreeSet::new();
    only.insert(0x8);
    let opts = RecompileOptions {
        nso,
        out: out.clone(),
        name: "fixture".to_owned(),
        base: acnh_recomp::REF_BASE,
        jobs: 2,
        only: Some(only),
        hooks: None,
        variant: Some(pchtxt),
    };
    let report = recompile(&opts, &mut |_| {}).unwrap();
    assert_eq!(report.functions, 1);

    let funcs_h = std::fs::read_to_string(out.join("funcs.h")).unwrap();
    assert!(funcs_h.contains("extern \"C\" bool nx_variant_on;"));
    assert!(funcs_h.contains("void fixture_00000014(nx::CPU& c);"));

    let code = std::fs::read_to_string(out.join("code_000.cpp")).unwrap();
    assert!(code.contains("fixture_00000008__v0"));
    assert!(code.contains("fixture_00000008__v1"));
    assert!(code.contains("if (nx_variant_on && nx_variant_ok(0x8))"));

    let table = std::fs::read_to_string(out.join("table.cpp")).unwrap();
    assert!(
        table.contains("nx::VariantRegistrar variant_registrar(\"fixture\", variant_words, 1);")
    );
    assert!(out.join("code_variant.cpp").is_file());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stats_prints_overview_and_histogram() {
    let dir = temp_dir("stats");
    let nso = dir.join("main.nso");
    std::fs::write(&nso, synthetic_nso()).unwrap();
    let mut buf = Vec::new();
    stats(&nso, &mut buf).unwrap();
    let text = String::from_utf8(buf).unwrap();
    assert!(text.contains("text 0x0+0x1c"));
    assert!(text.contains("symbols 3: imported 0, defined 2"));
    assert!(text.contains("instructions "));
    assert!(text.contains("ret"));
    let _ = std::fs::remove_dir_all(&dir);
}
