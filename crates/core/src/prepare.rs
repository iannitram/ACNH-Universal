//! Prepare (SPEC.md M1, M13): from a checked game folder to a build for one device family, on
//! a hub. The Rust replacement of upstream's `tools/setup_acnh.py` and the scripts it runs
//! (`acnh_version.py`, `nso2bin.py`, `pack_romfs.py`, `pack_spirv.py`, `extract_shaders.py`,
//! `sarc_endian.py`, `msg_rewrap.py`), stage by stage (`recipe::Stage`), each skipped when its
//! output is already there. Until a stage is ported it runs upstream's script (`Step::Script`),
//! so a build works at every step of the move.
//!
//! Upstream's script is one command for all its stages, so while stages are unported one run
//! of it is followed through its `== <stage>` lines, as upstream's launcher follows it
//! (`launcher/pipeline.cpp`): `prepare` reports and waits on each stage as the script
//! reaches it, and names the stage the script failed in. `prepare` decides whether the
//! script runs at all and which stages are reported (those whose output was missing); inside
//! a run, upstream's own checks decide what is redone.
//!
//! Everything a prepare writes is made from my game folder: it stays in `out` on the hub, and
//! reaches my other devices only through the hub, over the home network (M11, M14).

use crate::error::{Error, Result};
use crate::gamefolder::GameFolder;
use crate::platform::Os;
use crate::recipe::{hub_for, output, serves, Hub, Stage, STAGES};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::thread::JoinHandle;

/// How a stage runs today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Our Rust code.
    Rust,
    /// Upstream's script, as program and arguments (paths as `Path::display` strings), run
    /// from the repo root.
    Script(Vec<String>),
}

/// What a prepare is asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub game: GameFolder,
    /// The build folder (upstream's `--out`).
    pub out: PathBuf,
    pub target: Os,
    /// Compile jobs; 0 lets the stage choose (upstream: free RAM / 700 MB, at most the cores).
    pub jobs: u32,
}

/// How `stage` runs for `req` right now. While no stage is ported, every stage is
/// `Script(["python3", "tools/setup_acnh.py", "--game", <game root>, "--out", <out>])`
/// (`python` instead of `python3` on Windows). Upstream's script has no argument that picks a
/// stage (its `FORCE` only redoes one), so the unported stages share this one command: a
/// runner starts it once and follows it stage by stage (`ProcessRunner`). A ported stage's
/// arm becomes `Step::Rust`.
pub fn step(stage: Stage, req: &Request) -> Step {
    match stage {
        Stage::Tools
        | Stage::Game
        | Stage::Version
        | Stage::Recompile
        | Stage::Shaders
        | Stage::Fonts
        | Stage::Build => Step::Script(vec![
            if req.target == Os::Windows {
                "python"
            } else {
                "python3"
            }
            .to_owned(),
            "tools/setup_acnh.py".to_owned(),
            "--game".to_owned(),
            req.game.root.display().to_string(),
            "--out".to_owned(),
            req.out.display().to_string(),
        ]),
    }
}

/// Runs the script of a stage. `Err` holds the log lines of a failed run; a line
/// `== <stage>` in them names the stage the script was in (`Stage::from_line`).
pub trait ScriptRunner {
    /// Run `args` (a `Step::Script`) for `stage`, and come back when that stage of it is over.
    fn run(&mut self, stage: Stage, args: &[String]) -> std::result::Result<(), Vec<String>>;

    /// No stage is left to ask for: wait for what `run` left running. A runner whose `run`
    /// leaves nothing running has nothing to do.
    fn finish(&mut self) -> std::result::Result<(), Vec<String>> {
        Ok(())
    }
}

/// The real runner: upstream's command as a child process in the repo root, started once
/// for all the stages that share it, as upstream's launcher does (`launcher/pipeline.cpp`).
///
/// Upstream's script goes through every stage by itself and prints `== <stage>` as each one
/// begins. `run` starts the command at the first stage asked for, reads its output, and
/// returns when the script begins a later stage; the next `run` picks the same child up
/// there, so a stage is under way exactly while we wait on it. The script cannot start at a
/// stage or stop after one: before the first stage asked for and after the last one it does
/// its own checks, skipping what is already there, and `finish` waits for it to end. A
/// failed run's log starts at the `== <stage>` line of the stage it failed in.
pub struct ProcessRunner {
    root: PathBuf,
    jobs: u32,
    script: Option<Script>,
}

/// One run of upstream's command.
struct Script {
    args: Vec<String>,
    child: Child,
    stdout: BufReader<ChildStdout>,
    stderr: Option<JoinHandle<Vec<String>>>,
    /// The stage of its last `== <stage>` line.
    at: Option<Stage>,
    /// Its output from that line on.
    log: Vec<String>,
    /// It ended, with success.
    ended: bool,
}

impl ProcessRunner {
    /// A runner for scripts in `root` (the repo root). `jobs` above 0 is passed on as
    /// upstream's `JOBS`.
    pub fn new(root: PathBuf, jobs: u32) -> ProcessRunner {
        ProcessRunner {
            root,
            jobs,
            script: None,
        }
    }

    fn follow(&mut self, until: Option<Stage>) -> std::result::Result<(), Vec<String>> {
        let Some(script) = self.script.as_mut() else {
            return Ok(());
        };
        let result = script.follow(until);
        if result.is_err() {
            self.script = None;
        }
        result
    }
}

impl ScriptRunner for ProcessRunner {
    fn run(&mut self, stage: Stage, args: &[String]) -> std::result::Result<(), Vec<String>> {
        if self.script.as_ref().is_some_and(|s| s.args != args) {
            self.finish()?; // another command: the one before it ends first
        }
        if self.script.is_none() {
            self.script = Some(Script::start(&self.root, self.jobs, args)?);
        }
        self.follow(Some(stage))
    }

    fn finish(&mut self) -> std::result::Result<(), Vec<String>> {
        let result = self.follow(None);
        self.script = None;
        result
    }
}

impl Script {
    /// Start `args` in `root`, as upstream's launcher starts its setup: no input, unbuffered
    /// Python output (a stage line arrives when its stage begins). `FORCE` does not pass: it
    /// redoes work that is there, and `FORCE=game` deletes an imported copy of the game.
    fn start(root: &Path, jobs: u32, args: &[String]) -> std::result::Result<Script, Vec<String>> {
        let Some((program, rest)) = args.split_first() else {
            return Err(vec!["no command".to_owned()]);
        };
        let mut command = Command::new(program);
        command
            .args(rest)
            .current_dir(root)
            .env("PYTHONUNBUFFERED", "1")
            .env_remove("FORCE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if jobs > 0 {
            command.env("JOBS", jobs.to_string());
        }
        let mut child = command
            .spawn()
            .map_err(|e| vec![format!("cannot run {program}: {e}")])?;
        // Standard error is read on its own thread, so a script that fills it never blocks.
        let stderr = child.stderr.take().map(|pipe| {
            let mut pipe = BufReader::new(pipe);
            std::thread::spawn(move || std::iter::from_fn(|| next_line(&mut pipe)).collect())
        });
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(vec![format!("cannot read the output of {program}")]);
        };
        Ok(Script {
            args: args.to_vec(),
            child,
            stdout: BufReader::new(stdout),
            stderr,
            at: None,
            log: Vec::new(),
            ended: false,
        })
    }

    /// Read the script's output until it begins a stage after `until` (`None`: until it
    /// ends). `Err` when it ended with a failure: the log of the stage it was in, then what
    /// it wrote to standard error.
    fn follow(&mut self, until: Option<Stage>) -> std::result::Result<(), Vec<String>> {
        while !self.ended {
            if until.is_some_and(|until| self.at.is_some_and(|at| at > until)) {
                return Ok(());
            }
            let Some(line) = next_line(&mut self.stdout) else {
                break;
            };
            if let Some(stage) = Stage::from_line(&line) {
                self.at = Some(stage);
                self.log.clear();
            }
            self.log.push(line);
        }
        if self.ended {
            return Ok(());
        }
        let status = self.child.wait();
        self.ended = true;
        let mut log = std::mem::take(&mut self.log);
        if let Some(stderr) = self.stderr.take() {
            log.extend(stderr.join().unwrap_or_default());
        }
        match status {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => {
                if log.is_empty() {
                    log.push(format!("{}: {status}", self.args.join(" ")));
                }
                Err(log)
            }
            Err(e) => {
                log.push(format!("cannot wait for {}: {e}", self.args.join(" ")));
                Err(log)
            }
        }
    }
}

impl Drop for Script {
    /// A run nobody waits for any more (a failed prepare) does not go on behind our back.
    fn drop(&mut self) {
        if !self.ended {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// The next line of `pipe` without its line end, bytes that are not UTF-8 replaced; `None`
/// at its end.
fn next_line(pipe: &mut impl BufRead) -> Option<String> {
    let mut bytes = Vec::new();
    match pipe.read_until(b'\n', &mut bytes) {
        Ok(read) if read > 0 => Some(
            String::from_utf8_lossy(&bytes)
                .trim_end_matches(['\n', '\r'])
                .to_owned(),
        ),
        _ => None,
    }
}

/// Where a passed tools stage is recorded: upstream keeps its tools in the repo's `private/`
/// folder, so the stage leaves nothing of its own in `out`.
fn tools_record(req: &Request) -> PathBuf {
    req.out.join(".prepare-tools.json")
}

/// Whether everything `stage` leaves in `req.out` is there. Game and recompile leave one
/// file per module; upstream's build leaves the player itself in `build/` (on the Mac the
/// plain executable: the app bundle is stage 8's).
fn stage_complete(stage: Stage, req: &Request) -> bool {
    const MODULES: [&str; 3] = ["main", "sdk", "subsdk0"];
    match stage {
        Stage::Tools => tools_record(req).is_file(),
        Stage::Version => req.out.join("version.ini").is_file(),
        Stage::Game => MODULES.iter().all(|module| {
            req.out
                .join("analysis")
                .join(format!("{module}.bin"))
                .is_file()
        }),
        Stage::Recompile => MODULES
            .iter()
            .all(|module| req.out.join("gen").join(module).join("table.cpp").is_file()),
        Stage::Shaders => req.out.join("spirv").join("summary.txt").is_file(),
        Stage::Fonts => req
            .out
            .join("fonts")
            .join("glyphs_xbox")
            .join("console")
            .join("romfs")
            .join("Font")
            .join("ScalableFont.sarc.zs")
            .is_file(),
        Stage::Build => req
            .out
            .join("build")
            .join(if req.target == Os::Mac {
                "acnh"
            } else {
                output(req.target)
            })
            .is_file(),
    }
}

/// The repo root upstream's scripts run from: the checkout this was built from (as
/// upstream's launcher falls back to its own source folder).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Run every stage in order, calling `progress` as each one starts, and return `req.out`. A
/// stage whose output was already in `req.out` is skipped, and when every stage's is, nothing
/// runs. A failed stage is `Error::Build` naming it, with the last 20 lines of its log, run
/// through `redact`. A target this hub does not serve (`recipe::serves`), or one upstream's
/// script does not build (Android, iOS), is `Error::Unsupported`.
pub fn prepare(req: &Request, progress: &mut dyn FnMut(Stage)) -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    prepare_with(
        req,
        hub_for(Os::current()),
        &mut ProcessRunner::new(repo_root(), req.jobs),
        &home,
        progress,
    )
}

/// `prepare` for a given hub, script runner and home folder (the seam the tests use).
pub fn prepare_with(
    req: &Request,
    hub: Hub,
    runner: &mut dyn ScriptRunner,
    home: &Path,
    progress: &mut dyn FnMut(Stage),
) -> Result<PathBuf> {
    if !serves(hub).contains(&req.target) {
        return Err(Error::Unsupported(format!(
            "the {} hub does not build for {}",
            hub.key(),
            req.target.key()
        )));
    }
    // Upstream's script builds the desktop player of the machine it runs on. Its Android
    // route is another script with its own data folder (ADR 0005) and iOS has none yet
    // (stage 8): until those are built, neither is answered with the hub's desktop build.
    if matches!(req.target, Os::Android | Os::Ios) {
        return Err(Error::Unsupported(format!(
            "no stage builds for {} yet",
            req.target.key()
        )));
    }
    // What was there before anything ran. A script that runs on while we look may have
    // finished its next stage already: only an output from before skips a stage.
    let before = STAGES.map(|stage| stage_complete(stage, req));
    let mut last = None;
    for (stage, was_there) in STAGES.into_iter().zip(before) {
        if was_there && stage_complete(stage, req) {
            continue;
        }
        progress(stage);
        if let Step::Script(args) = step(stage, req) {
            runner
                .run(stage, &args)
                .map_err(|log| build_error(stage, &log, home))?;
        }
        if stage == Stage::Tools {
            std::fs::create_dir_all(&req.out)?;
            std::fs::write(tools_record(req), "{\"tools\": \"ready\"}\n")?;
        }
        last = Some(stage);
    }
    if let Some(stage) = last {
        runner
            .finish()
            .map_err(|log| build_error(stage, &log, home))?;
    }
    Ok(req.out.clone())
}

/// `Error::Build` for the failed `log` of a script run for `stage`: the stage is the one the
/// log names last (`== <stage>`, when upstream's script failed in a stage we were not waiting
/// on), else `stage`; the text is its last 20 lines, each through `redact`.
fn build_error(stage: Stage, log: &[String], home: &Path) -> Error {
    let stage = log
        .iter()
        .rev()
        .find_map(|line| Stage::from_line(line))
        .unwrap_or(stage);
    let tail: Vec<String> = log[log.len().saturating_sub(20)..]
        .iter()
        .map(|line| redact(line, home))
        .collect();
    Error::Build(format!("stage {}: {}", stage.key(), tail.join("\n")))
}

/// A log line made safe to show or report: the home folder (`home`) becomes `~`, and anything
/// that looks like a key (a run of 32 or more hex digits that is not a build id the gate
/// knows) becomes `<hex>`. Game file names stay; their contents never reach a log.
pub fn redact(line: &str, home: &Path) -> String {
    let home = home.display().to_string();
    let line = if home.is_empty() {
        line.to_owned()
    } else {
        line.replace(&home, "~")
    };
    let mut redacted = String::with_capacity(line.len());
    let mut run_start = None;

    for (index, character) in line.char_indices() {
        if character.is_ascii_hexdigit() {
            run_start.get_or_insert(index);
        } else if let Some(start) = run_start.take() {
            redact_hex_run(&line[start..index], &mut redacted);
            redacted.push(character);
        } else {
            redacted.push(character);
        }
    }

    if let Some(start) = run_start {
        redact_hex_run(&line[start..], &mut redacted);
    }

    redacted
}

fn redact_hex_run(run: &str, redacted: &mut String) {
    if run.len() >= 32 && crate::version::from_build_id(run).is_err() {
        redacted.push_str("<hex>");
    } else {
        redacted.push_str(run);
    }
}
