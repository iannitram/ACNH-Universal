//! acnh-recomp: the static recompiler in Rust (SPEC.md M13; docs/adr/0002-runtime-strangler.md).
//! It replaces upstream's `recomp/` (Python: `python3 -m recomp`), which turns `exefs/main`
//! (and `sdk`, `subsdk0`) into the C++ the game library is built from. Ported pass by pass;
//! the Rust output must match the Python output byte for byte on the same input (hand-written
//! AArch64, never game code), and `tests/difftest.py` stays the oracle. Until a pass is
//! ported, `prepare` runs upstream's tool (`prepare::Step::Script`). Planned: only the pass
//! list is in the skeleton.
//!
//! It reads my game folder on a hub and writes generated code beside the build; neither ever
//! enters this repo or a bundle (M14).

/// The recompiler's passes, in the order they run, with the Python they replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    /// Read the NSO: segments, symbols, relocations (`recomp/nso.py`).
    Load,
    /// Find functions and their bounds (`recomp/analysis.py`, `recomp/ehframe.py`).
    Analyse,
    /// AArch64 to C++ (`recomp/emit*.py`, `recomp/regcache.py`).
    Emit,
}

/// Every pass, in order.
pub const PASSES: [Pass; 3] = [Pass::Load, Pass::Analyse, Pass::Emit];

/// True once a pass is ported to Rust; false sends it to upstream's tool. None are yet.
pub fn ported(pass: Pass) -> bool {
    let _ = pass;
    false
}
