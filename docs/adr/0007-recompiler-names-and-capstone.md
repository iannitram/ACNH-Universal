# 0007: Recompiler binary name and Capstone versions

**Decided:** 2026-11-26 (#389, 3.07b).

## Decision

- The Rust recompiler's binary is `acnh-recompiler`. The library stays `acnh_recomp` (`use acnh_recomp::...` is unchanged). The C++ command line `prepare/recomp/` keeps the name `acnh_recomp`: the APK's on-phone Prepare and CI build it.
- Capstone stays at two versions: the Rust crate uses 5.0.6 (`capstone` 0.14 / `capstone-sys` 0.18), the vendored C++ copy (`third_party/capstone`) uses 5.0.7. The CI oracle (`tests/recomp_emit.py`) is pinned to 5.0.6 to match Rust.

## Why

- Nothing runs the Rust binary by name (the engine calls the crate in process), so renaming it only touches usage text and docs. Renaming the C++ target would touch the Android build and CI.
- Moving Rust to 5.0.7 needs a newer crate or a vendored sys crate that nobody has tested. Moving the C++ copy to 5.0.6 would change the on-phone Prepare, which was checked on a device at 5.0.7.
- The C++ recompiler goes away at 3.08 (#351), which ends the split. The two recompilers are not expected to print identical text.

## After 1.0

If one version is wanted, pick 5.0.7 once capstone-rs ships it.
