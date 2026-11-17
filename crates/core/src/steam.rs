//! The Steam shortcut that starts the game from Game Mode (SPEC.md M2): a non-Steam game entry
//! in `shortcuts.vdf` whose target is `acnh-universal play`, so save sync wraps every start.
//! Steam rewrites the file when it quits, so the launcher writes it only while Steam is
//! closed (`write_shortcut`). Copied from iannitram/totk-universal's `steam` (MIT) by its
//! issues.

use crate::error::{not_built, Result};
use crate::platform::Vars;
use crate::vdf::Value;
use std::path::{Path, PathBuf};

/// CRC-32 (IEEE 802.3, reflected, polynomial 0xEDB88320, initial and final XOR 0xFFFFFFFF):
/// `crc32(b"123456789") == 0xCBF43926`.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut c: u32 = 0xFFFF_FFFF;
    for &b in bytes {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// The shortcut's app id: `crc32(exe + name) | 0x8000_0000`, with `exe` exactly as stored
/// in the shortcut (quoted).
pub fn shortcut_app_id(exe: &str, name: &str) -> u32 {
    crc32(format!("{exe}{name}").as_bytes()) | 0x8000_0000
}

/// The 64-bit id `steam://rungameid/<id>` takes: `(app_id as u64) << 32 | 0x0200_0000`.
pub fn rungameid(app_id: u32) -> u64 {
    ((app_id as u64) << 32) | 0x0200_0000
}

/// A shortcut as the launcher writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    pub name: String,
    /// The program, unquoted.
    pub exe: String,
    pub start_dir: String,
    pub launch_options: String,
    pub icon: String,
}

impl Shortcut {
    /// `exe` in double quotes, as Steam stores it.
    pub fn quoted_exe(&self) -> String {
        format!("\"{}\"", self.exe)
    }

    /// `shortcut_app_id(&self.quoted_exe(), &self.name)`.
    pub fn app_id(&self) -> u32 {
        shortcut_app_id(&self.quoted_exe(), &self.name)
    }
}

/// The shortcut's map, items in this order: "appid" Int(app_id), "AppName" Str(name), "Exe"
/// Str(quoted exe), "StartDir" Str(start_dir quoted the same way), "icon" Str(icon),
/// "ShortcutPath" Str(""), "LaunchOptions" Str(launch_options), "IsHidden" Int(0),
/// "AllowDesktopConfig" Int(1), "AllowOverlay" Int(1), "OpenVR" Int(0), "Devkit" Int(0),
/// "DevkitGameID" Str(""), "DevkitOverrideAppID" Int(0), "LastPlayTime" Int(0),
/// "FlatpakAppID" Str(""), "tags" Map(empty).
pub fn shortcut_entry(s: &Shortcut) -> Value {
    not_built!("steam::shortcut_entry")
}

/// Add `s` to a shortcuts.vdf document, or replace the entry with the same "appid", and return
/// its app id. An empty document gets the "shortcuts" map first. Entries keep their order (a
/// new one goes last) and are renumbered "0", "1", ... A document whose first item is not the
/// map "shortcuts" is `Error::Steam("shortcuts.vdf has no shortcuts list")`.
// A Vec, not a slice: an empty document gets its "shortcuts" map pushed. Drop the allow
// when the body is built (it pushes, so clippy is satisfied).
#[allow(clippy::ptr_arg)]
pub fn upsert(doc: &mut Vec<(String, Value)>, s: &Shortcut) -> Result<u32> {
    not_built("steam::upsert")
}

/// Where Steam keeps its user folders on the OS this is built for, in the order to try: Linux
/// `<home>/.local/share/Steam/userdata`, `<home>/.steam/steam/userdata`,
/// `<home>/.var/app/com.valvesoftware.Steam/.local/share/Steam/userdata` (Flatpak); Windows
/// `C:\Program Files (x86)\Steam\userdata`; Mac
/// `<home>/Library/Application Support/Steam/userdata`.
pub fn userdata_roots(vars: &Vars) -> Vec<PathBuf> {
    if cfg!(target_os = "windows") {
        vec![PathBuf::from(r"C:\Program Files (x86)\Steam\userdata")]
    } else if cfg!(target_os = "macos") {
        vec![vars.home.join("Library/Application Support/Steam/userdata")]
    } else {
        vec![
            vars.home.join(".local/share/Steam/userdata"),
            vars.home.join(".steam/steam/userdata"),
            vars.home
                .join(".var/app/com.valvesoftware.Steam/.local/share/Steam/userdata"),
        ]
    }
}

/// Write the shortcut into every Steam user's `config/shortcuts.vdf` under `userdata`
/// (keeping a `.bak` of the old file), only when Steam is not running (`steam_running`);
/// else `Error::Steam("close Steam first")`. Returns the files written.
pub fn write_shortcut(
    userdata: &Path,
    s: &Shortcut,
    steam_running: &dyn Fn() -> bool,
) -> Result<Vec<PathBuf>> {
    not_built("steam::write_shortcut")
}
