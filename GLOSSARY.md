# Glossary

The project's own words, one line each. Code, issues and specs use these words.

| Term | Means | Not to be confused with |
|---|---|---|
| game folder | My own prepared game: `exefs/` (`main`, `sdk`, `subsdk0`, `main.npdm`), `romfs/` with the update applied, `aoc/` when present (`gamefolder::GameFolder`) | a dump file (NSP, XCI: never the input) |
| build id | The first 16 bytes of the module id in `exefs/main`'s NSO header, as 32 upper-case hex digits (`nso::build_id`); it names the game version | our build's `build_id` (a bundle's hash) |
| version gate | The refusal of any build id other than 3.0.2, 3.0.3, 2.0.7 and 1.0.0 (`version::from_build_id`) | upstream's version table, which builds any |
| native 60 | Upstream's 60 FPS patch, on 3.0.2 and 3.0.3 only (`Version::native60`) | a frame cap in `config.ini` |
| prepare | Turning a game folder into a build for one device family on a hub, stage by stage (`prepare::prepare`) | an install |
| stage | One step of a prepare: tools, game, version, recompile, shaders, fonts, build (`recipe::Stage`) | a roadmap stage |
| build | What a prepare leaves: the player and, beside it, the game data made from my folder | a bundle |
| player | The executable that runs the game (`acnh`, `acnh.exe`, `ACNH.app`, `ACNH.ipa`, `acnh.apk`: `recipe::output`) | the launcher |
| game library | The recompiled game code (`gen/` plus `games/acnh/`) built as a library the player loads through `nx_game_abi()`; game data, never bundled | the runtime |
| hub | One of the three machines that hold the game folder and build for others: the Strix Halo tablet, the desktop, the Mac (`recipe::Hub`) | a server |
| bundle | The list of a build's player files with sizes and SHA-256, served by a hub and checked before install (`bundle::Bundle`); never game data | the game data transfer |
| install | `<data>/acnh-universal/install/`: the installed build, with the device-owned `root/` and `logs/` kept across installs | the game folder |
| data root | `install/root`, upstream's `NX_DATA_ROOT`: `config.ini`, `mods/`, the game data, and `user/` with the island | the settings folder (`launcher.ini`) |
| island | My save: the whole `<data root>/user/device_save/` folder, the folder save-relay syncs (`saves::save_dir`) | a backup |
| backup | A copy of the island's synced files in `user/backups/<YYYYmmdd-HHMMSS>/`, the 10 newest kept (`backup`) | save-relay's history |
| save card | The island's card on the Continue screen: save time, device, play time, picture (`savecard::SaveCard`) | the island's name (after 1.0) |
| device profile | The settings a device starts the game with, written as `config.ini` entries (`profile::DeviceProfile`) | the data root |
| fleet record | What one launcher reports to its hubs after a sync: device, OS, version, build, last sync, free space (`fleet::DeviceRecord`) | a bundle |
| drift | A different game version or build from the device that made the latest save, warned before play (`fleet::Drift`) | a sync conflict |
| Continue screen | The launcher's first page: the island's card and one button to sync and play | the in-game overlay |
| upstream | Nycolazs/ACNHRecomp, which this repo was copied from; the `upstream` remote, fetch only | a peer |
| upstream-sync | An issue that ports an upstream change by hand into code of ours that is already Rust | a daily merge |
