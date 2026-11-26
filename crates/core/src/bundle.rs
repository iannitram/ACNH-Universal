//! Build bundles (SPEC.md M11; docs/adr/0004-build-hubs.md): the player a hub built, as a
//! list of files with their hashes, which a hub serves and a client checks before it installs.
//! Shape from iannitram/totk-universal's `bundle` (MIT).
//! Implementation copied from totk-universal's bundle module.
//!
//! A bundle never holds game data: not the game folder, not what is made from it (the game
//! library, `romfs.pak`, `spirv.pak`, `gen/`), not a save. `is_forbidden` is checked for every
//! file. The game data a prepare made goes to my own devices separately, over the paired home
//! network connection only (`hub`, the game-data transfer), never in a bundle.

use crate::error::{Error, Result};
use crate::platform::Os;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

/// One file of a build, its path relative to the build folder (with `/`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleFile {
    pub path: String,
    pub size: u64,
    /// Lowercase hex SHA-256 of the file.
    pub sha256: String,
}

/// A finished player build for one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    pub target: Os,
    /// The game version it was built for ("3.0.2").
    pub version: String,
    /// The commit of this repo it was built from.
    pub commit: String,
    /// `build_id`: names this exact set of files.
    pub build_id: String,
    /// Sorted by `path`.
    pub files: Vec<BundleFile>,
}

/// The entries at the top of an install that belong to the device, not the build: never
/// bundled, and kept from the old install by `install`: `root` is the data root (the island,
/// `config.ini`, the fan mods, and the game data a prepare made), `logs` the launcher's logs.
pub const DEVICE_OWNED: &[&str] = &["root", "logs"];

/// True when a path (relative, with `/`) must never be in a bundle: a file name ending, in any
/// case, in `.nsp`, `.xci`, `.nca`, `.nsz`, `.xcz`, `.nso`, `.pak` or `.keys`; a file whose
/// name starts with `lib` and contains `_game.` (the game library); any path with a part
/// `exefs`, `romfs`, `aoc`, `gen`, `game`, `device_save` or `backups`; or anything whose first
/// part is in `DEVICE_OWNED`.
pub fn is_forbidden(rel_path: &str) -> bool {
    let lower = rel_path.to_ascii_lowercase();
    let parts: Vec<_> = lower.split('/').collect();
    let Some(name) = parts.last() else {
        return false;
    };

    [
        ".nsp", ".xci", ".nca", ".nsz", ".xcz", ".nso", ".pak", ".keys",
    ]
    .iter()
    .any(|suffix| name.ends_with(suffix))
        || (name.starts_with("lib") && name.contains("_game."))
        || parts.iter().any(|part| {
            matches!(
                *part,
                "exefs" | "romfs" | "aoc" | "gen" | "game" | "device_save" | "backups"
            )
        })
        || DEVICE_OWNED
            .iter()
            .any(|owned| parts.first().is_some_and(|part| part == owned))
}

/// The bundle as text, one item per line, each line ending in `\n`: `acnh-bundle 1`,
/// `target <os key>`, `version <version>`, `commit <commit>`, `build <build_id>`, then one
/// `file <size> <sha256> <path>` line per file in order.
pub fn to_text(b: &Bundle) -> String {
    let mut text = format!(
        "acnh-bundle 1\ntarget {}\nversion {}\ncommit {}\nbuild {}\n",
        b.target.key(),
        b.version,
        b.commit,
        b.build_id
    );
    for file in &b.files {
        text.push_str(&format!(
            "file {} {} {}\n",
            file.size, file.sha256, file.path
        ));
    }
    text
}

/// Reads `to_text`'s format back. Any other first line, a missing or out-of-order header
/// line, an unknown os key, a number that does not parse, or a sha256 that is not 64 lowercase
/// hex digits is `Error::Hub("bad bundle line <n>: <the line>")` (n from 1); a forbidden path
/// is `Error::Hub("refusing to bundle <path>: game data and saves never go in a bundle")`.
pub fn from_text(text: &str) -> Result<Bundle> {
    let lines: Vec<_> = text.lines().collect();
    let bad_line = |index: usize| {
        Error::Hub(format!(
            "bad bundle line {}: {}",
            index + 1,
            lines.get(index).copied().unwrap_or("")
        ))
    };

    if lines.first().copied().unwrap_or("") != "acnh-bundle 1" {
        return Err(bad_line(0));
    }

    let target_line = lines.get(1).copied().unwrap_or("");
    let target = target_line
        .strip_prefix("target ")
        .and_then(Os::from_key)
        .ok_or_else(|| bad_line(1))?;

    let version_line = lines.get(2).copied().unwrap_or("");
    let version = version_line
        .strip_prefix("version ")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| bad_line(2))?
        .to_string();

    let commit_line = lines.get(3).copied().unwrap_or("");
    let commit = commit_line
        .strip_prefix("commit ")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| bad_line(3))?
        .to_string();

    let build_line = lines.get(4).copied().unwrap_or("");
    let build_id = build_line
        .strip_prefix("build ")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| bad_line(4))?
        .to_string();

    let mut files = Vec::new();
    for (index, line) in lines.iter().enumerate().skip(5) {
        let fields: Vec<_> = line.splitn(4, ' ').collect();
        let parsed = if fields.len() == 4 && fields[0] == "file" {
            fields[1].parse::<u64>().ok().and_then(|size| {
                let sha256 = fields[2];
                let path = fields[3];
                (sha256.len() == 64
                    && sha256
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    && !path.is_empty())
                .then(|| (size, sha256.to_string(), path.to_string()))
            })
        } else {
            None
        };
        let Some((size, sha256, path)) = parsed else {
            return Err(bad_line(index));
        };
        if is_forbidden(&path) {
            return Err(Error::Hub(format!(
                "refusing to bundle {path}: game data and saves never go in a bundle"
            )));
        }
        files.push(BundleFile { path, size, sha256 });
    }

    Ok(Bundle {
        target,
        version,
        commit,
        build_id,
        files,
    })
}

/// The bundle of a build folder: every file under `dir` (recursively; folders themselves are
/// not listed), skipping anything in `DEVICE_OWNED`; a forbidden file is the `from_text`
/// refusal error. `build_id` is the first 16 hex digits of the SHA-256 of all the
/// `file <size> <sha256> <path>\n` lines joined in order.
pub fn manifest(dir: &Path, target: Os, version: &str, commit: &str) -> Result<Bundle> {
    fn walk(root: &Path, folder: &Path, files: &mut Vec<BundleFile>) -> Result<()> {
        for entry in fs::read_dir(folder)? {
            let entry = entry?;
            let path = entry.path();
            let relative = path.strip_prefix(root).expect("walked path is under root");
            let rel_path = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            if relative.components().count() == 1 && DEVICE_OWNED.contains(&rel_path.as_str()) {
                continue;
            }
            if entry.file_type()?.is_dir() {
                walk(root, &path, files)?;
            } else {
                if is_forbidden(&rel_path) {
                    return Err(Error::Hub(format!(
                        "refusing to bundle {rel_path}: game data and saves never go in a bundle"
                    )));
                }
                let bytes = fs::read(&path)?;
                files.push(BundleFile {
                    path: rel_path,
                    size: bytes.len() as u64,
                    sha256: format!("{:x}", Sha256::digest(&bytes)),
                });
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    walk(dir, dir, &mut files)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let lines = files
        .iter()
        .map(|file| format!("file {} {} {}\n", file.size, file.sha256, file.path))
        .collect::<String>();
    let build_id = format!("{:x}", Sha256::digest(lines.as_bytes()))[..16].to_string();
    Ok(Bundle {
        target,
        version: version.to_string(),
        commit: commit.to_string(),
        build_id,
        files,
    })
}

/// Checks every file of `b` in `dir` for its size and hash, in order; the first that is
/// missing or differs is `Error::Hub("<path> does not match the bundle")`. Files in `dir`
/// that the bundle does not list are ignored.
pub fn verify(b: &Bundle, dir: &Path) -> Result<()> {
    for file in &b.files {
        let path: PathBuf = dir.join(&file.path);
        let matches = fs::read(path).ok().is_some_and(|bytes| {
            bytes.len() as u64 == file.size
                && format!("{:x}", Sha256::digest(&bytes)) == file.sha256
        });
        if !matches {
            return Err(Error::Hub(format!(
                "{} does not match the bundle",
                file.path
            )));
        }
    }
    Ok(())
}

/// A cheap `verify`: every file of `b` in `dir` exists with the listed size, with no file read
/// or hashed. The first that is missing or differs is `Error::Hub("<path> does not match the
/// bundle")`.
pub fn check_sizes(b: &Bundle, dir: &Path) -> Result<()> {
    for file in &b.files {
        let matches = fs::metadata(dir.join(&file.path))
            .ok()
            .is_some_and(|meta| meta.is_file() && meta.len() == file.size);
        if !matches {
            return Err(Error::Hub(format!(
                "{} does not match the bundle",
                file.path
            )));
        }
    }
    Ok(())
}

/// Installs a checked build: `verify(b, staged)` first (nothing changes if it fails); then
/// move each `DEVICE_OWNED` entry the old `install_dir` has into `staged`, rename
/// `install_dir` to `<install_dir>.old`, rename `staged` to `install_dir`, and delete
/// `<install_dir>.old`. With no old `install_dir`, `staged` is just renamed. The island lives
/// in `root/`, so it moves with it; the update flow backs it up before this runs (M7).
/// A failed step undoes the steps before it: the old install is back in `install_dir` with its
/// device-owned entries, and `staged` still holds the new build. A leftover `<install_dir>.old`
/// from an earlier run is removed first; a failure to delete the old install after the swap is
/// not reported, and the next install removes it.
pub fn install(b: &Bundle, staged: &Path, install_dir: &Path) -> Result<()> {
    verify(b, staged)?;

    let mut old_name = install_dir.as_os_str().to_os_string();
    old_name.push(".old");
    let old_dir = PathBuf::from(old_name);
    if old_dir.exists() {
        fs::remove_dir_all(&old_dir)?;
    }
    if !install_dir.exists() {
        fs::rename(staged, install_dir)?;
        return Ok(());
    }

    let mut moved = Vec::new();
    for &entry in DEVICE_OWNED {
        let from = install_dir.join(entry);
        if from.exists() {
            if let Err(error) = fs::rename(&from, staged.join(entry)) {
                put_back(&moved, staged, install_dir);
                return Err(error.into());
            }
            moved.push(entry);
        }
    }

    if let Err(error) = fs::rename(install_dir, &old_dir) {
        put_back(&moved, staged, install_dir);
        return Err(error.into());
    }
    if let Err(error) = fs::rename(staged, install_dir) {
        put_back(&moved, staged, &old_dir);
        let _ = fs::rename(&old_dir, install_dir);
        return Err(error.into());
    }

    let _ = fs::remove_dir_all(&old_dir);
    Ok(())
}

/// Moves entries back after a failed install step; the original error is reported.
fn put_back(moved: &[&str], from: &Path, to: &Path) {
    for entry in moved {
        let _ = fs::rename(from.join(entry), to.join(entry));
    }
}
