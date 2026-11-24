//! The in-app game download, platform-free (issues #230 and #231; SPEC.md M18): the manifest the
//! prepared game's files are listed in, the resume rule, the hash check, the free-space check, and
//! the staging into `game.partial/` with one rename to `game/` once every file checks out.
//!
//! `manifest.json` sits beside the files in the owner's Dropbox (tools/stage_dropbox_game.sh for
//! Android, `tools/package_ios.sh --small` for iOS):
//!
//! ```json
//! {"version":1,"build_id":"<32 hex>","abi":"<NX_GAME_ABI>","files":[{"path":"romfs.pak","size":1,"sha256":"<64 hex>"}]}
//! ```
//!
//! `build_id` and `abi` are optional in the file (an older Android manifest has neither) and
//! checked against what the app plays when present. Two ways in: `download` drives a blocking
//! `Transport` through the whole job; `plan`, `verified` and `install` are its steps, for a
//! platform that moves the bytes itself (iOS: a background `URLSession`).
//!
//! Layout under the app's private folder `root`: `game/` (finished, with its `manifest.json`),
//! `game.partial/` (files so far, `verified.txt` listing the ones whose hash was checked). Nothing
//! here holds game data of its own (SPEC.md M14): names, sizes and hashes only.

use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The finished game, under `root`.
pub const GAME_DIR: &str = "game";
/// Where the files arrive, under `root`, until all of them check out.
pub const PARTIAL_DIR: &str = "game.partial";
/// The manifest's name, beside the files and inside `game/` once installed.
pub const MANIFEST_NAME: &str = "manifest.json";
/// The partial folder's list of files whose hash was checked: one `<path> <sha256> <size>` per line.
const VERIFIED: &str = "verified.txt";
const OLD_DIR: &str = "game.old";

/// What went wrong; `Display` is the plain message the first-start screen shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    /// `manifest.json` could not be read (the string says why).
    Manifest(String),
    /// The manifest is for a game this app cannot run; both sides are named.
    Refused {
        found_build: String,
        found_abi: String,
        want_builds: String,
        want_abi: String,
    },
    /// Not enough room on the device: bytes still to fetch, and bytes free.
    NoSpace { needed: u64, free: u64 },
    /// A file still has the wrong hash after being fetched again.
    BadHash(String),
    /// The transport failed (no network, Dropbox refused, ...); a retry may work.
    Transport(String),
    /// A local file operation failed.
    Io(String),
}

impl DownloadError {
    /// A short stable name, for a platform layer that passes the error across a C interface.
    pub fn code(&self) -> &'static str {
        match self {
            DownloadError::Manifest(_) => "manifest",
            DownloadError::Refused { .. } => "refused",
            DownloadError::NoSpace { .. } => "no_space",
            DownloadError::BadHash(_) => "bad_hash",
            DownloadError::Transport(_) => "transport",
            DownloadError::Io(_) => "io",
        }
    }
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DownloadError::Manifest(why) => write!(f, "The game's file list is not usable: {why}"),
            DownloadError::Refused {
                found_build,
                found_abi,
                want_builds,
                want_abi,
            } => write!(
                f,
                "The game in Dropbox is for another version of this app (game build {found_build}, \
                 runtime {found_abi}); this app needs game build {want_builds} and runtime {want_abi}. \
                 Update the app or prepare the game again."
            ),
            DownloadError::NoSpace { needed, free } => write!(
                f,
                "Not enough room: the game needs {} more and {} is free.",
                human(*needed),
                human(*free)
            ),
            DownloadError::BadHash(path) => write!(
                f,
                "{path} did not download correctly, even fetched again. Try again later."
            ),
            DownloadError::Transport(why) => write!(f, "{why}"),
            DownloadError::Io(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for DownloadError {}

impl From<std::io::Error> for DownloadError {
    fn from(e: std::io::Error) -> DownloadError {
        DownloadError::Io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, DownloadError>;

/// "11.2 GB", "340 MB": decimal units, as the first-start screen shows sizes.
pub fn human(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else {
        format!("{bytes} bytes")
    }
}

/// One file of the prepared game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestFile {
    /// Relative, `/`-separated, never `..`.
    pub path: String,
    pub size: u64,
    /// Lowercase hex SHA-256.
    pub sha256: String,
}

/// `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub version: u32,
    /// The game's `exefs/main` build id, when the manifest names one.
    pub build_id: Option<String>,
    /// The runtime ABI (`NX_GAME_ABI`) the game was prepared for, when the manifest names one.
    pub abi: Option<String>,
    pub files: Vec<ManifestFile>,
}

fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && path != MANIFEST_NAME
        && path != VERIFIED
        && !path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}

impl Manifest {
    pub fn parse(bytes: &[u8]) -> Result<Manifest> {
        let bad = |why: &str| DownloadError::Manifest(why.to_string());
        let doc: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| bad("it is not JSON"))?;
        let version = doc.get("version").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
        let text = |key: &str| {
            doc.get(key)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let list = doc
            .get("files")
            .and_then(|v| v.as_array())
            .ok_or_else(|| bad("it has no files"))?;
        let mut files = Vec::with_capacity(list.len());
        let mut seen = HashSet::new();
        for entry in list {
            let path = entry.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let size = entry.get("size").and_then(|v| v.as_u64());
            let sha = entry.get("sha256").and_then(|v| v.as_str()).unwrap_or("");
            if !safe_path(path) {
                return Err(bad("a file has a path that is not allowed"));
            }
            let size = size.ok_or_else(|| bad("a file has no size"))?;
            if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(bad("a file has no SHA-256"));
            }
            if !seen.insert(path.to_string()) {
                return Err(bad("a file is listed twice"));
            }
            files.push(ManifestFile {
                path: path.to_string(),
                size,
                sha256: sha.to_ascii_lowercase(),
            });
        }
        if files.is_empty() {
            return Err(bad("it lists no files"));
        }
        Ok(Manifest {
            version,
            build_id: text("build_id").map(|s| s.to_ascii_uppercase()),
            abi: text("abi"),
            files,
        })
    }

    /// Every file's size, added up.
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// What this app can run. An empty `build_ids` or `abi` accepts any.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Expect {
    /// The game builds (32 hex digits, any case) the app plays.
    pub build_ids: Vec<String>,
    pub abi: String,
}

impl Expect {
    /// Refuse a manifest for another build or runtime, naming both sides. A manifest that names
    /// neither (an older Android one) passes: the files are then checked only by hash.
    pub fn check(&self, manifest: &Manifest) -> Result<()> {
        let build_ok = match (&manifest.build_id, self.build_ids.is_empty()) {
            (Some(found), false) => self.build_ids.iter().any(|b| b.eq_ignore_ascii_case(found)),
            _ => true,
        };
        let abi_ok = match (&manifest.abi, self.abi.is_empty()) {
            (Some(found), false) => *found == self.abi,
            _ => true,
        };
        if build_ok && abi_ok {
            return Ok(());
        }
        let any = |s: &str| {
            if s.is_empty() {
                "any".to_string()
            } else {
                s.to_string()
            }
        };
        Err(DownloadError::Refused {
            found_build: manifest
                .build_id
                .clone()
                .unwrap_or_else(|| "unknown".into()),
            found_abi: manifest.abi.clone().unwrap_or_else(|| "unknown".into()),
            want_builds: any(&self.build_ids.join(", ")),
            want_abi: any(&self.abi),
        })
    }
}

/// Moves bytes. The core decides what to fetch and checks it; the transport only reads.
pub trait Transport {
    /// `manifest.json`, whole.
    fn manifest(&mut self) -> std::result::Result<Vec<u8>, String>;
    /// Write the bytes of `path` from `offset` to its end into `sink` (an HTTP `Range` request
    /// when `offset` is not 0). A failure partway leaves what was written: the next run resumes.
    fn fetch(
        &mut self,
        path: &str,
        offset: u64,
        sink: &mut dyn Write,
    ) -> std::result::Result<(), String>;
}

/// How one file stands before the download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    /// Bytes already in `game.partial/`; the fetch starts here (`Range: bytes=<have>-`).
    pub have: u64,
}

impl Item {
    /// Nothing left to fetch.
    pub fn complete(&self) -> bool {
        self.have == self.size
    }
}

/// What a download has still to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub items: Vec<Item>,
    pub total: u64,
}

impl Plan {
    /// Bytes still to fetch.
    pub fn remaining(&self) -> u64 {
        self.items.iter().map(|i| i.size - i.have).sum()
    }
    /// The files that need fetching, in manifest order.
    pub fn to_fetch(&self) -> impl Iterator<Item = &Item> {
        self.items.iter().filter(|i| !i.complete())
    }
}

fn file_hash(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn verified_set(partial: &Path) -> HashSet<String> {
    fs::read_to_string(partial.join(VERIFIED))
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

fn verified_line(file: &ManifestFile) -> String {
    format!("{} {} {}", file.path, file.sha256, file.size)
}

fn make_writable(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::symlink_metadata(dir) {
            if meta.file_type().is_symlink() {
                return;
            }
            let mode = meta.permissions().mode();
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(mode | 0o200));
            if meta.is_dir() {
                if let Ok(entries) = fs::read_dir(dir) {
                    for entry in entries.flatten() {
                        make_writable(&entry.path());
                    }
                }
            }
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn make_read_only(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::symlink_metadata(dir) {
            if meta.file_type().is_symlink() {
                return;
            }
            if meta.is_dir() {
                if let Ok(entries) = fs::read_dir(dir) {
                    for entry in entries.flatten() {
                        make_read_only(&entry.path());
                    }
                }
            }
            let mode = meta.permissions().mode() & !0o222;
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(mode));
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Remove a folder that may hold read-only files (a finished `game/`).
pub fn remove_all(dir: &Path) -> std::io::Result<()> {
    if dir.exists() {
        make_writable(dir);
        fs::remove_dir_all(dir)?;
    }
    Ok(())
}

/// The resume rule, applied to what `root/game.partial/` holds:
///
/// - a file of the right size whose hash was checked before, or checks out now: complete, skipped;
/// - a file of the right size with another hash: deleted, fetched again from 0;
/// - a shorter file: kept, fetched from its length on (`Range`);
/// - a longer file, or a file the manifest does not list: deleted.
pub fn plan(root: &Path, manifest: &Manifest) -> Result<Plan> {
    let partial = root.join(PARTIAL_DIR);
    fs::create_dir_all(&partial)?;
    let mut verified = verified_set(&partial);
    let listed: HashSet<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
    // Anything the manifest does not list (a leftover of another build) goes.
    sweep(&partial, &partial, &listed)?;
    let mut items = Vec::with_capacity(manifest.files.len());
    for file in &manifest.files {
        let local = partial.join(&file.path);
        let mut len = fs::metadata(&local)
            .ok()
            .filter(|m| m.is_file())
            .map(|m| m.len());
        if len.is_none() && file.size == 0 {
            // An empty file has nothing to fetch: make it, then it is checked like any other.
            if let Some(parent) = local.parent() {
                fs::create_dir_all(parent)?;
            }
            File::create(&local)?;
            len = Some(0);
        }
        let line = verified_line(file);
        let have = match len {
            None => 0,
            Some(len) if len == file.size => {
                if verified.contains(&line) || file_hash(&local)? == file.sha256 {
                    if verified.insert(line.clone()) {
                        append_line(&partial.join(VERIFIED), &line)?;
                    }
                    file.size
                } else {
                    fs::remove_file(&local)?;
                    0
                }
            }
            Some(len) if len < file.size => len,
            Some(_) => {
                fs::remove_file(&local)?;
                0
            }
        };
        items.push(Item {
            path: file.path.clone(),
            size: file.size,
            sha256: file.sha256.clone(),
            have,
        });
    }
    Ok(Plan {
        items,
        total: manifest.total_bytes(),
    })
}

fn sweep(base: &Path, dir: &Path, listed: &HashSet<&str>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path
            .strip_prefix(base)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        if entry.file_type()?.is_dir() {
            sweep(base, &path, listed)?;
            // An emptied folder is left; it costs nothing.
        } else if rel != VERIFIED && !listed.contains(rel.as_str()) {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

fn append_line(path: &Path, line: &str) -> Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

/// Refuse before any file is requested when the device lacks the room for what is left.
pub fn check_space(plan: &Plan, free_bytes: u64) -> Result<()> {
    let needed = plan.remaining();
    if needed > free_bytes {
        return Err(DownloadError::NoSpace {
            needed,
            free: free_bytes,
        });
    }
    Ok(())
}

/// Check a file the transport finished against the manifest. A wrong hash deletes the file (so
/// the next plan fetches it again from 0) and is `Ok(false)`; a right one is recorded as checked.
pub fn verified(root: &Path, file: &ManifestFile) -> Result<bool> {
    let partial = root.join(PARTIAL_DIR);
    let local = partial.join(&file.path);
    let len = fs::metadata(&local).map(|m| m.len()).unwrap_or(0);
    if len == file.size && file_hash(&local)? == file.sha256 {
        append_line(&partial.join(VERIFIED), &verified_line(file))?;
        return Ok(true);
    }
    if local.exists() {
        fs::remove_file(&local)?;
    }
    Ok(false)
}

/// Move `game.partial/` to `game/`: only when every file is listed as checked. The manifest is
/// written into it last, so a `game/` with a `manifest.json` is a finished one. An older `game/`
/// is replaced by one rename pair and then removed. `read_only` makes the files and folders
/// read-only (the app's own copy is never edited).
pub fn install(root: &Path, manifest_bytes: &[u8], read_only: bool) -> Result<()> {
    let manifest = Manifest::parse(manifest_bytes)?;
    let partial = root.join(PARTIAL_DIR);
    let checked = verified_set(&partial);
    for file in &manifest.files {
        let local = partial.join(&file.path);
        let right = fs::metadata(&local)
            .map(|m| m.len() == file.size)
            .unwrap_or(false);
        if !right || !checked.contains(&verified_line(file)) {
            return Err(DownloadError::Io(format!(
                "{} is not complete: the game was not installed",
                file.path
            )));
        }
    }
    let _ = fs::remove_file(partial.join(VERIFIED));
    fs::write(partial.join(MANIFEST_NAME), manifest_bytes)?;
    let game = root.join(GAME_DIR);
    let old = root.join(OLD_DIR);
    remove_all(&old)?;
    if game.exists() {
        fs::rename(&game, &old)?;
    }
    fs::rename(&partial, &game)?;
    if read_only {
        make_read_only(&game);
    }
    remove_all(&old)?;
    Ok(())
}

/// True when `root/game/` is a finished install of exactly this manifest.
pub fn installed(root: &Path, manifest_bytes: &[u8]) -> bool {
    fs::read(root.join(GAME_DIR).join(MANIFEST_NAME))
        .map(|have| have == manifest_bytes)
        .unwrap_or(false)
}

/// What `download` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// `game/` already held exactly this manifest's game.
    AlreadyInstalled,
    /// `game/` now holds it.
    Installed,
}

/// The whole job through a blocking transport: manifest, refusal, plan, space check, fetch what
/// is missing (resuming), check each file (once more from 0 when the hash is wrong, then refuse),
/// install. A failure leaves `game.partial/` for the next run and no new `game/`.
pub fn download(
    transport: &mut dyn Transport,
    root: &Path,
    expect: &Expect,
    free_bytes: u64,
    read_only: bool,
) -> Result<Outcome> {
    let bytes = transport.manifest().map_err(DownloadError::Transport)?;
    let manifest = Manifest::parse(&bytes)?;
    expect.check(&manifest)?;
    if installed(root, &bytes) {
        return Ok(Outcome::AlreadyInstalled);
    }
    fs::create_dir_all(root)?;
    let plan = plan(root, &manifest)?;
    check_space(&plan, free_bytes)?;
    let partial: PathBuf = root.join(PARTIAL_DIR);
    for file in &manifest.files {
        let have = plan
            .items
            .iter()
            .find(|i| i.path == file.path)
            .map(|i| i.have)
            .unwrap_or(0);
        if have == file.size {
            continue;
        }
        let mut from = have;
        for attempt in 0..2 {
            fetch_into(transport, &partial, file, from)?;
            if verified(root, file)? {
                break;
            }
            if attempt == 1 {
                return Err(DownloadError::BadHash(file.path.clone()));
            }
            from = 0;
        }
    }
    install(root, &bytes, read_only)?;
    Ok(Outcome::Installed)
}

fn fetch_into(
    transport: &mut dyn Transport,
    partial: &Path,
    file: &ManifestFile,
    offset: u64,
) -> Result<()> {
    let local = partial.join(&file.path);
    if let Some(parent) = local.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut out = if offset == 0 {
        File::create(&local)?
    } else {
        OpenOptions::new().append(true).open(&local)?
    };
    let result = transport.fetch(&file.path, offset, &mut out);
    out.flush()?;
    result.map_err(DownloadError::Transport)
}
