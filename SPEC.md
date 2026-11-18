# ACNH Universal: spec for 1.0

**Approval:** 2026-11-17, in the Mac satellite session: "Everything is approved", after I was told that my save-relay and controller rulings of 2026-11-17 widen Musts (here M5, which now covers the Mac and a controller connected or disconnected mid-game, quoted in M5) and that a changed Must needs my approval again; it approves the spec as changed in PR #122. Before that: 2026-11-02, in the Project Manager session's question tool: "Approve all, same plan (Recommended)", with my word for every copy: "every -universal will have the same goals + roadmap". The plan is the one approved for totk-universal (2026-11-01) and Uncharted-Universal (2026-11-01): one Rust engine, a Continue screen, home build hubs, six device families, the same save everywhere. Only what is particular to this game changes here: the title, the versions, the "plays" bar, upstream's Android work as our Android base, and the real code layout. Later the same day, two rulings of mine: iPhone and iPad: "Free Apple ID for now (Recommended)": the iOS app is signed with a free Apple ID and re-signed every 7 days; the paid Apple developer account is a later upgrade (M4). Fan mods: "Make them optional (Recommended)": the pt-BR translation and the SpeedFix are off by default, with a launcher switch for each (M16, #85). Both are my own rulings, so the spec stays approved. A changed Must or Won't sets this back to "not yet" until I approve again.

## 1.0 in one sentence

I play *Animal Crossing: New Horizons* (title ID `01006F8002326000`), version 3.0.2, 3.0.3, 2.0.7 or 1.0.0, built from my own game folder, on my AMD handheld and my Strix Halo tablet (SteamOS, from Game Mode), my RTX 3090 Ti desktop (Windows), my Mac, my iPhone or iPad with a controller, and an Adreno Android device with a controller; it plays a week of island life on the AMD handheld (M12); and the same island loads on every device through save-relay.

## A copy, built our way

This repo is a copy of [Nycolazs/ACNHRecomp](https://github.com/Nycolazs/ACNHRecomp) (MIT), not a GitHub fork; the `upstream` remote points there, fetch only. Upstream's plan is in this repo: `docs/STATUS.md` (in Portuguese: what works, what is next), `docs/NATIVE60.md` (60 FPS), `docs/NATIVE_METAL.md` (Metal on Mac, then iOS), `docs/ANDROID.md`, `docs/LOCAL_PLAY.md` (local wireless over the LAN), `docs/PARALLEL_REPLAY.md`, `docs/VERSIONS.md`. Upstream keeps all of that. Our delta (R18):

| Delta | Upstream? |
|---|---|
| Our code in Rust: the launcher, the prep tools (`tools/setup_acnh.py` and the steps it runs, `recomp/`) and the host runtime (`runtime/src` outside the renderer); the recompiled game code, the NVN renderer and the in-game settings overlay stay C++ behind the game library's C ABI | keep on our fork |
| Continue screen launcher (Rust + Slint) replacing `launcher/`, home build hubs, fleet view, Steam artwork | keep on our fork |
| The four-version gate (refuse any other build id with a plain message) | keep on our fork |
| An iPhone/iPad build | keep on our fork; the Metal renderer itself is upstream's (`runtime/src/mtl`) |
| Shared saves through save-relay | keep on our fork |
| SteamOS packaging and handheld profiles (the AMD handheld, the Strix Halo tablet) | keep on our fork |
| Fixes to upstream's code that any player hits (crashes, saves, input, a platform build) | send upstream as bug reports or a patch, at my word (R36) |

Upstream is very active. Syncs (R21) keep coming for the renderer, `games/acnh/`, `recomp/` and docs. Once a module of ours is Rust, upstream's C++ or Python change to it becomes an `upstream-sync` issue that ports the change by hand; that cost is accepted, and the runtime moves last for that reason (ADR 0002).

## Musts

Device checks are `needs-owner-device`: they need my machines and my game folder.

| # | Must | Done looks like | Check |
|---|---|---|---|
| M1 | WHEN I point the launcher on a build hub at my game folder (`exefs/` with `main`, `sdk`, `subsdk0`, `main.npdm`; `romfs/` with the update applied; `aoc/` when present), THE LAUNCHER SHALL check the folder, read the build id of `exefs/main` and build the game for every device that hub serves: the Strix Halo tablet for both SteamOS handhelds and Android, the RTX 3090 Ti desktop for Windows, the Mac for Mac, iPhone and iPad. It SHALL accept only 3.0.2 (recommended), 3.0.3, 2.0.7 and 1.0.0 and refuse any other build with a plain message naming the four, SHALL keep the game folder and what is made from it (`gen/`, the game library, `romfs.pak`, shaders) on the three hubs and my own devices only, and SHALL never copy, upload or log game files beyond the home network | A working build per device from its hub; a folder of another version is refused with the message; a folder missing `exefs/main` says which file is missing | core tests with fake game folders (a fake NSO header with each build id); one real build per hub |
| M2 | ON the AMD handheld and the Strix Halo tablet, THE LAUNCHER SHALL add itself to Steam with the game's artwork (grid, hero, logo, icon: from the game folder's icon when it has one, else the project's own `assets/acnhrecomp.png`) so Game Mode opens the Continue screen (M10), and the game SHALL play on the built-in controls with that device's profile (resolution, frame cap, power) | Steam's library shows the artwork; an hour of play from Game Mode with no crash, on each handheld | on each handheld |
| M3 | ON the RTX 3090 Ti desktop, THE GAME SHALL play from the launcher with a controller | An hour of play with no crash | on the device |
| M4 | ON my Mac, THE LAUNCHER SHALL build and run the Mac app natively on Apple silicon (no Rosetta), and SHALL build the iPhone/iPad app, sign it with my free Apple ID (re-signed every 7 days; the paid account is a later upgrade) and install it over a cable | The game plays on the Mac; the iPhone or iPad app installs and reaches the island | on the devices |
| M5 | ON the Mac, the iPhone or the iPad, THE GAME SHALL play with an Xbox, PlayStation or MFi controller through Apple's GameController framework, every button mapped, and SHALL pick up a controller connected or disconnected mid-game; on the iPhone or iPad the touch controls SHALL be hidden while a controller is connected (owner ruling 2026-11-17: "The ipad os and mac versions are obviously going to need to support an external controller. Make sure that's in the road map"; confirmed: "Yes, 1.0, Mac and iPad (Recommended)") | A session on the island with a controller on the Mac and on the iPad; a controller disconnected mid-game and connected again takes over without a restart | on the devices |
| M6 | ON EVERY DEVICE, THE LAUNCHER SHALL pull my island through save-relay before the game starts and push it after the game quits, syncing the whole `user/device_save/` folder (`main.dat`, `mainHeader.dat`, `Villager0`-`Villager7/` and the other files the game writes there) and never `user/backups/`, `config.ini` or `launcher.ini`; the same island SHALL load on all six | An island saved on one device loads on each of the others | save-relay manifest tests; the round trip on all devices |
| M7 | WHEN I update the launcher or the build, THE LAUNCHER SHALL back up my island first (`user/backups/<YYYYmmdd-HHMMSS>/`, the 10 newest kept), rebuild, and keep it | The island is intact after an update and the backup is listed | core test with a fake build; on one device |
| M8 | EVERY DAY, upstream's `main` SHALL be merged into ours (R21), a conflict becoming an `upstream-sync` issue taken first; and a fix of ours that any player hits SHALL be offered upstream at my word (R36), listed in the launcher's About page until upstream has it | `main` is never more than a day behind upstream; each offered fix has a link | the sync log; the About list |
| M9 | ON an Android device with an Adreno GPU, THE LAUNCHER (the Android build of the same app, in the same APK as the player) SHALL install the game library and data pulled from the Strix Halo tablet, and the game SHALL play with a controller | A session on the island with a controller, no crash | on the device |
| M10 | WHEN I open the launcher on any device, IT SHALL show one controller-first Continue screen (full screen in Game Mode, a window on desktop, mouse works too): my island's card (the save's file time, the device it was played on, its play time, the game's icon; the island's name after 1.0), with one button to sync and play; first run SHALL be a guided setup driven by the controller; Saves, Settings, Fleet and About SHALL sit one button away; a sync conflict SHALL show both islands side by side and I pick one | From Game Mode, A starts the game in one press; a forced conflict shows two cards and keeps the one I pick | view-model tests with fake saves; on each handheld |
| M11 | WHEN a device without the game folder starts its launcher at home, IT SHALL find a hub on the home network, pair once with a code shown on both screens, and pull its build (player, game library, `romfs.pak`, `spirv.pak`, fonts, mods) over the home network only; a fleet view SHALL show each paired device's version, build, last sync and free space, and SHALL warn before I play on a device whose version or build differs from the one that made the latest save | The AMD handheld installs from the Strix Halo tablet; a device on another version shows the warning | tests with a fake hub on loopback; on the devices |
| M12 | ON the AMD handheld, THE GAME SHALL "play": (a) a fresh island from the first boot through the end of day one (the first save in the tent), then quit and reload it; (b) with my own developed island (imported from a JKSV backup), 7 in-game days in a row (real days, or `NX_TIME_OFFSET` steps of a day), each day visiting the residents, Nook's Cranny (buy and sell), the Able Sisters, the museum (catch and donate, then walk its wings), Resident Services and the airport; (c) the Saturday K.K. Slider concert at or after 18:00; (d) save, quit, restart and load at the end; with no crash or hang anywhere, at the profile's frame rate (30, or 60 on 3.0.2 and 3.0.3 with native 60) | A play log per part (`playlog.jsonl`: part, day, place, result) with every line "ok" | on the device |
| M13 | OUR CODE SHALL be Rust: the launcher, the prep tools and the host runtime ship as Rust, and no Python or C++ of ours ships in 1.0; the recompiled game code, the NVN renderer (`runtime/src/nvn`, `vk_*`, `mtl/`), the in-game settings overlay (`runtime/src/ui`) and the Android Java shell stay in their languages, behind a C ABI. Debug scripts that never ship (`tools/find_*.py`, `capture*.ps1` and the like) may stay | `git ls-files launcher tools/*.py recomp` holds no shipped source; the game builds and boots at every step of the move | CI; the boot smoke test per step |
| M14 | NO GAME FILES, EVER: nothing from a game folder, a dump, keys, firmware, the recompiled code (`gen/`), extracted shaders or game-binary analysis is in the repo, a release, a log or an issue; `tools/check_no_game_data.sh` stays the pre-commit hook and runs in CI | The check is green on every PR; the release file list holds none | CI; review of every PR |
| M15 | EVERY BUILD SHALL be verified: `cargo fmt`, `cargo clippy -D warnings` and `cargo test` pass on Linux, Windows and macOS for each merge to `main`, the C++ unit tests (`NX_TESTS=ON`) pass on Linux, and the Android (`aarch64-linux-android`) and Apple (`aarch64-apple-ios`) targets cross-build | CI green | CI |
| M16 | WHEN I prepare or play, THE LAUNCHER SHALL leave both fan mods off, the pt-BR fan translation and tmouh's 60 FPS SpeedFix, unless I turn each on with its own switch in Settings (my ruling, 2026-11-02: "Make them optional") | A fresh install has no `ptbr` mod and no SpeedFix build; each switch turns its mod on at the next prepare, and off again | core tests with fake mod zips; one prepare on a hub |
| M17 | ON EVERY DEVICE, THE GAME SHALL adapt to the aspect ratio of the screen it runs on, taller as well as wider than 16:9, live when the window changes or a folding phone folds (my ruling, 2026-11-12: "All our games should have the ability to adjust to the aspect ratio of the machine they're running on"): the render size from the display, the HUD at the safe edges, the field of view Hor+ on wide screens and Vert+ on tall ones, video letterboxed, pillarbox only as the fallback (versions without upstream's hooks: see the Won'ts). The four screens: 6:5 (Pixel Fold unfolded, 2208x1840), 3:2 (iPad mini, 2266x1488), 16:9 (1080p projector), 16:10 (ROG Flow Z13, 2560x1600) | A capture at each of the four aspects with the HUD inside the safe area; a resize without a restart | aspect-math unit test; captures with simulated window sizes; on the devices (`needs-owner-device`); feature #91 |
| M18 | ON EVERY DEVICE, THE FINAL BUILD SHALL be one contained app with the game inside, made from my own game folder (my answers, 2026-11-18, in the Mac satellite session's question tool, asked whether "the game inside" covers every platform or only the Apple apps: "Every platform"): on the Windows desktop, one folder with the launcher `.exe`, the player and the game side by side, moved or deleted as one unit, with no installer ("One folder (Recommended)"); on Linux and SteamOS, one folder in the same way, not one AppImage ("One folder (Recommended)"); on those PCs and handhelds the folder SHALL be built on each device by its own launcher from my game folder, never uploaded or shared ("Built on each device (Recommended)"); on Android, where an APK cannot hold the game, the one app SHALL copy the game into its own private storage at first start, with nothing outside the app ("App's private storage (Recommended)"); the Mac app and the iPhone/iPad app SHALL go only to my own Dropbox. The game SHALL never go into this repo, a release, a PR, CI, a log, a public place or a server of ours. Approved 2026-11-18, in the same session, as a changed spec: "Approve as described (Recommended)" | On each device the game plays from its one folder or app alone; a Windows or Linux folder still plays after it is moved; on Android nothing of the game sits outside the app; the release file list and CI hold no game data | core tests with a fake game folder; `tools/check_no_game_data.sh` and the release file list in CI; on the devices (`needs-owner-device`); steps #190 (Windows), #191 (Linux, SteamOS), #192 (Android), #171 and #172 (Mac, iPhone/iPad) |

## Won'ts (1.0)

- Any game data, keys or firmware in this repo, its releases or its logs, ever. The bundle holds no game data either: the hub copies a build made from my game folder only to my own devices, over the home network. Only the final build of M18 holds the game, inside its one app, and only on my own devices or, for the Mac and iPhone/iPad apps, in my own Dropbox (approved 2026-11-18 with M18).
- Decrypting an NSP or XCI, or handling keys: the input is a prepared game folder (my own dump's `exefs/` and `romfs/`, or one made by ACNHRecomp-Importer), as upstream has it.
- Versions other than 3.0.2, 3.0.3, 2.0.7 and 1.0.0 (upstream builds any; we refuse the rest). Native 60 FPS and ultrawide only where upstream has them (3.0.2, 3.0.3).
- Online play: Nintendo's servers, Nook Link, the Dream Suite, online visits. Builds and islands move between my devices over the home network and save-relay only, never through a server of ours.
- Local wireless play over the LAN (`docs/LOCAL_PLAY.md`) at 1.0: kept as upstream has it, untested by us, after 1.0.
- Save editing (NHSE-style changes to the island) and design-pattern sync: after 1.0 at the earliest.
- Steam Frame or other devices; Android GPUs other than Adreno (Mali is after 1.0).
- Rewriting the renderer, the recompiled game code or the in-game overlay in Rust.
- Above 60 FPS as a target (upstream's frame generation stays as it is).

## Prior art

Approved 2026-11-02 with this spec. The first 17 are this game's peers; the last 7 are shared by every -universal repo.

peer: Nycolazs/ACNHRecomp | MIT | the upstream this repo was copied from; the `upstream` remote points here; credit for everything we take
peer: peiikai/LASRecomp | MIT (GitHub reads NOASSERTION; LICENSE is MIT text) | the most mature Switch recomp on macOS, Windows and Linux: how to split per-game code from shared code
peer: BluetufieBugado/SuperMarioWonderRecomp | MIT | surviving copy of our framework's upstream (BelmanteGu's is gone): NVN on Vulkan and MoltenVK, macOS/Android/Windows builds
peer: fedebuonco/nxrt | Unlicense | a shared runtime for Switch recomps (loader, kernel, HLE services, NVN renderers): closest to our one-runtime plan
peer: fedebuonco/nxrt-frontend | Unlicense | launcher/manager for nxrt recomps: prior art for the Continue screen
peer: cesar527/ACNHRecomp | MIT | upstream fork with an Android thermal governor: handheld and phone power modes
peer: ZUlxian/ACNHRecomp | MIT | rewritten fork: plain game-folder import, Windows port, crash and 16:10 fixes
peer: bingnnvjn/nvn-native-android | MIT | architecture notes for Switch games native on Android (NSO rewriting, Horizon HLE, NVN to Vulkan)
peer: Timqt/d2r-android | MIT | hand-written NVN to Vulkan layer tested on Adreno 650 and Mali: mobile GPU workarounds
peer: kwsch/NHSE | GPL-3.0 | the standard ACNH save editor: save format, encryption, hashes for save-relay (ideas only)
peer: citron-neo/emulator | GPL-3.0 | active yuzu successor: HLE service and NVN/Maxwell reference (ideas only)
peer: pound-emu/pound | GPL-3.0 | new Switch emulator: readable service and kernel code (ideas only)
peer: kaiyao7/SwitchRecomp | GPL-3.0 | alternative Switch static-recomp design (decoder, typed IR, C codegen) (ideas only)
peer: bgyss/SwitchRecomp | no licence file | early Switch static-recomp research listed in upstream's references (ideas only)
peer: skyline-emu/skyline | no licence file | Android Switch emulator: surfaces, input, GPU drivers (ideas only)
peer: FluffyFishGames/ACNHDesignPatternEditor | GPL-3.0 | ACNH design-pattern data, a possible extra to sync (ideas only)
peer: Norviah/animal-crossing | MIT | JSON database of ACNH items and villagers: Continue-screen metadata
peer: FEX-Emu/FEX | MIT | x86-64 on ARM64; not needed here (the game code is recompiled to each CPU), kept for the shared Android notes
peer: K11MCH1/libadrenotools | BSD-2-Clause | loads Turnip on Android
peer: slint-ui/slint | royalty-free licence (attribution) | the launcher UI
peer: gilrs (crate) | MIT or Apache-2.0 | gamepad input on desktop
peer: iannitram/save-relay | MIT | all save sync
peer: iannitram/totk-universal | MIT | sibling: the same launcher, hubs, fleet, Steam and save-card modules in Rust, copied with credit
peer: iannitram/Uncharted-Universal | GPL-2.0 | sibling copy under the same plan: the runtime-to-Rust strangler order and the stage layout (ideas only; its code is not copied, since this repo is MIT)

## Problem

I want to pick up any of my devices and keep playing my island, but today each device prepares its own build from the game folder through a Python script and a C++ launcher, there is no iPhone or iPad build, and the island stays on the device that saved it.

## Solution

One Rust engine with a Slint launcher on every device, opening on the Continue screen. Three home build hubs hold the game folder and build each device's install; the other devices pull it over the home network. save-relay wraps every start. The prep tools, then the host runtime, move from Python and C++ to Rust module by module, so the game keeps building and booting at every step. Android keeps upstream's route (the runtime in the APK, the game library in the game folder, Turnip on Adreno); Apple builds the same recompiled code for the Mac and for iOS, on upstream's Metal renderer.

## User stories

1. As the owner, I want to pick up any of my devices and continue the same island, so that I never copy saves by hand.
2. As the owner, I want to start the game from Game Mode on my handhelds, so that I never need the desktop.
3. As the owner, I want to play on my iPhone, iPad or Android device with a controller, so that I can play away from home.
4. As the owner, I want one button on any device to put me back on my island.
5. As the owner, I want my game folder on as few machines as possible, so that a lost handheld or phone holds only a build.
6. As a contributor, I want one language and one build (`cargo`) for everything of ours, so that the code is easy to change and test on every OS.

## Implementation decisions

- **Workspace** (ADR 0001): `crates/core` (`acnh_core`: game folder check, version gate, NSO header, prepare, config files, saves, save cards, backups, update, profiles, recipes and hubs, Steam, artwork, bundles, fleet), `crates/recomp` (`acnh_recomp`: the AArch64 to C++ recompiler that replaces `recomp/`), `crates/runtime` (`acnh_runtime`: the host runtime that replaces `runtime/src` outside the renderer), `crates/cli` (`acnh-universal`), `app/` (`acnh-app`, Rust + Slint). The renderer, `games/acnh/` and the overlay stay C++ and CMake, built from `crates/runtime`'s build script, behind the existing C ABI (`runtime/include/nx/game.h`, `nx_game_abi()`).
- **Moving the runtime** (ADR 0002): strangler order, last. `crates/runtime` builds as a static library linked into the player; each `hle_*`, `svc_*`, `platform_*` or kernel file is replaced by a Rust module exporting the same symbols; the loader and `main.cpp` move last. A boot smoke test (the player reaches its first frame on a fake game library) gates each step, plus a device check on the AMD handheld. The recompiler moves before the runtime: its output must match `recomp/`'s byte for byte on a fake NSO (`tests/difftest.py` stays the oracle).
- **Launcher** (ADR 0003): all Rust with a Slint UI, as totk-universal's ADR 0004: view models in plain Rust, `.slint` only draws; gamepad through gilrs on desktop and key events on Android. `launcher/` (SDL3 + ImGui) stays until the Slint app has parity, then goes. The generic modules (vdf, Steam shortcuts, platform folders, hub, bundle, fleet, save card shape) are copied from totk-universal (MIT) with credit.
- **Build hubs** (ADR 0004): the Strix Halo tablet (Linux handhelds and Android: the NDK build runs on Linux), the RTX 3090 Ti desktop (Windows), the Mac (Mac, iPhone, iPad). mDNS discovery, a one-time pairing code (SPAKE2), the stream sealed, home network only. The hub runs the stages (version, recompile, shaders, fonts, build) and copies the result to my own devices; a device never holds the game folder.
- **Android** (ADR 0005): upstream's route is our base: the APK carries the runtime only (`lib<game>.so`, `Home.java`), the game library and data go into the game folder on the device; Turnip on Adreno (`tools/build_turnip.sh`). Our Slint launcher is built into the same APK and replaces `Home.java`'s folder screen; it pulls the game library and data from the hub instead of `adb push`.
- **Apple** (ADR 0006): the same recompiled C++ compiled for `aarch64-apple-darwin` and `aarch64-apple-ios`, no JIT needed. The renderer is MoltenVK today and upstream's native Metal (`runtime/src/mtl`, `docs/NATIVE_METAL.md`) when it lands. The Mac launcher builds and installs the iOS app over a cable with Xcode signing. Risks, checked first in stage 8: guest pointers are host pointers, so the runtime reserves the guest's whole 39-bit address space at a fixed host address (`runtime/include/nx/runtime.h`: `0xC0_0800_0000` up on Apple, a heap of up to 6 GiB), which on iOS needs the extended virtual addressing entitlement and may not fit an iPhone's limits; a free Apple ID re-signs every 7 days and limits memory; the paid Apple Developer Program lifts both.
- **Saves:** a save-relay manifest for ACNH naming `<data root>/user/device_save/` per device; the layout is the same on every device, so the island moves unchanged. One island per console, as the game has it. All devices should run the same version (the game refuses a save from a newer version); the fleet view warns (M11).
- **Windows:** the Visual Studio build tools, clang-cl and lld that `tools/setup_acnh.py` fetches today, for the C++ parts; the Rust target is `x86_64-pc-windows-msvc`.

## Testing decisions

- Core logic (game folder check, version gate, NSO header, config files, saves, backups, Steam files) is tested with fake game folders in `cargo test` on Linux, Windows and macOS. No test reads a real game file.
- The recompiler port is tested against `recomp/`'s output on hand-written AArch64 snippets (never game code), and by `tests/difftest.py` (Unicorn) as today.
- Each runtime module's Rust tests replace or join the C++ test for it; the boot smoke test runs in CI on Linux.
- Launcher pages are tested through their view models (button presses in, page state out), and once headless through Slint's testing backend.
- Hub pairing, transfer and the fleet view are tested with a fake hub on loopback.
- Everything that runs the game on hardware is a device check (`needs-owner-device`).

## Platforms

| | Linux / SteamOS | Windows | macOS | iOS / iPadOS | Android |
|---|---|---|---|---|---|
| 1.0 device | AMD handheld, Strix Halo tablet | RTX 3090 Ti desktop | a Mac with Apple silicon + controller | iPhone or iPad + controller | an Adreno device + controller |
| Game code runs | recompiled, x86-64 | recompiled, x86-64 | recompiled, ARM64 | recompiled, ARM64 | recompiled, ARM64 |
| Renderer | NVN on Vulkan | NVN on Vulkan | MoltenVK, then Metal | Metal (or MoltenVK) | NVN on Vulkan (Turnip) |
| Hub | the Strix Halo tablet | the desktop | the Mac | the Mac | the Strix Halo tablet |
| Launcher | Slint app (AppImage) | Slint app | Slint app | none: the game app | Slint app, same APK |
