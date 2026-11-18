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

use acnh_core::config::Config;
use acnh_core::error::not_built;
use acnh_core::fleet;
use acnh_core::gamefolder;
use acnh_core::launch::{self, Game, Keep, Relay, Sync};
use acnh_core::platform::{AppDirs, Os, Vars};
use acnh_core::prepare::{self, Request};
use acnh_core::profile;
use acnh_core::recipe;
use acnh_core::saves;
use acnh_core::steam::{self, Shortcut};
use acnh_core::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

const USAGE: &str = "usage: acnh-universal check|build|play|steam-add|hub|fleet";
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

        if options.command == "hub" {
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
            "--game" | "--target" | "--out" | "--profile" => {
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
        "hub" => not_built("hub"),
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
    let out = options
        .out
        .clone()
        .unwrap_or_else(|| repo_root().join("private").join("acnh"));
    let request = Request {
        game: gamefolder::check(game)?,
        out,
        target,
        jobs: 0,
    };
    let out = prepare::prepare(&request, &mut |stage| println!("== {}", stage.key()))?;
    println!("built {} for {}", out.display(), target.key());
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

    let mut program = dirs.install().join(recipe::output(os));
    if os == Os::Mac {
        program = program.join("Contents").join("MacOS").join("acnh");
    }
    let player = Player { program };
    let relay = Offline;
    let save_dir = saves::save_dir(&root);
    let env = vec![("NX_DATA_ROOT".to_owned(), root.display().to_string())];
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
        Sync::Done(message) => println!("{step}: {message}"),
        Sync::Offline => println!("{step}: offline"),
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

    let exe = std::env::current_exe()?;
    let start_dir = exe
        .parent()
        .map(|parent| parent.display().to_string())
        .unwrap_or_default();
    let shortcut = Shortcut {
        name: NAME.to_owned(),
        exe: exe.display().to_string(),
        start_dir,
        launch_options: "play".to_owned(),
        icon: Path::new("assets/acnhrecomp.png").display().to_string(),
    };
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

fn fleet_command(_options: &Options) -> Result<i32> {
    let os = Os::current();
    let vars = Vars::from_lookup(os, |name| std::env::var(name).ok())?;
    let dirs = AppDirs::new(&vars);
    let file = dirs.config.join("fleet.txt");
    if file.is_file() {
        let text = std::fs::read_to_string(&file)?;
        let mut records = 0;
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let record = fleet::from_line(line)?;
            println!("{}", fleet::to_line(&record)?);
            records += 1;
        }
        if records > 0 {
            return Ok(0);
        }
    }
    println!("Only this device so far");
    Ok(0)
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

struct Offline;

impl Relay for Offline {
    fn pull(&self, _save_dir: &Path, _resolve: Option<Keep>) -> Result<Sync> {
        Ok(Sync::Offline)
    }

    fn push(&self, _save_dir: &Path, _resolve: Option<Keep>) -> Result<Sync> {
        Ok(Sync::Offline)
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
