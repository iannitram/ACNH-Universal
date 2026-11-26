use acnh_recomp::load::{
    decompress_lz4_block, load, load_bytes, plt_imports, Error, DT_FINI_ARRAY, DT_FINI_ARRAYSZ,
    DT_INIT_ARRAY, DT_INIT_ARRAYSZ, DT_JMPREL, DT_NULL, DT_PLTRELSZ, DT_RELA, DT_RELASZ, DT_RELR,
    DT_RELRSZ, DT_STRSZ, DT_STRTAB, DT_SYMENT, DT_SYMTAB, R_AARCH64_GLOB_DAT, R_AARCH64_JUMP_SLOT,
    R_AARCH64_RELATIVE, SHN_UNDEF, STT_FUNC,
};
use std::path::Path;

/// Helper to encode raw LZ4 block containing only literals.
fn lz4_compress_literals(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let len = data.len();
    if len < 15 {
        out.push((len << 4) as u8);
    } else {
        out.push(0xF0);
        let mut rem = len - 15;
        while rem >= 255 {
            out.push(255);
            rem -= 255;
        }
        out.push(rem as u8);
    }
    out.extend_from_slice(data);
    out
}

/// Helper to build a complete hand-written NSO binary.
struct NsoBuilder {
    text: Vec<u8>,
    ro: Vec<u8>,
    data: Vec<u8>,
    text_mem_off: u32,
    ro_mem_off: u32,
    data_mem_off: u32,
    bss_size: u32,
    compress_mask: u32,
}

impl NsoBuilder {
    fn new() -> Self {
        Self {
            text: Vec::new(),
            ro: Vec::new(),
            data: Vec::new(),
            text_mem_off: 0,
            ro_mem_off: 0x1000,
            data_mem_off: 0x2000,
            bss_size: 0x500,
            compress_mask: 0,
        }
    }

    fn build(&self) -> Vec<u8> {
        let mut file = vec![0u8; 0x100]; // 0x100 header
        file[0..4].copy_from_slice(b"NSO0");
        file[0x0C..0x10].copy_from_slice(&self.compress_mask.to_le_bytes());
        file[0x3C..0x40].copy_from_slice(&self.bss_size.to_le_bytes());

        let seg_data = [&self.text, &self.ro, &self.data];
        let mem_offs = [self.text_mem_off, self.ro_mem_off, self.data_mem_off];

        let mut current_file_off = 0x100u32;
        for i in 0..3 {
            let uncompressed = seg_data[i];
            let blob = if (self.compress_mask & (1 << i)) != 0 {
                lz4_compress_literals(uncompressed)
            } else {
                uncompressed.clone()
            };

            let seg_hdr = 0x10 + i * 0x10;
            file[seg_hdr..seg_hdr + 4].copy_from_slice(&current_file_off.to_le_bytes());
            file[seg_hdr + 4..seg_hdr + 8].copy_from_slice(&mem_offs[i].to_le_bytes());
            file[seg_hdr + 8..seg_hdr + 12]
                .copy_from_slice(&(uncompressed.len() as u32).to_le_bytes());

            file[0x60 + i * 4..0x64 + i * 4].copy_from_slice(&(blob.len() as u32).to_le_bytes());

            file.extend_from_slice(&blob);
            current_file_off += blob.len() as u32;
        }

        file
    }
}

/// Creates a synthetic module memory layout containing:
/// - text segment with MOD0 header pointing to dynamic section
/// - dynamic section with strtab, symtab, rela, jmprel, relr, init_array, fini_array
/// - data segment containing GOT and tables
fn create_test_nso_data(compressed: bool) -> Vec<u8> {
    let mut builder = NsoBuilder::new();
    if compressed {
        builder.compress_mask = 0b111; // text, ro, data all compressed
    }

    // Segments layout:
    // text: mem_off = 0, size = 0x1000
    // ro:   mem_off = 0x1000, size = 0x1000
    // data: mem_off = 0x2000, size = 0x1000

    let mut text = vec![0u8; 0x1000];
    let mut ro = vec![0u8; 0x1000];
    let mut data = vec![0u8; 0x1000];

    // MOD0 header at text offset 0x20
    let mod0_offset: u32 = 0x20;
    text[4..8].copy_from_slice(&mod0_offset.to_le_bytes());

    text[mod0_offset as usize..mod0_offset as usize + 4].copy_from_slice(b"MOD0");

    // MOD0 relative offsets:
    // dyn_off: let dynamic section be in rodata at 0x1100 (relative to mod0 = 0x1100 - 0x20 = 0x10E0)
    let dyn_abs: u32 = 0x1100;
    let dyn_rel: i32 = dyn_abs as i32 - mod0_offset as i32;

    // bss_start: 0x3000 (relative to mod0 = 0x3000 - 0x20 = 0x2FE0)
    let bss_start_abs: u32 = 0x3000;
    let bss_start_rel: i32 = bss_start_abs as i32 - mod0_offset as i32;

    // bss_end: 0x3800
    let bss_end_abs: u32 = 0x3800;
    let bss_end_rel: i32 = bss_end_abs as i32 - mod0_offset as i32;

    // eh_start: 0x1050
    let eh_start_abs: u32 = 0x1050;
    let eh_start_rel: i32 = eh_start_abs as i32 - mod0_offset as i32;

    // eh_end: 0x1080
    let eh_end_abs: u32 = 0x1080;
    let eh_end_rel: i32 = eh_end_abs as i32 - mod0_offset as i32;

    let mod0_fields = [
        dyn_rel,
        bss_start_rel,
        bss_end_rel,
        eh_start_rel,
        eh_end_rel,
    ];
    for (i, &val) in mod0_fields.iter().enumerate() {
        let off = (mod0_offset as usize) + 4 + i * 4;
        text[off..off + 4].copy_from_slice(&val.to_le_bytes());
    }

    // In rodata (mem_off = 0x1000):
    // symtab: 0x1200
    // strtab: 0x1248 (symtab sits right before strtab; 3 symbols * 24 = 72 = 0x48 bytes)
    // rela: 0x1300 (1 entry = 24 bytes)
    // jmprel: 0x1318 (1 entry = 24 bytes)
    // relr: 0x1330 (2 entries = 16 bytes)
    // init_array: 0x1350 (1 entry = 8 bytes)
    // fini_array: 0x1358 (1 entry = 8 bytes)

    let symtab_addr: u64 = 0x1200;
    let strtab_addr: u64 = 0x1248;
    let rela_addr: u64 = 0x1300;
    let jmprel_addr: u64 = 0x1318;
    let relr_addr: u64 = 0x1330;
    let init_arr_addr: u64 = 0x1350;
    let fini_arr_addr: u64 = 0x1358;

    // String table:
    // 0: "\0"
    // 1: "my_func\0"
    // 9: "imported_func\0"
    let mut strtab_bytes = Vec::new();
    strtab_bytes.push(0u8);
    let my_func_str_idx = strtab_bytes.len() as u32;
    strtab_bytes.extend_from_slice(b"my_func\0");
    let imported_str_idx = strtab_bytes.len() as u32;
    strtab_bytes.extend_from_slice(b"imported_func\0");

    let strtab_ro_off = (strtab_addr - 0x1000) as usize;
    ro[strtab_ro_off..strtab_ro_off + strtab_bytes.len()].copy_from_slice(&strtab_bytes);

    // Symbol table: 3 symbols
    // Sym 0: null symbol
    // Sym 1: defined function `my_func`, value = 0x100, size = 0x20, STT_FUNC (2), STB_GLOBAL (1) -> st_info = 0x12, shndx = 1
    // Sym 2: imported function `imported_func`, value = 0, size = 0, STT_FUNC (2), STB_GLOBAL (1) -> st_info = 0x12, shndx = SHN_UNDEF (0)
    let symtab_ro_off = (symtab_addr - 0x1000) as usize;

    // Sym 1:
    let s1_off = symtab_ro_off + 24;
    ro[s1_off..s1_off + 4].copy_from_slice(&my_func_str_idx.to_le_bytes());
    ro[s1_off + 4] = (1 << 4) | STT_FUNC; // info: bind 1, type 2
    ro[s1_off + 5] = 0; // other
    ro[s1_off + 6..s1_off + 8].copy_from_slice(&1u16.to_le_bytes()); // shndx = 1
    ro[s1_off + 8..s1_off + 16].copy_from_slice(&0x100u64.to_le_bytes()); // value
    ro[s1_off + 16..s1_off + 24].copy_from_slice(&0x20u64.to_le_bytes()); // size

    // Sym 2:
    let s2_off = symtab_ro_off + 48;
    ro[s2_off..s2_off + 4].copy_from_slice(&imported_str_idx.to_le_bytes());
    ro[s2_off + 4] = (1 << 4) | STT_FUNC;
    ro[s2_off + 5] = 0;
    ro[s2_off + 6..s2_off + 8].copy_from_slice(&SHN_UNDEF.to_le_bytes()); // shndx = 0 (imported)
    ro[s2_off + 8..s2_off + 16].copy_from_slice(&0u64.to_le_bytes());
    ro[s2_off + 16..s2_off + 24].copy_from_slice(&0u64.to_le_bytes());

    // RELA table (1 entry: R_AARCH64_GLOB_DAT at 0x2008 for sym 2)
    let rela_ro_off = (rela_addr - 0x1000) as usize;
    let rela_info: u64 = ((2u64) << 32) | (R_AARCH64_GLOB_DAT as u64);
    ro[rela_ro_off..rela_ro_off + 8].copy_from_slice(&0x2008u64.to_le_bytes()); // offset
    ro[rela_ro_off + 8..rela_ro_off + 16].copy_from_slice(&rela_info.to_le_bytes()); // info
    ro[rela_ro_off + 16..rela_ro_off + 24].copy_from_slice(&0i64.to_le_bytes()); // addend

    // JMPREL table (1 entry: R_AARCH64_JUMP_SLOT at 0x2010 for sym 2)
    let jmprel_ro_off = (jmprel_addr - 0x1000) as usize;
    let jmprel_info: u64 = ((2u64) << 32) | (R_AARCH64_JUMP_SLOT as u64);
    ro[jmprel_ro_off..jmprel_ro_off + 8].copy_from_slice(&0x2010u64.to_le_bytes());
    ro[jmprel_ro_off + 8..jmprel_ro_off + 16].copy_from_slice(&jmprel_info.to_le_bytes());
    ro[jmprel_ro_off + 16..jmprel_ro_off + 24].copy_from_slice(&0i64.to_le_bytes());

    // RELR table:
    // entry 0: address 0x2020 (even word -> R_AARCH64_RELATIVE at 0x2020, where = 0x2028)
    // entry 1: bitmap with bit 1 set (odd word -> R_AARCH64_RELATIVE at 0x2028)
    let relr_ro_off = (relr_addr - 0x1000) as usize;
    ro[relr_ro_off..relr_ro_off + 8].copy_from_slice(&0x2020u64.to_le_bytes());
    ro[relr_ro_off + 8..relr_ro_off + 16].copy_from_slice(&0x03u64.to_le_bytes()); // bit 0 (odd indicator) | bit 1 (reloc at where + 0)

    // Data segment initial values for the RELR targets:
    // At 0x2020 (offset 0x20 in data): initial value 0x5555
    data[0x20..0x28].copy_from_slice(&0x5555u64.to_le_bytes());
    // At 0x2028 (offset 0x28 in data): initial value 0x6666
    data[0x28..0x30].copy_from_slice(&0x6666u64.to_le_bytes());

    // init_array: entry at 0x1350 pointing to 0x100 (which will be R_AARCH64_RELATIVE relocated)
    // We add an R_AARCH64_RELATIVE relocation at 0x1350 in data or rodata!
    // But wait, DT_INIT_ARRAY has 1 entry. Let's make it hold 0x4321 initially.
    let init_ro_off = (init_arr_addr - 0x1000) as usize;
    ro[init_ro_off..init_ro_off + 8].copy_from_slice(&0x4321u64.to_le_bytes());

    // fini_array: 1 entry with 0x9876
    let fini_ro_off = (fini_arr_addr - 0x1000) as usize;
    ro[fini_ro_off..fini_ro_off + 8].copy_from_slice(&0x9876u64.to_le_bytes());

    // Dynamic section:
    let dyn_tags: Vec<(i64, u64)> = vec![
        (DT_STRTAB, strtab_addr),
        (DT_STRSZ, strtab_bytes.len() as u64),
        (DT_SYMTAB, symtab_addr),
        (DT_SYMENT, 24),
        (DT_RELA, rela_addr),
        (DT_RELASZ, 24),
        (DT_JMPREL, jmprel_addr),
        (DT_PLTRELSZ, 24),
        (DT_RELR, relr_addr),
        (DT_RELRSZ, 16),
        (DT_INIT_ARRAY, init_arr_addr),
        (DT_INIT_ARRAYSZ, 8),
        (DT_FINI_ARRAY, fini_arr_addr),
        (DT_FINI_ARRAYSZ, 8),
        (DT_NULL, 0),
    ];

    let dyn_ro_off = (dyn_abs - 0x1000) as usize;
    for (i, (tag, val)) in dyn_tags.iter().enumerate() {
        let entry_off = dyn_ro_off + i * 16;
        ro[entry_off..entry_off + 8].copy_from_slice(&tag.to_le_bytes());
        ro[entry_off + 8..entry_off + 16].copy_from_slice(&val.to_le_bytes());
    }

    builder.text = text;
    builder.ro = ro;
    builder.data = data;
    builder.build()
}

#[test]
fn load_uncompressed_nso() {
    let raw = create_test_nso_data(false);
    let m = load_bytes(&raw, Some("test_mod")).unwrap();

    assert_eq!(m.name, "test_mod");
    assert_eq!(m.text, (0, 0x1000));
    assert_eq!(m.ro, (0x1000, 0x1000));
    assert_eq!(m.data, (0x2000, 0x1000));

    // BSS and EH frame from MOD0
    assert_eq!(m.bss, (0x3000, 0x800)); // 0x3800 - 0x3000 = 0x800
    assert_eq!(m.eh_frame_hdr, (0x1050, 0x30)); // 0x1080 - 0x1050 = 0x30

    // Dynamic tags
    assert_eq!(m.dynamic.get(&DT_STRTAB), Some(&0x1248));
    assert_eq!(m.dynamic.get(&DT_SYMTAB), Some(&0x1200));
    assert_eq!(m.dynamic.get(&DT_SYMENT), Some(&24));

    // Symbols
    assert_eq!(m.symbols.len(), 3);
    assert_eq!(m.symbols[0].name, "");
    assert_eq!(m.symbols[1].name, "my_func");
    assert_eq!(m.symbols[1].value, 0x100);
    assert_eq!(m.symbols[1].size, 0x20);
    assert_eq!(m.symbols[1].r#type, STT_FUNC);
    assert_eq!(m.symbols[1].bind, 1);
    assert_eq!(m.symbols[1].shndx, 1);
    assert!(!m.symbols[1].imported());

    assert_eq!(m.symbols[2].name, "imported_func");
    assert_eq!(m.symbols[2].value, 0);
    assert_eq!(m.symbols[2].shndx, SHN_UNDEF);
    assert!(m.symbols[2].imported());

    // Relocations: 1 RELA + 1 JMPREL + 2 RELR = 4
    assert_eq!(m.relocs.len(), 4);

    // RELA entry
    assert_eq!(m.relocs[0].offset, 0x2008);
    assert_eq!(m.relocs[0].r#type, R_AARCH64_GLOB_DAT);
    assert_eq!(m.relocs[0].sym, 2);

    // JMPREL entry
    assert_eq!(m.relocs[1].offset, 0x2010);
    assert_eq!(m.relocs[1].r#type, R_AARCH64_JUMP_SLOT);
    assert_eq!(m.relocs[1].sym, 2);

    // RELR entries
    assert_eq!(m.relocs[2].offset, 0x2020);
    assert_eq!(m.relocs[2].r#type, R_AARCH64_RELATIVE);
    assert_eq!(m.relocs[2].addend, 0x5555);

    assert_eq!(m.relocs[3].offset, 0x2028);
    assert_eq!(m.relocs[3].r#type, R_AARCH64_RELATIVE);
    assert_eq!(m.relocs[3].addend, 0x6666);

    // init_array and fini_array
    assert_eq!(m.init_array, vec![0x4321]);
    assert_eq!(m.fini_array, vec![0x9876]);

    // PLT imports: offset -> imported symbol name
    let imports = plt_imports(&m);
    assert_eq!(imports.len(), 2);
    assert_eq!(imports.get(&0x2008), Some(&"imported_func".to_string()));
    assert_eq!(imports.get(&0x2010), Some(&"imported_func".to_string()));

    // Helpers
    assert!(m.in_text(0x500));
    assert!(!m.in_text(0x1500));
    assert!(m.in_image(0x2FFF));
    assert!(!m.in_image(0x5000));
    assert_eq!(m.u32(4), 0x20);
    assert_eq!(m.cstr(0x1248 + 1), "my_func");
}

#[test]
fn load_lz4_compressed_nso() {
    let raw = create_test_nso_data(true);
    let m = load_bytes(&raw, Some("compressed_mod")).unwrap();

    assert_eq!(m.name, "compressed_mod");
    assert_eq!(m.text, (0, 0x1000));
    assert_eq!(m.ro, (0x1000, 0x1000));
    assert_eq!(m.data, (0x2000, 0x1000));
    assert_eq!(m.symbols.len(), 3);
    assert_eq!(m.symbols[1].name, "my_func");
    assert_eq!(m.symbols[2].name, "imported_func");
    assert_eq!(m.relocs.len(), 4);
}

#[test]
fn test_error_handling() {
    // Too short
    assert!(matches!(
        load_bytes(&[0; 10], None),
        Err(Error::Malformed(_))
    ));

    // Bad magic
    let mut bad_magic = vec![0u8; 0x100];
    bad_magic[0..4].copy_from_slice(b"BAD0");
    assert!(matches!(
        load_bytes(&bad_magic, None),
        Err(Error::NotAnNso(_))
    ));

    // Missing MOD0
    let mut bad_mod0 = vec![0u8; 0x100];
    bad_mod0[0..4].copy_from_slice(b"NSO0");
    assert!(matches!(
        load_bytes(&bad_mod0, None),
        Err(Error::Mod0NotFound)
    ));

    // File not found
    assert!(matches!(
        load(Path::new("non_existent_file.nso"), None),
        Err(Error::Io(_))
    ));
}

#[test]
fn test_lz4_decompress_block() {
    let original = b"The quick brown fox jumps over the lazy dog. 1234567890";
    let compressed = lz4_compress_literals(original);
    let decompressed = decompress_lz4_block(&compressed, original.len()).unwrap();
    assert_eq!(decompressed, original);

    // Size mismatch error
    let err = decompress_lz4_block(&compressed, original.len() + 10);
    assert!(matches!(err, Err(Error::Decompress(_))));
}

/// A header whose segment fields ask for gigabytes must fail, not allocate them.
#[test]
fn oversized_segment_fields_are_malformed() {
    let mut raw = create_test_nso_data(false);
    // Text segment: memory offset 0xFFFF_0000 puts the image far past any real module.
    raw[0x14..0x18].copy_from_slice(&0xFFFF_0000u32.to_le_bytes());
    assert!(matches!(load_bytes(&raw, None), Err(Error::Malformed(_))));

    let mut raw = create_test_nso_data(true);
    // Compressed text claiming 4 GiB - 1 of output from a few bytes.
    raw[0x18..0x1C].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(load_bytes(&raw, None).is_err());
}

/// A dynamic section that puts DT_STRTAB far past DT_SYMTAB (with DT_SYMENT 1) must not walk billions
/// of symbol slots.
#[test]
fn hostile_symbol_table_bounds_return_quickly() {
    let raw = create_test_nso_data(false);
    let mut m = load_bytes(&raw, None).unwrap();
    let set_dyn = |m: &mut acnh_recomp::load::Module, tag: i64, old: u64, new: u64| {
        let mut key = tag.to_le_bytes().to_vec();
        key.extend_from_slice(&old.to_le_bytes());
        let at = m
            .image
            .windows(16)
            .position(|w| w == key.as_slice())
            .unwrap();
        m.image[at + 8..at + 16].copy_from_slice(&new.to_le_bytes());
    };
    set_dyn(&mut m, DT_STRTAB, 0x1248, u64::MAX / 2);
    set_dyn(&mut m, DT_SYMENT, 24, 1);
    m.symbols.clear();
    let started = std::time::Instant::now();
    acnh_recomp::load::parse_mod0(&mut m).unwrap();
    assert!(started.elapsed().as_secs() < 5);
    // Only the slots inside the image were read.
    assert!(m.symbols.len() <= m.image.len());
}
