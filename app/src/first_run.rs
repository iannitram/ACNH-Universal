//! Runs first-run requests off the window thread.
use crate::vm::first_run::{Event, FirstRunVm, Request, Step};
use crate::vm::Nav;
use acnh_core::platform::{AppDirs, Os, Vars};
use acnh_core::{bundle, gamefolder, hub, prepare};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct FirstRun {
    pub vm: FirstRunVm,
    dirs: AppDirs,
    vars: Vars,
    sender: Sender<(u64, Event)>,
    receiver: Receiver<(u64, Event)>,
    generation: u64,
    active: Option<Request>,
    cancelled: Arc<AtomicU8>,
    cancelling_build: bool,
}
impl FirstRun {
    pub fn new(vars: Vars) -> Self {
        let dirs = AppDirs::new(&vars);
        let (sender, receiver) = mpsc::channel();
        let mut vm = FirstRunVm::new(vars.home.clone());
        vm.steam_applicable = Os::current() == Os::Linux
            && acnh_core::steam::userdata_roots(&vars)
                .iter()
                .any(|p| p.is_dir());
        Self {
            vm,
            dirs,
            vars,
            sender,
            receiver,
            generation: 0,
            active: None,
            cancelled: Arc::new(AtomicU8::new(0)),
            cancelling_build: false,
        }
    }
    pub fn game_root(&self) -> PathBuf {
        self.dirs.data.join("prepare")
    }
    pub fn needed(&self) -> bool {
        if self.dirs.config.join("first-run-complete").is_file() {
            return false;
        }
        let launcher = std::env::current_exe().unwrap_or_default();
        let player = acnh_core::assemble::player(Os::current(), &launcher, &self.dirs.install());
        if !player.is_file() {
            return true;
        }
        let prepared_here = self.dirs.data_root().join("analysis/main.bin").is_file();
        let packaged = player
            .parent()
            .and_then(|p| p.parent())
            .is_some_and(|p| p.join("Resources/game/analysis/main.bin").is_file());
        !(prepared_here || packaged)
    }
    pub fn complete(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dirs.config)?;
        std::fs::write(self.dirs.config.join("first-run-complete"), b"1\n")
    }
    pub fn nav(&mut self, nav: Nav) {
        if nav == Nav::Back && self.vm.busy {
            match self.active.as_ref() {
                Some(Request::Build(_)) => {
                    if self
                        .cancelled
                        .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                    {
                        self.cancelling_build = true;
                        self.vm.message =
                            "Cancelling after the current preparation finishes.".into();
                    } else {
                        self.vm.message = "Installing the completed build; please wait.".into();
                    }
                }
                Some(Request::Connect(_)) | Some(Request::Discover) => {
                    self.generation += 1;
                    self.active = None;
                    self.vm.busy = false;
                    self.vm.handle(Nav::Back);
                }
                _ => {}
            }
            return;
        }
        self.vm.handle(nav);
        self.dispatch();
    }
    pub fn click(&mut self, index: usize) {
        self.vm.activate(index);
        self.dispatch();
    }
    pub fn pump(&mut self) -> bool {
        let events: Vec<_> = self.receiver.try_iter().collect();
        let any = !events.is_empty();
        for (generation, event) in events {
            if generation != self.generation {
                continue;
            }
            let final_event = !matches!(event, Event::Stage(_));
            if self.cancelling_build {
                if final_event {
                    self.cancelling_build = false;
                    self.active = None;
                    self.vm.busy = false;
                    self.vm.message = "Preparation cancelled; finished stages are kept.".into();
                    self.vm.step = Step::FindGame;
                    self.vm.focus = 0;
                }
            } else {
                if final_event {
                    self.active = None;
                }
                self.vm.event(event);
            }
        }
        self.dispatch();
        any
    }
    pub fn done(&self) -> bool {
        self.vm.step == Step::Done
    }
    fn dispatch(&mut self) {
        let Some(request) = self.vm.take_request() else {
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        self.active = Some(request.clone());
        self.cancelled = Arc::new(AtomicU8::new(0));
        let cancelled = Arc::clone(&self.cancelled);
        let dirs = self.dirs.clone();
        let vars = self.vars.clone();
        let sender = self.sender.clone();
        thread::spawn(move || {
            let event = run(request, &dirs, &vars, &sender, generation, &cancelled);
            let _ = sender.send((generation, event));
        });
    }
}
fn device(dirs: &AppDirs) -> acnh_core::Result<String> {
    let path = dirs.config.join("hub-device");
    if let Ok(name) = std::fs::read_to_string(&path) {
        return Ok(name.trim().to_owned());
    }
    std::fs::create_dir_all(&dirs.config)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = format!("device-{stamp:x}-{:x}", std::process::id());
    std::fs::write(path, &name)?;
    Ok(name)
}
fn safe_error(error: acnh_core::Error, home: &Path) -> String {
    match error {
        acnh_core::Error::WrongVersion { .. } => {
            "Supported versions are 3.0.2, 3.0.3, 2.0.7, and 1.0.0.".into()
        }
        acnh_core::Error::BadGameFolder(reason) if reason.contains(" is missing") => reason,
        other => prepare::redact(&other.to_string(), home),
    }
}
fn run(
    request: Request,
    dirs: &AppDirs,
    vars: &Vars,
    sender: &Sender<(u64, Event)>,
    generation: u64,
    cancelled: &AtomicU8,
) -> Event {
    match request {
        Request::Folders(root) => {
            let mut folders = std::fs::read_dir(root)
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| {
                    e.file_type()
                        .ok()
                        .filter(|t| t.is_dir() && !t.is_symlink())
                        .map(|_| e.path())
                })
                .collect::<Vec<PathBuf>>();
            folders.sort();
            Event::Folders(folders)
        }
        Request::Check(path) => Event::Checked(
            gamefolder::check(&path)
                .map(|g| g.version.name().into())
                .map_err(|e| safe_error(e, &vars.home)),
        ),
        Request::Discover => Event::Hubs(
            hub::discover(3000)
                .map(|hubs| {
                    hubs.into_iter()
                        .filter(|hub| hub.targets.contains(&Os::current()))
                        .collect()
                })
                .map_err(|e| safe_error(e, &vars.home)),
        ),
        Request::Connect(addr) => Event::Connected(
            hub::TcpTransport::connect(addr)
                .map(|_| ())
                .map_err(|_| "Hub could not be reached".into()),
        ),
        Request::Pair(addr, name, code) => Event::Paired(
            device(dirs)
                .and_then(|id| {
                    hub::pair_hub(addr, dirs, &id, &name, &code, Os::current()).map(|_| ())
                })
                .map_err(|_| "Codes did not match".into()),
        ),
        Request::Pull(addr, name) => Event::Finished(
            (|| {
                std::fs::create_dir_all(&dirs.data)?;
                let staged = tempfile::Builder::new()
                    .prefix(".first-run-")
                    .tempdir_in(&dirs.data)?;
                let id = device(dirs)?;
                let bundle = hub::pull_hub(addr, dirs, &id, &name, Os::current(), staged.path())?;
                bundle::install(&bundle, staged.path(), &dirs.install())?;
                hub::report_hub_build(addr, dirs, &id, &name, &bundle)?;
                Ok(())
            })()
            .map_err(|e| safe_error(e, &vars.home)),
        ),
        Request::Build(path) => Event::Finished(
            (|| {
                let config = match std::fs::read_to_string(dirs.data_root().join("config.ini")) {
                    Ok(text) => acnh_core::config::Config::parse(&text)?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
                    Err(e) => return Err(e.into()),
                };
                let request = prepare::Request {
                    game: gamefolder::check(&path)?,
                    out: dirs.data.join("prepare"),
                    target: Os::current(),
                    jobs: 0,
                    source: None,
                    fan_mods: prepare::FanMods {
                        ptbr: config.enabled("mods.ptbr"),
                        speedfix: config.enabled("mods.speedfix"),
                    },
                };
                let out = prepare::prepare(&request, &mut |stage| {
                    let _ = sender.send((generation, Event::Stage(stage)));
                })?;
                if cancelled
                    .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst)
                    .is_err()
                {
                    return Err(acnh_core::Error::Build("preparation cancelled".into()));
                }
                prepare::assemble(Os::current(), &out.join("build"), &dirs.install())?;
                Ok(())
            })()
            .map_err(|e| safe_error(e, &vars.home)),
        ),
        Request::AddSteam => Event::Steam(add_steam(vars).map_err(|e| safe_error(e, &vars.home))),
    }
}

fn add_steam(vars: &Vars) -> acnh_core::Result<()> {
    use acnh_core::steam::{self, Shortcut};
    if Os::current() != Os::Linux {
        return Err(acnh_core::Error::Steam(
            "Steam setup is available on Linux devices".into(),
        ));
    }
    let userdata = steam::userdata_roots(vars)
        .into_iter()
        .find(|p| p.is_dir())
        .ok_or_else(|| acnh_core::Error::Steam("No Steam user folder found".into()))?;
    let exe = std::env::current_exe()?;
    let shortcut = Shortcut {
        name: "Animal Crossing: New Horizons".into(),
        exe: exe.display().to_string(),
        start_dir: exe.parent().unwrap_or(Path::new(".")).display().to_string(),
        launch_options: "play".into(),
        icon: "assets/acnhrecomp.png".into(),
    };
    let running = || {
        std::process::Command::new("pgrep")
            .arg("-x")
            .arg("steam")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if steam::write_shortcut(&userdata, &shortcut, &[], &running)?.is_empty() {
        return Err(acnh_core::Error::Steam("No Steam user folder found".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fake() -> FirstRun {
        let root = PathBuf::from("/fake");
        FirstRun::new(Vars {
            home: root.clone(),
            data: root.clone(),
            config: root,
        })
    }
    #[test]
    fn cancelled_connection_ignores_late_success() {
        let mut first = fake();
        first.vm.step = Step::EnterAddress;
        first.vm.busy = true;
        first.active = Some(Request::Connect("127.0.0.1:1234".parse().unwrap()));
        first.nav(Nav::Back);
        first.sender.send((0, Event::Connected(Ok(())))).unwrap();
        first.pump();
        assert_ne!(first.vm.step, Step::Pair);
        assert!(first.vm.endpoint.is_none());
    }
    #[test]
    fn build_cancel_waits_for_safe_boundary_and_ignores_result() {
        let mut first = fake();
        first.vm.step = Step::GetBuild;
        first.vm.busy = true;
        first.active = Some(Request::Build(PathBuf::from("/fake")));
        first.nav(Nav::Back);
        assert!(first.vm.busy);
        assert_eq!(first.cancelled.load(Ordering::SeqCst), 1);
        first.sender.send((0, Event::Finished(Ok(())))).unwrap();
        first.pump();
        assert_eq!(first.vm.step, Step::FindGame);
        assert!(!first.vm.busy);
    }
    #[test]
    fn folder_errors_name_missing_file_and_allowed_versions() {
        let home = Path::new("/fake");
        let message = safe_error(
            acnh_core::Error::WrongVersion {
                found: "unknown".into(),
            },
            home,
        );
        for version in ["3.0.2", "3.0.3", "2.0.7", "1.0.0"] {
            assert!(message.contains(version));
        }
        let message = safe_error(
            acnh_core::Error::BadGameFolder("exefs/sdk is missing".into()),
            home,
        );
        assert!(message.contains("exefs/sdk"));
    }
}
