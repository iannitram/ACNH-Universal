//! acnh-recomp: the static recompiler in Rust (SPEC.md M13; docs/adr/0002-runtime-strangler.md).
//! It replaces upstream's `recomp/` (Python: `python3 -m recomp`), which turns `exefs/main`
//! (and `sdk`, `subsdk0`) into the C++ the game library is built from. Ported pass by pass;
//! the Rust output must match the Python output byte for byte on the same input (hand-written
//! AArch64, never game code), and `tests/difftest.py` stays the oracle. Prepare runs
//! upstream's driver (`prepare::Step::Script`) until the full emitter and Rust driver land.
//!
//! It reads my game folder on a hub and writes generated code beside the build; neither ever
//! enters this repo or a bundle (M14).

pub mod analyse;
pub mod decode;
pub mod ehframe;
pub mod emit;
pub mod load;

pub use load::{
    apply_relative, load, load_bytes, load_file, plt_imports, Error, Module, Reloc, Symbol,
    DT_FINI, DT_FINI_ARRAY, DT_FINI_ARRAYSZ, DT_GNU_HASH, DT_HASH, DT_INIT, DT_INIT_ARRAY,
    DT_INIT_ARRAYSZ, DT_JMPREL, DT_NEEDED, DT_NULL, DT_PLTGOT, DT_PLTRELSZ, DT_REL, DT_RELA,
    DT_RELAENT, DT_RELASZ, DT_RELR, DT_RELRENT, DT_RELRSZ, DT_STRSZ, DT_STRTAB, DT_SYMENT,
    DT_SYMTAB, R_AARCH64_ABS64, R_AARCH64_GLOB_DAT, R_AARCH64_JUMP_SLOT, R_AARCH64_RELATIVE,
    SHN_UNDEF, STT_FUNC,
};

/// The recompiler's passes, in the order they run, with the Python they replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    /// Read the NSO: segments, symbols, relocations (`recomp/nso.py`).
    Load,
    /// Find functions and their bounds (`recomp/analysis.py`, `recomp/ehframe.py`).
    Analyse,
    /// Complete AArch64 to C++ emission, including the floating-point and SIMD pass (3.04).
    Emit,
}

/// Every pass, in order.
pub const PASSES: [Pass; 3] = [Pass::Load, Pass::Analyse, Pass::Emit];

/// True once a pass is ported to Rust; false sends it to upstream's tool.
pub fn ported(pass: Pass) -> bool {
    matches!(pass, Pass::Load | Pass::Analyse)
}
