//! Updating the launcher or the build (SPEC.md M7): back the island up first, rebuild, and
//! check it is still there. Copied from iannitram/totk-universal's `update` (MIT) by its issue.

use crate::backup::{backup_saves, prune, KEEP};
use crate::error::{Error, Result};
use crate::saves::{check_island, is_synced};
use std::fs;
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

/// Back up `save_dir` into `backups` under `stamp`, prune to the newest `backup::KEEP`, run
/// `rebuild`, then check the island is still there and count the synced files again. A
/// failure after the backup keeps it and names its folder (`; restore <backup>`), so the
/// island can be put back by hand. Fewer synced files after than before is
/// `Error::Saves("saves changed during the update; restore <backup>")` and nothing is
/// deleted.
pub fn update(
    save_dir: &Path,
    backups: &Path,
    stamp: &str,
    rebuild: &mut dyn FnMut() -> Result<()>,
) -> Result<Updated> {
    let backup = backup_saves(save_dir, backups, stamp)?;
    prune(backups, KEEP).map_err(|error| name_backup(error, &backup))?;
    let files_before = synced_files(save_dir).map_err(|error| name_backup(error, &backup))?;
    rebuild().map_err(|error| name_backup(error, &backup))?;
    check_island(save_dir).map_err(|error| name_backup(error, &backup))?;
    let files_after = synced_files(save_dir).map_err(|error| name_backup(error, &backup))?;
    if files_after < files_before {
        return Err(Error::Saves(format!(
            "saves changed during the update; restore {}",
            backup.display()
        )));
    }
    Ok(Updated {
        backup,
        files_before,
        files_after,
    })
}

/// The synced files under `dir`, without following anything outside it.
fn synced_files(dir: &Path) -> Result<usize> {
    let mut count = 0;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.is_file() {
                let Ok(rel) = path.strip_prefix(dir) else {
                    continue;
                };
                if let Some(rel) = forward_slash(rel) {
                    if is_synced(&rel) {
                        count += 1;
                    }
                }
            }
        }
    }
    Ok(count)
}

fn forward_slash(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for part in path.components() {
        match part {
            std::path::Component::Normal(name) => parts.push(name.to_str()?),
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

/// `error` with its backup named, so a failure after the backup can be undone by hand. The
/// variant is kept when it carries a message.
fn name_backup(error: Error, backup: &Path) -> Error {
    let note = format!("; restore {}", backup.display());
    match error {
        Error::NotAcnh(m) => Error::NotAcnh(format!("{m}{note}")),
        Error::BadGameFolder(m) => Error::BadGameFolder(format!("{m}{note}")),
        Error::Env(m) => Error::Env(format!("{m}{note}")),
        Error::Unsupported(m) => Error::Unsupported(format!("{m}{note}")),
        Error::Config(m) => Error::Config(format!("{m}{note}")),
        Error::Steam(m) => Error::Steam(format!("{m}{note}")),
        Error::Saves(m) => Error::Saves(format!("{m}{note}")),
        Error::Build(m) => Error::Build(format!("{m}{note}")),
        Error::Hub(m) => Error::Hub(format!("{m}{note}")),
        Error::Io(m) => Error::Io(format!("{m}{note}")),
        other => Error::Saves(format!("{other}{note}")),
    }
}
