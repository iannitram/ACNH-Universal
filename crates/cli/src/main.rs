//! `acnh-universal`: the launcher without its window. The Steam shortcut in Game Mode runs
//! `acnh-universal play` (SPEC.md M2, M6); the other commands do what the app's pages do.
//!
//! ```text
//! acnh-universal check  --game <folder>
//! acnh-universal build  --game <folder> [--target <os>] [--out <folder>]
//! acnh-universal play   [--profile <id>]
//! acnh-universal steam-add
//! acnh-universal hub    serve|pair|pull
//! acnh-universal fleet
//! ```
//!
//! Every command prints a plain message. A runtime error exits 1; with `--json` it prints
//! `{"code":"<Error::code>"}` instead. An unknown command exits 2 with the usage.

#![forbid(unsafe_code)]

use acnh_core::assemble;
use acnh_core::bundle;
use acnh_core::config::Config;
use acnh_core::fleet;
use acnh_core::gamefolder;
use acnh_core::hub::{self, TcpTransport};
use acnh_core::launch::{self, Game, Keep, Relay, Sync};
use acnh_core::platform::{AppDirs, Os, Vars};
use acnh_core::prepare::{self, Request};
use acnh_core::profile;
use acnh_core::saves;
use acnh_core::steam;
use acnh_core::{Error, Result};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Command;

const USAGE: &str = "usage: acnh-universal check|build|play|steam-add|hub|fleet\n\
hub serve --address <ip:port> --name <hub> --hub <role> --out <builds> [--pair] [--prepared <data>]\n\
hub pair --address <ip:port> --name <device> --peer <hub> [--target <os>]\n\
hub pull --address <ip:port> --name <device> --peer <hub> [--target <os>] [--prepared <private-data-root>]\n\
hub report --address <ip:port> --name <device> --peer <hub> --target <os> --version <version> --build-id <id> --last-sync <seconds> --free <bytes>";
const NAME: &str = "Animal Crossing: New Horizons";

#[derive(Default)]
struct Options {
    command: String,
    game: Option<PathBuf>,
    target: Option<String>,
    out: Option<PathBuf>,
    profile: Option<String>,
    json: bool,
    help: bool,
    hub_action: Option<String>,
    address: Option<String>,
    name: Option<String>,
    peer: Option<String>,
    hub: Option<String>,
    prepared: Option<PathBuf>,
    pair: bool,
    version: Option<String>,
    build_id: Option<String>,
    last_sync: Option<String>,
    free: Option<String>,
}

fn parse(args: &[String]) -> std::result::Result<Options, String> {
    let mut options = Options::default();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if options.command.is_empty() {
            match arg.as_str() {
                "--json" => {
                    options.json = true;
                    index += 1;
                }
                "--help" | "-h" => {
                    options.help = true;
                    index += 1;
                }
                _ if arg.starts_with('-') => {
                    return Err(format!("unknown option {arg}\n{USAGE}"));
                }
                _ => {
                    options.command = arg.clone();
                    index += 1;
                }
            }
            continue;
        }

        if options.command == "hub" && options.hub_action.is_none() && !arg.starts_with('-') {
            options.hub_action = Some(arg.clone());
            index += 1;
            continue;
        }

        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value.to_owned())),
            None => (arg.as_str(), None),
        };
        match flag {
            "--json" if inline.is_none() => {
                options.json = true;
                index += 1;
            }
            "--help" | "-h" if inline.is_none() => {
                options.help = true;
                index += 1;
            }
            "--pair" if inline.is_none() => {
                options.pair = true;
                index += 1;
            }
            "--game" | "--target" | "--out" | "--profile" | "--address" | "--name" | "--peer"
            | "--hub" | "--prepared" | "--version" | "--build-id" | "--last-sync" | "--free" => {
                let value = match inline {
                    Some(value) => {
                        index += 1;
                        value
                    }
                    None => {
                        let value = args
                            .get(index + 1)
                            .ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))?
                            .clone();
                        index += 2;
                        value
                    }
                };
                set_value(&mut options, flag, value)?;
            }
            _ => return Err(format!("unknown option {arg}\n{USAGE}")),
        }
    }

    if options.command.is_empty() && !options.help {
        return Err(USAGE.to_owned());
    }
    if matches!(options.command.as_str(), "check" | "build") && options.game.is_none() {
        return Err(format!("--game <folder> is needed\n{USAGE}"));
    }
    Ok(options)
}

fn set_value(options: &mut Options, flag: &str, value: String) -> std::result::Result<(), String> {
    match flag {
        "--game" => options.game = Some(PathBuf::from(value)),
        "--target" => options.target = Some(value),
        "--out" => options.out = Some(PathBuf::from(value)),
        "--profile" => options.profile = Some(value),
        "--address" => options.address = Some(value),
        "--name" => options.name = Some(value),
        "--peer" => options.peer = Some(value),
        "--hub" => options.hub = Some(value),
        "--prepared" => options.prepared = Some(PathBuf::from(value)),
        "--version" => options.version = Some(value),
        "--build-id" => options.build_id = Some(value),
        "--last-sync" => options.last_sync = Some(value),
        "--free" => options.free = Some(value),
        _ => return Err(format!("unknown option {flag}\n{USAGE}")),
    }
    Ok(())
}

fn main() {
    std::process::exit(run(std::env::args().skip(1).collect()));
}

fn run(args: Vec<String>) -> i32 {
    let options = match parse(&args) {
        Ok(options) => options,
        Err(usage) => {
            println!("{usage}");
            return 2;
        }
    };
    if options.help || options.command == "help" {
        println!("{USAGE}");
        return 0;
    }

    let result = match options.command.as_str() {
        "check" => check(&options),
        "build" => build(&options),
        "play" => play(&options),
        "steam-add" => steam_add(&options),
        "hub" => hub_command(&options),
        "fleet" => fleet_command(&options),
        _ => {
            println!("{USAGE}");
            return 2;
        }
    };

    match result {
        Ok(code) => code,
        Err(error) => {
            if options.json {
                println!("{{\"code\":\"{}\"}}", error.code());
            } else {
                eprintln!("{error}");
            }
            1
        }
    }
}

fn check(options: &Options) -> Result<i32> {
    let game = options
        .game
        .as_deref()
        .ok_or_else(|| Error::Env("--game <folder> is not set".into()))?;
    let folder = gamefolder::check(game)?;
    println!(
        "{} holds {} version {}",
        folder.root.display(),
        NAME,
        folder.version.name()
    );
    Ok(0)
}

fn build(options: &Options) -> Result<i32> {
    let game = options
        .game
        .as_deref()
        .ok_or_else(|| Error::Env("--game <folder> is not set".into()))?;
    let target = match options.target.as_deref() {
        Some(key) => Os::from_key(key).ok_or_else(|| {
            Error::Unsupported(format!(
                "unknown target {key}: linux, windows, mac, ios or android"
            ))
        })?,
        None => Os::current(),
    };
    let vars = Vars::from_lookup(Os::current(), |name| std::env::var(name).ok())?;
    // The Mac and iPhone builds hold the recompiled game: by default they are made in the
    // data root's `prepared/` folder, never in a git work tree.
    let out = options.out.clone().unwrap_or_else(|| {
        if matches!(target, Os::Mac | Os::Ios) {
            AppDirs::new(&vars).data_root().join("prepared")
        } else {
            repo_root().join("private").join("acnh")
        }
    });
    let config_path = AppDirs::new(&vars).data_root().join("config.ini");
    let config = match std::fs::read_to_string(&config_path) {
        Ok(text) => acnh_core::config::Config::parse(&text)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Default::default(),
        Err(error) => {
            return Err(Error::Io(format!(
                "cannot read {}: {error}",
                config_path.display()
            )))
        }
    };
    let delivery = match prepare::delivery_subfolder(target) {
        Some(subfolder @ ("ios" | "mac")) => {
            let launcher =
                match std::fs::read_to_string(AppDirs::new(&vars).config.join("launcher.ini")) {
                    Ok(text) => acnh_core::config::Config::parse(&text)?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        Default::default()
                    }
                    Err(error) => return Err(error.into()),
                };
            Some(prepare::delivery_folder(&launcher, &vars.home, subfolder))
        }
        _ => None,
    };
    let request = Request {
        game: gamefolder::check(game)?,
        out,
        target,
        jobs: 0,
        source: None,
        fan_mods: prepare::FanMods {
            ptbr: config.enabled("mods.ptbr"),
            speedfix: config.enabled("mods.speedfix"),
        },
        delivery,
        install: (target == Os::Mac).then(|| AppDirs::new(&vars).install()),
    };
    let out = prepare::prepare(&request, &mut |stage| println!("== {}", stage.key()))?;
    if matches!(target, Os::Ios | Os::Mac) {
        // The one contained app is the `.ipa` or `.zip` in Dropbox (the Mac's app was
        // assembled in the install folder on the way): nothing to assemble here.
        println!("built {} for {}", out.display(), target.key());
        return Ok(0);
    }
    println!("built {} for {}", out.display(), target.key());
    // The bare player becomes a self-contained one in the install folder (on the Mac, the
    // one contained app).
    let vars = Vars::from_lookup(Os::current(), |name| std::env::var(name).ok())?;
    let install = AppDirs::new(&vars).install();
    let player = prepare::assemble(target, &out.join("build"), &install)?;
    println!("assembled {}", player.display());
    Ok(0)
}

fn play(options: &Options) -> Result<i32> {
    let os = Os::current();
    let vars = Vars::from_lookup(os, |name| std::env::var(name).ok())?;
    let dirs = AppDirs::new(&vars);
    let root = dirs.data_root();

    if let Some(id) = options.profile.as_deref() {
        let chosen = profile::by_id(id).ok_or_else(|| {
            Error::Config(format!(
                "unknown profile {id}: amd-handheld, strix-halo-tablet or desktop"
            ))
        })?;
        apply_profile(&root, chosen)?;
        println!("profile {}: {}", chosen.id, chosen.name);
    }

    // On the Mac the player is beside this executable in the one contained app.
    let launcher = std::env::current_exe().unwrap_or_default();
    let program = assemble::player(os, &launcher, &dirs.install());
    let player = Player { program };
    let relay = RelayOff;
    let save_dir = saves::save_dir(&root);
    let mut env = vec![("NX_DATA_ROOT".to_owned(), root.display().to_string())];
    if os == Os::Mac {
        // As the app does (#171): the player opens its log with fopen and makes no folder.
        std::fs::create_dir_all(dirs.logs())?;
        env.push((
            "NX_LOG".to_owned(),
            dirs.logs().join("nx.log").display().to_string(),
        ));
        if let Some(game) = player
            .program
            .parent()
            .and_then(Path::parent)
            .map(|contents| contents.join("Resources/game"))
            .filter(|game| game.join("analysis/main.bin").is_file())
        {
            env.push(("NX_GAME_ROOT".to_owned(), game.display().to_string()));
        }
    }
    let choose = |_: &Sync| Keep::Local;
    let played = launch::play(&relay, &player, &save_dir, &env, &choose)?;
    report_sync("pull", &played.pulled);
    report_sync("push", &played.pushed);
    println!("game exited with {}", played.exit_code);
    Ok(played.exit_code)
}

fn apply_profile(root: &Path, chosen: &profile::DeviceProfile) -> Result<()> {
    let path = root.join("config.ini");
    let mut config = if path.is_file() {
        Config::parse(&std::fs::read_to_string(&path)?)?
    } else {
        Config::default()
    };
    for (key, value) in chosen.settings {
        if config.get(key).is_none() {
            config.set(key, value);
        }
    }
    std::fs::create_dir_all(root)?;
    std::fs::write(path, config.to_text())?;
    Ok(())
}

fn report_sync(step: &str, sync: &Sync) {
    match sync {
        Sync::Done(message) | Sync::Resolved { message, .. } => println!("{step}: {message}"),
        Sync::Offline => println!("{step}: offline"),
        Sync::Off => println!("{step}: sync off"),
        Sync::Held { .. } => println!("{step}: held"),
        Sync::Conflict { .. } => println!("{step}: conflict"),
    }
}

fn steam_add(_options: &Options) -> Result<i32> {
    let os = Os::current();
    let vars = Vars::from_lookup(os, |name| std::env::var(name).ok())?;
    let userdata = steam::userdata_roots(&vars)
        .into_iter()
        .find(|path| path.is_dir())
        .ok_or_else(|| Error::Steam("no Steam user folder found".into()))?;

    let exe = std::env::current_exe()?.with_file_name(if cfg!(windows) {
        "acnh-app.exe"
    } else {
        "acnh-app"
    });
    let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from);
    if !appimage.as_deref().unwrap_or(&exe).is_file() {
        return Err(Error::Steam("the launcher executable is missing".into()));
    }
    if steam_running() {
        return Err(Error::Steam("close Steam first".into()));
    }
    let mut icon = steam::assets_dir()?.join("acnhrecomp.png");
    if appimage.is_some() {
        // Steam reads the icon after this AppImage mount has been removed.
        let dir = AppDirs::new(&vars).data;
        std::fs::create_dir_all(&dir)?;
        let mut file = tempfile::NamedTempFile::new_in(&dir)?;
        std::io::copy(&mut std::fs::File::open(&icon)?, &mut file)?;
        icon = dir.join("steam-launcher-icon.png");
        file.persist(&icon).map_err(|err| err.error)?;
    }
    let shortcut = steam::launcher_shortcut(&exe, appimage.as_deref(), NAME, &icon);
    let written = steam::write_shortcut(&userdata, &shortcut, &[], &steam_running)?;
    if written.is_empty() {
        return Err(Error::Steam("no Steam user folder found".into()));
    }
    let paths = written
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    println!("added {NAME} to Steam ({paths})");
    Ok(0)
}

fn steam_running() -> bool {
    if cfg!(target_os = "windows") {
        return Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq steam.exe", "/NH"])
            .output()
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .to_ascii_lowercase()
                    .contains("steam.exe")
            })
            .unwrap_or(false);
    }
    ["steam", "steam_osx", "Steam"].iter().any(|name| {
        Command::new("pgrep")
            .args(["-x", name])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    })
}

fn app_dirs() -> Result<AppDirs> {
    Ok(AppDirs::new(&Vars::from_lookup(Os::current(), |name| {
        std::env::var(name).ok()
    })?))
}

fn required<'a>(value: &'a Option<String>, label: &str) -> Result<&'a str> {
    value
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::Hub(format!("{label} is required")))
}

fn address(options: &Options) -> Result<SocketAddr> {
    let address: SocketAddr = required(&options.address, "--address")?
        .parse()
        .map_err(|_| Error::Hub("invalid address".into()))?;
    if !hub::is_home_network(address.ip()) || address.port() == 0 {
        return Err(Error::Hub(
            "address is outside the home network or has no port".into(),
        ));
    }
    Ok(address)
}

fn target(options: &Options) -> Result<Os> {
    match options.target.as_deref() {
        Some(value) => Os::from_key(value).ok_or_else(|| Error::Hub("invalid target".into())),
        None => Ok(Os::current()),
    }
}

fn code() -> Result<String> {
    use std::io::{self, Write};
    eprint!("Enter the six-digit pairing code: ");
    io::stderr().flush()?;
    let mut code = String::new();
    io::stdin().read_line(&mut code)?;
    let code = code.trim().to_owned();
    hub::PairingSession::new(&code)?;
    Ok(code)
}

fn hub_command(options: &Options) -> Result<i32> {
    let dirs = app_dirs()?;
    let name = required(&options.name, "--name")?;
    let addr = address(options)?;
    match options.hub_action.as_deref() {
        Some("serve") => {
            let role = acnh_core::recipe::hub_by_key(required(&options.hub, "--hub")?)?;
            if options.prepared.is_some() && role != acnh_core::recipe::Hub::StrixHalo {
                return Err(Error::Hub(
                    "prepared Android data requires the Strix Halo hub".into(),
                ));
            }
            let builds = options
                .out
                .as_deref()
                .ok_or_else(|| Error::Hub("--out is required".into()))?;
            let listener = TcpListener::bind(addr)?;
            let hostname = format!("{name}.local.");
            let _announcement = hub::announce(name, &hostname, role, &[addr.ip()], addr.port())?;
            let mut pairing = if options.pair {
                Some(hub::PairingSession::new(&code()?)?)
            } else {
                None
            };
            println!(
                "Hub listening; pairing {}",
                if pairing.is_some() { "armed" } else { "off" }
            );
            for incoming in listener.incoming() {
                let stream = incoming?;
                let mut transport = match TcpTransport::from_stream(stream) {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                if let Err(error) = hub::serve_request(
                    &mut transport,
                    &dirs,
                    builds,
                    options.prepared.as_deref(),
                    pairing.as_mut(),
                ) {
                    eprintln!("Hub request failed: {}", error.code());
                }
            }
            Ok(0)
        }
        Some("pair") => {
            let peer = required(&options.peer, "--peer")?;
            hub::pair_hub(addr, &dirs, name, peer, &code()?, target(options)?)?;
            println!("Paired with hub");
            Ok(0)
        }
        Some("pull") => {
            let peer = required(&options.peer, "--peer")?;
            let target = target(options)?;
            if let Some(root) = &options.prepared {
                hub::pull_hub_prepared(addr, &dirs, name, peer, target, root)?;
                println!("Prepared data installed");
            } else {
                if options.out.is_some() {
                    return Err(Error::Hub("--out is not used for pulls".into()));
                }
                std::fs::create_dir_all(&dirs.data)?;
                let staged = tempfile::Builder::new()
                    .prefix(".hub-pull-")
                    .tempdir_in(&dirs.data)?;
                let bundle = hub::pull_hub(addr, &dirs, name, peer, target, staged.path())?;
                bundle::install(&bundle, staged.path(), &dirs.install())?;
                hub::report_hub_build(addr, &dirs, name, peer, &bundle)
                    .map_err(|_| Error::Hub("build installed, but fleet report failed".into()))?;
                println!("Build installed");
            }
            Ok(0)
        }
        Some("report") => {
            let peer = required(&options.peer, "--peer")?;
            let parse = |value: &Option<String>, label: &str| -> Result<u64> {
                required(value, label)?
                    .parse()
                    .map_err(|_| Error::Hub(format!("invalid {label}")))
            };
            let record = fleet::DeviceRecord {
                device: name.into(),
                os: target(options)?,
                version: required(&options.version, "--version")?.into(),
                build_id: required(&options.build_id, "--build-id")?.into(),
                last_sync_unix: parse(&options.last_sync, "--last-sync")?,
                free_bytes: parse(&options.free, "--free")?,
            };
            hub::report_hub(addr, &dirs, peer, &record)?;
            println!("Fleet report sent");
            Ok(0)
        }
        _ => Err(Error::Hub("usage: hub serve|pair|pull|report".into())),
    }
}

fn fleet_command(_options: &Options) -> Result<i32> {
    let records = hub::read_fleet(&app_dirs()?)?;
    if records.is_empty() {
        println!("Only this device so far");
    }
    for record in records {
        println!("{}", fleet::to_line(&record)?);
    }
    Ok(0)
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

struct RelayOff;

impl Relay for RelayOff {
    fn pull(&self, _save_dir: &Path, _resolve: Option<Keep>) -> Result<Sync> {
        Ok(Sync::Off)
    }

    fn push(&self, _save_dir: &Path, _resolve: Option<Keep>) -> Result<Sync> {
        Ok(Sync::Off)
    }
}

struct Player {
    program: PathBuf,
}

impl Game for Player {
    fn run(&self, env: &[(String, String)]) -> Result<i32> {
        let status = Command::new(&self.program)
            .envs(env.iter().map(|(name, value)| (name, value)))
            .status()
            .map_err(|error| {
                Error::Io(format!("cannot start {}: {error}", self.program.display()))
            })?;
        Ok(status.code().unwrap_or(1))
    }
}
