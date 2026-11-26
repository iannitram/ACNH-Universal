# Roadmap

Updated: 2026-11-25

features:
- housekeeping: Build verification, release hygiene and upstream sync
- engine-core: Game preparation, launch, backups and save-relay adapters
- faces: Command line and controller-first Slint launcher
- recompiler: Rust AArch64 loader, analysis, C++ emitter and driver
- runtime: Rust host runtime behind the game ABI
- hubs: Discovery, paired transfer and fleet records on the home network
- steamos: Contained local builds and Game Mode on Linux and SteamOS
- windows: Contained local builds and controller play on Windows
- apple: Contained Mac and iOS builds, SideStore, controllers and saves
- android: Adreno build, private game storage, controller and save support
- one-island: The same island across all six devices
- design-pass: Final launcher and Game Mode design review
- resolution: Responsive rendering, HUD and safe areas across displays

From `SPEC.md` (approved 2026-11-02; M5 widened and approved again 2026-11-17; M4 and the Dropbox exception changed and approved again 2026-11-18; M18 added and approved 2026-11-18; M18, M4 and the Dropbox exception changed 2026-11-23 for an in-app download from my own Dropbox, and the SPEC as changed in PR #232 approved again the same day: "Approve, keep big .ipa") to 1.0: *Animal Crossing: New Horizons* 3.0.2, 3.0.3, 2.0.7 or 1.0.0, built from my own game folder, on the AMD handheld, the Strix Halo tablet, the RTX 3090 Ti desktop, a Mac, an iPhone or iPad, and an Adreno Android device; a week of island life on the AMD handheld (M12); the same island on each through save-relay; and our code in Rust. The code map is `docs/code-map.md`, the words are `GLOSSARY.md`, the decisions are `docs/adr/` (0001 workspace, 0002 prep tools then recompiler then runtime to Rust, 0003 Slint launcher, 0004 build hubs, 0005 Android keeps upstream's route, 0006 Apple builds the same recompiled code), the upstream and peers are in `PEERS.md`. Every piece is an issue titled `<id>: ...`; `Blocked by` in each issue sets the order.

## Where things stand

This roadmap was audited against `main` on 2026-11-25 by reading the code (eight readers, then a second, independent read of every finding; nothing was built or run for it). The earlier text said the 1.0 code was complete on every platform and only device runs remained. That was too strong. What is true:

- **Built and merged:** the engine and Prepare in Rust, the launcher and the command line, the Rust recompiler (cut over, Python `recomp/` deleted), the hub library, the contained app for every platform, the Android and iPhone/iPad in-app download, save sync on every platform, resolution independence. The list with issue and PR numbers is in [Merged pieces](#merged-pieces).
- **Not what the roadmap said:**
  - **Runtime (stage 4).** The 11 groups have Rust cores, but 8,200 lines of non-renderer C++ (the network, LDN and `nextendo` services, save storage, amiibo) and the seven platform hosts (5,700 lines: window, input, audio, `main`) are still C++ and still compiled. M13 names neither as staying. Mine to rule on (#352).
  - **Hubs (stage 5).** The library is built; the product is not. Nothing publishes a build for `hub serve`, the launcher cannot be a hub, and a PC that pulls from a hub gets the player with no game data and can never finish first run (#357, #358). The AMD handheld's run (#68) cannot start until these are fixed.
  - **Android (stage 9).** Since PR #304 the app icon opens the Slint launcher, which cannot get or start the game (#363); Prepare's `android` target cannot build the game library it prepared (#365). Every device check on record was made before #304 merged.
  - **One island (stage 10).** The iPad can sync saves only through Nextcloud and the Android app only through Dropbox, so no provider reaches all six devices (#361).
  - **CI and release (stage 0).** `cpp-windows` has never passed since it was added, so the Windows player has never been built in CI (#322). `release.yml` has never run and cannot (#323). There is no version scheme of our own (#324).
  - **Python (M13).** Prepare still runs Python in five places (#335).
  - **SPEC.** The desktop cloud download (PRs #315, #316) is not in `SPEC.md` and contradicts M18 and M11 as written (#321).
- **Checked on a device so far:** the Pixel Fold (Mali-G710, not the 1.0 Adreno device) ran the game, made its first save, signed in to Dropbox and pulled an island, and ran the 60 fps SpeedFix build (#264; outdoors it holds 29–32 fps cold and 21–24 hot, so 30 fps with frame generation is its practical setting). The Mac booted an early build to the opening at 30 fps (#73, 2026-11-18). The iPad mini Simulator reaches frames at about 45 fps, then stops at a Metal limit only the Simulator has. Build 0.11.0 (1101021) of my prepared game is in my Dropbox as a Mac app, a big `.ipa` and a small `.ipa` with its game folder; none is installed yet.

## Decisions I owe

Each of these stops work until I answer. The issue holds the detail.

| Decision | Why it is needed | Issue |
|---|---|---|
| May a PC download its prepared game from a cloud folder, from which services, and must the folder be encrypted? | The code does it (Dropbox, Google Drive, Nextcloud; not encrypted); SPEC M18 and M11 say no; my comment on #68 (2026-11-18) says Dropbox, encrypted | #321 |
| Does the C++ left in `runtime/src` get ported for 1.0, or does M13 name it as staying? | Stage 4 was recorded as complete and is not | #352 |
| Do hubs serve game data to PCs (M11 as written), or only the player? | First run cannot finish on a pull-only PC | #358 |
| Which Adreno device is the 1.0 Android device, or does M9 change? | Every Android result is from the Mali Pixel Fold | #370 |
| Where does the one Android signing key live? | Each checkout signs with its own; an APK from another machine cannot update the app | #366 |
| How does the SpeedFix patch (CC BY-NC 4.0) ship, and what must `NOTICE` carry? | It is tracked in this public MIT repo and built into my apps | #329 |
| Our version and tag scheme, and who cuts 1.0 | The workspace version is `0.0.0`; builds read upstream's tags | #324, #370 |
| Do upstream's online services (`nextendo`, `hle_ssl`, `hle_hostnet`) stay on in our builds? | Online play is a Won't for 1.0; they came in with the sync | #352 |
| One recompiler or two after 1.0 | The on-phone Prepare still uses upstream's C++ recompiler | #351 |

## What to do next

Work is in three bands. Inside a band, anything not blocked can be taken.

### Band 1: make `main` buildable and releasable

| Issue | Piece | What |
|---|---|---|
| #322 | 0.04 | `cpp-windows` green (raise the LLVM pin); expect the never-run test and player steps to find more |
| #323 | 0.05 | `release.yml` can package the source and run |
| #324 | 0.06 | One version scheme, the same on every clone |
| #325 | 0.07 | CI builds the tests and the Android launcher it skips; the emitter oracle can fail |
| #326 | 0.08 | The game-data check's gaps (M14) |
| #328 | 0.10 | Pin the Rust toolchain, save-relay's revision, actions and downloads |
| #327 | 0.09 | Delete dead scripts, headers and workflow |

### Band 2: fix what blocks each device run

| Device run | Fix first |
|---|---|
| 6.01 (#67), the Strix Halo tablet as the Linux hub | #332 (a second Prepare loses its tools), #334 (silent skips, toolchain checks), #331 (a changed recompiler is not noticed), #333 (truncated packs), #341 (Add to Steam), #357 (publish and serve a build); the launcher to install comes from #323 or a checkout |
| 6.02 (#68), the AMD handheld pulls from the tablet | #357, #358 (game data for a pull-only PC), #340 (the island during an install), #359 (`hub serve`), #360 (the fleet warning) |
| 6.03 (#69), the week on the island (M12) | #68; the play-log template and the JKSV import steps (#370) |
| 7.02 (#71), the RTX 3090 Ti desktop | #322, #332, #345 (first run and the folder picker on Windows) |
| 8.02 (#73), the Mac app | #362 (packaging; how to open an ad-hoc-signed app) |
| 8.03 (#74) and 8.03f (#231), the iPad | #355 (the small `.ipa` with no game) |
| 8.05 (#76), 8.08 (#121), Mac and iPad save sync | #361 (a provider both the iPad and Android can use), #338 (device names), #323 and #328 (a release with save-relay pinned to a tag) |
| 9.04 (#82), the Android device | #363 (the front door), #365 (Prepare's `android` target), #366 (signing, libvpx, the APK), #367 (the in-app download), #368 (the manifest on Linux), #364 (`Home` takes a folder from any app), #354 (the final push, sound), #369 (build scripts) |
| 10.01 (#83), one island on every device | #361, #338, #337 (backups), #360, every run above |
| 11.01 (#84), the design pass | #348 (the brief against the built screens), #347 (one rule for button prompts), #342, #344 |

### Band 3: M13 and cleanup, alongside

| Issue | Piece | What |
|---|---|---|
| #335 | 1.37 | Python out of Prepare |
| #352 | 4.13 | The C++ left in the runtime, after my ruling |
| #336 | 1.38 | Split `prepare.rs`, remove dead code and duplicates in `crates/core` |
| #356 | 4.17 | Runtime: mirrored prototypes, three SDL hosts, giant files, `// SAFETY:` lines |
| #348 | 2.17 | Launcher and CLI: one build path, shared relay and Steam code, dead view models |
| #350 | 3.07 | Recompiler hardening, dead code, speed |
| #349 | 3.06 | NEON on 64-bit lanes |
| #353 | 4.14 | The IPC reply clamp |
| #343, #344, #342 | 2.12, 2.13, 2.11 | CLI exit codes; Continue's stuck states; closing the window during play |
| #339, #346 | 1.41, 2.15 | The cloud download's faults, after #321 |
| #329, #330 | 0.11, 0.12 | Licences and `NOTICE`; the other stale docs and the issue labels |
| #370 | 10.02 | Acceptance in SPEC that no issue asks for |

## Features, by Must

"Built" is code merged in `main`. "Open" is what stands between that and the SPEC's evidence.

| Must | Built | Open |
|---|---|---|
| M1 build from my game folder, four versions only | 1.02–1.04, 1.07, 1.22, 1.22a, 1.22b, 1.27–1.31, 2.01, 2.05 | #331, #332, #333, #334; a real build per hub: #67, #73, and the Windows desktop (no issue: #370); only 3.0.3 has been built |
| M2 Steam with artwork, the handheld profile | 1.05, 1.06, 1.11–1.15, 1.23, 2.04d, 2.05 | #341; #67, #68 |
| M3 the Windows desktop | 7.01a (#190) | #322 (CI has never built it), #345; #71 |
| M4 Mac app, iOS app as a Dropbox `.ipa` installed with SideStore | 8.01, 8.02a, 8.02b, 8.03a–8.03g, 8.05a | #362, #355; #73, #74, #231, #77 |
| M5 Mac, iPhone and iPad with a controller | 2.04b, `input.layout = auto` (#251 / PR #252), iOS touch hidden with a controller (PR #293), Apple glyphs (PR #292), Android pad prompts (#173) | #347; #75, #124, #78 |
| M6 save-relay before and after every game | 1.08, 1.24, 1.26, 1.26a, 1.26b, 2.04a, 2.05a, 2.06a, 8.05a, 9.03 (#81) | #361, #338, #342, #343, #344, #354; #76, #121, #83 |
| M7 updates back the island up | 1.10, 1.25 | #337 (an update can delete the game's own backups), #340; no device evidence asked for (#370) |
| M8 daily upstream sync, fixes offered upstream | the sync is process (R21), by hand; last one PR #246 (#145, 140 commits, upstream's newest dated 2026-11-17); About's list (2.08) is empty | no sync log, no fix offered yet (#370) |
| M9 Android on Adreno | 9.01, 9.01a, 9.02, 9.02a–9.02c, 9.03 | #363, #364, #365, #366, #367, #368, #354; no Adreno device named (#370); #82 |
| M10 Continue screen | 1.09, 2.02, 2.02a, 2.04, 2.04a–2.04d, 2.05, 2.05a, 2.06, 2.06a | #348, #344, #345; the conflict cards on a handheld (#370); #84 |
| M11 pull from a hub, fleet | 1.17–1.21, 5.01–5.05 (the library), 2.07, 9.02 | #357, #358, #359, #360, #340; #68 |
| M12 a week on the island | | #69 (needs my developed island as a JKSV backup); the play log has no template (#370) |
| M13 our code in Rust | the prep tools (1.31, #34), the recompiler (3.05, #49), the launcher (2.10, #44), upstream's packaging retired (2.09a, #174), the Rust cores of 11 runtime groups (#51–#61) | #352 (C++ left in the runtime), #335 (Python in Prepare), #351 (two recompilers, after 1.0) |
| M14 no game files | `tools/check_no_game_data.sh` in CI, 1.16 `redact`, 1.17 `bundle::is_forbidden`, the package scripts' refusals | #326; the release file-list check has never run (#323); #321 |
| M15 every build verified | 0.01, 0.02; CI on Linux, Windows (Rust), macOS | #322, #325; no CI builds the APK, the Mac player or Android's C++ |
| M16 fan mods off by default, a switch each | 1.32 (#85); my own builds turn the SpeedFix on (decision of 2026-11-23) | each switch on and off at a Prepare on a hub (#370); #329 |
| M17 every aspect, wide or tall | #91 / PR #299 (`nvn_aspect`), the Pixel Fold's two screens (#249 / PRs #248, #250), checked on an emulator | captures on devices and a live resize (#370, #84); the launcher has no narrow layout (#348) |
| M18 one app with the game inside, on every platform | Mac and iPad (8.02a, 8.02b, 1.22b, 8.05a, 8.03e), Windows (7.01a), Linux (6.01a), Android (9.02a, 9.02, 9.02b), the small `.ipa` (8.03f, 8.03g) | #363, #365, #367; "move the folder and launch again" is asked by no issue (#370); #231, #74 |

## Decisions

- owner ruling 2026-11-17: "Fold the save relay into the Mac app via a step in the roadmap. Should be a 1.0 that exists in every universal app with and ios and Mac component. Revise all roadmaps if you need to." Here: 8.08, save-relay in the released Mac app, in stage 8 (1.0).
- owner ruling 2026-11-17: "The save relay Mac and iOS functions need to be brought into 1.0" and "Revise the roadmap" (confirmed: "Yes, all into 1.0 (Recommended)"). This replaces the earlier "Ships with iOS app" placement. Here nothing moves: the Mac app (2.09, 8.02), the iPhone/iPad app (8.03), save-relay in the iOS app (8.05) and in the released Mac app (8.08), and the round trip (10.01) were already 1.0.
- owner ruling 2026-11-17: "The ipad os and mac versions are obviously going to need to support an external controller. Make sure that's in the road map" (confirmed: "Yes, 1.0, Mac and iPad (Recommended)"). Here: 8.04 (iPhone and iPad) and 8.09 (the Mac), both 1.0, and the round trip (10.01) waits on both; M5 in `SPEC.md` now covers the Mac and a controller connected or disconnected mid-game, so the spec needed my approval again: approved 2026-11-17 ("Everything is approved").
- owner rulings 2026-11-18, in the Mac satellite session: "Let's build the whole launcher with the relay into it now. That is all 1.0"; "Minimum launcher 1.0 steps being detected button overlays, full controller support, and save relay built and incorporated should be the same for every platform of every universal app"; "Controller support and the save relay are in the build? Also detectable onscreen button prompts for any controller". Here: the launcher's save-relay, controller and button-prompt pieces are all 1.0, on every platform. The relay: 1.26a, 1.26b, 2.04a, 2.05a, 2.06a, and 8.05a on the iPad. Controllers and prompts for the pad in hand (any pad, a generic set for one the launcher cannot name): 2.02a, 2.04b, 1.30a (the game's own icons), 9.01a on Android, 8.05a on the iPad. What a hub needs to build and serve from the installed launcher: 1.22a, 1.22b, 1.29a, 2.04c, 2.04d, 2.09a.
- owner ruling 2026-11-18: "Also, can the final build be one contained app?" (yes: one app per Apple platform). Here: on the Mac, `ACNH-Universal.app` holds the launcher and the player (1.22b, 8.02a); on the iPad, the `.ipa` is the one app (8.03e, 8.05a). Every platform (Linux, Windows, Mac, Android, iOS) gets the game inside one contained app (owner ruling 2026-11-18); Android is one APK (9.01).
- owner rulings 2026-11-18, in the question tool: asked whether my prepared game folder should sit beside the app or inside it, "Game inside the app"; then, typed: "The game isn't going in the repo. Just the app the repo app creates". Here: each final Apple build holds the game prepared from my own game folder, read-only inside the app (8.02b reads it there; 8.02a and 8.05a put it there). The launcher's Prepare builds it on my Mac from my own game folder and puts it only in my own Dropbox. The game never goes into this repo, a release, a PR, CI, a log, a public place or a server of ours. `SPEC.md` (Won'ts) changes in PR #144, so the spec waits for my approval again.
- owner ruling 2026-11-18, in the question tool, asked about the Mac's data root and the packaged Mac app's folder: "Yes, those two (Recommended)". Here: on the Mac, saves, settings and logs live in `~/Library/Application Support/ACNH-Universal`, and the app goes to `Dropbox/_transfers/ACNH-Universal/mac/` (8.02a), beside the `.ipa`'s `ios/`. On the iPad, the app's Documents folder is the data root (8.03a).
- owner ruling 2026-11-18, in the question tool, asked how the save relay should be built into the iPad app: "Rust API (Recommended)". Here: 8.05 goes through save-relay's Rust API in the player (`acnh_core::relay`, as on the Mac and PC), with no C interface and no Swift bindings; 8.05a builds it, and ADR 0006 changes to match.
- owner ruling 2026-11-18, in the question tool: "Yes, file them now (Recommended)". Here: the steps above are filed as issues #156 to #174, and their blockers are added to the existing issues.
- owner ruling 2026-11-18, typed, on what tests the Apple build: "I think the real test here will be booting the mac version. Saving (using the relay.) Loading the game on the ipad and loading the save from the mac". Here: 8.05 (#76) is my acceptance test for the Apple build, four checks on my own devices in this order: the Mac app boots the game (8.02, #73); I save on the Mac and the island goes up through save-relay when the game quits (2.04a, #164); the iPad app boots the game (8.03, #74); the iPad loads the island saved on the Mac (8.05a, #172). 8.06 leaves 8.05's blockers, and 8.05 no longer asks for save-relay's release tag, because 8.08 waits on 8.05. The way back, an island saved on the iPad loading on the Mac, stays with 8.08.
- owner ruling 2026-11-18: "To test install on my iPad I'll need a file on Dropbox. I don't have direct access to the Mac so we'll need a different side load process" (confirmed: "SideStore on the iPad (Recommended)", "Free Apple ID for now (Recommended)", "All universal apps (Recommended)"). This replaces the install over a cable. Here: 8.03, the Mac builds an unsigned `.ipa` (`iphoneos`, arm64) of the iPhone/iPad app and puts it in `Dropbox/_transfers/ACNH-Universal/ios/` with its version in the file name (the Mac mini's Dropbox does not sync the Dropbox "Apps" folder; the first test `.ipa` is already there), and I install it from Dropbox with SideStore on the iPad, which signs it with my free Apple ID, re-signed every 7 days on the device; 8.04 and 8.05 now name that app instead of the one installed by cable. The 8.01 finding (`docs/apple-ios.md`) stands: a free Apple ID re-signs every 7 days and gets neither the increased-memory-limit nor the extended-virtual-addressing entitlement; the paid program stays a later upgrade. M4 in `SPEC.md` changes to match, so the spec needed my approval again: approved 2026-11-18 ("Re-approve both (Recommended)").
- owner ruling 2026-11-18: "Final deliverables getting to me via Dropbox are really the only way in this case so exception cleared". Here: the final builds (the iPad `.ipa` and the Mac app) go to my own Dropbox under `_transfers/ACNH-Universal/`, an exception to the home-network-only wording of M1, M11 and the Won'ts; game data still never goes into a repo, a public place or a server of ours. `SPEC.md` (Won'ts) records it; the changed Won't was approved with M4 on 2026-11-18 ("Re-approve both (Recommended)"). Since 2026-11-23 it also covers the prepared game's files for the in-app download (M18; "Approve, keep big .ipa").
- owner's answers 2026-11-18, between about 10:00 and 10:35 MST, in the Mac satellite session's question tool: "the game inside" covers every platform, not only the Apple apps ("Every platform"); on the PCs and handhelds the one app is built on each device by its own launcher from my game folder, never uploaded or shared ("Built on each device (Recommended)"); on Windows, and on Linux and SteamOS, it is one folder, with no installer and no AppImage ("One folder (Recommended)", both); on Android the one app copies the game into its own private storage at first start, with nothing outside the app ("App's private storage (Recommended)"). Here: 7.01a (#190, Windows), 6.01a (#191, Linux and SteamOS) and 9.02a (#192, Android) filed, and 9.02 (#80) now pulls into the app's private storage; the Apple one-app steps (8.02b #157, 1.22b #162, 8.02a #171, 8.05a #172, 8.03e #170) already put the game inside. M18 in `SPEC.md` says this, so the spec needed my approval again: approved 2026-11-18 ("Approve as described (Recommended)"; PR #193).
- owner's question 2026-11-23, in the Mac session: "Is it possible to design the android and iOS versions of the app to do that?" (store games that install small, then download the rest of the game inside the app, resuming after the app closes); on filing the steps: "Yes, file them"; the source, asked a hub on the home network or my own Dropbox: "Lets do dropbox"; and: "Yes, write it" (the SPEC change). Approved: "Approve, keep big .ipa" (PR #232; the `.ipa` with the game inside stays beside the download). Here: the SPEC change (#229), then 9.02b (#230) and 8.03f (#231).
- owner's answer 2026-11-23, in the Mac session, asked which Dropbox access the in-app download should use (with the note that the path in the Dropbox exception changes): "Reuse Save Relay's app". Here: 9.02b's Android download (#233) signs in to save-relay's Dropbox app, an App folder app, and reads only `Apps/save-relay/acnh-universal-game/android/` (the manifest and the prepared game's files, staged there by `tools/stage_dropbox_game.sh`); `SPEC.md`'s Dropbox exception names that folder. 8.03f (#231) does the same on the iPad.
- owner's choice 2026-11-23, in the Mac session: Dropbox for the Android save sync. Here: 9.03 (#81) brings save-relay into the Android app with its Dropbox backend, through save-relay's own Android bindings (its C interface and JNI, and its Kotlin wrapper ported to Java), the same Dropbox app the download signs in to.
- owner's question 2026-11-23, in the Mac session: "Can we try 60 on both platforms?" (Android and the iPad). Here: my own builds turn the SpeedFix on (`mods.speedfix`, the patch in `prepare/recomp/acnh/speedfix-3.0.3.pchtxt`). The 3.0.3 prepare with it is running on the Mac for the iPad, and the Android game library with it will be built on the Strix Halo tablet and staged to my Dropbox. The SpeedFix stays off by default with its own switch (M16), and 60 is not above 60 FPS (the Won'ts), so the spec stays approved.

Since these were written: the SPEC change of 2026-11-18 was approved the same day; the 3.0.3 prepare with the SpeedFix ran on the Mac and the Android game library was built on the Linux hub and staged (#264). My comment on #68 of 2026-11-18 ("the game folder lives in Dropbox, encrypted; keys stay on the device") is not yet a decision here: #321 asks me to make it one or withdraw it. The red `cpp-windows` job was accepted for the Apple merges in their merge commits; it is not an Apple fault and is #322.

## Stages

Each stage names its platforms (R04). Stages overlap: anything not blocked can be taken. Upstream keeps moving; once a module of ours is Rust, an upstream change to the code it replaced becomes an `upstream-sync` issue that ports the change by hand.

0. **Housekeeping** (Linux, Windows, macOS, Android and iOS in CI). Done: 0.01 the C++ unit tests on `ubuntu-24.04` (#2), 0.02 the Android cross-build and the iOS check (#3), 0.03 the SPEC change for the in-app download (#229 / PR #232), hosted runners (1.23b, #155), the upstream sync of 2026-11-23 (#145 / PR #246). Open: 0.04 (#322), 0.05 (#323), 0.06 (#324), 0.07 (#325), 0.08 (#326), 0.09 (#327), 0.10 (#328), 0.11 (#329), 0.12 (#330), 0.13 (#321). The daily upstream sync is done by hand; no workflow runs it.
1. **Engine core: the prep tools in Rust** (Linux, Windows, macOS in CI). Done: 1.01–1.32. Open: 1.33 (#331), 1.34 (#332), 1.35 (#333), 1.36 (#334), 1.37 (#335), 1.38 (#336), 1.39 (#337), 1.40 (#338), 1.41 (#339), 1.42 (#340), 1.43 (#341).
2. **Faces** (Linux, Windows, macOS; Android with 9.01). Done: 2.01–2.10. Open: 2.11 (#342), 2.12 (#343), 2.13 (#344), 2.14 (#345), 2.15 (#346), 2.16 (#347), 2.17 (#348).
3. **Recompiler to Rust** (Linux, Windows, macOS in CI). Done: 3.01–3.05; `crates/recomp` builds the game on every hub and Python `recomp/` is deleted (#49 / PR #286). Upstream's C++ `prepare/recomp` is **not** retired: only its source guard `recomp_source_test` left the build (#247 / PR #309); `nx_recomp`, `acnh_recomp` and `recomp_test` are still built, for the APK's on-phone Prepare. Open: 3.06 (#349), 3.07 (#350); after 1.0: 3.08 (#351).
4. **Runtime to Rust** (Linux, Windows, macOS, Android, iOS). Done: 4.01 the scaffold, and the Rust cores of the 11 groups, 4.02 `Config` (#51 / PR #276), 4.03 `Time` (#52 / PR #280), 4.04 `Saves` (#53 / PR #285), 4.05 `Files` (#54 / PR #290), 4.06 `Input` (#55 / PR #297), 4.07 `Audio` (#56 / PR #295), 4.08 `Services` (#57 / PR #302), 4.09 `Kernel` (#58 / PR #301), 4.10 `Memory` (#59 / PR #303), 4.11 `Platform` (#60 / PR #305), 4.12 `Loader` (#61 / PR #311). Not done: the C++ listed in the code map's runtime section (4.13, #352). ADR 0002 asks for a device check on the AMD handheld per step; none was recorded. Open: 4.13 (#352), 4.14 (#353), 4.15 (#354), 4.16 (#355), 4.17 (#356).
5. **Build hubs** (Linux, Windows, macOS; Android as a client). Done in the library: 5.01 discovery (#62), 5.02 pairing (#63), 5.03 sealed transfer (#64), 5.04 prepared-data transfer for Android (#65), 5.05 the hub command line and fleet reports (#66); Android wiring (#80 / PR #296). Open: 5.06 (#357), 5.07 (#358), 5.08 (#359), 5.09 (#360).
6. **SteamOS handhelds** (Linux). Done: 6.01a the contained folder (#191 / PR #275). Open: the fixes in Band 2, then 6.01 (#67), 6.02 (#68), 6.03 (#69).
7. **Windows desktop.** Done: 7.01a the contained folder (#190 / PR #277). 7.01 (#70 / PR #283) added the CI job; it has never passed (#322). Open: #322, then 7.02 (#71).
8. **Mac, iPhone and iPad.** Done in code: 8.01, 8.02a, 8.02b, 8.03a–8.03g, 8.05a. Open: 8.10 (#361), 8.11 (#362), 4.16 (#355), then the device runs in [Apple build and device acceptance](#apple-build-and-device-acceptance).
9. **Android** (Adreno; the Pixel Fold's Mali-G710 for information). Done in code: 9.01 (#79 / PR #304), 9.01a (#173 / PR #289), 9.02 (#80 / PR #296), 9.02a (#192 / PR #284), 9.02b (#230 / PRs #233, #298), 9.02c (#234 / PR #235), 9.03 (#81 / PR #259; #261 / PR #265; #262 / PR #272). Open: 9.05 (#363), 9.06 (#364), 9.07 (#365), 9.08 (#366), 9.09 (#367), 9.10 (#368), 9.11 (#369), 4.15 (#354), then 9.04 (#82). Planned after 1.0, no issue yet: self-update from Dropbox, a new-build check at start, a SideStore source feed.
10. **One island everywhere.** 10.01 (#83) the round trip across all six devices with the fleet warning on a version mismatch; 10.02 (#370) the acceptance SPEC asks for that no issue owns, and the issue for cutting 1.0.
11. **Design pass** (R04), last before 1.0. 11.01 (#84), after #348.

## Merged pieces

| Stage | Merged (issue / PR) |
|---|---|
| Build verification and sync | 0.01–0.02 (#2, #3); hosted runners (1.23b, #155); the SPEC change (0.03, #229 / PR #232); the upstream sync of 2026-11-23 (#145 / PR #246); the fix of `main` after the runtime ports (PR #294) |
| Engine and Prepare | 1.01–1.32 (#4–#34, #85, #152, #156, #159–#163, #189): the Rust stages, `assemble` (1.22b), `pack_folder` (6.01a #191 / PR #275, 7.01a #190 / PR #277), the `android` delivery target (#230 / PR #298), the recompiler staleness record (#227 / PR #273; inert since `recomp/` was deleted: #331), `atomic_write` (#244 / PR #245), the shader translator's write-xor-execute switch (PR #257), the Dropbox clean-up retry (PR #318), unchanged shader and pack work skipped (PR #319), `tools/setup_acnh.py` retired (1.31, #34 / PR #281) |
| Launcher | 2.01–2.10 (#35–#44, #158, #164–#169, #174): Continue, first run, Saves, Settings, Fleet, About, the real relay, controller input and glyphs, Prepare/Pull/Update, the Steam shortcut, release builds, the cloud-first first run (PRs #315, #316; see #321), `launcher/` retired (2.10, #44 / PR #274), upstream's packaging retired (2.09a, #174 / PR #288) |
| Recompiler and runtime | 3.01–3.05 (#45–#49; PR #286); the source guard retired (#247 / PR #309); 4.01–4.12 (#50–#61, PRs in Stages); resolution independence (#91 / PR #299); the yield back-off (#238 / PR #239); the pipeline-cache header check (#242 / PR #243); `input.layout = auto` (#251 / PR #252); the cbindgen fixes for the NDK build (PR #312) |
| Hub library | 5.01–5.05 (#62–#66); Android wiring (#80 / PR #296) |
| Apple build | 8.01 (#72), 8.02b (#157), 8.02a (#171), 8.03a–e and 8.05a (#148–#151, #170, #172), the Simulator memory guard (#240 / PR #241), libvpx on the phones (#253 / PR #254), iOS save-sync ways out (PRs #279, #291), Apple glyphs (PR #292), iOS touch controls hidden with a controller (PR #293), the small `.ipa` (8.03f, #231 / PR #282; device check open), Prepare's small `.ipa` (8.03g, #306 / PR #314), Sync now in the game (#260 / PR #278), the SpeedFix in the Apple apps (PR #307) |
| Android | 9.01–9.03 as in Stages; sound after an output change (#255 / PRs #256, #287); Mali-G710 work (#268 / PR #300); the Pixel Fold's screens (#249 / PRs #248, #250); a version per build (#263 / PR #270); the 60 fps and 720p defaults (#266 / PR #269); Shadow Quality (#267 / PR #271) |

## Apple build and device acceptance

### Build on the Mac

Every row is merged and checked in CI on `macos-15` with synthetic inputs: clippy and tests with all features, the save-relay link check, the iOS Rust check and the runtime's iOS static library, the layout tests and `check_ios_compile.sh`, `build_ios.sh` for the Simulator and the device, the bundle checks, `prepare_runner`'s `ios_real` test, and `tests/package_mac_test.sh` and `tests/package_ios_test.sh`. CI does not build or link the Mac player.

| Piece | Status | Still unverified |
|---|---|---|
| 8.01 Address-space and signing finding (#72) | Complete; my iPad probe is in #150 | |
| 8.02b Read-only game root (#157) | Complete | |
| 8.02a The Mac's one contained app (#171, PRs #220, #223) | Complete | A package of my real game opened on the Mac, Gatekeeper on first open (nothing strips quarantine: #362), a release test tag (needs #323): all #73 |
| 8.03a–b Compile and bundle (#148, #149) | Complete; `build_ios.sh` builds libvpx itself and stops without it | Movies on a device (#74) |
| 8.03c Compact guest layout (#150, PR #210) | Complete | Device boot and memory use (#74) |
| 8.03d Package a device build (#151, PR #199) | Complete | A leftover staging folder blocks later runs (#362) |
| 8.03e The `ios` target through Prepare (#170, PR #219) | Complete | The preflight does not check cmake, ninja or the Vulkan headers (#334) |
| 8.03f The small `.ipa` with the in-app download (#231, PR #282) | Code complete | The download on a device (#231); declining it (#355) |
| 8.03g The small `.ipa` through Prepare (#306, PR #314) | Complete | I upload `small/acnh-universal-game/ios/` to `Apps/save-relay/acnh-universal-game/ios/` by hand: this Mac's Dropbox does not sync `Apps/` |
| 8.05a The iPad's one app (#172, PRs #218, #222) | Complete, Nextcloud only | Dropbox for saves (#361); whether a pad can answer the alerts; every alert but the setup one (#74, #75, #76) |
| 8.04 iOS input (#75), 8.09 Mac input (#124) | Code merged (PRs #252, #292, #293) | Xbox, PlayStation and MFi mappings, dead zones, triggers, hot-plug, glyphs, touch back when no pad remains: device checks |

### Device checks

All mine, in this order; 8.02 (#73) has no open blocker.

1. **8.02 (#73).** Install the contained Mac app from my Dropbox and boot on Apple silicon. Read prepared data from the app; saves, settings and logs in Application Support. Note what Gatekeeper asks. Relaunch and reload.
2. **8.03 (#74).** Install the big `.ipa` with SideStore and the free Apple ID, boot on the iPad. Record the version, the install, the SideStore refresh, any memory or lifecycle failure. Repeat on an iPhone when one is at hand. Then the small `.ipa` (#231): upload its game folder, install, sign in to Dropbox, let the download stop and resume once. The two `.ipa`s share one bundle id, so one replaces the other.
3. **8.05 (#76) and 8.08 (#121)**, after #361. Boot on the Mac, save and quit so the relay pushes, boot the iPad and load that island; then back. The released Mac launcher carries the pinned relay (#323, #328). A failed required sync holds Play; with sync off, local play.
4. **8.09 (#124) and 8.04 (#75).** The controller checks in the table, on the Mac after #73 and on the iPad after #74.
5. **8.06–8.07 (#77, #78).** A session on each with a pad disconnected and reconnected, then save and reload. Carry the results into 10.01.

## Open-source pieces and where they fit

| Project | Licence | Where | How |
|---|---|---|---|
| Nycolazs/ACNHRecomp | MIT | the whole repo | upstream: synced by hand (R21), credited in About |
| iannitram/totk-universal | MIT | `platform`, `vdf`, `steam`, `artwork`, `backup`, `bundle`, `fleet`, `launch`, `update`, `hub`, `savecard`, `profile`, `recipe`, the Continue screen brief | copy with credit in each file header |
| iannitram/save-relay | MIT | `relay`, the iOS player (its Rust API), Android (its C interface and JNI), its Dropbox app for the in-app download | git dependency, pinned only by `Cargo.lock` (#328) |
| iannitram/Uncharted-Universal | GPL-2.0 | the strangler order and stage layout | learn only (this repo is MIT) |
| peiikai/LASRecomp | MIT | `recomp`, `runtime` | learn |
| BluetufieBugado/SuperMarioWonderRecomp | MIT | `runtime/src/nvn`, Android, Windows | learn |
| fedebuonco/nxrt, nxrt-frontend | Unlicense | `crates/runtime` (the yield back-off in `kernel.rs`, PR #239; its unused C++ copy was removed in #327), `app/` | adopt with credit; learn |
| cesar527/ACNHRecomp | MIT | `profile`, Android | adopt with credit: the thermal governor, power modes |
| ZUlxian/ACNHRecomp | MIT | `gamefolder`, Windows | adopt with credit |
| bingnnvjn/nvn-native-android, Timqt/d2r-android | MIT | Android, the pipeline cache (PR #243, idea only) | learn |
| iannitram/craft-libs | MIT OR Apache-2.0 | `platform::atomic_write` | adopt with credit (PR #245) |
| webmproject/libvpx | BSD-3-Clause | movies on the Android and iOS players | built from source by `tools/build_libvpx.sh` (v1.16.0, decoder only) |
| capstone-engine/capstone | BSD-3-Clause | `crates/recomp` (5.0.6, linked), `third_party/capstone` (5.0.7, upstream's C++ recompiler in the APK) | dependency; not yet in `NOTICE` (#329) |
| tmouh's SpeedFix | CC BY-NC 4.0 | `prepare/recomp/acnh/speedfix-*.pchtxt` | a fan mod, off by default; how it ships is mine to rule (#329) |
| kwsch/NHSE | GPL-3.0 | `savecard` after 1.0 | ideas only |
| citron-neo/emulator, pound-emu/pound | GPL-3.0 | `runtime` services | ideas only |
| kaiyao7/SwitchRecomp, bgyss/SwitchRecomp | GPL-3.0, none | `recomp` | ideas only |
| skyline-emu/skyline | none | Android | ideas only |
| FluffyFishGames/ACNHDesignPatternEditor | GPL-3.0 | after 1.0 | ideas only |
| Norviah/animal-crossing | MIT | Continue screen metadata after 1.0 | data, with credit |
| K11MCH1/libadrenotools | BSD-2-Clause | Android (Turnip) | through upstream's `tools/build_turnip.sh` |
| slint-ui/slint | royalty-free (attribution) | `app/` | dependency; About carries the notice |
| gilrs, mdns-sd, spake2, chacha20poly1305, sha2, cbindgen, cmake | MIT or Apache-2.0 | `app/`, `hub`, `bundle`, `runtime` | dependencies |

## Save Relay integration

I'm adopting [Save Relay](https://github.com/iannitram/save-relay) for this port's saves. It shares game saves between machines through my own Nextcloud, Google Drive or Dropbox, pulling at launch and pushing at exit, as one small Rust core reused by every game port. It is in the code on every platform, but not with one provider everywhere: the desktop launcher and CLI offer all three; the iPad's player offers Nextcloud only; the Android app offers Dropbox only (verified live on my Pixel Fold: sign-in and first pull). Until the iPad can use Dropbox for saves (#361) the round trip (#83) cannot pass through the iPad and the Android device with one provider. The Android and iPhone/iPad game download shares save-relay's Dropbox app ("Reuse Save Relay's app").

This repo links save-relay by git with no `rev` or `tag` in any manifest; only `Cargo.lock` holds it, at `132b9e5` (2026-11-17), 15 commits before the pre-release `v1.0.0-rc.2` (the lock still reports 1.0.0-rc.1). The copies in `third_party/save-relay/` and `android/java/org/saverelay/` carry that revision by hand. 8.08 (#121) asks for a release tag; save-relay has no final release yet (#328).

## After 1.0

The Won'ts in `SPEC.md`: local wireless play over the LAN (`docs/LOCAL_PLAY.md`, kept as upstream has it), save editing and design-pattern sync, the island's name on the save card (needs a save reader of our own), online play, Android GPUs other than Adreno (Mali), versions other than the four, above 60 FPS, and rewriting the renderer, the game code or the overlay in Rust. Upstream's Local Play and online services came in with the sync of 2026-11-23 as upstream has them; they are not 1.0 targets. The Android player already runs on the device's own Vulkan driver on other GPUs, and my Mali phone plays the game that way; making them a target is still after 1.0.

With an issue: one shared `universal-core` crate for the modules every Universal port copies (#147); one recompiler instead of two (#351). Without one yet: Android self-update from Dropbox, a new-build check at start, a SideStore source feed.

A Meta Quest app for playing on a large virtual theater screen in 3D is planned after 1.0. I expect it will probably follow 2.0.

## Game files

No game file is in this repo, and this file does not say where my copy is. Prepare reads my own game folder on each hub and writes the prepared game only into the app it builds and my own Dropbox; for the in-app download, `tools/stage_dropbox_game.sh` copies the prepared game's files and their manifest into save-relay's app folder in my own Dropbox, and only my own apps signed in to it read them (M14, M18). The launcher's cloud download for PCs and `acnh-universal cloud-upload` go beyond that and wait on #321.
