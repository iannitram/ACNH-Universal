//! The island's save (SPEC.md M6): where it lives, what syncs, and the save-relay manifest.
//! Same layout as upstream's `runtime/src/saves.cpp`: one island per console, in
//! `<data root>/user/device_save/`; backups in `<data root>/user/backups/<stamp>/`. The layout
//! is the same on every device, so the island moves unchanged.

use crate::error::{not_built, Result};
use std::path::{Path, PathBuf};

/// The files an island must have (upstream's `import_save` check), in this order.
pub const REQUIRED: [&str; 3] = ["main.dat", "mainHeader.dat", "Villager0/personal.dat"];

/// `<data_root>/user/device_save`.
pub fn save_dir(data_root: &Path) -> PathBuf {
    not_built!("saves::save_dir")
}

/// `<data_root>/user/backups`.
pub fn backups_dir(data_root: &Path) -> PathBuf {
    not_built!("saves::backups_dir")
}

/// True when a path relative to `save_dir` (with `/`) syncs: every file in the save folder,
/// except a name ending in `.tmp`, `.bak` or `~` and anything under a part named `backups`.
/// An empty path, an absolute path (starting with `/`) or a path with a `..` part is false.
pub fn is_synced(rel_path: &str) -> bool {
    not_built!("saves::is_synced")
}

/// Check that `dir` holds an island: each of `REQUIRED` in order, missing, is
/// `Error::Saves("<name> missing")`. A missing `dir` is `Error::Saves("no island at <dir>")`.
pub fn check_island(dir: &Path) -> Result<()> {
    not_built("saves::check_island")
}

/// The save-relay manifest for this game (save-relay's `src/manifest.rs`, format 1), as
/// JSON text exactly (one line, no spaces, `\n` at the end):
///
/// ```text
/// {"format":1,"id":"acnh","name":"Animal Crossing: New Horizons","saves":{},"saves_from":"launcher","include":["**"],"exclude":["**/*.tmp","**/*.bak","**/*~","backups/**"]}
/// ```
///
/// `saves_from` is "launcher": the launcher passes `--saves-dir <save_dir>` on every device.
pub fn relay_manifest() -> String {
    not_built!("saves::relay_manifest")
}
