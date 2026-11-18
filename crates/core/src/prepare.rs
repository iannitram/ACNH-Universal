//! Prepare (SPEC.md M1, M13): from a checked game folder to a build for one device family, on
//! a hub. The Rust replacement of upstream's `tools/setup_acnh.py` and the scripts it runs
//! (`acnh_version.py`, `nso2bin.py`, `pack_romfs.py`, `pack_spirv.py`, `extract_shaders.py`,
//! `sarc_endian.py`, `msg_rewrap.py`), stage by stage (`recipe::Stage`), each skipped when its
//! output is already there. Until a stage is ported it runs upstream's script (`Step::Script`),
//! so a build works at every step of the move.
//!
//! Everything a prepare writes is made from my game folder: it stays in `out` on the hub, and
//! reaches my other devices only through the hub, over the home network (M11, M14).

use crate::error::{Error, Result};
use crate::gamefolder::GameFolder;
use crate::platform::Os;
use crate::recipe::{hub_for, output, serves, Hub, Stage, STAGES};
use std::path::{Path, PathBuf};

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
/// `Script(["python3", "tools/setup_acnh.py", "--game", <game root>, "--out", <out>])` with
/// `FORCE=<stage key>` left to the caller (`python` instead of `python3` on Windows).
pub fn step(stage: Stage, req: &Request) -> Step {
    let _ = stage; // upstream's script runs every stage in one go; none is ported yet
    let python = if req.target == Os::Windows {
        "python"
    } else {
        "python3"
    };
    Step::Script(vec![
        python.to_owned(),
        "tools/setup_acnh.py".to_owned(),
        "--game".to_owned(),
        req.game.root.display().to_string(),
        "--out".to_owned(),
        req.out.display().to_string(),
    ])
}

/// Runs upstream's script for a stage. `Err` holds the log lines of a failed run.
pub trait ScriptRunner {
    fn run(&mut self, stage: Stage, args: &[String]) -> std::result::Result<(), Vec<String>>;
}

/// The real runner: a child process from the repo root, stdout and stderr as the log.
pub struct ProcessRunner;

impl ScriptRunner for ProcessRunner {
    fn run(&mut self, stage: Stage, args: &[String]) -> std::result::Result<(), Vec<String>> {
        let Some((program, rest)) = args.split_first() else {
            return Err(vec!["no command".to_owned()]);
        };
        let output = std::process::Command::new(program)
            .args(rest)
            .env("FORCE", stage.key())
            .output()
            .map_err(|e| vec![format!("cannot run {program}: {e}")])?;
        if output.status.success() {
            return Ok(());
        }
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        Err(text.lines().map(str::to_owned).collect())
    }
}

/// What `stage` leaves under `req.out`, when it leaves one thing to look for. `None`: the
/// stage has no output of its own there and always runs (upstream's script skips its own work).
fn stage_output(stage: Stage, req: &Request) -> Option<PathBuf> {
    match stage {
        Stage::Tools | Stage::Version => None,
        Stage::Game => Some(req.out.join("analysis").join("main.bin")),
        Stage::Recompile => Some(req.out.join("gen").join("main").join("table.cpp")),
        Stage::Shaders => Some(req.out.join("spirv").join("summary.txt")),
        Stage::Fonts => Some(
            req.out
                .join("fonts")
                .join("glyphs_xbox")
                .join("console")
                .join("romfs")
                .join("Font")
                .join("ScalableFont.sarc.zs"),
        ),
        Stage::Build => Some(req.out.join("build").join(output(req.target))),
    }
}

/// Run every stage in order, calling `progress` as each one starts, and return `req.out`. A
/// failed stage is `Error::Build` with the last 20 lines of its log, run through `redact`.
/// A target this hub does not serve (`recipe::serves`) is `Error::Unsupported`.
pub fn prepare(req: &Request, progress: &mut dyn FnMut(Stage)) -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    prepare_with(
        req,
        hub_for(Os::current()),
        &mut ProcessRunner,
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
    for stage in STAGES {
        if stage_output(stage, req).is_some_and(|path| path.exists()) {
            continue;
        }
        progress(stage);
        if let Step::Script(args) = step(stage, req) {
            if let Err(log) = runner.run(stage, &args) {
                let start = log.len().saturating_sub(20);
                let tail: Vec<String> = log[start..].iter().map(|l| redact(l, home)).collect();
                return Err(Error::Build(format!(
                    "stage {}: {}",
                    stage.key(),
                    tail.join("\n")
                )));
            }
        }
    }
    Ok(req.out.clone())
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
