//! acnh-recomp: the static recompiler in Rust (SPEC.md M13; docs/adr/0002-runtime-strangler.md).
//! It replaces upstream's `recomp/` (Python), turning `exefs/main` (and `sdk`, `subsdk0`) into
//! the C++ the game library is built from. `tests/difftest.py` (Unicorn) is the execution oracle.
//!
//! It reads my game folder on a hub and writes generated code beside the build; neither ever
//! enters this repo or a bundle (M14).

pub mod analyse;
pub mod decode;
pub mod driver;
pub mod ehframe;
pub mod emit;
pub mod load;
pub mod stats;

/// SHA-256 of this crate's `src/` tree, taken when it was built (`build.rs`): it changes with
/// any change to the emitter, so a `gen/` made by an older one can be told from a current one.
pub const EMITTER_FINGERPRINT: &str = env!("ACNH_RECOMP_FINGERPRINT");

pub use driver::{recompile, RecompileOptions, Report, INSNS_PER_FILE, REF_BASE};
pub use load::{
    apply_relative, load, load_bytes, load_file, plt_imports, Error, Module, Reloc, Symbol,
    DT_FINI, DT_FINI_ARRAY, DT_FINI_ARRAYSZ, DT_GNU_HASH, DT_HASH, DT_INIT, DT_INIT_ARRAY,
    DT_INIT_ARRAYSZ, DT_JMPREL, DT_NEEDED, DT_NULL, DT_PLTGOT, DT_PLTRELSZ, DT_REL, DT_RELA,
    DT_RELAENT, DT_RELASZ, DT_RELR, DT_RELRENT, DT_RELRSZ, DT_STRSZ, DT_STRTAB, DT_SYMENT,
    DT_SYMTAB, R_AARCH64_ABS64, R_AARCH64_GLOB_DAT, R_AARCH64_JUMP_SLOT, R_AARCH64_RELATIVE,
    SHN_UNDEF, STT_FUNC,
};

/// The recompiler's passes, in the order they run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    /// Read the NSO: segments, symbols, relocations.
    Load,
    /// Find functions and their bounds.
    Analyse,
    /// Complete AArch64 to C++ emission, including the floating-point and SIMD pass (3.04).
    Emit,
    /// Whole-module driver: chunking, parallel emission, headers, module table, variants, stats (3.05).
    Driver,
}

/// Every pass, in order.
pub const PASSES: [Pass; 4] = [Pass::Load, Pass::Analyse, Pass::Emit, Pass::Driver];

/// True once a pass is ported to Rust; false sends it to upstream's tool.
pub fn ported(pass: Pass) -> bool {
    matches!(pass, Pass::Load | Pass::Analyse | Pass::Emit | Pass::Driver)
}
