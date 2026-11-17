//! Build recipes and hubs (SPEC.md M1, M11; docs/adr/0004-build-hubs.md): which machine builds
//! for each device family, what the build leaves, and the stages of a prepare. The hub shape
//! is copied from iannitram/totk-universal's `recipe` (MIT); ACNH has one engine, so there is
//! no port to choose.

use crate::error::{Error, Result};
use crate::platform::Os;

/// One of the three build hubs: the only machines that hold the game folder. Every other
/// launcher pulls its build from one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hub {
    /// The Strix Halo tablet (SteamOS): both SteamOS handhelds, and Android (the NDK build
    /// runs on Linux).
    StrixHalo,
    /// The RTX 3090 Ti desktop (Windows): itself.
    Desktop,
    /// The Mac: itself, and the iPhone or iPad by cable.
    Mac,
}

impl Hub {
    /// "strix-halo", "desktop", "mac".
    pub fn key(self) -> &'static str {
        match self {
            Hub::StrixHalo => "strix-halo",
            Hub::Desktop => "desktop",
            Hub::Mac => "mac",
        }
    }
}

/// The hub that builds for `target`: Linux and Android are `StrixHalo`, Windows is `Desktop`,
/// Mac and Ios are `Mac`.
pub fn hub_for(target: Os) -> Hub {
    match target {
        Os::Linux | Os::Android => Hub::StrixHalo,
        Os::Windows => Hub::Desktop,
        Os::Mac | Os::Ios => Hub::Mac,
    }
}

/// The targets `hub` builds for, in this order: `StrixHalo` is `[Linux, Android]`, `Desktop`
/// is `[Windows]`, `Mac` is `[Mac, Ios]`. For every target t, `serves(hub_for(t))` holds t.
pub fn serves(hub: Hub) -> &'static [Os] {
    match hub {
        Hub::StrixHalo => &[Os::Linux, Os::Android],
        Hub::Desktop => &[Os::Windows],
        Hub::Mac => &[Os::Mac, Os::Ios],
    }
}

/// The hub named by `key` ("strix-halo", "desktop", "mac"), or
/// `Error::Hub("unknown hub <key>: strix-halo, desktop or mac")`.
pub fn hub_by_key(key: &str) -> Result<Hub> {
    match key {
        "strix-halo" => Ok(Hub::StrixHalo),
        "desktop" => Ok(Hub::Desktop),
        "mac" => Ok(Hub::Mac),
        _ => Err(Error::Hub(format!(
            "unknown hub {key}: strix-halo, desktop or mac"
        ))),
    }
}

/// A stage of a prepare, in upstream's order (`tools/setup_acnh.py`: "tools -> game ->
/// version -> recompile -> shaders -> fonts -> build").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    Tools,
    Game,
    Version,
    Recompile,
    Shaders,
    Fonts,
    Build,
}

/// Every `Stage`, in order.
pub const STAGES: [Stage; 7] = [
    Stage::Tools,
    Stage::Game,
    Stage::Version,
    Stage::Recompile,
    Stage::Shaders,
    Stage::Fonts,
    Stage::Build,
];

impl Stage {
    /// "tools", "game", "version", "recompile", "shaders", "fonts", "build".
    pub fn key(self) -> &'static str {
        match self {
            Stage::Tools => "tools",
            Stage::Game => "game",
            Stage::Version => "version",
            Stage::Recompile => "recompile",
            Stage::Shaders => "shaders",
            Stage::Fonts => "fonts",
            Stage::Build => "build",
        }
    }

    /// The stage a progress line names: a line `== <name>` (as `setup_acnh.py` prints, and
    /// `launcher/pipeline.cpp` reads) whose first word after `== ` is a stage key, any case;
    /// anything else is `None`.
    pub fn from_line(line: &str) -> Option<Stage> {
        let name = line.strip_prefix("== ")?.split_whitespace().next()?;
        STAGES
            .into_iter()
            .find(|stage| stage.key().eq_ignore_ascii_case(name))
    }
}

/// What a build for `target` leaves in its output folder, the player's file name:
/// Linux "acnh", Windows "acnh.exe", Mac "ACNH.app", Ios "ACNH.ipa", Android "acnh.apk".
pub fn output(target: Os) -> &'static str {
    match target {
        Os::Linux => "acnh",
        Os::Windows => "acnh.exe",
        Os::Mac => "ACNH.app",
        Os::Ios => "ACNH.ipa",
        Os::Android => "acnh.apk",
    }
}
