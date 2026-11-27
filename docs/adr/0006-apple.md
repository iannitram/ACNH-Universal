# 0006: Apple: the same recompiled code on Mac and iOS

**Decided:** 2026-11-02 (`SPEC.md`, M4, M5).

## Decision

The same recompiled C++ is compiled for `aarch64-apple-darwin` and `aarch64-apple-ios`; no JIT is needed. The renderer is MoltenVK today and upstream's native Metal (`runtime/src/mtl`, `docs/NATIVE_METAL.md`) when it lands. The Mac is the hub for both: the Mac launcher builds the iOS app as an unsigned `.ipa` and puts it in Dropbox, and I install it with SideStore (amended 2026-11-18, below). On iOS the game app is the launcher; save-relay runs in it through save-relay's Rust API (`acnh_core::relay`, as on the Mac and PC; owner ruling 2026-11-18: "Rust API (Recommended)"), replacing its C interface (iannitram/save-relay#49).

## Risks, checked first (piece 8.01)

- **Address space.** Guest pointers are host pointers, so the runtime reserves the guest's whole 39-bit address space at a fixed host address (`runtime/include/nx/runtime.h`: moved up by `kGuestShift` = `0xC0_0000_0000` on Apple, a heap of up to 6 GiB). On iOS that needs the extended virtual addressing entitlement and may not fit an iPhone's limits. Settled by #72 and my iPad probe in #150: it does not fit even with the entitlement, so the Apple builds use a compact guest layout (8.03c, PR #210; `docs/apple-ios.md`).
- **Signing.** A free Apple ID re-signs every 7 days and limits memory; the paid Apple Developer Program lifts both. That is my decision when stage 8 starts. Status 2026-11-26: the free Apple ID stands ("Free Apple ID for now"), and the entitlement risk is the one #150 settled.

## Status 2026-11-26

The `.ipa` comes in two forms: the big one with the game inside (`--game`) and the small one that downloads it at first start (`--small`, #231, built by Prepare's `ios` target, #306). An earlier version of this ADR had no small `.ipa`. Neither is installed on a device yet.

## Why

Recompiled code runs at full speed on Apple silicon, and upstream already builds the Mac.

## Amended 2026-11-18

Owner ruling 2026-11-18: "To test install on my iPad I'll need a file on Dropbox. I don't have direct access to the Mac so we'll need a different side load process" (confirmed: "SideStore on the iPad (Recommended)"). The iOS app is no longer installed over a cable: the Mac builds an unsigned `.ipa` into `Dropbox/_transfers/ACNH-Universal/ios/` and I install it with SideStore on the device, which signs it with my free Apple ID and re-signs it every 7 days. The signing risk above is unchanged. Owner ruling 2026-11-18: "Final deliverables getting to me via Dropbox are really the only way in this case so exception cleared": the final builds (the iPad `.ipa` and the Mac app) may go to my own Dropbox under `_transfers/ACNH-Universal/`.
