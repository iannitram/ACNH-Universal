//! The runner of a prepare: `ScriptRunner`, the real `ProcessRunner` and the pre-flight checks.

use super::*;
use crate::fsutil::GitTree;

pub(super) fn script_python(target: Os) -> &'static str {
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
        for warning in rust_game(req, &self.root())? {
            self.warn(&warning);
        }
        Ok(())
    }

    /// A warning a stage wants shown: the build goes on. The default drops it; the process
    /// runner keeps it for `prepare_with_warnings`.
    fn warn(&mut self, _message: &str) {}

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

    /// Whether the tools the Tools stage leaves under the source folder are all there and
    /// current. A test runner has no tools to look for, so the default says yes; the process
    /// runner looks (`tools_current`).
    fn tools_current(&self) -> bool {
        true
    }

    /// The Rust fonts stage. The default runs it (`rust_fonts`).
    fn fonts(&mut self, req: &Request) -> Result<()> {
        for warning in rust_fonts(req)? {
            self.warn(&warning);
        }
        Ok(())
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
pub(super) fn absolute(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path)
        .map_err(|e| Error::Io(format!("cannot make {} absolute: {e}", path.display())))
}

/// The folders a Finder-launched app's minimal `PATH` misses: cargo's, then Homebrew's.
pub(super) fn tool_dirs(home: &Path) -> Vec<PathBuf> {
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
pub(super) fn probe_tool(
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
pub(super) fn cargo_dir(runner: &mut dyn ScriptRunner, home: &Path) -> Option<PathBuf> {
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
pub(super) fn check_ios_tools(runner: &mut dyn ScriptRunner, home: &Path) -> Result<()> {
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
pub(super) fn check_outside_git(
    req: &Request,
    runner: &mut dyn ScriptRunner,
    home: &Path,
) -> Result<()> {
    // A relative `out` is taken from the working folder.
    let out = absolute(&req.out)?;
    let existing = crate::fsutil::nearest_existing_dir(&out);
    let mut ask = |args: &[&str]| match runner.probe("git", args) {
        Some(answer) => Some((true, answer, String::new())),
        None => runner
            .probe_stderr("git", args)
            .filter(|(ok, _)| !ok)
            .map(|(ok, error)| (ok, String::new(), error)),
    };
    match crate::fsutil::git_in_work_tree(existing, &mut ask) {
        GitTree::Inside => Err(Error::Build(format!(
            "the prepared folder {} is inside a git work tree: the Apple builds hold the \
             recompiled game and must not be made there. Use a folder outside it.",
            redact(&out.display().to_string(), home)
        ))),
        GitTree::Outside => Ok(()),
        GitTree::Unknown(_) => Err(Error::Build(format!(
            "cannot tell whether {} is inside a git work tree (is git installed?): the \
             Apple builds hold the recompiled game and must not be made in one",
            redact(&out.display().to_string(), home)
        ))),
    }
}

/// The real runner: a command as a child process in the source folder, run to its end. A
/// failed run's log holds what it wrote to standard output, then to standard error.
pub struct ProcessRunner {
    root: PathBuf,
    jobs: u32,
    warnings: Vec<String>,
}

impl ProcessRunner {
    /// A runner for commands in `root` (the source folder). `jobs` above 0 is passed on as
    /// `JOBS`.
    pub fn new(root: PathBuf, jobs: u32) -> ProcessRunner {
        ProcessRunner {
            root,
            jobs,
            warnings: Vec::new(),
        }
    }

    /// The warnings the stages raised so far, in order; the runner forgets them.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }
}

impl ScriptRunner for ProcessRunner {
    fn warn(&mut self, message: &str) {
        self.warnings.push(message.to_owned());
    }

    fn run(&mut self, _stage: Stage, args: &[String]) -> std::result::Result<(), Vec<String>> {
        let Some((program, rest)) = args.split_first() else {
            return Err(vec!["no command".to_owned()]);
        };
        // No input. `FORCE` does not pass: it redoes work that is there.
        let mut command = Command::new(program);
        command
            .args(rest)
            .current_dir(&self.root)
            .env_remove("FORCE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if self.jobs > 0 {
            command.env("JOBS", self.jobs.to_string());
        }
        let mut child = command
            .spawn()
            .map_err(|e| vec![format!("cannot run {program}: {e}")])?;
        // Standard error is read on its own thread, so a command that fills it never blocks.
        let stderr = child.stderr.take().map(|pipe| {
            let mut pipe = BufReader::new(pipe);
            std::thread::spawn(move || {
                std::iter::from_fn(|| next_line(&mut pipe)).collect::<Vec<String>>()
            })
        });
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(vec![format!("cannot read the output of {program}")]);
        };
        let mut stdout = BufReader::new(stdout);
        let mut log: Vec<String> = std::iter::from_fn(|| next_line(&mut stdout)).collect();
        let status = child.wait();
        if let Some(stderr) = stderr {
            log.extend(stderr.join().unwrap_or_default());
        }
        match status {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => {
                if log.is_empty() {
                    log.push(format!("{}: {status}", args.join(" ")));
                }
                Err(log)
            }
            Err(e) => {
                log.push(format!("cannot wait for {}: {e}", args.join(" ")));
                Err(log)
            }
        }
    }

    fn recompile(&mut self, req: &Request) -> Result<()> {
        rust_recompile(req, &self.root, self.jobs)
    }

    fn build(&mut self, req: &Request) -> Result<()> {
        rust_build(req, &self.root, self.jobs)
    }

    fn tools_current(&self) -> bool {
        tools_current(&self.root)
    }

    fn root(&self) -> PathBuf {
        self.root.clone()
    }

    fn set_root(&mut self, root: &Path) {
        self.root = root.to_owned();
    }
}

/// The next line of `pipe` without its line end, bytes that are not UTF-8 replaced; `None`
/// at its end.
pub(super) fn next_line(pipe: &mut impl BufRead) -> Option<String> {
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
