//! Runs first-run requests off the window thread.
use crate::relay_setup::{PageView, RelayRunner, RelaySettings, RelaySetup};
use crate::vm::first_run::{Event, FirstRunVm, Request, Step};
use crate::vm::settings::SettingsVm;
use crate::vm::Nav;
use acnh_core::platform::{AppDirs, Os, Vars};
use acnh_core::{gamefolder, hub, prepare, profile};
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
    relay: RelayRunner,
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    clipboard: Option<arboard::Clipboard>,
}
/// A physical keyboard key for whichever text field is open: the first run's save sync step
/// while the first run is up, else Settings' sign-in pages. True when a field took the key;
/// false leaves it to the controller map. This runs before the controller map, so letters
/// and digits reach the field.
pub fn route_text(
    first: &mut FirstRun,
    first_open: bool,
    relay: &mut RelaySettings,
    text: &str,
) -> bool {
    if first_open {
        first.text_key(text)
    } else {
        relay.text_key(text)
    }
}

/// Save sync setup as this machine has it.
pub fn system_relay_setup() -> Arc<dyn RelaySetup> {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    if let Some(setup) = crate::relay_setup::SystemSetup::system() {
        return Arc::new(setup);
    }
    Arc::new(crate::relay_setup::Unavailable)
}

impl FirstRun {
    pub fn new(vars: Vars) -> Self {
        Self::with_relay(vars, system_relay_setup())
    }

    /// The first run with save sync setup given (a fake in tests).
    pub fn with_relay(vars: Vars, setup: Arc<dyn RelaySetup>) -> Self {
        let dirs = AppDirs::new(&vars);
        let (sender, receiver) = mpsc::channel();
        let mut vm = FirstRunVm::new(vars.home.clone());
        let cpu = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        let detected = profile::detect(&cpu);
        vm.profile = profile::builtin()
            .iter()
            .position(|item| item.id == detected.id)
            .unwrap_or(vm.profile);
        vm.relay_available = setup.available();
        vm.relay_set_up = setup.account().is_some();
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
            relay: RelayRunner::new(setup),
            #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
            clipboard: None,
        }
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
        let builds = crate::build_actions::BuildActions {
            dirs: self.dirs.clone(),
            os: Os::current(),
        };
        !builds.has_game_here(&player)
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
    /// A key typed on a physical keyboard while a text field is open.
    pub fn text_key(&mut self, text: &str) -> bool {
        if self.vm.step != Step::Relay || !crate::relay_setup::text_key(&mut self.vm.relay, text) {
            return false;
        }
        self.dispatch();
        true
    }
    pub fn pump(&mut self) -> bool {
        let relay = self.relay.pump(&mut self.vm.relay);
        let relay_any = relay.is_some();
        if let Some(outcome) = relay {
            self.vm.relay_outcome(outcome);
        }
        let events: Vec<_> = self.receiver.try_iter().collect();
        let any = !events.is_empty() || relay_any;
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
    /// What the guided page draws for the step the first run is on. The save sync step shows
    /// its own focus (a key of the keyboard while it is open) and its own message.
    pub fn page(&self) -> PageView {
        let relay = self.vm.step == Step::Relay;
        let progress = if self.vm.step == Step::GetBuild {
            self.vm
                .stage
                .and_then(|stage| acnh_core::recipe::STAGES.iter().position(|s| *s == stage))
                .map(|n| (n * 100 / 7) as i32)
                .unwrap_or(0)
        } else {
            -1
        };
        PageView {
            heading: "Guided setup".into(),
            title: format!("Step {} of 6\n{}", self.vm.step_number(), self.vm.title()),
            options: self.vm.options(),
            focus: self.vm.focus_index(),
            busy: self.vm.busy,
            message: if relay {
                self.vm.relay.message.clone()
            } else {
                self.vm.message.clone()
            },
            cancel_available: self.can_cancel() || (relay && self.vm.relay.busy()),
            keypad: self.vm.keypad_open(),
            columns: self.vm.columns(),
            editor: relay && self.vm.relay.keyboard_open(),
            progress,
        }
    }

    pub fn done(&self) -> bool {
        self.vm.step == Step::Done
    }
    fn dispatch(&mut self) {
        self.relay.dispatch(&mut self.vm.relay);
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
/// The machine's own name as a short, plain label ("deck", "macbook"), or "device".
fn friendly_host() -> String {
    let raw = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|name| !name.is_empty())
        .or_else(|| {
            let out = std::process::Command::new("hostname").output().ok()?;
            String::from_utf8(out.stdout).ok()
        })
        .unwrap_or_default();
    let label: String = raw
        .trim()
        .split('.')
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(24)
        .collect::<String>()
        .to_ascii_lowercase();
    if label.is_empty() {
        "device".into()
    } else {
        label
    }
}

pub(crate) fn device(dirs: &AppDirs) -> acnh_core::Result<String> {
    let path = dirs.config.join("hub-device");
    if let Ok(name) = std::fs::read_to_string(&path) {
        return Ok(name.trim().to_owned());
    }
    std::fs::create_dir_all(&dirs.config)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // The hub lists paired devices by this name: the machine's own name leads it.
    let name = format!(
        "{}-{:x}",
        friendly_host(),
        (stamp ^ u128::from(std::process::id())) & 0xffff_ffff
    );
    acnh_core::platform::atomic_write(&path, name.as_bytes())?;
    Ok(name)
}
fn save_launcher_values(dirs: &AppDirs, values: &[(&str, &str)]) -> acnh_core::Result<()> {
    let path = dirs.config.join("launcher.ini");
    let mut config = match std::fs::read_to_string(&path) {
        Ok(text) => acnh_core::config::Config::parse(&text)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
        Err(e) => return Err(e.into()),
    };
    for &(key, value) in values {
        if value.contains(['\r', '\n']) {
            return Err(acnh_core::Error::Config(format!(
                "{key} must be a single line"
            )));
        }
        config.set(key, value);
    }
    std::fs::create_dir_all(&dirs.config)?;
    acnh_core::platform::atomic_write(&path, config.to_text().as_bytes())
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
            if root.as_os_str().is_empty() && cfg!(windows) {
                return Event::Folders(drive_roots(|p| p.is_dir()));
            }
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
                .and_then(|id| hub::pair_hub(addr, dirs, &id, &name, &code, Os::current()))
                .map_err(|_| "Codes did not match".to_owned())
                .and_then(|_| {
                    save_launcher_values(
                        dirs,
                        &[("hub_name", &name), ("hub_address", &addr.to_string())],
                    )
                    .map_err(|_| "Could not save the paired hub address".into())
                }),
        ),
        Request::Pull(addr, name) => Event::Finished(
            (|| {
                std::fs::create_dir_all(&dirs.data)?;
                let id = device(dirs)?;
                if Os::current() == Os::Android {
                    let game_dir = crate::build_actions::BuildActions {
                        dirs: dirs.clone(),
                        os: Os::Android,
                    }
                    .pull_android_prepared(
                        addr,
                        &id,
                        &name,
                        &dirs.data,
                        u64::MAX,
                    )?;
                    if !crate::build_actions::BuildActions::android_game_installed(&game_dir) {
                        return Err(acnh_core::Error::Build(
                            "the installed Android game could not be verified".into(),
                        ));
                    }
                    return Ok(());
                }
                // The same staging folder every time: a pull that was cut continues from
                // the chunks it already has.
                let staged = hub::resume_dir(dirs)?;
                let bundle = hub::pull_hub(addr, dirs, &id, &name, Os::current(), &staged)?;
                if cancelled
                    .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst)
                    .is_err()
                {
                    return Err(acnh_core::Error::Build("download cancelled".into()));
                }
                crate::build_actions::BuildActions {
                    dirs: dirs.clone(),
                    os: Os::current(),
                }
                .install_bundle(&bundle, &staged)?;
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
                let target = Os::current();
                // The Mac's one contained app goes to Dropbox, as the hub's Prepare does.
                let delivery = if target == Os::Mac {
                    let launcher = match std::fs::read_to_string(dirs.config.join("launcher.ini")) {
                        Ok(text) => acnh_core::config::Config::parse(&text)?,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
                        Err(e) => return Err(e.into()),
                    };
                    Some(prepare::delivery_folder(&launcher, &vars.home, "mac"))
                } else {
                    None
                };
                let request = prepare::Request {
                    game: gamefolder::check(&path)?,
                    out: prepare::prepared_folder(target, dirs),
                    target,
                    jobs: 0,
                    source: None,
                    fan_mods: prepare::FanMods {
                        ptbr: config.enabled("mods.ptbr"),
                        speedfix: config.enabled("mods.speedfix"),
                    },
                    delivery,
                    install: (target == Os::Mac).then(|| dirs.install()),
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
                if target != Os::Mac {
                    prepare::assemble(target, &out.join("build"), &dirs.install())?;
                }
                save_launcher_values(dirs, &[("game_folder", &path.to_string_lossy())])?;
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
        Request::CloudDownloadAndSync => Event::CloudSyncFinished(
            cloud_download_and_sync(dirs).map_err(|e| safe_error(e, &vars.home)),
        ),
        Request::SystemPickFolder(start) => Event::SystemPicked(pick_folder_native(&start)),
        Request::CopyLog(_) => unreachable!("clipboard stays on the window thread"),
    }
}

fn os_slug(os: Os) -> &'static str {
    match os {
        Os::Linux => "linux",
        Os::Windows => "windows",
        Os::Mac => "macos",
        Os::Android => "android",
        Os::Ios => "ios",
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn cloud_download_and_sync(dirs: &AppDirs) -> acnh_core::Result<()> {
    let sr_vars = save_relay::platform::PathVars::current()
        .map_err(|e| acnh_core::Error::Saves(e.to_string()))?;
    let sr_dirs = save_relay::platform::AppDirs::from_vars(&sr_vars);
    let creds = save_relay::creds::KeyringStore::new();
    let transport = Arc::new(save_relay::http::UreqTransport::new());
    let relay = acnh_core::relay::open(&sr_dirs, &creds, transport)?
        .ok_or_else(|| acnh_core::Error::Saves("Save sync is not set up. Sign in first.".into()))?;
    std::fs::create_dir_all(&dirs.data)?;
    let game_dir = acnh_core::relay::download_game_from_relay(
        &relay,
        &dirs.data,
        &dirs.data.join("game"),
        u64::MAX,
        os_slug(Os::current()),
        // The app embeds no build id or game ABI to compare against yet (#339, ruling #321);
        // empty skips those two checks, and the manifest is still validated and every file hashed.
        "",
        "",
    )?;
    if Os::current() != Os::Android && Os::current() != Os::Ios {
        install_downloaded_desktop_game(&game_dir, &dirs.install())?;
    }
    let save_dir = dirs.data_root().join("user/device_save");
    std::fs::create_dir_all(&save_dir)?;
    let _ = relay.pull_with(&save_dir, None, false)?;
    Ok(())
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn install_downloaded_desktop_game(game_dir: &Path, install_dir: &Path) -> acnh_core::Result<()> {
    make_tree_writable(game_dir)?;
    std::fs::create_dir_all(install_dir)?;
    move_game_tree(game_dir, game_dir, install_dir)?;
    let manifest_src = game_dir.join("manifest.json");
    if manifest_src.is_file() {
        move_file_replace(&manifest_src, &install_dir.join("manifest.json"), false)?;
    }
    let _ = std::fs::remove_dir_all(game_dir);
    Ok(())
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn make_tree_writable(path: &Path) -> acnh_core::Result<()> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(());
    };
    let mut perms = meta.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = perms.mode();
        perms.set_mode(if meta.is_dir() {
            mode | 0o700
        } else {
            mode | 0o200
        });
    }
    #[cfg(not(unix))]
    {
        // Windows has only the read-only attribute: clearing it is what "writable" means there.
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
    }
    std::fs::set_permissions(path, perms)?;
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            make_tree_writable(&entry?.path())?;
        }
    }
    Ok(())
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn move_game_tree(root: &Path, dir: &Path, install_dir: &Path) -> acnh_core::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .map_err(|e| acnh_core::Error::Io(e.to_string()))?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if rel_str == "manifest.json"
            || rel_str == "root/config.ini"
            || rel_str == "root/user"
            || rel_str.starts_with("root/user/")
        {
            continue;
        }
        if path.is_dir() {
            move_game_tree(root, &path, install_dir)?;
        } else if path.is_file() {
            let executable = rel_str == "acnh"
                || rel_str.ends_with(".exe")
                || rel_str.contains(".so")
                || rel_str.ends_with(".dylib");
            move_file_replace(&path, &install_dir.join(rel), executable)?;
        }
    }
    Ok(())
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn move_file_replace(src: &Path, dst: &Path, executable: bool) -> acnh_core::Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if dst.exists() {
        let _ = make_tree_writable(dst);
        let _ = std::fs::remove_file(dst);
    }
    if std::fs::rename(src, dst).is_err() {
        std::fs::copy(src, dst)?;
        let _ = std::fs::remove_file(src);
    }
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dst, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    Ok(())
}

#[cfg(any(target_os = "android", target_os = "ios"))]
fn cloud_download_and_sync(_dirs: &AppDirs) -> acnh_core::Result<()> {
    Err(acnh_core::Error::Unsupported(
        "Cloud download uses the mobile app downloader on this platform.".into(),
    ))
}

#[cfg(target_os = "linux")]
fn pick_folder_native(start: &Path) -> Option<PathBuf> {
    let start_str = start.to_string_lossy().into_owned();
    if let Ok(out) = std::process::Command::new("kdialog")
        .args(["--getexistingdirectory", &start_str])
        .output()
    {
        if out.status.success() {
            let picked = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if !picked.is_empty() {
                return Some(PathBuf::from(picked));
            }
        }
    }
    let zenity_start = if start_str.ends_with('/') {
        start_str
    } else {
        format!("{start_str}/")
    };
    if let Ok(out) = std::process::Command::new("zenity")
        .args([
            "--file-selection",
            "--directory",
            &format!("--filename={zenity_start}"),
        ])
        .output()
    {
        if out.status.success() {
            let picked = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if !picked.is_empty() {
                return Some(PathBuf::from(picked));
            }
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn pick_folder_native(_start: &Path) -> Option<PathBuf> {
    None
}

/// The drive roots (`A:\\` to `Z:\\`) `exists` accepts: what the Windows browser lists above a
/// drive root, so it can change drive.
fn drive_roots(exists: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    ('A'..='Z')
        .map(|letter| PathBuf::from(format!("{letter}:\\")))
        .filter(|root| exists(root))
        .collect()
}

fn add_steam(vars: &Vars, dirs: &AppDirs, folder: &Path) -> acnh_core::Result<()> {
    use acnh_core::steam;
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
    let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from);
    let shortcut = steam::launcher_shortcut(
        &exe,
        appimage.as_deref(),
        "Animal Crossing: New Horizons",
        &icon,
    );
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
            image::open(
                acnh_core::steam::assets_dir()
                    .unwrap_or_default()
                    .join("acnhrecomp.png"),
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
    fn drive_roots_lists_the_existing_drives() {
        let roots = drive_roots(|p| p.to_string_lossy().starts_with(['C', 'D']));
        assert_eq!(roots, vec![PathBuf::from("C:\\"), PathBuf::from("D:\\")]);
    }

    #[test]
    fn needed_reads_the_prepared_and_contained_game() {
        let root = tempfile::tempdir().unwrap();
        let vars = Vars {
            home: root.path().into(),
            data: root.path().join("data"),
            config: root.path().join("config"),
        };
        let first = FirstRun::new(vars);
        // The test binary is the launcher; make it the "installed player" too.
        let launcher = std::env::current_exe().unwrap();
        let player = acnh_core::assemble::player(Os::current(), &launcher, &first.dirs.install());
        std::fs::create_dir_all(player.parent().unwrap()).unwrap();
        std::fs::write(&player, b"p").unwrap();
        assert!(first.needed());
        let prepared = acnh_core::prepare::prepared_folder(Os::current(), &first.dirs);
        std::fs::create_dir_all(prepared.join("analysis")).unwrap();
        std::fs::write(prepared.join("analysis/main.bin"), []).unwrap();
        assert!(!first.needed());
    }

    #[test]
    fn launcher_values_preserve_existing_settings() {
        let root = tempfile::tempdir().unwrap();
        let dirs = AppDirs::new(&Vars {
            home: root.path().into(),
            data: root.path().join("data"),
            config: root.path().join("config"),
        });
        std::fs::create_dir_all(&dirs.config).unwrap();
        std::fs::write(dirs.config.join("launcher.ini"), "other = kept\n").unwrap();
        save_launcher_values(&dirs, &[("game_folder", "/example/game")]).unwrap();
        let text = std::fs::read_to_string(dirs.config.join("launcher.ini")).unwrap();
        let config = acnh_core::config::Config::parse(&text).unwrap();
        assert_eq!(config.get("other"), Some("kept"));
        assert_eq!(config.get("game_folder"), Some("/example/game"));
    }

    #[test]
    fn steam_artwork_uses_the_size_for_each_library_slot() {
        let root = tempfile::tempdir().unwrap();
        // A generated icon keeps the test independent of where the executable lives.
        image::RgbaImage::from_pixel(64, 64, image::Rgba([10, 120, 200, 255]))
            .save(root.path().join("icon.png"))
            .unwrap();
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
