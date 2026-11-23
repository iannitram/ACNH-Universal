//! Prepare and update work requested from Continue.
use acnh_core::platform::{AppDirs, Os};
use acnh_core::{
    assemble, backup, bundle, config::Config, gamefolder, hub, prepare, recipe, saves, update,
    Error, Result,
};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct BuildActions {
    pub dirs: AppDirs,
    pub os: Os,
}

impl BuildActions {
    pub fn game_folder(&self) -> Result<Option<PathBuf>> {
        let path = self.dirs.config.join("launcher.ini");
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Config::parse(&text)?
                .get("game_folder")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    pub fn installed(&self, player: &Path) -> bool {
        if !player.is_file() {
            return false;
        }
        if self.game_folder().ok().flatten().is_some() {
            return true;
        }
        std::fs::read_to_string(self.dirs.config.join("installed-bundle.txt"))
            .ok()
            .and_then(|text| bundle::from_text(&text).ok())
            .is_some_and(|manifest| {
                manifest.target == self.os
                    && bundle::verify(&manifest, &self.dirs.install()).is_ok()
            })
    }
    pub fn build(&self, report: &dyn Fn(String)) -> Result<()> {
        if let Some(folder) = self.game_folder()? {
            let game = gamefolder::check(&folder)?;
            let config = match std::fs::read_to_string(self.dirs.data_root().join("config.ini")) {
                Ok(text) => Config::parse(&text)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
                Err(e) => return Err(e.into()),
            };
            prepare_targets(self.os, report, &mut |target, progress| {
                let request = prepare::Request {
                    game: game.clone(),
                    out: if target == self.os {
                        self.dirs.data.join("prepare")
                    } else {
                        self.dirs.data.join("prepare-targets").join(target.key())
                    },
                    target,
                    jobs: 0,
                    source: None,
                    fan_mods: prepare::FanMods {
                        ptbr: config.enabled("mods.ptbr"),
                        speedfix: config.enabled("mods.speedfix"),
                    },
                };
                let out = prepare::prepare(&request, progress)?;
                if target != self.os {
                    return Err(Error::Unsupported(format!(
                        "no delivery for {} yet",
                        target.key()
                    )));
                }
                assemble::assemble(target, &out.join("build"), &self.dirs.install())?;
                Ok(())
            })
        } else {
            report("Finding paired hub".into());
            let peers = hub::read_peers(&self.dirs)?;
            let configured = std::fs::read_to_string(self.dirs.config.join("launcher.ini"))
                .ok()
                .and_then(|text| Config::parse(&text).ok())
                .and_then(|config| {
                    let name = config.get("hub_name")?.to_owned();
                    let address = config.get("hub_address")?.parse().ok()?;
                    peers
                        .iter()
                        .any(|p| p.device == name)
                        .then_some((name, address))
                });
            let discovered = hub::discover(3000)
                .unwrap_or_default()
                .into_iter()
                .find(|found| {
                    found.targets.contains(&self.os) && peers.iter().any(|p| p.device == found.hub)
                })
                .map(|found| (found.hub, std::net::SocketAddr::new(found.addr, found.port)));
            let (name, address) = discovered
                .or(configured)
                .ok_or_else(|| Error::Hub("No paired hub found".into()))?;
            let id = crate::first_run::device(&self.dirs)?;
            report("Pulling build".into());
            self.install_pulled(|staged| {
                hub::pull_hub(address, &self.dirs, &id, &name, self.os, staged)
            })
        }
    }
    fn install_pulled(&self, pull: impl FnOnce(&Path) -> Result<bundle::Bundle>) -> Result<()> {
        std::fs::create_dir_all(&self.dirs.data)?;
        let staged = tempfile::Builder::new()
            .prefix(".continue-")
            .tempdir_in(&self.dirs.data)?;
        let manifest = pull(staged.path())?;
        bundle::install(&manifest, staged.path(), &self.dirs.install())?;
        std::fs::create_dir_all(&self.dirs.config)?;
        std::fs::write(
            self.dirs.config.join("installed-bundle.txt"),
            bundle::to_text(&manifest),
        )?;
        Ok(())
    }
    pub fn update(&self, report: &dyn Fn(String)) -> Result<()> {
        self.update_with(&mut || self.build(report))
    }
    fn update_with(&self, rebuild: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        update::update(
            &saves::save_dir(&self.dirs.data_root()),
            &saves::backups_dir(&self.dirs.data_root()),
            &backup::stamp(now),
            rebuild,
        )
        .map(|_| ())
    }
}

type PrepareTarget<'a> = dyn FnMut(Os, &mut dyn FnMut(recipe::Stage)) -> Result<()> + 'a;

fn prepare_targets(os: Os, report: &dyn Fn(String), run: &mut PrepareTarget<'_>) -> Result<()> {
    for &target in recipe::serves(recipe::hub_for(os)) {
        let mut stage = String::new();
        let result = run(target, &mut |s| {
            stage = s.key().into();
            report(stage.clone());
        });
        match result {
            Ok(()) => {}
            Err(Error::Unsupported(_)) if target != os => {
                report(format!("{} is not supported yet; skipped", target.key()))
            }
            Err(e) => {
                return Err(Error::Build(format!(
                    "{}: {e}",
                    if stage.is_empty() { "prepare" } else { &stage }
                )))
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use acnh_core::platform::Vars;

    #[test]
    fn prepares_stages_in_order_and_shows_unsupported_target() {
        let messages = std::cell::RefCell::new(Vec::new());
        let report = |message| messages.borrow_mut().push(message);
        prepare_targets(Os::Mac, &report, &mut |target, progress| {
            if target == Os::Ios {
                return Err(Error::Unsupported("not ready".into()));
            }
            for stage in recipe::STAGES {
                progress(stage);
            }
            Ok(())
        })
        .unwrap();
        let mut expected: Vec<String> = recipe::STAGES.iter().map(|s| s.key().into()).collect();
        expected.push("ios is not supported yet; skipped".into());
        assert_eq!(*messages.borrow(), expected);
    }

    #[test]
    fn failed_stage_is_named() {
        let result = prepare_targets(Os::Windows, &|_| {}, &mut |_, progress| {
            progress(recipe::Stage::Build);
            Err(Error::Build("failed".into()))
        });
        assert!(result.unwrap_err().to_string().contains("build"));
    }

    #[test]
    fn update_backs_up_before_rebuild_and_names_backup_on_failure() {
        let root = tempfile::tempdir().unwrap();
        let vars = Vars {
            home: root.path().into(),
            data: root.path().join("data"),
            config: root.path().join("config"),
        };
        let actions = BuildActions {
            dirs: AppDirs::new(&vars),
            os: Os::Linux,
        };
        let save = saves::save_dir(&actions.dirs.data_root());
        std::fs::create_dir_all(&save).unwrap();
        std::fs::write(save.join("main.dat"), b"island").unwrap();
        std::fs::write(save.join("mainHeader.dat"), b"header").unwrap();
        let backups = saves::backups_dir(&actions.dirs.data_root());
        let result = actions.update_with(&mut || {
            assert!(std::fs::read_dir(&backups).unwrap().next().is_some());
            Err(Error::Build("build failed".into()))
        });
        assert!(result.unwrap_err().to_string().contains("restore"));
        assert_eq!(std::fs::read(save.join("main.dat")).unwrap(), b"island");
    }

    #[test]
    fn pull_installs_sealed_bundle_and_preserves_device_folders() {
        let root = tempfile::tempdir().unwrap();
        let vars = Vars {
            home: root.path().into(),
            data: root.path().join("data"),
            config: root.path().join("config"),
        };
        let actions = BuildActions {
            dirs: AppDirs::new(&vars),
            os: Os::Linux,
        };
        let build = root.path().join("builds/one");
        std::fs::create_dir_all(&build).unwrap();
        std::fs::write(build.join("acnh"), b"new player").unwrap();
        let manifest = bundle::manifest(&build, Os::Linux, "1", "abc").unwrap();
        std::fs::write(build.join("bundle.txt"), bundle::to_text(&manifest)).unwrap();
        for folder in ["root", "logs"] {
            let dir = actions.dirs.install().join(folder);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("keep"), folder).unwrap();
        }
        let peer = hub::Peer {
            device: "test".into(),
            key_hex: "42".repeat(32),
        };
        let (mut server, mut client) = hub::MemoryTransport::pair();
        std::thread::scope(|scope| {
            let server = scope
                .spawn(|| hub::serve(&mut server, &peer, &root.path().join("builds"), Os::Linux));
            actions
                .install_pulled(|staged| hub::pull(&mut client, &peer, Os::Linux, staged))
                .unwrap();
            server.join().unwrap().unwrap();
        });
        assert_eq!(
            std::fs::read(actions.dirs.install().join("acnh")).unwrap(),
            b"new player"
        );
        for folder in ["root", "logs"] {
            assert_eq!(
                std::fs::read_to_string(actions.dirs.install().join(folder).join("keep")).unwrap(),
                folder
            );
        }
        assert!(actions.installed(&actions.dirs.install().join("acnh")));
    }

    #[test]
    fn installed_requires_the_manifest_to_match_on_a_client() {
        let root = tempfile::tempdir().unwrap();
        let vars = Vars {
            home: root.path().into(),
            data: root.path().join("data"),
            config: root.path().join("config"),
        };
        let actions = BuildActions {
            dirs: AppDirs::new(&vars),
            os: Os::Linux,
        };
        let install = actions.dirs.install();
        std::fs::create_dir_all(&install).unwrap();
        let player = install.join("acnh");
        std::fs::write(&player, b"player").unwrap();
        assert!(!actions.installed(&player));
        let manifest = bundle::manifest(&install, Os::Linux, "1", "abc").unwrap();
        std::fs::create_dir_all(&actions.dirs.config).unwrap();
        std::fs::write(
            actions.dirs.config.join("installed-bundle.txt"),
            bundle::to_text(&manifest),
        )
        .unwrap();
        assert!(actions.installed(&player));
        std::fs::write(&player, b"modified").unwrap();
        assert!(!actions.installed(&player));
    }
}
