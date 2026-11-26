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

pub use driver::{recompile, RecompileOptions, Report, INSNS_PER_FILE, REF_BASE};
pub use load::{
    load, load_bytes, load_file, plt_imports, Error, Module, Reloc, Symbol, DT_FINI, DT_FINI_ARRAY,
    DT_FINI_ARRAYSZ, DT_GNU_HASH, DT_HASH, DT_INIT, DT_INIT_ARRAY, DT_INIT_ARRAYSZ, DT_JMPREL,
    DT_NEEDED, DT_NULL, DT_PLTGOT, DT_PLTRELSZ, DT_REL, DT_RELA, DT_RELAENT, DT_RELASZ, DT_RELR,
    DT_RELRENT, DT_RELRSZ, DT_STRSZ, DT_STRTAB, DT_SYMENT, DT_SYMTAB, R_AARCH64_ABS64,
    R_AARCH64_GLOB_DAT, R_AARCH64_JUMP_SLOT, R_AARCH64_RELATIVE, SHN_UNDEF, STT_FUNC,
};
