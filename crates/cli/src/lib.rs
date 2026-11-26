//! The `acnh-universal` commands: the launcher without its window. The Steam shortcut in Game Mode runs
//! `acnh-universal play` (SPEC.md M2, M6); the other commands do what the app's pages do.
//!
//! ```text
//! acnh-universal check  --game <folder>
//! acnh-universal build  --game <folder> [--target <os>] [--out <folder>]
//! acnh-universal play   [--profile <id>] [--keep relay|here] [--anyway]
//! acnh-universal sync   setup --server <url> --user <name> | sign-in gdrive|dropbox | status | off
//!                       | push [--keep relay|here]
//! acnh-universal steam-add
//! acnh-universal hub    serve|pair|pull|report
//! acnh-universal fleet
//! acnh-universal cloud-upload --os <os> --install <folder> --prepared <folder>
//! acnh-universal help | --help
//! ```
//!
//! Every command prints a plain message. A runtime error exits 1; with `--json` it prints
//! `{"code":"<Error::code>"}` instead. An unknown command exits 2 with the usage; `--help` exits 0.
//!
//! `play` never answers a save conflict on its own: a conflict exits 3 (unless `--keep relay`
//! or `--keep here` was given), a held island exits 4 (unless `--anyway`), and save sync that
//! is set up but cannot be reached exits 5. None of the three starts the game. A push that
//! conflicts after the game ran exits 6, whatever `--keep` says (settle it with
//! `sync push --keep relay|here`); a push that finds the relay unreachable after the game ran
//! exits 5 (the island stays here). `sync status` exits 5 when the relay cannot be reached. A game that quits with a non-zero code makes `play`
//! exit 1 and print that code, so its own code never looks like one of these.

#![forbid(unsafe_code)]

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod sync;

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
use acnh_core::savecard;
use acnh_core::saves;
use acnh_core::steam;
use acnh_core::{Error, Result};
use std::cell::{Cell, RefCell};
use std::io::{BufRead, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
use crate::sync::SyncEnv;

const USAGE: &str = "usage: acnh-universal check|build|play|sync|steam-add|hub|fleet|cloud-upload\n\
play [--profile <id>] [--keep relay|here] [--anyway]\n\
sync setup --server <url> --user <name> [--device <name>] (app password on standard input)\n\
sync sign-in gdrive|dropbox | sync status | sync off | sync push [--keep relay|here]\n\
cloud-upload --os linux --install <folder> --prepared <folder>\n\
exit codes: play 3 conflict, 4 held, 5 relay unreachable, 6 push conflict after the game ran; the game's own failure is 1\n\
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
    keep: Option<Keep>,
    anyway: bool,
    server: Option<String>,
    user: Option<String>,
    device: Option<String>,
    sync_arg: Option<String>,
    os: Option<String>,
    install: Option<PathBuf>,
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

        if matches!(options.command.as_str(), "hub" | "sync")
            && options.hub_action.is_none()
            && !arg.starts_with('-')
        {
            options.hub_action = Some(arg.clone());
            index += 1;
            continue;
        }
        if options.command == "sync"
            && options.hub_action.is_some()
            && options.sync_arg.is_none()
            && !arg.starts_with('-')
        {
            options.sync_arg = Some(arg.clone());
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
            "--anyway" if inline.is_none() => {
                options.anyway = true;
                index += 1;
            }
            "--pair" if inline.is_none() => {
                options.pair = true;
                index += 1;
            }
            "--game" | "--target" | "--out" | "--profile" | "--address" | "--name" | "--peer"
            | "--hub" | "--prepared" | "--version" | "--build-id" | "--last-sync" | "--free"
            | "--keep" | "--server" | "--user" | "--device" | "--os" | "--install" => {
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
        "--keep" => {
            options.keep = Some(match value.as_str() {
                "relay" => Keep::Server,
                "here" => Keep::Local,
                _ => return Err(format!("--keep takes relay or here\n{USAGE}")),
            })
        }
        "--server" => options.server = Some(value),
        "--user" => options.user = Some(value),
        "--device" => options.device = Some(value),
        "--os" => options.os = Some(value),
        "--install" => options.install = Some(PathBuf::from(value)),
        _ => return Err(format!("unknown option {flag}\n{USAGE}")),
    }
    Ok(())
}

/// Where `play` gets its relay: the save-relay setup on this machine (`Open`), or one the
/// caller built (`Fixed`, for tests).
#[derive(Clone, Copy)]
pub enum RelaySource<'a> {
    Open,
    Fixed(&'a dyn Relay),
}

/// What the commands run against: the machine's folders, the game, the relay and the streams.
pub struct Context<'a> {
    pub os: Os,
    pub var: &'a dyn Fn(&str) -> Option<String>,
    /// The player; `None` starts the one installed beside the launcher.
    pub game: Option<&'a dyn Game>,
    pub relay: RelaySource<'a>,
    pub stdin: &'a mut dyn BufRead,
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
    /// Save sync's folders, credential store and network (desktop builds only).
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    pub sync: SyncEnv<'a>,
}

/// Run one command line against this machine and return the exit code.
pub fn run_process(args: Vec<String>) -> i32 {
    let var = |name: &str| std::env::var(name).ok();
    let stdin = std::io::stdin();
    let (mut stdin, mut out, mut err) = (stdin.lock(), std::io::stdout(), std::io::stderr());
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    let creds = sync::real_credentials();
    let mut cx = Context {
        os: Os::current(),
        var: &var,
        game: None,
        relay: RelaySource::Open,
        stdin: &mut stdin,
        out: &mut out,
        err: &mut err,
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        sync: SyncEnv::real(&creds),
    };
    run(args, &mut cx)
}

/// Run one command line: the exit code, with its messages on `cx.out` and `cx.err`.
pub fn run(args: Vec<String>, cx: &mut Context<'_>) -> i32 {
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
    // Checked after `--help`, so `build --help` and `check --help` print the usage and exit 0.
    if matches!(options.command.as_str(), "check" | "build") && options.game.is_none() {
        println!("--game <folder> is needed\n{USAGE}");
        return 2;
    }

    let result = match options.command.as_str() {
        "check" => check(&options),
        "build" => build(&options),
        "play" => play(&options, cx),
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        "sync" => sync::command(&options, cx),
        "steam-add" => steam_add(&options),
        "hub" => hub_command(&options, cx),
        "fleet" => fleet_command(&options),
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        "cloud-upload" => cloud_upload(&options, cx),
        _ => {
            println!("{USAGE}");
            return 2;
        }
    };

    match result {
        Ok(code) => code,
        Err(error) => fail(&options, cx, &error, 1),
    }
}

/// Print `error` (its `code()` with `--json`) and give `exit`.
pub(crate) fn fail(options: &Options, cx: &mut Context<'_>, error: &Error, exit: i32) -> i32 {
    let _ = if options.json {
        writeln!(cx.out, "{{\"code\":\"{}\"}}", error.code())
    } else {
        writeln!(cx.err, "{error}")
    };
    exit
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub(crate) fn required_text<'a>(value: &'a Option<String>, label: &str) -> Result<&'a str> {
    value
        .as_deref()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| Error::Config(format!("{label} is required")))
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
    // Every build holds the recompiled game: by default it is made in the data root's
    // `prepared/` folder, never in a git work tree (nor at a path baked in at compile time).
    let out = options
        .out
        .clone()
        .unwrap_or_else(|| default_build_out(&vars));
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

/// Where `build` makes its output without `--out`: the data root's `prepared/` folder.
fn default_build_out(vars: &Vars) -> PathBuf {
    AppDirs::new(vars).data_root().join("prepared")
}

fn play(options: &Options, cx: &mut Context<'_>) -> Result<i32> {
    let os = cx.os;
    let vars = Vars::from_lookup(os, cx.var)?;
    let dirs = AppDirs::for_os(os, &vars);
    let root = dirs.data_root();

    if let Some(id) = options.profile.as_deref() {
        let chosen = profile::by_id(id).ok_or_else(|| {
            Error::Config(format!(
                "unknown profile {id}: amd-handheld, strix-halo-tablet or desktop"
            ))
        })?;
        apply_profile(&root, chosen)?;
        writeln!(cx.out, "profile {}: {}", chosen.id, chosen.name)?;
    }

    // On the Mac the player is beside this executable in the one contained app.
    let installed;
    let mut program = None;
    let game: &dyn Game = match cx.game {
        Some(game) => game,
        None => {
            let launcher = std::env::current_exe().unwrap_or_default();
            let path = assemble::player(os, &launcher, &dirs.install());
            program = Some(path.clone());
            installed = Player { program: path };
            &installed
        }
    };
    let opened;
    let relay: &dyn Relay = match cx.relay {
        RelaySource::Fixed(relay) => relay,
        RelaySource::Open => {
            opened = open_relay(cx)?;
            &*opened
        }
    };

    let save_dir = saves::save_dir(&root);
    let backups = saves::backups_dir(&root);
    let mut env = vec![("NX_DATA_ROOT".to_owned(), root.display().to_string())];
    if os == Os::Mac {
        // As the app does (#171): the player opens its log with fopen and makes no folder.
        std::fs::create_dir_all(dirs.logs())?;
        env.push((
            "NX_LOG".to_owned(),
            dirs.logs().join("nx.log").display().to_string(),
        ));
        if let Some(game_root) = program
            .as_deref()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .map(|contents| contents.join("Resources/game"))
            .filter(|game_root| game_root.join("analysis/main.bin").is_file())
        {
            env.push(("NX_GAME_ROOT".to_owned(), game_root.display().to_string()));
        }
    }
    let guard = Guard {
        inner: relay,
        keep: options.keep,
        answer_push: false,
        anyway: options.anyway,
        backups: &backups,
        stopped: RefCell::new(None),
    };
    let ran = Ran {
        inner: game,
        code: Cell::new(None),
    };
    // The guard never reports a conflict upward, so `choose` is never asked.
    let choose = |_: &Sync| Keep::Local;
    match launch::play(&guard, &ran, &save_dir, &env, &choose) {
        Ok(played) => {
            report_sync(cx.out, "pull", &played.pulled)?;
            report_sync(cx.out, "push", &played.pushed)?;
            writeln!(cx.out, "game exited with {}", played.exit_code)?;
            // The game's own code never shares a number with the codes above.
            if played.exit_code != 0 {
                Ok(1)
            } else if played.pushed == Sync::Offline {
                // The island stays here; the same state `sync push` answers with 5.
                Ok(fail(options, cx, &Error::RelayUnreachable, 5))
            } else {
                Ok(0)
            }
        }
        Err(error) => match guard.stopped.take() {
            Some(Stop::Conflict {
                step: Step::Pull,
                server,
                local,
            }) => {
                let error = Error::Saves(
                    "the island changed here and on the relay: nothing was changed".into(),
                );
                if !options.json {
                    writeln!(
                        cx.out,
                        "Both have a newer island. The game was not started."
                    )?;
                    print_card(cx.out, "relay", &server)?;
                    print_card(cx.out, "here", &local)?;
                    writeln!(cx.out, "Run play again with --keep relay or --keep here.")?;
                }
                Ok(fail(options, cx, &error, 3))
            }
            Some(Stop::Conflict {
                step: Step::Push,
                server,
                local,
            }) => {
                let error = Error::Saves(
                    "the island changed on the relay while the game ran: it was not sent".into(),
                );
                if !options.json {
                    writeln!(
                        cx.out,
                        "The game ran (exit {}) and the island here was kept, but not sent: \
                         the relay changed meanwhile.",
                        ran.code.get().unwrap_or(0)
                    )?;
                    print_card(cx.out, "relay", &server)?;
                    print_card(cx.out, "here", &local)?;
                    writeln!(
                        cx.out,
                        "Settle it with: acnh-universal sync push --keep relay|here"
                    )?;
                }
                Ok(fail(options, cx, &error, 6))
            }
            Some(Stop::Held { device, since }) => {
                let error = Error::Saves(format!(
                    "the island is open on {device} since {since}: the game was not started \
                     (--anyway plays regardless)"
                ));
                Ok(fail(options, cx, &error, 4))
            }
            None if error == Error::RelayUnreachable => Ok(fail(options, cx, &error, 5)),
            None => Err(error),
        },
    }
}

/// The player, remembering the code it quit with (a failed push afterwards still reports it).
struct Ran<'a> {
    inner: &'a dyn Game,
    code: Cell<Option<i32>>,
}

impl Game for Ran<'_> {
    fn run(&self, env: &[(String, String)]) -> Result<i32> {
        let code = self.inner.run(env)?;
        self.code.set(Some(code));
        Ok(code)
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub(crate) fn open_relay(cx: &Context<'_>) -> Result<Box<dyn Relay>> {
    // An error opening save sync stops play: there is no quiet fall back to no sync.
    Ok(
        match acnh_core::relay::open(&cx.sync.dirs()?, cx.sync.creds, cx.sync.transport.clone())? {
            Some(relay) => Box::new(relay),
            None => Box::new(RelayOff),
        },
    )
}

/// Android and the iPad get save sync through their own apps.
#[cfg(any(target_os = "android", target_os = "ios"))]
pub(crate) fn open_relay(_cx: &Context<'_>) -> Result<Box<dyn Relay>> {
    Ok(Box::new(RelayOff))
}

pub(crate) fn print_card(out: &mut dyn Write, side: &str, card: &savecard::SaveCard) -> Result<()> {
    let [saved, played] = savecard::side_by_side(card);
    let device = if card.device.is_empty() {
        "unknown device"
    } else {
        &card.device
    };
    writeln!(out, "  {side}: {device}, {saved}, {played}")?;
    Ok(())
}

/// Which relay step stopped.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Pull,
    Push,
}

/// Why a step stopped: it was told nothing about which island to keep.
pub(crate) enum Stop {
    Conflict {
        step: Step,
        server: savecard::SaveCard,
        local: savecard::SaveCard,
    },
    Held {
        device: String,
        since: String,
    },
}

/// The relay as `play` uses it: a conflict is answered only by `--keep`, a held island is
/// played only with `--anyway`; otherwise the step fails with the reason kept in `stopped`.
pub(crate) struct Guard<'a> {
    pub(crate) inner: &'a dyn Relay,
    pub(crate) keep: Option<Keep>,
    /// Whether `keep` also answers a push conflict. `play` leaves it off: a conflict after the
    /// game ran is never answered on its own (exit 6, settled with `sync push --keep`).
    pub(crate) answer_push: bool,
    pub(crate) anyway: bool,
    pub(crate) backups: &'a Path,
    pub(crate) stopped: RefCell<Option<Stop>>,
}

type Retry<'a> = &'a dyn Fn(Keep) -> Result<Sync>;

impl Guard<'_> {
    fn settle(&self, save_dir: &Path, step: Step, first: Sync, retry: Retry<'_>) -> Result<Sync> {
        let keep = match step {
            Step::Push if !self.answer_push => None,
            _ => self.keep,
        };
        match first {
            Sync::Conflict { server, local, .. } => match keep {
                Some(keep) => {
                    if keep == Keep::Server {
                        // The island here is about to be replaced: keep a copy of it first.
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|since| since.as_secs())
                            .unwrap_or(0);
                        acnh_core::backup::backup_saves(
                            save_dir,
                            self.backups,
                            &acnh_core::backup::stamp(now),
                        )?;
                    }
                    retry(keep)
                }
                None => {
                    *self.stopped.borrow_mut() = Some(Stop::Conflict {
                        step,
                        server,
                        local,
                    });
                    Err(Error::Saves(
                        "the island changed here and on the relay".into(),
                    ))
                }
            },
            other => Ok(other),
        }
    }

    fn held(&self, result: Sync) -> Result<Sync> {
        match result {
            Sync::Held { device, since } => {
                *self.stopped.borrow_mut() = Some(Stop::Held {
                    device: device.clone(),
                    since,
                });
                Err(Error::Saves(format!("the island is open on {device}")))
            }
            other => Ok(other),
        }
    }
}

impl Relay for Guard<'_> {
    fn pull(&self, save_dir: &Path, _resolve: Option<Keep>) -> Result<Sync> {
        let (first, anyway) = match self.inner.pull(save_dir, None)? {
            Sync::Held { .. } if self.anyway => (self.inner.pull_anyway(save_dir, None)?, true),
            other => (other, false),
        };
        // A held island that also changed here is still a conflict, answered only by --keep.
        let settled = self.settle(save_dir, Step::Pull, first, &|keep| {
            if anyway {
                self.inner.pull_anyway(save_dir, Some(keep))
            } else {
                self.inner.pull(save_dir, Some(keep))
            }
        })?;
        self.held(settled)
    }

    fn push(&self, save_dir: &Path, _resolve: Option<Keep>) -> Result<Sync> {
        let first = self.inner.push(save_dir, None)?;
        self.settle(save_dir, Step::Push, first, &|keep| {
            self.inner.push(save_dir, Some(keep))
        })
    }

    fn pull_anyway(&self, save_dir: &Path, resolve: Option<Keep>) -> Result<Sync> {
        self.inner.pull_anyway(save_dir, resolve)
    }

    fn status(&self, save_dir: &Path) -> Result<launch::RelayStatus> {
        self.inner.status(save_dir)
    }
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

pub(crate) fn report_sync(out: &mut dyn Write, step: &str, sync: &Sync) -> Result<()> {
    match sync {
        Sync::Done(message) | Sync::Resolved { message, .. } => writeln!(out, "{step}: {message}")?,
        Sync::Offline => writeln!(
            out,
            "{step}: the relay did not answer; the island stays here until it does"
        )?,
        Sync::Off => writeln!(out, "{step}: save sync is off")?,
        Sync::Held { .. } => writeln!(out, "{step}: held")?,
        Sync::Conflict { .. } => writeln!(out, "{step}: conflict")?,
    }
    Ok(())
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

fn code(stdin: &mut dyn BufRead) -> Result<String> {
    use std::io::{self, Write};
    eprint!("Enter the six-digit pairing code: ");
    io::stderr().flush()?;
    let mut code = String::new();
    stdin.read_line(&mut code)?;
    let code = code.trim().to_owned();
    hub::PairingSession::new(&code)?;
    Ok(code)
}

fn hub_command(options: &Options, cx: &mut Context<'_>) -> Result<i32> {
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
                Some(hub::PairingSession::new(&code(cx.stdin)?)?)
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
            hub::pair_hub(addr, &dirs, name, peer, &code(cx.stdin)?, target(options)?)?;
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

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn cloud_upload(options: &Options, cx: &mut Context<'_>) -> Result<i32> {
    let os = options.os.as_deref().unwrap_or("linux");
    if os != "linux" {
        return Err(Error::Unsupported(format!(
            "cloud-upload currently supports --os linux only (got {os})"
        )));
    }
    let install = options
        .install
        .as_deref()
        .ok_or_else(|| Error::Env("--install <folder> is required".into()))?;
    let prepared = options
        .prepared
        .as_deref()
        .ok_or_else(|| Error::Env("--prepared <folder> is required".into()))?;
    let sr_dirs = cx.sync.dirs()?;
    let relay = acnh_core::relay::open(
        &sr_dirs,
        cx.sync.creds,
        std::sync::Arc::clone(&cx.sync.transport),
    )?
    .ok_or_else(|| {
        Error::Saves("save sync is not set up (run sync sign-in dropbox first)".into())
    })?;
    let files = acnh_core::relay::collect_linux_cloud_files(install, prepared)?;
    let err = &mut *cx.err;
    let manifest = acnh_core::relay::upload_game_to_dropbox(
        &relay,
        os,
        &files,
        options.build_id.as_deref().unwrap_or(""),
        "",
        &mut |msg| {
            let _ = writeln!(err, "{msg}");
        },
    )?;
    let total_bytes: u64 = manifest.files.iter().map(|f| f.size).sum();
    let _ = writeln!(
        cx.out,
        "Uploaded {} files ({:.2} GB) to /acnh-universal-game/{os}/",
        manifest.files.len(),
        total_bytes as f64 / 1_000_000_000.0
    );
    Ok(0)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_build_folder_is_under_the_data_root() {
        let vars = Vars::from_lookup(Os::current(), |name| match name {
            "HOME" => Some("/home/player".to_owned()),
            "LOCALAPPDATA" | "APPDATA" => Some("/home/player/AppData".to_owned()),
            _ => None,
        })
        .unwrap();
        let out = default_build_out(&vars);
        assert!(out.starts_with(AppDirs::new(&vars).data_root()), "{out:?}");
        assert!(out.ends_with("prepared"));
        assert!(!out.starts_with(env!("CARGO_MANIFEST_DIR")));
    }
}
