//! Updating the launcher or the build (SPEC.md M7): back the island up first, rebuild, and
//! check it is still there. Copied from iannitram/totk-universal's `update` (MIT) by its issue.

use crate::error::{not_built, Result};
use std::path::{Path, PathBuf};

/// What an update did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Updated {
    /// The backup made before the rebuild (`backup::backup_saves`).
    pub backup: PathBuf,
    /// Synced files in the save folder before and after; equal on success.
    pub files_before: usize,
    pub files_after: usize,
}

/// Back up `save_dir` into `backups` under `stamp`, run `rebuild`, then count the synced
/// files again. A rebuild error is returned after the backup (the backup stays). Fewer synced
/// files after than before is `Error::Saves("saves changed during the update; restore
/// <backup>")` and nothing is deleted. Keeps the newest `backup::KEEP` backups.
pub fn update(
    save_dir: &Path,
    backups: &Path,
    stamp: &str,
    rebuild: &mut dyn FnMut() -> Result<()>,
) -> Result<Updated> {
    not_built("update::update")
}
