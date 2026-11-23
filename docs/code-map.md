# Code map: ACNH Universal

The intended shape of the repo for 1.0 (`SPEC.md`, approved 2026-11-02; M5 widened and approved again 2026-11-17; M4 and the Dropbox exception changed and approved again 2026-11-18): **one Rust engine, two faces, one recompiler, one runtime, three hubs, five platforms**. The engine (`crates/core`, library `acnh_core`) checks my game folder and its version, prepares a build for each device family on a hub, serves it to my other devices over the home network, writes the Steam shortcut with artwork, and wraps every start in a save-relay pull and push. The recompiler (`crates/recomp`, `acnh_recomp`) turns `exefs/main` into the C++ the game library is built from, replacing upstream's `recomp/` (Python). The runtime (`crates/runtime`, `acnh_runtime`) is the host side, replacing `runtime/src` outside the renderer one group of files at a time (ADR 0002), behind the C ABI the game library already uses (`runtime/include/nx/game.h`, `nx_game_abi()`). The NVN renderer, `games/acnh/` and the in-game overlay stay C++ (ADR 0001). The faces are the Slint launcher (`app/`, ADR 0003) and the `acnh-universal` command line (`crates/cli`). Android keeps upstream's route (ADR 0005); Mac, iPhone and iPad build the same recompiled code (ADR 0006).

Status per module: **present** (built), **partial** (some paths are wired), **stub** (the final types and signatures exist; constants and trivial pure helpers may be built, and any other function returns `Err(Error::NotBuilt(..))` or panics with "not built yet: <what>" when it cannot fail), **planned** (not in the tree), **legacy** (upstream's C++ or Python that we replace, removed when its Rust replacement is present and the game builds and boots). "Musts" columns cite `SPEC.md` (M1–M18); every module that replaces C++ or Python also serves M13. An issue that builds a module moves its status in the same PR. Integration branch: `main`.

## Platforms (R04)

| Device family | 1.0 device | Game code | Renderer | Built on (hub) | Launcher | Save sync in | CI |
|---|---|---|---|---|---|---|---|
| Linux / SteamOS | the AMD handheld, the Strix Halo tablet | recompiled, x86-64 | NVN on Vulkan (RADV) | the Strix Halo tablet; the AMD handheld pulls | Slint launcher. The release supplies an AppImage and #191 covers the locally contained game folder | the launcher | GitHub-hosted Linux (ubuntu-24.04): workspace, game-data check and the Android cross-build, the C++ unit tests, fake-game boot and emitter parity |
| Windows | the RTX 3090 Ti desktop | recompiled, x86-64 | NVN on Vulkan | the desktop | Slint app | the launcher | GitHub-hosted Windows (windows-2025): workspace (now); the C++ build (7.01) |
| macOS | a Mac with Apple silicon + controller | recompiled, ARM64 | MoltenVK, then upstream's Metal | the Mac | Slint launcher. Preparing the contained Mac and iOS apps is #171 and #170 | the launcher | GitHub-hosted macOS (macos-15): workspace (now) |
| iOS / iPadOS | iPhone or iPad + controller | recompiled, ARM64 (no JIT) | Metal (or MoltenVK) | the Mac, as an unsigned `.ipa` in Dropbox, installed with SideStore | game app | planned in the player through the Rust relay API (#172) | Rust target check, C++ compile, device and Simulator stub bundles, IPA smoke |
| Android | Adreno device + controller | recompiled, ARM64 | NVN on Vulkan (Turnip) | the Strix Halo tablet | Slint app, same APK as the player | the launcher | GitHub-hosted Linux (ubuntu-24.04): `aarch64-linux-android` cross-build, NDK 28.2.13676358 (now) |

Where the game root is: `plat::game_root()` (`runtime/src/game_root.h`) is `NX_GAME_ROOT` when set; else, on macOS and iOS, `game/` beside SDL's base (`ACNH-Universal.app/Contents/Resources/game/` on the Mac, `ACNH.app/game/` on the iPad) when it holds `analysis/main.bin`; else the data root (Linux, Windows, Android and a development build read as before). The player reads packaged game data from the game root and uses the writable data root for saves and settings. #171 aligns the Mac launcher and player's data root and log path under `~/Library/Application Support/ACNH-Universal`. On iOS the data root is Documents, with the player log move tracked in #172.

### Apple build entry points

| Path | Current behavior | Remaining integration |
|---|---|---|
| `crates/core/src/assemble.rs` | After desktop Prepare, assembles the Mac launcher and arm64 player with their libraries and MoltenVK. `assemble::player` resolves the child executable beside the Mac launcher. | #171 stages prepared game resources in the final app and aligns the launcher's data root with the player's before delivery. |
| `tools/check_ios_compile.sh`, `tools/build_ios.sh`, `ios/Info.plist.in` | Verify iOS C++ compilation and build `ACNH.app` with CMake, SDL3 and MoltenVK. The script defaults to handwritten stubs in `tests/ios_stub_gen/` and accepts a generated-source directory. | Device builds use a runtime-selected compact guest window. #170 connects the device build to Prepare. |
| `tools/package_ios.sh`, `docs/IOS.md` | Package an arm64 device bundle as a versioned IPA. macOS CI exercises the script with a stub. | #170 wires the package step into Prepare, #172 stages the prepared game in the app, and #74 verifies SideStore on the owner's device. |
| `app/src/play.rs`, `crates/core/src/launch.rs`, `crates/core/src/relay.rs` | Desktop launches the player as a child and core exposes the relay and conflict flows. The Slint entry point supplies `RelayOff`. | #164 wires real desktop relay. #172 uses the same Rust adapter in the iOS player with app lifecycle hooks. |

See the [Apple roadmap](../ROADMAP.md#apple-build-and-device-acceptance) for implementation order and device acceptance.

Rules that keep it portable: `crates/core` is pure Rust (no C library, no OpenSSL; the hub's crates are `mdns-sd`, RustCrypto `spake2`, `chacha20poly1305`, `sha2`, behind feature `hub`); no `std::os::*` outside `platform` and `steam`; every OS rule takes its folders from `platform::Vars`, so Linux, Windows and Mac rules are tested on every runner; tests build temporary folders under `std::env::temp_dir()` and never read a real game file. `crates/core` has `#![forbid(unsafe_code)]`; the runtime is the only crate with `unsafe` and FFI, and each `unsafe` block carries a `// SAFETY:` line.

## How it fits together

```mermaid
flowchart TD
    subgraph Faces
      App["app/: Rust + Slint<br/>Continue, first run, Saves, Settings, Fleet, About"]
      CLI["crates/cli: acnh-universal check | build | play | steam-add | hub | fleet"]
    end
    Steam["Steam Game Mode shortcut + artwork"] -->|"opens the app full screen"| CLI
    App --> Core
    CLI --> Core
    subgraph Core["crates/core: acnh_core"]
      Folder["gamefolder, nso, version (the gate)"] --> Prepare["prepare: stages tools, game, version, recompile, shaders, fonts, build"]
      Prepare --> Recipe["recipe: hubs, stages, outputs"]
      Launch["launch: pull, run, push"] --> Saves["saves, savecard, backup"]
      Launch --> Profile["profile, config (config.ini, launcher.ini)"]
      Prepare --> Hub["hub, bundle, fleet"]
      SteamMod["steam, vdf, artwork"]
      Update["update"] --> Prepare
      Update --> Saves
    end
    Prepare -->|"recompile stage"| Recomp["crates/recomp: acnh_recomp<br/>Load, Analyse, Emit"]
    Recomp -->|"C++ in gen/"| GameLib["the game library: gen/ + games/acnh/ (C++, built on the hub)"]
    Launch -->|"starts with NX_DATA_ROOT"| Player["the player: runtime/src (C++ today) + crates/runtime"]
    subgraph Runtime["crates/runtime: acnh_runtime"]
      Svc["Config, Time, Saves, Files, Input, Audio, Services, Kernel, Memory, Platform, Loader"]
    end
    Player --> Runtime
    Player -->|"nx_game_abi()"| GameLib
    Player --> NVN["runtime/src/nvn, vk_*, mtl/, ui/ (C++, kept)"]
    NVN --> VK["Vulkan, MoltenVK, Metal"]
    Saves --> SR[("iannitram/save-relay")]
    Hub <-->|"home network only"| Peers[("my other launchers")]
```

## Modules: `crates/core` (`acnh_core`)

| Module | Status | Owns | Replaces | Musts |
|---|---|---|---|---|
| `error` | present | `Error` with a stable `code()` and a plain message per variant, `NotBuilt`, `not_built`, `Result`; messages name files, never their contents | | all |
| `platform` | present | `Os` (linux, windows, mac, ios, android) with `key`/`from_key`, `Vars` (home, data, config from an env lookup), `AppDirs` (`<config>/acnh-universal`; `<data>/acnh-universal`: `install/`, the data root `install/root`, logs `install/logs`) | the folder code of `launcher/files.cpp` | M1, M2, M13 |
| `version` | present | `Version` (3.0.2, 3.0.3, 2.0.7, 1.0.0) with name, build id, `native60` and `from_name`; `RECOMMENDED`; `from_build_id`, the gate that refuses any other build with a message naming the four | `tools/acnh_version.py`, `games/acnh/versions.txt` (read) | M1 |
| `nso` | present | `build_id` from the first 0x60 bytes of `exefs/main` (magic `NSO0`, 16 bytes at 0x40, upper hex), `read_build_id` | the header read of `tools/acnh_version.py` and `launcher/files.cpp` (`build_id`) | M1, M14 |
| `nso2bin` | present | `parse` (three segments decompressed with pure-Rust `lz4_flex`, bss size), `Nso::image`/`image` (flat image at the memory offsets, bss excluded), `write`; a truncated segment or bad LZ4 is `Error::NotAcnh` | `tools/nso2bin.py` (the segment reads of `recomp/nso.py` stay in `Load`) | M1, M13 |
| `gamefolder` | present | `GameFolder` (root, `exefs/`, `romfs/` or `extract/romfs/`, `aoc/<index>/` sorted, `from_importer`, version), `check` naming the first missing file | `find_game`, `from_importer` in `tools/setup_acnh.py`; `game_parts` in `launcher/files.cpp` | M1 |
| `config` | present | `Config` of `Line`s: `parse`, `get` (last wins), `set` (in place, else appended), `to_text`; `config.ini` in the data root and `launcher.ini` in the settings folder | `runtime/src/config.cpp` (format), the ini code of `launcher/files.cpp` | M2 |
| `profile` | present | `DeviceProfile` (amd-handheld 30 fps, strix-halo-tablet 60 fps, desktop), `builtin`, `by_id`, `detect` from the CPU model; settings as `config.ini` entries, power as advice only | | M2 |
| `recipe` | present | `Hub` (strix-halo, desktop, mac), `hub_for`, `serves`, `hub_by_key`; `Stage` (tools, game, version, recompile, shaders, fonts, build) with `from_line` for `== <stage>` progress lines; `output` per target (`acnh`, `acnh.exe`, `ACNH.app`, `ACNH.ipa`, `acnh.apk`); `MAC_APP` (`ACNH-Universal.app`, the Mac's one contained app, which `assemble` makes) | the stage list of `tools/setup_acnh.py`, the progress parsing of `launcher/pipeline.cpp` | M1, M11 |
| `prepare` | partial | `Request` (game, out, target, jobs, source, fan-mod choices), `Step` (`Rust`, or upstream's `Script` until a stage is ported), `step` (Rust for shaders, with script invocations before and after it for the other stages), `prepare`, `prepare_with` and `prepare_installed` (present: the source named by `Request`, else the packaged source beside the executable and a checkout for a `cargo run`; a packaged source is copied to `<data>/source/<commit>/` and the script runs there, and a package from another commit is refused; stages in order, a stage reported and waited on unless its outputs were already there. A fully cached run only recovers unfinished mod reflow; a `ScriptRunner` seam; `ProcessRunner` follows each invocation of upstream's command through its `== <stage>` lines, so inside a run upstream's own checks decide what is redone; redacted `Error::Build` naming the stage the script failed in; Android and iOS targets are `Unsupported` in Prepare; the iOS CMake and packaging scripts run independently; the build stage leaves the bare player in `<out>/build/`, which `assemble` makes self-contained. The setup script still runs the game stage and leaves newly extracted mods for the Rust dialogue reflow at its next stage marker. `prepare::assemble` is `assemble::assemble`), `Missing` and `toolchain`/`toolchain_with` (present: the tools `tools/setup_acnh.py` needs before its first stage, from a given PATH and the Mac's Homebrew prefixes, each with the command that installs it; required pieces stop a prepare, libvpx and libvorbis are warnings), `redact` (present: no home folder or stray hex ids in a log line) | `tools/setup_acnh.py` and the scripts it runs: `nso2bin.py`, `pack_romfs.py`, `pack_spirv.py`, `extract_shaders.py`, `sarc_endian.py`, `msg_rewrap.py` (1.27-1.31); `launcher/pipeline.cpp` | M1, M13, M14 |
| `assemble` | present | after the build stage, the self-contained player in the install folder (1.22b): `assemble(os, build, install)`, `Plan` and `assemble_with` behind a `Tools` seam (`patchelf`, `install_name_tool`, `codesign`; `HostTools` runs them). Linux: `acnh` and the libraries a stock system may lack (SDL3, libvpx, libvorbis, libogg, read from the ELF dynamic section) beside it, found through `$ORIGIN`. Windows: `acnh.exe`, the DLLs the build put beside it and those it imports (read from the PE import table; Windows's own stay the system's). Mac: `ACNH-Universal.app`, a copy of the launcher's bundle (or one made from a checkout's `target/release/` and `assets/mac/Info.plist.in`), with `Contents/MacOS/acnh`, the Homebrew libraries, the Vulkan loader and MoltenVK in `Contents/Frameworks` (named `@executable_path/../Frameworks/<name>`), `Contents/Resources/MoltenVK_icd.json`, an ad-hoc signature on each binary and the bundle last; refused inside a git work tree (the player holds game code) and for any Mach-O that is not arm64 only (`mach_links`, read from the header). `player`: the program a launcher starts (on the Mac the one beside its own executable, else the one in the app under `install/`) | the hub half of `tools/package_release_linux.sh`, `package_release_win.sh`, `package_release_mac.sh` (their launcher and zip parts are 2.09a's) | M1, M4, M11, M14 |
| `msg` | present in Prepare | Reflows newly extracted dialogue, mail, tutorials and Island Life 101 messages (1.30, #33). Preserves SARC order, MSBT sections and UTF-16 control tags. Stages modified archives as a batch and resumes interrupted replacement. Pillow supplies matching dialogue font advances through a small Python probe, and zstd handles compression. The probe needs a compatible replacement before Python can be retired | `tools/msg_rewrap.py` in Rust Prepare | M1, M13 |
| `pack` | present | `romfs`, `romfs_range`, `romfs_size`, `spirv` write the `NXRP` ROMFS and `SPK1` SPIR-V packs during Prepare | `tools/pack_romfs.py`, `tools/pack_spirv.py` | M1, M13 |
| `sarc` | present | Parse and rewrite endian-aware SARC archives for shader extraction and dialogue reflow | `tools/sarc_endian.py` | M1, M13 |
| `shaders` | present in Prepare | Plan and extract shader groups, find a .NET 10 SDK and the Tools-stage harness, run translation with optional SPIR-V validation, check the summary and pack the output (#160) | `tools/extract_shaders.py` | M1, M13 |
| `glyphs` | Rust stage, script dispatch | Draw the shared button TrueType font and rewrite ParkExt for Xbox, PlayStation and Switch layouts. Reuse the SARC reader and zstd, with synthetic outline and archive tests. Prepare still dispatches Fonts to the setup script until #34. | `tools/fonts/make_button_font.py`, `tools/make_button_glyphs.py` | M1, M5, M13, M14 |
| `saves` | present | `save_dir` (`<data root>/user/device_save`), `backups_dir` (`user/backups`), `REQUIRED`, `is_synced` (what travels), `check_island`, `relay_manifest` (save-relay's manifest for ACNH) | the save folder of `runtime/src/saves.cpp` (read, not moved; the runtime's own `Saves` group is 4.04) | M6 |
| `savecard` | present | `SaveCard` (save time from `main.dat`, device, play time, version, picture), `read_card` from `<save_dir>/../launcher-card.txt`, `latest`, `side_by_side` for a conflict | | M10 |
| `backup` | present | `stamp` (`YYYYmmdd-HHMMSS`, UTC), `backup_saves` (the synced files), `list`, `prune` (`KEEP` = 10) | the backups of `runtime/src/saves.cpp` (same place and shape) | M7 |
| `update` | present in core | `Updated`; `update` backs up and prunes island backups, calls the rebuild, checks for saved files afterward and reports the backup on failure. Launcher wiring is #169 | | M7 |
| `launch` | present | `Relay` and `Game` traits, `Sync`, `Keep`, `play` (pull, conflict with both cards, run the player with the profile, push) | starting the player in `launcher/desktop.cpp` | M6, M10 |
| `bundle` | present | `Bundle` (target, version, commit, build id, files with size and SHA-256), `DEVICE_OWNED` (`root`, `logs`), `is_forbidden` (never game data, the game library, packs, `gen/`, saves), `to_text`/`from_text`, `manifest`/`verify`, `install` (keeps `DEVICE_OWNED`) | | M11, M14 |
| `fleet` | present | `DeviceRecord` (device, os, version, build id, last sync, free space), `Drift`, `drift`, `warning`, `to_line`/`from_line` | | M11 |
| `hub` | partial (feature `hub`) | `is_home_network`; `announce` and `discover` (mDNS `_acnh-universal._tcp` on the home network, #62); `Transport`, `TcpTransport`, `MemoryTransport`; `PairingSession`, `pair`/`pair_in` (six-digit SPAKE2, expiry and attempt limits, and key replacement, #63), `read_peers`/`remove_peer`; `serve`/`pull` (sealed, resumable build bundles and bounded-memory verification, #64); `serve_prepared`/`pull_prepared` (separate paired protocol for the Android library, packs, shared fonts, glyph archives and RomFS mod overlays, verified and installed under the private data root's `game/`, #65). `hub/service.rs` routes TCP requests, binds pairing to the client and target, and stores authenticated `DeviceRecord`s in `fleet.txt` (#66). Build pulls preserve the last reported save-sync time and free space. Desktop and Android launcher wiring are follow-ups. [Peer scan](peers.md) | | M9, M11, M18 |
| `vdf` | present | Steam binary KeyValues `Value`, `write`, `read` (depth limit `MAX_DEPTH` = 64) | | M2 |
| `steam` | present | `crc32`, `shortcut_app_id`, `rungameid`, `Shortcut::quoted_exe`, `Shortcut::app_id`, `userdata_roots`, `shortcut_entry`, `upsert`, and `write_shortcut` (Steam closed, backup kept). `launcher_shortcut` opens full-screen Continue | the desktop and Start-menu shortcuts of `launcher/desktop.cpp` and `tools/install_mac_shortcut.sh` (the Steam entry is new) | M2 |
| `artwork` | present | `Art` (grid, wide grid, hero, logo, icon), `file_name`, `size`, `grid_dir`, `paths`; made from the game folder's icon, else `assets/acnhrecomp.png`; nothing downloaded | | M2 |
| `relay` | present (feature `relay`) | the save-relay adapter: links its library, maps outcomes to `launch::Sync`; `open` reads save-relay's own setup and opens its remote, `setup_nextcloud`/`start_sign_in`/`finish_sign_in` set it up (browser or Game Mode code flow), `forget` removes the config and secret | | M6 |

`platform`, `vdf`, `steam`, `artwork`, `backup`, `bundle`, `fleet`, `launch`, `update`, `hub` and the shapes of `savecard`, `profile` and `recipe` are copied from iannitram/totk-universal (MIT) with credit in the file header, by their issues.

## Modules: `crates/recomp` (`acnh_recomp`)

Ported pass by pass (ADR 0002). Each pass's output must match `python3 -m recomp` byte for byte on the same hand-written AArch64 input (never game code); `tests/difftest.py` (Unicorn) stays the oracle. `prepare` keeps upstream's driver for the recompile stage until 3.05.

| Pass | Status | Replaces (lines) | Order | Musts |
|---|---|---|---|---|
| `Load` | present | `recomp/nso.py` (200): segments, symbols, relocations | 3.01 | M13 |
| `Analyse` | present | `recomp/analysis.py` (439), `recomp/ehframe.py` (142): functions and their bounds | 3.02 | M13 |
| `decode` (analysis helper) | present | Targeted AArch64 decoding for analysis, including branch targets and jump-table patterns | 3.02 | M13 |
| `Emit` (integer, branches) | present in `crates/recomp/src/emit/` | Integer, memory and branch handlers in `recomp/emit.py`, plus `emit_int.py` and `regcache.py`. Capstone 5 supplies operand details | 3.03 | M13 |
| `Emit` (FP, NEON) | present in `crates/recomp/src/emit/` | Scalar FP and fixed-point conversion (`fp.rs`), NEON lane operations and widening/narrowing (`neon.rs`, `neon_wide.rs`), structured transfers (`structured.rs`), and atomic, crypto and SDK extensions (`extension.rs`) | 3.04 | M13 |
| driver | planned | `recomp/__main__.py` (303), `stats.py` (48) | 3.05 | M13 |

The synthetic emitter check compares the Rust translation unit with the Python driver in four cache and hook configurations, then runs the Rust statements through the Unicorn execution oracle. Linux CI runs it with Capstone 5.0.6, lz4 and Unicorn. The test creates its binary fixtures and generated C++ in a temporary directory.

```sh
cargo build -p acnh-recomp --example recomp-emitter
python3 -m tests.recomp_emit target/debug/examples/recomp-emitter
```

If the default scratch address is unavailable on macOS, pass `--buffer-addr 0x600000000000`.

## Modules: `crates/runtime` (`acnh_runtime`)

Built as a static library linked into the player while C++ remains; each Rust group exports the same `extern "C"` symbols as the files it replaces (header from `cbindgen`). Order is `SERVICES`: leaves first, the loader last. Each step keeps the boot smoke test green and is followed by a device check on the AMD handheld.

| Group | Status | Replaces (`runtime/src/`) | Order | Musts |
|---|---|---|---|---|
| scaffold | present | the CMake build of `runtime/` driven from `build.rs`; the boot smoke test | 4.01 | M13, M15 |
| `Config` | stub (listed) | `config.cpp`: `config.ini` to the `NX_*` settings | 4.02 | M2, M13 |
| `Time` | stub (listed) | `hle_time.cpp`, `svc_time.cpp`: the island clock (`NX_TIME_OFFSET`) | 4.03 | M12, M13 |
| `Saves` | stub (listed) | `saves.cpp`, `users.cpp`, `svc_account.cpp` | 4.04 | M6, M13 |
| `Files` | stub (listed) | `hle_fs.cpp`, `game_folder.cpp`, `mod_fetch.cpp`, `game_root.h`: the romfs, its packs, fonts, glyphs, shaders and add-on content from the game root; mods and the player's own files from the data root | 4.05 | M13 |
| `Input` | stub (listed) | `hle_hid.cpp`, `input_script.cpp`, `touch_layout.cpp` | 4.06 | M3, M5, M9, M13 |
| `Audio` | stub (listed) | `hle_audio.cpp` | 4.07 | M13 |
| `Services` | stub (listed) | `svc_am.cpp`, `svc_set.cpp`, `svc_misc.cpp`, `svc_process.cpp`, the other `hle_*.cpp` (bcat, friends, ldn, movie, net, nfp, ngc, pl, ro, vi) | 4.08 | M13 |
| `Kernel` | stub (listed) | `kernel.cpp`, `ipc.cpp`, `kernel_watch.cpp` | 4.09 | M13 |
| `Memory` | stub (listed) | `memory.cpp`, `memguard_*.cpp`, `write_watch.cpp`: the guest address space. Device iOS uses the compact window from `compact_layout.cpp`. macOS and the iOS Simulator use the desktop layout | 4.10 | M4, M5, M13 |
| `Platform` | stub (listed) | `platform_*.cpp`, `thermal.cpp`, `perf_hint.cpp` | 4.11 | M13 |
| `Loader` | stub (listed) | `loader.cpp`, `game_abi.cpp`, `main.cpp`: load the game library and start it | 4.12 | M13 |

"Stub (listed)": the group is named in `SERVICES` and `ported` says false; no code moves until its issue. The rest of `runtime/src` outside the renderer (`bug_report.cpp`, `frame_stats.cpp`, `discord_presence.cpp`, `cpu_profile.cpp`, `sampler.cpp`, `glyphs.cpp`, `app_icon.cpp`) goes with the group that calls it, or stays C++ beside the overlay when it is overlay code.

## Faces

| Path | Status | Owns | Replaces | Musts |
|---|---|---|---|---|
| `crates/cli` (`acnh-universal`) | present | `check`, `build`, `play`, `steam-add`, `hub serve\|pair\|pull\|report`, `fleet`; `hub --help` shows the arguments; `--json` prints the error `code()`; the Steam shortcut opens the app | `tools/setup_acnh.py`'s command line, `tools/launcher.sh` | M1, M2, M6, M11 |
| `app/` (`acnh-app`) | partial | Desktop Slint launcher links `acnh_core`. Continue starts the player after checking relay status and can Prepare, Pull or Update a build. The navigation row opens Saves, About, fan-mod Settings and Fleet. Guided first run opens for an unprepared desktop (#39). Relay and Android integration remain on the roadmap | `launcher/` (SDL3 + ImGui: `main.cpp`, `desktop.cpp`, `pages.cpp`, `pipeline.cpp`, `files.cpp`) | M2, M4, M9-M11, M13 |
| `app/src/first_run.rs` | present on desktop | Runs local Prepare and assembly off the UI thread, or receives a verified player bundle and returns to local Prepare. Applies device defaults and writes the optional Linux Steam shortcut with artwork. Android private-data integration is tracked in #80 and #192. | | M1, M2, M10, M11 |
| `app/src/play.rs` | present | Continue action, player process, build progress and save-sync results including conflict and hold choices | | M6, M10 |
| `app/src/ui.rs` | present | Converts Continue view state and focus targets between Rust and Slint | | M10 |
| `app/src/vm/` | partial | `continue_page`, `saves`, `settings`, and `about` exist. Settings stores the pt-BR and SpeedFix opt-ins in `config.ini` (#85), and Prepare snapshots them for each build. `fleet` reads the local hub’s authenticated records, presents versions, builds, save-sync and free-space fields, and labels the comparison device. `first_run` manages folder validation, hub discovery and pairing, progress, and device setup | | M8, M10, M11 |
| `app/src/input.rs` | partial | Maps gilrs events and keyboard text to desktop `Nav`. Identifies controller layouts by device ID, tracks hot-plug and last-used input, and repeats stick and D-pad navigation. Android input integration is #173 | | M9, M10 |
| `app/ui/` | partial | Slint window, Continue, About, Fleet, read-only Saves, guided first run and fan-mod Settings. Shared input hints select original SVGs for the active controller | `launcher/pages.cpp` | M10 |
| `docs/design/continue-screen.md` | present | the Continue screen brief, shared with totk-universal | | M10 |

## Upstream code that stays (C++ or Java, behind a C ABI)

| Path | Owns | Musts |
|---|---|---|
| `runtime/src/nvn/`, `vk_*.cpp`, `runtime/src/mtl/` | the NVN renderer on Vulkan, MoltenVK and Metal | M2-M5, M9 |
| `runtime/src/ui/` | the in-game settings overlay | M2 |
| `games/acnh/` | the per-game C++ the recompiled code links with, `versions.txt` | M1 |
| `gen/` (made on a hub, never committed) | the recompiled game code | M1, M14 |
| `android/` | the APK shell (`Home.java`), Gradle; our launcher joins it at 9.01 | M9 |
| `tools/build_turnip.sh`, `tools/build_android.sh` | Turnip and the Android build | M9 |
| `tests/*_test.cpp`, `tests/difftest.py`, `tests/astc_check.py`, `tests/metal_shader_check.mm` | the renderer's tests and the recompiler's oracle | M15 |
| `tools/check_no_game_data.sh` | the pre-commit and CI game-data check | M14 |

## Legacy (removed by M13)

| Path | Removed when |
|---|---|
| `launcher/` (C++, SDL3 + ImGui, about 3.6k lines), `tools/launcher.sh` | the Slint app has parity: Continue, first run, Settings, Add to Steam, update (2.10) |
| `tools/setup_acnh.py` (633), `tools/setup_acnh.sh`, and the scripts it runs: `acnh_version.py`, `nso2bin.py`, `pack_romfs.py`, `pack_spirv.py`, `extract_shaders.py`, `sarc_endian.py`, `msg_rewrap.py` | the setup stages move into Rust (1.31). The Python recompiler route stays until 3.05 |
| `recomp/` (Python, about 3.8k lines) | the recompile stage runs `crates/recomp` (3.05) |
| `runtime/src` outside the renderer and the overlay | the `Loader` group is Rust and the game boots (4.12) |
| `tools/package_release_*.sh`, `tools/release_readme.sh` | the release workflow builds the Rust app (2.09); the half a hub needs (the player with its libraries) is `assemble` (1.22b) |

Debug tools that never ship (`tools/find_*.py`, `capture*.ps1`, `probe.py`, `symbolize.py` and the like) may stay (M13).

## Build and test

```sh
cargo build --workspace
cargo test --workspace
cargo test --workspace --all-features    # with the hub
python3 tools/setup_acnh.py --help       # upstream's build, until 1.31
```

CI (M15): `.github/workflows/check.yml` runs the Rust workspace checks on hosted Linux, Windows and macOS, checks tracked files for game data, cross-builds Android, and checks the Rust iOS target. Linux also runs the C++ units, the fake-game boot smoke (4.01) and the emitter parity and Unicorn checks (#47). Android uses the NDK's LLVM archiver to build the Capstone dependency. macOS compiles the C++ runtime for both iOS SDKs, builds both stub bundles and checks device IPA packaging. `build.yml` holds the legacy build and `release.yml` packages the Rust launcher.

## Build hubs (ADR 0004)

| Hub | Builds for | Clients |
|---|---|---|
| the Strix Halo tablet | linux, android | itself, the AMD handheld, the Android device |
| the RTX 3090 Ti desktop | windows | itself |
| the Mac | mac, ios | itself; the iPhone or iPad as an `.ipa` in Dropbox (SideStore) |

After its build stage a hub uses `assemble` (1.22b) to stage the desktop player and its libraries. On the Mac this produces an arm64 `ACNH-Universal.app` with the launcher, player and MoltenVK. The final Mac resource layout and Dropbox handoff are #171.

The hub bundle manifest covers the transferable build and its file hashes. M11's separately paired game-data transfer is #65. Under M18, the final Linux and Windows folders are built locally on each device (#191, #190), Android keeps its prepared game in private storage (#192), and the final Apple apps may go to the owner's own Dropbox. Game data stays out of Git, CI, releases and logs (M14).

## Not verified yet

- The four build ids are upstream's (`games/acnh/versions.txt`); each is confirmed by a real build on a hub (6.01).
- The free-ID iPad probe is recorded in #150. Booting the game with the compact layout, SideStore installation of the completed IPA, and the Apple save-relay round trip remain device checks.
- How long an upstream sync takes once `recomp/` and `runtime/src` are Rust; measured at stages 3 and 4.
- The play time on the save card: written by the launcher to `launcher-card.txt` after each session, since the game's own count needs a save reader (after 1.0).
- mDNS from Game Mode and on Android (multicast lock).
