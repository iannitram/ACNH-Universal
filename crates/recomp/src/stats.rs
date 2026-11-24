//! Module overview and instruction histogram (port of `recomp/stats.py`).

use crate::driver::DriverError;
use crate::load::{load_file, plt_imports};
use capstone::arch::arm64;
use capstone::prelude::*;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

const VECTOR_SUFFIXES: &[&str] = &[
    ".4s", ".2s", ".8b", ".16b", ".4h", ".8h", ".2d", ".1d", ".s[", ".d[", ".h[", ".b[",
];

/// Write an overview and instruction histogram for the NSO at `nso_path` to `out`.
pub fn stats(nso_path: &Path, out: &mut dyn Write) -> Result<(), DriverError> {
    let m = load_file(nso_path)?;
    writeln!(
        out,
        "text 0x{:x}+0x{:x}  ro 0x{:x}+0x{:x}  data 0x{:x}+0x{:x}  bss 0x{:x}+0x{:x}",
        m.text.0, m.text.1, m.ro.0, m.ro.1, m.data.0, m.data.1, m.bss.0, m.bss.1
    )?;
    let imports = m
        .symbols
        .iter()
        .filter(|s| s.imported() && !s.name.is_empty())
        .count();
    let exports = m
        .symbols
        .iter()
        .filter(|s| !s.imported() && !s.name.is_empty())
        .count();
    writeln!(
        out,
        "symbols {}: imported {}, defined {}",
        m.symbols.len(),
        imports,
        exports
    )?;
    let mut kinds: BTreeMap<u32, usize> = BTreeMap::new();
    for r in &m.relocs {
        *kinds.entry(r.r#type).or_default() += 1;
    }
    let kinds_str = kinds
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(out, "relocs {{{kinds_str}}}")?;
    writeln!(
        out,
        "init_array {}  fini_array {}  eh_frame_hdr 0x{:x}",
        m.init_array.len(),
        m.fini_array.len(),
        m.eh_frame_hdr.0
    )?;
    writeln!(out, "plt/got imports {}", plt_imports(&m).len())?;

    let mut cs = Capstone::new()
        .arm64()
        .mode(arm64::ArchMode::Arm)
        .build()
        .map_err(|e| DriverError::Message(format!("capstone init: {e}")))?;
    cs.set_skipdata(true)
        .map_err(|e| DriverError::Message(format!("capstone skipdata: {e}")))?;

    let (start, size) = m.text;
    let end = (start as usize).saturating_add(size as usize);
    let code = m.image.get(start as usize..end).unwrap_or(&[]);
    let insns = cs
        .disasm_all(code, start)
        .map_err(|e| DriverError::Message(format!("capstone disasm: {e}")))?;

    let mut hist: BTreeMap<String, usize> = BTreeMap::new();
    let mut invalid = 0usize;
    for insn in insns.as_ref() {
        let mnem = insn.mnemonic().unwrap_or("");
        if mnem == ".byte" {
            invalid += 1;
            continue;
        }
        let ops = insn.op_str().unwrap_or("");
        let mut key = mnem.to_owned();
        if ops.contains('v') && VECTOR_SUFFIXES.iter().any(|t| ops.contains(t)) {
            key.push_str(".v");
        }
        *hist.entry(key).or_default() += 1;
    }

    let total: usize = hist.values().sum();
    writeln!(
        out,
        "instructions {total}, undecodable words {invalid}, distinct mnemonics {}",
        hist.len()
    )?;
    let mut sorted: Vec<(&String, &usize)> = hist.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    for (mnem, n) in sorted {
        writeln!(out, "{n:9} {mnem}")?;
    }
    Ok(())
}
