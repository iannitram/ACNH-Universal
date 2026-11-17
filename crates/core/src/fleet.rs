//! The fleet view (SPEC.md M11): what each paired device last reported, and the warning before
//! play when this device's version or build differs from the one that made the latest save.
//! Shape from iannitram/totk-universal's `fleet` (MIT); ACNH has one engine, so the record
//! names the game version and our build instead of a port and its pin.

use crate::error::{Error, Result};
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
    if here.version != saved_by.version {
        Drift::Version {
            theirs: saved_by.version.clone(),
            ours: here.version.clone(),
        }
    } else if here.build_id != saved_by.build_id {
        Drift::Build {
            theirs: saved_by.build_id.clone(),
            ours: here.build_id.clone(),
        }
    } else {
        Drift::None
    }
}

/// The warning the Continue screen shows, or `None` for `Drift::None`:
/// `Version`: "<device> made this save with version <theirs>; this device has <ours>. Put
/// the same version on both, or the game may refuse the island." `Build`: "<device> made this
/// save with another build of ACNH Universal. Update this device first, or play on."
/// `<device>` is `saved_by.device`.
pub fn warning(drift: &Drift, saved_by: &DeviceRecord) -> Option<String> {
    match drift {
        Drift::None => None,
        Drift::Version { theirs, ours } => Some(format!(
            "{} made this save with version {theirs}; this device has {ours}. Put the same version on both, or the game may refuse the island.",
            saved_by.device
        )),
        Drift::Build { .. } => Some(format!(
            "{} made this save with another build of ACNH Universal. Update this device first, or play on.",
            saved_by.device
        )),
    }
}

/// A record as one tab-separated line, no trailing newline: device, os key, version, build
/// id, last sync, free bytes. A tab or newline in `device` is
/// `Error::Hub("a device name cannot hold a tab or a new line")`.
pub fn to_line(r: &DeviceRecord) -> Result<String> {
    if r.device.contains(['\t', '\n', '\r']) {
        return Err(Error::Hub(
            "a device name cannot hold a tab or a new line".to_string(),
        ));
    }
    Ok(format!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        r.device,
        r.os.key(),
        r.version,
        r.build_id,
        r.last_sync_unix,
        r.free_bytes
    ))
}

/// Reads `to_line`'s format back; anything else is `Error::Hub("bad device record: <line>")`.
pub fn from_line(line: &str) -> Result<DeviceRecord> {
    let bad_line = || Error::Hub(format!("bad device record: {line}"));
    let fields: Vec<_> = line.split('\t').collect();
    if fields.len() != 6 {
        return Err(bad_line());
    }
    if fields[0].contains(['\n', '\r']) {
        return Err(bad_line());
    }
    let os = Os::from_key(fields[1]).ok_or_else(bad_line)?;
    let last_sync_unix: u64 = fields[4].parse().map_err(|_| bad_line())?;
    let free_bytes: u64 = fields[5].parse().map_err(|_| bad_line())?;
    if last_sync_unix.to_string() != fields[4] || free_bytes.to_string() != fields[5] {
        return Err(bad_line());
    }
    Ok(DeviceRecord {
        device: fields[0].to_string(),
        os,
        version: fields[2].to_string(),
        build_id: fields[3].to_string(),
        last_sync_unix,
        free_bytes,
    })
}
