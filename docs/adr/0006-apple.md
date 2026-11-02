# 0006: Apple: the same recompiled code on Mac and iOS

**Decided:** 2026-11-02 (`SPEC.md`, M4, M5).

## Decision

The same recompiled C++ is compiled for `aarch64-apple-darwin` and `aarch64-apple-ios`; no JIT is needed. The renderer is MoltenVK today and upstream's native Metal (`runtime/src/mtl`, `docs/NATIVE_METAL.md`) when it lands. The Mac is the hub for both: the Mac launcher builds the iOS app with Xcode signing and installs it over a cable. On iOS the game app is the launcher; save-relay runs in it through its C interface (iannitram/save-relay#49).

## Risks, checked first (piece 8.01)

- **Address space.** Guest pointers are host pointers, so the runtime reserves the guest's whole 39-bit address space at a fixed host address (`runtime/include/nx/runtime.h`: moved up by `kGuestShift` = `0xC0_0000_0000` on Apple, a heap of up to 6 GiB). On iOS that needs the extended virtual addressing entitlement and may not fit an iPhone's limits.
- **Signing.** A free Apple ID re-signs every 7 days and limits memory; the paid Apple Developer Program lifts both. That is my decision when stage 8 starts.

## Why

Recompiled code runs at full speed on Apple silicon, and upstream already builds the Mac.
