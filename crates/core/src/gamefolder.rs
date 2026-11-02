//! The game folder check (SPEC.md M1): the layout upstream's `tools/setup_acnh.py`
//! (`find_game`, `from_importer`) and `launcher/files.cpp` (`game_parts`) accept, checked
//! before a prepare so a missing file is named at once. Lists names only; never reads game
//! files except `exefs/main`'s header (`nso`).
//!
//! ```text
//! <game folder>/exefs/main, sdk, subsdk0, main.npdm
//! <game folder>/romfs/            (or extract/romfs/)
//! <game folder>/aoc/<index>/      (add-on content, optional)
//! <game folder>/game.ini, .import/  (written by ACNHRecomp-Importer, optional)
//! ```

use crate::error::{not_built, Result};
use crate::version::Version;
use std::path::{Path, PathBuf};

/// The files `exefs/` must hold, in this order.
pub const EXEFS_FILES: [&str; 4] = ["main", "sdk", "subsdk0", "main.npdm"];

/// What `check` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameFolder {
    pub root: PathBuf,
    pub exefs: PathBuf,
    /// `<root>/romfs`, else `<root>/extract/romfs`.
    pub romfs: PathBuf,
    /// The folders under `<root>/aoc/`, sorted by name; empty when there is none.
    pub aoc: Vec<PathBuf>,
    /// True when `<root>/.import` is a folder (ACNHRecomp-Importer made it).
    pub from_importer: bool,
    /// From the build id of `exefs/main`, through the gate.
    pub version: Version,
}

/// Check the folder in this order, and stop at the first problem:
/// 1. `root` is not a folder: `Error::BadGameFolder("<root> is not a folder")`.
/// 2. no `exefs/` folder: `Error::BadGameFolder("exefs/ is missing")`.
/// 3. each of `EXEFS_FILES` in order, missing: `Error::BadGameFolder("exefs/<name> is missing")`.
/// 4. neither `romfs/` nor `extract/romfs/`: `Error::BadGameFolder("romfs/ is missing")`.
/// 5. `nso::read_build_id(exefs/main)`, then `version::from_build_id`; their errors pass
///    through unchanged.
pub fn check(root: &Path) -> Result<GameFolder> {
    not_built("gamefolder::check")
}
