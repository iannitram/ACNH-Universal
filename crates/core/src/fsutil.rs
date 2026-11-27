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

/// Lowercase hex SHA-256 of `bytes`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The work tree `path` is in, if any: the nearest folder above it that holds `.git` (a
/// folder, or a file for a linked work tree). It reads the disk only, so it needs no git: the
/// Mac assembler, which may run where git is not installed, asks it.
pub(crate) fn git_work_tree(path: &Path) -> Option<PathBuf> {
    let path = std::path::absolute(path).ok()?;
    path.ancestors()
        .find(|folder| folder.join(".git").exists())
        .map(Path::to_path_buf)
}

/// What asking git about a folder came to.
pub(crate) enum GitTree {
    Inside,
    Outside,
    /// Git could not answer; what it said, if anything.
    Unknown(String),
}

/// How git is run: its arguments in, whether it succeeded with what it wrote to standard
/// output and to standard error out; `None` when it cannot be started. The prepare tests
/// and the packer tests stand in for it.
pub(crate) type GitRun<'a> = &'a mut dyn FnMut(&[&str]) -> Option<(bool, String, String)>;

/// Whether `existing` (a folder that exists) is inside a git work tree. Only git's own word
/// that this is no repository counts as outside when it fails.
pub(crate) fn git_in_work_tree(existing: &Path, run: GitRun<'_>) -> GitTree {
    let folder = existing.display().to_string();
    match run(&["-C", &folder, "rev-parse", "--is-inside-work-tree"]) {
        Some((true, answer, _)) if answer.trim() == "true" => GitTree::Inside,
        Some((true, ..)) => GitTree::Outside,
        Some((false, _, error)) if error.contains("not a git repository") => GitTree::Outside,
        Some((false, _, error)) => GitTree::Unknown(error.trim().to_owned()),
        None => GitTree::Unknown(String::new()),
    }
}

/// `git_in_work_tree` with the git on this machine.
pub(crate) fn host_git_in_work_tree(existing: &Path) -> GitTree {
    git_in_work_tree(existing, &mut |args| {
        let output = std::process::Command::new("git")
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .ok()?;
        Some((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    })
}

/// The nearest folder at or above `path` that exists, as `git -C` needs one.
pub(crate) fn nearest_existing_dir(path: &Path) -> &Path {
    let mut existing = path;
    while !existing.is_dir() {
        match existing.parent() {
            Some(parent) => existing = parent,
            None => break,
        }
    }
    existing
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

/// Copy the files of `from` into `to` (created if needed), following links and leaving out
/// `.DS_Store` and anything that is neither a file nor a folder.
pub(crate) fn copy_tree_following_links(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".DS_Store" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        let meta = fs::metadata(&src_path)?;
        if meta.is_dir() {
            copy_tree_following_links(&src_path, &dst_path)?;
        } else if meta.is_file() {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}
