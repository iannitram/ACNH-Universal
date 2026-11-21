//! Switch NSO loader: segments, MOD0, .dynamic, symbols and relocations (recomp/nso.py).
//!
//! Addresses are module-relative (the module is linked at 0 and relocated by rtld).

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

// Dynamic tags
pub const DT_NULL: i64 = 0;
pub const DT_NEEDED: i64 = 1;
pub const DT_PLTRELSZ: i64 = 2;
pub const DT_PLTGOT: i64 = 3;
pub const DT_HASH: i64 = 4;
pub const DT_STRTAB: i64 = 5;
pub const DT_SYMTAB: i64 = 6;
pub const DT_RELA: i64 = 7;
pub const DT_RELASZ: i64 = 8;
pub const DT_RELAENT: i64 = 9;
pub const DT_STRSZ: i64 = 10;
pub const DT_SYMENT: i64 = 11;
pub const DT_INIT: i64 = 12;
pub const DT_FINI: i64 = 13;
pub const DT_REL: i64 = 17;
pub const DT_JMPREL: i64 = 23;
pub const DT_INIT_ARRAY: i64 = 25;
pub const DT_FINI_ARRAY: i64 = 26;
pub const DT_INIT_ARRAYSZ: i64 = 27;
pub const DT_FINI_ARRAYSZ: i64 = 28;
pub const DT_GNU_HASH: i64 = 0x6FFFFEF5;
pub const DT_RELRSZ: i64 = 35;
pub const DT_RELR: i64 = 36;
pub const DT_RELRENT: i64 = 37;

// Relocation types
pub const R_AARCH64_ABS64: u32 = 257;
pub const R_AARCH64_GLOB_DAT: u32 = 1025;
pub const R_AARCH64_JUMP_SLOT: u32 = 1026;
pub const R_AARCH64_RELATIVE: u32 = 1027;

// Symbol types and sections
pub const STT_FUNC: u8 = 2;
pub const SHN_UNDEF: u16 = 0;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Io(String),
    NotAnNso(String),
    Mod0NotFound,
    Decompress(String),
    Malformed(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::NotAnNso(msg) => write!(f, "not an NSO: {msg}"),
            Error::Mod0NotFound => write!(f, "MOD0 not found"),
            Error::Decompress(msg) => write!(f, "decompression failed: {msg}"),
            Error::Malformed(msg) => write!(f, "malformed NSO: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub value: u64,
    pub size: u64,
    pub r#type: u8,
    pub bind: u8,
    pub shndx: u16,
}

impl Symbol {
    pub fn imported(&self) -> bool {
        self.shndx == SHN_UNDEF
    }

    pub fn is_imported(&self) -> bool {
        self.imported()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reloc {
    pub offset: u64,
    pub r#type: u32,
    pub sym: u32,
    pub addend: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Module {
    pub name: String,
    pub image: Vec<u8>,
    pub text: (u64, u64),
    pub ro: (u64, u64),
    pub data: (u64, u64),
    pub bss: (u64, u64),
    pub symbols: Vec<Symbol>,
    pub relocs: Vec<Reloc>,
    pub dynamic: BTreeMap<i64, u64>,
    pub init_array: Vec<u64>,
    pub fini_array: Vec<u64>,
    pub eh_frame_hdr: (u64, u64),
}

impl Module {
    pub fn u32(&self, addr: u64) -> u32 {
        let a = addr as usize;
        if a.saturating_add(4) <= self.image.len() {
            let bytes = [
                self.image[a],
                self.image[a + 1],
                self.image[a + 2],
                self.image[a + 3],
            ];
            u32::from_le_bytes(bytes)
        } else {
            0
        }
    }

    pub fn u64(&self, addr: u64) -> u64 {
        let a = addr as usize;
        if a.saturating_add(8) <= self.image.len() {
            let bytes = [
                self.image[a],
                self.image[a + 1],
                self.image[a + 2],
                self.image[a + 3],
                self.image[a + 4],
                self.image[a + 5],
                self.image[a + 6],
                self.image[a + 7],
            ];
            u64::from_le_bytes(bytes)
        } else {
            0
        }
    }

    pub fn in_text(&self, addr: u64) -> bool {
        self.text.0 <= addr && addr < self.text.0.saturating_add(self.text.1)
    }

    pub fn in_image(&self, addr: u64) -> bool {
        addr < self.image.len() as u64
    }

    pub fn cstr(&self, addr: u64) -> String {
        let a = addr as usize;
        if a >= self.image.len() {
            return String::new();
        }
        let end = match self.image[a..].iter().position(|&b| b == 0) {
            Some(pos) => a + pos,
            None => self.image.len(),
        };
        String::from_utf8_lossy(&self.image[a..end]).into_owned()
    }

    pub fn apply_relative(&mut self) {
        apply_relative(self);
    }

    pub fn plt_imports(&self) -> BTreeMap<u64, String> {
        plt_imports(self)
    }
}

/// Decompress an LZ4 raw block with known uncompressed size (as used in NSO segments).
pub fn decompress_lz4_block(input: &[u8], uncompressed_size: usize) -> Result<Vec<u8>, Error> {
    let mut output = Vec::with_capacity(uncompressed_size);
    let mut ip = 0;

    while ip < input.len() && output.len() < uncompressed_size {
        let token = input[ip];
        ip += 1;

        // High 4 bits: literal length
        let mut lit_len = ((token >> 4) & 0x0F) as usize;
        if lit_len == 15 {
            while ip < input.len() {
                let extra = input[ip];
                ip += 1;
                lit_len = lit_len
                    .checked_add(extra as usize)
                    .ok_or_else(|| Error::Decompress("literal length overflow".into()))?;
                if extra != 255 {
                    break;
                }
            }
        }

        if ip.checked_add(lit_len).is_none_or(|end| end > input.len()) {
            return Err(Error::Decompress("literal length exceeds input".into()));
        }
        if output
            .len()
            .checked_add(lit_len)
            .is_none_or(|end| end > uncompressed_size)
        {
            return Err(Error::Decompress(
                "uncompressed size exceeded by literals".into(),
            ));
        }
        output.extend_from_slice(&input[ip..ip + lit_len]);
        ip += lit_len;

        if ip >= input.len() || output.len() >= uncompressed_size {
            break;
        }

        // Match copy: 2-byte offset
        if ip.saturating_add(2) > input.len() {
            return Err(Error::Decompress("truncated match offset".into()));
        }
        let offset = u16::from_le_bytes([input[ip], input[ip + 1]]) as usize;
        ip += 2;
        if offset == 0 || offset > output.len() {
            return Err(Error::Decompress(format!("invalid match offset {offset}")));
        }

        let mut match_len = ((token & 0x0F) as usize) + 4;
        if (token & 0x0F) == 15 {
            while ip < input.len() {
                let extra = input[ip];
                ip += 1;
                match_len = match_len
                    .checked_add(extra as usize)
                    .ok_or_else(|| Error::Decompress("match length overflow".into()))?;
                if extra != 255 {
                    break;
                }
            }
        }

        if output
            .len()
            .checked_add(match_len)
            .is_none_or(|end| end > uncompressed_size)
        {
            return Err(Error::Decompress(
                "uncompressed size exceeded by match".into(),
            ));
        }

        let start = output.len() - offset;
        for i in 0..match_len {
            let b = output[start + i];
            output.push(b);
        }
    }

    if output.len() != uncompressed_size {
        return Err(Error::Decompress(format!(
            "decompressed size mismatch: got {} bytes, expected {uncompressed_size}",
            output.len()
        )));
    }

    Ok(output)
}

fn read_u32_le(buf: &[u8], offset: usize) -> u32 {
    if offset.saturating_add(4) <= buf.len() {
        u32::from_le_bytes([
            buf[offset],
            buf[offset + 1],
            buf[offset + 2],
            buf[offset + 3],
        ])
    } else {
        0
    }
}

pub fn load_bytes(raw: &[u8], name: Option<&str>) -> Result<Module, Error> {
    if raw.len() < 0x6C {
        return Err(Error::Malformed("NSO file too short for header".into()));
    }
    if &raw[..4] != b"NSO0" {
        return Err(Error::NotAnNso("missing NSO0 magic".into()));
    }

    let flags = read_u32_le(raw, 0x0C);
    let mut segs = Vec::with_capacity(3);

    for i in 0..3 {
        let seg_hdr = 0x10 + i * 0x10;
        let file_off = read_u32_le(raw, seg_hdr) as usize;
        let mem_off = read_u32_le(raw, seg_hdr + 4) as usize;
        let size = read_u32_le(raw, seg_hdr + 8) as usize;
        let comp = read_u32_le(raw, 0x60 + i * 4) as usize;

        if file_off.checked_add(comp).is_none_or(|end| end > raw.len()) {
            return Err(Error::Malformed(format!(
                "segment {i} extends beyond file bounds (offset {file_off:#x}, comp {comp:#x}, file len {:#x})",
                raw.len()
            )));
        }

        let compressed_blob = &raw[file_off..file_off + comp];
        let blob = if flags & (1 << i) != 0 {
            decompress_lz4_block(compressed_blob, size)?
        } else {
            compressed_blob.to_vec()
        };
        segs.push((mem_off, blob));
    }

    let bss_size = read_u32_le(raw, 0x3C) as u64;
    let data_end = segs[2].0.saturating_add(segs[2].1.len());

    let mut total_size = data_end;
    for (off, blob) in &segs {
        total_size = total_size.max(off.saturating_add(blob.len()));
    }

    let mut image = vec![0u8; total_size];
    for (off, blob) in &segs {
        image[*off..*off + blob.len()].copy_from_slice(blob);
    }

    let mut m = Module {
        name: name.unwrap_or("").to_string(),
        image,
        text: (segs[0].0 as u64, segs[0].1.len() as u64),
        ro: (segs[1].0 as u64, segs[1].1.len() as u64),
        data: (segs[2].0 as u64, segs[2].1.len() as u64),
        bss: (data_end as u64, bss_size),
        symbols: Vec::new(),
        relocs: Vec::new(),
        dynamic: BTreeMap::new(),
        init_array: Vec::new(),
        fini_array: Vec::new(),
        eh_frame_hdr: (0, 0),
    };

    parse_mod0(&mut m)?;
    Ok(m)
}

pub fn load(path: impl AsRef<Path>, name: Option<&str>) -> Result<Module, Error> {
    let p = path.as_ref();
    let raw = std::fs::read(p)?;
    let default_name = p.to_string_lossy();
    load_bytes(&raw, Some(name.unwrap_or(&default_name)))
}

pub fn load_file(path: impl AsRef<Path>) -> Result<Module, Error> {
    load(path, None)
}

pub fn parse_mod0(m: &mut Module) -> Result<(), Error> {
    let mod0 = m.u32(4) as usize;
    if mod0.saturating_add(24) > m.image.len() {
        return Err(Error::Mod0NotFound);
    }
    if &m.image[mod0..mod0 + 4] != b"MOD0" {
        return Err(Error::Mod0NotFound);
    }

    let read_mod0_rel = |offset: usize| -> u64 {
        let delta = i32::from_le_bytes([
            m.image[offset],
            m.image[offset + 1],
            m.image[offset + 2],
            m.image[offset + 3],
        ]) as i64;
        (mod0 as i64).saturating_add(delta) as u64
    };

    let dyn_off = read_mod0_rel(mod0 + 4);
    let bss_start = read_mod0_rel(mod0 + 8);
    let bss_end = read_mod0_rel(mod0 + 12);
    let eh_start = read_mod0_rel(mod0 + 16);
    let eh_end = read_mod0_rel(mod0 + 20);

    m.bss = (bss_start, bss_end.saturating_sub(bss_start));
    m.eh_frame_hdr = (eh_start, eh_end.saturating_sub(eh_start));

    let mut dyn_map = BTreeMap::new();
    let mut addr = dyn_off as usize;
    while addr.saturating_add(16) <= m.image.len() {
        let tag = i64::from_le_bytes([
            m.image[addr],
            m.image[addr + 1],
            m.image[addr + 2],
            m.image[addr + 3],
            m.image[addr + 4],
            m.image[addr + 5],
            m.image[addr + 6],
            m.image[addr + 7],
        ]);
        let val = u64::from_le_bytes([
            m.image[addr + 8],
            m.image[addr + 9],
            m.image[addr + 10],
            m.image[addr + 11],
            m.image[addr + 12],
            m.image[addr + 13],
            m.image[addr + 14],
            m.image[addr + 15],
        ]);
        addr += 16;
        if tag == DT_NULL {
            break;
        }
        dyn_map.entry(tag).or_insert(val);
    }
    m.dynamic = dyn_map;

    if let (Some(&strtab), Some(&symtab)) = (m.dynamic.get(&DT_STRTAB), m.dynamic.get(&DT_SYMTAB)) {
        let syment = m.dynamic.get(&DT_SYMENT).copied().unwrap_or(24);
        if syment > 0 && strtab >= symtab {
            let nsyms = (strtab - symtab) / syment;
            for i in 0..nsyms {
                let sym_addr = (symtab + i * syment) as usize;
                if sym_addr.saturating_add(24) <= m.image.len() {
                    let st_name = u32::from_le_bytes([
                        m.image[sym_addr],
                        m.image[sym_addr + 1],
                        m.image[sym_addr + 2],
                        m.image[sym_addr + 3],
                    ]);
                    let st_info = m.image[sym_addr + 4];
                    let _st_other = m.image[sym_addr + 5];
                    let st_shndx =
                        u16::from_le_bytes([m.image[sym_addr + 6], m.image[sym_addr + 7]]);
                    let st_value = u64::from_le_bytes([
                        m.image[sym_addr + 8],
                        m.image[sym_addr + 9],
                        m.image[sym_addr + 10],
                        m.image[sym_addr + 11],
                        m.image[sym_addr + 12],
                        m.image[sym_addr + 13],
                        m.image[sym_addr + 14],
                        m.image[sym_addr + 15],
                    ]);
                    let st_size = u64::from_le_bytes([
                        m.image[sym_addr + 16],
                        m.image[sym_addr + 17],
                        m.image[sym_addr + 18],
                        m.image[sym_addr + 19],
                        m.image[sym_addr + 20],
                        m.image[sym_addr + 21],
                        m.image[sym_addr + 22],
                        m.image[sym_addr + 23],
                    ]);

                    let name = m.cstr(strtab.saturating_add(st_name as u64));
                    m.symbols.push(Symbol {
                        name,
                        value: st_value,
                        size: st_size,
                        r#type: st_info & 0x0F,
                        bind: st_info >> 4,
                        shndx: st_shndx,
                    });
                }
            }
        }
    }

    let parse_relocs = |image: &[u8], start: u64, size: u64, relocs: &mut Vec<Reloc>| {
        let start_a = start as usize;
        let end_a = (start.saturating_add(size)) as usize;
        let mut a = start_a;
        while a.saturating_add(24) <= end_a && a.saturating_add(24) <= image.len() {
            let off = u64::from_le_bytes([
                image[a],
                image[a + 1],
                image[a + 2],
                image[a + 3],
                image[a + 4],
                image[a + 5],
                image[a + 6],
                image[a + 7],
            ]);
            let info = u64::from_le_bytes([
                image[a + 8],
                image[a + 9],
                image[a + 10],
                image[a + 11],
                image[a + 12],
                image[a + 13],
                image[a + 14],
                image[a + 15],
            ]);
            let addend = i64::from_le_bytes([
                image[a + 16],
                image[a + 17],
                image[a + 18],
                image[a + 19],
                image[a + 20],
                image[a + 21],
                image[a + 22],
                image[a + 23],
            ]);
            relocs.push(Reloc {
                offset: off,
                r#type: (info & 0xFFFF_FFFF) as u32,
                sym: (info >> 32) as u32,
                addend,
            });
            a += 24;
        }
    };

    let read_u64_at = |image: &[u8], addr: u64| -> u64 {
        let a = addr as usize;
        if a.saturating_add(8) <= image.len() {
            u64::from_le_bytes([
                image[a],
                image[a + 1],
                image[a + 2],
                image[a + 3],
                image[a + 4],
                image[a + 5],
                image[a + 6],
                image[a + 7],
            ])
        } else {
            0
        }
    };

    let parse_relr = |image: &[u8], start: u64, size: u64, relocs: &mut Vec<Reloc>| {
        let start_a = start as usize;
        let end_a = (start.saturating_add(size)) as usize;
        let mut where_addr: u64 = 0;
        let mut a = start_a;
        while a.saturating_add(8) <= end_a && a.saturating_add(8) <= image.len() {
            let e = u64::from_le_bytes([
                image[a],
                image[a + 1],
                image[a + 2],
                image[a + 3],
                image[a + 4],
                image[a + 5],
                image[a + 6],
                image[a + 7],
            ]);
            if e & 1 == 0 {
                relocs.push(Reloc {
                    offset: e,
                    r#type: R_AARCH64_RELATIVE,
                    sym: 0,
                    addend: read_u64_at(image, e) as i64,
                });
                where_addr = e.wrapping_add(8);
            } else {
                for k in 1..64 {
                    if (e >> k) & 1 == 1 {
                        let off = where_addr.wrapping_add((k - 1) * 8);
                        relocs.push(Reloc {
                            offset: off,
                            r#type: R_AARCH64_RELATIVE,
                            sym: 0,
                            addend: read_u64_at(image, off) as i64,
                        });
                    }
                }
                where_addr = where_addr.wrapping_add(63 * 8);
            }
            a += 8;
        }
    };

    if let (Some(&rela), Some(&relasz)) = (m.dynamic.get(&DT_RELA), m.dynamic.get(&DT_RELASZ)) {
        parse_relocs(&m.image, rela, relasz, &mut m.relocs);
    }
    if let (Some(&jmprel), Some(&pltrelsz)) =
        (m.dynamic.get(&DT_JMPREL), m.dynamic.get(&DT_PLTRELSZ))
    {
        parse_relocs(&m.image, jmprel, pltrelsz, &mut m.relocs);
    }
    if let (Some(&relr_off), Some(&relrsz)) = (m.dynamic.get(&DT_RELR), m.dynamic.get(&DT_RELRSZ)) {
        parse_relr(&m.image, relr_off, relrsz, &mut m.relocs);
    }

    let mut rel_at = std::collections::HashMap::new();
    for r in &m.relocs {
        rel_at.insert(r.offset, r);
    }

    let parse_ptr_array = |image: &[u8], start: u64, size: u64| -> Vec<u64> {
        let mut out = Vec::new();
        let start_a = start as usize;
        let end_a = (start.saturating_add(size)) as usize;
        let mut a = start_a;
        while a.saturating_add(8) <= end_a && a.saturating_add(8) <= image.len() {
            let addr_u64 = a as u64;
            let val = if let Some(r) = rel_at.get(&addr_u64) {
                if r.r#type == R_AARCH64_RELATIVE {
                    r.addend as u64
                } else {
                    read_u64_at(image, addr_u64)
                }
            } else {
                read_u64_at(image, addr_u64)
            };
            out.push(val);
            a += 8;
        }
        out
    };

    if let (Some(&init_arr), Some(&init_arr_sz)) = (
        m.dynamic.get(&DT_INIT_ARRAY),
        m.dynamic.get(&DT_INIT_ARRAYSZ),
    ) {
        m.init_array = parse_ptr_array(&m.image, init_arr, init_arr_sz);
    }
    if let (Some(&fini_arr), Some(&fini_arr_sz)) = (
        m.dynamic.get(&DT_FINI_ARRAY),
        m.dynamic.get(&DT_FINI_ARRAYSZ),
    ) {
        m.fini_array = parse_ptr_array(&m.image, fini_arr, fini_arr_sz);
    }

    Ok(())
}

pub fn apply_relative(m: &mut Module) {
    for r in &m.relocs {
        if r.r#type == R_AARCH64_RELATIVE {
            let off = r.offset as usize;
            if off.saturating_add(8) <= m.image.len() {
                let bytes = (r.addend as u64).to_le_bytes();
                m.image[off..off + 8].copy_from_slice(&bytes);
            }
        }
    }
}

pub fn plt_imports(m: &Module) -> BTreeMap<u64, String> {
    let mut out = BTreeMap::new();
    for r in &m.relocs {
        if matches!(
            r.r#type,
            R_AARCH64_JUMP_SLOT | R_AARCH64_GLOB_DAT | R_AARCH64_ABS64
        ) && r.sym != 0
        {
            if let Some(s) = m.symbols.get(r.sym as usize) {
                if s.imported() {
                    out.insert(r.offset, s.name.clone());
                }
            }
        }
    }
    out
}
