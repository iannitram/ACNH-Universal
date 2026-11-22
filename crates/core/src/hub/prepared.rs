//! Prepared Android game data, separate from the public player bundle protocol.
use super::*;
use serde::{Deserialize, Serialize};

const DATA_WIRE: &[u8; 8] = b"ACNHDAT1";
const MAX_FILES: usize = 100_000;

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

// The prepared Android layout, not the dump or a general directory synchronizer.
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
        ["fonts", file] => {
            *file == "shared.ttf" || (0..=5).any(|n| *file == format!("shared_{n}.ttf"))
        }
        ["fonts", "glyphs_xbox" | "glyphs_playstation" | "glyphs_switch", "console" | "xbox" | "nintendo", "romfs", "Font", "ScalableFont.sarc.zs"] => {
            true
        }
        ["mods", "ptbr" | "speedfix60", "romfs", rest @ ..] if !rest.is_empty() => {
            let name = rest.last().unwrap().to_ascii_lowercase();
            [".pack", ".bcsv", ".msbt", ".szs", ".sarc", ".zs"]
                .iter()
                .any(|suffix| name.ends_with(suffix))
        }
        _ => false,
    }
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
    for required in ["libacnh_game.so", "romfs.pak", "spirv.pak"] {
        if !paths.contains(required) {
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
    for name in ["libacnh_game.so", "romfs.pak", "spirv.pak"] {
        let (size, hash) = hash_file(&file_path(root, name, false)?)?;
        files.push(Entry {
            path: name.into(),
            size,
            hash,
        });
    }
    for dir in ["fonts", "mods"] {
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
/// `root` is a prepared layout, never a game dump. The caller owns the home-network transport.
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
        Ok(())
    })()
    .map_err(transfer_error)
}

struct InstallLock(PathBuf);

impl Drop for InstallLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

/// Receive a verified prepared Android tree into `data_root/game`, preserving its siblings
/// (including the client's saves and settings). `data_root` must be app-private storage.
pub fn pull_prepared(
    t: &mut dyn Transport,
    dirs: &AppDirs,
    device: &str,
    target: Os,
    data_root: &Path,
) -> Result<()> {
    (|| {
        target_android(target)?;
        let peer = trusted(dirs, device)?;
        let key = pairing_key(&peer)?;
        let mut channel = handshake_for(t, &key, target, false, DATA_WIRE)?;
        let text = channel.receive(t, b"prepared-manifest/v1", MAX_MANIFEST)?;
        let manifest: Manifest = serde_json::from_slice(&text)
            .map_err(|_| hub_error("invalid prepared data manifest"))?;
        validate(&manifest)?;
        require_dir(data_root)?;
        let lock_path = data_root.join(".prepared-install-lock");
        fs::create_dir(&lock_path)
            .map_err(|_| hub_error("prepared installation is busy or needs recovery"))?;
        let _lock = InstallLock(lock_path);
        let stage = tempfile::Builder::new()
            .prefix(".prepared-")
            .tempdir_in(data_root)?;
        let digest = Sha256::digest(&text);
        for (i, entry) in manifest.files.iter().enumerate() {
            let path = file_path(stage.path(), &entry.path, true)?;
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
        let dest = data_root.join("game");
        let backup = data_root.join(".prepared-previous");
        match fs::symlink_metadata(&backup) {
            Ok(_) => return Err(hub_error("previous prepared data needs recovery")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let had_old = match fs::symlink_metadata(&dest) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => true,
            Ok(_) => return Err(hub_error("game root is not a plain directory")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        if had_old {
            fs::rename(&dest, &backup)?;
        }
        if let Err(e) = fs::rename(stage.path(), &dest) {
            if had_old {
                let _ = fs::rename(&backup, &dest);
            }
            return Err(e.into());
        }
        if had_old {
            fs::remove_dir_all(backup)?;
        }
        channel.send(t, &digest, vec![1])?;
        Ok(())
    })()
    .map_err(transfer_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_rejects_duplicates_wrong_target_and_bad_hashes() {
        let entry = |path: &str| Entry {
            path: path.into(),
            size: 0,
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
        manifest.files[0].hash = "bad".into();
        assert!(validate(&manifest).is_err());
    }

    #[test]
    fn manifest_rejects_unsafe_and_sensitive_paths() {
        for path in [
            "../romfs.pak",
            "mods/ptbr/romfs/../key.pack",
            "mods/ptbr/romfs/secret.keys",
            "fonts/../romfs.pak",
            "analysis/main.bin",
            "user/device_save",
            "exefs/main",
        ] {
            assert!(!allowed(path), "{path}");
        }
        assert!(allowed("fonts/shared.ttf"));
        assert!(allowed(
            "fonts/glyphs_xbox/console/romfs/Font/ScalableFont.sarc.zs"
        ));
        assert!(!allowed(
            "fonts/glyphs_xbox/console/romfs/Font/other.sarc.zs"
        ));
    }
}
