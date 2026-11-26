//! Prepare (SPEC.md M1, M13): from a checked game folder to a build for one device family, on
//! a hub. The Rust replacement of upstream's `tools/setup_acnh.py` and the scripts it ran
//! (`acnh_version.py`, `nso2bin.py`, `pack_romfs.py`, `pack_spirv.py`, `extract_shaders.py`,
//! `sarc_endian.py`, `msg_rewrap.py`, `make_button_font.py`, `make_button_glyphs.py`), stage by
//! stage (`recipe::Stage`), each skipped when its output is already there. Every stage except
//! `Stage::Recompile` (`python3 -m recomp` until #49) runs in Rust.
//!
//! Everything a prepare writes is made from my game folder: it stays in `out` on the hub, and
//! reaches my other devices only through the hub, over the home network (M11, M14).

pub use crate::assemble::assemble;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::gamefolder::GameFolder;
pub use crate::pack_folder::{
    app_out_dir, app_out_dir_for, pack_folder, pack_folder_with, PackContext,
};
use crate::platform::{AppDirs, Os, Vars};
use crate::recipe::{hub_for, output, serves, Hub, Stage, STAGES};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod build;
mod deliver;
mod game;
mod recompile;
mod redact;
mod runner;
mod shaders;
mod toolchain;
mod tools;

pub use build::*;
pub use deliver::*;
pub use game::*;
pub use recompile::*;
pub use redact::*;
pub use runner::*;
pub use shaders::*;
pub use toolchain::*;
pub use tools::*;

/// The file a packaged source holds at its root: the commit the tree was cut from.
const SOURCE_FILE: &str = "SOURCE";

/// Fan mod choices, off on a fresh install.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FanMods {
    pub ptbr: bool,
    pub speedfix: bool,
}

/// What a prepare is asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub game: GameFolder,
    /// The build folder (upstream's `--out`).
    pub out: PathBuf,
    pub target: Os,
    /// Compile jobs; 0 lets the stage choose (upstream: free RAM / 700 MB, at most the cores).
    pub jobs: u32,
    /// The source tree to build from. `None` finds the packaged source beside the executable,
    /// and keeps the checkout when there is none (a `cargo run`).
    pub source: Option<PathBuf>,
    /// Snapshot of the two opt-in settings for the whole prepare.
    pub fan_mods: FanMods,
    /// Where a target's final deliverable goes (`delivery_folder`): `<dropbox>/_transfers/
    /// ACNH-Universal/<ios|mac>/`. The `ios` and `mac` targets need it; the others ignore it.
    pub delivery: Option<PathBuf>,
    /// The folder the `mac` target assembles its app in (`<data>/install`); the other targets
    /// ignore it.
    pub install: Option<PathBuf>,
}

/// Where a passed tools stage is recorded: upstream keeps its tools in the repo's `private/`
/// folder, so the stage leaves nothing of its own in `out`.
fn tools_record(req: &Request) -> PathBuf {
    req.out.join(".prepare-tools.json")
}

/// Where the recompiler input hash is recorded beside `gen/`.
fn recomp_hash_file(req: &Request) -> PathBuf {
    req.out.join("gen").join(".recomp-hash")
}

/// Whether everything `stage` leaves in `req.out` is there. Game and recompile leave one
/// file per module; upstream's build leaves the bare player in `build/` (on the Mac the
/// plain executable), which `assemble` then makes self-contained in the install folder.
fn stage_complete(stage: Stage, req: &Request, expected_recomp_hash: &str) -> bool {
    const MODULES: [&str; 3] = ["main", "sdk", "subsdk0"];
    match stage {
        Stage::Tools => tools_record(req).is_file(),
        Stage::Version => req.out.join("version.ini").is_file(),
        Stage::Game => MODULES.iter().all(|module| {
            req.out
                .join("analysis")
                .join(format!("{module}.bin"))
                .is_file()
        }),
        Stage::Recompile => {
            MODULES
                .iter()
                .all(|module| req.out.join("gen").join(module).join("table.cpp").is_file())
                && std::fs::read_to_string(recomp_hash_file(req))
                    .ok()
                    .is_some_and(|recorded| recorded.trim() == expected_recomp_hash)
        }
        Stage::Shaders => req.out.join("spirv").join("summary.txt").is_file(),
        Stage::Fonts => req
            .out
            .join("fonts")
            .join("glyphs_xbox")
            .join("console")
            .join("romfs")
            .join("Font")
            .join("ScalableFont.sarc.zs")
            .is_file(),
        Stage::Build => req
            .out
            .join("build")
            .join(if matches!(req.target, Os::Mac | Os::Ios) {
                "acnh"
            } else {
                output(req.target)
            })
            .is_file(),
    }
}

/// The repo root upstream's scripts run from: the checkout this was built from (as
/// upstream's launcher falls back to its own source folder).
pub(crate) fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The packaged source beside an executable: `source/` in the Windows zip, `../Resources/source`
/// in the Mac app, and `../share/acnh-universal/source` in the AppImage.
fn packaged_source(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    [
        "source",
        "../Resources/source",
        "../share/acnh-universal/source",
    ]
    .into_iter()
    .map(|relative| dir.join(relative))
    .find(|source| source.join(SOURCE_FILE).is_file())
}

/// The commit a packaged source names, or `None` when the folder is a checkout.
fn source_commit(source: &Path) -> Option<String> {
    let text = std::fs::read_to_string(source.join(SOURCE_FILE)).ok()?;
    let commit = text.lines().next()?.trim();
    (!commit.is_empty()).then(|| commit.to_owned())
}

/// Copy a source tree, replacing the last copy of its commit.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    if to.exists() {
        std::fs::remove_dir_all(to)?;
    }
    crate::fsutil::copy_tree_skipping(from, to, &|_| false)
}

/// The folder the script runs in: the named source, else the packaged source beside `exe`,
/// else the checkout. A packaged source is copied to `<data>/source/<commit>/` because the
/// package may be read-only and the script writes into `private/`. A package from another
/// commit is refused.
fn source_root(req: &Request, exe: &Path, dirs: &AppDirs, commit: Option<&str>) -> Result<PathBuf> {
    let source = req
        .source
        .clone()
        .or_else(|| packaged_source(exe))
        .unwrap_or_else(repo_root);
    let Some(found) = source_commit(&source) else {
        return Ok(source);
    };
    let expected = commit.unwrap_or(found.as_str());
    if found.as_str() != expected {
        return Err(Error::Io(format!(
            "the source in this package is commit {found}, not {expected}: use the package \
             built from the app's own source"
        )));
    }
    let copy = dirs.data.join("source").join(&found);
    copy_tree(&source, &copy)?;
    Ok(copy)
}

/// The commit this binary was built from (the release sets `ACNH_SOURCE_COMMIT`).
fn app_commit() -> Option<&'static str> {
    option_env!("ACNH_SOURCE_COMMIT")
        .map(str::trim)
        .filter(|commit| !commit.is_empty())
}

/// Run every stage in order, calling `progress` as each one starts, and return `req.out`. A
/// stage whose output was already in `req.out` is skipped. When all outputs exist, Prepare
/// only recovers interrupted message reflow. A failed stage is `Error::Build` naming it,
/// with the last 20 lines of its log, run
/// through `redact`. Missing required tools are `Error::Build` before the first stage. A
/// target this hub does not serve (`recipe::serves`), or one upstream's script does not build
/// (Android), is `Error::Unsupported`. For the `ios` target (Mac hub, `Request::delivery`
/// set) it runs `preflight`, the shared stages, the iPhone build and the packaging, and
/// returns the `.ipa`'s path in the delivery folder instead of `Request::out`. The `mac` target
/// (`Request::install` too) does the same with the contained app and returns the `.zip`'s path.
pub fn prepare(req: &Request, progress: &mut dyn FnMut(Stage)) -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    let exe = std::env::current_exe()
        .map_err(|e| Error::Io(format!("cannot find this app's executable: {e}")))?;
    let vars = Vars::from_lookup(Os::current(), |name| std::env::var(name).ok())?;
    let dirs = AppDirs::new(&vars);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let missing = toolchain(Os::current(), &path);
    prepare_installed(
        req,
        hub_for(Os::current()),
        &exe,
        &dirs,
        app_commit(),
        &missing,
        &mut ProcessRunner::new(repo_root(), req.jobs),
        &home,
        progress,
    )
}

/// `prepare` for an executable at `exe`, the app folders `dirs`, the source's `commit` and the
/// toolchain `missing` (the seam the tests use). A packaged source is copied into `dirs` and
/// the runner runs there.
#[allow(clippy::too_many_arguments)]
pub fn prepare_installed(
    req: &Request,
    hub: Hub,
    exe: &Path,
    dirs: &AppDirs,
    commit: Option<&str>,
    missing: &[Missing],
    runner: &mut dyn ScriptRunner,
    home: &Path,
    progress: &mut dyn FnMut(Stage),
) -> Result<PathBuf> {
    let source = source_root(req, exe, dirs, commit)?;
    runner.set_root(&source);
    run_stages(req, hub, missing, runner, home, progress)
}

/// `prepare` for a given hub, script runner and home folder, with no toolchain check (the
/// seam the runner tests use).
pub fn prepare_with(
    req: &Request,
    hub: Hub,
    runner: &mut dyn ScriptRunner,
    home: &Path,
    progress: &mut dyn FnMut(Stage),
) -> Result<PathBuf> {
    run_stages(req, hub, &[], runner, home, progress)
}

fn run_stages(
    req: &Request,
    hub: Hub,
    missing: &[Missing],
    runner: &mut dyn ScriptRunner,
    home: &Path,
    progress: &mut dyn FnMut(Stage),
) -> Result<PathBuf> {
    if !serves(hub).contains(&req.target) {
        return Err(Error::Unsupported(format!(
            "the {} hub does not build for {}: that is built on the {} hub{}",
            hub.key(),
            req.target.key(),
            hub_for(req.target).key(),
            if hub_for(req.target) == Hub::Mac {
                " (the Mac)"
            } else {
                ""
            }
        )));
    }
    let required: Vec<&Missing> = missing.iter().filter(|piece| !piece.warning).collect();
    if !required.is_empty() {
        let mut message = String::from("missing build tools:");
        for piece in required {
            message.push_str("\n  ");
            message.push_str(&piece.message);
        }
        return Err(Error::Build(message));
    }
    // The steps and the work-tree check name the same, absolute folder.
    let absolute_req;
    let req = if matches!(req.target, Os::Ios | Os::Mac | Os::Android) {
        absolute_req = Request {
            out: absolute(&req.out)?,
            ..req.clone()
        };
        &absolute_req
    } else {
        req
    };
    preflight(req, runner, home)?;
    // Recover a mod left by a previous child, even when every stage is cached.
    // Its font comes from that run's linked RomFS if the caller selected a new source.
    let prepared_romfs = req.out.join("extract").join("romfs");
    let recovery_romfs = if prepared_romfs.is_dir() {
        &prepared_romfs
    } else {
        &req.game.romfs
    };
    crate::msg::finish_pending(recovery_romfs, &req.out, script_python(req.target))
        .map_err(|error| build_error(Stage::Game, &[error.to_string()], home))?;
    // What was there before anything ran. A script that runs on while we look may have
    // finished its next stage already: only an output from before skips a stage.
    let flags = format!("{} {}\n", req.fan_mods.ptbr, req.fan_mods.speedfix);
    let record = req.out.join(".fan-mod-settings");
    let changed = std::fs::read_to_string(&record).ok().as_deref() != Some(flags.as_str());
    let source_changed = std::fs::read_to_string(req.out.join("game_source.txt"))
        .ok()
        .is_some_and(|source| Path::new(source.trim()) != req.game.root);
    // Recheck opted-in inputs for changed archives and patches, and migrate old builds.
    let ptbr_present = std::fs::symlink_metadata(req.out.join("mods/ptbr")).is_ok();
    let speedfix_present = std::fs::read_to_string(req.out.join("gen/main/profile.txt"))
        .is_ok_and(|profile| profile.split_whitespace().any(|arg| arg == "--variant"));
    let recheck = changed
        || source_changed
        || req.fan_mods.ptbr
        || req.fan_mods.speedfix
        || ptbr_present
        || speedfix_present;
    let expected_recomp_hash = recomp_hash(&runner.root())?;
    let before = STAGES.map(|stage| {
        stage_complete(stage, req, &expected_recomp_hash)
            && !(recheck
                && matches!(
                    stage,
                    Stage::Game | Stage::Recompile | Stage::Shaders | Stage::Build
                ))
    });
    let mut last = None;
    for (stage, was_there) in STAGES.into_iter().zip(before) {
        if was_there && stage_complete(stage, req, &expected_recomp_hash) {
            continue;
        }
        progress(stage);
        match stage {
            Stage::Tools => runner.tools(req)?,
            Stage::Game => runner.game(req)?,
            Stage::Version => runner.version(req)?,
            Stage::Recompile => runner.recompile(req)?,
            Stage::Shaders => {
                runner
                    .before_shaders()
                    .map_err(|log| build_error(stage, &log, home))?;
                runner.shaders(req)?;
            }
            Stage::Fonts => runner.fonts(req)?,
            Stage::Build => runner.build(req)?,
        }
        if stage == Stage::Tools {
            std::fs::create_dir_all(&req.out)?;
            std::fs::write(tools_record(req), "{\"tools\": \"ready\"}\n")?;
        }
        if stage == Stage::Recompile {
            let hash_file = recomp_hash_file(req);
            if let Some(parent) = hash_file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(hash_file, format!("{expected_recomp_hash}\n"))?;
        }
        last = Some(stage);
    }
    if let Some(stage) = last {
        runner
            .finish()
            .map_err(|log| build_error(stage, &log, home))?;
    }
    std::fs::write(record, flags)?;
    match req.target {
        Os::Ios => ios_deliver(req, runner, home),
        Os::Mac => mac_deliver(req, runner, home),
        Os::Android => android_deliver(req, runner, home),
        _ => Ok(req.out.clone()),
    }
}
