# 0002: The prep tools, then the recompiler, then the runtime move to Rust, one piece at a time

**Decided:** 2026-11-02, following ADR 0001 (`SPEC.md`, Implementation decisions).

## Decision

Strangler order. At every step the game still builds from my game folder and boots.

1. **Prep tools** (stage 1): `tools/setup_acnh.py` and the scripts it runs (`acnh_version.py`, `nso2bin.py`, `pack_romfs.py`, `pack_spirv.py`, `extract_shaders.py`, `sarc_endian.py`, `msg_rewrap.py`) become `crates/core` modules, driven by `prepare`. Until a stage is ported, `prepare` runs upstream's script for it (`prepare::Step::Script`).
2. **Recompiler** (stage 3): `recomp/` (Python) becomes `crates/recomp`, pass by pass (load, analyse, emit). Its output must match the Python output byte for byte on hand-written AArch64 (never game code); `tests/difftest.py` (Unicorn) stays the oracle. When every pass matches, `recomp/` goes.
3. **Runtime** (stage 4): `crates/runtime` builds as a static library linked into the player. Each group of `runtime/src` files (config, time, saves, files, input, audio, services, kernel, memory, platform, then the loader and `main.cpp` last) is replaced by a Rust module exporting the same C symbols (header from `cbindgen`), and the C++ files go in the same PR.

## Rules

- A boot smoke test (the player reaches its first frame on a fake game library) runs in CI on Linux and gates every runtime step; each step also gets a device check on the AMD handheld.
- No behaviour change in a port PR; a bug found while porting is its own issue.
- Every `unsafe` block has a `// SAFETY:` line; `crates/core` has none.

## Why

A rewrite in one go would leave the game unplayable for months. The prep tools are small and well bounded; the recompiler has an oracle; the runtime is where upstream works most, so it moves last, when the sync cost of each module is known.
