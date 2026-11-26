//! Contained app folder packing for Linux/SteamOS (#191) and Windows (#190).
//!
//! Packs `<out>/ACNH-Universal/` with the launcher binaries (`acnh-app`, `acnh-universal`,
//! `acnhrecomp.png`, and `source/`), the assembled player (`acnh` + `$ORIGIN` libraries),
//! and a read-only `game/` folder (`analysis/`, `romfs.pak`, `spirv.pak`, `fonts/`, and
//! optional `aoc/`). Nothing else is copied, and nothing is written outside `<out>/ACNH-Universal/`.

use crate::bundle::is_forbidden;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::platform::{AppDirs, Os, Vars};
use crate::prepare::redact;
use crate::recipe::output;
use crate::steam::assets_dir_for;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

const APP_FOLDER: &str = "ACNH-Universal";
const PARTIAL_FOLDER: &str = ".ACNH-Universal.partial";
const OLD_FOLDER: &str = ".ACNH-Universal.old";

/// Default output parent folder for the contained app: `launcher.ini`'s `app_dir` key when
/// set and non-empty, else `<home>/Applications`.
pub fn app_out_dir(config: &Config, home: &Path) -> PathBuf {
    app_out_dir_for(Os::Linux, config, home, home)
}

/// Default output parent folder for the contained app on `os`: `launcher.ini`'s `app_dir`
/// key when set and non-empty, else `<data>/Programs` on Windows (`%LOCALAPPDATA%\Programs\`)
/// and `<home>/Applications` elsewhere.
pub fn app_out_dir_for(os: Os, config: &Config, data: &Path, home: &Path) -> PathBuf {
    match config.get("app_dir") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ if os == Os::Windows => data.join("Programs"),
        _ => home.join("Applications"),
    }
}

/// Dependencies and environment overrides for [`pack_folder_with`] (the seam tests use).
pub struct PackContext<'a> {
    pub os: Os,
    pub appdir: Option<&'a Path>,
    pub home: &'a Path,
    pub data_root: &'a Path,
    pub game_folder: Option<&'a Path>,
    pub dropbox: Option<&'a Path>,
    pub onedrive: Option<&'a Path>,
    pub ci: bool,
    pub runner_temp: Option<&'a Path>,
    pub git_in_work_tree: &'a mut dyn FnMut(&Path) -> Result<bool>,
    pub copy_file: &'a mut dyn FnMut(&Path, &Path) -> Result<()>,
}

/// Pack `<out>/ACNH-Universal/` using the host environment (`HOME`/`USERPROFILE`, `APPDIR`,
/// `OneDrive`, `CI`, `RUNNER_TEMP`, `launcher.ini`).
pub fn pack_folder(
    launcher: &Path,
    assembled: &Path,
    prepared: &Path,
    out: &Path,
) -> Result<PathBuf> {
    let os = Os::current();
    let vars = Vars::from_lookup(os, |name| std::env::var(name).ok())?;
    let dirs = AppDirs::new(&vars);
    let launcher_cfg = match fs::read_to_string(dirs.config.join("launcher.ini")) {
        Ok(text) => Config::parse(&text)?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(err) => return Err(err.into()),
    };
    let dropbox = launcher_cfg
        .get("dropbox")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    let onedrive = env_path("OneDrive").or_else(|| env_path("ONEDRIVE"));
    let game_folder = launcher_cfg
        .get("game_folder")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            fs::read_to_string(prepared.join("game_source.txt"))
                .ok()
                .map(|s| s.trim_end_matches(['\r', '\n']).to_owned())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        });
    let appdir = env_path("APPDIR");
    let ci = env_nonempty("CI") || env_nonempty("GITHUB_ACTIONS");
    let runner_temp = env_path("RUNNER_TEMP");
    let data_root = dirs.data_root();
    let mut git_probe = |dir: &Path| host_git_in_work_tree(dir);
    let mut copy = |from: &Path, to: &Path| host_copy_file(from, to);

    let mut ctx = PackContext {
        os,
        appdir: appdir.as_deref(),
        home: &vars.home,
        data_root: &data_root,
        game_folder: game_folder.as_deref(),
        dropbox: dropbox.as_deref(),
        onedrive: onedrive.as_deref(),
        ci,
        runner_temp: runner_temp.as_deref(),
        git_in_work_tree: &mut git_probe,
        copy_file: &mut copy,
    };
    pack_folder_with(launcher, assembled, prepared, out, &mut ctx)
}

/// Pack `<out>/ACNH-Universal/` with explicit environment and file-operation seams.
pub fn pack_folder_with(
    launcher: &Path,
    assembled: &Path,
    prepared: &Path,
    out: &Path,
    ctx: &mut PackContext<'_>,
) -> Result<PathBuf> {
    repack_if_stale(prepared, ctx)?;
    check_prepared(prepared, ctx)?;
    check_out_location(out, ctx)?;

    let launcher_files = resolve_launcher_files(launcher, ctx.appdir, ctx.os)?;
    let assembled_files = collect_assembled_files(assembled, ctx.os)?;

    let final_dir = out.join(APP_FOLDER);
    let partial_dir = out.join(PARTIAL_FOLDER);
    let old_dir = out.join(OLD_FOLDER);

    fs::create_dir_all(out)?;
    if partial_dir.exists() {
        remove_tree_writable(&partial_dir)?;
    }

    if let Err(err) = stage_folder(
        &partial_dir,
        &launcher_files,
        &assembled_files,
        prepared,
        ctx,
    ) {
        let _ = remove_tree_writable(&partial_dir);
        return Err(err);
    }

    if old_dir.exists() {
        remove_tree_writable(&old_dir)?;
    }

    if final_dir.exists() {
        fs::rename(&final_dir, &old_dir)?;
        if let Err(err) = fs::rename(&partial_dir, &final_dir) {
            let _ = fs::rename(&old_dir, &final_dir);
            let _ = remove_tree_writable(&partial_dir);
            return Err(err.into());
        }
        let _ = remove_tree_writable(&old_dir);
    } else if let Err(err) = fs::rename(&partial_dir, &final_dir) {
        let _ = remove_tree_writable(&partial_dir);
        return Err(err.into());
    }

    Ok(final_dir)
}

struct LauncherFiles {
    acnh_app: PathBuf,
    acnh_universal: PathBuf,
    icon: Option<PathBuf>,
    picture: Option<PathBuf>,
    source: Option<PathBuf>,
}

fn resolve_launcher_files(launcher: &Path, appdir: Option<&Path>, os: Os) -> Result<LauncherFiles> {
    let suffix = if os == Os::Windows { ".exe" } else { "" };
    let app_name = format!("acnh-app{suffix}");
    let cli_name = format!("acnh-universal{suffix}");

    let (acnh_app, acnh_universal, icon, picture, source) = if let Some(appdir) = appdir {
        let share = appdir.join("usr/share/acnh-universal");
        let source = share.join("source");
        if !source.is_dir() {
            return Err(Error::Build(format!(
                "AppImage mount is missing {}",
                source.display()
            )));
        }
        (
            appdir.join("usr/bin").join(&app_name),
            appdir.join("usr/bin").join(&cli_name),
            None,
            Some(share.join("acnhrecomp.png")),
            Some(source),
        )
    } else {
        let dir = launcher.parent().unwrap_or(Path::new("."));
        let assets = assets_dir_for(launcher, None);
        let source = dir.join("source");
        let ico = assets.join("acnhrecomp.ico");
        let png = assets.join("acnhrecomp.png");
        let (icon, picture) = if os == Os::Windows {
            (Some(ico), png.is_file().then_some(png))
        } else {
            (None, Some(png))
        };
        (
            dir.join(&app_name),
            dir.join(&cli_name),
            icon,
            picture,
            source.is_dir().then_some(source),
        )
    };

    for (label, path) in [
        ("acnh-app", Some(&acnh_app)),
        ("acnh-universal", Some(&acnh_universal)),
        ("acnhrecomp.ico", icon.as_ref()),
        ("acnhrecomp.png", picture.as_ref()),
    ] {
        if let Some(path) = path {
            if !path.is_file() {
                return Err(Error::Build(format!(
                    "missing launcher file {label} at {}",
                    path.display()
                )));
            }
        }
    }

    Ok(LauncherFiles {
        acnh_app,
        acnh_universal,
        icon,
        picture,
        source,
    })
}

fn collect_assembled_files(assembled: &Path, os: Os) -> Result<Vec<(String, PathBuf)>> {
    let player_name = output(os);
    if !assembled.join(player_name).is_file() {
        return Err(Error::Build(format!(
            "assembled player {player_name} is missing in {}",
            assembled.display()
        )));
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(assembled)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_forbidden(&name) {
            continue;
        }
        files.push((name, path));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

/// Rebuilds a pack whose key record is missing or stale (a killed run can leave a truncated
/// `romfs.pak` beside intact records of other stages), when the game's RomFS is at hand. A
/// no-op while the records match, and without a game folder there is nothing to rebuild from.
fn repack_if_stale(prepared: &Path, ctx: &PackContext<'_>) -> Result<()> {
    let Some(game) = ctx.game_folder else {
        return Ok(());
    };
    let Some(romfs) = [game.join("romfs"), game.join("extract").join("romfs")]
        .into_iter()
        .find(|dir| dir.is_dir())
    else {
        return Ok(());
    };
    if !prepared.is_dir() {
        return Ok(());
    }
    crate::prepare::pack_prepared(prepared, &romfs)
}

fn check_prepared(prepared: &Path, ctx: &PackContext<'_>) -> Result<()> {
    for rel in ["analysis/main.bin", "romfs.pak", "spirv.pak"] {
        if !prepared.join(rel).is_file() {
            return Err(Error::Build(format!(
                "prepared folder is missing {rel}: {}",
                redact(&prepared.display().to_string(), ctx.home)
            )));
        }
    }
    if ctx.ci {
        check_empty_decoy_tree(prepared, prepared)?;
    }
    Ok(())
}

fn check_empty_decoy_tree(root: &Path, dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            check_empty_decoy_tree(root, &path)?;
        } else if entry.metadata()?.len() > 0 {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            return Err(Error::Build(format!(
                "under CI, prepared folder must hold only empty decoy files (non-empty: {})",
                rel.display()
            )));
        }
    }
    Ok(())
}

fn check_out_location(out: &Path, ctx: &mut PackContext<'_>) -> Result<()> {
    let abs_out = normalize_abs(out)?;
    let redacted = redact(&abs_out.display().to_string(), ctx.home);

    let mut existing = abs_out.as_path();
    while !existing.is_dir() {
        match existing.parent() {
            Some(parent) => existing = parent,
            None => break,
        }
    }
    if (ctx.git_in_work_tree)(existing)? {
        return Err(Error::Build(format!(
            "cannot pack into {redacted}: it is inside a git work tree"
        )));
    }

    if let Some(game) = ctx.game_folder {
        if abs_out.starts_with(normalize_abs(game)?) {
            return Err(Error::Build(format!(
                "cannot pack into {redacted}: it is inside the game folder"
            )));
        }
    }

    if abs_out.starts_with(normalize_abs(ctx.data_root)?) {
        return Err(Error::Build(format!(
            "cannot pack into {redacted}: it is inside the data root"
        )));
    }

    let dropbox = ctx
        .dropbox
        .map(Path::to_path_buf)
        .unwrap_or_else(|| ctx.home.join("Dropbox"));
    if abs_out.starts_with(normalize_abs(&dropbox)?) {
        return Err(Error::Build(format!(
            "cannot pack into {redacted}: it is inside Dropbox (a synced folder)"
        )));
    }

    let nextcloud = ctx.home.join("Nextcloud");
    if abs_out.starts_with(normalize_abs(&nextcloud)?) {
        return Err(Error::Build(format!(
            "cannot pack into {redacted}: it is inside Nextcloud (a synced folder)"
        )));
    }

    if let Some(onedrive) = ctx.onedrive {
        if abs_out.starts_with(normalize_abs(onedrive)?) {
            return Err(Error::Build(format!(
                "cannot pack into {redacted}: it is inside OneDrive (a synced folder)"
            )));
        }
    }

    if ctx.ci {
        let Some(runner_temp) = ctx.runner_temp else {
            return Err(Error::Build(format!(
                "under CI, RUNNER_TEMP must be set and contain {redacted}"
            )));
        };
        if !abs_out.starts_with(normalize_abs(runner_temp)?) {
            return Err(Error::Build(format!(
                "under CI, cannot pack into {redacted}: it is outside RUNNER_TEMP"
            )));
        }
    }

    Ok(())
}

fn stage_folder(
    partial: &Path,
    launcher: &LauncherFiles,
    assembled: &[(String, PathBuf)],
    prepared: &Path,
    ctx: &mut PackContext<'_>,
) -> Result<()> {
    fs::create_dir_all(partial)?;

    let app_dest = partial.join(
        launcher
            .acnh_app
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("acnh-app")),
    );
    (ctx.copy_file)(&launcher.acnh_app, &app_dest)?;
    set_executable(&app_dest)?;

    let cli_dest = partial.join(
        launcher
            .acnh_universal
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("acnh-universal")),
    );
    (ctx.copy_file)(&launcher.acnh_universal, &cli_dest)?;
    set_executable(&cli_dest)?;

    if let Some(icon) = &launcher.icon {
        (ctx.copy_file)(icon, &partial.join("acnhrecomp.ico"))?;
    }
    if let Some(picture) = &launcher.picture {
        (ctx.copy_file)(picture, &partial.join("acnhrecomp.png"))?;
    }

    if let Some(source) = &launcher.source {
        copy_dir_files(source, &partial.join("source"), ctx.copy_file)?;
    }

    for (name, src) in assembled {
        let dest = partial.join(name);
        (ctx.copy_file)(src, &dest)?;
        if name == "acnh" || name == "acnh.exe" {
            set_executable(&dest)?;
        }
    }

    let game_dir = partial.join("game");
    fs::create_dir_all(&game_dir)?;
    copy_dir_files(
        &prepared.join("analysis"),
        &game_dir.join("analysis"),
        ctx.copy_file,
    )?;
    (ctx.copy_file)(&prepared.join("romfs.pak"), &game_dir.join("romfs.pak"))?;
    (ctx.copy_file)(&prepared.join("spirv.pak"), &game_dir.join("spirv.pak"))?;

    let fonts = prepared.join("fonts");
    if fonts.is_dir() {
        copy_dir_files(&fonts, &game_dir.join("fonts"), ctx.copy_file)?;
    }
    let aoc = prepared.join("aoc");
    if aoc.is_dir() {
        copy_dir_files(&aoc, &game_dir.join("aoc"), ctx.copy_file)?;
    }

    make_read_only_recursive(&game_dir)?;
    Ok(())
}

fn copy_dir_files(
    from: &Path,
    to: &Path,
    copy_file: &mut dyn FnMut(&Path, &Path) -> Result<()>,
) -> Result<()> {
    fs::create_dir_all(to)?;
    let mut entries: Vec<_> = fs::read_dir(from)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let src = entry.path();
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_files(&src, &dest, copy_file)?;
        } else {
            copy_file(&src, &dest)?;
        }
    }
    Ok(())
}

fn host_copy_file(from: &Path, to: &Path) -> Result<()> {
    fs::copy(from, to)?;
    Ok(())
}

fn set_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn make_read_only_recursive(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            make_read_only_recursive(&path)?;
        } else {
            set_read_only(&path)?;
        }
    }
    set_read_only(dir)?;
    Ok(())
}

fn set_read_only(path: &Path) -> Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(perms.mode() & !0o222);
    }
    #[cfg(not(unix))]
    {
        perms.set_readonly(true);
    }
    fs::set_permissions(path, perms)?;
    Ok(())
}

fn remove_tree_writable(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    make_writable_recursive(dir)?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

fn make_writable_recursive(dir: &Path) -> Result<()> {
    set_writable(dir)?;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            make_writable_recursive(&path)?;
        } else {
            set_writable(&path)?;
        }
    }
    Ok(())
}

fn set_writable(path: &Path) -> Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(perms.mode() | 0o200);
    }
    #[cfg(not(unix))]
    {
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
    }
    fs::set_permissions(path, perms)?;
    Ok(())
}

fn host_git_in_work_tree(existing: &Path) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(existing)
        .args(["rev-parse", "--is-inside-work-tree"])
        .stdin(Stdio::null())
        .output()
        .map_err(|err| {
            Error::Build(format!(
                "cannot tell whether {} is inside a git work tree (is git installed?): {err}",
                existing.display()
            ))
        })?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim() == "true");
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("not a git repository") {
        return Ok(false);
    }
    Err(Error::Build(format!(
        "cannot tell whether {} is inside a git work tree: {}",
        existing.display(),
        stderr.trim()
    )))
}

fn normalize_abs(path: &Path) -> Result<PathBuf> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut stack = Vec::new();
    for comp in abs.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    let lexical: PathBuf = stack.iter().collect();
    // Canonicalize the deepest existing ancestor so symlinks (e.g. /tmp -> /private/tmp)
    // compare identically on both sides.
    let mut tail = Vec::new();
    let mut cursor = lexical.as_path();
    while !cursor.exists() {
        if let Some(name) = cursor.file_name() {
            tail.push(name.to_os_string());
        }
        match cursor.parent() {
            Some(parent) => cursor = parent,
            None => break,
        }
    }
    let mut base = fs::canonicalize(cursor).unwrap_or_else(|_| cursor.to_path_buf());
    for part in tail.into_iter().rev() {
        base.push(part);
    }
    Ok(base)
}

fn env_nonempty(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}
