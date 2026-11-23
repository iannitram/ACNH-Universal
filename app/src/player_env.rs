//! What the app hands the player at start: its program and environment (SPEC.md M1, M4; #171).
//! On the Mac the data root and its `logs/` are made first (the player opens its log with
//! `fopen` and makes no folder for it), and the player reads the game from inside the app.

use crate::build_actions::BuildActions;
use acnh_core::platform::Os;
use acnh_core::Result;
use std::path::{Path, PathBuf};

/// The player to start, the environment to start it with, and whether the build is missing.
#[derive(Debug, PartialEq, Eq)]
pub struct PlayerSetup {
    pub program: PathBuf,
    pub env: Vec<(String, String)>,
    /// No player, or no game for it to read: the Continue page opens in `BuildMissing`.
    pub build_missing: bool,
}

/// `<app>/Contents/Resources/game` for a launcher at `<app>/Contents/MacOS/<launcher>`.
fn own_game(launcher: &Path) -> Option<PathBuf> {
    let contents = launcher.parent()?.parent()?;
    Some(contents.join("Resources/game"))
}

/// Make the data root and its logs folder, then build the player's environment.
///
/// - Every OS: `NX_DATA_ROOT` is the data root.
/// - Mac: the data root and `logs/` are created; `NX_LOG` is `<data root>/logs/nx.log`;
///   `NX_GAME_ROOT` is the app's own `Contents/Resources/game` when it holds one; it wins over
///   a local prepare on the same Mac.
/// - Other OSes: `NX_GAME_ROOT` only when a local prepare names one, as before.
pub fn player_setup(builds: &BuildActions, launcher: &Path) -> Result<PlayerSetup> {
    let root = builds.dirs.data_root();
    let (program, mut game_root) = builds.launch_location(launcher);
    let mut env = vec![("NX_DATA_ROOT".to_string(), root.display().to_string())];
    if builds.os == Os::Mac {
        std::fs::create_dir_all(builds.dirs.logs())?;
        env.push((
            "NX_LOG".into(),
            builds.dirs.logs().join("nx.log").display().to_string(),
        ));
        if game_root.is_none() {
            game_root = own_game(launcher).filter(|game| game.is_dir());
        }
    }
    if let Some(game_root) = game_root {
        env.push(("NX_GAME_ROOT".into(), game_root.display().to_string()));
    }
    let build_missing = !builds.installed(&program);
    Ok(PlayerSetup {
        program,
        env,
        build_missing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use acnh_core::platform::{AppDirs, Vars};

    fn setup(root: &Path, with_game: bool, with_player: bool) -> (BuildActions, PathBuf) {
        let vars = Vars {
            home: root.join("home"),
            data: root.join("home/Library/Application Support"),
            config: root.join("home/Library/Application Support"),
        };
        let builds = BuildActions {
            dirs: AppDirs::for_os(Os::Mac, &vars),
            os: Os::Mac,
        };
        let contents = root.join("ACNH-Universal.app/Contents");
        std::fs::create_dir_all(contents.join("MacOS")).unwrap();
        let launcher = contents.join("MacOS/acnh-app");
        std::fs::write(&launcher, b"").unwrap();
        if with_player {
            std::fs::write(contents.join("MacOS/acnh"), b"").unwrap();
        }
        if with_game {
            std::fs::create_dir_all(contents.join("Resources/game/analysis")).unwrap();
            std::fs::write(contents.join("Resources/game/analysis/main.bin"), b"").unwrap();
        }
        (builds, launcher)
    }

    fn value<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
        env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn the_mac_app_starts_the_player_with_its_folders() {
        let root = tempfile::tempdir().unwrap();
        let (builds, launcher) = setup(root.path(), true, true);
        let setup = player_setup(&builds, &launcher).unwrap();
        let data = root
            .path()
            .join("home/Library/Application Support/ACNH-Universal");
        // Compare as paths: on a Windows runner the joined segments keep their own separators.
        let path = |key| value(&setup.env, key).map(std::path::PathBuf::from);
        assert_eq!(path("NX_DATA_ROOT"), Some(data.clone()));
        assert_eq!(path("NX_LOG"), Some(data.join("logs/nx.log")));
        assert_eq!(
            path("NX_GAME_ROOT"),
            Some(
                root.path()
                    .join("ACNH-Universal.app/Contents/Resources/game")
            )
        );
        assert!(data.is_dir() && data.join("logs").is_dir());
        assert!(!setup.build_missing);
    }

    #[test]
    fn the_contained_app_wins_over_a_local_prepare_on_the_hub() {
        let root = tempfile::tempdir().unwrap();
        let (builds, launcher) = setup(root.path(), true, true);
        // The hub that prepared this app: a game folder, a prepare and an installed player.
        std::fs::create_dir_all(&builds.dirs.config).unwrap();
        std::fs::write(
            builds.dirs.config.join("launcher.ini"),
            "game_folder=/games/acnh\n",
        )
        .unwrap();
        std::fs::create_dir_all(builds.dirs.data.join("prepare/analysis")).unwrap();
        std::fs::write(builds.dirs.data.join("prepare/analysis/main.bin"), b"").unwrap();
        let installed = acnh_core::assemble::player(Os::Mac, Path::new(""), &builds.dirs.install());
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(&installed, b"").unwrap();

        let setup = player_setup(&builds, &launcher).unwrap();
        assert_eq!(setup.program, launcher.with_file_name("acnh"));
        assert_eq!(
            value(&setup.env, "NX_GAME_ROOT").map(PathBuf::from),
            Some(
                root.path()
                    .join("ACNH-Universal.app/Contents/Resources/game")
            )
        );
        assert!(!setup.build_missing);
    }

    #[test]
    fn an_app_without_a_game_folder_opens_build_missing() {
        let root = tempfile::tempdir().unwrap();
        for with_player in [true, false] {
            let (builds, launcher) = setup(root.path(), false, with_player);
            let setup = player_setup(&builds, &launcher).unwrap();
            assert!(setup.build_missing);
            assert_eq!(value(&setup.env, "NX_GAME_ROOT"), None);
            assert!(builds.dirs.logs().is_dir());
        }
    }

    #[test]
    fn other_systems_make_no_folders_and_set_no_log() {
        let root = tempfile::tempdir().unwrap();
        let vars = Vars {
            home: root.path().into(),
            data: root.path().join("data"),
            config: root.path().join("config"),
        };
        let builds = BuildActions {
            dirs: AppDirs::for_os(Os::Linux, &vars),
            os: Os::Linux,
        };
        let setup = player_setup(&builds, &root.path().join("acnh-app")).unwrap();
        assert_eq!(
            value(&setup.env, "NX_DATA_ROOT"),
            builds.dirs.data_root().to_str()
        );
        assert_eq!(value(&setup.env, "NX_LOG"), None);
        assert!(!root.path().join("data").exists());
    }
}
