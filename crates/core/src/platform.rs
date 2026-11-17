//! Which OS this is, and the folders the launcher and the player use there. Values come from
//! environment variables through a lookup function, so the rules are tested on every OS
//! without touching the real environment (docs/code-map.md, `platform`). Copied from
//! iannitram/totk-universal (MIT) by its issue; the folder names change to `acnh-universal`.

use crate::error::{Error, Result};
use std::path::PathBuf;

/// A device family the launcher builds for (SPEC.md M1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Os {
    /// SteamOS on the AMD handheld and the Strix Halo tablet, and other Linux.
    Linux,
    /// The RTX 3090 Ti desktop.
    Windows,
    /// macOS on Apple silicon.
    Mac,
    /// iPhone and iPad (built on the Mac, installed by cable).
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
    /// `<config>/acnh-universal`: launcher settings.
    pub config: PathBuf,
    /// `<data>/acnh-universal`: the install, which holds the data root and the logs.
    pub data: PathBuf,
}

impl AppDirs {
    /// `config` is `<vars.config>/acnh-universal`, `data` is `<vars.data>/acnh-universal`.
    pub fn new(vars: &Vars) -> AppDirs {
        AppDirs {
            config: vars.config.join("acnh-universal"),
            data: vars.data.join("acnh-universal"),
        }
    }

    /// `<data>/install`: the player and the game library, pulled from a hub or built here.
    pub fn install(&self) -> PathBuf {
        self.data.join("install")
    }

    /// `<data>/install/root`: the runtime's data root (`NX_DATA_ROOT`: `config.ini`, `mods/`,
    /// `user/`), inside the install, which `bundle::install` keeps (`DEVICE_OWNED`).
    pub fn data_root(&self) -> PathBuf {
        self.install().join("root")
    }

    /// `<data>/install/logs`, also kept by `bundle::install`.
    pub fn logs(&self) -> PathBuf {
        self.install().join("logs")
    }
}
