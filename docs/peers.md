# Peer code scan — 2026-11-17

Compared against local starting commit `01f42bcfd580a8b631d8e30ae3d63e257f51d925`.
Machine class: Apple silicon Mac (Darwin arm64). This is a source/API scan and synthetic
Rust validation, not an owner-device game run.

Peer discovery came from `SPEC.md:63–88`, `ROADMAP.md:50–71`,
`docs/code-map.md`, `docs/REFERENCES.md` and the README credits.
The project is a copy of Nycolazs/ACNHRecomp, not a GitHub fork; the worktree’s
`upstream` fetch URL is empty, so the documented upstream was queried explicitly.
The root licence is MIT. The roadmap’s per-peer rules override mere technical
compatibility: only its explicit copy/adopt peers were considered for imports.
“Learn”, “ideas only” and “link” entries remain links.

The three selected pieces are all from the MIT sibling totk-universal and target
the mapped location `crates/core/src/hub.rs` (#62–#64). Estimated engineering
effort avoided: **6–10 days** before remaining integration and device validation.
These are estimates, not measured completed roadmap days.

## Method and limits

Default-branch heads and their commit metadata, source files, file headers and
root licences were read through the connected GitHub API. Every available peer
below is pinned to its observed head, rather than a moving branch link.
The table records commit dates; repository “pushed” times can be newer without
a change to the default branch.

GitHub repository searches included `ACNHRecomp`, `SwitchRecomp`, `nxrt`,
`totk-universal`, `libadrenotools`, `gilrs`, `citron emulator` and `SDL`.
Recent ACNH forks were checked rather than assuming old listed forks were ahead.
SirOMGitsYOU and YousefMohsen0 resolve to the same upstream head.
969981 adds source tracking/release policy, not the missing controller/relay work.
SDL provides a current primary source for GameController behavior. This is a
bounded candidate scan, not a claim to have searched every GitHub repository.

**Path clarification pending:** “never edit anything outside this worktree”
conflicts with the separately requested /tmp clones and external issue-file write.
No peer clone has been created (in /tmp or inside the worktree), and the external
issue file has not been written. Read-only pinned API inspection was used while
that clarification remained unanswered. The ready issue text is saved in ignored
`target/peer-scan/peer-ACNH-Universal.md`. Build caches, logs and temporary test folders
were directed under ignored `target/peer-scan/`; transfer fixtures additionally use
ignored `target/hub-transfer-tests/`. No external credential/service setup was used.

No game data, dumps, real keys, shaders, game signatures or offset tables were
imported. Transfer tests construct synthetic bytes and synthetic pairing keys.
Peer binaries, prebuilt APKs/libraries and signing material were not downloaded.
In particular Timqt’s binary assets and nvn-native’s signing assets are excluded.
Source links that lead to game-specific hooks are research links only.

## Pinned peer inventory

Source file headers were checked for the cited findings. Unless a finding says
otherwise, those headers had no separate licence grant or conflicting restriction;
the linked root licence applies. For Slint, documentation/examples and framework
licensing are distinguished. Submodules/third-party assets are not blanket-covered
by a peer’s root licence and are not imported here.

| Peer | Observed default-branch commit | Branch · commit date (UTC) | Licence evidence at that pin |
|---|---|---|---|
| Nycolazs/ACNHRecomp | [`765aa5409c79a09d0e3f9ff977b54aabea763c9e`](https://github.com/Nycolazs/ACNHRecomp/tree/765aa5409c79a09d0e3f9ff977b54aabea763c9e) | `main` · 2026-11-17 | [MIT](https://github.com/Nycolazs/ACNHRecomp/blob/765aa5409c79a09d0e3f9ff977b54aabea763c9e/LICENSE) |
| iannitram/totk-universal | [`80148e16e60b5ca5d34ca87d3ef3360567490352`](https://github.com/iannitram/totk-universal/tree/80148e16e60b5ca5d34ca87d3ef3360567490352) | `main` · 2026-11-17 | [MIT](https://github.com/iannitram/totk-universal/blob/80148e16e60b5ca5d34ca87d3ef3360567490352/LICENSE) |
| iannitram/save-relay | [`132b9e57c7a46c596aacbd3d0a56a2579d89b9bc`](https://github.com/iannitram/save-relay/tree/132b9e57c7a46c596aacbd3d0a56a2579d89b9bc) | `main` · 2026-11-17 | [MIT](https://github.com/iannitram/save-relay/blob/132b9e57c7a46c596aacbd3d0a56a2579d89b9bc/LICENSE) |
| peiikai/LASRecomp | [`e4247f0be6d89b77388b1cdd9cabee131531a53d`](https://github.com/peiikai/LASRecomp/tree/e4247f0be6d89b77388b1cdd9cabee131531a53d) | `main` · 2026-11-16 | [MIT (LICENSE text)](https://github.com/peiikai/LASRecomp/blob/e4247f0be6d89b77388b1cdd9cabee131531a53d/LICENSE) |
| fedebuonco/nxrt | [`c41d913a27c5400984dcfc8ac2c384b32e477849`](https://github.com/fedebuonco/nxrt/tree/c41d913a27c5400984dcfc8ac2c384b32e477849) | `master` · 2026-10-29 | [Unlicense](https://github.com/fedebuonco/nxrt/blob/c41d913a27c5400984dcfc8ac2c384b32e477849/LICENSE) |
| fedebuonco/nxrt-frontend | [`5b75d34c50d1cc67f8b45b205bd202a1da505357`](https://github.com/fedebuonco/nxrt-frontend/tree/5b75d34c50d1cc67f8b45b205bd202a1da505357) | `main` · 2026-10-29 | [Unlicense](https://github.com/fedebuonco/nxrt-frontend/blob/5b75d34c50d1cc67f8b45b205bd202a1da505357/LICENSE) |
| cesar527/ACNHRecomp | [`b9a74f988671f3281f53a7ddb4b2b033f1afb76b`](https://github.com/cesar527/ACNHRecomp/tree/b9a74f988671f3281f53a7ddb4b2b033f1afb76b) | `main` · 2026-10-28 | [MIT](https://github.com/cesar527/ACNHRecomp/blob/b9a74f988671f3281f53a7ddb4b2b033f1afb76b/LICENSE) |
| ZUlxian/ACNHRecomp | [`d35ccfc10996f523d6605666c6de95bafcd3705e`](https://github.com/ZUlxian/ACNHRecomp/tree/d35ccfc10996f523d6605666c6de95bafcd3705e) | `main` · 2026-10-26 | [MIT](https://github.com/ZUlxian/ACNHRecomp/blob/d35ccfc10996f523d6605666c6de95bafcd3705e/LICENSE) |
| BluetufieBugado/SuperMarioWonderRecomp | [`9ecad0cef164008a474b5ff0fa50e6ef55edba60`](https://github.com/BluetufieBugado/SuperMarioWonderRecomp/tree/9ecad0cef164008a474b5ff0fa50e6ef55edba60) | `main` · 2026-10-16 | [MIT](https://github.com/BluetufieBugado/SuperMarioWonderRecomp/blob/9ecad0cef164008a474b5ff0fa50e6ef55edba60/LICENSE) |
| bingnnvjn/nvn-native-android | [`950925acc7f22cb880f999ec5b790085f2f32b64`](https://github.com/bingnnvjn/nvn-native-android/tree/950925acc7f22cb880f999ec5b790085f2f32b64) | `main` · 2026-11-15 | [MIT](https://github.com/bingnnvjn/nvn-native-android/blob/950925acc7f22cb880f999ec5b790085f2f32b64/LICENSE) |
| Timqt/d2r-android | [`a2372bc4f6c0929dd409a1639723874559da77fb`](https://github.com/Timqt/d2r-android/tree/a2372bc4f6c0929dd409a1639723874559da77fb) | `main` · 2026-10-23 | [MIT](https://github.com/Timqt/d2r-android/blob/a2372bc4f6c0929dd409a1639723874559da77fb/LICENSE) |
| kwsch/NHSE | [`cb0745415945776f73375bf0a434a8babf059307`](https://github.com/kwsch/NHSE/tree/cb0745415945776f73375bf0a434a8babf059307) | `master` · 2026-05-23 | [GPL-3.0](https://github.com/kwsch/NHSE/blob/cb0745415945776f73375bf0a434a8babf059307/LICENSE) |
| pound-emu/pound | [`f7967a8a59bddd460ba9b924c3f4e2693cc028a3`](https://github.com/pound-emu/pound/tree/f7967a8a59bddd460ba9b924c3f4e2693cc028a3) | `main` · 2026-11-14 | [GPL-3.0](https://github.com/pound-emu/pound/blob/f7967a8a59bddd460ba9b924c3f4e2693cc028a3/LICENSE) |
| kaiyao7/SwitchRecomp | [`f7b26dcab3aaaa444303ba70cc549d781e440932`](https://github.com/kaiyao7/SwitchRecomp/tree/f7b26dcab3aaaa444303ba70cc549d781e440932) | `main` · 2026-11-05 | [GPL-3.0](https://github.com/kaiyao7/SwitchRecomp/blob/f7b26dcab3aaaa444303ba70cc549d781e440932/LICENSE) |
| bgyss/SwitchRecomp | [`0f99e1c5c1e78fdc8e01e58a0f8dc8622e82976b`](https://github.com/bgyss/SwitchRecomp/tree/0f99e1c5c1e78fdc8e01e58a0f8dc8622e82976b) | `main` · 2026-02-18 | [no licence file](https://github.com/bgyss/SwitchRecomp/blob/0f99e1c5c1e78fdc8e01e58a0f8dc8622e82976b/README.md) |
| skyline-emu/skyline | [`21d31f74b9e711b2b71ee57e016b6b2e8966e896`](https://github.com/skyline-emu/skyline/tree/21d31f74b9e711b2b71ee57e016b6b2e8966e896) | `ftx1` · 2026-08-18 | [no current source/licence; archival README](https://github.com/skyline-emu/skyline/blob/21d31f74b9e711b2b71ee57e016b6b2e8966e896/README.md) |
| FluffyFishGames/ACNHDesignPatternEditor | [`a104379c6d45ad8b62fe2781d3a5ad9ca2229c79`](https://github.com/FluffyFishGames/ACNHDesignPatternEditor/tree/a104379c6d45ad8b62fe2781d3a5ad9ca2229c79) | `master` · 2026-05-01 | [GPL-3.0](https://github.com/FluffyFishGames/ACNHDesignPatternEditor/blob/a104379c6d45ad8b62fe2781d3a5ad9ca2229c79/LICENSE) |
| Norviah/animal-crossing | [`8bec58729a10879d575ba18d50a1dda74804a374`](https://github.com/Norviah/animal-crossing/tree/8bec58729a10879d575ba18d50a1dda74804a374) | `master` · 2022-01-07 | [MIT](https://github.com/Norviah/animal-crossing/blob/8bec58729a10879d575ba18d50a1dda74804a374/LICENSE) |
| FEX-Emu/FEX | [`be66e03d65217f53a7a2e5af46473cc9a79ce3f8`](https://github.com/FEX-Emu/FEX/tree/be66e03d65217f53a7a2e5af46473cc9a79ce3f8) | `main` · 2026-11-17 | [MIT](https://github.com/FEX-Emu/FEX/blob/be66e03d65217f53a7a2e5af46473cc9a79ce3f8/LICENSE) |
| slint-ui/slint | [`12a4256a9b0d8af02feab534eafbe744806acbfb`](https://github.com/slint-ui/slint/tree/12a4256a9b0d8af02feab534eafbe744806acbfb) | `master` · 2026-11-17 | [framework: selectable terms; docs/examples: MIT](https://github.com/slint-ui/slint/blob/12a4256a9b0d8af02feab534eafbe744806acbfb/LICENSE.md) |
| iannitram/Uncharted-Universal | [`bbbc618a10714f156f2563f505615776a42386cc`](https://github.com/iannitram/Uncharted-Universal/tree/bbbc618a10714f156f2563f505615776a42386cc) | `main` · 2026-11-17 | [GPL-2.0](https://github.com/iannitram/Uncharted-Universal/blob/bbbc618a10714f156f2563f505615776a42386cc/LICENSE) |
| SirOMGitsYOU/ACNHRecomp | [`765aa5409c79a09d0e3f9ff977b54aabea763c9e`](https://github.com/SirOMGitsYOU/ACNHRecomp/tree/765aa5409c79a09d0e3f9ff977b54aabea763c9e) | `main` · 2026-11-17 | [MIT](https://github.com/SirOMGitsYOU/ACNHRecomp/blob/765aa5409c79a09d0e3f9ff977b54aabea763c9e/LICENSE) |
| 969981/ACNHRecomp | [`d166d5fa1b909268c7de5a413fbf0065e9a10068`](https://github.com/969981/ACNHRecomp/tree/d166d5fa1b909268c7de5a413fbf0065e9a10068) | `main` · 2026-11-11 | [MIT](https://github.com/969981/ACNHRecomp/blob/d166d5fa1b909268c7de5a413fbf0065e9a10068/LICENSE) |
| YousefMohsen0/ACNHRecomp | [`765aa5409c79a09d0e3f9ff977b54aabea763c9e`](https://github.com/YousefMohsen0/ACNHRecomp/tree/765aa5409c79a09d0e3f9ff977b54aabea763c9e) | `main` · 2026-11-17 | [MIT](https://github.com/YousefMohsen0/ACNHRecomp/blob/765aa5409c79a09d0e3f9ff977b54aabea763c9e/LICENSE) |
| bylaws/libadrenotools | [`8fae8ce254dfc1344527e05301e43f37dea2df80`](https://github.com/bylaws/libadrenotools/tree/8fae8ce254dfc1344527e05301e43f37dea2df80) | `master` · 2024-09-10 | [BSD-2-Clause](https://github.com/bylaws/libadrenotools/blob/8fae8ce254dfc1344527e05301e43f37dea2df80/LICENSE) |
| Arvamer/gilrs | [`78581b7d58425f28c6edf8917d8a7b209393058c`](https://github.com/Arvamer/gilrs/tree/78581b7d58425f28c6edf8917d8a7b209393058c) | `master` · 2025-11-11 | [MIT OR Apache-2.0](https://github.com/Arvamer/gilrs/blob/78581b7d58425f28c6edf8917d8a7b209393058c/LICENSE-MIT) |
| libsdl-org/SDL | [`1fc62260fc81ecbead66be1a1997bc3ee673afba`](https://github.com/libsdl-org/SDL/tree/1fc62260fc81ecbead66be1a1997bc3ee673afba) | `main` · 2026-11-18 | [Zlib](https://github.com/libsdl-org/SDL/blob/1fc62260fc81ecbead66be1a1997bc3ee673afba/LICENSE.txt) |

Unavailable/stale names:

- `citron-neo/emulator`: GitHub API returned 404. SPEC’s GPL-3.0/ideas-only rule
  remains; no current head, files or licence could be verified. Search results for
  similarly named mirrors were not treated as verified upstream code.
- `K11MCH1/libadrenotools`: API returned 404. Search found canonical
  `bylaws/libadrenotools`; this repo already pins that exact head in CMake.
- `gilrs-project/gilrs`: API returned 404; `Arvamer/gilrs` supplies the readable
  desktop backend and dual MIT/Apache licence.
- `skyline-emu/skyline`: the pinned default branch contains only an archival
  README; no current emulator code or LICENSE is available. Its historical MPL
  statement does not grant access to absent source.
- `bgyss/SwitchRecomp`: no licence file in the pinned tree and no permission grant
  in the inspected Rust file. Keep it ideas-only as SPEC requires.

## Findings

Line ranges refer to peer files at the full commit recorded above. Licence
evidence is pinned in the inventory; “root LICENSE” does not assert that all bundled
third-party code has that licence. Time savings exclude owner-device sessions.

| Peer | Pinned source and lines | Licence / file-header check | Helps | Decision | Why / estimated saving |
|---|---|---|---|---|---|
| iannitram/totk-universal | [crates/core/src/hub.rs:27–285](https://github.com/iannitram/totk-universal/blob/80148e16e60b5ca5d34ca87d3ef3360567490352/crates/core/src/hub.rs#L27-L285) | MIT; no overriding file header | #62 | Imported (1/3) | mDNS browse/filter/dedup, TCP boundary checks and in-memory streams; only the address predicate existed here. About 1–2 days saved. Production announcement still needs wiring; the fake announcement test is Linux-only. |
| iannitram/totk-universal | [crates/core/src/hub.rs:287–544](https://github.com/iannitram/totk-universal/blob/80148e16e60b5ca5d34ca87d3ef3360567490352/crates/core/src/hub.rs#L287-L544) | MIT; no overriding file header | #63 | Imported (2/3) | Six-digit symmetric SPAKE2, directional key confirmation, expiry/attempt limits, peer persistence and redacted Debug. About 2–3 days saved; Unix private-file handling moved to platform. |
| iannitram/totk-universal | [crates/core/src/hub.rs:546–1136](https://github.com/iannitram/totk-universal/blob/80148e16e60b5ca5d34ca87d3ef3360567490352/crates/core/src/hub.rs#L546-L1136) | MIT; no overriding file header | #64; foundation for #68, #80 | Imported (3/3) | Paired challenge/response; 64 KiB ChaCha20-Poly1305 chunks; authenticated resume receipts, SHA-256 verification and path guards. About 3–5 days saved. Ordinary build bundles only; neither game-data transfer nor CLI integration is supplied. |
| iannitram/totk-universal | [crates/core/src/vdf.rs:31–62](https://github.com/iannitram/totk-universal/blob/80148e16e60b5ca5d34ca87d3ef3360567490352/crates/core/src/vdf.rs#L31-L62) | MIT; no overriding file header | #112 | Link-only | Same unchecked NUL byte serialization as ours; no ready fix. Keep the API choice coordinated with the twins named by #112. No implementation time saved. |
| iannitram/totk-universal | [crates/core/src/fleet.rs:1–130](https://github.com/iannitram/totk-universal/blob/80148e16e60b5ca5d34ca87d3ef3360567490352/crates/core/src/fleet.rs#L1-L130) | MIT; no overriding file header | #66 | Link-only | Record codec and drift warnings already exist here; sibling hub has no fleet-record exchange. No missing wire implementation to import. |
| libsdl-org/SDL | [src/joystick/apple/SDL_mfijoystick.m:631–830](https://github.com/libsdl-org/SDL/blob/1fc62260fc81ecbead66be1a1997bc3ee673afba/src/joystick/apple/SDL_mfijoystick.m#L631-L830) | Zlib; matching file header | #124, #75, #77, #78 | Link-only | GameController extended-pad mapping plus connect/disconnect notification observers. SDL is already linked on Mac. This may avoid writing a second input backend (1–2 days), but installed SDL driver selection, all buttons, touch policy and two controller families still require device evidence. |
| Nycolazs/ACNHRecomp | [runtime/src/platform_mac.cpp:350–500](https://github.com/Nycolazs/ACNHRecomp/blob/765aa5409c79a09d0e3f9ff977b54aabea763c9e/runtime/src/platform_mac.cpp#L350-L500) | MIT; root LICENSE | #124, #55, #77 | Link-only | SDL-to-Switch mapping and multi-pad management already exist locally. Compare upstream changes through daily sync; do not replace fork changes wholesale. No completed GameController device proof. |
| peiikai/LASRecomp | [runtime/hle/input.c:1–180](https://github.com/peiikai/LASRecomp/blob/e4247f0be6d89b77388b1cdd9cabee131531a53d/runtime/hle/input.c#L1-L180) | MIT confirmed by LICENSE (API reports NOASSERTION); no overriding header | #55, #75, #124 | Link-only (roadmap: learn) | SDL gamepad/rumble/LED lifecycle reference, rather than a new Rust GameController layer. About half a day of design research; no ACNH mapping/device proof. |
| peiikai/LASRecomp | [runtime/platform/ios.c:1–110](https://github.com/peiikai/LASRecomp/blob/e4247f0be6d89b77388b1cdd9cabee131531a53d/runtime/platform/ios.c#L1-L110) | MIT; no overriding file header | #59, #60, #73, #74, #78 | Link-only (roadmap: learn) | iOS host and memory-entitlement checks; compare with ios.entitlements:1–11 and gamebuild.c:23–50. Useful failure diagnostics (half–one day); not a compatible guest-memory replacement or cable-install run. |
| iannitram/save-relay | [src/api.rs:19–115](https://github.com/iannitram/save-relay/blob/132b9e57c7a46c596aacbd3d0a56a2579d89b9bc/src/api.rs#L19-L115) | MIT; no overriding file header | #121, #76, #81, #83 | Link-only (roadmap: link) | Typed outcome/card contract is ready; adapter is absent here. Reuse by a pinned supported release dependency (1–2 days saved), not vendoring. Scanned Cargo.toml says 1.0.0-rc.1; #121 still requires the supported Mac release tag. |
| iannitram/save-relay | [include/save_relay.h:1–92](https://github.com/iannitram/save-relay/blob/132b9e57c7a46c596aacbd3d0a56a2579d89b9bc/include/save_relay.h#L1-L92) | MIT; no overriding file header | #76, #81, #121 | Link-only (roadmap: link) | Existing host callbacks for app folders, credentials and HTTP, init/pull/push/free; src/ffi.rs:356–548 implements them. Saves 2–4 days of ABI design; Apple Keychain/network/lifecycle glue remains. |
| iannitram/save-relay | [bindings/android/src/main/kotlin/org/saverelay/SaveRelay.kt:1–107](https://github.com/iannitram/save-relay/blob/132b9e57c7a46c596aacbd3d0a56a2579d89b9bc/bindings/android/src/main/kotlin/org/saverelay/SaveRelay.kt#L1-L107) | MIT; no overriding file header | #81, #79 | Link-only (roadmap: link) | Kotlin/JNI lifecycle facade and SaveRelayHost.kt:1–34 provide a ready embedding route (1–2 days). Must link the library and provide app-owned credentials/HTTP; no ACNH launcher glue present. |
| slint-ui/slint | [docs/astro/src/content/docs/guide/platforms/mobile/android.mdx:12–90](https://github.com/slint-ui/slint/blob/12a4256a9b0d8af02feab534eafbe744806acbfb/docs/astro/src/content/docs/guide/platforms/mobile/android.mdx#L12-L90) | Documentation MIT; runtime separately licensed per LICENSE.md:6–20 | #79 | Link-only (dependency route) | android-activity entry point, cdylib and package metadata (half–one day saved). Must reconcile its Activity with this repo's player; desktop app currently has no Android entry point. Follow selected framework licence/attribution. |
| bingnnvjn/nvn-native-android | [shell/android/java/com/bingnnvjn/nva/MainActivity.java:18–110](https://github.com/bingnnvjn/nvn-native-android/blob/950925acc7f22cb880f999ec5b790085f2f32b64/shell/android/java/com/bingnnvjn/nva/MainActivity.java#L18-L110) | MIT; no overriding file header | #79, #80, #60 | Link-only (roadmap: learn) | Rust/JNI shell lifecycle and private data directory contract (half–one day research). Different runtime and launcher; not Slint, a hub puller or a full ACNH port. |
| Timqt/d2r-android | [adapt/android-gamepad-and-frame-rate.patch:1–85](https://github.com/Timqt/d2r-android/blob/a2372bc4f6c0929dd409a1639723874559da77fb/adapt/android-gamepad-and-frame-rate.patch#L1-L85) | MIT root; patch context keeps upstream licensing; no import | #55, #75, #82 | Link-only (roadmap: learn) | Documents controller/touch ownership and JNI UI re-entry hazard. About half a day saved avoiding lifecycle mistakes. Android-specific; no Apple controller proof. APK, prebuilt libraries, signing material and other binary assets excluded. |
| BluetufieBugado/SuperMarioWonderRecomp | [runtime/src/platform_android_input.cpp:48–165](https://github.com/BluetufieBugado/SuperMarioWonderRecomp/blob/9ecad0cef164008a474b5ff0fa50e6ef55edba60/runtime/src/platform_android_input.cpp#L48-L165) | MIT; no overriding file header | #55, #79, #82 | Link-only (roadmap: learn) | Controller/touch ownership pattern already inherited in our Android input layer. Different game/version; no missing Rust implementation or on-device ACNH validation. |
| cesar527/ACNHRecomp | [runtime/src/thermal.cpp:1–65](https://github.com/cesar527/ACNHRecomp/blob/b9a74f988671f3281f53a7ddb4b2b033f1afb76b/runtime/src/thermal.cpp#L1-L65) | MIT; no overriding file header | #67, #69, #82, #60 | Link-only | Older Android thermal policies; our thermal.cpp is the newer upstream blob. Governor and root clock controls are not portable handheld-profile imports. No new applicable piece; device sessions still required. |
| ZUlxian/ACNHRecomp | [runtime/src/nvn/nvn_ultrawide.cpp:89–145](https://github.com/ZUlxian/ACNHRecomp/blob/d35ccfc10996f523d6605666c6de95bafcd3705e/runtime/src/nvn/nvn_ultrawide.cpp#L89-L145) | MIT; no overriding file header | #91 | Link-only | Older 16:10/aspect parsing; newer local code already does aspect selection and resize. Does not supply general tall-screen/HUD behavior. No signature/offset/hook tables copied. |
| Nycolazs/ACNHRecomp | [runtime/src/nvn/nvn_ultrawide.cpp:91–125](https://github.com/Nycolazs/ACNHRecomp/blob/765aa5409c79a09d0e3f9ff977b54aabea763c9e/runtime/src/nvn/nvn_ultrawide.cpp#L91-L125) | MIT; no overriding file header | #91 | Link-only | Current display aspect and render-scale plumbing are useful entry points; local equivalents already exist. Completing tall FOV and safe HUD edges is game-specific work. No ready generic fix found. |
| Nycolazs/ACNHRecomp | [runtime/src/mod_fetch.cpp:110–147](https://github.com/Nycolazs/ACNHRecomp/blob/765aa5409c79a09d0e3f9ff977b54aabea763c9e/runtime/src/mod_fetch.cpp#L110-L147) | MIT; no overriding file header | #85, #54 | Link-only | Still one on-by-default mod-fetch switch, not independent opt-in pt-BR/SpeedFix controls. Already present here; do not import mod payloads or download scripts. |
| Nycolazs/ACNHRecomp | [.github/workflows/build.yml:41–82](https://github.com/Nycolazs/ACNHRecomp/blob/765aa5409c79a09d0e3f9ff977b54aabea763c9e/.github/workflows/build.yml#L41-L82) | MIT; root LICENSE | #70, #73, #74 | Link-only | Windows C++ toolchain/build recipe (half–one day of setup reference), but upstream workflow already exists here. Our self-hosted runner and Rust-player checks still need implementation/actual builds. |
| fedebuonco/nxrt | [runtime/services/time.c:1–132](https://github.com/fedebuonco/nxrt/blob/c41d913a27c5400984dcfc8ac2c384b32e477849/runtime/services/time.c#L1-L132) | Unlicense; no overriding file header | #52 | Link-only (roadmap: learn) | Small host-clock/shared-memory service reference; ours already has C++ equivalents. Rust ABI/behavior port remains. Half a day of orientation, not a drop-in Rust group. |
| fedebuonco/nxrt | [runtime/services/fs.c:721–910](https://github.com/fedebuonco/nxrt/blob/c41d913a27c5400984dcfc8ac2c384b32e477849/runtime/services/fs.c#L721-L910) | Unlicense; no overriding file header | #53, #54 | Link-only (roadmap: learn) | Transactional host save/filesystem commit handling and Windows open-file constraints. About one day of design review; different service ABI. No save data or format tables copied. |
| fedebuonco/nxrt | [runtime/services/audio.c:1–90](https://github.com/fedebuonco/nxrt/blob/c41d913a27c5400984dcfc8ac2c384b32e477849/runtime/services/audio.c#L1-L90) | Unlicense; no overriding file header | #56 | Link-only (roadmap: learn) | Audio service interface reference; local hle_audio.cpp already exists. Different runtime, C code; Rust callback/ownership port still needed. |
| fedebuonco/nxrt | [runtime/kernel.c:1–90](https://github.com/fedebuonco/nxrt/blob/c41d913a27c5400984dcfc8ac2c384b32e477849/runtime/kernel.c#L1-L90) | Unlicense; no overriding file header | #57, #58, #59 | Link-only (roadmap: learn) | Handle, thread and SVC architecture orientation (one day). Existing C++ services are the ABI truth; no matching Rust implementation found. |
| fedebuonco/nxrt | [runtime/loader.c:1–90](https://github.com/fedebuonco/nxrt/blob/c41d913a27c5400984dcfc8ac2c384b32e477849/runtime/loader.c#L1-L90) | Unlicense; no overriding file header | #61, #49 | Link-only (roadmap: learn) | NSO/relocation/symbol resolution design reference; loader is C, not Rust recompiler driver. Existing Python/C++ parity still required; half a day orientation. |
| fedebuonco/nxrt-frontend | [src-tauri/src/pads.rs:1–110](https://github.com/fedebuonco/nxrt-frontend/blob/5b75d34c50d1cc67f8b45b205bd202a1da505357/src-tauri/src/pads.rs#L1-L110) | Unlicense; no overriding file header | #84, #55 | Link-only (roadmap: learn) | Releases SDL controllers while a game owns them and explains a Pro Controller/gilrs driver conflict. Half a day of lifecycle research; launcher is Tauri, not this Slint design. |
| iannitram/Uncharted-Universal | [crates/runtime/src/lib.rs:1–77](https://github.com/iannitram/Uncharted-Universal/blob/bbbc618a10714f156f2563f505615776a42386cc/crates/runtime/src/lib.rs#L1-L77) | GPL-2.0; root LICENSE | #50, #51, #52, #53, #54, #55, #56, #57, #58, #59, #60, #61, #66 | Link-only (ideas only) | Rust staticlib/C ABI strangler structure and runtime modules are useful plan comparisons; incompatible with this MIT repo and explicitly learn-only. No code imported. |
| bgyss/SwitchRecomp | [crates/recomp-runtime/src/lib.rs:1–60](https://github.com/bgyss/SwitchRecomp/blob/0f99e1c5c1e78fdc8e01e58a0f8dc8622e82976b/crates/recomp-runtime/src/lib.rs#L1-L60) | No licence file; no permission in file header | #49, #50, #55, #56, #59 | Link-only (ideas only) | Rust scaffolding and replay architecture, not ACNH-compatible passes. No copying permission and no proven matching driver. |
| kaiyao7/SwitchRecomp | [src/codegen/codegen.cpp:1–45](https://github.com/kaiyao7/SwitchRecomp/blob/f7b26dcab3aaaa444303ba70cc549d781e440932/src/codegen/codegen.cpp#L1-L45) | GPL-3.0; root LICENSE | #49, #59, #61 | Link-only (ideas only) | C11 IR codegen and a different runtime, not the required Rust/Python byte-parity driver. Incompatible licence; no code copied. |
| pound-emu/pound | [src/core/memory/memory.c:1–90](https://github.com/pound-emu/pound/blob/f7967a8a59bddd460ba9b924c3f4e2693cc028a3/src/core/memory/memory.c#L1-L90) | GPL-3.0; root LICENSE | #57, #58, #59 | Link-only (ideas only) | Current emulator memory model is an architectural reference only; different ABI and incompatible licence. |
| kwsch/NHSE | [NHSE.Core/Encryption/EncryptedSaveFile.cs:1–5](https://github.com/kwsch/NHSE/blob/cb0745415945776f73375bf0a434a8babf059307/NHSE.Core/Encryption/EncryptedSaveFile.cs#L1-L5) | GPL-3.0; root LICENSE | #53, #76, #81, #83 | Link-only (ideas only) | Save-format research is not needed for opaque directory relay. Do not copy encryption constants, data, offset tables or saves. No immediate implementation saving. |
| FluffyFishGames/ACNHDesignPatternEditor | [Assets/Plugins/Savegame/Data/DesignPattern.cs:1–20](https://github.com/FluffyFishGames/ACNHDesignPatternEditor/blob/a104379c6d45ad8b62fe2781d3a5ad9ca2229c79/Assets/Plugins/Savegame/Data/DesignPattern.cs#L1-L20) | GPL-3.0; root LICENSE | #53, #84 | Link-only (ideas only) | Pattern editing/sync is explicitly after 1.0; incompatible licence and no current-step implementation benefit. |
| skyline-emu/skyline | [README.md:1–18](https://github.com/skyline-emu/skyline/blob/21d31f74b9e711b2b71ee57e016b6b2e8966e896/README.md#L1-L18) | Current tree has no source/licence; README describes historical MPL-2.0 | #55, #60, #79, #82 | Link-only (unavailable source) | Default branch is now an archival notice. No source to inspect or import; do not reconstruct from unlicensed mirrors. |
| Norviah/animal-crossing | [README.md:1–10](https://github.com/Norviah/animal-crossing/blob/8bec58729a10879d575ba18d50a1dda74804a374/README.md#L1-L10) | MIT for repository; spreadsheet attribution also requested at README:77 | #84 | Link-only | Item/villager metadata is after 1.0; not a missing launcher design implementation. No JSON, images or other data imported. |
| FEX-Emu/FEX | [Readme.md:1–14](https://github.com/FEX-Emu/FEX/blob/be66e03d65217f53a7a2e5af46473cc9a79ce3f8/Readme.md#L1-L14) | MIT; root LICENSE | #60, #79 | Link-only | x86-on-ARM translation is not needed by ACNH's native per-CPU recompilation route. No immediate saving. |
| bylaws/libadrenotools | [include/adrenotools/driver.h:1–55](https://github.com/bylaws/libadrenotools/blob/8fae8ce254dfc1344527e05301e43f37dea2df80/include/adrenotools/driver.h#L1-L55) | BSD-2-Clause; explicit matching SPDX | #60, #79, #82 | Link-only (existing dependency) | Canonical replacement for stale K11MCH1 URL; this exact commit is already pinned in CMakeLists.txt. Keep using its loader; no duplicate source import. |
| Arvamer/gilrs | [gilrs-core/src/platform/macos/gamepad.rs:1–45](https://github.com/Arvamer/gilrs/blob/78581b7d58425f28c6edf8917d8a7b209393058c/gilrs-core/src/platform/macos/gamepad.rs#L1-L45) | MIT OR Apache-2.0 (both root licence files) | #124, #55, #84 | Link-only (existing dependency) | Launcher input only; macOS backend is not evidence of the game's GameController route. No additional current-step implementation imported. |
| 969981/ACNHRecomp | [.github/workflows/upstream-sync-source-release.yml:1–55](https://github.com/969981/ACNHRecomp/blob/d166d5fa1b909268c7de5a413fbf0065e9a10068/.github/workflows/upstream-sync-source-release.yml#L1-L55) | MIT; root LICENSE | #70, #73 | Link-only (search result) | Recent source-release fork, not new controller/relay/runtime work. Source-tracking workflow does not replace self-hosted C++ build coverage. |
| SirOMGitsYOU/ACNHRecomp | [runtime/src/platform_mac.cpp:350–500](https://github.com/SirOMGitsYOU/ACNHRecomp/blob/765aa5409c79a09d0e3f9ff977b54aabea763c9e/runtime/src/platform_mac.cpp#L350-L500) | MIT; same source tree as upstream | #124, #75 | Link-only (search result) | Default-branch commit exactly matches Nycolazs at scan time; no extra implementation. YousefMohsen0/ACNHRecomp also resolves to the same commit. |

## Coverage of every requested open step

“No ready piece” means none in the inspected sources, not proof that none exists
elsewhere. No issue is closed or marked complete by this scan. Research estimates overlap; only the three imported pieces are counted in the 6–10 day total.

| Step | Evidence considered | Missing behavior / result | Estimated effort avoided |
|---|---|---|---|
| #124 | #124 SDL Apple backend and upstream mapping | Existing SDL backend may already satisfy the framework route. Verify installed driver and two controller families on the owner’s Mac; no new direct backend imported.  1–2 days if existing SDL route is confirmed |
| #121 | save-relay Rust contract, C ABI, supported release tagging | Link dependency at the supported Mac tag, enable relay in release build, supply Keychain/setup, and prove pull/run/push on devices. No released-app claim.  1–2 days via linked API; release/device work excluded |
| #112 | totk vdf writer | Same NUL corruption defect. No fix imported; coordinate Result-vs-caller-validation choice with Ratchet/Guardian twins.  0; same bug |
| #91 | ZUlxian and upstream aspect plumbing | Already represented locally; no generic tall-FOV/safe-HUD solution. No hooks, signatures or offset tables imported.  0; missing behavior not found |
| #85 | upstream mod_fetch | Still default-on and combined. No ready per-mod opt-in launcher implementation found.  0; missing switches not found |
| #84 | nxrt frontend; Slint mobile docs | Lifecycle/design references only. Sibling has no docs/design/continue-screen.md at the scanned pin (404); no completed ACNH design pass.  0 implementation days; lifecycle research only |
| #83 | save-relay APIs; hub imports | Infrastructure can save implementation work; a real one-island round trip still needs all device builds, credentials and owner observations.  0 additional; device round trip required |
| #82 | Wonder/Timqt Android input; existing Turnip dependency | Controller/touch and GPU-loader references only. No Adreno ACNH controller session performed.  0 implementation days; device session required |
| #81 | save-relay C/JNI/Kotlin binding | Ready binding to link; Android app credential/network/lifecycle integration remains.  1–2 days via linked binding |
| #80 | hub build transfer; Android shell reference | Build-only transport now present. #65’s separate game library/data channel, pairing UI and launcher pull path still needed.  Uses #64 foundation; no separate saving counted |
| #79 | Slint Android entry/package guide; nvn-native Android shell | Ready packaging examples, different player Activity ownership. No same-APK Slint/player integration imported.  0.5–1 day packaging research |
| #78 | SDL Apple hot-plug; LAS iOS diagnostics | Reference only; owner iPhone/iPad session and controller kinds remain untested.  0; owner-device session |
| #77 | SDL Apple hot-plug; upstream Mac input | Reference only; no native game session or controller hot-plug proof on owner’s Mac.  0; owner-device session |
| #76 | save-relay app host C ABI | Link existing staticlib/ffi feature and supply secure Apple callbacks; background/foreground save behavior and device run remain.  2–4 days ABI work avoided by linking |
| #75 | SDL GameController mapping; Android touch ownership patterns | Input mapping prior art only. iOS layer and touch visibility on controller count still require implementation/device tests.  Up to 1 day of input research; integration unproven |
| #74 | LAS iOS host, entitlements and gamebuild reference | Can reduce entitlement/build diagnosis, but no signing, cable installation or game folder used.  0.5–1 day diagnostic research; installation still required |
| #73 | LAS iOS build reference; existing upstream Mac build route | Native-build reference only; no game built/booted. This scan ran Rust tests on Darwin arm64.  0; native build/play evidence still required |
| #71 | existing upstream SDL input | An RTX 3090 Ti hour-long controller run is evidence, not code available to import; no session claimed.  0; owner-device session |
| #70 | upstream Windows workflow; source-release fork | C++ build recipe already here. Self-hosted Windows runner/player build proof remains.  0.5–1 day setup reference; already local |
| #69 | existing thermal/input; opaque save relay | Week-long island validation on the AMD handheld cannot be imported; no island/save accessed.  0; week-long validation |
| #68 | imported serve/pull APIs | Substantial transport work reused; Game Mode pull orchestration and handheld session still needed.  Uses #64 foundation; no separate saving counted |
| #67 | hub discovery/transport; existing profile/thermal | Hub service wiring and Strix Halo Game Mode session remain; Android root governor is not a generic Linux power profile.  Uses #62 foundation; no separate saving counted |
| #66 | sibling fleet codec; GPL sibling service architecture | Codec already local; fleet exchange and hub/fleet commands not supplied by these imports. GPL code excluded.  0; codec already local, wire code absent |
| #65 | sibling transfer’s bundle whitelist | No peer game-data channel found that follows this project’s owned-device policy. Current transfer deliberately rejects game library/data/saves.  0; owned game-data channel absent |
| #64 | sibling authenticated resumable transfer | Imported library APIs and adversarial/64 MiB loopback tests. Caller installs returned verified bundle. No CLI or live hub service supplied.  3–5 days (imported) |
| #63 | sibling SPAKE2 pairing | Imported session, confirmation, persistence/replacement/removal, redaction and expiry/attempt-limit tests. UI code generation/display remains.  2–3 days (imported) |
| #62 | sibling mDNS and transport | Imported browse/address filtering and transports. Fake announcer test retained for Linux; production announcement wiring remains.  1–2 days (imported; announcement wiring excluded) |
| #61 | nxrt loader; GPL recompiler architecture | Different C ABI/language; no matching Rust Loader group. Existing C++ must remain until parity/boot checks.  0.5 day orientation; no Rust group |
| #60 | LAS iOS host; Android shell; libadrenotools | Useful platform references/dependency already used; no equivalent Rust Platform group imported.  0.5–1 day orientation; no Rust group |
| #59 | nxrt kernel; GPL memory references | Different runtime layouts/ABIs; no compatible ready Rust guest-memory group, especially Apple reservation semantics.  0.5 day orientation; no Rust group |
| #58 | nxrt kernel; GPL sibling/reference | Architecture orientation only. No matching Rust kernel/IPC/watch replacement.  0.5 day orientation; no Rust group |
| #57 | nxrt service/kernel design; GPL emulator | No same-ABI Rust Services group. GPL sources remain links.  0.5 day orientation; no Rust group |
| #56 | nxrt audio service; unlicensed Rust scaffold | C reference and incomplete/no-permission Rust code; no compatible finished Rust Audio group.  0 implementation days; reference only |
| #55 | SDL/nxrt/Wonder input; frontend ownership | Input/lifecycle research, but no matching Rust ABI replacement for hle_hid/input_script/touch_layout.  0.5 day lifecycle research; no Rust group |
| #54 | nxrt filesystem and upstream mod fetching | Transactional save/file design reference; no matching Rust Files group.  0.5 day orientation; no Rust group |
| #53 | nxrt file commits; save-relay; NHSE link | Host save I/O still needs Rust ABI port. NHSE format/encryption code is GPL and unnecessary for opaque sync.  0.5 day orientation; no Rust group |
| #52 | nxrt/LAS time services | Small C reference only. Existing hle_time/svc_time behavior and offsets must be preserved in the Rust port.  0.5 day orientation; no Rust group |
| #51 | GPL sibling runtime facade | No compatible peer Rust NX config parser/group found; local core::config is already present but not the runtime C ABI.  0; compatible peer group not found |
| #50 | GPL sibling staticlib/C ABI structure; unlicensed scaffold | Plan reference only. ACNH CMake build.rs integration and boot smoke harness are still absent.  0 implementation days; plan reference only |
| #49 | nxrt loader/dispatch; GPL C codegen; unlicensed Rust scaffolding | No ready compatible Rust driver with this repo’s Python byte-parity passes. Recompiler is still only a pass-list scaffold.  0; no matching Rust driver |

## Imported and adapted

1. Discovery and transports: sibling `hub.rs:27–285`, plus
   [hub_discover.rs:1–116](https://github.com/iannitram/totk-universal/blob/80148e16e60b5ca5d34ca87d3ef3360567490352/crates/core/tests/hub_discover.rs#L1-L116).
2. SPAKE2 pairing and persisted peers: `hub.rs:287–544`, plus
   [hub_pair.rs:1–344](https://github.com/iannitram/totk-universal/blob/80148e16e60b5ca5d34ca87d3ef3360567490352/crates/core/tests/hub_pair.rs#L1-L344).
   The permission test moved to `platform.rs` with the OS-specific file creation.
3. Authenticated resumable build transfer: `hub.rs:546–1136`, plus
   [hub_transfer.rs:1–671](https://github.com/iannitram/totk-universal/blob/80148e16e60b5ca5d34ca87d3ef3360567490352/crates/core/tests/hub_transfer.rs#L1-L671).

Adaptations: service `_acnh-universal._tcp.local.`; separate ACNH PAKE/transfer
domain labels and `ACNHHUB1` wire marker (not cross-compatible with TOTK);
ACNH bundle version/commit fields; preserved local address predicate and
bundle forbidden-file policy; device-owned `root` instead of sibling `profile`.
On Unix, new peer/chunk files start at mode 0600, through `platform`; the
permission test covers replacement/removal too. Optional pure-Rust dependencies
are behind `hub`. SPDX/source-pin headers and the full MIT notice are in
the touched source/tests and `NOTICE`.

Resolved optional dependencies are `mdns-sd 0.21.7`, `spake2 0.4.0`,
`hmac 0.12.1`, `serde 1.0.229`, `serde_json 1.0.154` and
`chacha20poly1305 0.11.0`. Each packaged Cargo manifest declares
`MIT OR Apache-2.0` (or the reversed expression); exact versions and registry
checksums are retained in `Cargo.lock`.

These APIs do not start a persistent hub, publish mDNS announcements, generate
or display pairing codes, wire app/CLI actions, exchange fleet records, transfer
game data, or run save-relay. `pull` returns a verified staged bundle; its caller
uses `bundle::install`. Only synthetic tests invoke install in this scan.
The 64 MiB test checks bounded protocol I/O; it is not a whole-process memory
benchmark. All gameplay, signing/cable installation, controller-family checks,
Game Mode sessions and real-island round trips remain owner-device work.

## Validation

See the ready issue text for exact commands and final exit codes. Tests
use no real game folder or save. On this Mac the real multicast discovery test
is intentionally Linux-only; deterministic resolved-record filtering and loopback
TCP tests run here. A successful Mac Rust build does not certify Windows,
Android, iOS or a native C++ game build.

Commands and exit codes (all builds capped at two jobs):

| Command / check | Exit |
|---|---|
| `cargo fmt --all --check` | 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | 0 |
| `cargo test --workspace` | 0 |
| `cargo test --workspace --all-features` | 0 |
| `cargo test -p acnh-core --all-features` | 0 |
| `bash tools/check_no_game_data.sh` (staged files) | 0 |
| `git diff --cached --check` | 0 |
| Coverage/full-pin/SPDX checks (Python) | 0 |
| Direct dependency packaged-licence check (Python) | 0 |

All-feature core tests: 176 passed, including 32 newly imported/adapted tests on
this Mac. Default-feature core tests: 140 passed. The 64 MiB transfer test takes
over 60 seconds in this debug build and passed. Cargo emitted the existing
unused `acnh-core` manifest dependency warning for the stub CLI; both clippy
commands still exited 0.

Full logs and the command ledger are in ignored `target/peer-scan/logs/`.
No C++/CMake/runtime source was changed, so no C++ game build was invoked.
No Windows/Android/iOS or actual-game/device success is claimed.
