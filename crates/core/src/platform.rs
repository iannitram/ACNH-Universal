// SPDX-License-Identifier: MIT
// Copyright (c) 2026 iannitram
// Private-file creation adapted from iannitram/totk-universal at
// 80148e16e60b5ca5d34ca87d3ef3360567490352, crates/core/src/hub.rs:526-533,925-932;
// permission test from crates/core/tests/hub_pair.rs:119-148 at the same commit.
// See NOTICE for the MIT notice.
// The atomic write adapted from iannitram/craft-libs crates/atomic (MIT OR Apache-2.0), commit
// f3327e14ca55e6ff65ab82e629917b38a6590e3a.
//! Which OS this is, and the folders the launcher and the player use there. Values come from
//! environment variables through a lookup function, so the rules are tested on every OS
//! without touching the real environment (docs/code-map.md, `platform`). Copied from
//! iannitram/totk-universal (MIT) by its issue; the folder names change to `acnh-universal`.

use crate::error::{Error, Result};
use std::path::PathBuf;
#[cfg(all(test, feature = "hub", unix))]
mod hub_file_tests {
    use super::*;
    use crate::hub::{pair_in, read_peers, remove_peer, MemoryTransport, PairingSession};
    use std::fs;

    struct Folders(PathBuf);

    impl Folders {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("acnh-hub-private-{}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn dirs(&self, side: &str) -> AppDirs {
            AppDirs::new(&Vars {
                home: self.0.clone(),
                data: self.0.join(side),
                config: self.0.join(side),
            })
        }
    }

    impl Drop for Folders {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn run_pair(
        a: &AppDirs,
        b: &AppDirs,
        code: &str,
    ) -> (Result<crate::hub::Peer>, Result<crate::hub::Peer>) {
        let (mut left, mut right) = MemoryTransport::pair();
        std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                pair_in(
                    &mut left,
                    &mut PairingSession::new(code).unwrap(),
                    "client",
                    a,
                )
            });
            let second = scope.spawn(|| {
                pair_in(
                    &mut right,
                    &mut PairingSession::new(code).unwrap(),
                    "hub",
                    b,
                )
            });
            (first.join().unwrap(), second.join().unwrap())
        })
    }

    #[cfg(unix)]
    #[test]
    fn peers_file_is_private_even_after_replacement_and_removal() {
        use std::os::unix::fs::PermissionsExt;

        let folders = Folders::new();
        let (a, b) = (folders.dirs("a"), folders.dirs("b"));
        for _ in 0..2 {
            let (left, right) = run_pair(&a, &b, "012345");
            left.unwrap();
            right.unwrap();
            assert_eq!(read_peers(&a).unwrap().len(), 1);
            for dirs in [&a, &b] {
                let mode = fs::metadata(dirs.config.join("peers.json"))
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o600);
            }
        }
        remove_peer(&a, "client").unwrap();
        assert_eq!(
            fs::metadata(a.config.join("peers.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

/// Create a new private file. Keep OS-specific permission rules in this module.
pub(crate) fn open_private_new(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

static TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Replace `path` with `bytes` so a crash never leaves it truncated (see [`atomic_write_with`]).
pub fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => std::path::Path::new("."),
    };
    atomic_write_with(path, dir, |f| Ok(f.write_all(bytes)?))
}

/// Stage a private temp file in `temp_dir` (the same filesystem as `path`), let `write` fill it,
/// sync it, rename it over `path` and flush the parent directory on Unix. The original stays
/// intact and the temp file is removed if anything fails. A kill mid-write leaves only the temp
/// file behind.
pub fn atomic_write_with(
    path: &std::path::Path,
    temp_dir: &std::path::Path,
    write: impl FnOnce(&mut std::fs::File) -> Result<()>,
) -> Result<()> {
    use std::sync::atomic::Ordering;
    let temp = temp_dir.join(format!(
        ".atomic-{}-{}.tmp",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = open_private_new(&temp)?;
    let result = (|| -> Result<()> {
        write(&mut file)?;
        sync_file(&file)?;
        drop(file);
        rename_retrying(&temp, path)?;
        #[cfg(unix)]
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            // Best effort: the rename has already committed.
            let _ = std::fs::File::open(parent).and_then(|d| sync_file(&d));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// `sync_all`, which is `F_FULLFSYNC` on Apple platforms. Network shares there refuse it
/// (ENOTTY, ENOTSUP); the rename is still atomic, so that refusal is not an error.
fn sync_file(file: &std::fs::File) -> std::io::Result<()> {
    match file.sync_all() {
        Err(e) if cfg!(target_vendor = "apple") && matches!(e.raw_os_error(), Some(25 | 45)) => {
            Ok(())
        }
        other => other,
    }
}

/// `rename` replaces an existing file on every OS (`MoveFileEx` with REPLACE_EXISTING on
/// Windows). Antivirus and indexers hold files briefly there, so retry sharing violations.
fn rename_retrying(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    let (mut left, mut delay) = (if cfg!(windows) { 7 } else { 0 }, 10u64);
    loop {
        match std::fs::rename(from, to) {
            Err(e)
                if left > 0
                    && (e.kind() == std::io::ErrorKind::PermissionDenied
                        || matches!(e.raw_os_error(), Some(32 | 33))) =>
            {
                left -= 1;
                std::thread::sleep(std::time::Duration::from_millis(delay));
                delay = (delay * 2).min(200);
            }
            other => return other,
        }
    }
}

/// A device family the launcher builds for (SPEC.md M1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Os {
    /// SteamOS on the AMD handheld and the Strix Halo tablet, and other Linux.
    Linux,
    /// The RTX 3090 Ti desktop.
    Windows,
    /// macOS on Apple silicon.
    Mac,
    /// iPhone and iPad. The Mac builds the IPA for installation from Dropbox with SideStore.
    Ios,
    /// Android arm64 with an Adreno GPU.
    Android,
}

/// Every `Os`, in the order above.
pub const ALL_OS: [Os; 5] = [Os::Linux, Os::Windows, Os::Mac, Os::Ios, Os::Android];

impl Os {
    /// The OS this binary was built for (`cfg!(target_os = ...)`: "windows" is Windows,
    /// "macos" Mac, "ios" Ios, "android" Android; anything else is Linux).
    pub fn current() -> Os {
        if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Mac
        } else if cfg!(target_os = "ios") {
            Os::Ios
        } else if cfg!(target_os = "android") {
            Os::Android
        } else {
            Os::Linux
        }
    }

    /// "linux", "windows", "mac", "ios", "android".
    pub fn key(self) -> &'static str {
        match self {
            Os::Linux => "linux",
            Os::Windows => "windows",
            Os::Mac => "mac",
            Os::Ios => "ios",
            Os::Android => "android",
        }
    }

    /// The `Os` whose `key()` is `key` exactly, or `None`.
    pub fn from_key(key: &str) -> Option<Os> {
        match key {
            "linux" => Some(Os::Linux),
            "windows" => Some(Os::Windows),
            "mac" => Some(Os::Mac),
            "ios" => Some(Os::Ios),
            "android" => Some(Os::Android),
            _ => None,
        }
    }
}

/// The user's folders on a desktop OS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vars {
    pub home: PathBuf,
    /// Per-user application data (XDG data home, `%LOCALAPPDATA%`, `~/Library/Application Support`).
    pub data: PathBuf,
    /// Per-user configuration (XDG config home, `%APPDATA%`, `~/Library/Application Support`).
    pub config: PathBuf,
}

impl Vars {
    /// Resolve the folders for `os` from environment variables read through `var`
    /// (pass `|k| std::env::var(k).ok()` for the real environment). An empty value counts as
    /// unset.
    ///
    /// - Linux: home `HOME`; data `XDG_DATA_HOME`, else `<home>/.local/share`; config
    ///   `XDG_CONFIG_HOME`, else `<home>/.config`.
    /// - Windows: home `USERPROFILE`; data `LOCALAPPDATA`; config `APPDATA`.
    /// - Mac: home `HOME`; data and config both `<home>/Library/Application Support`.
    /// - Ios, Android: `Error::Unsupported("the app supplies its folders on <key>")`.
    ///
    /// A missing required variable is `Error::Env("<NAME> is not set")`, first one missing
    /// in the order above.
    pub fn from_lookup(os: Os, var: impl Fn(&str) -> Option<String>) -> Result<Vars> {
        let required = |name: &str| -> Result<PathBuf> {
            var(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .ok_or_else(|| Error::Env(format!("{name} is not set")))
        };

        match os {
            Os::Linux => {
                let home = required("HOME")?;
                let data = var("XDG_DATA_HOME")
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".local/share"));
                let config = var("XDG_CONFIG_HOME")
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".config"));
                Ok(Vars { home, data, config })
            }
            Os::Windows => Ok(Vars {
                home: required("USERPROFILE")?,
                data: required("LOCALAPPDATA")?,
                config: required("APPDATA")?,
            }),
            Os::Mac => {
                let home = required("HOME")?;
                let support = home.join("Library/Application Support");
                Ok(Vars {
                    home,
                    data: support.clone(),
                    config: support,
                })
            }
            Os::Ios | Os::Android => Err(Error::Unsupported(format!(
                "the app supplies its folders on {}",
                os.key()
            ))),
        }
    }
}

/// The launcher's own folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppDirs {
    /// `<config>/acnh-universal`: launcher settings. On the Mac,
    /// `~/Library/Application Support/ACNH-Universal`, shared with `data`.
    pub config: PathBuf,
    /// `<data>/acnh-universal`: the install, which holds the data root and the logs. On the
    /// Mac, `~/Library/Application Support/ACNH-Universal`, which is also the data root.
    pub data: PathBuf,
}

/// The Mac's folder under `~/Library/Application Support`, for the data root, the settings and
/// the logs together (they stay together whether or not the volume is case-sensitive).
pub const MAC_FOLDER: &str = "ACNH-Universal";

impl AppDirs {
    /// The folders for the OS this binary runs on (`AppDirs::for_os(Os::current(), vars)`).
    pub fn new(vars: &Vars) -> AppDirs {
        AppDirs::for_os(Os::current(), vars)
    }

    /// Linux, Windows and the rest: `config` is `<vars.config>/acnh-universal`, `data` is
    /// `<vars.data>/acnh-universal`. Mac: both are `<vars.data>/ACNH-Universal`.
    pub fn for_os(os: Os, vars: &Vars) -> AppDirs {
        if os == Os::Mac {
            return AppDirs {
                config: vars.config.join(MAC_FOLDER),
                data: vars.data.join(MAC_FOLDER),
            };
        }
        AppDirs {
            config: vars.config.join("acnh-universal"),
            data: vars.data.join("acnh-universal"),
        }
    }

    /// Whether these are the Mac's one folder (`data` is named `ACNH-Universal`; the other
    /// systems' is `acnh-universal`), where the data root and the logs sit directly in `data`.
    fn contained(&self) -> bool {
        self.data.file_name() == Some(std::ffi::OsStr::new(MAC_FOLDER))
    }

    /// `<data>/install`: the player and the game library, pulled from a hub or built here.
    pub fn install(&self) -> PathBuf {
        self.data.join("install")
    }

    /// `<data>/install/root`: the runtime's data root (`NX_DATA_ROOT`: `config.ini`, `mods/`,
    /// `user/`), inside the install, which `bundle::install` keeps (`DEVICE_OWNED`). On the
    /// Mac it is `<data>` itself.
    pub fn data_root(&self) -> PathBuf {
        if self.contained() {
            return self.data.clone();
        }
        self.install().join("root")
    }

    /// `<data>/install/logs`, also kept by `bundle::install`. On the Mac, `<data>/logs`.
    pub fn logs(&self) -> PathBuf {
        if self.contained() {
            return self.data.join("logs");
        }
        self.install().join("logs")
    }
}

/// True when `path` is a symbolic link or Windows directory junction.
pub fn is_dir_link(path: &std::path::Path) -> bool {
    std::fs::symlink_metadata(path)
        .ok()
        .is_some_and(|meta| meta.file_type().is_symlink())
}

/// Removes a directory link (symbolic link or Windows junction) without touching its target.
pub fn unlink_dir(path: &std::path::Path) -> Result<()> {
    #[cfg(windows)]
    {
        if std::fs::remove_dir(path).is_ok() {
            return Ok(());
        }
    }
    std::fs::remove_file(path)?;
    Ok(())
}

/// Links `link` to `target` (a symbolic link on Unix, a directory junction on Windows).
pub fn link_dir(target: &std::path::Path, link: &std::path::Path) -> Result<()> {
    if is_dir_link(link) {
        if std::fs::canonicalize(link).ok() == std::fs::canonicalize(target).ok() {
            return Ok(());
        }
        unlink_dir(link)?;
    } else if link.is_dir() {
        std::fs::remove_dir_all(link)?;
    }
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)?;
    }
    #[cfg(windows)]
    {
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        if !status.success() {
            return Err(Error::Io(format!(
                "cannot create directory junction {} -> {}",
                link.display(),
                target.display()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod atomic_tests {
    use super::{atomic_write, atomic_write_with};
    use crate::error::Error;

    fn names(dir: &std::path::Path) -> Vec<String> {
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn creates_replaces_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        atomic_write(&path, b"one").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"one");
        atomic_write(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert_eq!(names(dir.path()), ["a.txt"]);
    }

    #[test]
    fn failing_writer_keeps_original_and_removes_temp() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        atomic_write(&path, b"keep").unwrap();
        let result = atomic_write_with(&path, dir.path(), |f| {
            f.write_all(b"half")?;
            Err(Error::Io("boom".into()))
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
        assert_eq!(names(dir.path()), ["a.txt"]);
    }

    #[test]
    fn missing_parent_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope").join("a.txt");
        assert!(atomic_write(&path, b"x").is_err());
        assert!(names(dir.path()).is_empty());
    }
}
