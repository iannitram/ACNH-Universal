//! Build recipes and hubs (SPEC.md M1, M11; docs/adr/0004-build-hubs.md): which machine builds
//! for each device family, what the build leaves, and the stages of a prepare. The hub shape
//! is copied from iannitram/totk-universal's `recipe` (MIT); ACNH has one engine, so there is
//! no port to choose.

use crate::error::{not_built, Result};
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
        not_built!("recipe::Hub::key")
    }
}

/// The hub that builds for `target`: Linux and Android are `StrixHalo`, Windows is `Desktop`,
/// Mac and Ios are `Mac`.
pub fn hub_for(target: Os) -> Hub {
    not_built!("recipe::hub_for")
}

/// The targets `hub` builds for, in this order: `StrixHalo` is `[Linux, Android]`, `Desktop`
/// is `[Windows]`, `Mac` is `[Mac, Ios]`. For every target t, `serves(hub_for(t))` holds t.
pub fn serves(hub: Hub) -> &'static [Os] {
    not_built!("recipe::serves")
}

/// The hub named by `key` ("strix-halo", "desktop", "mac"), or
/// `Error::Hub("unknown hub <key>: strix-halo, desktop or mac")`.
pub fn hub_by_key(key: &str) -> Result<Hub> {
    not_built("recipe::hub_by_key")
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
        not_built!("recipe::Stage::key")
    }

    /// The stage a progress line names: a line `== <name>` (as `setup_acnh.py` prints, and
    /// `launcher/pipeline.cpp` reads) whose first word after `== ` is a stage key, any case;
    /// anything else is `None`.
    pub fn from_line(line: &str) -> Option<Stage> {
        not_built!("recipe::Stage::from_line")
    }
}

/// What a build for `target` leaves in its output folder, the player's file name:
/// Linux "acnh", Windows "acnh.exe", Mac "ACNH.app", Ios "ACNH.ipa", Android "acnh.apk".
pub fn output(target: Os) -> &'static str {
    not_built!("recipe::output")
}
