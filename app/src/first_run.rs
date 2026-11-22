//! Runs first-run requests off the window thread.
use crate::vm::first_run::{Event, FirstRunVm, Request, Step};
use crate::vm::settings::SettingsVm;
use crate::vm::Nav;
use acnh_core::platform::{AppDirs, Os, Vars};
use acnh_core::{bundle, gamefolder, hub, prepare, profile};
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
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    clipboard: Option<arboard::Clipboard>,
}
impl FirstRun {
    pub fn new(vars: Vars) -> Self {
        let dirs = AppDirs::new(&vars);
        let (sender, receiver) = mpsc::channel();
        let mut vm = FirstRunVm::new(vars.home.clone());
        let cpu = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        let detected = profile::detect(&cpu);
        vm.profile = profile::builtin()
            .iter()
            .position(|item| item.id == detected.id)
            .unwrap_or(vm.profile);
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
            #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
            clipboard: None,
        }
    }
    pub fn game_root(&self) -> PathBuf {
        self.dirs.data.join("prepare")
    }
    pub fn prepared_player(&self) -> PathBuf {
        acnh_core::assemble::player(Os::current(), Path::new(""), &self.dirs.install())
    }
    pub fn needed(&self) -> bool {
        if self.dirs.config.join("first-run-complete").is_file()
            && self.game_root().join("analysis/main.bin").is_file()
        {
            return !self.prepared_player().is_file();
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
        use std::io::Write;

        std::fs::create_dir_all(&self.dirs.config)?;
        let mut marker = tempfile::NamedTempFile::new_in(&self.dirs.config)?;
        marker.write_all(b"1\n")?;
        marker.as_file().sync_all()?;
        marker.persist(self.dirs.config.join("first-run-complete"))?;
        Ok(())
    }
    pub fn can_cancel(&self) -> bool {
        matches!(
            self.active.as_ref(),
            Some(Request::Build(_) | Request::Pull(..) | Request::Connect(_) | Request::Discover)
        ) && self.cancelled.load(Ordering::SeqCst) == 0
    }
    pub fn nav(&mut self, nav: Nav) {
        if nav == Nav::Back && self.vm.busy {
            match self.active.as_ref() {
                Some(Request::Build(_) | Request::Pull(..)) => {
                    if self.cancelling_build {
                        return;
                    }
                    if self
                        .cancelled
                        .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                    {
                        self.cancelling_build = true;
                        self.vm.message =
                            "Waiting for the current work to finish before cancelling.".into();
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
                    self.vm.message =
                        "Setup cancelled. Finished preparation stages are kept.".into();
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
        if let Request::CopyLog(text) = request {
            self.vm.message = if self.copy_log(text) {
                "Log copied.".into()
            } else {
                "Clipboard unavailable.".into()
            };
            return;
        }
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

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    fn copy_log(&mut self, text: String) -> bool {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        self.clipboard
            .as_mut()
            .is_some_and(|clipboard| clipboard.set_text(text).is_ok())
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    fn copy_log(&mut self, _text: String) -> bool {
        false
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
                if cancelled
                    .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst)
                    .is_err()
                {
                    return Err(acnh_core::Error::Build("download cancelled".into()));
                }
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
        Request::Finish { profile, steam } => Event::FinishedSetup {
            steam: steam.is_some(),
            result: (|| {
                let selected = profile::builtin()
                    .get(profile)
                    .ok_or_else(|| acnh_core::Error::Config("Unknown device profile".into()))?;
                SettingsVm::new(dirs.data_root(), selected).apply_profile()?;
                if let Some(folder) = steam {
                    add_steam(vars, dirs, &folder)?;
                }
                Ok(())
            })()
            .map_err(|error| safe_error(error, &vars.home)),
        },
        Request::CopyLog(_) => unreachable!("clipboard stays on the window thread"),
    }
}

fn add_steam(vars: &Vars, dirs: &AppDirs, folder: &Path) -> acnh_core::Result<()> {
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
    let running = || {
        std::process::Command::new("pgrep")
            .arg("-x")
            .arg("steam")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if running() {
        return Err(acnh_core::Error::Steam(
            "Close Steam before adding the shortcut".into(),
        ));
    }
    let images = steam_artwork(folder)?;
    let icon = dirs.data.join("steam-icon.png");
    if let Some((_, bytes)) = images
        .iter()
        .find(|(art, _)| *art == acnh_core::artwork::Art::Icon)
    {
        std::fs::write(&icon, bytes)?;
    }
    let exe = std::env::current_exe()?;
    let shortcut = Shortcut {
        name: "Animal Crossing: New Horizons".into(),
        exe: exe.display().to_string(),
        start_dir: exe.parent().unwrap_or(Path::new(".")).display().to_string(),
        launch_options: String::new(),
        icon: icon.display().to_string(),
    };
    if steam::write_shortcut(&userdata, &shortcut, &images, &running)?.is_empty() {
        return Err(acnh_core::Error::Steam("No Steam user folder found".into()));
    }
    Ok(())
}

fn steam_artwork(folder: &Path) -> acnh_core::Result<Vec<(acnh_core::artwork::Art, Vec<u8>)>> {
    use acnh_core::artwork;
    use image::{imageops, ImageFormat, RgbaImage};
    use std::io::Cursor;

    let icon = folder.join("icon.png");
    let source = icon
        .metadata()
        .ok()
        .filter(|metadata| metadata.is_file() && metadata.len() <= 8 * 1024 * 1024)
        .and_then(|_| image::open(&icon).ok())
        .map(Ok)
        .unwrap_or_else(|| {
            image::load_from_memory_with_format(
                include_bytes!("../../assets/acnhrecomp.png"),
                ImageFormat::Png,
            )
        })
        .map_err(|_| acnh_core::Error::Steam("Could not load shortcut artwork".into()))?;
    artwork::ALL
        .into_iter()
        .map(|art| {
            let (width, height) = artwork::size(art);
            let resized = source
                .resize(width, height, imageops::FilterType::Lanczos3)
                .to_rgba8();
            let mut canvas = RgbaImage::new(width, height);
            imageops::overlay(
                &mut canvas,
                &resized,
                i64::from((width - resized.width()) / 2),
                i64::from((height - resized.height()) / 2),
            );
            let mut bytes = Cursor::new(Vec::new());
            canvas
                .write_to(&mut bytes, ImageFormat::Png)
                .map_err(|_| acnh_core::Error::Steam("Could not encode shortcut artwork".into()))?;
            Ok((art, bytes.into_inner()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_artwork_uses_the_size_for_each_library_slot() {
        let root = tempfile::tempdir().unwrap();
        let images = steam_artwork(root.path()).unwrap();
        assert_eq!(images.len(), acnh_core::artwork::ALL.len());
        for (art, bytes) in images {
            let image =
                image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).unwrap();
            assert_eq!(
                (image.width(), image.height()),
                acnh_core::artwork::size(art)
            );
        }
    }
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
