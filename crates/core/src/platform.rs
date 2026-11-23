// SPDX-License-Identifier: MIT
// Copyright (c) 2026 iannitram
// Private-file creation adapted from iannitram/totk-universal at
// 80148e16e60b5ca5d34ca87d3ef3360567490352, crates/core/src/hub.rs:526-533,925-932;
// permission test from crates/core/tests/hub_pair.rs:119-148 at the same commit.
// See NOTICE for the MIT notice.
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
#[cfg(feature = "hub")]
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
