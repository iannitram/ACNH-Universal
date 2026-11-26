// SPDX-License-Identifier: MIT
// Copyright (c) 2026 iannitram
//! File-tree helpers shared by the game packers and the Mac assembler: read-only and writable
//! trees, a streaming SHA-256, the git work tree check and a filtered tree copy.

use crate::error::Result;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Remove the write bits of `dir` and everything under it.
pub(crate) fn make_read_only_recursive(dir: &Path) -> Result<()> {
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

pub(crate) fn set_read_only(path: &Path) -> Result<()> {
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

/// Make `dir` writable, then delete it. A missing folder is not an error.
pub(crate) fn remove_tree_writable(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    make_writable_recursive(dir)?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

pub(crate) fn make_writable_recursive(dir: &Path) -> Result<()> {
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

pub(crate) fn set_writable(path: &Path) -> Result<()> {
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

/// Size and lowercase hex SHA-256 of a file, read in 64 KiB blocks.
pub(crate) fn size_and_sha256(path: &Path) -> Result<(u64, String)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size = size.saturating_add(n as u64);
        hasher.update(&buf[..n]);
    }
    Ok((size, format!("{:x}", hasher.finalize())))
}

/// The work tree `path` is in, if any: the nearest folder above it that holds `.git`.
pub(crate) fn git_work_tree(path: &Path) -> Option<PathBuf> {
    let path = std::path::absolute(path).ok()?;
    path.ancestors()
        .find(|folder| folder.join(".git").exists())
        .map(Path::to_path_buf)
}

/// Copy the files of `from` into `to` (created if needed), skipping each folder `skip` names.
pub(crate) fn copy_tree_skipping(
    from: &Path,
    to: &Path,
    skip: &dyn Fn(&Path) -> bool,
) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            if skip(&path) {
                continue;
            }
            copy_tree_skipping(&path, &target, skip)?;
        } else {
            fs::copy(&path, &target)?;
        }
    }
    Ok(())
}
