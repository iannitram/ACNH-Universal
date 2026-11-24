//! Prepared Android game data over a separate paired protocol.
use super::*;
use serde::{Deserialize, Serialize};

const DATA_WIRE: &[u8; 8] = b"ACNHDAT1";
const MAX_FILES: usize = 100_000;
const REQUIRED: &[&str] = &["libacnh_game.so", "romfs.pak", "spirv.pak"];

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

// Accept the prepared Android layout and the RomFS overlays produced by Prepare.
fn allowed(path: &str) -> bool {
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
        ["libacnh_game.so" | "romfs.pak" | "spirv.pak"] => true,
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

fn validate(manifest: &Manifest) -> Result<()> {
    if manifest.target != "android" || manifest.files.is_empty() || manifest.files.len() > MAX_FILES
    {
        return Err(hub_error("invalid prepared data manifest"));
    }
    let mut paths = BTreeSet::new();
    for file in &manifest.files {
        if !allowed(&file.path)
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
    for &required in REQUIRED {
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
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buf = [0; CHUNK_SIZE];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size = size
            .checked_add(n as u64)
            .ok_or_else(|| hub_error("prepared file too large"))?;
        hash.update(&buf[..n]);
    }
    Ok((size, format!("{:x}", hash.finalize())))
}

// Walk only recognized roots, and reject symlinks rather than follow them.
fn collect(root: &Path, relative: &str, files: &mut Vec<Entry>) -> Result<()> {
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
            collect(root, &path, files)?;
        } else if kind.is_file() && allowed(&path) {
            let (size, hash) = hash_file(&file_path(root, &path, false)?)?;
            files.push(Entry { path, size, hash });
        }
    }
    Ok(())
}

fn make_manifest(root: &Path) -> Result<Manifest> {
    require_dir(root)?;
    let mut files = Vec::new();
    for &name in REQUIRED {
        let (size, hash) = hash_file(&file_path(root, name, false)?)?;
        files.push(Entry {
            path: name.into(),
            size,
            hash,
        });
    }
    for dir in ["analysis", "aoc", "fonts", "mods"] {
        collect(root, dir, &mut files)?;
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let result = Manifest {
        target: "android".into(),
        files,
    };
    validate(&result)?;
    Ok(result)
}

/// Serve prepared Android data to a locally remembered peer over a sealed channel.
/// `root` contains a prepared library, packs and optional assets. The caller supplies a
/// home-network transport. Success means the client received and verified every file.
pub fn serve_prepared(
    t: &mut dyn Transport,
    dirs: &AppDirs,
    device: &str,
    target: Os,
    root: &Path,
) -> Result<()> {
    (|| {
        target_android(target)?;
        let peer = trusted(dirs, device)?;
        let key = pairing_key(&peer)?;
        let mut channel = handshake_for(t, &key, target, true, DATA_WIRE)?;
        let manifest = make_manifest(root)?;
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
) -> Result<()> {
    target_android(target)?;
    let peer = trusted(dirs, device)?;
    let key = pairing_key(&peer)?;
    let mut channel = handshake_for(t, &key, target, false, DATA_WIRE)?;
    let text = channel.receive(t, b"prepared-manifest/v1", MAX_MANIFEST)?;
    let manifest: Manifest =
        serde_json::from_slice(&text).map_err(|_| hub_error("invalid prepared data manifest"))?;
    validate(&manifest)?;
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
        let _lock = lock_install(data_root)?;
        let dest = data_root.join("game");
        let backup = data_root.join(".prepared-previous");
        let stage_path = data_root.join(".prepared-staging");
        let had_old = recover_install(&dest, &backup, &stage_path)?;
        fs::create_dir(&stage_path)?;
        let stage = Staging(stage_path);
        pull_verified_to_stage(t, dirs, device, target, &stage.0)?;
        if had_old {
            fs::rename(&dest, &backup)?;
        }
        if let Err(e) = fs::rename(&stage.0, &dest) {
            if had_old {
                let _ = fs::rename(&backup, &dest);
            }
            return Err(e.into());
        }
        if had_old {
            let _ = fs::remove_dir_all(backup);
        }
        Ok(())
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
    let _lock = lock_install(private_dir)?;
    let staged = tempfile::Builder::new()
        .prefix(".prepared-pull-")
        .tempdir_in(private_dir)?;
    pull_verified_to_stage(t, dirs, device, target, staged.path()).map_err(transfer_error)?;
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
        assert!(validate(&manifest).is_ok());
        manifest.files.push(entry("romfs.pak"));
        assert!(validate(&manifest).is_err());
        manifest.files.pop();
        manifest.target = "linux".into();
        assert!(validate(&manifest).is_err());
        manifest.target = "android".into();
        manifest.files[1].size = 0;
        assert!(validate(&manifest).is_err());
        manifest.files[1].size = 1;
        manifest.files[0].hash = "bad".into();
        assert!(validate(&manifest).is_err());
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
            assert!(!allowed(path), "{path}");
        }
        assert!(allowed("analysis/main.bin"));
        assert!(allowed("aoc/1/data"));
        assert!(allowed("fonts/shared.ttf"));
        assert!(allowed("mods/custom/romfs/Layout/test.bflyt"));
        assert!(allowed(
            "fonts/glyphs_xbox/console/romfs/Font/ScalableFont.sarc.zs"
        ));
        assert!(!allowed(
            "fonts/glyphs_xbox/console/romfs/Font/other.sarc.zs"
        ));
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
