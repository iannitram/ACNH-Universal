# Code map: ACNH Universal

The shape of the repo against 1.0 (`SPEC.md`, approved 2026-11-02; M5 widened and approved again 2026-11-17; M4 and the Dropbox exception changed and approved again 2026-11-18; M18, M4 and the Dropbox exception changed so Android and iPhone/iPad may download the game at first start from my own Dropbox, and approved again 2026-11-23, #232): **one Rust engine, two faces, one recompiler, one runtime, three hubs, five platforms**. The engine (`crates/core`, library `acnh_core`) checks my game folder and its version, prepares a build for each device family on a hub, serves it to my other devices, writes the Steam shortcut with artwork, and wraps every start in a save-relay pull and push. The recompiler (`crates/recomp`, `acnh_recomp`) turns `exefs/main` into the C++ the game library is built from. The runtime (`crates/runtime`, `acnh_runtime`) is the host side behind the C ABI the game library uses (`runtime/include/nx/game.h`, `nx_game_abi()`), with C++ still around it (see its section). The NVN renderer, `games/acnh/` and the in-game overlay stay C++ (ADR 0001). The faces are the Slint launcher (`app/`, ADR 0003) and the `acnh-universal` command line (`crates/cli`). Android keeps upstream's route (ADR 0005); Mac, iPhone and iPad build the same recompiled code (ADR 0006).

**How to read the status column.** **present**: built and reached by a face or a build. **partial**: built, with a named gap. **library only**: built and tested, but no face or build step reaches part of it. **legacy**: upstream's C++ or Python that M13 says we replace. **stays**: C++ or Java that SPEC M13 names as staying. A status that an open issue qualifies carries the issue number. "Musts" cite `SPEC.md` (M1–M18). An issue that changes a module moves its row in the same PR.

**How this map was checked.** Against `main` at `dc87368` on 2026-11-25, by reading: eight readers over the whole tree, then a second, independent read of each of 43 findings (17 held as stated, 26 held with corrections, none was refuted). Nothing was built or run for it. The earlier map said that all non-renderer C++ in `runtime/src` was ported, that upstream's Python prep scripts were retired, that CI built every test target and that the hubs were done. Those four statements were wrong and are corrected below; the work they hid is in `ROADMAP.md` (issues #321–#370).

## Size, by language

Tracked first-party lines outside `private/` and `third_party/`:

| Language | Lines | Files | Where |
|---|---|---|---|
| C++ and headers | 146,871 | 276 | `runtime/` 132,120 (most of it the renderer); `prepare/` 11,773; `tests/` 1,876; `games/` 951 |
| Rust | 79,707 | 163 | `crates/` 66,373; `app/` 13,334 |
| Python | 4,964 | 50 | `tools/` 4,207; `tests/` 665 |
| Java | 4,344 | 17 | `android/` |
| Shell | 3,049 | 32 | `tools/` 2,047; `tests/` 933 |
| C# | 2,208 | 6 | `tools/maxwell/spirv_harness` |
| CMake | 1,148 | 4 | `CMakeLists.txt` 883; `prepare/` 265 |
| Slint | 967 | 7 | `app/ui/` |
| Objective-C++ | 851 | 2 | `runtime/src/appgame_ios.mm` 766; `tests/` 85 |
| PowerShell | 390 | 4 | `tools/` (developer capture tools) |

## Platforms (R04)

| Device family | 1.0 device | Game code | Renderer | Built on (hub) | Front door | Save sync | CI |
|---|---|---|---|---|---|---|---|
| Linux / SteamOS | the AMD handheld, the Strix Halo tablet | recompiled, x86-64 | NVN on Vulkan (RADV) | the Strix Halo tablet; the AMD handheld is meant to pull (blocked: #357, #358) | Slint launcher; the release is an AppImage and `pack_folder` makes the contained folder on the device (#191) | the launcher: Nextcloud, Google Drive or Dropbox | ubuntu-24.04: the workspace, the save-relay link check, the game-data check, the Rust emitter smoke test and Unicorn oracle, C++ unit tests, the game-root check, the fake-game boot, the Android cross-build |
| Windows | the RTX 3090 Ti desktop | recompiled, x86-64 | NVN on Vulkan | the desktop | Slint launcher; a zip, and `pack_folder` makes the contained folder (#190) | the launcher | windows-2025: the Rust workspace. `cpp-windows` (C++ and the player) has never passed (#322) |
| macOS | a Mac with Apple silicon + controller | recompiled, ARM64 | MoltenVK, then upstream's Metal | the Mac | Slint launcher inside `ACNH-Universal.app`; Prepare makes the contained Mac app and both `.ipa`s into Dropbox | the launcher | macos-15: the workspace with all features, the MoltenVK assemble test, the layout tests, the Mac package test, the staging test. The Mac player is never built in CI |
| iOS / iPadOS | iPhone or iPad + controller | recompiled, ARM64 (no JIT) | Metal (or MoltenVK) | the Mac, by Prepare's `ios` target: `ACNH-Universal-<version>.ipa` with the game inside, and a small one with no game in `ios/small/` plus its game folder; installed with SideStore | the game app itself | in the player through save-relay's Rust API: **Nextcloud only** (#361) | macos-15: the Rust target check, the runtime static library, C++ compile for both SDKs, stub bundles and their checks, Prepare's `ios_real` test, the stub `.ipa` and `tests/package_ios_test.sh` |
| Android | an Adreno device + controller (none named yet); my Pixel Fold (Mali-G710) for information | recompiled, ARM64 | NVN on Vulkan: on Adreno the bundled Turnip (the system driver on Adreno 8xx), elsewhere the device's own driver | the Strix Halo tablet (ADR 0004); Prepare's `android` target is broken for a prepared folder (#365); upstream's on-phone Prepare also builds it on the phone | `LauncherActivity` (the Slint launcher) is the only launcher activity since PR #304 and cannot get or start the game (#363). `Home` (upstream's Java shell: folder picker, on-phone Prepare, hub pull, Dropbox download, start) is reached only by the icon's long-press shortcut, the sign-in return and the game's back-to-start | in the app through save-relay's C interface and JNI: **Dropbox only** | ubuntu-24.04: `cargo build --workspace --target aarch64-linux-android` (without the `android-launcher` feature) and `tests/android_app_check.sh`. No APK, no Android C++ |

### Where each platform keeps the game and saves

`plat::game_root()` (`runtime/src/game_root.h`) is `NX_GAME_ROOT` when set; else, on macOS and iOS, `game/` beside SDL's base when it holds `analysis/main.bin`; else, on iOS, the game the small `.ipa` downloaded; on Android `<files>/game` when it holds `analysis/main.bin`; else the data root. The launcher starts the player with `NX_DATA_ROOT`, `NX_LOG` and `NX_GAME_ROOT` (`app/src/player_env.rs`).

| Platform | The game (read-only) | Saves and settings | How the game gets there |
|---|---|---|---|
| Linux, Windows | `ACNH-Universal/game/` in the contained folder; after a local Prepare the prepared folder (`<data>/acnh-universal/prepare`) | `<data>/acnh-universal/install/root/`: `user/device_save`, `user/backups`, `config.ini`. It sits inside the install folder a hub pull replaces (#340) | Prepare on the device. A pull from a hub brings the player only (#358). The cloud download (#321) |
| macOS | `ACNH-Universal.app/Contents/Resources/game/` | `~/Library/Application Support/ACNH-Universal` (`user/`, `config.ini`, `logs/`) | inside the zipped app Prepare delivers to `<dropbox>/_transfers/ACNH-Universal/mac/` |
| iOS / iPadOS | `ACNH.app/game/`, or `Library/Application Support/game/` with the small `.ipa` | the app's Documents folder (`user/`, `config.ini`, `logs/nx.log`); save-relay's config in `Library/save-relay` | inside the `.ipa` in `<dropbox>/_transfers/ACNH-Universal/ios/`; or downloaded at first start from `Apps/save-relay/acnh-universal-game/ios/` (I upload `ios/small/acnh-universal-game/ios/` there by hand) |
| Android | `<files>/game/` (`analysis/`, `romfs.pak`, `spirv.pak`, `fonts/`, `aoc/`, `libacnh_game.so`) | `<files>/root/`; never in a cloud backup or device transfer (`allowBackup="false"`, `res/xml/data_extraction_rules.xml`) | downloaded from `Apps/save-relay/acnh-universal-game/android/` (put there by `tools/stage_dropbox_game.sh` or by hand: Prepare's `android` target writes to `_transfers/ACNH-Universal/android/`, which the app does not read, #365); pulled from a hub (`HubPull.java`); prepared on the phone (`Prepare.java`); or copied from a folder on the device |

`<dropbox>` is `launcher.ini`'s `dropbox` key, else `Dropbox` in the home folder (`prepare::delivery_folder`). The Mac hub's Prepare writes both Apple targets into one prepared folder, `<data root>/prepared/`.

Rules that keep it portable: `crates/core` links no C library and no OpenSSL (the hub's crates are `mdns-sd`, RustCrypto `spake2`, `chacha20poly1305`, `sha2`, behind feature `hub`) and has `#![forbid(unsafe_code)]`; it does run host programs (see [What Prepare needs on a hub](#what-prepare-needs-on-a-hub)). No `std::os::*` outside `platform` and `steam` except `#[cfg(unix)]` file modes and tests; every OS rule takes its folders from `platform::Vars`, so Linux, Windows and Mac rules are tested on every runner; tests build temporary folders and never read a real game file. The runtime holds the `unsafe` and FFI, apart from the Android app's JNI entry and device query (`app/src/android_launcher.rs`, `app/src/input/android.rs`).

## How it fits together

```mermaid
flowchart TD
    subgraph Faces
      App["app/: Rust + Slint<br/>Continue, first run, Saves, Settings, Fleet, About"]
      CLI["crates/cli: acnh-universal check | build | play | sync | steam-add | hub | fleet | cloud-upload"]
    end
    Steam["Steam Game Mode shortcut + artwork"] -->|"opens the app full screen"| App
    App --> Core
    CLI --> Core
    subgraph Core["crates/core: acnh_core"]
      Folder["gamefolder, nso, version (the gate)"] --> Prepare["prepare: stages tools, game, version, recompile, shaders, fonts, build; delivery"]
      Prepare --> Assemble["assemble, pack_folder: the contained app"]
      Launch["launch: status, pull, run, push"] --> Saves["saves, savecard, backup"]
      Launch --> Relay["relay: save-relay adapter, cloud game download"]
      Hub["hub, bundle, fleet (CLI serves; launcher only pulls)"]
      SteamMod["steam, vdf, artwork"]
      Update["update"] --> Prepare
    end
    Prepare -->|"recompile stage"| Recomp["crates/recomp: acnh_recomp"]
    Prepare -.->|"python3, dotnet, zstd, cmake, ninja"| Host["host tools"]
    Recomp -->|"C++ in gen/"| GameLib["the game code: gen/ + games/acnh/ (C++, built on the hub, linked into the player)"]
    Launch -->|"NX_DATA_ROOT, NX_GAME_ROOT"| Player["the player"]
    subgraph PlayerParts["the player"]
      RT["crates/runtime (Rust): 11 group cores, relay, appgame"]
      Wr["runtime/src/*_wrapper.cpp + platform hosts + services still C++"]
      NVN["runtime/src/nvn, vk_*, mtl/, ui/ (C++, stays)"]
    end
    Player --> PlayerParts
    Wr --> RT
    NVN --> VK["Vulkan, MoltenVK, Metal"]
    Relay --> SR[("iannitram/save-relay")]
    Shell["android/: LauncherActivity (icon), Home, Prepare, HubPull, GameDownload, DropboxAuth"] -->|"the game into <files>/game/"| Player
    Shell -->|"on-phone Prepare"| UPrep["prepare/ (upstream C++, with its own recompiler)"]
    Hub <-->|"home network only"| Peers[("my other launchers")]
```

## Modules: `crates/core` (`acnh_core`)

| Module | Status | Owns | Musts |
|---|---|---|---|
| `error` | present | `Error` with a stable `code()` and a plain message per variant (`RelayUnreachable`, `RelaySignIn`, …); messages name files, never their contents. `NotBuilt`, `not_built` and the `not_built!` macro are dead (#336) | all |
| `platform` | present | `Os` with `key`/`from_key`; `Vars`; `AppDirs::for_os` (`<config>/acnh-universal`; `<data>/acnh-universal` with `install/`, the data root `install/root`, logs `install/logs`; on the Mac both are `~/Library/Application Support/ACNH-Universal`); `atomic_write`, `atomic_write_with` (#245); `link_dir`, `unlink_dir`, `is_dir_link` | M1, M2 |
| `version` | present | `Version` (3.0.2, 3.0.3, 2.0.7, 1.0.0) with name, build id, `native60`, `from_name`; `RECOMMENDED`; `from_build_id`, the gate that refuses any other build | M1 |
| `nso` | present | `build_id` from the first 0x60 bytes of `exefs/main`, `read_build_id` | M1, M14 |
| `nso2bin` | partial (#333) | `parse` (pure-Rust `lz4_flex`), `Nso::image`, `write`; the write is in place, not through a temp file | M1, M13 |
| `gamefolder` | present | `GameFolder` (root, `exefs/`, `romfs/` or `extract/romfs/`, `aoc/<index>/`, `from_importer`, version), `check` naming the first missing file | M1 |
| `config` | present | `Config` of `Line`s: `parse`, `get` (last wins), `enabled`, `set`, `to_text`; `config.ini` in the data root and `launcher.ini` in the settings folder | M2 |
| `profile` | present | `DeviceProfile` (amd-handheld 30 fps, strix-halo-tablet 60 fps, desktop), `builtin`, `by_id`, `detect`; settings as `config.ini` entries | M2 |
| `recipe` | present | `Hub` (strix-halo, desktop, mac), `hub_for`, `serves`, `hub_by_key`; `Stage` (tools, game, version, recompile, shaders, fonts, build) with `from_line`; `output` per target; `MAC_APP` | M1, M11 |
| `prepare` | partial (#331–#336, #365) | See [Prepare](#prepare) below. 3,746 lines in one file | M1, M13, M14, M18 |
| `pack_folder` | present | The contained Linux and Windows app folder (#191, #190): `app_out_dir`, `app_out_dir_for`, `PackContext`, `pack_folder_with`, the `app_dir` key in `launcher.ini`, `check_prepared`, refusal inside a git work tree | M2, M3, M18 |
| `assemble` | present | After the build stage, the self-contained player (1.22b): `assemble`, `Plan`, `assemble_with` behind a `Tools` seam (`patchelf`, `install_name_tool`, `codesign`). Linux: `acnh` and the libraries a stock system may lack beside it, found through `$ORIGIN`. Windows: `acnh.exe` and the DLLs it imports. Mac: `ACNH-Universal.app` with `Contents/MacOS/acnh`, the Vulkan loader and MoltenVK in `Contents/Frameworks`, an ad-hoc signature, arm64 only, refused inside a git work tree. `player`: the program a launcher starts. The recompiled code is linked into the player; no game library is shipped on desktop | M1, M4, M11, M14 |
| `appgame` | present | The phone game download and Android's private storage layout. `install`, `install_with` (copy a prepared folder into `<files>/game/`); `Manifest`, `ManifestFile` (`manifest.json`: `version`, `build_id`, `nx_game_abi`, each file's path, size and SHA-256; a hand-written JSON parser); the blocking `DownloadTransport` trait and `download` (resume over `game.partial/`, hash check with one refetch, free-space check, one rename, read-only); `partial_bytes`, `verify_partial`, `install_partial` for iOS's background session; `sha256_file`. Used by the desktop cloud download and by iOS. **Android's download does not use it**: `GameDownload.java` is its own implementation (#367) | M14, M18 |
| `msg` | partial (#334, #335) | Reflows extracted dialogue, mail and tutorial messages for the pt-BR mod (1.30): SARC order, MSBT sections and UTF-16 control tags preserved, archives replaced as a batch. Font advances come from a **Python Pillow probe** on the system Python and compression from the `zstd` program; when either is missing the reflow is skipped with at most one line on stderr and is not retried | M1, M13 |
| `pack` | partial (#333) | `romfs`, `romfs_range`, `romfs_size`, `spirv` write the `NXRP` and `SPK1` packs. `spirv` writes through a temp file; `romfs_range` writes the multi-gigabyte pack in place | M1, M13 |
| `sarc` | partial (#334) | Parse and rewrite endian-aware SARC archives; `fix_archives` (returns `Ok(0)` when `zstd` is missing) | M1, M13 |
| `shaders` | partial (#334, #335) | `plan`, `extract`, `cleanup`, `Runner`, `stage_with`: plan and extract shader groups by running **`tools/maxwell/containers.py`, `exefs_shaders.py` and `model_shaders.py`** on the system Python, find a .NET 10 SDK and the harness (`tools/maxwell/spirv_harness`, built to `private/build/spirv_harness/`), translate with optional SPIR-V validation, check the summary. Extraction failures are printed and dropped. On macOS the translator runs with `DOTNET_TieredCompilation=0` and `DOTNET_EnableWriteXorExecute=0` | M1, M13 |
| `glyphs` | present | The shared button TrueType font and the ParkExt rewrite for Xbox, PlayStation and Switch layouts (`button_font`, `controller`, `geometry`, `truetype`, `variants`); uses the `zstd` program; writes no `shared.ttf` when no host font is found | M1, M5, M13 |
| `saves` | present | `save_dir` (`<data root>/user/device_save`), `backups_dir`, `REQUIRED`, `is_synced`, `check_island`, `relay_manifest` | M6 |
| `savecard` | present | `SaveCard` (save time, device, play time, version, picture), `read_card` from `launcher-card.txt` (the relay's `saved` and `file_modified_ns` lines override the `main.dat` time), `latest`, `side_by_side` | M10 |
| `backup` | partial (#337) | `stamp` (UTC), `backup_saves`, `list`, `prune` (`KEEP` = 10): flat `user/backups/<stamp>/`. **Not the same layout as the game's own backups** (`user/backups/<island> <hash>/<local stamp>/`, `crates/runtime/src/saves.rs`), and `prune` counts those folders as its own | M7 |
| `update` | present | `update` backs up, prunes, rebuilds, checks the saves afterward and reports the backup on failure; the only caller of `prune` | M7 |
| `launch` | present | `Relay` and `Game` traits; `RelayStatus` (Ready, RelayNewer, LocalNewer, Held, Unavailable, Conflict, Off); `Sync`, `Keep`, `Played`; `play` (pull, conflict with both cards, run, push). `Sync::Offline` at the pull refuses play (#168) | M6, M10 |
| `bundle` | partial (#340) | `Bundle` (target, version, commit, build id, files with size and SHA-256), `DEVICE_OWNED` (`root`, `logs`), `is_forbidden`, `to_text`/`from_text`, `manifest`/`verify`, `install`. `manifest` is called only by tests: nothing publishes a bundle (#357). `install` moves `root/` through a temp folder | M11, M14 |
| `fleet` | present | `DeviceRecord`, `Drift`, `drift`, `warning`, `to_line`/`from_line` | M11 |
| `hub` (feature `hub`) | library only (#357–#360) | `is_home_network`; `announce`, `discover` (mDNS `_acnh-universal._tcp`, `SERVICE`); `Transport`, `TcpTransport`, `MemoryTransport`; `PairingSession`, `pair`/`pair_in` (six-digit SPAKE2; `MAX_FAILED_ATTEMPTS` = 3, `CODE_LIFETIME` = 5 min; the code is typed in, nothing generates one), `read_peers`, `remove_peer`; `serve`/`pull` (sealed, chunked: `CHUNK_SIZE` 64 KiB, `IO_TIMEOUT` 5 min); `Found`, `Peer`, `Clock`. `hub/prepared.rs`: `serve_prepared`/`pull_prepared`, the prepared-data protocol, **Android targets only**. `hub/service.rs`: `serve_request`, `pair_hub`, `pull_hub`, `pull_hub_prepared`, `pull_hub_prepared_app`, `report_hub`, `report_hub_build`, `read_fleet`. Only `acnh-universal hub serve` announces and serves; the launcher discovers, pairs and pulls. [Peer scan](peers.md) | M9, M11, M18 |
| `vdf` | present | Steam binary KeyValues `Value`, `write`, `read` (`MAX_DEPTH` = 64) | M2 |
| `steam` | partial (#341) | `crc32`, `shortcut_app_id`, `rungameid`, `userdata_roots`, `shortcut_entry`, `upsert` (matches on app id only), `write_shortcut` (Steam closed, one `.bak`), `launcher_shortcut`, `launcher_shortcut_for`, `assets_dir`, `assets_dir_for` | M2 |
| `artwork` | present | `Art` (grid, wide grid, hero, logo, icon), `file_name`, `size`, `grid_dir`, `paths`; made from the game folder's icon, else `assets/acnhrecomp.png` | M2 |
| `relay` (feature `relay`) | partial (#338, #339) | `SaveRelay`: `open`, `with_root`, `with_version`, `pull_with`, `map_sync`, `checkpoint` (`Checkpointed`, `Remark`), `remote`, `service`, `dropbox_tokens`; `setup_nextcloud`, `start_sign_in`, `finish_sign_in`, `finish_sign_in_cancellable` (`CANCELLED`), `forget`. The cloud game download: `RemoteDownloadTransport`, `download_game_from_remote`, `download_game_from_relay`; feature `cloud-stream` adds `DropboxStreamTransport`, `upload_game_to_dropbox`, `collect_linux_cloud_files` (#321). The device name falls back to the literal "device". 1,612 lines | M6, M18 |

`platform`, `vdf`, `steam`, `artwork`, `backup`, `bundle`, `fleet`, `launch`, `update`, `hub` and the shapes of `error`, `savecard`, `profile` and `recipe` are copied from iannitram/totk-universal (MIT) with credit in the file header.

### Prepare

`Request` (game, out, target, jobs, source, `FanMods`, `delivery`, `install`); `prepare`, `prepare_with`, `prepare_installed`. A packaged source is copied to `<data>/source/<commit>/` on every run and the build runs there (which also deletes the tools kept under it, #332); a package from another commit is refused. Stages run in order (`rust_tools`, `rust_game`, `rust_version`, `rust_recompile`, `rust_shaders`, `rust_fonts`, `rust_build`), each skipped when its outputs are there; a failure is a redacted `Error::Build` naming the stage. `Step::Script` and the script-following code remain but nothing uses them (#336).

- **Records in the prepared folder:** `.prepare-tools.json` (the Tools stage is skipped when it exists), `.fan-mod-settings`, `game_source.txt`, `gen/.recomp-hash`, `.speedfix-old-patch`, `.shaders.key`, `.romfs.pak.key`, `.spirv.pak.key` (`redo_unless_current`, `tree_key`, `file_key`: paths, sizes and modification times, PR #319).
- **The recompiler staleness check** (`recomp_hash`, #227) hashes the deleted `recomp/*.py` and the workspace version `0.0.0`: it is a constant and never notices a changed recompiler (#331). The same `0.0.0` is the "version" term of the three keys.
- **Tool pins** (`VK_HEADERS`, `LLVM` 23.1.3, `SDL3` 3.4.16, `IMGUI`, `RYUBING`, `DOTNET`, `FFX`, `DLSS`): fetched with `curl`, no checksum; imgui, Ryubing, .NET and the harness are checked by existence only, so a bumped pin does nothing on a hub that has the old one (#332, #328).
- **Fan mods:** `install_mods`, `install_mod_archive`, `speedfix_patch`, `speedfix_args`; the patch is `prepare/recomp/acnh/speedfix-3.0.2.pchtxt` / `-3.0.3.pchtxt`.
- **Targets.** `mac` (`mac_deliver`): preflight, stages, packs if missing, `assemble`, `tools/package_mac.sh` → `<delivery>/ACNH-Universal-<version>.zip`. `ios` (`ios_deliver`): preflight, stages, packs, refusal without a 32-hex build id or the runtime ABI, `tools/build_ios.sh iphoneos <out>/gen <out>/build-ios`, optional `tools/bundle_mods.sh`, `tools/package_ios.sh --game`, then `--small --no-folder` and `stage_ios_delivery` → `<delivery>/small/` with `manifest.json` (`IOS_SMALL_FOLDER`, `nx_game_abi`). `android` (`android_deliver`, `stage_android_delivery`): runs `tools/build_android.sh` with no arguments, which compiles the checkout's `private/acnh/gen`, not the folder just prepared; stages whatever `libacnh_game.so` it finds, with no build-id or ABI guard, into `_transfers/ACNH-Universal/android/` (#365). `stage_game_delivery` swaps folders through `.<name>.new` / `.<name>.old` with `remove_dir_retrying` (5 tries, 1 s apart, PR #318).
- **Checks:** `toolchain`, `toolchain_with`, `Missing`, `preflight`, `check_delivery`, `check_outside_git`, `redact`. They check cargo, clang, curl, python3 (Mac and Linux only), Xcode tools and Homebrew formulae on the Mac, SDL3/libvpx/libvorbis on Linux, MSVC on Windows; for `ios`, the SDK, cargo and the Rust target. They do not check `zstd`, `tar`, `bash`, `patchelf`, Pillow, python on Windows, or anything for Android (#334).

### What Prepare needs on a hub

Not in the earlier map. Every hub needs, at run time:

| Need | Used by | Checked by `toolchain`? |
|---|---|---|
| python3 with `venv` (and Pillow for the pt-BR reflow) | the venv that supplies `cmake` and `ninja`; `.zip`/`.tar.zst` and mod-zip extraction; the three `tools/maxwell/*.py` shader scripts; the font probe; `package_ios.sh`, `package_mac.sh`, `stage_dropbox_game.sh` | python3 on Mac and Linux; not Pillow, not on Windows |
| .NET 10 SDK | the shader translator (`spirv_harness`, built from Ryubing fetched from Codeberg) | downloaded when absent |
| `zstd` (program) | `sarc`, `msg`, `glyphs`, `containers.py` | no |
| `curl`, `tar`, `git`, `bash`; `patchelf` on Linux; `codesign`, `install_name_tool`, `xcrun` on the Mac; `cmd`, `vswhere`, vcpkg on Windows | fetching, assembling, signing | `curl` only |
| network during Prepare | GitHub, Codeberg, Microsoft, PyPI; SPIRV-Cross and mbedTLS through CMake `FetchContent`; SDL3, MoltenVK and libvpx for iOS; the mods from upstream's release | no offline mode |
| iOS target: Xcode's `iphoneos` SDK, the `aarch64-apple-ios` Rust target, Homebrew `cmake`, `ninja`, `vulkan-headers` | `build_ios.sh` | the first two |
| Android target: the Android SDK (`ndk;28.2.13676358`, `build-tools;35.0.0`, `platforms;android-35`), JDK 17+, the `aarch64-linux-android` Rust target; `tools/build_libvpx.sh android` run by hand (else black movies); for the on-phone Prepare, zig and a NativeAOT shader tool (Linux only) | `build_android.sh` | none |
| a host font (five fixed paths, DejaVu on Linux) | `shared.ttf` | no |

## Modules: `crates/recomp` (`acnh_recomp`)

The recompiler on every hub (3.05, #49). It replaced upstream's Python `recomp/`, deleted in commit `40bd295` (PR #286); the file headers still cite those Python files (#350).

| Part | Status | Files | Order |
|---|---|---|---|
| `Load` | present | `load.rs`: segments (its own LZ4), symbols, relocations | 3.01 |
| `Analyse` | present | `analyse.rs`, `ehframe.rs`, `decode.rs` (targeted AArch64 decoding: branch targets, jump tables) | 3.02 |
| `Emit` | partial (#349) | `emit/mod.rs` (branches, the jump-table switch, setjmp, system registers, function assembly), `integer.rs`, `memory.rs`, `regcache.rs`, `fp.rs`, `neon.rs`, `neon_wide.rs`, `structured.rs`, `extension.rs`. Capstone 5.0.6 (`capstone-sys`) supplies operands. Saturating and rounding NEON forms on 64-bit lanes are emitted wrongly | 3.03, 3.04 |
| driver | partial (#331) | `driver.rs` (chunking, workers, `.pchtxt` patches, never removes old chunks), `stats.rs`, `main.rs` (the `acnh_recomp` binary), `examples/recomp-emitter.rs` | 3.05 |

Upstream's C++ recompiler `prepare/recomp/` is a second, separate recompiler (about 5,900 lines with Capstone 5.0.7 vendored in `third_party/capstone`), still built as `nx_recomp`, `acnh_recomp` (the same name as the Rust binary) and `recomp_test`, linked into `nx_prepare` for the APK's on-phone Prepare. Only its source guard `recomp_source_test` left the build (#247); the file is still tracked. One recompiler is #351.

**The check.** `tests/recomp_emit.py` runs the Rust `recomp-emitter` example in four cache and hook modes, checks three substrings in the output, then runs `tests/difftest.py`, which executes Rust-emitted statements against Unicorn. There is no comparison with a Python driver any more. An instruction the emitter does not handle is counted as skipped and CI stays green (#325). Linux CI runs it with `capstone==5.0.6 lz4==4.4.5 unicorn==2.1.4` and the runner's `clang++`.

```sh
cargo build -p acnh-recomp --example recomp-emitter
python3 -m tests.recomp_emit target/debug/examples/recomp-emitter
```

If the default scratch address is unavailable on macOS, pass `--buffer-addr 0x600000000000`.

## The player: `crates/runtime` (`acnh_runtime`) and `runtime/src`

`crates/runtime` is a static library linked into the player; each Rust group exports `extern "C"` symbols (header from `cbindgen`, not checked in) that a C++ file in `runtime/src` calls. `build.rs` can drive the CMake build of `runtime/`; `tests/fake_game/` is the synthetic game of the boot smoke test. A player cannot link without the Rust archive.

**What is Rust and what is not.** Rust: 25,533 lines in `crates/runtime/src`. Non-renderer, non-overlay C++ still in `runtime/src`: 41 files, 20,439 lines, 40 of them compiled into the player. M13 does not name them as staying; #352 asks for the ruling.

| Group | Rust (`crates/runtime/src`) | C++ beside it (`runtime/src`) | Status |
|---|---|---|---|
| `Config` | `config.rs` 1,489 | `config_wrapper.cpp` 203 | ported |
| `Time` | `time.rs` 875 | `time_wrapper.cpp` 415 | ported |
| `Audio` | `audio.rs` 1,517 | `audio_wrapper.cpp` 374 | ported |
| `Memory` | `memory.rs` 1,077 (compact layout on an iOS device, desktop layout elsewhere) | `memory_wrapper.cpp` 343, `compact_layout.cpp` 58 | ported |
| `Input` | `input.rs` 2,338 | `input_wrapper.cpp` 443, `touch_input.cpp` 197 | ported |
| `Loader` | `loader.rs` 1,819 | `loader_wrapper.cpp` 477; `main()` is in each platform host | ported |
| `Kernel` | `kernel.rs` 2,754 (SVCs, IPC, the yield back-off) | `kernel_wrapper.cpp` 651: a second handle table mirrored from Rust, event state, the `sm:` registry | core ported; wrapper holds state (#356) |
| `Saves` | `saves.rs` 2,317 (save folder, users, zip, the game's own backups, the JKSV scan) | `saves_wrapper.cpp` 513 (the `acc:u0` services), `save_storage.cpp` 244, `save_profiles.cpp` 259, `save_unlink.cpp` 363, `hle_olsc.cpp` 435 | partial |
| `Files` | `files.rs` 2,354 (romfs, packs, fonts, glyphs, shaders, aoc, mods) | `files_wrapper.cpp` 592 (also loads a from-folder game library), `mod_manager.cpp` 674 (a static library nothing links: dead) | ported |
| `Services` | `services.rs` 3,467 (the leaf logic of am, set, misc, process) | `services_wrapper.cpp` 1,945 (the service-object graph, movie decode, about 600 registrations); **not ported:** `hle_hostnet.cpp` 673, `hle_vsock.cpp` 516, `hle_ssl.cpp` 625, `hle_npns.cpp` 379, `ldn_link.cpp` 485, `ldn_lan.cpp` 200, `nextendo.cpp` 1,112, `nextendo_http.cpp` 211, `nextendo_token.cpp` 152, `nextendo_status.cpp` 36, `nfp_host.cpp` 426, `amiibo_decode.cpp` 64, `acnh_amiibo_hook.cpp` 47 | partial |
| `Platform` | `platform.rs` 1,849 (roots, thermal and perf rules, pad kind) | `platform_wrapper.cpp` 603 (ADPF, thermal); **the hosts, still C++:** `platform_android_wrapper.cpp` 1,842, `platform_mac_wrapper.cpp` 1,313 (macOS and iOS), `platform_linux_wrapper.cpp` 911, `platform_sdl_wrapper.cpp` 690 (Windows), `platform_win_wrapper.cpp` 352, `platform_android_input_wrapper.cpp` 327, `platform_android_audio_wrapper.cpp` 241. They own the window, the event loop, the audio stream, the JNI entry points and `main` | partial |

Also in `crates/runtime`:

- **`relay`** (iOS and Android only): the player's save sync. `relay.rs` is platform-free: `decide` and `Action` (with `RefuseOffline`), the `Prompt`, `Connector` and `Settings` traits, `FileSettings`, `Roots`, `Player::before_game`/`after_game`/`checkpoint`/`resume`, and for Android `FileStore` (a `0600` credential file), `BridgePrompt`, `CallbackTransport`. `relay_ios.rs` (`relay::ios`): the Keychain store, the `UIAlertController` prompts (Nextcloud server, user, app password: the only provider, #361), and `acnh_relay_before_game`, `_after_game`, `_terminate` (the final push, bounded to 3 s), `_background`, `_foreground`, `_sync_now`, `_sync_status`, called from `platform_mac_wrapper.cpp`. `relay_android.rs` (`relay::android`): `acnh_relay_android_before_game`, `_after_game` (not bounded, #354), `_background`, `_ready`, `_resume`, `_sync_now`, `_sync_status`, `_state`, `_forget`, `_sign_out`, `_cred_get`/`_set`/`_delete`, called from `runtime/src/relay_android.cpp` (219 lines, the JNI natives of `Relay.java`); Dropbox only. Both call back through `nx_relay_log`, `nx_relay_ask_off`, `nx_relay_remember_not_now` (so C++ stays the only writer of `config.ini`), and on Android `nx_relay_ask` and `nx_relay_http`.
- **`appgame`, `appgame_ios`** (#231): `appgame.rs` drives `acnh_core::appgame` step by step as JSON (`plan_json`, `finish_json`, `installed_matches`, `api_arg`); `appgame_ios.rs` holds the Dropbox sign-in for the download (a code pasted from Safari, the refresh token in the Keychain under its own account) and the `acnh_appgame_*` exports. `runtime/src/appgame_ios.mm` (766 lines) shows the first-start screens and moves the bytes in a background `URLSession` in 1 GiB `Range` chunks (`kChunk`).
- **`hub_android`** (Android only): the bridges `HubPull.java` calls (#80), with `runtime/src/hub_android.cpp` (63 lines).
- **Tests:** `tests/ios_relay.rs` (35 tests), `tests/android_relay.rs` (44) and `tests/ios_appgame.rs` (5) include the platform-free sources by path so they run on every desktop runner; `tests/header.rs` checks two properties of the generated header.

`unsafe` blocks: `kernel.rs` (63), `services.rs` (79) and `memory.rs` (9) carry no `// SAFETY:` line; `audio.rs`, `files.rs`, `input.rs` and `saves.rs` carry them on some. ADR 0002 says every block has one (#356).

Kernel env (`kernel.rs`): `NX_YIELD_BACKOFF=start_us,cap_us` (default `2000,250`; empty or `0` = off) makes a guest thread that yields in a loop without changing x19..x28 sleep per yield; `NX_SPIN_NAPS=1` takes precedence with the older flat 0.5 ms nap. `runtime/src/yield_backoff.h`, the credited copy from nxrt, is included by nothing (#327).

### C++ that stays (SPEC M13)

| Path | Owns | Musts |
|---|---|---|
| `runtime/src/nvn/`, `vk_*.cpp`, `runtime/src/mtl/` | the NVN renderer on Vulkan, MoltenVK and Metal. `nvn/nvn_aspect.cpp` (`tests/aspect_test.cpp`, #91): aspect, projection, safe-edge and letterbox math. `nvn/nvn_bc.cpp`: BC1–5 and BC7 decoded on the CPU where the GPU lacks them (`NX_FORCE_BC_DECODE=1`). `nvn/nvn_pipeline_cache_blob.h`: the saved pipeline cache's header and size check (#243). `nvn/nvn_dedup.cpp`, `nvn_sets.cpp`, `nvn_vk.cpp`: the Mali-G710 work (#268). `vk_swapchain.cpp`: Android pre-rotation (#250). `nvn/nvn_ultrawide.cpp`: the view as tall as 0.45 of 16:9. `video.shadows` / `NX_SHADOWS` (#267) | M2–M5, M9, M17 |
| `runtime/src/ui/` | the in-game settings overlay (`settings.cpp`, `i18n.cpp`): the version in the footer (#263), `mods.speedfix` with `video.frame_rate = 60` and `video.mode = handheld` on Android (#266), Sync now on the Saves page (#260), "Back to the start screen" on Android (#262) | M2 |
| `bug_report.cpp`, `frame_stats.cpp`, `discord_presence.cpp`, `cpu_profile.cpp`, `sampler.cpp`, `glyphs.cpp`, `app_icon.cpp`, `controller_kind.h`, `mbedtls/` | overlay and peripheral files. `controller_kind.h` decides the in-game pad prompts by vendor or name; the launcher uses a different table (#347) | |
| `games/acnh/` | the per-game C++ the recompiled code links with, `versions.txt` | M1 |
| `gen/` (made on a hub, never committed) | the recompiled game code | M1, M14 |

The Android Vulkan driver choice lives in `platform_android_wrapper.cpp` (`load_vulkan`, `vulkan_preflight`): `NX_GPU_DRIVER`, else `video.gpu_driver` (auto, turnip, system, custom). On Adreno, auto loads the bundled Turnip through libadrenotools (the system driver on Adreno 8xx); any other GPU, and any failure, uses the device's own `libvulkan.so`. `vulkan_preflight` answers what the GPU lacks and `Home.java` says so before starting the game (#235).

## Faces

| Path | Status | Owns | Musts |
|---|---|---|---|
| `crates/cli` (`acnh-universal`) | partial (#343) | `lib.rs` (`run(args, &mut Context)`, in-process for tests; `main.rs` calls `run_process`), `sync.rs` (desktop only). Commands: `check`; `build` (`--target ios\|mac` on the Mac delivers to Dropbox; `--out` defaults to `<data root>/prepared` for those and to a compile-time checkout path for the others); `play` (`--profile`, `--keep relay\|here`, `--anyway`; exit 3 conflict, 4 held, 5 unreachable at the pull, 6 push conflict after the game unless `--keep` was given, which then answers it silently; an offline push exits 0); `sync setup [--device]\|sign-in gdrive\|dropbox\|status\|off\|push [--keep]`; `steam-add` (no artwork); `hub serve [--pair] [--prepared]\|pair\|pull\|report`; `fleet`; `cloud-upload --os linux --install <folder> --prepared <folder>` (Dropbox only, #321); `help`; `--json` prints the error `code()`. Usage errors exit 2, also for `build --help` | M1, M2, M6, M11 |
| `app/` (`acnh-app`) | partial | The Slint launcher on desktop and, with feature `android-launcher`, on Android (`android_launcher.rs`, `LauncherActivity.java`). `main.rs` (`--fullscreen` for the Steam shortcut), `shell.rs` (the session and the 16 ms timer that pumps jobs and polls pads, with no focus check and no close handler, #342), `lib.rs` | M2, M4, M9–M11 |
| `app/src/play.rs` | present | Continue's action, the player as a child process (`Command`, which is why the Android launcher cannot start the game, #363), build progress, save-sync results with conflict and hold choices; `Request::SyncNow`, `Request::Retry` | M6, M10 |
| `app/src/build_actions.rs` | present | `BuildActions` (`game_folder`, `launch_location`, `installed`, `build`, `update`): Prepare and Update off the window thread, the contained folder (`pack_folder`), the hub pull. On the Mac hub Prepare runs both Apple targets. `installed` hashes every file of a pulled install on the window thread (#345) | M1, M7, M11 |
| `app/src/player_env.rs` | present | `player_setup`: `NX_DATA_ROOT`, `NX_LOG`, `NX_GAME_ROOT` and the folders they need | M1, M4 |
| `app/src/first_run.rs` | partial (#345, #346, #358) | Local Prepare and assembly off the window thread; a hub pull of the player (after which first run asks for a local Prepare and cannot finish without one); `cloud_download_and_sync` (the cloud-first path: no cancel, no progress, no free-space check); `pick_folder_native` (kdialog or zenity, Linux only); device defaults and the Linux Steam shortcut with artwork. `FirstRun::needed` tests a path nothing writes | M1, M2, M10, M11 |
| `app/src/relay.rs` | present on desktop | `system()`, `choose` (the adapter, `RelayOff` or `RelayFailed`), `opened_each_step` | M6 |
| `app/src/relay_setup.rs` | present | The `RelaySetup` seam over `acnh_core::relay`, its runner, `RelaySettings`, `RecheckWhenClosed`, `machine_name` (used for Nextcloud only, #338), `fake::FakeSetup` (compiled into release builds). Not linked on Android and iOS | M6, M10 |
| `app/src/vm/` | partial | `continue_page` (2,156 lines: the states, the waiting push, Retry; in `RelayUnavailable` Retry never starts a game, in `RelayError` and `PlayerFailed` it runs Play again, which still pulls first; the drift warning is unwired, #360), `saves` (Sync now; lists the game's island folders as backups), `settings` (the pt-BR and SpeedFix switches (#85); the save sync rows `RelayRows`), `about` (`offered_fixes.txt`, empty; the Slint notice), `fleet`, `first_run` (the five-choice Welcome, folder choice, hub discovery and pairing on a keypad, progress, save sync, device setup), `relay_setup`, `text_entry` (the on-screen keyboard), `hints`. `vm.rs` holds `Nav` and `InputDevice` | M8, M10, M11 |
| `app/src/input.rs`, `input/android.rs` | partial (#347) | gilrs events, keyboard, and on Android key events only (no stick or HAT) to `Nav`; its own vendor-and-product table for the pad kind, hot-plug, last-used input, stick and D-pad repeat. `Nav::Quit` is never produced | M9, M10 |
| `app/src/ui.rs`, `app/ui/` | present | View state to Slint; `main.slint` (also Settings), Continue, About, Fleet, Saves, `first_run.slint` (also the save sync pages), `hints.slint` with the glyph SVGs (ten unused). English only; fixed text sizes | M10 |
| `app/tests/`, `crates/cli/tests/`, `app/build.rs` | present | the faces' integration tests and the Slint build | M15 |
| `docs/design/continue-screen.md` | behind the code (#348) | the Continue screen brief, shared with totk-universal. Not built: Restore on Saves, a device profile and Add to Steam in Settings, "Pair a device" and "Share this build" on Fleet, the version on About, page switching with Prev/Next. Not in the brief: the cloud-first Welcome | M10 |

## Android: `android/`

The APK shell, no Gradle; Java in `java/io/github/acnhrecomp/` and `java/org/saverelay/`.

| File | Whose | Owns |
|---|---|---|
| `LauncherActivity.java` | ours (#79 / PR #304) | an empty `NativeActivity` subclass that loads `libacnh_app.so`; **the only MAIN/LAUNCHER activity**; `tools/build_android.sh` always builds it |
| `Home.java`, `Screen.java`, `Picker.java`, `SignIn.java`, `TextInput.java` | upstream's, extended | the start screen: folder picker, on-phone Prepare, hub pull, Dropbox download, the save-sync step, start. Exported, and takes an `install_from` folder from the starting intent with no check (#364). `<files>/game/` and `<files>/root/` (#192) |
| `Prepare.java`, `PrepareService.java`, `PrepareNative.java` | upstream's | the on-phone Prepare over `libacnh_prepare.so` |
| `HubPull.java` | ours (#80 / PR #296) | a paired hub pull into `<files>/game/` |
| `GameDownload.java`, `GameDownloadJob.java` | ours (#230 / PRs #233, #298) | the in-app download: a user-initiated `JobService` on API 34+ or a `dataSync` foreground service; its own manifest parse, HTTP Range resume, SHA-256 and move-in, no Rust; ignores the manifest's build id and ABI (#367) |
| `DropboxAuth.java` | ours | OAuth 2 with PKCE through save-relay's Dropbox app (#261, #262) |
| `Relay.java`, `org.saverelay.*`, `third_party/save-relay/save_relay_jni.c` | ours and save-relay's | save sync (#81 / PR #259) |
| `AndroidManifest.xml`, `res/` | | `allowBackup="false"`, Vulkan 1.1, a resizable `NativeActivity` (exported), `Home` `singleTask` (exported), `RUN_USER_INITIATED_JOBS`, `MANAGE_EXTERNAL_STORAGE` (for the folder picker), `res/xml/shortcuts.xml` (the long-press "Change game folder" into `Home`) |

## Upstream's Prepare in C++: `prepare/` (from the sync, #246)

Upstream's own preparation, ported from ACNHRecomp-Importer v1.0.7 (MIT). It is what the APK's on-phone Prepare runs; our hubs prepare with `crates/core` and `crates/recomp`. It is also built on desktop CI, where nothing ships it.

| Path | Owns |
|---|---|
| `prepare/prepare.h`, `prepare.cpp`, `prepare.cmake` | `prep::start`, `status`, `cancel`: Check, ExeFS, Recompile, Shaders, Packs, Compile, Finish on a worker thread |
| `prepare/prepare_cli.cpp` | `acnh_prepare` (exit 0 ready, 1 a stage failed, 2 refused, 4 cancelled, 64 a wrong command line) |
| `prepare/prepare_jni.cpp` | `libacnh_prepare.so` for `PrepareNative.java` |
| `prepare/io/` | the probe of the picks, the NSOs, `romfs.pak` and `spirv.pak` (`prepare_io_test`); included by every desktop configure |
| `prepare/gamelib/` | the game library from `gen/` with zig |
| `prepare/recomp/` | upstream's C++ recompiler (see the recompiler section); `acnh/speedfix-3.0.2.pchtxt` and `speedfix-3.0.3.pchtxt` are tmouh's SpeedFix (CC BY-NC 4.0, `acnh/SPEEDFIX.md`; #329) |
| `prepare/prepare_e2e_test.cpp`, `fake_shader_tool.cpp` | end-to-end tests on synthetic input; CI leaves both out, though only `prepare_e2e_zig` needs zig (#325) |

## Tools and scripts

| Path | State | What it is |
|---|---|---|
| `tools/build_ios.sh`, `tools/check_ios_compile.sh`, `ios/Info.plist.in` | live | the iOS build: the Rust static library, CMake, static SDL3 (3.4.18 here, 3.4.16 in Prepare) and MoltenVK, libvpx built when missing (stops without it), `UIFileSharingEnabled`; takes the generated-code and build folders as arguments. Picks the runtime header with `find \| head` (#369) |
| `tools/package_ios.sh` | live (#362) | `<version> <build dir> --game <prepared>\|--no-game\|--small <prepared> --build-id <32 hex> [--abi <12 hex>] [--no-folder] <out folder> [--mods <dir>]`: the arm64 device bundle as `<out>/ACNH-Universal-<version>.ipa`, staged in a `.partial` folder inside the out folder (a leftover refuses later runs); under CI the out folder must be in `$RUNNER_TEMP` and game items empty |
| `tools/package_mac.sh` | live (#362) | `<version> <app> <game folder>\|--no-game <out folder> [--mods <folder>]`: `ACNH-Universal.app` with the game in `Contents/Resources/game/`, an ad-hoc signature, zipped without `.DS_Store`. Only Prepare's `mac` target runs it |
| `tools/remove_tree.sh` | live | removal that tries 5 times, 1 s apart, while Finder or Dropbox writes a `.DS_Store` (PR #318) |
| `tools/stage_dropbox_game.sh` | broken on Linux (#368) | `<prepared> <game library> <dest>` (Android) or `--os linux --install <install> <prepared> <dest>`: copies the game into `<dest>/acnh-universal-game/<os>/` through `.new`/`.old` and writes `manifest.json` last. Its `stat` and `shasum` calls are macOS forms; it writes empty build-id and ABI fields |
| `tools/build_android.sh`, `tools/build_version.sh`, `tools/rust_licenses.py`, `tools/make_android_icon.py` | live (#365, #366, #369) | the APK: `[--install] [--jobs N] [--no-game] [--no-phone-prepare]`. Builds `libacnh_runtime.a`, `libacnh_app.so` (always, with `android-launcher`), CMake with the NDK into `private/acnh/build-android`, then javac, d8, aapt2, zipalign and apksigner with a debug key generated per checkout, into `private/acnh/apk-app/acnh.apk`. The generated-code folder is fixed to `private/acnh/gen`. The game code goes beside the APK as `libacnh_game.so`. `build_version.sh` takes the base version from whatever tags the clone has (#324) |
| `tools/build_libvpx.sh` | live | `android\|iphoneos\|iphonesimulator`: libvpx v1.16.0 as a static VP8/VP9 decoder into `private/third_party/libvpx-android/` or `libvpx-ios/<sdk>/`. `build_android.sh` does not call it |
| `tools/build_turnip.sh`, `tools/patches/` | live (#369) | Mesa Turnip for Adreno with the NDK; a patch that no longer applies is skipped silently |
| `tools/fetch_zig.py`, `tools/build_shader_tool.py`, `tools/make_fake_gen.py` | live | the zig and the NativeAOT shader tool the on-phone Prepare bundles, and the stand-in `gen/` for `prepare_e2e_zig` |
| `tools/maxwell/` | live, on every hub | `spirv_harness` (.NET 10, with `extract/*.cs` and `translator/*.patch`), and the Python the shaders stage runs: `containers.py`, `exefs_shaders.py`, `model_shaders.py`, `yaz0.py`, `yaz0_fast.c`, `apply_patch.py`; `bntx.py` is a developer tool |
| `tools/bundle_mods.sh`, `tools/check_bundled_mods.sh` | live | the fan mods a package carries, and the check the package scripts run |
| `tools/check_no_game_data.sh`, `tools/check_release_files.sh` | live (#326) | the commit and CI game-data check, and the release file-list check |
| `tools/boot_smoke.sh` | live | the fake-game boot and frame gate (4.01), Linux CI |
| `tools/gen_shaders.py`, `tools/gen_spirv_inc.py`, `tools/gen_vk.py`, `tools/shaders/`, `tools/fonts/make_ui_cjk_font.py`, `tools/make_icons.sh` | live, by hand | upstream's generators of committed sources and assets |
| `tools/translate_shader_misses.sh`, `tools/bc/`, `tools/play/`, `tools/cpu_profile.py`, `tools/symbolize.py`, `tools/find_*.py`, `tools/frames.py`, `tools/burst_anomalies.py`, `tools/detect_explode.py`, `tools/capture.ps1`, `tools/record_run.ps1`, `tools/msl_check.swift` | developer only | upstream's analysis and play-test tools (M13 lets them stay) |
| `tools/push_android.sh`, `tools/android_install.sh`, `tools/setup_60fps.sh` | broken (#327) | upstream's adb route and 60 fps setup: they call `tools/pack_romfs.py`, `tools/pack_spirv.py`, `tools/setup_acnh.py` or `python -m recomp`, all deleted. `docs/ANDROID.md` and `README.md` (upstream's) still describe them |
| `tools/probe.py`, `tools/gen_mods.py`, `tools/intel/`, `tools/regress.ps1`, `tools/capture_burst.ps1` | dead (#327) | import the deleted `recomp` package, write a `games/hades/` that does not exist, or are referenced by nothing |
| `tools/acnh_mac.sh`, `tools/acnh_linux.sh`, `Jogar ACNH.command`, `tools/install_mac_shortcut.sh` | upstream's | run presets that assume `private/acnh/build`; `Jogar ACNH.command` imports the deleted `setup_acnh` |
| `tools/discord_guard.py`, `.github/workflows/discord-guard.yml`, `.github/FUNDING.yml` | upstream's, inert here | the workflow is guarded to upstream's repo and is skipped every 5 minutes (#327) |

## Tests

| Path | Runs in CI? |
|---|---|
| `crates/*/tests/`, `app/tests/` | yes, all three OSes |
| `tests/recomp_emit.py`, `tests/difftest.py` | Linux |
| `tests/aspect_test.cpp`, `astc_test.cpp`, `controller_kind_test.cpp`, `game_root_test.cpp`, `glyph_layout_test.cpp`, `gpu_pick_test.cpp`, `ldn_net_test.cpp`, `nextendo_test.cpp`, `pipeline_cache_blob_test.cpp`, `tiling_test.cpp`, `touch_safe_test.cpp`, `prepare_io_test`, `recomp_test` | Linux (`-DNX_TESTS=ON`) |
| `tests/layout_test.cpp`, `desktop_layout_test.cpp` | macOS |
| `tests/memguard_test.cpp` (Windows with the runtime archive), `tests/touch_input_test.cpp` (macOS with it), `tests/shader_cache_test.cpp` (needs another game's folder), `prepare_e2e` | **no** (#325) |
| `tests/game_root_check.sh`, `tests/android_app_check.sh` | Linux |
| `tests/package_mac_test.sh`, `tests/package_ios_test.sh`, `tests/stage_dropbox_game_test.sh`, `tests/race_rm.sh`, `tests/ios_stub_gen/` | macOS only (the staging test should also run on Linux, #368) |
| `tests/astc_check.py`, `tests/metal_shader_check.mm` | no, by hand |

## Build and test

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test --workspace --all-features    # with the hub
cmake -B build -G Ninja -DNX_TESTS=ON -DNX_GAME=acnh && ninja -C build   # NX_GAME defaults to a game that is not here
```

CI (M15), `.github/workflows/check.yml`; runs on `main` cancel each other, so most commits never get a finished run:

| Job | Runner | Runs | State on `main` |
|---|---|---|---|
| `no-game-data` | ubuntu-24.04 | `tools/check_no_game_data.sh` over every tracked file | green |
| `rust` (Linux, Windows) | ubuntu-24.04, windows-2025 | fmt, clippy and tests (default and all features), the save-relay link check; on Linux the emitter smoke test and Unicorn oracle | green |
| `cpp` | ubuntu-24.04 | CMake with `NX_TESTS`, every `*_test` but `prepare_e2e_test` and `shader_cache_test`, ctest, `tests/game_root_check.sh` | green |
| `boot-smoke` | ubuntu-24.04 | SDL3 3.4.16 and Vulkan-Headers 1.4.357 from source, `tools/boot_smoke.sh` | green |
| `cpp-windows` | windows-2025 | clang-cl (LLVM 20.1.8), the C++ parts and tests, ctest, the player | **red since it was added on 2026-11-24** (the runner's MSVC STL needs Clang 22 or newer); its test and player steps have never run (#322) |
| `android` | ubuntu-24.04 | NDK 28.2.13676358, `cargo build --workspace --target aarch64-linux-android`, `tests/android_app_check.sh` | green |
| `rust-macos` | macos-15 | clippy and tests with all features, the iOS check and runtime library, the two layout tests, `check_ios_compile.sh`, `build_ios.sh` for both SDKs, the bundle checks, `prepare_runner ios_real`, the Mac, staging and iOS package tests | green |

`release.yml` (a `v*` tag, or by hand) is meant to build `acnh-app` and `acnh-cli` for Linux (AppImage), Windows (zip) and the Mac (zipped app), package the source an installed launcher prepares from, check each file list and publish a draft release. It has never run and cannot: it archives the deleted `recomp/`, and the packaged source lacks `prepare/`, `third_party/` and `android/` (#323). No workflow builds the APK, Android's libvpx, the Mac player or any Android C++, signs or notarises anything, or attaches licence texts (#329). No Rust toolchain is pinned; actions are pinned by tag; `save-relay` is pinned only by `Cargo.lock` (#328). The workspace version is `0.0.0` and origin has no tags (#324).

## Build hubs (ADR 0004)

| Hub | Builds for | Clients |
|---|---|---|
| the Strix Halo tablet | linux, android | itself; the AMD handheld (by pull: #357, #358); the Android device (the game library staged into my Dropbox) |
| the RTX 3090 Ti desktop | windows | itself |
| the Mac | mac, ios | itself, as `ACNH-Universal.app` zipped into `Dropbox/_transfers/ACNH-Universal/mac/`; the iPhone or iPad as `ACNH-Universal-<version>.ipa` in `ios/`, with the small `.ipa` and its game folder in `ios/small/`, installed with SideStore |

How a hub gets its launcher in the first place is not settled: there is no release to install from (#323), so today a hub runs from a checkout. Under M18 the Linux and Windows folders are built on each device, Android keeps its game in private storage, and the Apple apps go to my own Dropbox. Android and iPhone/iPad may download the prepared game from my own Dropbox at first start. The launcher's cloud download for PCs goes beyond what SPEC allows (#321). Game data stays out of Git, CI, releases and logs (M14).

## Not verified yet

- The four build ids are upstream's (`games/acnh/versions.txt`); only 3.0.3 has been built for real.
- Nothing in the audit of 2026-11-25 was run. Each open bug issue says "reproduce before fixing".
- The Windows player, the Mac player and the Android C++ have never been built in CI; the Windows C++ unit tests have never run there.
- A release: `release.yml`, an installed launcher preparing from a packaged source, the file-list check on real artefacts.
- The Android front door since PR #304: every device check on record predates it. Whether the Slint launcher on Android receives pad buttons at all.
- The Pixel Fold's inner and outer screens on the device (checked on an emulator); Android on Adreno with the bundled Turnip; GPUs other than Mali-G710; the movies and the sound moving to a new output on a phone; the quit push on Android.
- The `.ipa` and the small `.ipa`'s download on a device; the compact layout booting the game; SideStore installing the real `.ipa` (#74); the controller mapping (#75); the 60 fps `.ipa` (#264); whether a pad can answer the iOS save-sync alerts.
- The Mac app from my Dropbox on my Mac (#73), Gatekeeper on first open, its session (#77), its controller mapping (#124), save-relay in the released app (#121).
- Any island crossing between two devices; the fleet warning; a hub pull between two real machines; mDNS from Game Mode and on Android; the credential store inside a Game Mode session; macOS's Local Network prompt and Windows Firewall for a hub.
- Whether the game uses the NEON forms #349 lists.
- The play time on the save card: written by the launcher to `launcher-card.txt` after each session, since the game's own count needs a save reader (after 1.0).

## Docs

| Path | What it is |
|---|---|
| `SPEC.md`, `ROADMAP.md`, `GLOSSARY.md`, `PIPELINE.md`, `AGENTS.fork.md`, `CLAUDE.md`, `PEERS.md`, `CHANGELOG.fork.md`, `NOTICE`, `LICENSE` | ours: the spec, the plan, the words, the stage, the agent rules, peers, our changelog, credits. Several are behind `main` (#330); `NOTICE` is incomplete (#329) |
| `docs/adr/` (0001–0006, `README.md`) | decisions; 0001–0006 each have a stale line (#330) |
| `docs/design/continue-screen.md` | the launcher brief (#348) |
| `docs/apple-ios.md`, `docs/IOS.md`, `docs/peers.md` | ours: the address-space finding (#72), the iOS build notes (its `package_ios.sh` example no longer runs, #330), the peer scan of 2026-11-17 |
| `README.md`, `docs/ANDROID.md`, `docs/STATUS.md`, `docs/ACNH.md`, `docs/NATIVE60.md`, `docs/NATIVE_METAL.md`, `docs/PARALLEL_REPLAY.md`, `docs/LOCAL_PLAY.md`, `docs/NEXTENDO.md`, `docs/VERSIONS.md`, `docs/REFERENCES.md` | upstream's. `README.md` and `docs/ANDROID.md` still describe `tools/setup_acnh.py`, `push_android.sh` and the memory-card route, which are gone or broken here. `docs/ANDROID.md` also has our additions (the Dropbox download; its line that the iPad syncs saves through Dropbox is wrong, #361). We do not edit upstream's text for pipeline reasons |
