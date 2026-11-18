//! Island backups before an update (SPEC.md M7), in upstream's place and shape:
//! `<data root>/user/backups/<YYYYmmdd-HHMMSS>/` holding the synced files of the save folder
//! (`saves::is_synced`) at their relative paths, the 10 newest kept, so a person can restore
//! one by hand. Copied from iannitram/totk-universal's `backup` (MIT) by its issue.

use crate::error::{Error, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// How many backups `prune` keeps after an update.
pub const KEEP: usize = 10;

/// The stamp for `unix` seconds (UTC): `YYYYmmdd-HHMMSS`, e.g. 0 is "19700101-000000".
pub fn stamp(unix: u64) -> String {
    let secs = (unix % 60) as u32;
    let mins = ((unix / 60) % 60) as u32;
    let hours = ((unix / 3600) % 24) as u32;
    let days = (unix / 86400) as i64;

    // Howard Hinnant's civil date algorithm from days since 1970-01-01 (epoch)
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!("{year:04}{m:02}{d:02}-{hours:02}{mins:02}{secs:02}")
}

fn path_to_forward_slash(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for c in path.components() {
        match c {
            std::path::Component::Normal(s) => parts.push(s.to_str()?),
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

/// Copy every synced file under `save_dir` into `<backups>/<stamp>/` (creating folders as
/// needed) and return that folder. Files that do not sync are left out. An existing
/// `<backups>/<stamp>` is `Error::Saves("backup <stamp> exists")` and nothing is written.
/// A missing `save_dir` is `Error::Saves("no save folder at <save_dir>")`.
pub fn backup_saves(save_dir: &Path, backups: &Path, stamp: &str) -> Result<PathBuf> {
    if !save_dir.is_dir() {
        return Err(Error::Saves(format!(
            "no save folder at {}",
            save_dir.display()
        )));
    }
    let dest = backups.join(stamp);
    if dest.exists() {
        return Err(Error::Saves(format!("backup {stamp} exists")));
    }
    fs::create_dir_all(&dest)?;
    let mut files = Vec::new();
    collect_files(save_dir, &mut files)?;
    for file in files {
        let Ok(rel) = file.strip_prefix(save_dir) else {
            continue;
        };
        let Some(rel_str) = path_to_forward_slash(rel) else {
            continue;
        };
        if crate::saves::is_synced(&rel_str) {
            let target = dest.join(rel);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&file, &target)?;
        }
    }
    Ok(dest)
}

/// The backups in `backups`, by folder name, oldest first (names are stamps, which sort by
/// time). A missing `backups` folder is an empty list. Files are not listed.
pub fn list(backups: &Path) -> Result<Vec<String>> {
    if !backups.exists() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(backups)? {
        let entry = entry?;
        if entry.path().is_dir() {
            if let Ok(name) = entry.file_name().into_string() {
                names.push(name);
            }
        }
    }
    names.sort();
    Ok(names)
}

/// Delete the oldest backups so that `keep` remain; return the deleted names, oldest first.
pub fn prune(backups: &Path, keep: usize) -> Result<Vec<String>> {
    let all = list(backups)?;
    if all.len() <= keep {
        return Ok(Vec::new());
    }
    let to_remove_count = all.len() - keep;
    let (to_remove, _) = all.split_at(to_remove_count);
    let mut removed = Vec::with_capacity(to_remove_count);
    for name in to_remove {
        fs::remove_dir_all(backups.join(name))?;
        removed.push(name.clone());
    }
    Ok(removed)
}
