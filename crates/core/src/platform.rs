//! Which OS this is, and the folders the launcher and the player use there. Values come from
//! environment variables through a lookup function, so the rules are tested on every OS
//! without touching the real environment (docs/code-map.md, `platform`). Copied from
//! iannitram/totk-universal (MIT) by its issue; the folder names change to `acnh-universal`.

use crate::error::{not_built, Result};
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
        not_built!("platform::Os::current")
    }

    /// "linux", "windows", "mac", "ios", "android".
    pub fn key(self) -> &'static str {
        not_built!("platform::Os::key")
    }

    /// The `Os` whose `key()` is `key` exactly, or `None`.
    pub fn from_key(key: &str) -> Option<Os> {
        not_built!("platform::Os::from_key")
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
        not_built("platform::Vars::from_lookup")
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
        not_built!("platform::AppDirs::new")
    }

    /// `<data>/install`: the player and the game library, pulled from a hub or built here.
    pub fn install(&self) -> PathBuf {
        not_built!("platform::AppDirs::install")
    }

    /// `<data>/install/root`: the runtime's data root (`NX_DATA_ROOT`: `config.ini`, `mods/`,
    /// `user/`), inside the install, which `bundle::install` keeps (`DEVICE_OWNED`).
    pub fn data_root(&self) -> PathBuf {
        not_built!("platform::AppDirs::data_root")
    }

    /// `<data>/install/logs`, also kept by `bundle::install`.
    pub fn logs(&self) -> PathBuf {
        not_built!("platform::AppDirs::logs")
    }
}
