//! The iPhone and iPad game download's plan and checks (issue #231; SPEC.md M18), the
//! platform-free half of `appgame_ios.rs`. The player's Objective-C++ (`runtime/src/appgame_ios.mm`)
//! moves the bytes in a background `URLSession`, which a blocking `appgame::download` cannot drive;
//! this half asks `acnh_core::appgame` (the manifest, the resume rule, the hash check and the one
//! rename to `game/` that the Android download uses too) what to fetch and from which byte, checks
//! each file, and installs `game/` once all of them check out. Every answer is a JSON object the
//! C++ side reads; nothing here touches the network.
//!
//! The tests run it on every runner (`tests/ios_appgame.rs`).

use acnh_core::appgame::{self as core, Manifest, GAME_DIR, PARTIAL_DIR};
use acnh_core::error::Error;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

/// Where the prepared game waits in the owner's Dropbox: inside save-relay's app folder
/// (`Apps/save-relay/`), so the API path starts here. `tools/package_ios.sh --small` writes it.
pub const REMOTE_DIR: &str = "/acnh-universal-game/ios";

/// The manifest being downloaded, kept beside `game.partial/` so a relaunch by the system (the
/// background session finishing) needs no new request to check files and install. The player
/// writes it once the person agrees to the download, never `plan_json`: a plan made only to ask
/// "Download the game (about N GB)?" must leave nothing behind that a later launch takes for a yes.
pub const PENDING: &str = "game.manifest.pending.json";

/// The files whose hash was checked (`<path> <sha256> <size>` per line), beside `game.partial/`:
/// so a relaunch does not hash eleven gigabytes again to know what it has.
const VERIFIED: &str = "game.verified.txt";

const MANIFEST_NAME: &str = "manifest.json";

fn failure(code: &str, why: &str) -> Value {
    json!({"ok": false, "code": code, "error": why})
}

fn from_error(error: &Error) -> Value {
    let code = match error {
        Error::Unsupported(_) => "refused",
        Error::BadGameFolder(_) => "manifest",
        _ => "io",
    };
    failure(code, &error.to_string())
}

/// "11.2 GB", "340 MB": decimal units, as the first-start screen shows sizes.
fn human(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else {
        format!("{bytes} bytes")
    }
}

/// `Dropbox-API-Arg` for `path` (a name in the manifest, or `manifest.json`): ASCII JSON.
pub fn api_arg(path: &str) -> String {
    save_relay::dropbox::header_json(&json!({"path": format!("{REMOTE_DIR}/{path}")}))
}

fn verified_line(file: &core::ManifestFile) -> String {
    format!("{} {} {}", file.path, file.sha256, file.size)
}

fn verified_set(root: &Path) -> Vec<String> {
    fs::read_to_string(root.join(VERIFIED))
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

fn mark_verified(root: &Path, file: &core::ManifestFile) -> std::io::Result<()> {
    use std::io::Write;
    let mut list = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join(VERIFIED))?;
    writeln!(list, "{}", verified_line(file))
}

/// Refuse a manifest for another build or runtime, naming both sides. `abi` and `builds` (comma
/// separated) say what this app plays; empty accepts any. Builds compare in any case.
fn check_expected(manifest: &Manifest, abi: &str, builds: &str) -> Result<(), Error> {
    let builds: Vec<&str> = builds
        .split(',')
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .collect();
    let abi = abi.trim();
    if !builds.is_empty()
        && !builds
            .iter()
            .any(|b| b.eq_ignore_ascii_case(&manifest.build_id))
    {
        return Err(Error::Unsupported(format!(
            "The game in Dropbox is for another version of this app (game build {}, runtime {}); \
             this app needs game build {} and runtime {}. Update the app or prepare the game again.",
            manifest.build_id,
            manifest.nx_game_abi,
            builds.join(", "),
            if abi.is_empty() { "any" } else { abi },
        )));
    }
    let abi = if abi.is_empty() {
        &manifest.nx_game_abi
    } else {
        abi
    };
    // The build was matched above (in any case); `validate` also checks the ABI, every path and
    // every hash.
    manifest.validate(&manifest.build_id, abi)
}

fn parse(manifest: &[u8], abi: &str, builds: &str) -> Result<Manifest, Error> {
    let parsed = Manifest::from_json(manifest)?;
    check_expected(&parsed, abi, builds)?;
    Ok(parsed)
}

fn installed_manifest(root: &Path, manifest: &[u8]) -> bool {
    fs::read(root.join(GAME_DIR).join(MANIFEST_NAME))
        .map(|have| have == manifest)
        .unwrap_or(false)
}

/// What is left to fetch. `abi` and `builds` (comma separated) say what this app plays; empty
/// accepts any. `free_bytes` is the device's free space for this app.
///
/// `{"ok":true,"installed":true}` when `game/` already is this game; else `{"ok":true,
/// "installed":false,"total":N,"remaining":M,"fetch":[{"path","size","offset"}]}`; else
/// `{"ok":false,"code","error"}` (a refused build, too little room, an unusable manifest). Writes
/// nothing but the (empty) folder `root` and `game.partial/`.
pub fn plan_json(root: &Path, manifest: &[u8], abi: &str, builds: &str, free_bytes: u64) -> Value {
    let run = || -> Result<Value, Error> {
        let parsed = parse(manifest, abi, builds)?;
        if installed_manifest(root, manifest) {
            return Ok(json!({"ok": true, "installed": true}));
        }
        fs::create_dir_all(root)?;
        let checked = verified_set(root);
        let mut fetch = Vec::new();
        let mut total = 0u64;
        let mut remaining = 0u64;
        for file in &parsed.files {
            total = total.saturating_add(file.size);
            let have = if checked.contains(&verified_line(file))
                && fs::metadata(root.join(PARTIAL_DIR).join(&file.path))
                    .map(|m| m.is_file() && m.len() == file.size)
                    .unwrap_or(false)
            {
                file.size
            } else {
                core::partial_bytes(root, file)?
            };
            if file.size == 0 {
                // An empty file has nothing to fetch: make it, and it is checked like any other.
                let local = root.join(PARTIAL_DIR).join(&file.path);
                if let Some(parent) = local.parent() {
                    fs::create_dir_all(parent)?;
                }
                if !local.is_file() {
                    fs::File::create(&local)?;
                }
            }
            if have < file.size {
                remaining = remaining.saturating_add(file.size - have);
                fetch.push(json!({"path": file.path, "size": file.size, "offset": have}));
            } else if !checked.contains(&verified_line(file)) {
                mark_verified(root, file)?;
            }
        }
        if remaining > free_bytes {
            return Ok(failure(
                "no_space",
                &format!(
                    "Not enough room: the game needs {} more and {} is free.",
                    human(remaining),
                    human(free_bytes)
                ),
            ));
        }
        Ok(json!({
            "ok": true,
            "installed": false,
            "total": total,
            "remaining": remaining,
            "fetch": fetch,
        }))
    };
    run().unwrap_or_else(|e| from_error(&e))
}

fn pending(root: &Path) -> Result<(Vec<u8>, Manifest), Error> {
    let bytes =
        fs::read(root.join(PENDING)).map_err(|_| Error::Io("no download is waiting".into()))?;
    let manifest = Manifest::from_json(&bytes)?;
    Ok((bytes, manifest))
}

/// A file the session finished: `{"ok":true,"verified":true}`, or `verified:false` after a wrong
/// hash (the file is deleted, so the next plan fetches it again from 0).
pub fn done_json(root: &Path, path: &str) -> Value {
    let run = || -> Result<Value, Error> {
        let (_, manifest) = pending(root)?;
        let file = manifest
            .files
            .iter()
            .find(|f| f.path == path)
            .ok_or_else(|| Error::BadGameFolder(format!("{path} is not in the file list")))?;
        let right = core::verify_partial(root, file)?;
        if right {
            mark_verified(root, file)?;
        }
        Ok(json!({"ok": true, "verified": right}))
    };
    run().unwrap_or_else(|e| from_error(&e))
}

/// Every file checked: `game/` replaces `game.partial/` in one rename, read-only.
pub fn finish_json(root: &Path) -> Value {
    let run = || -> Result<Value, Error> {
        let (bytes, manifest) = pending(root)?;
        let checked = verified_set(root);
        if let Some(file) = manifest
            .files
            .iter()
            .find(|f| !checked.contains(&verified_line(f)))
        {
            return Err(Error::Io(format!(
                "{} is not checked: the game was not installed",
                file.path
            )));
        }
        core::install_partial(root, &root.join(GAME_DIR), &bytes)?;
        let _ = fs::remove_file(root.join(PENDING));
        let _ = fs::remove_file(root.join(VERIFIED));
        Ok(json!({"ok": true}))
    };
    run().unwrap_or_else(|e| from_error(&e))
}

/// True when `game/` holds a finished download (its `manifest.json`).
pub fn installed(root: &Path) -> bool {
    root.join(GAME_DIR).join(MANIFEST_NAME).is_file()
}
