//! The fleet view (SPEC.md M11): what each paired device last reported, and the warning before
//! play when this device's version or build differs from the one that made the latest save.
//! Shape from iannitram/totk-universal's `fleet` (MIT); ACNH has one engine, so the record
//! names the game version and our build instead of a port and its pin.

use crate::error::{not_built, Result};
use crate::platform::Os;

/// What one launcher reports to its hubs after each sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRecord {
    /// The device's name as the player set it ("AMD handheld").
    pub device: String,
    pub os: Os,
    /// The game version it plays ("3.0.2").
    pub version: String,
    /// The bundle's `build_id`.
    pub build_id: String,
    /// Seconds since the Unix epoch of the last save sync; 0 for never.
    pub last_sync_unix: u64,
    pub free_bytes: u64,
}

/// What `drift` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Drift {
    /// Same version and build.
    None,
    /// Another game version: the game refuses a save from a newer version.
    Version { theirs: String, ours: String },
    /// The same version, another build of ours.
    Build { theirs: String, ours: String },
}

/// Compares this device with the one that made the latest save. Different `version`:
/// `Version`. Same version, different `build_id`: `Build`. Otherwise `None`.
pub fn drift(here: &DeviceRecord, saved_by: &DeviceRecord) -> Drift {
    not_built!("fleet::drift")
}

/// The warning the Continue screen shows, or `None` for `Drift::None`:
/// `Version`: "<device> made this save with version <theirs>; this device has <ours>. Put
/// the same version on both, or the game may refuse the island." `Build`: "<device> made this
/// save with another build of ACNH Universal. Update this device first, or play on."
/// `<device>` is `saved_by.device`.
pub fn warning(drift: &Drift, saved_by: &DeviceRecord) -> Option<String> {
    not_built!("fleet::warning")
}

/// A record as one tab-separated line, no trailing newline: device, os key, version, build
/// id, last sync, free bytes. A tab or newline in `device` is
/// `Error::Hub("a device name cannot hold a tab or a new line")`.
pub fn to_line(r: &DeviceRecord) -> Result<String> {
    not_built("fleet::to_line")
}

/// Reads `to_line`'s format back; anything else is `Error::Hub("bad device record: <line>")`.
pub fn from_line(line: &str) -> Result<DeviceRecord> {
    not_built("fleet::from_line")
}
