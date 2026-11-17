//! The four versions we play and the gate that refuses the rest (SPEC.md M1). A version is
//! told by the build id of `exefs/main` (`nso::build_id`), as upstream's
//! `games/acnh/versions.txt` and `tools/acnh_version.py` do; upstream builds any version, we
//! refuse the others with a plain message (our delta, SPEC.md).

use crate::error::{Error, Result};

/// A version of the game we play.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Version {
    /// 3.0.2, the recommended one.
    V3_0_2,
    V3_0_3,
    V2_0_7,
    /// The base game with no update.
    V1_0_0,
}

/// Every `Version`, recommended first: 3.0.2, 3.0.3, 2.0.7, 1.0.0.
pub const ALL: [Version; 4] = [
    Version::V3_0_2,
    Version::V3_0_3,
    Version::V2_0_7,
    Version::V1_0_0,
];

/// The version the first run picks unless I choose another.
pub const RECOMMENDED: Version = Version::V3_0_2;

impl Version {
    /// "3.0.2", "3.0.3", "2.0.7", "1.0.0".
    pub fn name(self) -> &'static str {
        match self {
            Version::V3_0_2 => "3.0.2",
            Version::V3_0_3 => "3.0.3",
            Version::V2_0_7 => "2.0.7",
            Version::V1_0_0 => "1.0.0",
        }
    }

    /// The build id of `exefs/main` for this version, 32 uppercase hex digits, as in
    /// upstream's `games/acnh/versions.txt`:
    /// 3.0.2 `FCD2BB238ABE99E925B4E452DF1F41F1`, 3.0.3 `FF1D1C05670DB6021C85B624A710B963`,
    /// 2.0.7 `0948E48778171EE6D5D7A65813ED5FCC`, 1.0.0 `7FC1BAFF976AECA414520CB89F4616CB`.
    pub fn build_id(self) -> &'static str {
        match self {
            Version::V3_0_2 => "FCD2BB238ABE99E925B4E452DF1F41F1",
            Version::V3_0_3 => "FF1D1C05670DB6021C85B624A710B963",
            Version::V2_0_7 => "0948E48778171EE6D5D7A65813ED5FCC",
            Version::V1_0_0 => "7FC1BAFF976AECA414520CB89F4616CB",
        }
    }

    /// True where upstream has native 60 FPS and ultrawide (docs/VERSIONS.md): 3.0.2 and
    /// 3.0.3 only.
    pub fn native60(self) -> bool {
        matches!(self, Version::V3_0_2 | Version::V3_0_3)
    }

    /// The `Version` whose `name()` is `name`, or `None`.
    pub fn from_name(name: &str) -> Option<Version> {
        ALL.into_iter().find(|version| version.name() == name)
    }
}

/// The gate: the `Version` whose build id is `build_id`, compared without regard to case and
/// ignoring spaces at either end. Anything else (another build, or text that is not 32 hex
/// digits) is `Error::WrongVersion { found }` with `found` the trimmed input in uppercase.
pub fn from_build_id(build_id: &str) -> Result<Version> {
    let found = build_id.trim().to_uppercase();
    ALL.into_iter()
        .find(|version| version.build_id() == found)
        .ok_or(Error::WrongVersion { found })
}
