//! The game stage: the fan mods (PT-BR, SpeedFix) and the version stage.

use super::*;

/// A mod's `.sarc.zs` archives as the game reads them (`sarc::fix_archives`), for every mod
/// installed in `out/mods/<name>/romfs`.
pub(super) fn fix_mod_archives(req: &Request) -> Result<()> {
    let Ok(mods) = std::fs::read_dir(req.out.join("mods")) else {
        return Ok(());
    };
    let mut romfs: Vec<PathBuf> = mods
        .filter_map(|entry| entry.ok().map(|entry| entry.path().join("romfs")))
        .filter(|path| path.is_dir())
        .collect();
    romfs.sort();
    for path in romfs {
        crate::sarc::fix_archives(&path)?;
    }
    Ok(())
}

pub(super) fn mod_component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.ends_with([' ', '.'])
        && !name.chars().any(|c| {
            matches!(c, '\\' | '/' | ':' | '<' | '>' | '"' | '|' | '?' | '*') || c <= '\x1f'
        })
}

/// Extracts `zpath`'s `romfs/` into `<mods_root>/<name>` with pending message reflow.
pub fn install_mod_archive(
    mods_root: &Path,
    name: &str,
    zpath: &Path,
    romfs: &Path,
    python: &str,
) -> Result<()> {
    let dst = mods_root.join(name);
    if crate::platform::is_dir_link(&dst) {
        return Err(Error::Build(
            "mod output must be a regular directory".into(),
        ));
    }
    if let (Ok(zp), Ok(dp)) = (std::fs::canonicalize(zpath), std::fs::canonicalize(&dst)) {
        if zp.starts_with(&dp) {
            return Err(Error::Build(
                "keep the source mod archive outside its prepared output".into(),
            ));
        }
    }
    std::fs::create_dir_all(mods_root)?;
    let temp = tempfile::Builder::new()
        .prefix(".prepare-mod-")
        .tempdir_in(mods_root)?;
    let ready = temp.path().join("ready");
    std::fs::create_dir_all(&ready)?;

    let script = r#"import os, re, shutil, sys, zipfile
def ok(n):
    return bool(n) and n not in ('.', '..') and not n.endswith((' ', '.')) and not re.search(r'[\\/:<>"|?*\x00-\x1f]', n)
zpath, ready = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(zpath) as z:
    names = [e.filename for e in z.infolist()]
    if any(not all(ok(p) for p in path.rstrip('/').split('/')) for path in names if path.rstrip('/')):
        sys.exit('unsafe path in the zip')
    prefix = ''
    if not any(path.startswith('romfs/') for path in names):
        tops = {path.split('/')[0] for path in names if '/romfs/' in path}
        if len(tops) != 1:
            sys.exit('no romfs/ folder in the zip')
        prefix = tops.pop() + '/'
    for entry in z.infolist():
        if entry.is_dir() or not entry.filename.startswith(prefix + 'romfs/'):
            continue
        out = os.path.join(ready, entry.filename[len(prefix):])
        os.makedirs(os.path.dirname(out), exist_ok=True)
        with z.open(entry) as s, open(out, 'wb') as d:
            shutil.copyfileobj(s, d)
if not os.path.isdir(os.path.join(ready, 'romfs')):
    sys.exit('the mod archive contains no RomFS files')
"#;

    run_logged(
        Stage::Game,
        Path::new(python),
        &[
            "-c".to_owned(),
            script.to_owned(),
            zpath.display().to_string(),
            ready.display().to_string(),
        ],
        mods_root,
        &[],
        &[],
        None,
    )?;

    std::fs::create_dir_all(ready.join(".msg-reflow-pending"))?;
    let previous = temp.path().join("previous");
    let had_previous = std::fs::symlink_metadata(&dst).is_ok();
    if had_previous {
        std::fs::rename(&dst, &previous)?;
    }
    if let Err(err) = std::fs::rename(&ready, &dst) {
        if had_previous {
            let _ = std::fs::rename(&previous, &dst);
        }
        return Err(err.into());
    }
    let out = mods_root.parent().unwrap_or(mods_root);
    crate::msg::finish_pending(romfs, out, python).map_err(|e| stage_err(Stage::Game, e))?;
    Ok(())
}

/// Installs or removes opted-in fan mods (`ptbr`) and explicit `--mod <name>=<zip>` archives.
pub fn install_mods(
    out: &Path,
    romfs: &Path,
    game_src: &Path,
    root: &Path,
    ptbr_enabled: bool,
    mods: &[(&str, &Path)],
    python: &str,
) -> Result<()> {
    let mods_root = out.join("mods");
    if crate::platform::is_dir_link(&mods_root) {
        if ptbr_enabled || !mods.is_empty() {
            return Err(Error::Build(
                "mod output must be a regular directory".into(),
            ));
        }
        return Ok(());
    }
    let ptbr = mods_root.join("ptbr");
    let explicit_ptbr = mods.iter().any(|(name, _)| *name == "ptbr");
    let mut to_install: Vec<(String, PathBuf)> = Vec::new();
    if ptbr_enabled && !explicit_ptbr {
        let mut found = Vec::new();
        for dir in [game_src.to_path_buf(), root.join("private/translations")] {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_lowercase();
                    if name.ends_with(".zip") && name.contains("ptbr") && path.is_file() {
                        found.push(path);
                    }
                }
            }
        }
        let have = ptbr.join("romfs");
        if found.is_empty() && !have.is_dir() {
            return Err(Error::Build(
                "pt-BR is enabled but no ptbr zip was found in the game folder or private/translations"
                    .into(),
            ));
        }
        if let Some(newest) = found
            .into_iter()
            .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        {
            if !have.is_dir() || mtime_newer(&newest, &have) {
                to_install.push(("ptbr".to_owned(), newest));
            }
        }
    } else if !ptbr_enabled && !explicit_ptbr && std::fs::symlink_metadata(&ptbr).is_ok() {
        if crate::platform::is_dir_link(&ptbr) {
            crate::platform::unlink_dir(&ptbr)?;
        } else {
            std::fs::remove_dir_all(&ptbr)?;
        }
    }
    for (name, zpath) in mods {
        if !mod_component(name) || !zpath.is_file() {
            return Err(Error::Build("--mod wants <name>=<zip>".into()));
        }
        to_install.push(((*name).to_owned(), zpath.to_path_buf()));
    }
    for (name, zpath) in to_install {
        install_mod_archive(&mods_root, &name, &zpath, romfs, python)?;
    }
    Ok(())
}

/// Finds a player-supplied SpeedFix patch without using a stale output patch after a game change.
pub fn speedfix_patch(
    game: &Path,
    output: &Path,
    version: &str,
    source_changed: bool,
) -> Option<PathBuf> {
    let patch_name = if version == "3.0.2" {
        "speedfix_main_302.pchtxt"
    } else {
        "speedfix_main.pchtxt"
    };
    let mut candidates = Vec::new();
    if !game.as_os_str().is_empty() {
        candidates.push(game.join(patch_name));
        candidates.push(game.join("exefs").join(patch_name));
    }
    let output_patch = output.join("speedfix_main.pchtxt");
    let stale_marker = output.join(".speedfix-old-patch");
    let stale = std::fs::metadata(&output_patch)
        .ok()
        .and_then(|stat| {
            let mtime = stat
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_nanos();
            let fp = format!("{mtime} {}", stat.len());
            let recorded = std::fs::read_to_string(&stale_marker).ok()?;
            Some(recorded.trim() == fp)
        })
        .unwrap_or(false);
    if !source_changed && !stale {
        candidates.push(output_patch);
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Returns `["--variant", <patch>]` when SpeedFix is enabled for `main` on 3.0.2 or 3.0.3.
pub fn speedfix_args(
    enabled: bool,
    module: &str,
    version: &str,
    game: &Path,
    output: &Path,
    source_changed: bool,
) -> Result<Vec<String>> {
    if !enabled || module != "main" || !matches!(version, "3.0.2" | "3.0.3") {
        return Ok(Vec::new());
    }
    let patch = speedfix_patch(game, output, version, source_changed).ok_or_else(|| {
        Error::Build("SpeedFix is enabled but no patch for this game version was found".into())
    })?;
    Ok(vec!["--variant".to_owned(), patch.display().to_string()])
}

/// The Rust game stage: links ExeFS/RomFS/AOC, extracts flat NSO images (`nso2bin`), and
/// installs/removes opted-in fan mods.
pub fn rust_game(req: &Request, root: &Path) -> Result<()> {
    let stage = Stage::Game;
    let exefs = req.out.join("exefs");
    let romfs = req.out.join("extract/romfs");
    let aoc = req.out.join("aoc");
    let src_file = req.out.join("game_source.txt");
    let old = std::fs::read_to_string(&src_file)
        .ok()
        .map(|s| s.trim().to_owned())
        .unwrap_or_default();
    let real_exefs = exefs.is_dir() && !crate::platform::is_dir_link(&exefs);
    let source_changed =
        (!old.is_empty() && Path::new(&old) != req.game.root) || (old.is_empty() && real_exefs);
    if source_changed {
        if real_exefs {
            return Err(stage_err(
                stage,
                format!(
                    "{} holds a game imported into it: give the game folder its own --out folder",
                    req.out.display()
                ),
            ));
        }
        let old_patch = req.out.join("speedfix_main.pchtxt");
        if let Ok(stat) = std::fs::metadata(&old_patch) {
            if let Ok(mtime) = stat.modified().and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .map_err(std::io::Error::other)
            }) {
                let _ = crate::platform::atomic_write(
                    &req.out.join(".speedfix-old-patch"),
                    format!("{} {}\n", mtime.as_nanos(), stat.len()).as_bytes(),
                );
            }
        }
        for d in ["analysis", "gen", "spirv", "shaders_extracted"] {
            let _ = std::fs::remove_dir_all(req.out.join(d));
        }
        let _ = std::fs::remove_file(req.out.join("version.ini"));
    }

    crate::platform::link_dir(&req.game.exefs, &exefs)?;
    crate::platform::link_dir(&req.game.romfs, &romfs)?;
    let game_aoc = req.game.root.join("aoc");
    if !req.game.aoc.is_empty() && game_aoc.is_dir() {
        crate::platform::link_dir(&game_aoc, &aoc)?;
    } else if crate::platform::is_dir_link(&aoc) {
        crate::platform::unlink_dir(&aoc)?;
    }
    crate::platform::atomic_write(
        &src_file,
        format!("{}\n", req.game.root.display()).as_bytes(),
    )?;

    let analysis = req.out.join("analysis");
    std::fs::create_dir_all(&analysis)?;
    for m in ["main", "sdk", "subsdk0"] {
        let nso = exefs.join(m);
        let out_bin = analysis.join(format!("{m}.bin"));
        if !out_bin.is_file() || mtime_newer(&nso, &out_bin) {
            crate::nso2bin::write(&nso, &out_bin)?;
        }
    }

    install_mods(
        &req.out,
        &romfs,
        &req.game.root,
        root,
        req.fan_mods.ptbr,
        &[],
        script_python(req.target),
    )?;
    Ok(())
}

/// The Rust version stage: reads `exefs/main`'s build id, matches `games/acnh/versions.txt`,
/// and writes `<out>/version.ini`.
pub fn rust_version(req: &Request, root: &Path) -> Result<()> {
    let exefs = req.out.join("exefs");
    let bid = crate::nso::read_build_id(&exefs.join("main"))?;
    let mut known: Option<(String, String)> = None;
    if let Ok(table) = std::fs::read_to_string(root.join("games/acnh/versions.txt")) {
        for line in table.lines() {
            let code = line.split('#').next().unwrap_or("");
            let mut parts = code.split_whitespace();
            if let (Some(id), Some(ver), Some(hooks)) = (parts.next(), parts.next(), parts.next()) {
                if id.eq_ignore_ascii_case(&bid) {
                    known = Some((ver.to_owned(), hooks.to_owned()));
                    break;
                }
            }
        }
    }
    let game_ini = std::fs::canonicalize(&exefs)
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("game.ini")))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| Config::parse(&s).ok());
    let tv = game_ini
        .as_ref()
        .filter(|c| {
            c.get("build_id")
                .is_none_or(|b| b.eq_ignore_ascii_case(&bid))
        })
        .and_then(|c| c.get("title_version"))
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(-1);

    let (version, hooks) = match known {
        Some(pair) => pair,
        None => {
            let ver = match crate::version::from_build_id(&bid) {
                Ok(v) => v.name().to_owned(),
                Err(_) if tv == 0 => "1.0.0".to_owned(),
                Err(_) if tv == (29 << 16) => "2.0.7".to_owned(),
                Err(_) if tv == (33 << 16) => "3.0.2".to_owned(),
                Err(_) if tv > 0 => format!("release {}", tv >> 16),
                Err(_) => "unknown".to_owned(),
            };
            (ver, "-".to_owned())
        }
    };

    let content = format!(
        "# Written by acnh_core::prepare: the version this folder holds and its build profile.\n\
         build_id = {bid}\n\
         version = {version}\n\
         title_version = {tv}\n\
         hooks = {hooks}\n"
    );
    crate::platform::atomic_write(&req.out.join("version.ini"), content.as_bytes())?;
    Ok(())
}
