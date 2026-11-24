//! The game inside an app on a phone or tablet (SPEC.md M18). `download` fetches the prepared
//! game from the owner's own Dropbox into the app's private storage, resumably, with the
//! transport passed in: iOS (issue #231) brings a background `URLSession`, Android (issue #230)
//! its own job.
//!
//! Android private-storage game installer (#192).
//!
//! Copies only the six prepared game entries (`analysis/`, `romfs.pak`, `spirv.pak`,
//! `fonts/`, `aoc/` when present, and `libacnh_game.so`) into `<private>/game.partial/`,
//! resuming over matching-size files, marks them read-only, and atomically renames
//! `game.partial/` to `<private>/game/`. Writes nowhere outside the given private folder.

pub mod download;

use crate::error::{Error, Result};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Final folder name inside the app's private storage.
pub const GAME_DIR: &str = "game";
/// Staging folder name while copying.
pub const PARTIAL_DIR: &str = "game.partial";

const REQUIRED_FILES: [&str; 4] = [
    "analysis/main.bin",
    "romfs.pak",
    "spirv.pak",
    "libacnh_game.so",
];

#[derive(Debug, Clone)]
struct CopyItem {
    src: PathBuf,
    rel: PathBuf,
    size: u64,
}

/// Install the prepared game from `source` into `dest` (inside `private_dir`), checking
/// that `free_bytes` covers the remaining bytes to copy.
pub fn install(source: &Path, private_dir: &Path, dest: &Path, free_bytes: u64) -> Result<PathBuf> {
    let ci = env_nonempty("CI") || env_nonempty("GITHUB_ACTIONS");
    let mut copy = |from: &Path, to: &Path| -> Result<()> {
        fs::copy(from, to)?;
        Ok(())
    };
    install_with(source, private_dir, dest, free_bytes, ci, &mut copy)
}

/// Install with explicit CI flag and file-copy seam for deterministic tests.
pub fn install_with(
    source: &Path,
    private_dir: &Path,
    dest: &Path,
    free_bytes: u64,
    ci: bool,
    copy_file: &mut dyn FnMut(&Path, &Path) -> Result<()>,
) -> Result<PathBuf> {
    check_dest_inside_private(private_dir, dest)?;
    let items = collect_copy_plan(source, ci)?;

    let partial = private_dir.join(PARTIAL_DIR);
    let mut needed_bytes: u64 = 0;
    for item in &items {
        let dst_file = partial.join(&item.rel);
        if !already_copied(&dst_file, item.size) {
            needed_bytes = needed_bytes.saturating_add(item.size);
        }
    }
    if free_bytes < needed_bytes {
        return Err(Error::Io(format!(
            "not enough free space: need {needed_bytes} bytes ({free_bytes} bytes available)"
        )));
    }

    fs::create_dir_all(&partial)?;
    for item in &items {
        let dst_file = partial.join(&item.rel);
        if already_copied(&dst_file, item.size) {
            continue;
        }
        if let Some(parent) = dst_file.parent() {
            fs::create_dir_all(parent)?;
            if parent.exists() {
                set_writable(parent)?;
            }
        }
        if dst_file.exists() {
            set_writable(&dst_file)?;
        }
        copy_file(&item.src, &dst_file)?;
    }

    make_read_only_recursive(&partial)?;
    if dest.exists() {
        remove_tree_writable(dest)?;
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(&partial, dest)?;
    Ok(dest.to_path_buf())
}

fn env_nonempty(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

fn check_dest_inside_private(private_dir: &Path, dest: &Path) -> Result<()> {
    let norm_private = normalize_path(private_dir)?;
    let norm_dest = normalize_path(dest)?;
    if !norm_dest.starts_with(&norm_private) || norm_dest == norm_private {
        return Err(Error::Unsupported(format!(
            "destination {} is outside private folder {}",
            dest.display(),
            private_dir.display()
        )));
    }
    if private_dir.exists() {
        let canon_private = fs::canonicalize(private_dir)?;
        let mut probe = private_dir.to_path_buf();
        if let Ok(rel) = norm_dest.strip_prefix(&norm_private) {
            for comp in rel.components() {
                probe.push(comp);
                if probe.exists() {
                    let canon_probe = fs::canonicalize(&probe)?;
                    if !canon_probe.starts_with(&canon_private) {
                        return Err(Error::Unsupported(format!(
                            "destination {} escapes private folder {}",
                            dest.display(),
                            private_dir.display()
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

fn normalize_path(path: &Path) -> Result<PathBuf> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut out = PathBuf::new();
    for comp in abs.components() {
        match comp {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return Err(Error::Unsupported(format!(
                        "invalid path {}",
                        path.display()
                    )));
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    Ok(out)
}

fn resolve_game_lib(source: &Path) -> Option<PathBuf> {
    let direct = source.join("libacnh_game.so");
    if direct.is_file() {
        return Some(direct);
    }
    let importer = source.join("game/android-arm64/libacnh_game.so");
    if importer.is_file() {
        return Some(importer);
    }
    None
}

fn collect_copy_plan(source: &Path, ci: bool) -> Result<Vec<CopyItem>> {
    if !source.is_dir() {
        return Err(Error::BadGameFolder(format!(
            "{} is not a folder",
            source.display()
        )));
    }
    for req in REQUIRED_FILES {
        let ok = if req == "libacnh_game.so" {
            resolve_game_lib(source).is_some()
        } else {
            source.join(req).is_file()
        };
        if !ok {
            return Err(Error::BadGameFolder(format!("{req} is missing")));
        }
    }

    let mut items = Vec::new();
    for dir_name in ["analysis", "fonts", "aoc"] {
        let dir = source.join(dir_name);
        if dir.is_dir() {
            collect_dir(&dir, Path::new(dir_name), &mut items)?;
        }
    }
    for file_name in ["romfs.pak", "spirv.pak"] {
        let src = source.join(file_name);
        let meta = fs::metadata(&src)?;
        items.push(CopyItem {
            src,
            rel: PathBuf::from(file_name),
            size: meta.len(),
        });
    }
    let lib_src = resolve_game_lib(source)
        .ok_or_else(|| Error::BadGameFolder("libacnh_game.so is missing".into()))?;
    let lib_meta = fs::metadata(&lib_src)?;
    items.push(CopyItem {
        src: lib_src,
        rel: PathBuf::from("libacnh_game.so"),
        size: lib_meta.len(),
    });

    items.sort_by(|a, b| a.rel.cmp(&b.rel));

    if ci {
        for item in &items {
            if item.size > 0 {
                return Err(Error::Build(format!(
                    "under CI, game file must be an empty decoy: {}",
                    item.rel.display()
                )));
            }
        }
    }

    Ok(items)
}

fn collect_dir(dir: &Path, rel_prefix: &Path, items: &mut Vec<CopyItem>) -> Result<()> {
    let mut entries = fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let rel = rel_prefix.join(entry.file_name());
        if file_type.is_dir() {
            collect_dir(&path, &rel, items)?;
        } else if file_type.is_file() {
            let meta = fs::metadata(&path)?;
            items.push(CopyItem {
                src: path,
                rel,
                size: meta.len(),
            });
        }
    }
    Ok(())
}

fn already_copied(dst: &Path, expected_size: u64) -> bool {
    fs::metadata(dst)
        .map(|m| m.is_file() && m.len() == expected_size)
        .unwrap_or(false)
}

fn make_read_only_recursive(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            make_read_only_recursive(&path)?;
        } else {
            set_read_only(&path)?;
        }
    }
    set_read_only(dir)?;
    Ok(())
}

fn set_read_only(path: &Path) -> Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(perms.mode() & !0o222);
    }
    #[cfg(not(unix))]
    {
        perms.set_readonly(true);
    }
    fs::set_permissions(path, perms)?;
    Ok(())
}

fn remove_tree_writable(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    make_writable_recursive(dir)?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

fn make_writable_recursive(dir: &Path) -> Result<()> {
    set_writable(dir)?;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            make_writable_recursive(&path)?;
        } else {
            set_writable(&path)?;
        }
    }
    Ok(())
}

fn set_writable(path: &Path) -> Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(perms.mode() | 0o200);
    }
    #[cfg(not(unix))]
    {
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
    }
    fs::set_permissions(path, perms)?;
    Ok(())
}
