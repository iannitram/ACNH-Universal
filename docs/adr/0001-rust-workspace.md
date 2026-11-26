# 0001: One Rust workspace for our code

**Decided:** 2026-11-02, by me in the Project Manager session's question tool: "Approve all, same plan (Recommended)" (`SPEC.md`, M13).

## Decision

Our code becomes one Cargo workspace:

- `crates/core` (`acnh_core`): the engine. Game folder check, the four-version gate, the NSO header, prepare, config files, saves, save cards, backups, update, device profiles, recipes, Steam, artwork, bundles, fleet, and the hub behind feature `hub`.
- `crates/recomp` (`acnh_recomp`, binary `acnh-recompiler`): the AArch64 to C++ recompiler, replacing upstream's Python `recomp/`.
- `crates/runtime` (`acnh_runtime`): the host runtime, replacing `runtime/src` outside the renderer.
- `crates/cli` (`acnh-universal`): the launcher without its window.
- `app/` (`acnh-app`): the Slint launcher (ADR 0003). Built; it is a workspace member.

What stays in its language, behind the game library's C interface (`runtime/include/nx/game.h`, `nx_game_abi()`): the recompiled game code and `games/acnh/` (C++), the NVN renderer (`runtime/src/nvn`, `vk_*`, `mtl/`), the in-game settings overlay (`runtime/src/ui`), and the Android Java shell (`android/java`). CMake keeps building those, driven from `crates/runtime`'s build script once it links them.

## Why

- One language and one build (`cargo`) for everything of ours, tested the same way on Linux, Windows and macOS.
- The renderer is most of upstream's daily work; keeping it C++ keeps those syncs merging.
- totk-universal (MIT) already has the launcher's generic modules in Rust; copying them with credit saves the work.

## Cost

Once a module is Rust, upstream's Python or C++ change to it no longer merges: each becomes an `upstream-sync` issue that ports the change by hand. Upstream is very active, so the runtime moves last (ADR 0002).
