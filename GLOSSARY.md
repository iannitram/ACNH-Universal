# Glossary

The project's own words, one line each. Code, issues and specs use these words.

| Term | Means | Not to be confused with |
|---|---|---|
| game folder | My own prepared game: `exefs/` (`main`, `sdk`, `subsdk0`, `main.npdm`), `romfs/` with the update applied, `aoc/` when present (`gamefolder::GameFolder`) | a dump file (NSP, XCI: never the input) |
| build id | The first 16 bytes of the module id in `exefs/main`'s NSO header, as 32 upper-case hex digits (`nso::build_id`); it names the game version | our build's `build_id` (a bundle's hash) |
| version gate | The refusal of any build id other than 3.0.2, 3.0.3, 2.0.7 and 1.0.0 (`version::from_build_id`) | upstream's version table, which builds any |
| native 60 | Upstream's 60 FPS patch, on 3.0.2 and 3.0.3 only (`Version::native60`) | a frame cap in `config.ini`, and the SpeedFix |
| SpeedFix | The fan mod (CC BY-NC 4.0, `prepare/recomp/acnh/speedfix-*.pchtxt`) that makes the game logic run at 60 fps (`mods.speedfix`, `video.frame_rate = 60`); off by default with a switch, on in my own builds (M16) | native 60 |
| prepare | Turning a game folder into a build for one device family on a hub, stage by stage (`prepare::prepare`) | an install |
| stage | One step of a prepare: tools, game, version, recompile, shaders, fonts, build (`recipe::Stage`) | a roadmap stage |
| build | What a prepare leaves: the player and, beside it, the game data made from my folder | a bundle |
| player | The executable that runs the game (`acnh`, `acnh.exe`, `ACNH.app`, `ACNH.ipa`, `acnh.apk`: `recipe::output`), the part of a contained app that is not the game | the launcher |
| contained app | One app per platform that holds the game made from my own game folder (M18): the Mac app, the iPhone/iPad `.ipa`, the Android `.apk` that keeps the game in its private storage, the desktop folder built on the device by its own launcher | a bundle |
| small .ipa | The iPhone/iPad app with no game inside: the game arrives by in-app download (`tools/package_ios.sh --small`, issue #231); the big `.ipa` (`--game`) has it inside | a bundle |
| in-app download | The first start of the Android app or the small `.ipa` fetches the game from my own Dropbox, in the background and resumable, hash-checked against `manifest.json` (`acnh_core::appgame`; M18) | a hub pull |
| delivery | What Prepare leaves for a device: the app (or the player) in `Dropbox/_transfers/ACNH-Universal/<os>/` and, for the small apps, the game folder `acnh-universal-game/<os>/` that the in-app download reads; always my own Dropbox, never the repo | a bundle |
| game library | The recompiled game code (`gen/` plus `games/acnh/`) built as a library the player loads through `nx_game_abi()`; game data, never bundled | the runtime |
| hub | One of the three machines that hold the game folder and build for others: the Strix Halo tablet, the desktop, the Mac (`recipe::Hub`) | a server |
| bundle | The list of a build's player files with sizes and SHA-256, served by a hub and checked before install (`bundle::Bundle`); never game data | the game data transfer |
| install | `<data>/acnh-universal/install/`: the installed build on a desktop, with the device-owned `root/` and `logs/` kept across installs (an Apple or Android contained app keeps its data in its own folders, not here) | the game folder |
| data root | `install/root` on a desktop, upstream's `NX_DATA_ROOT`: `config.ini`, `mods/`, the game data, and `user/` with the island; on the Mac `~/Library/Application Support/ACNH-Universal`, on the iPad the app's Documents folder, on Android the app's own folders | the settings folder (`launcher.ini`) |
| island | My save: the whole `<data root>/user/device_save/` folder, the folder save-relay syncs (`saves::save_dir`) | a backup |
| backup | A copy of the island's synced files in `user/backups/<YYYYmmdd-HHMMSS>/`, the 10 newest kept (`backup`) | save-relay's history |
| save card | The island's card on the Continue screen: save time, device, play time, picture (`savecard::SaveCard`) | the island's name (after 1.0) |
| device profile | The settings a device starts the game with, written as `config.ini` entries (`profile::DeviceProfile`) | the data root |
| fleet record | What one launcher reports to its hubs after a sync: device, OS, version, build, last sync, free space (`fleet::DeviceRecord`) | a bundle |
| drift | A different game version or build from the device that made the latest save, warned before play (`fleet::Drift`) | a sync conflict |
| Continue screen | The launcher's first page: the island's card and one button to sync and play | the in-game overlay |
| upstream | Nycolazs/ACNHRecomp, which this repo was copied from; the `upstream` remote, fetch only | a peer |
| upstream-sync | An issue that ports an upstream change by hand into code of ours that is already Rust | a daily merge |
