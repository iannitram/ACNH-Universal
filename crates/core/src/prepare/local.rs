//! The local build: one set of steps that turns a checked game folder into a player on this
//! device, used by the CLI's `build`, the launcher's first run and its Build action alike
//! (#348). It reads the two settings files, makes the [`Request`], and after the prepare
//! assembles the player, packs the contained folder and remembers the game folder.

use super::{
    app_out_dir_for, assemble, delivery_folder, delivery_subfolder, pack_folder, FanMods, Request,
};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::gamefolder::GameFolder;
use crate::platform::{AppDirs, Os};
use std::path::{Path, PathBuf};

/// The settings file at `path`; a missing file is empty settings.
pub fn read_settings(path: &Path) -> Result<Config> {
    match std::fs::read_to_string(path) {
        Ok(text) => Config::parse(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(error) => Err(Error::Io(format!(
            "cannot read {}: {error}",
            path.display()
        ))),
    }
}

/// The request to prepare `target` into `out`. `deliver` says whether the Apple targets'
/// deliverable goes to the Dropbox delivery folder (it does on the Mac, and for the CLI).
pub fn local_request(
    dirs: &AppDirs,
    home: &Path,
    game: GameFolder,
    target: Os,
    out: PathBuf,
    deliver: bool,
) -> Result<Request> {
    let config = read_settings(&dirs.data_root().join("config.ini"))?;
    let launcher = read_settings(&dirs.config.join("launcher.ini"))?;
    Ok(Request {
        game,
        out,
        target,
        jobs: 0,
        source: None,
        fan_mods: FanMods {
            ptbr: config.enabled("mods.ptbr"),
            speedfix: config.enabled("mods.speedfix"),
        },
        delivery: delivery_subfolder(target)
            .filter(|_| deliver)
            .map(|subfolder| delivery_folder(&launcher, home, subfolder)),
        install: (target == Os::Mac).then(|| dirs.install()),
    })
}

/// What follows a prepare that made `out` for `target`: the bare player becomes a
/// self-contained one in the install folder, and on this device's own Linux or Windows the
/// contained folder is packed. The Apple targets have nothing to assemble here (their
/// deliverable is in Dropbox). Returns the assembled player.
pub fn finish_local_build(
    dirs: &AppDirs,
    home: &Path,
    launcher: &Config,
    target: Os,
    out: &Path,
) -> Result<Option<PathBuf>> {
    if matches!(target, Os::Ios | Os::Mac) {
        return Ok(None);
    }
    let player = assemble(target, &out.join("build"), &dirs.install())?;
    if target == Os::current() && matches!(target, Os::Linux | Os::Windows) {
        let local_data = dirs.data.parent().unwrap_or(&dirs.data);
        let app_dir = app_out_dir_for(target, launcher, local_data, home);
        pack_folder(&std::env::current_exe()?, &dirs.install(), out, &app_dir)?;
    }
    Ok(Some(player))
}

/// Remember `game` as this device's game folder (`launcher.ini`'s `game_folder`).
pub fn save_game_folder(dirs: &AppDirs, game: &Path) -> Result<()> {
    let value = game.to_string_lossy();
    if value.contains(['\r', '\n']) {
        return Err(Error::Config("game_folder must be a single line".into()));
    }
    let path = dirs.config.join("launcher.ini");
    let mut launcher = read_settings(&path)?;
    launcher.set("game_folder", &value);
    std::fs::create_dir_all(&dirs.config)?;
    crate::platform::atomic_write(&path, launcher.to_text().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::Version;

    fn dirs(root: &Path) -> AppDirs {
        AppDirs {
            config: root.join("config"),
            data: root.join("data"),
        }
    }

    fn game() -> GameFolder {
        GameFolder {
            root: PathBuf::from("game"),
            exefs: PathBuf::from("game/exefs"),
            romfs: PathBuf::from("game/romfs"),
            aoc: Vec::new(),
            from_importer: false,
            version: Version::V3_0_2,
        }
    }

    #[test]
    fn a_missing_settings_file_is_empty() {
        let temp = tempfile::tempdir().unwrap();
        let config = read_settings(&temp.path().join("none.ini")).unwrap();
        assert_eq!(config.get("game_folder"), None);
    }

    #[test]
    fn the_game_folder_is_saved_beside_the_other_launcher_settings() {
        let temp = tempfile::tempdir().unwrap();
        let dirs = dirs(temp.path());
        std::fs::create_dir_all(&dirs.config).unwrap();
        std::fs::write(dirs.config.join("launcher.ini"), "hub_name=kitchen\n").unwrap();
        save_game_folder(&dirs, Path::new("/example/game")).unwrap();
        let launcher = read_settings(&dirs.config.join("launcher.ini")).unwrap();
        assert_eq!(launcher.get("game_folder"), Some("/example/game"));
        assert_eq!(launcher.get("hub_name"), Some("kitchen"));
        assert!(save_game_folder(&dirs, Path::new("a\nb")).is_err());
    }

    #[test]
    fn the_request_follows_the_two_settings_files() {
        let temp = tempfile::tempdir().unwrap();
        let dirs = dirs(temp.path());
        std::fs::create_dir_all(dirs.data_root()).unwrap();
        std::fs::write(dirs.data_root().join("config.ini"), "mods.ptbr=true\n").unwrap();
        let home = temp.path().join("home");
        let out = temp.path().join("out");
        let request = local_request(&dirs, &home, game(), Os::Linux, out.clone(), true).unwrap();
        assert_eq!(request.out, out);
        assert!(request.fan_mods.ptbr);
        assert!(!request.fan_mods.speedfix);
        assert_eq!(request.delivery, None);
        assert_eq!(request.install, None);
        let apple = local_request(&dirs, &home, game(), Os::Ios, out.clone(), true).unwrap();
        assert_eq!(
            apple.delivery,
            Some(delivery_folder(&Config::default(), &home, "ios"))
        );
        let held_back = local_request(&dirs, &home, game(), Os::Ios, out, false).unwrap();
        assert_eq!(held_back.delivery, None);
    }

    #[test]
    fn the_apple_targets_have_nothing_to_assemble_here() {
        let temp = tempfile::tempdir().unwrap();
        let dirs = dirs(temp.path());
        let done = finish_local_build(&dirs, temp.path(), &Config::default(), Os::Ios, temp.path())
            .unwrap();
        assert_eq!(done, None);
    }
}
