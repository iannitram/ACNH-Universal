//! Prepare (SPEC.md M1, M13): from a checked game folder to a build for one device family, on
//! a hub. The Rust replacement of upstream's `tools/setup_acnh.py` and the scripts it runs
//! (`acnh_version.py`, `nso2bin.py`, `pack_romfs.py`, `pack_spirv.py`, `extract_shaders.py`,
//! `sarc_endian.py`, `msg_rewrap.py`), stage by stage (`recipe::Stage`), each skipped when its
//! output is already there. Until a stage is ported it runs upstream's script (`Step::Script`),
//! so a build works at every step of the move.
//!
//! Everything a prepare writes is made from my game folder: it stays in `out` on the hub, and
//! reaches my other devices only through the hub, over the home network (M11, M14).

use crate::error::{not_built, Result};
use crate::gamefolder::GameFolder;
use crate::platform::Os;
use crate::recipe::Stage;
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
    not_built!("prepare::step")
}

/// Run every stage in order, calling `progress` as each one starts, and return `req.out`. A
/// failed stage is `Error::Build` with the last 20 lines of its log, run through `redact`.
/// A target this hub does not serve (`recipe::serves`) is `Error::Unsupported`.
pub fn prepare(req: &Request, progress: &mut dyn FnMut(Stage)) -> Result<PathBuf> {
    not_built("prepare::prepare")
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
