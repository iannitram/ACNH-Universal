//! Build bundles (SPEC.md M11; docs/adr/0004-build-hubs.md): the player a hub built, as a
//! list of files with their hashes, which a hub serves and a client checks before it installs.
//! Shape from iannitram/totk-universal's `bundle` (MIT).
//!
//! A bundle never holds game data: not the game folder, not what is made from it (the game
//! library, `romfs.pak`, `spirv.pak`, `gen/`), not a save. `is_forbidden` is checked for every
//! file. The game data a prepare made goes to my own devices separately, over the paired home
//! network connection only (`hub`, the game-data transfer), never in a bundle.

use crate::error::{not_built, Error, Result};
use crate::platform::Os;
use std::path::Path;

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
    not_built("bundle::manifest")
}

/// Checks every file of `b` in `dir` for its size and hash, in order; the first that is
/// missing or differs is `Error::Hub("<path> does not match the bundle")`. Files in `dir`
/// that the bundle does not list are ignored.
pub fn verify(b: &Bundle, dir: &Path) -> Result<()> {
    not_built("bundle::verify")
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
    not_built("bundle::install")
}
