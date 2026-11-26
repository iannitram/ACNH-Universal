//! Publishing a finished build on a hub (`builds/<build id>/bundle.txt` beside its files, the
//! layout `serve` reads) and the stable staging folder a pull resumes in.
use super::*;

/// How many published builds of one target stay on the hub: the newest and the one before it.
const KEEP_BUILDS: usize = 2;

fn copy_files(from: &Path, to: &Path, b: &Bundle) -> Result<()> {
    for file in &b.files {
        let target = file_path(to, &file.path, true)?;
        fs::copy(from.join(&file.path), target)?;
    }
    Ok(())
}

fn is_build_name(name: &str) -> bool {
    name.len() == 16
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Publishes the build in `install` for `target` under `builds/<build id>/`: every bundled file
/// is copied there and `bundle.txt` is written last, so a hub serving at the same moment never
/// sees a half-published build. The device-owned entries (`root`, `logs`) and anything forbidden
/// never go in (`bundle::manifest`). Publishing the same files again only refreshes the
/// bundle text so it stays the newest. Older published builds of the same target beyond the
/// newest two are removed. Returns the published bundle.
pub fn publish_build(
    builds: &Path,
    install: &Path,
    target: Os,
    version: &str,
    commit: &str,
) -> Result<Bundle> {
    let b = bundle::manifest(install, target, version, commit)?;
    if b.files.is_empty() {
        return Err(hub_error("there is nothing to publish"));
    }
    let name = b.build_id.clone();
    fs::create_dir_all(builds)?;
    let dest = builds.join(&name);
    let text = bundle::to_text(&b);
    if dest.join("bundle.txt").is_file() && verify_streaming(&b, &dest).is_ok() {
        crate::platform::atomic_write(&dest.join("bundle.txt"), text.as_bytes())?;
    } else {
        let temp = builds.join(format!(".publishing-{name}"));
        if temp.exists() {
            fs::remove_dir_all(&temp)?;
        }
        if dest.exists() {
            fs::remove_dir_all(&dest)?;
        }
        fs::create_dir(&temp)?;
        let made = copy_files(install, &temp, &b)
            .and_then(|()| fs::write(temp.join("bundle.txt"), text.as_bytes()).map_err(Into::into))
            .and_then(|()| fs::rename(&temp, &dest).map_err(Into::into));
        if let Err(error) = made {
            let _ = fs::remove_dir_all(&temp);
            return Err(error);
        }
    }
    prune_builds(builds, target, &name);
    Ok(b)
}

// Best effort: the newest builds of this target stay, and so does `keep_name`. Only folders
// this module names (16 hex digits) are ever removed.
fn prune_builds(builds: &Path, target: Os, keep_name: &str) {
    let Ok(entries) = fs::read_dir(builds) else {
        return;
    };
    let mut ours = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let manifest = entry.path().join("bundle.txt");
        let Ok(meta) = fs::symlink_metadata(&manifest) else {
            continue;
        };
        if !is_build_name(&name) || !meta.is_file() {
            continue;
        }
        let Ok(text) = read_manifest(&manifest) else {
            continue;
        };
        if bundle::from_text(&text).is_ok_and(|b| b.target == target) {
            if let Ok(modified) = meta.modified() {
                ours.push((modified, name, entry.path()));
            }
        }
    }
    ours.sort();
    let surplus = ours.len().saturating_sub(KEEP_BUILDS);
    for (_, name, path) in ours.into_iter().take(surplus) {
        if name != keep_name {
            let _ = fs::remove_dir_all(path);
        }
    }
}

/// The staging folder a bundle pull uses: always the same one under `dirs.data`, so a pull that
/// was interrupted continues from the chunks it already has. Hand it to `pull_hub` and, once
/// the pull succeeded, to `bundle::install`, which moves it away; a failed pull leaves it for
/// the next try. It is created if missing.
pub fn resume_dir(dirs: &AppDirs) -> Result<PathBuf> {
    let dir = dirs.data.join(".hub-pull");
    fs::create_dir_all(&dirs.data)?;
    make_dir(&dir)?;
    Ok(dir)
}
