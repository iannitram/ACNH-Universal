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
//! During the game stage the script extracts mods, then Rust reflows their dialogue
//! before Prepare advances past the script's version marker.

pub use crate::assemble::assemble;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::gamefolder::GameFolder;
use crate::platform::{AppDirs, Os, Vars};
use crate::recipe::{hub_for, output, serves, Hub, Stage, STAGES};
use std::ffi::OsStr;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::thread::JoinHandle;

/// The file a packaged source holds at its root: the commit the tree was cut from.
const SOURCE_FILE: &str = "SOURCE";

/// How a stage runs today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Our Rust code.
    Rust,
    /// Upstream's script, as program and arguments (paths as `Path::display` strings), run
    /// from the repo root.
    Script(Vec<String>),
}

/// Fan mod choices, off on a fresh install.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FanMods {
    pub ptbr: bool,
    pub speedfix: bool,
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
    /// The source tree to build from. `None` finds the packaged source beside the executable,
    /// and keeps the checkout when there is none (a `cargo run`).
    pub source: Option<PathBuf>,
    /// Snapshot of the two opt-in settings for the whole prepare.
    pub fan_mods: FanMods,
    /// Where a target's final deliverable goes (`delivery_folder`): `<dropbox>/_transfers/
    /// ACNH-Universal/<ios|mac>/`. The `ios` and `mac` targets need it; the others ignore it.
    pub delivery: Option<PathBuf>,
    /// The folder the `mac` target assembles its app in (`<data>/install`); the other targets
    /// ignore it.
    pub install: Option<PathBuf>,
}

/// The folders of a delivery under the Dropbox folder: `_transfers/ACNH-Universal/<target>/`.
pub const DELIVERY_ROOT: [&str; 2] = ["_transfers", "ACNH-Universal"];

/// The delivery subfolder of `target`: "ios" for the iPhone and iPad, "mac" for the Mac,
/// `None` for the others (nothing of theirs goes to Dropbox).
pub fn delivery_subfolder(target: Os) -> Option<&'static str> {
    match target {
        Os::Ios => Some("ios"),
        Os::Mac => Some("mac"),
        _ => None,
    }
}

/// The delivery folder `<dropbox>/_transfers/ACNH-Universal/<subfolder>/` for `subfolder`
/// ("ios" or "mac"): `<dropbox>` is `launcher.ini`'s `dropbox` key when it is set and not
/// empty, else `<home>/Dropbox`.
pub fn delivery_folder(config: &Config, home: &Path, subfolder: &str) -> PathBuf {
    let dropbox = match config.get("dropbox") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => home.join("Dropbox"),
    };
    DELIVERY_ROOT
        .iter()
        .fold(dropbox, |path, part| path.join(part))
        .join(subfolder)
}

/// Stop with `Error::Build` unless the `_transfers/ACNH-Universal/` folder above `delivery`
/// is there. The message names the `dropbox` key of `launcher.ini` (paths through `redact`).
pub fn check_delivery(delivery: &Path, home: &Path) -> Result<()> {
    let root = delivery.parent().unwrap_or(delivery);
    if root.is_dir() {
        return Ok(());
    }
    Err(Error::Build(format!(
        "the folder {} is not there: make it in your Dropbox, or set the `dropbox` key in \
         launcher.ini to the folder that holds _transfers/ACNH-Universal/",
        redact(&root.display().to_string(), home)
    )))
}

/// The arguments of `tools/build_ios.sh`: the `iphoneos` SDK, the recompiled code in `gen`,
/// and the folder the build is made in.
fn build_ios_args(gen: &Path, build: &Path) -> Vec<String> {
    vec![
        "tools/build_ios.sh".to_owned(),
        "iphoneos".to_owned(),
        gen.display().to_string(),
        build.display().to_string(),
    ]
}

/// The arguments of `tools/package_ios.sh`: the game inside the `.ipa` comes from the
/// prepared folder `game`.
fn package_ios_args(version: &str, build: &Path, game: &Path, delivery: &Path) -> Vec<String> {
    vec![
        "tools/package_ios.sh".to_owned(),
        version.to_owned(),
        build.display().to_string(),
        "--game".to_owned(),
        game.display().to_string(),
        delivery.display().to_string(),
    ]
}

/// The arguments of `tools/package_mac.sh`: the contained app is the assembled `app`, with
/// the prepared folder `game` inside it.
fn package_mac_args(version: &str, app: &Path, game: &Path, delivery: &Path) -> Vec<String> {
    vec![
        "tools/package_mac.sh".to_owned(),
        version.to_owned(),
        app.display().to_string(),
        game.display().to_string(),
        delivery.display().to_string(),
    ]
}

/// The version the Apple packages are named with: the engine's.
fn package_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The `.zip` a Mac prepare makes in `delivery` (`package_mac.sh` names it).
fn zip_path(delivery: &Path) -> PathBuf {
    delivery.join(format!("ACNH-Universal-{}.zip", package_version()))
}

/// The prepared folder of `os`'s own target in `dirs`: on the Mac `<data root>/prepared/`
/// (one folder for both Apple targets), elsewhere `<data>/prepare/`.
pub fn prepared_folder(os: Os, dirs: &AppDirs) -> PathBuf {
    if os == Os::Mac {
        dirs.data_root().join("prepared")
    } else {
        dirs.data.join("prepare")
    }
}

/// The `.ipa` an iOS prepare makes in `delivery` (`package_ios.sh` names it).
fn ipa_path(delivery: &Path) -> PathBuf {
    delivery.join(format!("ACNH-Universal-{}.ipa", package_version()))
}

/// A build tool a hub is missing, and how to install it. Required pieces stop a prepare
/// before its first stage; a warning names what the build loses without them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    /// The tool or package, e.g. "clang" or "molten-vk".
    pub what: String,
    /// A plain message naming what is missing and the command that installs it.
    pub message: String,
    /// The command that installs it (also part of `message`).
    pub install: String,
    /// Only a warning: the build goes on, without what this piece gives it.
    pub warning: bool,
}

impl std::fmt::Display for Missing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// How `stage` runs for `req` right now. The shaders stage's packs are ours now
/// (`pack::romfs`, `pack::spirv`) and it is Rust (`sarc`, `shaders`); the rest is
/// `Script(["python3", "tools/setup_acnh.py", "--game", <game root>, "--out", <out>])`
/// (`python` instead of `python3` on Windows). Upstream's script has no argument that picks a
/// stage (its `FORCE` only redoes one), so the unported stages share this one command: a
/// runner starts it once and follows it stage by stage (`ProcessRunner`). A ported stage's
/// arm becomes `Step::Rust`.
pub fn step(stage: Stage, req: &Request) -> Step {
    match stage {
        Stage::Shaders => Step::Rust,
        Stage::Tools | Stage::Game | Stage::Version | Stage::Recompile => {
            let mut args = stage_script(req);
            // The script ends before the shaders stage: that one is ours.
            args.push("--stop-before-shaders".to_owned());
            Step::Script(args)
        }
        Stage::Fonts | Stage::Build => Step::Script(stage_script(req)),
    }
}

/// Upstream's one command for a stage that is still its script.
fn stage_script(req: &Request) -> Vec<String> {
    let mut args = vec![
        script_python(req.target).to_owned(),
        "tools/setup_acnh.py".to_owned(),
        "--game".to_owned(),
        req.game.root.display().to_string(),
        "--out".to_owned(),
        req.out.display().to_string(),
    ];
    if req.fan_mods.ptbr {
        args.push("--ptbr".to_owned());
    }
    if req.fan_mods.speedfix {
        args.push("--speedfix".to_owned());
    }
    args
}

fn script_python(target: Os) -> &'static str {
    if target == Os::Windows {
        "python"
    } else {
        "python3"
    }
}

/// Runs the script of a stage. `Err` holds the log lines of a failed run; a line
/// `== <stage>` in them names the stage the script was in (`Stage::from_line`).
pub trait ScriptRunner {
    /// Run `args` (a `Step::Script`) for `stage`, and come back when that stage of it is over.
    fn run(&mut self, stage: Stage, args: &[String]) -> std::result::Result<(), Vec<String>>;

    /// Wait for any first-pass setup child before shaders inspect its output.
    fn before_shaders(&mut self) -> std::result::Result<(), Vec<String>> {
        Ok(())
    }

    /// The Rust part of the shaders stage. The default runs it (`rust_shaders`); a test runner
    /// may stand in for it, as no translator is installed on a test machine.
    fn shaders(&mut self, req: &Request) -> Result<()> {
        rust_shaders(req, &self.root())
    }

    /// The source folder the tools are built under (what `set_root` last gave). The default
    /// is the checkout this was built from.
    fn root(&self) -> PathBuf {
        repo_root()
    }

    /// No stage is left to ask for: wait for what `run` left running. A runner whose `run`
    /// leaves nothing running has nothing to do.
    fn finish(&mut self) -> std::result::Result<(), Vec<String>> {
        Ok(())
    }

    /// The source folder the script runs in (`prepare` sets it before the first stage). A
    /// runner that starts the script itself uses it; a test runner may record it. The
    /// default does nothing.
    fn set_root(&mut self, _root: &Path) {}

    /// Where the game-stage script leaves work for Rust after mod extraction.
    fn set_message_paths(&mut self, _game_romfs: &Path, _out: &Path) {}

    /// Assemble the Mac's contained app from `build` in `install` (1.22b): the app's path. A
    /// test runner stands in for the machine's tools.
    fn assemble(&mut self, build: &Path, install: &Path) -> Result<PathBuf> {
        assemble(Os::Mac, build, install)
    }

    /// Run `program` with `args` and say whether it succeeded and what it wrote to standard
    /// error; `None` when it cannot be started. The pre-flight asks it only to tell a folder
    /// that is not a git repository from a git that failed.
    fn probe_stderr(&mut self, program: &str, args: &[&str]) -> Option<(bool, String)> {
        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .output()
            .ok()?;
        Some((
            output.status.success(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }

    /// Run `program` with `args` for its output, as the pre-flight checks of the `ios` target
    /// do (`xcrun`, `cargo`, `rustup`): `Some(stdout)` when it ran and succeeded, else `None`.
    /// A test runner stands in for the machine's tools.
    fn probe(&mut self, program: &str, args: &[&str]) -> Option<String> {
        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// `path` made absolute against the working folder (it need not exist).
fn absolute(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path)
        .map_err(|e| Error::Io(format!("cannot make {} absolute: {e}", path.display())))
}

/// The folders a Finder-launched app's minimal `PATH` misses: cargo's, then Homebrew's.
fn tool_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(cargo_home) = std::env::var_os("CARGO_HOME").filter(|v| !v.is_empty()) {
        dirs.push(PathBuf::from(cargo_home).join("bin"));
    }
    dirs.push(home.join(".cargo/bin"));
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs
}

/// `program` as `runner.probe` finds it: on the `PATH`, else in one of `tool_dirs`. The
/// output, and the folder when it was not on the `PATH`.
fn probe_tool(
    runner: &mut dyn ScriptRunner,
    home: &Path,
    program: &str,
    args: &[&str],
) -> Option<(String, Option<PathBuf>)> {
    if let Some(output) = runner.probe(program, args) {
        return Some((output, None));
    }
    tool_dirs(home).into_iter().find_map(|dir| {
        let found = dir.join(program);
        runner
            .probe(&found.display().to_string(), args)
            .map(|output| (output, Some(dir)))
    })
}

/// The folder `cargo` is in when the `PATH` does not have it (to put first on the `PATH` of
/// the build script, which calls cargo).
fn cargo_dir(runner: &mut dyn ScriptRunner, home: &Path) -> Option<PathBuf> {
    probe_tool(runner, home, "cargo", &["--version"]).and_then(|(_, dir)| dir)
}

/// The checks before a prepare starts, which stop it before its first stage. The `mac` target:
/// the delivery folder (`check_delivery`) and a prepared folder outside any git work tree. The
/// `ios` target: the same, and Xcode's `iphoneos` SDK (the command-line tools alone do not
/// have it), `cargo` and the `aarch64-apple-ios` Rust target. Other targets have none here.
/// `Error::Build` naming what is missing; the tools are found with `runner.probe`.
pub fn preflight(req: &Request, runner: &mut dyn ScriptRunner, home: &Path) -> Result<()> {
    if !matches!(req.target, Os::Ios | Os::Mac) {
        return Ok(());
    }
    let delivery = req.delivery.as_deref().ok_or_else(|| {
        Error::Env("the delivery folder is not set (the `dropbox` key in launcher.ini)".into())
    })?;
    check_delivery(delivery, home)?;
    if req.target == Os::Ios {
        check_ios_tools(runner, home)?;
    }
    check_outside_git(req, runner, home)
}

/// The iPhone build's tools: Xcode's SDK, `cargo` and the Rust target.
fn check_ios_tools(runner: &mut dyn ScriptRunner, home: &Path) -> Result<()> {
    let sdk = runner.probe("xcrun", &["--sdk", "iphoneos", "--show-sdk-path"]);
    if !sdk.is_some_and(|path| !path.trim().is_empty()) {
        return Err(Error::Build(
            "building for iPhone and iPad needs Xcode with its iphoneos SDK (the command-line \
             tools alone do not have it): install Xcode from the App Store, open it once, then \
             run `sudo xcode-select -s /Applications/Xcode.app`"
                .to_owned(),
        ));
    }
    if probe_tool(runner, home, "cargo", &["--version"]).is_none() {
        return Err(Error::Build(
            "building for iPhone and iPad needs cargo: install Rust from rustup.rs".to_owned(),
        ));
    }
    let targets = probe_tool(runner, home, "rustup", &["target", "list", "--installed"]);
    if !targets.is_some_and(|(list, _)| list.lines().any(|line| line.trim() == "aarch64-apple-ios"))
    {
        return Err(Error::Build(
            "the Rust target for iPhone and iPad is not installed: run \
             `rustup target add aarch64-apple-ios`"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Stop unless the prepared folder is outside every git work tree: it holds the recompiled
/// game.
fn check_outside_git(req: &Request, runner: &mut dyn ScriptRunner, home: &Path) -> Result<()> {
    // The nearest folder that exists, as `git -C` needs one; a relative `out` is taken from
    // the working folder.
    let out = absolute(&req.out)?;
    let mut existing = out.as_path();
    while !existing.is_dir() {
        match existing.parent() {
            Some(parent) => existing = parent,
            None => break,
        }
    }
    let git = [
        "-C",
        &existing.display().to_string(),
        "rev-parse",
        "--is-inside-work-tree",
    ];
    match runner.probe("git", &git) {
        Some(answer) if answer.trim() == "true" => Err(Error::Build(format!(
            "the prepared folder {} is inside a git work tree: the Apple builds hold the \
             recompiled game and must not be made there. Use a folder outside it.",
            redact(&out.display().to_string(), home)
        ))),
        Some(_) => Ok(()),
        // Git failed: only its own word that this is no repository lets the build go on.
        None => match runner.probe_stderr("git", &git) {
            Some((false, error)) if error.contains("not a git repository") => Ok(()),
            _ => Err(Error::Build(format!(
                "cannot tell whether {} is inside a git work tree (is git installed?): the \
                 Apple builds hold the recompiled game and must not be made in one",
                redact(&out.display().to_string(), home)
            ))),
        },
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
    messages: Option<MessagePaths>,
}

struct MessagePaths {
    game_romfs: PathBuf,
    out: PathBuf,
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
            messages: None,
        }
    }

    fn follow(&mut self, until: Option<Stage>) -> std::result::Result<(), Vec<String>> {
        let Some(script) = self.script.as_mut() else {
            return Ok(());
        };
        let result = script.follow(until, self.messages.as_ref());
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
            self.script = Some(Script::start(
                &self.root,
                self.jobs,
                args,
                self.messages.is_some(),
            )?);
        }
        self.follow(Some(stage))
    }

    fn before_shaders(&mut self) -> std::result::Result<(), Vec<String>> {
        self.finish()
    }

    fn finish(&mut self) -> std::result::Result<(), Vec<String>> {
        let result = self.follow(None);
        self.script = None;
        result
    }

    fn root(&self) -> PathBuf {
        self.root.clone()
    }

    fn set_root(&mut self, root: &Path) {
        if self.root == root {
            return;
        }
        if self.script.is_some() {
            self.script = None; // dropping it ends the child
        }
        self.root = root.to_owned();
    }

    fn set_message_paths(&mut self, game_romfs: &Path, out: &Path) {
        self.messages = Some(MessagePaths {
            game_romfs: game_romfs.to_owned(),
            out: out.to_owned(),
        });
    }
}

impl Script {
    /// Start `args` in `root`, as upstream's launcher starts its setup: no input, unbuffered
    /// Python output (a stage line arrives when its stage begins). `FORCE` does not pass: it
    /// redoes work that is there, and `FORCE=game` deletes an imported copy of the game.
    fn start(
        root: &Path,
        jobs: u32,
        args: &[String],
        rust_reflow: bool,
    ) -> std::result::Result<Script, Vec<String>> {
        let Some((program, rest)) = args.split_first() else {
            return Err(vec!["no command".to_owned()]);
        };
        let mut command = Command::new(program);
        command
            .args(rest)
            .current_dir(root)
            .env("PYTHONUNBUFFERED", "1")
            .env_remove("FORCE")
            .env_remove("ACNH_RUST_MSG_REFLOW")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if rust_reflow {
            command.env("ACNH_RUST_MSG_REFLOW", "1");
        }
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
    fn follow(
        &mut self,
        until: Option<Stage>,
        messages: Option<&MessagePaths>,
    ) -> std::result::Result<(), Vec<String>> {
        while !self.ended {
            if until.is_some_and(|until| self.at.is_some_and(|at| at > until)) {
                return Ok(());
            }
            let Some(line) = next_line(&mut self.stdout) else {
                break;
            };
            if let Some(stage) = Stage::from_line(&line) {
                // Both setup passes revisit the game stage. Its mod extraction has
                // ended at this marker, even if prepare skipped waiting on Game itself.
                if stage == Stage::Version {
                    if let Some(paths) = messages {
                        crate::msg::finish_pending(&paths.game_romfs, &paths.out, &self.args[0])
                            .map_err(|error| vec!["== game".to_owned(), error.to_string()])?;
                    }
                }
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

/// A mod's `.sarc.zs` archives as the game reads them (`sarc::fix_archives`), for every mod
/// installed in `out/mods/<name>/romfs`.
fn fix_mod_archives(req: &Request) -> Result<()> {
    let Ok(mods) = std::fs::read_dir(req.out.join("mods")) else {
        return Ok(());
    };
    let mut romfs: Vec<PathBuf> = mods
        .filter_map(|entry| entry.ok().map(|entry| entry.path().join("romfs")))
        .filter(|path| path.is_dir())
        .collect();
    romfs.sort();
    for path in romfs {
        crate::sarc::fix_archives(&path)?;
    }
    Ok(())
}

/// The Rust part of the shaders stage: pack the RomFS and the stage's `.spv` files for the
/// runtime (`pack::romfs`, `pack::spirv`). A source that is not there yet leaves its pack
/// alone, as upstream keeps the loose folder until something packs it.
fn pack_stage(req: &Request) -> Result<()> {
    std::fs::create_dir_all(&req.out)?;
    if req.game.romfs.is_dir() {
        crate::pack::romfs(&req.game.romfs, &req.out.join("romfs.pak"))?;
    }
    let spirv = req.out.join("spirv");
    if spirv.is_dir() {
        crate::pack::spirv(&spirv, &req.out.join("spirv.pak"))?;
    }
    Ok(())
}

/// The shaders stage, in Rust: extract the shaders, translate them with the tools' .NET
/// harness (built by the tools stage), fix the mods' archives and pack. The script has ended
/// before this stage and starts again after it.
fn rust_shaders(req: &Request, root: &Path) -> Result<()> {
    let stage = Stage::Shaders;
    let invalidate = |error| {
        let _ = std::fs::remove_file(req.out.join("spirv/summary.txt"));
        Error::Build(format!("stage {}: {error}", stage.key()))
    };
    fix_mod_archives(req).map_err(invalidate)?;
    crate::shaders::stage_with(
        &mut crate::shaders::ProcessRunner,
        root,
        &req.game.romfs,
        &req.out.join("analysis/main.bin"),
        &req.out,
        req.jobs,
    )
    .map_err(|error| Error::Build(format!("stage {}: {error}", stage.key())))?;
    // A summary only marks the stage complete once its archives and packs are ready.
    pack_stage(req).map_err(invalidate)
}

/// The Rust fonts stage for the Prepare runner. The legacy script still owns
/// stage dispatch until the remaining script stages can switch together.
pub fn rust_fonts(req: &Request) -> Result<()> {
    let host = [
        r"C:\Windows\Fonts\arial.ttf",
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
    ]
    .into_iter()
    .map(Path::new)
    .find(|path| path.is_file());
    crate::glyphs::stage(&req.out.join("extract/romfs"), &req.out.join("fonts"), host)
        .map_err(|error| Error::Build(format!("stage {}: {error}", Stage::Fonts.key())))
}

/// Whether everything `stage` leaves in `req.out` is there. Game and recompile leave one
/// file per module; upstream's build leaves the bare player in `build/` (on the Mac the
/// plain executable), which `assemble` then makes self-contained in the install folder.
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
            .join(if matches!(req.target, Os::Mac | Os::Ios) {
                "acnh"
            } else {
                output(req.target)
            })
            .is_file(),
    }
}

/// The repo root upstream's scripts run from: the checkout this was built from (as
/// upstream's launcher falls back to its own source folder).
pub(crate) fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The packaged source beside an executable: `source/` in the Windows zip, `../Resources/source`
/// in the Mac app, and `../share/acnh-universal/source` in the AppImage.
fn packaged_source(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    [
        "source",
        "../Resources/source",
        "../share/acnh-universal/source",
    ]
    .into_iter()
    .map(|relative| dir.join(relative))
    .find(|source| source.join(SOURCE_FILE).is_file())
}

/// The commit a packaged source names, or `None` when the folder is a checkout.
fn source_commit(source: &Path) -> Option<String> {
    let text = std::fs::read_to_string(source.join(SOURCE_FILE)).ok()?;
    let commit = text.lines().next()?.trim();
    (!commit.is_empty()).then(|| commit.to_owned())
}

/// Copy a source tree, replacing the last copy of its commit.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    if to.exists() {
        std::fs::remove_dir_all(to)?;
    }
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// The folder the script runs in: the named source, else the packaged source beside `exe`,
/// else the checkout. A packaged source is copied to `<data>/source/<commit>/` because the
/// package may be read-only and the script writes into `private/`. A package from another
/// commit is refused.
fn source_root(req: &Request, exe: &Path, dirs: &AppDirs, commit: Option<&str>) -> Result<PathBuf> {
    let source = req
        .source
        .clone()
        .or_else(|| packaged_source(exe))
        .unwrap_or_else(repo_root);
    let Some(found) = source_commit(&source) else {
        return Ok(source);
    };
    let expected = commit.unwrap_or(found.as_str());
    if found.as_str() != expected {
        return Err(Error::Io(format!(
            "the source in this package is commit {found}, not {expected}: use the package \
             built from the app's own source"
        )));
    }
    let copy = dirs.data.join("source").join(&found);
    copy_tree(&source, &copy)?;
    Ok(copy)
}

/// The commit this binary was built from (the release sets `ACNH_SOURCE_COMMIT`).
fn app_commit() -> Option<&'static str> {
    option_env!("ACNH_SOURCE_COMMIT")
        .map(str::trim)
        .filter(|commit| !commit.is_empty())
}

/// Run every stage in order, calling `progress` as each one starts, and return `req.out`. A
/// stage whose output was already in `req.out` is skipped. When all outputs exist, Prepare
/// only recovers interrupted message reflow. A failed stage is `Error::Build` naming it,
/// with the last 20 lines of its log, run
/// through `redact`. Missing required tools are `Error::Build` before the first stage. A
/// target this hub does not serve (`recipe::serves`), or one upstream's script does not build
/// (Android), is `Error::Unsupported`. For the `ios` target (Mac hub, `Request::delivery`
/// set) it runs `preflight`, the shared stages, the iPhone build and the packaging, and
/// returns the `.ipa`'s path in the delivery folder instead of `Request::out`. The `mac` target
/// (`Request::install` too) does the same with the contained app and returns the `.zip`'s path.
pub fn prepare(req: &Request, progress: &mut dyn FnMut(Stage)) -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    let exe = std::env::current_exe()
        .map_err(|e| Error::Io(format!("cannot find this app's executable: {e}")))?;
    let vars = Vars::from_lookup(Os::current(), |name| std::env::var(name).ok())?;
    let dirs = AppDirs::new(&vars);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let missing = toolchain(Os::current(), &path);
    prepare_installed(
        req,
        hub_for(Os::current()),
        &exe,
        &dirs,
        app_commit(),
        &missing,
        &mut ProcessRunner::new(repo_root(), req.jobs),
        &home,
        progress,
    )
}

/// `prepare` for an executable at `exe`, the app folders `dirs`, the source's `commit` and the
/// toolchain `missing` (the seam the tests use). A packaged source is copied into `dirs` and
/// the runner runs there.
#[allow(clippy::too_many_arguments)]
pub fn prepare_installed(
    req: &Request,
    hub: Hub,
    exe: &Path,
    dirs: &AppDirs,
    commit: Option<&str>,
    missing: &[Missing],
    runner: &mut dyn ScriptRunner,
    home: &Path,
    progress: &mut dyn FnMut(Stage),
) -> Result<PathBuf> {
    let source = source_root(req, exe, dirs, commit)?;
    runner.set_root(&source);
    run_stages(req, hub, missing, runner, home, progress)
}

/// `prepare` for a given hub, script runner and home folder, with no toolchain check (the
/// seam the runner tests use).
pub fn prepare_with(
    req: &Request,
    hub: Hub,
    runner: &mut dyn ScriptRunner,
    home: &Path,
    progress: &mut dyn FnMut(Stage),
) -> Result<PathBuf> {
    run_stages(req, hub, &[], runner, home, progress)
}

fn run_stages(
    req: &Request,
    hub: Hub,
    missing: &[Missing],
    runner: &mut dyn ScriptRunner,
    home: &Path,
    progress: &mut dyn FnMut(Stage),
) -> Result<PathBuf> {
    if !serves(hub).contains(&req.target) {
        return Err(Error::Unsupported(format!(
            "the {} hub does not build for {}: that is built on the {} hub{}",
            hub.key(),
            req.target.key(),
            hub_for(req.target).key(),
            if hub_for(req.target) == Hub::Mac {
                " (the Mac)"
            } else {
                ""
            }
        )));
    }
    // Upstream's script builds the desktop player of the machine it runs on. Its Android
    // route is another script with its own data folder (ADR 0005): until that is built, it
    // is not answered with the hub's desktop build.
    if req.target == Os::Android {
        return Err(Error::Unsupported(format!(
            "no stage builds for {} yet",
            req.target.key()
        )));
    }
    let required: Vec<&Missing> = missing.iter().filter(|piece| !piece.warning).collect();
    if !required.is_empty() {
        let mut message = String::from("missing build tools:");
        for piece in required {
            message.push_str("\n  ");
            message.push_str(&piece.message);
        }
        return Err(Error::Build(message));
    }
    // The steps and the work-tree check name the same, absolute folder.
    let absolute_req;
    let req = if matches!(req.target, Os::Ios | Os::Mac) {
        absolute_req = Request {
            out: absolute(&req.out)?,
            ..req.clone()
        };
        &absolute_req
    } else {
        req
    };
    preflight(req, runner, home)?;
    runner.set_message_paths(&req.game.romfs, &req.out);
    // Recover a mod left by a previous child, even when every stage is cached.
    // Its font comes from that run's linked RomFS if the caller selected a new source.
    let prepared_romfs = req.out.join("extract").join("romfs");
    let recovery_romfs = if prepared_romfs.is_dir() {
        &prepared_romfs
    } else {
        &req.game.romfs
    };
    crate::msg::finish_pending(recovery_romfs, &req.out, script_python(req.target))
        .map_err(|error| build_error(Stage::Game, &[error.to_string()], home))?;
    // What was there before anything ran. A script that runs on while we look may have
    // finished its next stage already: only an output from before skips a stage.
    let flags = format!("{} {}\n", req.fan_mods.ptbr, req.fan_mods.speedfix);
    let record = req.out.join(".fan-mod-settings");
    let changed = std::fs::read_to_string(&record).ok().as_deref() != Some(flags.as_str());
    let source_changed = std::fs::read_to_string(req.out.join("game_source.txt"))
        .ok()
        .is_some_and(|source| Path::new(source.trim()) != req.game.root);
    // Recheck opted-in inputs for changed archives and patches, and migrate old builds.
    let ptbr_present = std::fs::symlink_metadata(req.out.join("mods/ptbr")).is_ok();
    let speedfix_present = std::fs::read_to_string(req.out.join("gen/main/profile.txt"))
        .is_ok_and(|profile| profile.split_whitespace().any(|arg| arg == "--variant"));
    let recheck = changed
        || source_changed
        || req.fan_mods.ptbr
        || req.fan_mods.speedfix
        || ptbr_present
        || speedfix_present;
    let before = STAGES.map(|stage| {
        stage_complete(stage, req)
            && !(recheck
                && matches!(
                    stage,
                    Stage::Game | Stage::Recompile | Stage::Shaders | Stage::Build
                ))
    });
    let mut last = None;
    for (stage, was_there) in STAGES.into_iter().zip(before) {
        if was_there && stage_complete(stage, req) {
            continue;
        }
        progress(stage);
        match step(stage, req) {
            Step::Script(args) => runner
                .run(stage, &args)
                .map_err(|log| build_error(stage, &log, home))?,
            Step::Rust if stage == Stage::Shaders => {
                // If only an early stage ran, the script can still be installing mods.
                // Wait for the first pass to leave before inspecting or packing its files.
                runner
                    .before_shaders()
                    .map_err(|log| build_error(stage, &log, home))?;
                runner.shaders(req)?;
            }
            Step::Rust => {}
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
    std::fs::write(record, flags)?;
    match req.target {
        Os::Ios => ios_deliver(req, runner, home),
        Os::Mac => mac_deliver(req, runner, home),
        _ => Ok(req.out.clone()),
    }
}

/// What the `ios` target does after the shared stages (which made the Mac player there too,
/// as upstream's script cannot stop before it; the iOS build does not use it): the two packs
/// when they are missing, the `iphoneos` build under the prepared folder, and the `.ipa` in
/// the delivery folder, whose path it returns.
fn ios_deliver(req: &Request, runner: &mut dyn ScriptRunner, home: &Path) -> Result<PathBuf> {
    let delivery = req
        .delivery
        .as_deref()
        .ok_or_else(|| Error::Env("the delivery folder is not set".into()))?;
    if !(req.out.join("romfs.pak").is_file() && req.out.join("spirv.pak").is_file()) {
        pack_stage(req)?;
    }
    let build = req.out.join("build-ios");
    let mut build_args = build_ios_args(&req.out.join("gen"), &build);
    // A Finder-launched app's PATH has only launchd's folders: the build script finds cargo
    // and Homebrew's tools (cmake, ninja) first in these.
    let mut dirs: Vec<PathBuf> = cargo_dir(runner, home).into_iter().collect();
    dirs.extend(
        ["/opt/homebrew/bin", "/usr/local/bin"]
            .map(PathBuf::from)
            .into_iter()
            .filter(|dir| dir.is_dir()),
    );
    if !dirs.is_empty() {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(dirs.into_iter().chain(std::env::split_paths(&path)))
            .map_err(|e| Error::Build(format!("cannot make the PATH: {e}")))?;
        build_args.splice(
            0..0,
            ["env".to_owned(), format!("PATH={}", path.to_string_lossy())],
        );
    }
    for args in [
        build_args,
        package_ios_args(package_version(), &build, &req.out, delivery),
    ] {
        runner
            .run(Stage::Build, &args)
            .map_err(|log| build_error(Stage::Build, &log, home))?;
        runner
            .finish()
            .map_err(|log| build_error(Stage::Build, &log, home))?;
    }
    Ok(ipa_path(delivery))
}

/// What the `mac` target does after the shared stages: the two packs when they are missing,
/// 1.22b's assembly of the contained app in the install folder, and `package_mac.sh`, which
/// puts the prepared game inside it and zips it into the delivery folder, whose zip's path it
/// returns.
fn mac_deliver(req: &Request, runner: &mut dyn ScriptRunner, home: &Path) -> Result<PathBuf> {
    let delivery = req
        .delivery
        .as_deref()
        .ok_or_else(|| Error::Env("the delivery folder is not set".into()))?;
    let install = req
        .install
        .as_deref()
        .ok_or_else(|| Error::Env("the install folder is not set".into()))?;
    if !(req.out.join("romfs.pak").is_file() && req.out.join("spirv.pak").is_file()) {
        pack_stage(req)?;
    }
    let app = runner
        .assemble(&req.out.join("build"), install)
        .map_err(|e| build_error(Stage::Build, &[e.to_string()], home))?;
    runner
        .run(
            Stage::Build,
            &package_mac_args(package_version(), &app, &req.out, delivery),
        )
        .map_err(|log| build_error(Stage::Build, &log, home))?;
    runner
        .finish()
        .map_err(|log| build_error(Stage::Build, &log, home))?;
    Ok(zip_path(delivery))
}

/// The build tools `os` needs before its first stage, searched for in `path` (and, on the Mac,
/// in `homebrew`). Required pieces stop a prepare; a warning names what the build loses
/// without it. The script fetches CMake and Ninja, the Vulkan headers, .NET, and LLVM and
/// SDL3 on Windows itself, so none of those is checked here.
pub fn toolchain(os: Os, path: impl AsRef<OsStr>) -> Vec<Missing> {
    let homebrew = [PathBuf::from("/opt/homebrew"), PathBuf::from("/usr/local")];
    toolchain_with(os, path, &homebrew)
}

/// `toolchain` with the Homebrew prefixes to try (the seam the tests use).
pub fn toolchain_with(os: Os, path: impl AsRef<OsStr>, homebrew: &[PathBuf]) -> Vec<Missing> {
    let dirs: Vec<PathBuf> = std::env::split_paths(path.as_ref())
        .filter(|dir| !dir.as_os_str().is_empty())
        .collect();
    let mut missing = Vec::new();

    if os == Os::Windows {
        if !has_msvc_tools(&dirs) {
            missing.push(missing_piece(
                "the Visual Studio C++ build tools",
                "the Windows SDK and C runtime come from them.",
                "winget install Microsoft.VisualStudio.2022.BuildTools --override \"--quiet --wait \
                 --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended\"",
                false,
            ));
        }
        if find_tool(&dirs, os, "git").is_none() {
            missing.push(missing_piece(
                "git",
                "without it the script builds no libvpx and the game's movies are black.",
                "winget install Git.Git",
                true,
            ));
        }
        return missing;
    }

    for (tool, why) in [
        ("clang", "the C compiler the build uses."),
        ("clang++", "the C++ compiler the build uses."),
        ("curl", "the downloader the script uses."),
    ] {
        if find_tool(&dirs, os, tool).is_none() {
            missing.push(missing_piece(tool, why, install_command(os, tool), false));
        }
    }

    match find_tool(&dirs, os, "python3") {
        Some(python) => {
            if has_venv(&python) == Some(false) {
                missing.push(missing_piece(
                    "python3's venv module",
                    "the script makes its private venv with it.",
                    install_command(os, "venv"),
                    false,
                ));
            }
        }
        None => missing.push(missing_piece(
            "python3",
            "the build script is Python.",
            install_command(os, "python3"),
            false,
        )),
    }

    if os == Os::Mac {
        if !has_xcode(&dirs) {
            missing.push(missing_piece(
                "Xcode's command-line tools",
                "they give clang and git for CMake's SPIRV-Cross fetch.",
                "xcode-select --install",
                false,
            ));
        }
        match homebrew_prefix(&dirs, homebrew) {
            Some(prefix) => {
                for package in ["sdl3", "molten-vk", "vulkan-loader"] {
                    if !formula_installed(&prefix, package) {
                        missing.push(missing_piece(
                            package,
                            "the Mac build finds it through Homebrew.",
                            &format!("brew install {package}"),
                            false,
                        ));
                    }
                }
                for (package, why) in [
                    ("libvpx", "without it the game's movies are black."),
                    (
                        "libvorbis",
                        "without it the SDK's own Vorbis decoder is used.",
                    ),
                ] {
                    if !formula_installed(&prefix, package) {
                        missing.push(missing_piece(
                            package,
                            why,
                            &format!("brew install {package}"),
                            true,
                        ));
                    }
                }
            }
            None => {
                missing.push(missing_piece(
                    "Homebrew",
                    "the Mac build takes SDL3, MoltenVK and the Vulkan loader from it.",
                    install_command(os, "brew"),
                    false,
                ));
            }
        }
    } else {
        if !has_sdl3(&dirs) {
            missing.push(missing_piece(
                "the SDL3 development files",
                "CMake asks for SDL3.",
                install_command(os, "sdl3"),
                false,
            ));
        }
        for (package, why) in [
            ("libvpx", "without it the game's movies are black."),
            (
                "libvorbis",
                "without it the SDK's own Vorbis decoder is used.",
            ),
        ] {
            if !has_library(&dirs, package) {
                missing.push(missing_piece(
                    package,
                    why,
                    install_command(os, package),
                    true,
                ));
            }
        }
    }

    missing
}

fn missing_piece(what: &str, why: &str, install: &str, warning: bool) -> Missing {
    Missing {
        what: what.to_owned(),
        message: format!("Missing {what}: {why} Install it with: {install}"),
        install: install.to_owned(),
        warning,
    }
}

fn executable(os: Os, name: &str) -> String {
    if os == Os::Windows {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

fn find_tool(dirs: &[PathBuf], os: Os, name: &str) -> Option<PathBuf> {
    let file = executable(os, name);
    dirs.iter()
        .map(|dir| dir.join(&file))
        .find(|path| path.is_file())
}

fn prefix_of(dir: &Path) -> &Path {
    if dir.file_name() == Some(OsStr::new("bin")) {
        dir.parent().unwrap_or(dir)
    } else {
        dir
    }
}

fn homebrew_prefix(dirs: &[PathBuf], prefixes: &[PathBuf]) -> Option<PathBuf> {
    for prefix in prefixes {
        if prefix.join("bin").join("brew").is_file() {
            return Some(prefix.clone());
        }
        if prefix.file_name() == Some(OsStr::new("bin")) && prefix.join("brew").is_file() {
            return prefix.parent().map(Path::to_path_buf);
        }
    }
    for dir in dirs {
        if dir.join("brew").is_file() {
            return Some(prefix_of(dir).to_path_buf());
        }
        if dir.join("bin").join("brew").is_file() {
            return Some(dir.clone());
        }
    }
    None
}

fn formula_installed(prefix: &Path, package: &str) -> bool {
    prefix.join("opt").join(package).exists() || prefix.join("Cellar").join(package).is_dir()
}

/// `setup_acnh.py` finds the build tools through vswhere, at its Program Files place even
/// when the Visual Studio prompt added nothing to PATH.
fn has_msvc_tools(dirs: &[PathBuf]) -> bool {
    find_tool(dirs, Os::Windows, "vswhere").is_some()
        || std::env::var_os("ProgramFiles(x86)")
            .map(PathBuf::from)
            .is_some_and(|programs| {
                programs
                    .join("Microsoft Visual Studio/Installer/vswhere.exe")
                    .is_file()
            })
}

fn has_xcode(dirs: &[PathBuf]) -> bool {
    find_tool(dirs, Os::Mac, "xcode-select").is_some()
        || Path::new("/usr/bin/xcode-select").is_file()
        || Path::new("/Library/Developer/CommandLineTools/usr/bin/clang").is_file()
}

/// `Some(false)` when the interpreter's standard library is found and has no `venv` module;
/// `None` when it cannot be told from the folder (a fake in a test).
fn has_venv(python: &Path) -> Option<bool> {
    let prefix = prefix_of(python.parent()?);
    let mut roots = Vec::new();
    if let Ok(entries) = std::fs::read_dir(prefix.join("lib")) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("python") {
                roots.push(entry.path());
            }
        }
    }
    if roots.is_empty() {
        return None;
    }
    Some(
        roots
            .iter()
            .any(|root| root.join("venv").join("__init__.py").is_file()),
    )
}

fn has_sdl3(dirs: &[PathBuf]) -> bool {
    dirs.iter().any(|dir| {
        let prefix = prefix_of(dir);
        prefix.join("include/SDL3/SDL.h").is_file()
            || prefix.join("lib/cmake/SDL3/SDL3Config.cmake").is_file()
            || prefix
                .join("lib")
                .join("SDL3")
                .join("SDL3Config.cmake")
                .is_file()
    })
}

fn has_library(dirs: &[PathBuf], package: &str) -> bool {
    let module = package.trim_start_matches("lib");
    dirs.iter().any(|dir| library_in(prefix_of(dir), module))
}

fn library_in(prefix: &Path, module: &str) -> bool {
    let lib = prefix.join("lib");
    let pkg_config = format!("{module}.pc");
    let shared = [format!("lib{module}.so"), format!("lib{module}.dylib")];
    if lib.join("pkgconfig").join(&pkg_config).is_file()
        || shared.iter().any(|file| lib.join(file).is_file())
    {
        return true;
    }
    // Debian and Ubuntu put the development files under a multiarch folder.
    let Ok(entries) = std::fs::read_dir(&lib) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let dir = entry.path();
        dir.join("pkgconfig").join(&pkg_config).is_file()
            || shared.iter().any(|file| dir.join(file).is_file())
    })
}

fn install_command(os: Os, piece: &str) -> &'static str {
    if os == Os::Mac {
        match piece {
            "clang" | "clang++" => "xcode-select --install",
            "curl" => "brew install curl",
            "python3" => "brew install python",
            "venv" => "brew reinstall python",
            "sdl3" => "brew install sdl3",
            "molten-vk" => "brew install molten-vk",
            "vulkan-loader" => "brew install vulkan-loader",
            "libvpx" => "brew install libvpx",
            "libvorbis" => "brew install libvorbis",
            "brew" => {
                "/bin/bash -c \"$(curl -fsSL \
                 https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\""
            }
            _ => "install it with Homebrew",
        }
    } else {
        match piece {
            "clang" | "clang++" => "sudo apt install clang",
            "curl" => "sudo apt install curl",
            "python3" => "sudo apt install python3 python3-venv",
            "venv" => "sudo apt install python3-venv",
            "sdl3" => "sudo apt install libsdl3-dev",
            "libvpx" => "sudo apt install libvpx-dev",
            "libvorbis" => "sudo apt install libvorbis-dev",
            _ => "install it with your package manager",
        }
    }
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
