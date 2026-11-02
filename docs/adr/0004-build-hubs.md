# 0004: Three home build hubs

**Decided:** 2026-11-02 (`SPEC.md`, M1, M11).

## Decision

The game folder lives on three hubs only:

| Hub | Builds for |
|---|---|
| the Strix Halo tablet | both SteamOS handhelds (Linux x86-64), Android (the NDK build runs on Linux) |
| the RTX 3090 Ti desktop | Windows |
| the Mac | Mac, iPhone, iPad |

A hub runs the prepare stages (`recipe::Stage`: tools, game, version, recompile, shaders, fonts, build) and serves the result. A device without the game folder finds a hub by mDNS, pairs once with a code shown on both screens (SPAKE2), and pulls over a sealed stream (ChaCha20-Poly1305), on the home network only (`hub::is_home_network` checks every address).

Two kinds of transfer, kept apart:

- **The bundle** (`bundle`): the player we built, listed with sizes and SHA-256. It never holds game data: `bundle::is_forbidden` refuses the game folder, the game library, `romfs.pak`, `spirv.pak`, `gen/` and saves.
- **The game data a prepare made** (the game library, `romfs.pak`, `spirv.pak`, fonts): sent only to my own paired devices, never stored anywhere else, never logged.

The fleet view (`fleet`) keeps what each paired device last reported and warns before play on a version or build drift.

## Why

A lost handheld or phone then holds a build, never the game folder (user story 5). Each hub builds what its own toolchain builds best.

## Cost

A device away from home cannot pull a new build until it is home again; it keeps playing the one it has.
