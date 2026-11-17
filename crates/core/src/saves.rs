//! The island's save (SPEC.md M6): where it lives, what syncs, and the save-relay manifest.
//! Same layout as upstream's `runtime/src/saves.cpp`: one island per console, in
//! `<data root>/user/device_save/`; backups in `<data root>/user/backups/<stamp>/`. The layout
//! is the same on every device, so the island moves unchanged.

use crate::error::{Error, Result};
use std::path::{Path, PathBuf};

/// The files an island must have (upstream's `import_save` check), in this order.
pub const REQUIRED: [&str; 3] = ["main.dat", "mainHeader.dat", "Villager0/personal.dat"];

/// `<data_root>/user/device_save`.
pub fn save_dir(data_root: &Path) -> PathBuf {
    data_root.join("user").join("device_save")
}

/// `<data_root>/user/backups`.
pub fn backups_dir(data_root: &Path) -> PathBuf {
    data_root.join("user").join("backups")
}

/// True when a path relative to `save_dir` (with `/`) syncs: every file in the save folder,
/// except a name ending in `.tmp`, `.bak` or `~` and anything under a part named `backups`.
/// An empty path, an absolute path (starting with `/`) or a path with a `..` part is false.
pub fn is_synced(rel_path: &str) -> bool {
    if rel_path.is_empty() || rel_path.starts_with('/') {
        return false;
    }
    if rel_path.ends_with(".tmp") || rel_path.ends_with(".bak") || rel_path.ends_with('~') {
        return false;
    }
    for part in rel_path.split('/') {
        if part == ".." || part == "backups" {
            return false;
        }
    }
    true
}

/// Check that `dir` holds an island: each of `REQUIRED` in order, missing, is
/// `Error::Saves("<name> missing")`. A missing `dir` is `Error::Saves("no island at <dir>")`.
pub fn check_island(dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        return Err(Error::Saves(format!("no island at {}", dir.display())));
    }
    for file in REQUIRED {
        if !dir.join(file).is_file() {
            return Err(Error::Saves(format!("{file} missing")));
        }
    }
    Ok(())
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
    "{\"format\":1,\"id\":\"acnh\",\"name\":\"Animal Crossing: New Horizons\",\"saves\":{},\"saves_from\":\"launcher\",\"include\":[\"**\"],\"exclude\":[\"**/*.tmp\",\"**/*.bak\",\"**/*~\",\"backups/**\"]}\n".into()
}
