//! Island backups before an update (SPEC.md M7), in upstream's place and shape:
//! `<data root>/user/backups/<YYYYmmdd-HHMMSS>/` holding the synced files of the save folder
//! (`saves::is_synced`) at their relative paths, the 10 newest kept, so a person can restore
//! one by hand. Copied from iannitram/totk-universal's `backup` (MIT) by its issue.

use crate::error::{not_built, Result};
use std::path::{Path, PathBuf};

/// How many backups `prune` keeps after an update.
pub const KEEP: usize = 10;

/// The stamp for `unix` seconds (UTC): `YYYYmmdd-HHMMSS`, e.g. 0 is "19700101-000000".
pub fn stamp(unix: u64) -> String {
    not_built!("backup::stamp")
}

/// Copy every synced file under `save_dir` into `<backups>/<stamp>/` (creating folders as
/// needed) and return that folder. Files that do not sync are left out. An existing
/// `<backups>/<stamp>` is `Error::Saves("backup <stamp> exists")` and nothing is written.
/// A missing `save_dir` is `Error::Saves("no save folder at <save_dir>")`.
pub fn backup_saves(save_dir: &Path, backups: &Path, stamp: &str) -> Result<PathBuf> {
    not_built("backup::backup_saves")
}

/// The backups in `backups`, by folder name, oldest first (names are stamps, which sort by
/// time). A missing `backups` folder is an empty list. Files are not listed.
pub fn list(backups: &Path) -> Result<Vec<String>> {
    not_built("backup::list")
}

/// Delete the oldest backups so that `keep` remain; return the deleted names, oldest first.
pub fn prune(backups: &Path, keep: usize) -> Result<Vec<String>> {
    not_built("backup::prune")
}
