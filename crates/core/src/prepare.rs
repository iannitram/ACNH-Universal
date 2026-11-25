//! Prepare (SPEC.md M1, M13): from a checked game folder to a build for one device family, on
//! a hub. The Rust replacement of upstream's `tools/setup_acnh.py` and the scripts it ran
//! (`acnh_version.py`, `nso2bin.py`, `pack_romfs.py`, `pack_spirv.py`, `extract_shaders.py`,
//! `sarc_endian.py`, `msg_rewrap.py`, `make_button_font.py`, `make_button_glyphs.py`), stage by
//! stage (`recipe::Stage`), each skipped when its output is already there. Every stage except
//! `Stage::Recompile` (`python3 -m recomp` until #49) runs in Rust (`Step::Rust`).
//!
//! Everything a prepare writes is made from my game folder: it stays in `out` on the hub, and
//! reaches my other devices only through the hub, over the home network (M11, M14).

pub use crate::assemble::assemble;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::gamefolder::GameFolder;
pub use crate::pack_folder::{
    app_out_dir, app_out_dir_for, pack_folder, pack_folder_with, PackContext,
};
use crate::platform::{AppDirs, Os, Vars};
use crate::recipe::{hub_for, output, serves, Hub, Stage, STAGES};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
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
/// "android" for Android, `None` for the others (nothing of theirs goes to Dropbox).
pub fn delivery_subfolder(target: Os) -> Option<&'static str> {
    match target {
        Os::Ios => Some("ios"),
        Os::Mac => Some("mac"),
        Os::Android => Some("android"),
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
/// The bundled mods (`bundle_mods_args`) go beside the player in the app: `--mods <folder>`.
fn package_ios_args(
    version: &str,
    build: &Path,
    game: &Path,
    delivery: &Path,
    mods: &Path,
) -> Vec<String> {
    vec![
        "tools/package_ios.sh".to_owned(),
        version.to_owned(),
        build.display().to_string(),
        "--game".to_owned(),
        game.display().to_string(),
        delivery.display().to_string(),
        "--mods".to_owned(),
        mods.display().to_string(),
    ]
}

/// The arguments of `tools/bundle_mods.sh`: the pt-BR translation and the SpeedFix's files
/// (the 60 fps `video.frame_rate` needs them) are put in `<dest>/mods/`, which the Apple
/// packages carry beside the player, as the APK does.
///
/// The two zips are cached in `cache` (`ACNH_MODS_CACHE`), a folder of the prepared folder:
/// the installed app's source copy is replaced on every Prepare, so a cache in it would
/// download them again each time.
fn bundle_mods_args(dest: &Path, cache: &Path) -> Vec<String> {
    vec![
        "env".to_owned(),
        format!("ACNH_MODS_CACHE={}", cache.display()),
        "tools/bundle_mods.sh".to_owned(),
        dest.display().to_string(),
    ]
}

/// Where a prepare leaves the bundled mods it made for the package: the folder
/// `bundle_mods.sh` is given, the `mods/` inside it, and the folder of its cached zips.
fn bundled_mods_folder(out: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let dest = out.join("bundle-mods");
    let mods = dest.join("mods");
    (dest, mods, out.join("mods-cache"))
}

/// The arguments of `tools/package_mac.sh`: the contained app is the assembled `app`, with
/// the prepared folder `game` inside it.
fn package_mac_args(
    version: &str,
    app: &Path,
    game: &Path,
    delivery: &Path,
    mods: &Path,
) -> Vec<String> {
    vec![
        "tools/package_mac.sh".to_owned(),
        version.to_owned(),
        app.display().to_string(),
        game.display().to_string(),
        delivery.display().to_string(),
        "--mods".to_owned(),
        mods.display().to_string(),
    ]
}

/// The short build version the Apple packages are named with: `<BASE_VER>-<VCODE>` from
/// `tools/build_version.sh` (for example `0.11.0-1100891`), which the package scripts take
/// (they refuse the `+` of the long form). `base` has any `+build` suffix cut off.
fn short_build_version(base: &str, vcode: &str) -> Option<String> {
    let base = base.split('+').next().unwrap_or(base).trim();
    let vcode = vcode.trim();
    let version = format!("{base}-{vcode}");
    let plain = |c: char| c.is_ascii_alphanumeric() || c == '.' || c == '-';
    (!base.is_empty()
        && !vcode.is_empty()
        && vcode.chars().all(|c| c.is_ascii_digit())
        && version.chars().all(plain))
    .then_some(version)
}

/// The version the Apple packages are named with. In a git checkout (`root` has a `.git`)
/// it is `tools/build_version.sh`'s, run there. A packaged source has no `.git` (the release
/// makes it with `git archive`), so the script would print 0.0.0: the release writes the
/// script's output to `BUILD_VERSION` in the source instead, and that is read. When neither
/// answers, the engine's own version.
fn package_version(root: &Path) -> String {
    let fallback = || env!("CARGO_PKG_VERSION").to_owned();
    let text = if root.join(".git").exists() {
        let Ok(output) = std::process::Command::new("bash")
            .arg("tools/build_version.sh")
            .current_dir(root)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
        else {
            return fallback();
        };
        if !output.status.success() {
            return fallback();
        }
        String::from_utf8_lossy(&output.stdout).into_owned()
    } else {
        match std::fs::read_to_string(root.join("BUILD_VERSION")) {
            Ok(text) => text,
            Err(_) => return fallback(),
        }
    };
    let value = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .unwrap_or("")
            .to_owned()
    };
    short_build_version(&value("BASE_VER"), &value("VCODE")).unwrap_or_else(fallback)
}

/// The `.zip` a Mac prepare makes in `delivery` (`package_mac.sh` names it).
fn zip_path(delivery: &Path, version: &str) -> PathBuf {
    delivery.join(format!("ACNH-Universal-{version}.zip"))
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
fn ipa_path(delivery: &Path, version: &str) -> PathBuf {
    delivery.join(format!("ACNH-Universal-{version}.ipa"))
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

/// Pinned third-party versions for `Stage::Tools` and `Stage::Build`.
const VK_HEADERS: &str = "1.4.357";
const LLVM: &str = "23.1.3";
const SDL3: &str = "3.4.16";
const IMGUI: &str = "1.91.9";
const RYUBING: &str = "a82350bb77";
const DOTNET: &str = "10.0.401";
const FFX: &str = "v2.4.0";
const DLSS: &str = "v310.9.16";

/// How `stage` runs for `req`. Every stage runs in Rust (`Step::Rust`).
pub fn step(_stage: Stage, _req: &Request) -> Step {
    Step::Rust
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

    /// The Rust tools stage. The default runs it (`rust_tools`).
    fn tools(&mut self, req: &Request) -> Result<()> {
        rust_tools(req, &self.root())
    }

    /// The Rust game stage. The default runs it (`rust_game`).
    fn game(&mut self, req: &Request) -> Result<()> {
        rust_game(req, &self.root())
    }

    /// The Rust version stage. The default runs it (`rust_version`).
    fn version(&mut self, req: &Request) -> Result<()> {
        rust_version(req, &self.root())
    }

    /// The Rust recompile stage. The default runs it (`rust_recompile`).
    fn recompile(&mut self, req: &Request) -> Result<()> {
        rust_recompile(req, &self.root(), 0)
    }

    /// Wait for any first-pass setup child before shaders inspect its output.
    fn before_shaders(&mut self) -> std::result::Result<(), Vec<String>> {
        Ok(())
    }

    /// The Rust part of the shaders stage. The default runs it (`rust_shaders`); a test runner
    /// may stand in for it, as no translator is installed on a test machine.
    fn shaders(&mut self, req: &Request) -> Result<()> {
        rust_shaders(req, &self.root())
    }

    /// The Rust fonts stage. The default runs it (`rust_fonts`).
    fn fonts(&mut self, req: &Request) -> Result<()> {
        rust_fonts(req)
    }

    /// The Rust build stage. The default runs it (`rust_build`).
    fn build(&mut self, req: &Request) -> Result<()> {
        rust_build(req, &self.root(), 0)
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
    if !matches!(req.target, Os::Ios | Os::Mac | Os::Android) {
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

    fn recompile(&mut self, req: &Request) -> Result<()> {
        rust_recompile(req, &self.root, self.jobs)
    }

    fn build(&mut self, req: &Request) -> Result<()> {
        rust_build(req, &self.root, self.jobs)
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

/// Where the recompiler input hash is recorded beside `gen/`.
fn recomp_hash_file(req: &Request) -> PathBuf {
    req.out.join("gen").join(".recomp-hash")
}

/// Deterministic SHA-256 hash of the recompiler inputs (`recomp/*.py` in sorted name order,
/// plus the Rust emitter/version marker) so changes to the recompiler invalidate `gen/`.
pub fn recomp_hash(source: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    let recomp_dir = source.join("recomp");
    if recomp_dir.is_dir() {
        let mut entries: Vec<(String, PathBuf)> = Vec::new();
        for entry in std::fs::read_dir(&recomp_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name.ends_with(".py") {
                entries.push((name, path));
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, path) in entries {
            let bytes = std::fs::read(&path)?;
            hasher.update(name.as_bytes());
            hasher.update(b"\0");
            hasher.update(&bytes);
            hasher.update(b"\0");
        }
    }
    hasher.update(b"rust-emitter:");
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    hasher.update(b"\0");
    Ok(format!("{:x}", hasher.finalize()))
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

/// The Rust fonts stage for the Prepare runner.
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

fn venv_dir(root: &Path) -> PathBuf {
    root.join("private/venv")
}

fn venv_bin(root: &Path) -> PathBuf {
    venv_dir(root).join(if cfg!(windows) { "Scripts" } else { "bin" })
}

fn venv_python(root: &Path) -> PathBuf {
    venv_bin(root).join(if cfg!(windows) {
        "python.exe"
    } else {
        "python"
    })
}

fn stage_err(stage: Stage, message: impl std::fmt::Display) -> Error {
    Error::Build(format!("stage {}: {message}", stage.key()))
}

fn run_logged(
    stage: Stage,
    program: &Path,
    args: &[String],
    cwd: &Path,
    env: &[(String, String)],
    remove_env: &[&str],
    log_path: Option<&Path>,
) -> Result<()> {
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(cwd).stdin(Stdio::null());
    for key in remove_env {
        cmd.env_remove(key);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let output = cmd
        .output()
        .map_err(|e| stage_err(stage, format!("cannot run {}: {e}", program.display())))?;
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&stderr);
    }
    if let Some(path) = log_path {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, &combined);
    }
    if !output.status.success() {
        let lines: Vec<&str> = combined.lines().collect();
        let tail = lines[lines.len().saturating_sub(20)..].join("\n");
        let log_note = log_path
            .map(|p| format!(" (log: {})", p.display()))
            .unwrap_or_default();
        return Err(stage_err(
            stage,
            format!("failed: {}{log_note}\n{tail}", program.display()),
        ));
    }
    Ok(())
}

fn download_file(stage: Stage, url: &str, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = dest.with_extension("download");
    let curl = if cfg!(windows) { "curl.exe" } else { "curl" };
    run_logged(
        stage,
        Path::new(curl),
        &[
            "-fsSL".to_owned(),
            "-o".to_owned(),
            tmp.display().to_string(),
            url.to_owned(),
        ],
        dest.parent().unwrap_or_else(|| Path::new(".")),
        &[],
        &[],
        None,
    )?;
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

fn fetch_archive(stage: Stage, url: &str, dest_dir: &Path, strip: bool, py: &Path) -> Result<()> {
    std::fs::create_dir_all(dest_dir)?;
    let tmp = PathBuf::from(format!("{}.download", dest_dir.display()));
    download_file(stage, url, &tmp)?;
    let cwd = dest_dir;
    if url.ends_with(".tar.zst") {
        let script = "import sys, tarfile, zstandard; \
            d = zstandard.ZstdDecompressor(max_window_size=1<<31).stream_reader(open(sys.argv[1],'rb')); \
            t = tarfile.open(fileobj=d, mode='r|'); \
            strip = sys.argv[3] == '1'; \
            [t.extract(m, sys.argv[2], filter='data') for m in t if (not strip or ('/' in m.name and m.name.split('/',1)[1] and not setattr(m,'name',m.name.split('/',1)[1])))]";
        run_logged(
            stage,
            py,
            &[
                "-c".to_owned(),
                script.to_owned(),
                tmp.display().to_string(),
                dest_dir.display().to_string(),
                if strip { "1" } else { "0" }.to_owned(),
            ],
            cwd,
            &[],
            &[],
            None,
        )?;
    } else if url.ends_with(".zip") {
        let script = "import os, shutil, sys, zipfile; \
            z = zipfile.ZipFile(sys.argv[1]); \
            strip = sys.argv[3] == '1'; \
            names = z.namelist(); \
            top = (names[0].split('/')[0] + '/') if (strip and names) else ''; \
            [None for i in z.infolist() for n in [i.filename[len(top):] if (top and i.filename.startswith(top)) else i.filename] if n and not n.startswith('/') and '..' not in n.split('/') and ((os.makedirs(os.path.join(sys.argv[2], n), exist_ok=True) if i.is_dir() else (os.makedirs(os.path.dirname(os.path.join(sys.argv[2], n)), exist_ok=True), shutil.copyfileobj(z.open(i), open(os.path.join(sys.argv[2], n), 'wb')))) or True)]";
        run_logged(
            stage,
            py,
            &[
                "-c".to_owned(),
                script.to_owned(),
                tmp.display().to_string(),
                dest_dir.display().to_string(),
                if strip { "1" } else { "0" }.to_owned(),
            ],
            cwd,
            &[],
            &[],
            None,
        )?;
    } else {
        let mut args = vec![
            "-xzf".to_owned(),
            tmp.display().to_string(),
            "-C".to_owned(),
            dest_dir.display().to_string(),
        ];
        if strip {
            args.push("--strip-components=1".to_owned());
        }
        let tar = if cfg!(windows) { "tar.exe" } else { "tar" };
        run_logged(stage, Path::new(tar), &args, cwd, &[], &[], None)?;
    }
    let _ = std::fs::remove_file(&tmp);
    Ok(())
}

fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let name = executable(Os::current(), program);
    std::env::split_paths(&path)
        .map(|dir| dir.join(&name))
        .find(|p| p.is_file())
}

fn has_dotnet_10(dotnet: &Path) -> bool {
    Command::new(dotnet)
        .arg("--list-sdks")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|line| line.starts_with("10."))
        })
}

fn dotnet_rid() -> Option<(&'static str, &'static str)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some(("win-x64", "zip")),
        ("linux", "x86_64") => Some(("linux-x64", "tar.gz")),
        ("linux", "aarch64") => Some(("linux-arm64", "tar.gz")),
        ("macos", "aarch64") => Some(("osx-arm64", "tar.gz")),
        ("macos", "x86_64") => Some(("osx-x64", "tar.gz")),
        _ => None,
    }
}

/// The Rust tools stage: ensures Python venv (for `python3 -m recomp` until #49), fetches
/// Vulkan-Headers, Dear ImGui, Ryubing (and Windows SDKs/LLVM/SDL3/vcpkg), ensures .NET 10,
/// and builds `spirv_harness.dll`.
pub fn rust_tools(req: &Request, root: &Path) -> Result<()> {
    let stage = Stage::Tools;
    let p = root.join("private");
    let t = p.join("third_party");
    let logs = req.out.join("logs");
    std::fs::create_dir_all(&t)?;
    std::fs::create_dir_all(&logs)?;

    let py = venv_python(root);
    if !py.exists() {
        run_logged(
            stage,
            Path::new(script_python(req.target)),
            &[
                "-m".to_owned(),
                "venv".to_owned(),
                venv_dir(root).display().to_string(),
            ],
            root,
            &[],
            &[],
            None,
        )?;
    }
    let check_pkgs = Command::new(&py)
        .args([
            "-c",
            "import capstone, lz4, cmake, ninja, importlib.metadata as m; assert m.version('capstone') == '5.0.7'",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !check_pkgs {
        let mut pkgs = vec![
            "-m".to_owned(),
            "pip".to_owned(),
            "install".to_owned(),
            "-q".to_owned(),
            "capstone==5.0.7".to_owned(),
            "lz4".to_owned(),
            "cmake".to_owned(),
            "ninja".to_owned(),
        ];
        if cfg!(windows) {
            pkgs.push("zstandard".to_owned());
        }
        run_logged(stage, &py, &pkgs, root, &[], &[], None)?;
    }

    let vkh = t.join(format!("Vulkan-Headers-{VK_HEADERS}"));
    if !vkh.is_dir() {
        fetch_archive(
            stage,
            &format!("https://github.com/KhronosGroup/Vulkan-Headers/archive/refs/tags/v{VK_HEADERS}.tar.gz"),
            &vkh,
            true,
            &py,
        )?;
    }
    let imgui = t.join("imgui");
    if !imgui.join("imgui.cpp").exists() {
        fetch_archive(
            stage,
            &format!("https://github.com/ocornut/imgui/archive/refs/tags/v{IMGUI}.tar.gz"),
            &imgui,
            true,
            &py,
        )?;
    }
    let ryubing = t.join("ryubing");
    if !ryubing.join("Ryujinx.sln").exists() {
        fetch_archive(
            stage,
            &format!("https://codeberg.org/Software_arch/Ryubing/archive/{RYUBING}.tar.gz"),
            &ryubing,
            true,
            &py,
        )?;
    }

    if cfg!(windows) {
        rust_tools_windows(req, root, &t, &logs, &py)?;
    }

    let mut dotnet_env = vec![("DOTNET_CLI_TELEMETRY_OPTOUT".to_owned(), "1".to_owned())];
    let dotnet = match find_on_path("dotnet").filter(|p| has_dotnet_10(p)) {
        Some(sys) => sys,
        None => {
            let dn = t.join("dotnet");
            let exe = dn.join(executable(req.target, "dotnet"));
            if !exe.exists() {
                let (rid, ext) = dotnet_rid().ok_or_else(|| {
                    stage_err(stage, "unsupported host architecture for .NET 10 SDK")
                })?;
                fetch_archive(
                    stage,
                    &format!("https://builds.dotnet.microsoft.com/dotnet/Sdk/{DOTNET}/dotnet-sdk-{DOTNET}-{rid}.{ext}"),
                    &dn,
                    false,
                    &py,
                )?;
            }
            dotnet_env.push(("DOTNET_ROOT".to_owned(), dn.display().to_string()));
            let cur_path = std::env::var_os("PATH").unwrap_or_default();
            let joined =
                std::env::join_paths(std::iter::once(dn).chain(std::env::split_paths(&cur_path)))
                    .map_err(|e| stage_err(stage, e))?;
            dotnet_env.push(("PATH".to_owned(), joined.to_string_lossy().into_owned()));
            exe
        }
    };

    let harness = p.join("build/spirv_harness/bin/Release/net10.0/spirv_harness.dll");
    let alt_dir = p.join("build/spirv_harness/out");
    let alt = alt_dir.join("spirv_harness.dll");
    if !harness.exists() && !alt.exists() {
        let proj = "tools/maxwell/spirv_harness/SpirvHarness.csproj";
        let hlog = logs.join("harness.log");
        run_logged(
            stage,
            &dotnet,
            &[
                "build".to_owned(),
                proj.to_owned(),
                "-c".to_owned(),
                "Release".to_owned(),
                "-m:4".to_owned(),
            ],
            root,
            &dotnet_env,
            &["Platform", "PLATFORM"],
            Some(&hlog),
        )?;
        if !harness.exists() {
            run_logged(
                stage,
                &dotnet,
                &[
                    "build".to_owned(),
                    proj.to_owned(),
                    "-c".to_owned(),
                    "Release".to_owned(),
                    "-m:1".to_owned(),
                    "-o".to_owned(),
                    alt_dir.display().to_string(),
                ],
                root,
                &dotnet_env,
                &["Platform", "PLATFORM"],
                Some(&hlog),
            )?;
            if !alt.exists() {
                return Err(stage_err(
                    stage,
                    format!(
                        "the shader translator was built but spirv_harness.dll is missing (log: {})",
                        hlog.display()
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn rust_tools_windows(_req: &Request, root: &Path, t: &Path, logs: &Path, py: &Path) -> Result<()> {
    let stage = Stage::Tools;
    let base = format!("https://raw.githubusercontent.com/GPUOpen-LibrariesAndSDKs/FidelityFX-SDK/{FFX}/Kits/FidelityFX");
    let nv = format!("https://raw.githubusercontent.com/NVIDIA/DLSS/{DLSS}");
    let mut files = Vec::new();
    for h in [
        "api/include/ffx_api.h",
        "api/include/ffx_api_types.h",
        "api/include/vk/ffx_api_vk.h",
        "upscalers/include/ffx_upscale.h",
        "framegeneration/include/ffx_framegeneration.h",
        "framegeneration/include/ffx_framegeneration_api_types.h",
        "framegeneration/include/vk/ffx_api_framegeneration_vk.h",
    ] {
        files.push(("ffx", h.to_owned(), format!("{base}/{h}")));
    }
    files.push((
        "ffx",
        "LICENSE.md".to_owned(),
        format!("{base}/docs/license.md"),
    ));
    for d in [
        "amd_fidelityfx_loader_vk.dll",
        "amd_fidelityfx_upscaler_vk.dll",
        "amd_fidelityfx_framegeneration_vk.dll",
    ] {
        files.push(("ffx", format!("bin/{d}"), format!("{base}/signedbin/{d}")));
    }
    for h in [
        "nvsdk_ngx.h",
        "nvsdk_ngx_defs.h",
        "nvsdk_ngx_defs_vk.h",
        "nvsdk_ngx_helpers.h",
        "nvsdk_ngx_helpers_vk.h",
        "nvsdk_ngx_params.h",
        "nvsdk_ngx_vk.h",
    ] {
        files.push(("dlss", format!("include/{h}"), format!("{nv}/include/{h}")));
    }
    files.push((
        "dlss",
        "LICENSE.txt".to_owned(),
        format!("{nv}/LICENSE.txt"),
    ));
    files.push((
        "dlss",
        "lib/nvsdk_ngx_d.lib".to_owned(),
        format!("{nv}/lib/Windows_x86_64/x64/nvsdk_ngx_d.lib"),
    ));
    files.push((
        "dlss",
        "bin/nvngx_dlss.dll".to_owned(),
        format!("{nv}/lib/Windows_x86_64/rel/nvngx_dlss.dll"),
    ));
    for (top, rel, url) in files {
        let out = t.join(top).join(rel);
        if !out.exists() {
            download_file(stage, &url, &out)?;
        }
    }
    let llvm = t.join(format!("llvm-{LLVM}"));
    if !llvm.join("bin/clang-cl.exe").exists() {
        fetch_archive(
            stage,
            &format!("https://github.com/llvm/llvm-project/releases/download/llvmorg-{LLVM}/clang+llvm-{LLVM}-x86_64-pc-windows-msvc.tar.zst"),
            &llvm,
            true,
            py,
        )?;
    }
    let sdl = t.join(format!("SDL3-{SDL3}"));
    if !sdl.join("cmake").is_dir() {
        fetch_archive(
            stage,
            &format!("https://github.com/libsdl-org/SDL/releases/download/release-{SDL3}/SDL3-devel-{SDL3}-VC.zip"),
            &sdl,
            true,
            py,
        )?;
    }
    let movie_libs = windows_movie_libs(t);
    if (!movie_libs.join("lib/vpx.lib").exists() || !movie_libs.join("lib/zs.lib").exists())
        && find_on_path("git").is_some()
    {
        let vcpkg = t.join("vcpkg");
        let vcpkg_exe = vcpkg.join("vcpkg.exe");
        if !vcpkg_exe.exists() {
            if !vcpkg.is_dir() {
                run_logged(
                    stage,
                    Path::new("git"),
                    &[
                        "clone".to_owned(),
                        "-q".to_owned(),
                        "--depth".to_owned(),
                        "1".to_owned(),
                        "https://github.com/microsoft/vcpkg.git".to_owned(),
                        vcpkg.display().to_string(),
                    ],
                    root,
                    &[],
                    &[],
                    None,
                )?;
            }
            run_logged(
                stage,
                &vcpkg.join("bootstrap-vcpkg.bat"),
                &["-disableMetrics".to_owned()],
                &vcpkg,
                &[],
                &[],
                Some(&logs.join("vcpkg.log")),
            )?;
        }
        if vcpkg_exe.exists() {
            let env = msvc_env(stage, root, t)?;
            let triplet = "x64-windows-static-md-release";
            run_logged(
                stage,
                &vcpkg_exe,
                &[
                    "install".to_owned(),
                    format!("libvpx:{triplet}"),
                    format!("libvorbis:{triplet}"),
                    format!("zlib:{triplet}"),
                    "--disable-metrics".to_owned(),
                ],
                &vcpkg,
                &env,
                &["Platform", "PLATFORM"],
                Some(&logs.join("vcpkg.log")),
            )?;
        }
    }
    Ok(())
}

fn windows_movie_libs(t: &Path) -> PathBuf {
    let release = t.join("vcpkg/installed/x64-windows-static-md-release");
    let legacy = t.join("vcpkg/installed/x64-windows-static-md");
    if !release.join("lib/vpx.lib").exists() && legacy.join("lib/vpx.lib").exists() {
        legacy
    } else {
        release
    }
}

fn msvc_env(stage: Stage, root: &Path, t: &Path) -> Result<Vec<(String, String)>> {
    let prog_files = std::env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"));
    let vswhere = prog_files.join(r"Microsoft Visual Studio\Installer\vswhere.exe");
    let inst = if vswhere.is_file() {
        Command::new(&vswhere)
            .args([
                "-latest",
                "-products",
                "*",
                "-requires",
                "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
                "-property",
                "installationPath",
            ])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default()
    } else {
        String::new()
    };
    let bat = PathBuf::from(&inst).join(r"VC\Auxiliary\Build\vcvars64.bat");
    if inst.is_empty() || !bat.is_file() {
        return Err(stage_err(
            stage,
            "missing: the Visual Studio C++ build tools (the Windows SDK and C runtime)",
        ));
    }
    let out = Command::new("cmd")
        .args([
            "/s",
            "/c",
            &format!("\"{}\" >nul 2>&1 && set", bat.display()),
        ])
        .output()
        .map_err(|e| stage_err(stage, format!("vcvars64.bat failed: {e}")))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut env: Vec<(String, String)> = Vec::new();
    let mut path_val = String::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.is_empty() || k.eq_ignore_ascii_case("PLATFORM") {
            continue;
        }
        if k.eq_ignore_ascii_case("PATH") {
            path_val = v.to_owned();
        } else {
            env.push((k.to_owned(), v.to_owned()));
        }
    }
    let llvm_bin = t.join(format!("llvm-{LLVM}")).join("bin");
    let venv_scripts = venv_bin(root);
    let joined = std::env::join_paths(
        [llvm_bin, venv_scripts]
            .into_iter()
            .chain(std::env::split_paths(&path_val)),
    )
    .map_err(|e| stage_err(stage, e))?;
    env.push(("PATH".to_owned(), joined.to_string_lossy().into_owned()));
    Ok(env)
}

fn mtime_newer(a: &Path, b: &Path) -> bool {
    let Ok(ma) = std::fs::metadata(a).and_then(|m| m.modified()) else {
        return true;
    };
    let Ok(mb) = std::fs::metadata(b).and_then(|m| m.modified()) else {
        return true;
    };
    ma > mb
}

fn mod_component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.ends_with([' ', '.'])
        && !name.chars().any(|c| {
            matches!(c, '\\' | '/' | ':' | '<' | '>' | '"' | '|' | '?' | '*') || c <= '\x1f'
        })
}

/// Extracts `zpath`'s `romfs/` into `<mods_root>/<name>` with pending message reflow.
pub fn install_mod_archive(
    mods_root: &Path,
    name: &str,
    zpath: &Path,
    romfs: &Path,
    python: &str,
) -> Result<()> {
    let dst = mods_root.join(name);
    if crate::platform::is_dir_link(&dst) {
        return Err(Error::Build(
            "mod output must be a regular directory".into(),
        ));
    }
    if let (Ok(zp), Ok(dp)) = (std::fs::canonicalize(zpath), std::fs::canonicalize(&dst)) {
        if zp.starts_with(&dp) {
            return Err(Error::Build(
                "keep the source mod archive outside its prepared output".into(),
            ));
        }
    }
    std::fs::create_dir_all(mods_root)?;
    let temp = tempfile::Builder::new()
        .prefix(".prepare-mod-")
        .tempdir_in(mods_root)?;
    let ready = temp.path().join("ready");
    std::fs::create_dir_all(&ready)?;

    let script = r#"import os, re, shutil, sys, zipfile
def ok(n):
    return bool(n) and n not in ('.', '..') and not n.endswith((' ', '.')) and not re.search(r'[\\/:<>"|?*\x00-\x1f]', n)
zpath, ready = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(zpath) as z:
    names = [e.filename for e in z.infolist()]
    if any(not all(ok(p) for p in path.rstrip('/').split('/')) for path in names if path.rstrip('/')):
        sys.exit('unsafe path in the zip')
    prefix = ''
    if not any(path.startswith('romfs/') for path in names):
        tops = {path.split('/')[0] for path in names if '/romfs/' in path}
        if len(tops) != 1:
            sys.exit('no romfs/ folder in the zip')
        prefix = tops.pop() + '/'
    for entry in z.infolist():
        if entry.is_dir() or not entry.filename.startswith(prefix + 'romfs/'):
            continue
        out = os.path.join(ready, entry.filename[len(prefix):])
        os.makedirs(os.path.dirname(out), exist_ok=True)
        with z.open(entry) as s, open(out, 'wb') as d:
            shutil.copyfileobj(s, d)
if not os.path.isdir(os.path.join(ready, 'romfs')):
    sys.exit('the mod archive contains no RomFS files')
"#;

    run_logged(
        Stage::Game,
        Path::new(python),
        &[
            "-c".to_owned(),
            script.to_owned(),
            zpath.display().to_string(),
            ready.display().to_string(),
        ],
        mods_root,
        &[],
        &[],
        None,
    )?;

    std::fs::create_dir_all(ready.join(".msg-reflow-pending"))?;
    let previous = temp.path().join("previous");
    let had_previous = std::fs::symlink_metadata(&dst).is_ok();
    if had_previous {
        std::fs::rename(&dst, &previous)?;
    }
    if let Err(err) = std::fs::rename(&ready, &dst) {
        if had_previous {
            let _ = std::fs::rename(&previous, &dst);
        }
        return Err(err.into());
    }
    let out = mods_root.parent().unwrap_or(mods_root);
    crate::msg::finish_pending(romfs, out, python).map_err(|e| stage_err(Stage::Game, e))?;
    Ok(())
}

/// Installs or removes opted-in fan mods (`ptbr`) and explicit `--mod <name>=<zip>` archives.
pub fn install_mods(
    out: &Path,
    romfs: &Path,
    game_src: &Path,
    root: &Path,
    ptbr_enabled: bool,
    mods: &[(&str, &Path)],
    python: &str,
) -> Result<()> {
    let mods_root = out.join("mods");
    if crate::platform::is_dir_link(&mods_root) {
        if ptbr_enabled || !mods.is_empty() {
            return Err(Error::Build(
                "mod output must be a regular directory".into(),
            ));
        }
        return Ok(());
    }
    let ptbr = mods_root.join("ptbr");
    let explicit_ptbr = mods.iter().any(|(name, _)| *name == "ptbr");
    let mut to_install: Vec<(String, PathBuf)> = Vec::new();
    if ptbr_enabled && !explicit_ptbr {
        let mut found = Vec::new();
        for dir in [game_src.to_path_buf(), root.join("private/translations")] {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_lowercase();
                    if name.ends_with(".zip") && name.contains("ptbr") && path.is_file() {
                        found.push(path);
                    }
                }
            }
        }
        let have = ptbr.join("romfs");
        if found.is_empty() && !have.is_dir() {
            return Err(Error::Build(
                "pt-BR is enabled but no ptbr zip was found in the game folder or private/translations"
                    .into(),
            ));
        }
        if let Some(newest) = found
            .into_iter()
            .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        {
            if !have.is_dir() || mtime_newer(&newest, &have) {
                to_install.push(("ptbr".to_owned(), newest));
            }
        }
    } else if !ptbr_enabled && !explicit_ptbr && std::fs::symlink_metadata(&ptbr).is_ok() {
        if crate::platform::is_dir_link(&ptbr) {
            crate::platform::unlink_dir(&ptbr)?;
        } else {
            std::fs::remove_dir_all(&ptbr)?;
        }
    }
    for (name, zpath) in mods {
        if !mod_component(name) || !zpath.is_file() {
            return Err(Error::Build("--mod wants <name>=<zip>".into()));
        }
        to_install.push(((*name).to_owned(), zpath.to_path_buf()));
    }
    for (name, zpath) in to_install {
        install_mod_archive(&mods_root, &name, &zpath, romfs, python)?;
    }
    Ok(())
}

/// Finds a player-supplied SpeedFix patch without using a stale output patch after a game change.
pub fn speedfix_patch(
    game: &Path,
    output: &Path,
    version: &str,
    source_changed: bool,
) -> Option<PathBuf> {
    let patch_name = if version == "3.0.2" {
        "speedfix_main_302.pchtxt"
    } else {
        "speedfix_main.pchtxt"
    };
    let mut candidates = Vec::new();
    if !game.as_os_str().is_empty() {
        candidates.push(game.join(patch_name));
        candidates.push(game.join("exefs").join(patch_name));
    }
    let output_patch = output.join("speedfix_main.pchtxt");
    let stale_marker = output.join(".speedfix-old-patch");
    let stale = std::fs::metadata(&output_patch)
        .ok()
        .and_then(|stat| {
            let mtime = stat
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_nanos();
            let fp = format!("{mtime} {}", stat.len());
            let recorded = std::fs::read_to_string(&stale_marker).ok()?;
            Some(recorded.trim() == fp)
        })
        .unwrap_or(false);
    if !source_changed && !stale {
        candidates.push(output_patch);
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Returns `["--variant", <patch>]` when SpeedFix is enabled for `main` on 3.0.2 or 3.0.3.
pub fn speedfix_args(
    enabled: bool,
    module: &str,
    version: &str,
    game: &Path,
    output: &Path,
    source_changed: bool,
) -> Result<Vec<String>> {
    if !enabled || module != "main" || !matches!(version, "3.0.2" | "3.0.3") {
        return Ok(Vec::new());
    }
    let patch = speedfix_patch(game, output, version, source_changed).ok_or_else(|| {
        Error::Build("SpeedFix is enabled but no patch for this game version was found".into())
    })?;
    Ok(vec!["--variant".to_owned(), patch.display().to_string()])
}

/// The Rust game stage: links ExeFS/RomFS/AOC, extracts flat NSO images (`nso2bin`), and
/// installs/removes opted-in fan mods.
pub fn rust_game(req: &Request, root: &Path) -> Result<()> {
    let stage = Stage::Game;
    let exefs = req.out.join("exefs");
    let romfs = req.out.join("extract/romfs");
    let aoc = req.out.join("aoc");
    let src_file = req.out.join("game_source.txt");
    let old = std::fs::read_to_string(&src_file)
        .ok()
        .map(|s| s.trim().to_owned())
        .unwrap_or_default();
    let real_exefs = exefs.is_dir() && !crate::platform::is_dir_link(&exefs);
    let source_changed =
        (!old.is_empty() && Path::new(&old) != req.game.root) || (old.is_empty() && real_exefs);
    if source_changed {
        if real_exefs {
            return Err(stage_err(
                stage,
                format!(
                    "{} holds a game imported into it: give the game folder its own --out folder",
                    req.out.display()
                ),
            ));
        }
        let old_patch = req.out.join("speedfix_main.pchtxt");
        if let Ok(stat) = std::fs::metadata(&old_patch) {
            if let Ok(mtime) = stat.modified().and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .map_err(std::io::Error::other)
            }) {
                let _ = crate::platform::atomic_write(
                    &req.out.join(".speedfix-old-patch"),
                    format!("{} {}\n", mtime.as_nanos(), stat.len()).as_bytes(),
                );
            }
        }
        for d in ["analysis", "gen", "spirv", "shaders_extracted"] {
            let _ = std::fs::remove_dir_all(req.out.join(d));
        }
        let _ = std::fs::remove_file(req.out.join("version.ini"));
    }

    crate::platform::link_dir(&req.game.exefs, &exefs)?;
    crate::platform::link_dir(&req.game.romfs, &romfs)?;
    let game_aoc = req.game.root.join("aoc");
    if !req.game.aoc.is_empty() && game_aoc.is_dir() {
        crate::platform::link_dir(&game_aoc, &aoc)?;
    } else if crate::platform::is_dir_link(&aoc) {
        crate::platform::unlink_dir(&aoc)?;
    }
    crate::platform::atomic_write(
        &src_file,
        format!("{}\n", req.game.root.display()).as_bytes(),
    )?;

    let analysis = req.out.join("analysis");
    std::fs::create_dir_all(&analysis)?;
    for m in ["main", "sdk", "subsdk0"] {
        let nso = exefs.join(m);
        let out_bin = analysis.join(format!("{m}.bin"));
        if !out_bin.is_file() || mtime_newer(&nso, &out_bin) {
            crate::nso2bin::write(&nso, &out_bin)?;
        }
    }

    install_mods(
        &req.out,
        &romfs,
        &req.game.root,
        root,
        req.fan_mods.ptbr,
        &[],
        script_python(req.target),
    )?;
    Ok(())
}

/// The Rust version stage: reads `exefs/main`'s build id, matches `games/acnh/versions.txt`,
/// and writes `<out>/version.ini`.
pub fn rust_version(req: &Request, root: &Path) -> Result<()> {
    let exefs = req.out.join("exefs");
    let bid = crate::nso::read_build_id(&exefs.join("main"))?;
    let mut known: Option<(String, String)> = None;
    if let Ok(table) = std::fs::read_to_string(root.join("games/acnh/versions.txt")) {
        for line in table.lines() {
            let code = line.split('#').next().unwrap_or("");
            let mut parts = code.split_whitespace();
            if let (Some(id), Some(ver), Some(hooks)) = (parts.next(), parts.next(), parts.next()) {
                if id.eq_ignore_ascii_case(&bid) {
                    known = Some((ver.to_owned(), hooks.to_owned()));
                    break;
                }
            }
        }
    }
    let game_ini = std::fs::canonicalize(&exefs)
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("game.ini")))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| Config::parse(&s).ok());
    let tv = game_ini
        .as_ref()
        .filter(|c| {
            c.get("build_id")
                .is_none_or(|b| b.eq_ignore_ascii_case(&bid))
        })
        .and_then(|c| c.get("title_version"))
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(-1);

    let (version, hooks) = match known {
        Some(pair) => pair,
        None => {
            let ver = match crate::version::from_build_id(&bid) {
                Ok(v) => v.name().to_owned(),
                Err(_) if tv == 0 => "1.0.0".to_owned(),
                Err(_) if tv == (29 << 16) => "2.0.7".to_owned(),
                Err(_) if tv == (33 << 16) => "3.0.2".to_owned(),
                Err(_) if tv > 0 => format!("release {}", tv >> 16),
                Err(_) => "unknown".to_owned(),
            };
            (ver, "-".to_owned())
        }
    };

    let content = format!(
        "# Written by acnh_core::prepare: the version this folder holds and its build profile.\n\
         build_id = {bid}\n\
         version = {version}\n\
         title_version = {tv}\n\
         hooks = {hooks}\n"
    );
    crate::platform::atomic_write(&req.out.join("version.ini"), content.as_bytes())?;
    Ok(())
}

fn default_jobs(requested: u32) -> u32 {
    if requested > 0 {
        return requested;
    }
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1)
        .max(1)
}

fn prepared_gen(out: &Path, module: &str, profile: &str, expected_hash: &str) -> Option<PathBuf> {
    let src = std::fs::read_to_string(out.join("game_source.txt")).ok()?;
    let pre = Path::new(src.trim()).join("gen");
    let same = std::fs::read_to_string(pre.join("recomp_source.txt"))
        .ok()
        .is_some_and(|h| h.trim() == expected_hash);
    let prof = std::fs::read_to_string(pre.join(module).join("profile.txt")).ok()?;
    let table = pre.join(module).join("table.cpp");
    if same && prof.trim() == profile && table.is_file() {
        Some(pre.join(module))
    } else {
        None
    }
}

fn rust_recompile(req: &Request, root: &Path, jobs: u32) -> Result<()> {
    let ver_ini = std::fs::read_to_string(req.out.join("version.ini"))
        .ok()
        .and_then(|s| Config::parse(&s).ok())
        .unwrap_or_default();
    let hooks = ver_ini.get("hooks").unwrap_or("-").to_owned();
    let version = ver_ini.get("version").unwrap_or("unknown").to_owned();
    let game_src = std::fs::read_to_string(req.out.join("game_source.txt"))
        .ok()
        .map(|s| PathBuf::from(s.trim()))
        .unwrap_or_else(|| req.game.root.clone());

    let jobs = default_jobs(jobs);
    let expected_hash = recomp_hash(root)?;
    let recorded_hash = std::fs::read_to_string(recomp_hash_file(req)).ok();
    let hash_matches = recorded_hash
        .as_deref()
        .is_some_and(|h| h.trim() == expected_hash);

    let logs = req.out.join("logs");
    std::fs::create_dir_all(&logs)?;

    for (module, base) in [
        ("sdk", 0x40_1000_0000u64),
        ("subsdk0", 0x40_1800_0000u64),
        ("main", 0x40_0000_0000u64),
    ] {
        let mut h = Vec::new();
        let hooks_path = if module == "main" && hooks != "-" {
            h.push("--hooks".to_owned());
            h.push(hooks.clone());
            Some(root.join(&hooks))
        } else {
            None
        };
        let variant = speedfix_args(
            req.fan_mods.speedfix,
            module,
            &version,
            &game_src,
            &req.out,
            false,
        )?;
        let pch = variant.get(1).cloned();
        h.extend(variant);
        let profile = if h.is_empty() {
            "none".to_owned()
        } else {
            h.join(" ")
        };

        let gen = req.out.join("gen").join(module);
        let table = gen.join("table.cpp");
        let prof_file = gen.join("profile.txt");
        let prof_matches = std::fs::read_to_string(&prof_file)
            .ok()
            .is_some_and(|p| p.trim() == profile);
        let stale = !table.is_file()
            || !hash_matches
            || !prof_matches
            || (module == "main"
                && req.fan_mods.speedfix
                && pch
                    .as_deref()
                    .is_some_and(|p| mtime_newer(Path::new(p), &table)))
            || (module == "main" && hooks != "-" && mtime_newer(&root.join(&hooks), &table));

        if !stale {
            continue;
        }
        if let Some(pre) = prepared_gen(&req.out, module, &profile, &expected_hash) {
            copy_tree(&pre, &gen)?;
            crate::platform::atomic_write(&prof_file, format!("{profile}\n").as_bytes())?;
            continue;
        }

        let log_path = logs.join(format!("recomp_{module}.log"));
        let mut log_file = std::io::BufWriter::new(std::fs::File::create(&log_path)?);
        let opts = acnh_recomp::RecompileOptions {
            nso: req.out.join("exefs").join(module),
            out: gen.clone(),
            name: module.to_owned(),
            base,
            jobs: jobs as usize,
            only: None,
            hooks: hooks_path,
            variant: pch.map(PathBuf::from),
        };
        let result = acnh_recomp::recompile(&opts, &mut |line| {
            let _ = writeln!(log_file, "{line}");
        });
        let _ = log_file.flush();
        if let Err(err) = result {
            return Err(Error::Build(format!("stage recompile ({module}): {err}")));
        }
        crate::platform::atomic_write(&prof_file, format!("{profile}\n").as_bytes())?;
    }
    Ok(())
}

fn find_build_tool(root: &Path, name: &str) -> PathBuf {
    let in_venv = venv_bin(root).join(executable(Os::current(), name));
    if in_venv.is_file() {
        return in_venv;
    }
    if let Some(on_path) = find_on_path(name) {
        return on_path;
    }
    for dir in ["/opt/homebrew/bin", "/usr/local/bin"] {
        let candidate = Path::new(dir).join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(name)
}

fn brew_prefix() -> String {
    let brew = find_on_path("brew").or_else(|| {
        ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"]
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
    });
    brew.and_then(|exe| {
        Command::new(exe)
            .arg("--prefix")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
    })
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| "/opt/homebrew".to_owned())
}

/// Builds `libacnh_runtime` (release, host target) into `<out>/runtime-target` and returns the
/// static library and the folder holding the `acnh_runtime.h` cbindgen generated for it.
fn build_runtime(
    req: &Request,
    root: &Path,
    env: &[(String, String)],
    logs: &Path,
) -> Result<(PathBuf, PathBuf)> {
    let stage = Stage::Build;
    let target = req.out.join("runtime-target");
    let cargo = find_cargo(root);
    // A user's `build.build-dir` would move the build scripts' out dirs away from the target dir.
    let mut cargo_env = env.to_vec();
    cargo_env.push((
        "CARGO_BUILD_BUILD_DIR".to_owned(),
        target.display().to_string(),
    ));
    run_logged(
        stage,
        &cargo,
        &[
            "build".to_owned(),
            "-p".to_owned(),
            "acnh-runtime".to_owned(),
            "--release".to_owned(),
            "--target-dir".to_owned(),
            target.display().to_string(),
        ],
        root,
        &cargo_env,
        &[],
        Some(&logs.join("runtime.log")),
    )?;
    let release = target.join("release");
    let lib = release.join(if cfg!(windows) {
        "acnh_runtime.lib"
    } else {
        "libacnh_runtime.a"
    });
    if !lib.is_file() {
        return Err(stage_err(stage, format!("{} was not built", lib.display())));
    }
    // Cargo's build-script out dir is `build/<package>/<hash>/out` (newer cargo) or
    // `build/<package>-<hash>/out` (older): search for the header under any `acnh-runtime*`
    // directory, and take the newest if there are several.
    let header = find_runtime_header(&release.join("build"));
    let include = header
        .and_then(|h| h.parent().map(Path::to_path_buf))
        .ok_or_else(|| stage_err(stage, "cbindgen did not generate acnh_runtime.h".to_owned()))?;
    Ok((lib, include))
}

/// The newest `<dir>/**/acnh-runtime*/**/out/include/acnh_runtime.h`.
fn find_runtime_header(dir: &Path) -> Option<PathBuf> {
    fn walk(dir: &Path, depth: u32, in_runtime: bool, found: &mut Vec<PathBuf>) {
        if depth > 4 {
            return;
        }
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let runtime = in_runtime
                || entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("acnh-runtime");
            if runtime {
                let header = path.join("out/include/acnh_runtime.h");
                if header.is_file() {
                    found.push(header);
                    continue;
                }
            }
            walk(&path, depth + 1, runtime, found);
        }
    }
    let mut found = Vec::new();
    walk(dir, 0, false, &mut found);
    found
        .into_iter()
        .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
}

/// `cargo`: the build tools' search, then the rustup home, which is not always on a GUI's PATH.
fn find_cargo(root: &Path) -> PathBuf {
    let found = find_build_tool(root, "cargo");
    if found.is_file() {
        return found;
    }
    let name = executable(Os::current(), "cargo");
    let homes = [
        std::env::var_os("CARGO_HOME").map(PathBuf::from),
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(|h| PathBuf::from(h).join(".cargo")),
    ];
    homes
        .into_iter()
        .flatten()
        .map(|h| h.join("bin").join(&name))
        .find(|p| p.is_file())
        .unwrap_or(found)
}

/// The Rust build stage: configures CMake with Ninja and builds the `acnh` target.
pub fn rust_build(req: &Request, root: &Path, jobs: u32) -> Result<()> {
    let stage = Stage::Build;
    let t = root.join("private/third_party");
    let logs = req.out.join("logs");
    std::fs::create_dir_all(&logs)?;
    let cmake = find_build_tool(root, "cmake");
    let ninja = find_build_tool(root, "ninja");
    let vkh = t.join(format!("Vulkan-Headers-{VK_HEADERS}"));
    let build = req.out.join("build");
    let jobs = default_jobs(jobs);

    let cache = build.join("CMakeCache.txt");
    if let Ok(text) = std::fs::read_to_string(&cache) {
        if let Some(home) = text
            .lines()
            .find_map(|l| l.strip_prefix("CMAKE_HOME_DIRECTORY:"))
            .and_then(|rest| rest.split_once('='))
            .map(|(_, val)| val.trim())
        {
            if Path::new(home) != root {
                let _ = std::fs::remove_file(&cache);
                let _ = std::fs::remove_dir_all(build.join("CMakeFiles"));
            }
        }
    }

    let mut cmake_args = vec![
        "-S".to_owned(),
        ".".to_owned(),
        "-B".to_owned(),
        build.display().to_string(),
        "-G".to_owned(),
        "Ninja".to_owned(),
        format!("-DCMAKE_MAKE_PROGRAM={}", ninja.display()),
        "-DNX_GAME=acnh".to_owned(),
        "-DCMAKE_BUILD_TYPE=Release".to_owned(),
        format!("-DNX_VULKAN_INCLUDE={}", vkh.join("include").display()),
        format!("-DNX_GEN_DIR={}", req.out.join("gen").display()),
    ];

    let env = if cfg!(windows) {
        cmake_args.push("-DCMAKE_C_COMPILER=clang-cl".to_owned());
        cmake_args.push("-DCMAKE_CXX_COMPILER=clang-cl".to_owned());
        let sdl = t.join(format!("SDL3-{SDL3}"));
        let movie = windows_movie_libs(&t);
        cmake_args.push(format!(
            "-DCMAKE_PREFIX_PATH={};{}",
            sdl.display(),
            movie.display()
        ));
        msvc_env(stage, root, &t)?
    } else {
        if cfg!(target_os = "macos") {
            cmake_args.push(format!("-DCMAKE_PREFIX_PATH={}", brew_prefix()));
        }
        vec![
            ("CC".to_owned(), "clang".to_owned()),
            ("CXX".to_owned(), "clang++".to_owned()),
        ]
    };

    // The C++ player needs the Rust runtime (config, saves, files, kernel ...): build it for the
    // host, then hand CMake the archive and cbindgen's header, as tools/build_ios.sh does.
    let (runtime_lib, runtime_include) = build_runtime(req, root, &env, &logs)?;
    cmake_args.push(format!("-DNX_RUNTIME_ARCHIVE={}", runtime_lib.display()));
    cmake_args.push(format!(
        "-DNX_RUNTIME_INCLUDE={}",
        runtime_include.display()
    ));

    run_logged(
        stage,
        &cmake,
        &cmake_args,
        root,
        &env,
        &[],
        Some(&logs.join("cmake.log")),
    )?;
    run_logged(
        stage,
        &ninja,
        &[
            "-C".to_owned(),
            build.display().to_string(),
            format!("-j{jobs}"),
            "acnh".to_owned(),
        ],
        root,
        &env,
        &[],
        None,
    )?;
    Ok(())
}

/// Whether everything `stage` leaves in `req.out` is there. Game and recompile leave one
/// file per module; upstream's build leaves the bare player in `build/` (on the Mac the
/// plain executable), which `assemble` then makes self-contained in the install folder.
fn stage_complete(stage: Stage, req: &Request, expected_recomp_hash: &str) -> bool {
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
        Stage::Recompile => {
            MODULES
                .iter()
                .all(|module| req.out.join("gen").join(module).join("table.cpp").is_file())
                && std::fs::read_to_string(recomp_hash_file(req))
                    .ok()
                    .is_some_and(|recorded| recorded.trim() == expected_recomp_hash)
        }
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
    let req = if matches!(req.target, Os::Ios | Os::Mac | Os::Android) {
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
    let expected_recomp_hash = recomp_hash(&runner.root())?;
    let before = STAGES.map(|stage| {
        stage_complete(stage, req, &expected_recomp_hash)
            && !(recheck
                && matches!(
                    stage,
                    Stage::Game | Stage::Recompile | Stage::Shaders | Stage::Build
                ))
    });
    let mut last = None;
    for (stage, was_there) in STAGES.into_iter().zip(before) {
        if was_there && stage_complete(stage, req, &expected_recomp_hash) {
            continue;
        }
        progress(stage);
        match step(stage, req) {
            Step::Script(_args) => runner.recompile(req)?,
            Step::Rust => match stage {
                Stage::Tools => runner.tools(req)?,
                Stage::Game => runner.game(req)?,
                Stage::Version => runner.version(req)?,
                Stage::Recompile => runner.recompile(req)?,
                Stage::Shaders => {
                    runner
                        .before_shaders()
                        .map_err(|log| build_error(stage, &log, home))?;
                    runner.shaders(req)?;
                }
                Stage::Fonts => runner.fonts(req)?,
                Stage::Build => runner.build(req)?,
            },
        }
        if stage == Stage::Tools {
            std::fs::create_dir_all(&req.out)?;
            std::fs::write(tools_record(req), "{\"tools\": \"ready\"}\n")?;
        }
        if stage == Stage::Recompile {
            let hash_file = recomp_hash_file(req);
            if let Some(parent) = hash_file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(hash_file, format!("{expected_recomp_hash}\n"))?;
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
        Os::Android => android_deliver(req, runner, home),
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
    let version = package_version(&runner.root());
    let (mods_dest, mods, mods_cache) = bundled_mods_folder(&req.out);
    for args in [
        build_args,
        bundle_mods_args(&mods_dest, &mods_cache),
        package_ios_args(&version, &build, &req.out, delivery, &mods),
    ] {
        runner
            .run(Stage::Build, &args)
            .map_err(|log| build_error(Stage::Build, &log, home))?;
        runner
            .finish()
            .map_err(|log| build_error(Stage::Build, &log, home))?;
    }
    Ok(ipa_path(delivery, &version))
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
    let version = package_version(&runner.root());
    let (mods_dest, mods, mods_cache) = bundled_mods_folder(&req.out);
    for args in [
        bundle_mods_args(&mods_dest, &mods_cache),
        package_mac_args(&version, &app, &req.out, delivery, &mods),
    ] {
        runner
            .run(Stage::Build, &args)
            .map_err(|log| build_error(Stage::Build, &log, home))?;
        runner
            .finish()
            .map_err(|log| build_error(Stage::Build, &log, home))?;
    }
    Ok(zip_path(delivery, &version))
}

/// What the `android` target does after the shared stages: ensures `romfs.pak` and `spirv.pak`,
/// builds `libacnh_game.so` when missing, stages the prepared game files into the Dropbox
/// delivery folder (`_transfers/ACNH-Universal/android/`), and writes `manifest.json` last.
fn android_deliver(req: &Request, runner: &mut dyn ScriptRunner, home: &Path) -> Result<PathBuf> {
    let delivery = req
        .delivery
        .as_deref()
        .ok_or_else(|| Error::Env("the delivery folder is not set".into()))?;
    if !(req.out.join("romfs.pak").is_file() && req.out.join("spirv.pak").is_file()) {
        pack_stage(req)?;
    }

    let root = runner.root();
    let lib_path = if let Some(found) = find_android_game_lib(&req.out, &root) {
        found
    } else {
        let args = vec!["tools/build_android.sh".to_owned()];
        runner
            .run(Stage::Build, &args)
            .map_err(|log| build_error(Stage::Build, &log, home))?;
        runner
            .finish()
            .map_err(|log| build_error(Stage::Build, &log, home))?;
        find_android_game_lib(&req.out, &root)
            .ok_or_else(|| Error::Build("libacnh_game.so was not built for Android".into()))?
    };

    let build_id = read_prepared_build_id(&req.out);
    let abi = nx_game_abi(&root);
    stage_android_delivery(&req.out, &lib_path, delivery, &build_id, &abi)?;
    Ok(delivery.to_path_buf())
}

fn find_android_game_lib(out: &Path, root: &Path) -> Option<PathBuf> {
    [
        out.join("libacnh_game.so"),
        out.join("game/android-arm64/libacnh_game.so"),
        out.join("build-android/libacnh_game.so"),
        root.join("private/acnh/apk/libacnh_game.so"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

fn read_prepared_build_id(out: &Path) -> String {
    if let Ok(ini) = std::fs::read_to_string(out.join("version.ini")) {
        if let Ok(cfg) = Config::parse(&ini) {
            if let Some(bid) = cfg.get("build_id") {
                if !bid.is_empty() {
                    return bid.to_owned();
                }
            }
        }
    }
    let main = out.join("exefs/main");
    if main.is_file() {
        if let Ok(bid) = crate::nso::read_build_id(&main) {
            return bid;
        }
    }
    String::new()
}

/// Compute `NX_GAME_ABI` matching `CMakeLists.txt` (first 12 hex digits of SHA-1 over
/// `runtime/include/nx/{cpu.h,module.h,runtime.h}`).
pub fn nx_game_abi(root: &Path) -> String {
    let mut text = Vec::new();
    for header in ["cpu.h", "module.h", "runtime.h"] {
        let path = root.join("runtime/include/nx").join(header);
        let Ok(bytes) = std::fs::read(&path) else {
            return "unknown".to_owned();
        };
        text.extend_from_slice(&bytes);
    }
    sha1_hex_12(&text)
}

fn sha1_hex_12(data: &[u8]) -> String {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (i, word) in w[..16].iter_mut().enumerate() {
            let b = &chunk[i * 4..i * 4 + 4];
            *word = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let full = format!(
        "{:08x}{:08x}{:08x}{:08x}{:08x}",
        h[0], h[1], h[2], h[3], h[4]
    );
    full[..12].to_owned()
}

/// Stage the prepared Android game files and `manifest.json` atomically into `delivery`.
pub fn stage_android_delivery(
    prepared_out: &Path,
    game_lib: &Path,
    delivery: &Path,
    build_id: &str,
    nx_game_abi: &str,
) -> Result<()> {
    let parent = delivery.parent().unwrap_or(delivery);
    std::fs::create_dir_all(parent)?;
    let name = delivery
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("android");
    let staging = parent.join(format!(".{name}.new"));
    let backup = parent.join(format!(".{name}.old"));

    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;

    std::fs::copy(game_lib, staging.join("libacnh_game.so"))?;
    for pak in ["romfs.pak", "spirv.pak"] {
        let src = prepared_out.join(pak);
        if !src.is_file() {
            return Err(Error::Build(format!("missing {}", src.display())));
        }
        std::fs::copy(&src, staging.join(pak))?;
    }
    for dir in ["analysis", "fonts", "aoc"] {
        let src_dir = prepared_out.join(dir);
        if src_dir.is_dir() {
            copy_dir_follow_symlinks(&src_dir, &staging.join(dir))?;
        }
    }

    let mut manifest_files = Vec::new();
    collect_manifest_files(&staging, Path::new(""), &mut manifest_files)?;
    manifest_files.sort_by(|a, b| a.path.cmp(&b.path));

    let manifest = crate::appgame::Manifest {
        version: 1,
        build_id: build_id.to_owned(),
        nx_game_abi: nx_game_abi.to_owned(),
        files: manifest_files,
    };
    // Write manifest.json last so an incomplete folder is never mistaken for a finished one.
    std::fs::write(staging.join("manifest.json"), manifest.to_json().as_bytes())?;

    if backup.exists() {
        std::fs::remove_dir_all(&backup)?;
    }
    if delivery.exists() {
        std::fs::rename(delivery, &backup)?;
    }
    std::fs::rename(&staging, delivery)?;
    if backup.exists() {
        let _ = std::fs::remove_dir_all(&backup);
    }
    Ok(())
}

fn copy_dir_follow_symlinks(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".DS_Store" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        let meta = std::fs::metadata(&src_path)?;
        if meta.is_dir() {
            copy_dir_follow_symlinks(&src_path, &dst_path)?;
        } else if meta.is_file() {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn collect_manifest_files(
    dir: &Path,
    prefix: &Path,
    out: &mut Vec<crate::appgame::ManifestFile>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == ".DS_Store" || name_str.starts_with("manifest.json") {
            continue;
        }
        let path = entry.path();
        let rel = prefix.join(&name);
        let meta = std::fs::metadata(&path)?;
        if meta.is_dir() {
            collect_manifest_files(&path, &rel, out)?;
        } else if meta.is_file() {
            let rel_str = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let sha256 = crate::appgame::sha256_file(&path)?;
            out.push(crate::appgame::ManifestFile {
                path: rel_str,
                size: meta.len(),
                sha256,
            });
        }
    }
    Ok(())
}

/// The build tools `os` needs before its first stage, searched for in `path` (and, on the Mac,
/// in `homebrew`). Required pieces stop a prepare; a warning names what the build loses
/// without it. The script fetches CMake and Ninja, the Vulkan headers, .NET, and LLVM and
/// SDL3 on Windows itself, so none of those is checked here.
pub fn toolchain(os: Os, path: impl AsRef<OsStr>) -> Vec<Missing> {
    let homebrew = [PathBuf::from("/opt/homebrew"), PathBuf::from("/usr/local")];
    let mut missing = toolchain_with(os, path, &homebrew);
    // cargo is often only in the rustup home, which a GUI's PATH lacks.
    if cargo_in_homes(os) {
        missing.retain(|piece| piece.what != "cargo");
    }
    missing
}

fn cargo_in_homes(os: Os) -> bool {
    let name = executable(os, "cargo");
    [
        std::env::var_os("CARGO_HOME").map(PathBuf::from),
        std::env::var_os(if os == Os::Windows {
            "USERPROFILE"
        } else {
            "HOME"
        })
        .map(|h| PathBuf::from(h).join(".cargo")),
    ]
    .into_iter()
    .flatten()
    .any(|h| h.join("bin").join(&name).is_file())
        // The same Homebrew places `find_build_tool` tries (`brew install rust`): an app opened
        // from the Finder has no Homebrew on its PATH, but the build stage still finds cargo there.
        || (os != Os::Windows
            && ["/opt/homebrew/bin", "/usr/local/bin"]
                .iter()
                .any(|d| Path::new(d).join(&name).is_file()))
}

fn missing_cargo(dirs: &[PathBuf], os: Os) -> Option<Missing> {
    find_tool(dirs, os, "cargo").is_none().then(|| {
        missing_piece(
            "cargo",
            "the player links the Rust runtime it builds.",
            if os == Os::Windows {
                "winget install Rustlang.Rustup"
            } else {
                "curl https://sh.rustup.rs -sSf | sh"
            },
            false,
        )
    })
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
        missing.extend(missing_cargo(&dirs, os));
        return missing;
    }

    missing.extend(missing_cargo(&dirs, os));
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
/// `None` when it cannot be told from the folder (a fake in a test). The `lib` folders of both
/// the path as found and the file it links to are looked in: Homebrew's `bin/python3` is a link
/// into its Cellar, and the `lib/python3.N` beside the link holds only `site-packages`.
fn has_venv(python: &Path) -> Option<bool> {
    let mut prefixes = vec![prefix_of(python.parent()?).to_path_buf()];
    if let Some(dir) = std::fs::canonicalize(python)
        .ok()
        .and_then(|real| real.parent().map(Path::to_path_buf))
    {
        let prefix = prefix_of(&dir).to_path_buf();
        if !prefixes.contains(&prefix) {
            prefixes.push(prefix);
        }
    }
    let mut roots = Vec::new();
    for prefix in &prefixes {
        if let Ok(entries) = std::fs::read_dir(prefix.join("lib")) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("python") {
                    roots.push(entry.path());
                }
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

#[cfg(test)]
mod apple_version_tests {
    use super::{package_version, short_build_version};

    #[test]
    fn a_packaged_source_without_git_is_named_by_its_build_version_file() {
        let root = std::env::temp_dir().join(format!("acnh-pkg-version-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // No .git, no file: the engine's own version, never git's 0.0.0 of a bare folder.
        assert_eq!(package_version(&root), env!("CARGO_PKG_VERSION"));
        std::fs::write(
            root.join("BUILD_VERSION"),
            "BASE_VER=0.11.0\nVNAME=0.11.0\nVCODE=1100891\n",
        )
        .unwrap();
        assert_eq!(package_version(&root), "0.11.0-1100891");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_short_build_is_the_base_and_the_code_without_a_plus() {
        assert_eq!(
            short_build_version("0.11.0", "1100891").as_deref(),
            Some("0.11.0-1100891")
        );
        assert_eq!(
            short_build_version("0.11.0+abc1234", "1100891").as_deref(),
            Some("0.11.0-1100891")
        );
        assert_eq!(
            short_build_version("0.0.0", "10042\n").as_deref(),
            Some("0.0.0-10042")
        );
        // What the package scripts would refuse is never made.
        assert_eq!(short_build_version("", "1100891"), None);
        assert_eq!(short_build_version("0.11.0", ""), None);
        assert_eq!(short_build_version("0.11.0", "11x"), None);
        assert_eq!(short_build_version("0.11 0", "1100"), None);
        assert_eq!(short_build_version("../0", "1100"), None);
    }
}
