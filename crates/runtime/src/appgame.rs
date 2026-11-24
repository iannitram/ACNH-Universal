//! The iPhone and iPad game download's plan and checks (issue #231; SPEC.md M18), the
//! platform-free half of `appgame_ios.rs`. The player's Objective-C++ (`runtime/src/appgame_ios.mm`)
//! moves the bytes in a background `URLSession`; this half, through `acnh_core::appgame::download`,
//! decides what to fetch and from which byte, checks each file, and installs `game/` once all of them
//! check out. Every answer is a JSON object the C++ side reads; nothing here touches the network.
//!
//! The tests run it on every runner (`tests/ios_appgame.rs`).

use acnh_core::appgame::download::{self, DownloadError, Expect, Manifest, MANIFEST_NAME};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

/// Where the prepared game waits in the owner's Dropbox: inside save-relay's app folder
/// (`Apps/save-relay/`), so the API path starts here. `tools/package_ios.sh --small` writes it.
pub const REMOTE_DIR: &str = "/acnh-universal-game/ios";

/// The manifest being downloaded, kept beside `game.partial/` so a relaunch by the system (the
/// background session finishing) needs no new request to check files and install.
pub const PENDING: &str = "game.manifest.pending.json";

fn failure(error: &DownloadError) -> Value {
    json!({"ok": false, "code": error.code(), "error": error.to_string()})
}

/// `Dropbox-API-Arg` for `path` (a name in the manifest, or `manifest.json`): ASCII JSON.
pub fn api_arg(path: &str) -> String {
    save_relay::dropbox::header_json(&json!({"path": format!("{REMOTE_DIR}/{path}")}))
}

/// What is left to fetch. `abi` and `builds` (comma separated) say what this app plays; empty
/// accepts any. `free_bytes` is the device's free space for this app.
///
/// `{"ok":true,"installed":true}` when `game/` already is this game; else `{"ok":true,
/// "installed":false,"total":N,"remaining":M,"fetch":[{"path","size","offset"}]}`; else
/// `{"ok":false,"code","error"}` (a refused build, too little room, an unusable manifest).
pub fn plan_json(root: &Path, manifest: &[u8], abi: &str, builds: &str, free_bytes: u64) -> Value {
    let expect = Expect {
        abi: abi.trim().to_string(),
        build_ids: builds
            .split(',')
            .map(str::trim)
            .filter(|b| !b.is_empty())
            .map(str::to_string)
            .collect(),
    };
    let run = || -> download::Result<Value> {
        let parsed = Manifest::parse(manifest)?;
        expect.check(&parsed)?;
        if download::installed(root, manifest) {
            return Ok(json!({"ok": true, "installed": true}));
        }
        fs::create_dir_all(root)?;
        let plan = download::plan(root, &parsed)?;
        download::check_space(&plan, free_bytes)?;
        fs::write(root.join(PENDING), manifest)?;
        let fetch: Vec<Value> = plan
            .to_fetch()
            .map(|i| json!({"path": i.path, "size": i.size, "offset": i.have}))
            .collect();
        Ok(json!({
            "ok": true,
            "installed": false,
            "total": plan.total,
            "remaining": plan.remaining(),
            "fetch": fetch,
        }))
    };
    run().unwrap_or_else(|e| failure(&e))
}

fn pending(root: &Path) -> download::Result<(Vec<u8>, Manifest)> {
    let bytes = fs::read(root.join(PENDING))
        .map_err(|_| DownloadError::Io("no download is waiting".into()))?;
    let manifest = Manifest::parse(&bytes)?;
    Ok((bytes, manifest))
}

/// A file the session finished: `{"ok":true,"verified":true}`, or `verified:false` after a wrong
/// hash (the file is deleted, so the next plan fetches it again from 0).
pub fn done_json(root: &Path, path: &str) -> Value {
    let run = || -> download::Result<Value> {
        let (_, manifest) = pending(root)?;
        let file = manifest
            .files
            .iter()
            .find(|f| f.path == path)
            .ok_or_else(|| DownloadError::Manifest(format!("{path} is not in the file list")))?;
        let right = download::verified(root, file)?;
        Ok(json!({"ok": true, "verified": right}))
    };
    run().unwrap_or_else(|e| failure(&e))
}

/// Every file checked: `game/` replaces `game.partial/` in one rename, read-only.
pub fn finish_json(root: &Path) -> Value {
    let run = || -> download::Result<Value> {
        let (bytes, _) = pending(root)?;
        download::install(root, &bytes, true)?;
        let _ = fs::remove_file(root.join(PENDING));
        Ok(json!({"ok": true}))
    };
    run().unwrap_or_else(|e| failure(&e))
}

/// True when `game/` holds a finished download (its `manifest.json`).
pub fn installed(root: &Path) -> bool {
    root.join(download::GAME_DIR).join(MANIFEST_NAME).is_file()
}
