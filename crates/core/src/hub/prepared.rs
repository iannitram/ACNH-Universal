//! Prepared game data (Android, Linux, Windows) over a separate paired protocol.
use super::*;
use serde::{Deserialize, Serialize};

const DATA_WIRE: &[u8; 8] = b"ACNHDAT1";
const MAX_FILES: usize = 100_000;

/// What a hub serves for one target: the manifest's target name, the files at the top of the
/// tree (always sent) and every file the pulling device must receive for the game to start.
struct Layout {
    name: &'static str,
    root_files: &'static [&'static str],
    required: &'static [&'static str],
}

/// Android: the prepared tree is the app's `game/` folder, with the game library in it.
const ANDROID: Layout = Layout {
    name: "android",
    root_files: &["libacnh_game.so", "romfs.pak", "spirv.pak"],
    required: &["libacnh_game.so", "romfs.pak", "spirv.pak"],
};

/// Linux: the tree is the contained `game/` folder beside the player (what
/// `NX_GAME_ROOT` points at); the player and its libraries come in the ordinary bundle.
const LINUX: Layout = Layout {
    name: "linux",
    root_files: &["romfs.pak", "spirv.pak"],
    required: &["analysis/main.bin", "romfs.pak", "spirv.pak"],
};

/// Windows: the same contained `game/` folder as Linux.
const WINDOWS: Layout = Layout {
    name: "windows",
    ..LINUX
};

fn layout(target: Os) -> Result<&'static Layout> {
    match target {
        Os::Android => Ok(&ANDROID),
        Os::Linux => Ok(&LINUX),
        Os::Windows => Ok(&WINDOWS),
        _ => Err(hub_error(
            "prepared data is only available for Android, Linux and Windows",
        )),
    }
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    target: String,
    files: Vec<Entry>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    path: String,
    size: u64,
    hash: String,
}

// Accept the prepared layout of `layout` and the RomFS overlays produced by Prepare.
fn allowed(path: &str, layout: &Layout) -> bool {
    let parts: Vec<_> = path.split('/').collect();
    if parts.iter().any(|p| {
        p.is_empty()
            || *p == "."
            || *p == ".."
            || p.ends_with(['.', ' '])
            || p.chars()
                .any(|c| c.is_control() || "\\:<>\"|?*".contains(c))
    }) {
        return false;
    }
    match parts.as_slice() {
        [name] => layout.root_files.contains(name),
        ["analysis" | "aoc", rest @ ..] if !rest.is_empty() => {
            rest.iter().all(|component| overlay_component(component))
        }
        ["fonts", file] => {
            *file == "shared.ttf" || (0..=5).any(|n| *file == format!("shared_{n}.ttf"))
        }
        ["fonts", "glyphs_xbox" | "glyphs_playstation" | "glyphs_switch", "console" | "xbox" | "nintendo", "romfs", "Font", "ScalableFont.sarc.zs"] => {
            true
        }
        ["mods", name, "romfs", rest @ ..] if !rest.is_empty() => std::iter::once(name)
            .chain(rest)
            .all(|component| overlay_component(component)),
        _ => false,
    }
}

fn overlay_component(component: &str) -> bool {
    let lower = component.to_ascii_lowercase();
    let mut segments = lower.split('.');
    let stem = segments.next().unwrap_or_default();
    !matches!(
        stem,
        "" | "analysis"
            | "aoc"
            | "backups"
            | "device_save"
            | "exefs"
            | "firmware"
            | "gen"
            | "keys"
            | "logs"
            | "user"
            | "con"
            | "prn"
            | "aux"
            | "nul"
    ) && !(stem.len() == 4
        && matches!(&stem.as_bytes()[..3], b"com" | b"lpt")
        && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        && !matches!(
            lower.as_str(),
            "config.ini" | "launcher.ini" | "main.dat" | "mainheader.dat"
        )
        && !segments.any(|ext| {
            matches!(
                ext,
                "keys"
                    | "key"
                    | "pem"
                    | "p12"
                    | "nsp"
                    | "xci"
                    | "nca"
                    | "nsz"
                    | "xcz"
                    | "nso"
                    | "sav"
                    | "log"
            )
        })
}

fn validate(manifest: &Manifest, layout: &Layout) -> Result<()> {
    if manifest.target != layout.name
        || manifest.files.is_empty()
        || manifest.files.len() > MAX_FILES
    {
        return Err(hub_error("invalid prepared data manifest"));
    }
    let mut paths = BTreeSet::new();
    for file in &manifest.files {
        if !allowed(&file.path, layout)
            || file.hash.len() != 64
            || !file
                .hash
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            || !paths.insert(file.path.to_ascii_lowercase())
        {
            return Err(hub_error("invalid prepared data manifest"));
        }
    }
    for name in &paths {
        for (at, _) in name.match_indices('/') {
            if paths.contains(&name[..at]) {
                return Err(hub_error("overlapping prepared paths"));
            }
        }
    }
    for &required in layout.required {
        if !manifest
            .files
            .iter()
            .any(|file| file.path == required && file.size > 0)
        {
            return Err(hub_error("prepared data is incomplete"));
        }
    }
    Ok(())
}

fn trusted(dirs: &AppDirs, device: &str) -> Result<Peer> {
    read_peers(dirs)?
        .into_iter()
        .find(|p| p.device == device)
        .ok_or_else(|| hub_error("this device is not paired"))
}

fn target_android(target: Os) -> Result<()> {
    if target == Os::Android {
        Ok(())
    } else {
        Err(hub_error("prepared data is only available for Android"))
    }
}

fn hash_file(path: &Path) -> Result<(u64, String)> {
    crate::fsutil::size_and_sha256(path)
}

// Walk only recognized roots, and reject symlinks rather than follow them.
fn collect(root: &Path, relative: &str, layout: &Layout, files: &mut Vec<Entry>) -> Result<()> {
    let dir = root.join(relative);
    match fs::symlink_metadata(&dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
        Ok(m) if !m.is_dir() || m.file_type().is_symlink() => {
            return Err(hub_error("prepared folder is not a plain directory"))
        }
        Ok(_) => {}
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| hub_error("invalid prepared path"))?;
        let path = format!("{relative}/{name}");
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(hub_error("prepared path is a symlink"));
        }
        if kind.is_dir() {
            collect(root, &path, layout, files)?;
        } else if kind.is_file() && allowed(&path, layout) {
            let (size, hash) = hash_file(&file_path(root, &path, false)?)?;
            files.push(Entry { path, size, hash });
        }
    }
    Ok(())
}

fn make_manifest(root: &Path, layout: &Layout) -> Result<Manifest> {
    require_dir(root)?;
    let mut files = Vec::new();
    for &name in layout.root_files {
        let (size, hash) = hash_file(&file_path(root, name, false)?)?;
        files.push(Entry {
            path: name.into(),
            size,
            hash,
        });
    }
    for dir in ["analysis", "aoc", "fonts", "mods"] {
        collect(root, dir, layout, &mut files)?;
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let result = Manifest {
        target: layout.name.into(),
        files,
    };
    validate(&result, layout)?;
    Ok(result)
}

/// Serve prepared data for `target` (Android, Linux or Windows) to a locally remembered peer
/// over a sealed channel. `root` contains the target's packs, analysis and optional assets
/// (and, for Android, the game library). The caller supplies a
/// home-network transport. Success means the client received and verified every file.
pub fn serve_prepared(
    t: &mut dyn Transport,
    dirs: &AppDirs,
    device: &str,
    target: Os,
    root: &Path,
) -> Result<()> {
    (|| {
        let layout = layout(target)?;
        let peer = trusted(dirs, device)?;
        let key = pairing_key(&peer)?;
        let mut channel = handshake_for(t, &key, target, true, DATA_WIRE)?;
        let manifest = make_manifest(root, layout)?;
        let text = serde_json::to_vec(&manifest)
            .map_err(|_| hub_error("invalid prepared data manifest"))?;
        if text.len() > MAX_MANIFEST {
            return Err(hub_error("prepared data manifest too large"));
        }
        let digest = Sha256::digest(&text);
        channel.send(t, b"prepared-manifest/v1", text)?;
        for (i, entry) in manifest.files.iter().enumerate() {
            let mut source = File::open(file_path(root, &entry.path, false)?)?;
            let mut remaining = entry.size;
            let mut index = 0u64;
            while remaining > 0 {
                let len = remaining.min(CHUNK_SIZE as u64) as usize;
                let mut bytes = vec![0; len];
                source.read_exact(&mut bytes)?;
                let mut aad = digest.to_vec();
                aad.extend_from_slice(&(i as u64).to_be_bytes());
                aad.extend_from_slice(&index.to_be_bytes());
                channel.send(t, &aad, bytes)?;
                remaining -= len as u64;
                index += 1;
            }
        }
        if channel.receive(t, &digest, 1)? != [1] {
            return Err(hub_error("prepared transfer not confirmed"));
        }
        channel.send(t, &digest, vec![1])?;
        Ok(())
    })()
    .map_err(transfer_error)
}

fn lock_install(data_root: &Path) -> Result<File> {
    // Keep this inode between transfers so a waiting process always locks the same file.
    // Closing the handle also releases the lock if the app exits before installing.
    let path = data_root.join(".prepared-install-lock");
    let file = match crate::platform::open_private_new(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            if !fs::symlink_metadata(&path)?.is_file() {
                return Err(hub_error("prepared install lock is not a plain file"));
            }
            fs::OpenOptions::new().write(true).open(&path)?
        }
        Err(e) => return Err(e.into()),
    };
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(hub_error("prepared installation is busy")),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// Staging folders made by the pullers; a kill leaves them behind because only `Drop` removes them.
const STALE_STAGING: [&str; 4] = [".hub-pull-", ".prepared-pull-", ".continue-", ".first-run-"];

/// Remove leftover pull staging folders in `root`. Only real directories are removed (a symlink
/// is never followed), and the caller must hold the install lock so no live pull is touched.
fn sweep_staging(root: &Path) -> usize {
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !STALE_STAGING.iter().any(|prefix| name.starts_with(prefix)) {
            continue;
        }
        if entry.file_type().is_ok_and(|t| t.is_dir()) && fs::remove_dir_all(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Take the install lock for `root` and, while holding it, remove staging folders left by an
/// earlier killed pull. Keep the returned handle for the whole pull; dropping it releases the lock.
pub fn lock_install_and_sweep(root: &Path) -> Result<File> {
    let lock = lock_install(root)?;
    sweep_staging(root);
    Ok(lock)
}

struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn directory_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => Ok(true),
        Ok(_) => Err(hub_error("prepared install path is not a plain directory")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

// Called with the install lock held. The previous tree is authoritative until the staged
// tree has been renamed to game. A retry can always tell which rename had completed.
fn recover_install(dest: &Path, backup: &Path, stage: &Path) -> Result<bool> {
    let mut has_game = directory_exists(dest)?;
    if directory_exists(backup)? {
        if has_game {
            fs::remove_dir_all(backup)?;
        } else {
            fs::rename(backup, dest)?;
            has_game = true;
        }
    }
    if directory_exists(stage)? {
        fs::remove_dir_all(stage)?;
    }
    Ok(has_game)
}

fn pull_verified_to_stage(
    t: &mut dyn Transport,
    dirs: &AppDirs,
    device: &str,
    target: Os,
    stage_dir: &Path,
    free_bytes: u64,
) -> Result<()> {
    let layout = layout(target)?;
    let peer = trusted(dirs, device)?;
    let key = pairing_key(&peer)?;
    let mut channel = handshake_for(t, &key, target, false, DATA_WIRE)?;
    let text = channel.receive(t, b"prepared-manifest/v1", MAX_MANIFEST)?;
    let manifest: Manifest =
        serde_json::from_slice(&text).map_err(|_| hub_error("invalid prepared data manifest"))?;
    validate(&manifest, layout)?;
    let needed = manifest
        .files
        .iter()
        .fold(0u64, |sum, file| sum.saturating_add(file.size));
    if needed > free_bytes {
        return Err(hub_error("not enough free space for the prepared data"));
    }
    let digest = Sha256::digest(&text);
    for (i, entry) in manifest.files.iter().enumerate() {
        let path = file_path(stage_dir, &entry.path, true)?;
        let mut output = crate::platform::open_private_new(&path)?;
        let mut hash = Sha256::new();
        let mut remaining = entry.size;
        let mut index = 0u64;
        while remaining > 0 {
            let len = remaining.min(CHUNK_SIZE as u64) as usize;
            let mut aad = digest.to_vec();
            aad.extend_from_slice(&(i as u64).to_be_bytes());
            aad.extend_from_slice(&index.to_be_bytes());
            let bytes = channel.receive(t, &aad, len)?;
            if bytes.len() != len {
                return Err(hub_error("prepared chunk has wrong size"));
            }
            output.write_all(&bytes)?;
            hash.update(&bytes);
            remaining -= len as u64;
            index += 1;
        }
        output.sync_all()?;
        if format!("{:x}", hash.finalize()) != entry.hash {
            return Err(hub_error("prepared file failed verification"));
        }
    }
    channel.send(t, &digest, vec![1])?;
    if channel.receive(t, &digest, 1)? != [1] {
        return Err(hub_error("hub did not finish the prepared transfer"));
    }
    Ok(())
}

/// Receive a verified prepared Android tree into `data_root/game`, preserving its siblings
/// (including the client's saves and settings). `data_root` must be app-private storage,
/// with `game/` reserved for files managed by this transfer.
pub fn pull_prepared(
    t: &mut dyn Transport,
    dirs: &AppDirs,
    device: &str,
    target: Os,
    data_root: &Path,
) -> Result<()> {
    (|| {
        target_android(target)?;
        require_dir(data_root)?;
        let _lock = lock_install_and_sweep(data_root)?;
        let dest = data_root.join("game");
        let backup = data_root.join(".prepared-previous");
        let stage_path = data_root.join(".prepared-staging");
        let had_old = recover_install(&dest, &backup, &stage_path)?;
        fs::create_dir(&stage_path)?;
        let stage = Staging(stage_path);
        pull_verified_to_stage(t, dirs, device, target, &stage.0, u64::MAX)?;
        swap_in(&stage.0, &dest, &backup, had_old)
    })()
    .map_err(transfer_error)
}

// Move the verified tree to `dest`, keeping the old tree in `backup` until the rename is done.
fn swap_in(stage: &Path, dest: &Path, backup: &Path, had_old: bool) -> Result<()> {
    if had_old {
        fs::rename(dest, backup)?;
    }
    if let Err(e) = fs::rename(stage, dest) {
        if had_old {
            let _ = fs::rename(backup, dest);
        }
        return Err(e.into());
    }
    if had_old {
        let _ = fs::remove_dir_all(backup);
    }
    Ok(())
}

/// Receive a verified prepared tree for a Linux or Windows `target` and install it as the
/// contained `game/` folder beside the player, `<install>/game/` (where the player and the
/// first run look for `analysis/main.bin`). The tree is staged in `dirs.data`, so the swap is
/// one rename on the same disk; `free_bytes` is checked against the manifest before any file is
/// written. Saves, settings and the player are never touched. The install folder must already
/// hold the player (pull the bundle first). Returns the installed folder.
pub fn pull_prepared_desktop(
    t: &mut dyn Transport,
    dirs: &AppDirs,
    device: &str,
    target: Os,
    free_bytes: u64,
) -> Result<PathBuf> {
    (|| {
        if !matches!(target, Os::Linux | Os::Windows) {
            return Err(hub_error(
                "prepared data here is only for Linux and Windows",
            ));
        }
        require_dir(&dirs.data)?;
        let install = dirs.install();
        require_dir(&install)?;
        let _lock = lock_install_and_sweep(&dirs.data)?;
        let dest = install.join("game");
        let backup = dirs.data.join(".prepared-previous");
        let stage_path = dirs.data.join(".prepared-staging");
        let had_old = recover_install(&dest, &backup, &stage_path)?;
        fs::create_dir(&stage_path)?;
        let stage = Staging(stage_path);
        pull_verified_to_stage(t, dirs, device, target, &stage.0, free_bytes)?;
        swap_in(&stage.0, &dest, &backup, had_old)?;
        Ok(dest)
    })()
    .map_err(transfer_error)
}

/// Receive a verified prepared Android tree into a staging folder inside `private_dir`
/// and install it into `<private_dir>/game/` through [`crate::appgame::install`].
pub fn pull_prepared_app(
    t: &mut dyn Transport,
    dirs: &AppDirs,
    device: &str,
    target: Os,
    private_dir: &Path,
    free_bytes: u64,
) -> Result<PathBuf> {
    let ci = std::env::var_os("CI").is_some_and(|v| !v.is_empty())
        || std::env::var_os("GITHUB_ACTIONS").is_some_and(|v| !v.is_empty());
    pull_prepared_app_with(t, dirs, device, target, private_dir, free_bytes, ci)
}

/// Like [`pull_prepared_app`], with an explicit `ci` flag for deterministic host tests.
pub fn pull_prepared_app_with(
    t: &mut dyn Transport,
    dirs: &AppDirs,
    device: &str,
    target: Os,
    private_dir: &Path,
    free_bytes: u64,
    ci: bool,
) -> Result<PathBuf> {
    target_android(target)?;
    require_dir(private_dir)?;
    let _lock = lock_install_and_sweep(private_dir)?;
    let staged = tempfile::Builder::new()
        .prefix(".prepared-pull-")
        .tempdir_in(private_dir)?;
    pull_verified_to_stage(t, dirs, device, target, staged.path(), u64::MAX)
        .map_err(transfer_error)?;
    let dest = private_dir.join(crate::appgame::GAME_DIR);
    let mut copy = |from: &Path, to: &Path| -> Result<()> {
        fs::copy(from, to)?;
        Ok(())
    };
    crate::appgame::install_with(staged.path(), private_dir, &dest, free_bytes, ci, &mut copy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_rejects_duplicates_wrong_target_empty_packs_and_bad_hashes() {
        let entry = |path: &str| Entry {
            path: path.into(),
            size: 1,
            hash: "0".repeat(64),
        };
        let mut manifest = Manifest {
            target: "android".into(),
            files: vec![
                entry("libacnh_game.so"),
                entry("romfs.pak"),
                entry("spirv.pak"),
            ],
        };
        assert!(validate(&manifest, &ANDROID).is_ok());
        manifest.files.push(entry("romfs.pak"));
        assert!(validate(&manifest, &ANDROID).is_err());
        manifest.files.pop();
        manifest.target = "linux".into();
        assert!(validate(&manifest, &ANDROID).is_err());
        manifest.target = "android".into();
        manifest.files[1].size = 0;
        assert!(validate(&manifest, &ANDROID).is_err());
        manifest.files[1].size = 1;
        manifest.files[0].hash = "bad".into();
        assert!(validate(&manifest, &ANDROID).is_err());
    }

    #[test]
    fn desktop_manifest_needs_its_own_target_and_required_files() {
        let entry = |path: &str| Entry {
            path: path.into(),
            size: 1,
            hash: "0".repeat(64),
        };
        let files = || {
            vec![
                entry("analysis/main.bin"),
                entry("romfs.pak"),
                entry("spirv.pak"),
            ]
        };
        for (target, layout, other) in [("linux", &LINUX, &WINDOWS), ("windows", &WINDOWS, &LINUX)]
        {
            let mut manifest = Manifest {
                target: target.into(),
                files: files(),
            };
            assert!(validate(&manifest, layout).is_ok(), "{target}");
            // Another target's manifest, or an Android one, is never accepted.
            assert!(validate(&manifest, other).is_err(), "{target}");
            assert!(validate(&manifest, &ANDROID).is_err(), "{target}");
            // Missing the analysis the game starts from.
            manifest.files.remove(0);
            assert!(validate(&manifest, layout).is_err(), "{target}");
            // The Android game library is not part of a desktop tree.
            let mut manifest = Manifest {
                target: target.into(),
                files: files(),
            };
            manifest.files.push(entry("libacnh_game.so"));
            assert!(validate(&manifest, layout).is_err(), "{target}");
        }
        assert!(layout(Os::Linux).is_ok() && layout(Os::Windows).is_ok());
        assert!(layout(Os::Mac).is_err() && layout(Os::Ios).is_err());
    }

    #[test]
    fn manifest_rejects_unsafe_and_sensitive_paths() {
        for path in [
            "../romfs.pak",
            "mods/ptbr/romfs/../key.pack",
            "mods/ptbr/romfs/secret.keys",
            "mods/ptbr/romfs/secret.keys.zs",
            "mods/ptbr/romfs/user/device_save/test.pack",
            "mods/ptbr/romfs/con.pack",
            "mods/ptbr/romfs/config.ini",
            "fonts/../romfs.pak",
            "analysis/../secret.keys",
            "aoc/secret.keys",
            "mods/ptbr/romfs/analysis/main.bin",
            "user/device_save",
            "exefs/main",
        ] {
            assert!(!allowed(path, &ANDROID), "{path}");
            assert!(!allowed(path, &LINUX), "{path}");
        }
        assert!(allowed("analysis/main.bin", &ANDROID));
        assert!(allowed("analysis/main.bin", &LINUX));
        assert!(allowed("aoc/1/data", &ANDROID));
        assert!(allowed("aoc/1/data", &LINUX));
        assert!(allowed("fonts/shared.ttf", &ANDROID));
        assert!(allowed("fonts/shared.ttf", &LINUX));
        assert!(allowed("mods/custom/romfs/Layout/test.bflyt", &ANDROID));
        assert!(allowed("mods/custom/romfs/Layout/test.bflyt", &LINUX));
        assert!(allowed(
            "fonts/glyphs_xbox/console/romfs/Font/ScalableFont.sarc.zs",
            &ANDROID
        ));
        assert!(!allowed(
            "fonts/glyphs_xbox/console/romfs/Font/other.sarc.zs",
            &ANDROID
        ));
        assert!(allowed("romfs.pak", &WINDOWS) && !allowed("libacnh_game.so", &WINDOWS));
    }

    #[test]
    fn sweep_removes_stale_staging_only_and_needs_the_lock() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            ".hub-pull-a",
            ".prepared-pull-b",
            ".continue-c",
            ".first-run-d",
        ] {
            fs::create_dir(root.path().join(name)).unwrap();
            fs::write(root.path().join(name).join("part"), b"x").unwrap();
        }
        fs::create_dir(root.path().join("game")).unwrap();
        fs::write(root.path().join(".hub-pull-file"), b"keep").unwrap();
        let held = lock_install_and_sweep(root.path()).unwrap();
        assert!(lock_install_and_sweep(root.path()).is_err());
        for name in [
            ".hub-pull-a",
            ".prepared-pull-b",
            ".continue-c",
            ".first-run-d",
        ] {
            assert!(!root.path().join(name).exists(), "{name}");
        }
        assert!(root.path().join("game").is_dir());
        assert!(root.path().join(".hub-pull-file").is_file());
        drop(held);
    }

    #[test]
    fn install_recovers_both_sides_of_an_interrupted_swap() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("game");
        let backup = root.path().join(".prepared-previous");
        let stage = root.path().join(".prepared-staging");
        let lock = lock_install(root.path()).unwrap();
        assert!(lock_install(root.path()).is_err());

        fs::create_dir(&backup).unwrap();
        fs::write(backup.join("old"), b"old tree").unwrap();
        fs::create_dir(&stage).unwrap();
        assert!(recover_install(&dest, &backup, &stage).unwrap());
        assert_eq!(fs::read(dest.join("old")).unwrap(), b"old tree");
        assert!(!backup.exists());
        assert!(!stage.exists());

        fs::rename(&dest, &backup).unwrap();
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("new"), b"new tree").unwrap();
        drop(lock);
        let _lock = lock_install(root.path()).unwrap();
        assert!(recover_install(&dest, &backup, &stage).unwrap());
        assert_eq!(fs::read(dest.join("new")).unwrap(), b"new tree");
        assert!(!backup.exists());
    }
}
