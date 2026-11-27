//! The Steam shortcut opens the launcher Continue screen in Game Mode (SPEC.md M2).
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

/// Locate the launcher assets from an executable and optional AppImage mount directory.
pub fn assets_dir_for(exe: &Path, appdir: Option<&Path>) -> PathBuf {
    if let Some(appdir) = appdir {
        return appdir.join("usr/share/acnh-universal");
    }
    let folder = exe.parent().unwrap_or(Path::new("."));
    if let Some(contents) = folder
        .parent()
        .filter(|parent| folder.ends_with("MacOS") && parent.ends_with("Contents"))
    {
        return contents.join("Resources");
    }
    if folder.join("acnhrecomp.png").is_file() || folder.join("acnhrecomp.ico").is_file() {
        return folder.to_path_buf();
    }
    for ancestor in folder.ancestors() {
        let assets = ancestor.join("assets");
        if assets.join("acnhrecomp.png").is_file() || assets.join("acnhrecomp.ico").is_file() {
            return assets;
        }
    }
    folder.to_path_buf()
}

/// Locate assets independently of the process working directory.
pub fn assets_dir() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let appdir = std::env::var_os("APPDIR").map(PathBuf::from);
    Ok(assets_dir_for(&exe, appdir.as_deref()))
}

/// Build the launcher entry. On Linux and Windows, if a packed `ACNH-Universal` folder
/// exists (either beside `appimage`, in `app_out_dir_for`, or `exe` itself is inside one),
/// it wins over `appimage`.
pub fn launcher_shortcut(exe: &Path, appimage: Option<&Path>, name: &str, icon: &Path) -> Shortcut {
    let os = crate::platform::Os::current();
    let app_out = Vars::from_lookup(os, |k| std::env::var(k).ok())
        .ok()
        .map(|vars| {
            let dirs = crate::platform::AppDirs::new(&vars);
            let cfg = std::fs::read_to_string(dirs.config.join("launcher.ini"))
                .ok()
                .and_then(|text| crate::config::Config::parse(&text).ok())
                .unwrap_or_default();
            crate::pack_folder::app_out_dir_for(os, &cfg, &vars.data, &vars.home)
        });
    launcher_shortcut_for(exe, appimage, app_out.as_deref(), name, icon)
}

/// Pure helper for [`launcher_shortcut`] with an explicit `app_out` directory.
pub fn launcher_shortcut_for(
    exe: &Path,
    appimage: Option<&Path>,
    app_out: Option<&Path>,
    name: &str,
    icon: &Path,
) -> Shortcut {
    let packed = packed_launcher(exe, appimage, app_out);
    let chosen = packed.as_deref().or(appimage).unwrap_or(exe);
    Shortcut {
        name: name.into(),
        exe: chosen.display().to_string(),
        start_dir: chosen
            .parent()
            .unwrap_or(Path::new("."))
            .display()
            .to_string(),
        launch_options: "--fullscreen".into(),
        icon: icon.display().to_string(),
    }
}

fn packed_launcher(exe: &Path, appimage: Option<&Path>, app_out: Option<&Path>) -> Option<PathBuf> {
    if exe
        .parent()
        .is_some_and(|dir| dir.join("game/analysis/main.bin").is_file())
    {
        return Some(exe.to_path_buf());
    }
    if let Some(candidate) = appimage
        .and_then(Path::parent)
        .map(|dir| dir.join("ACNH-Universal").join("acnh-app"))
        .filter(|path| path.is_file())
    {
        return Some(candidate);
    }
    // One component per join: a "/" inside a component stays in the path as written on Windows,
    // so the shortcut would name `...\ACNH-Universal/acnh-app.exe`.
    for name in ["acnh-app.exe", "acnh-app"] {
        if let Some(candidate) = app_out
            .map(|dir| dir.join("ACNH-Universal").join(name))
            .filter(|path| path.is_file())
        {
            return Some(candidate);
        }
    }
    None
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
/// its app id. See [`upsert_replacing`], which also says which older launcher entries went.
pub fn upsert(doc: &mut Vec<(String, Value)>, s: &Shortcut) -> Result<u32> {
    upsert_replacing(doc, s).map(|(app_id, _)| app_id)
}

/// Whether `exe` (as stored, quotes allowed) is a path the launcher itself has been added
/// under: an AppImage, or the packed `ACNH-Universal/acnh-app`.
fn is_launcher_exe(exe: &str) -> bool {
    let path = Path::new(exe.trim_matches('"'));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let packed = matches!(name, "acnh-app" | "acnh-app.exe")
        && path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|dir| dir == "ACNH-Universal");
    packed || name.to_ascii_lowercase().ends_with(".appimage")
}

fn text_field<'a>(fields: &'a [(String, Value)], name: &str) -> Option<&'a str> {
    fields.iter().find_map(|(key, value)| match value {
        Value::Str(text) if key == name => Some(text.as_str()),
        _ => None,
    })
}

fn entry_app_id(fields: &[(String, Value)]) -> Option<u32> {
    fields.iter().find_map(|(key, value)| match value {
        Value::Int(id) if key == "appid" => Some(*id),
        _ => None,
    })
}

/// Add `s` to a shortcuts.vdf document and return its app id and the app ids of the older
/// launcher entries it replaced. An entry is replaced when it has the same "appid", or the same
/// "AppName" and an "Exe" the launcher itself uses (an AppImage or the packed
/// `ACNH-Universal/acnh-app`): the launcher moves from the AppImage to the packed folder, which
/// changes the app id, and the old entry must not stay beside the new one. The new entry takes
/// the place of the first one it replaces; else it goes last. Other entries keep their order. An
/// empty document gets the "shortcuts" map first. Entries are renumbered "0", "1", ... A document
/// whose first item is not the map "shortcuts" is
/// `Error::Steam("shortcuts.vdf has no shortcuts list")`.
// A Vec, not a slice: an empty document gets its "shortcuts" map pushed.
#[allow(clippy::ptr_arg)]
pub fn upsert_replacing(doc: &mut Vec<(String, Value)>, s: &Shortcut) -> Result<(u32, Vec<u32>)> {
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
    let mut replaced = Vec::new();
    let mut shortcuts: Vec<(String, Value)> = Vec::new();
    let mut placed = false;
    for (key, value) in std::mem::take(entries) {
        let ours = match &value {
            Value::Map(fields) => {
                let same_id = entry_app_id(fields) == Some(app_id);
                let older = text_field(fields, "AppName") == Some(s.name.as_str())
                    && text_field(fields, "Exe").is_some_and(is_launcher_exe);
                if older && !same_id {
                    // The id Steam shows the artwork under: stored, else worked out from the
                    // entry the way the launcher made it.
                    let old = entry_app_id(fields).or_else(|| {
                        Some(shortcut_app_id(
                            text_field(fields, "Exe")?,
                            text_field(fields, "AppName")?,
                        ))
                    });
                    replaced.extend(old.filter(|old| *old != app_id));
                }
                same_id || older
            }
            _ => false,
        };
        if !ours {
            shortcuts.push((key, value));
        } else if !placed {
            shortcuts.push((key, entry.clone()));
            placed = true;
        }
    }
    if !placed {
        shortcuts.push((String::new(), entry));
    }

    for (index, (key, _)) in shortcuts.iter_mut().enumerate() {
        *key = index.to_string();
    }
    *entries = shortcuts;
    replaced.dedup();
    Ok((app_id, replaced))
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

/// What a person is told when Steam is (or may be) running: `shortcuts.vdf` is only safe to
/// write while Steam is closed, and Steam is always running in Game Mode.
pub const CLOSE_STEAM: &str =
    "Steam must be closed first. Switch to Desktop Mode and quit Steam (Steam menu, Exit), then add the shortcut again";

/// The steps of [`CLOSE_STEAM`] as the end of a sentence that starts "To add this to Steam, ".
pub const CLOSE_STEAM_STEPS: &str =
    "switch to Desktop Mode, quit Steam (Steam menu, Exit), then run Add to Steam from there. The launcher never stops Steam for you.";

/// Whether the device is in Game Mode: the gamescope session, or Steam's gamepad UI. `desktop`
/// is `XDG_CURRENT_DESKTOP`; `gamepad_ui` is whether `SteamGamepadUI` is set.
pub fn game_mode(desktop: Option<&str>, gamepad_ui: bool) -> bool {
    desktop == Some("gamescope") || gamepad_ui
}

/// [`game_mode`] for this process's environment.
pub fn in_game_mode() -> bool {
    game_mode(
        std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
        std::env::var_os("SteamGamepadUI").is_some(),
    )
}

/// Whether Steam is running. It fails closed: a check that cannot be made (the program is
/// missing, will not start, or answers with an error) reads as running, so the file is never
/// written under a Steam that might rewrite it.
pub fn steam_running() -> bool {
    steam_running_on(None)
}

/// [`steam_running`] looking the check program up on `path` (instead of `PATH`), for tests.
pub fn steam_running_on(path: Option<&std::ffi::OsStr>) -> bool {
    use std::process::Command;
    let command = |program: &str| {
        let mut command = Command::new(program);
        if let Some(path) = path {
            command.env("PATH", path);
        }
        command
    };
    if cfg!(target_os = "windows") {
        return match command("tasklist")
            .args(["/FI", "IMAGENAME eq steam.exe", "/NH"])
            .output()
        {
            Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
                .to_ascii_lowercase()
                .contains("steam.exe"),
            _ => true,
        };
    }
    ["steam", "steam_osx", "Steam"].iter().any(|name| {
        // pgrep: 0 a match, 1 no match, anything else (2, 3, a signal) did not answer.
        !matches!(
            command("pgrep").args(["-x", name]).output(),
            Ok(output) if output.status.code() == Some(1)
        )
    })
}

/// Write the shortcut into every Steam user's `config/shortcuts.vdf` under `userdata`
/// (keeping the first `.bak` of the old file, never replacing it) and each of `images` at
/// `artwork::paths`, only when Steam is not running (`steam_running`); else
/// `Error::Steam(CLOSE_STEAM)` and nothing is written. An older launcher entry the new one
/// replaces loses its artwork files. An empty `shortcuts.vdf` reads as no shortcuts. Returns
/// the shortcuts files written.
pub fn write_shortcut(
    userdata: &Path,
    s: &Shortcut,
    images: &[(Art, Vec<u8>)],
    steam_running: &dyn Fn() -> bool,
) -> Result<Vec<PathBuf>> {
    if steam_running() {
        return Err(Error::Steam(CLOSE_STEAM.into()));
    }
    let mut written = Vec::new();
    for entry in std::fs::read_dir(userdata)? {
        let user_dir = entry?.path();
        if !user_dir.is_dir() {
            continue;
        }
        let file = user_dir.join("config").join("shortcuts.vdf");
        let old = if file.exists() {
            std::fs::read(&file)?
        } else {
            Vec::new()
        };
        let mut doc = vdf::read(&old)?;
        let (app_id, replaced) = upsert_replacing(&mut doc, s)?;
        let bytes = vdf::write(&doc)?;
        let backup = file.with_extension("vdf.bak");
        if !old.is_empty() && !backup.exists() {
            std::fs::copy(&file, &backup)?;
        }
        std::fs::create_dir_all(artwork::grid_dir(&user_dir))?;
        crate::platform::atomic_write(&file, &bytes)?;
        for old_id in replaced {
            for (_, path) in artwork::paths(&user_dir, old_id) {
                match std::fs::remove_file(&path) {
                    Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                        return Err(err.into())
                    }
                    _ => {}
                }
            }
        }
        for (art, path) in artwork::paths(&user_dir, app_id) {
            if let Some((_, bytes)) = images.iter().find(|(a, _)| *a == art) {
                crate::platform::atomic_write(&path, bytes)?;
            }
        }
        written.push(file);
    }
    Ok(written)
}

#[cfg(test)]
mod launcher_tests {
    use super::*;

    #[test]
    fn game_mode_is_gamescope_or_the_gamepad_ui() {
        assert!(game_mode(Some("gamescope"), false));
        assert!(game_mode(None, true));
        assert!(!game_mode(Some("KDE"), false));
        assert!(!game_mode(None, false));
    }

    #[test]
    fn launcher_exes_are_the_appimage_and_the_packed_app() {
        assert!(is_launcher_exe("\"/home/u/ACNH Universal.AppImage\""));
        assert!(is_launcher_exe("/home/u/ACNH-Universal/acnh-app"));
        assert!(is_launcher_exe("C:\\x/ACNH-Universal/acnh-app.exe"));
        assert!(!is_launcher_exe("/home/u/other/acnh-app"));
        assert!(!is_launcher_exe("/usr/bin/some-game"));
    }

    #[test]
    fn appimage_shortcut_uses_outer_path_and_fullscreen() {
        let image = Path::new("/home/user/Applications/ACNH.AppImage");
        let shortcut = launcher_shortcut_for(
            Path::new("/tmp/mount/usr/bin/acnh-app"),
            Some(image),
            None,
            "ACNH",
            Path::new("icon.png"),
        );
        assert_eq!(
            shortcut.quoted_exe(),
            "\"/home/user/Applications/ACNH.AppImage\""
        );
        assert_eq!(shortcut.start_dir, "/home/user/Applications");
        assert_eq!(shortcut.launch_options, "--fullscreen");
        assert_eq!(
            shortcut.app_id(),
            shortcut_app_id(&shortcut.quoted_exe(), "ACNH")
        );
        let mut doc = vec![];
        upsert(&mut doc, &shortcut).unwrap();
        assert_eq!(
            doc[0].1,
            Value::Map(vec![("0".into(), shortcut_entry(&shortcut))])
        );
    }

    #[test]
    fn assets_follow_packaging_layouts() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        assert_eq!(
            assets_dir_for(&root.join("usr/bin/acnh-app"), Some(root)),
            root.join("usr/share/acnh-universal")
        );
        assert_eq!(
            assets_dir_for(&root.join("Game.app/Contents/MacOS/acnh-app"), None),
            root.join("Game.app/Contents/Resources")
        );
        assert_eq!(
            assets_dir_for(&root.join("windows/acnh-app.exe"), None),
            root.join("windows")
        );
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("assets/acnhrecomp.png"), []).unwrap();
        assert_eq!(
            assets_dir_for(&root.join("target/debug/acnh-app"), None),
            root.join("assets")
        );
    }

    #[test]
    fn packed_folder_shortcut_replaces_appimage_when_folder_exists() {
        let root = tempfile::tempdir().unwrap();
        let apps = root.path().join("Applications");
        let packed = apps.join("ACNH-Universal");
        std::fs::create_dir_all(&packed).unwrap();
        let packed_exe = packed.join("acnh-app");
        std::fs::write(&packed_exe, b"").unwrap();
        let image = apps.join("ACNH.AppImage");

        let shortcut = launcher_shortcut_for(
            Path::new("/tmp/mount/usr/bin/acnh-app"),
            Some(&image),
            Some(&apps),
            "ACNH",
            Path::new("icon.png"),
        );
        assert_eq!(
            shortcut.quoted_exe(),
            format!("\"{}\"", packed_exe.display())
        );
        assert_eq!(shortcut.start_dir, packed.display().to_string());
        assert_eq!(shortcut.launch_options, "--fullscreen");
        assert_eq!(
            shortcut.app_id(),
            shortcut_app_id(&shortcut.quoted_exe(), "ACNH")
        );
    }

    #[test]
    fn windows_packed_folder_shortcut_uses_exe_and_start_dir() {
        let root = tempfile::tempdir().unwrap();
        let programs = root.path().join("Programs");
        let packed = programs.join("ACNH-Universal");
        std::fs::create_dir_all(&packed).unwrap();
        let packed_exe = packed.join("acnh-app.exe");
        std::fs::write(&packed_exe, b"").unwrap();

        let shortcut = launcher_shortcut_for(
            &root.path().join("release/acnh-app.exe"),
            None,
            Some(&programs),
            "ACNH",
            Path::new("acnhrecomp.ico"),
        );
        assert_eq!(
            shortcut.quoted_exe(),
            format!("\"{}\"", packed_exe.display())
        );
        assert_eq!(shortcut.start_dir, packed.display().to_string());
        assert_eq!(shortcut.launch_options, "--fullscreen");
        assert_eq!(
            shortcut.app_id(),
            shortcut_app_id(&shortcut.quoted_exe(), "ACNH")
        );
    }
}
