//! The Steam shortcut that starts the game from Game Mode (SPEC.md M2): a non-Steam game entry
//! in `shortcuts.vdf` whose target is `acnh-universal play`, so save sync wraps every start.
//! Steam rewrites the file when it quits, so the launcher writes it only while Steam is
//! closed (`write_shortcut`). Copied from iannitram/totk-universal's `steam` (MIT) by its
//! issues.

use crate::artwork::{self, Art};
use crate::error::{Error, Result};
use crate::platform::Vars;
use crate::vdf;
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
    let string = |value: String| Value::Str(value);
    Value::Map(vec![
        ("appid".into(), Value::Int(s.app_id())),
        ("AppName".into(), string(s.name.clone())),
        ("Exe".into(), string(s.quoted_exe())),
        ("StartDir".into(), string(format!("\"{}\"", s.start_dir))),
        ("icon".into(), string(s.icon.clone())),
        ("ShortcutPath".into(), string(String::new())),
        ("LaunchOptions".into(), string(s.launch_options.clone())),
        ("IsHidden".into(), Value::Int(0)),
        ("AllowDesktopConfig".into(), Value::Int(1)),
        ("AllowOverlay".into(), Value::Int(1)),
        ("OpenVR".into(), Value::Int(0)),
        ("Devkit".into(), Value::Int(0)),
        ("DevkitGameID".into(), string(String::new())),
        ("DevkitOverrideAppID".into(), Value::Int(0)),
        ("LastPlayTime".into(), Value::Int(0)),
        ("FlatpakAppID".into(), string(String::new())),
        ("tags".into(), Value::Map(vec![])),
    ])
}

/// Add `s` to a shortcuts.vdf document, or replace the entry with the same "appid", and return
/// its app id. An empty document gets the "shortcuts" map first. Entries keep their order (a
/// new one goes last) and are renumbered "0", "1", ... A document whose first item is not the
/// map "shortcuts" is `Error::Steam("shortcuts.vdf has no shortcuts list")`.
// A Vec, not a slice: an empty document gets its "shortcuts" map pushed. Drop the allow
// when the body is built (it pushes, so clippy is satisfied).
#[allow(clippy::ptr_arg)]
pub fn upsert(doc: &mut Vec<(String, Value)>, s: &Shortcut) -> Result<u32> {
    if doc.is_empty() {
        doc.push(("shortcuts".into(), Value::Map(vec![])));
    }

    let Some((key, Value::Map(entries))) = doc.first_mut() else {
        return Err(Error::Steam("shortcuts.vdf has no shortcuts list".into()));
    };
    if key != "shortcuts" {
        return Err(Error::Steam("shortcuts.vdf has no shortcuts list".into()));
    }

    let app_id = s.app_id();
    let entry = shortcut_entry(s);
    let mut shortcuts = std::mem::take(entries);
    if let Some(index) = shortcuts.iter().position(|(_, value)| {
        matches!(value, Value::Map(fields) if fields.iter().any(|(key, value)| {
            key == "appid" && *value == Value::Int(app_id)
        }))
    }) {
        shortcuts[index].1 = entry;
    } else {
        shortcuts.push((String::new(), entry));
    }

    for (index, (key, _)) in shortcuts.iter_mut().enumerate() {
        *key = index.to_string();
    }
    *entries = shortcuts;
    Ok(app_id)
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
/// (keeping a `.bak` of the old file) and each of `images` at `artwork::paths`, only when
/// Steam is not running (`steam_running`); else `Error::Steam("close Steam first")` and
/// nothing is written. Returns the shortcuts files written.
pub fn write_shortcut(
    userdata: &Path,
    s: &Shortcut,
    images: &[(Art, Vec<u8>)],
    steam_running: &dyn Fn() -> bool,
) -> Result<Vec<PathBuf>> {
    if steam_running() {
        return Err(Error::Steam("close Steam first".into()));
    }
    let mut written = Vec::new();
    for entry in std::fs::read_dir(userdata)? {
        let user_dir = entry?.path();
        if !user_dir.is_dir() {
            continue;
        }
        let file = user_dir.join("config").join("shortcuts.vdf");
        let mut doc = if file.exists() {
            vdf::read(&std::fs::read(&file)?)?
        } else {
            Vec::new()
        };
        let app_id = upsert(&mut doc, s)?;
        let bytes = vdf::write(&doc)?;
        if file.exists() {
            std::fs::copy(&file, file.with_extension("vdf.bak"))?;
        }
        std::fs::create_dir_all(artwork::grid_dir(&user_dir))?;
        std::fs::write(&file, bytes)?;
        for (art, path) in artwork::paths(&user_dir, app_id) {
            if let Some((_, bytes)) = images.iter().find(|(a, _)| *a == art) {
                std::fs::write(path, bytes)?;
            }
        }
        written.push(file);
    }
    Ok(written)
}
