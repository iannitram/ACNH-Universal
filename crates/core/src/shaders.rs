//! Shader extraction (issue 1.29): the Rust replacement of the driver in
//! `tools/extract_shaders.py`. It finds one group per shader container in the game's RomFS,
//! extracts each container into its own folder, drops the groups that held no shader, and
//! adds the blocks embedded in `main` (`exefs`) and the variations only models carry
//! (`model`), so the SPIR-V harness sees the same tree and writes the same `summary.txt`.
//!
//! The Maxwell formats themselves (BNSH, FSHA, VFXB, BAHS, the sample pair) are still read by
//! the tools under `tools/maxwell/`; this module is the stage driver, as upstream's script
//! was, and calls them with the same arguments and logs. Its per-container work is behind
//! [`Tools`], so the grouping and the cleanup can be tested without a game folder.

use crate::error::{Error, Result};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;

/// One container to extract: its group name (`shader_Park_UBER_All`, `system_GSys_...`) and
/// the file it comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub path: PathBuf,
}

/// What a run left: folders and `.code.bin` blocks under the output, and the groups that
/// failed with something other than "no shader container recognised".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub groups: usize,
    pub blocks: usize,
    pub failed: usize,
}

/// Where the tools that read the containers live: the repo root and the Python interpreter.
#[derive(Debug, Clone)]
pub struct Tools {
    pub root: PathBuf,
    pub python: String,
}

impl Default for Tools {
    fn default() -> Tools {
        Tools {
            root: repo_root(),
            python: if cfg!(windows) { "python" } else { "python3" }.to_owned(),
        }
    }
}

impl Tools {
    /// Tools rooted at `root` and run with `python`.
    pub fn new(root: PathBuf, python: String) -> Tools {
        Tools { root, python }
    }
}

/// The groups of `romfs` in upstream's order: `Shader/`, `Effect/`, everything under
/// `System/` (one group per file, named by its folder), `Swkbd/shader/*_Nx.arc.szs`,
/// `Layout/`, and, with `full`, `Model/`.
pub fn plan(romfs: &Path, full: bool) -> Result<Vec<Group>> {
    let mut groups = Vec::new();

    for path in files(&romfs.join("Shader"))? {
        groups.push(Group {
            name: format!("shader_{}", stem(&path)),
            path,
        });
    }
    for path in files(&romfs.join("Effect"))? {
        groups.push(Group {
            name: format!("effect_{}", stem(&path)),
            path,
        });
    }
    for (dir, names) in system_files(&romfs.join("System"))? {
        let folder = dir
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_default()
            .to_owned();
        for path in names {
            groups.push(Group {
                name: format!("system_{folder}_{}", stem(&path)),
                path,
            });
        }
    }
    for path in files(&romfs.join("Swkbd").join("shader"))? {
        let wanted = path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.ends_with("_Nx.arc.szs"));
        if wanted {
            groups.push(Group {
                name: format!("swkbd_{}", stem(&path)),
                path,
            });
        }
    }
    for path in files(&romfs.join("Layout"))? {
        groups.push(Group {
            name: format!("layout_{}", stem(&path)),
            path,
        });
    }
    if full {
        for path in files(&romfs.join("Model"))? {
            groups.push(Group {
                name: format!("model_{}", stem(&path)),
                path,
            });
        }
    }
    Ok(groups)
}

/// Extract every group of `romfs` into `out` (the `shaders_extracted` folder), then the
/// blocks in `main` (`exefs`) and the variations only models carry (`model`).
pub fn extract(romfs: &Path, main: &Path, out: &Path, jobs: u32, full: bool) -> Result<Summary> {
    extract_with(&Tools::default(), romfs, main, out, jobs, full)
}

/// [`extract`] with the container tools given (`root` and `python`).
pub fn extract_with(
    tools: &Tools,
    romfs: &Path,
    main: &Path,
    out: &Path,
    jobs: u32,
    full: bool,
) -> Result<Summary> {
    if !romfs.is_dir() {
        return Err(Error::BadGameFolder(format!(
            "no {}: import the game first",
            romfs.display()
        )));
    }
    fs::create_dir_all(out)?;

    let groups = plan(romfs, full)?;
    let container_workers = workers(jobs, groups.len());
    let next = AtomicUsize::new(0);
    let collected = Mutex::new(Vec::new());

    thread::scope(|scope| {
        for _ in 0..container_workers {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(group) = groups.get(index) else {
                    break;
                };
                let result = run_container(tools, group, out);
                collected
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push((index, group.name.clone(), result));
            });
        }
    });

    let mut results = collected
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    results.sort_by_key(|(index, _, _)| *index);

    let mut failed = 0;
    for (_, name, result) in results {
        match result {
            Ok(true) => {}
            Ok(false) => {
                if !log_has(
                    &out.join(format!("{name}.log")),
                    "no shader container recognised",
                ) {
                    report(&name, out, &mut failed);
                }
            }
            Err(error) => {
                failed += 1;
                println!("FAIL {name}: {error}");
            }
        }
    }

    cleanup(out)?;

    let exefs = out.join("exefs");
    let exefs_log = out.join("exefs.log");
    let exefs_args = [main.as_os_str(), OsStr::new("--extract"), exefs.as_os_str()];
    match run_script(
        tools,
        "tools/maxwell/exefs_shaders.py",
        &exefs_args,
        &exefs_log,
    ) {
        Ok(true) => {}
        Ok(false) => report("exefs", out, &mut failed),
        Err(error) => {
            failed += 1;
            println!("FAIL exefs: {error}");
        }
    }

    let model_log = out.join("model.log");
    let models = romfs.join("Model");
    let model_workers = workers(jobs, usize::MAX / 2).to_string();
    let model_args = [
        models.as_os_str(),
        out.as_os_str(),
        OsStr::new("--jobs"),
        OsStr::new(model_workers.as_str()),
    ];
    match run_script(
        tools,
        "tools/maxwell/model_shaders.py",
        &model_args,
        &model_log,
    ) {
        Ok(true) => {}
        Ok(false) => report("model", out, &mut failed),
        Err(error) => {
            failed += 1;
            println!("FAIL model: {error}");
        }
    }

    let groups = count_dirs(out)?;
    let blocks = count_blocks(out)?;
    println!("groups: {groups}  blocks: {blocks}");
    Ok(Summary {
        groups,
        blocks,
        failed,
    })
}

/// Remove the groups that held no shader (an empty folder, or one with only `manifest.tsv`)
/// and the logs of groups that left no folder, as upstream's script does.
pub fn cleanup(out: &Path) -> Result<()> {
    if !out.is_dir() {
        return Ok(());
    }

    let mut dirs = Vec::new();
    let mut logs = Vec::new();
    for entry in fs::read_dir(out)? {
        let path = entry?.path();
        if path.is_dir() {
            dirs.push(path);
        } else if path.extension() == Some(OsStr::new("log")) {
            logs.push(path);
        }
    }

    for dir in dirs {
        let mut only_manifest = true;
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.file_name() != Some(OsStr::new("manifest.tsv")) {
                only_manifest = false;
                break;
            }
        }
        if only_manifest {
            fs::remove_dir_all(&dir)?;
        }
    }

    for log in logs {
        if !log.with_extension("").is_dir() {
            fs::remove_file(log)?;
        }
    }
    Ok(())
}

/// Translate the extracted shaders into `out/spirv` with the .NET harness the tools stage
/// builds. `Ok(false)` when no harness or dotnet is installed (the caller can fall back to
/// upstream's script); `Err` when the translator ran and failed.
pub fn translate(out: &Path, jobs: u32) -> Result<bool> {
    let Some((dotnet, harness)) = translator() else {
        return Ok(false);
    };
    let extracted = out.join("shaders_extracted");
    let logs = out.join("logs");
    fs::create_dir_all(&logs)?;
    let log_path = logs.join("spirv.log");
    let log = fs::File::create(&log_path)?;
    let stderr = log.try_clone().ok();

    let workers = workers(jobs, usize::MAX / 2).to_string();
    let mut command = Command::new(dotnet);
    command
        .arg(harness)
        .arg("--batch")
        .arg(&extracted)
        .arg("--out")
        .arg(out.join("spirv"))
        .arg("--jobs")
        .arg(workers)
        .stdout(Stdio::from(log));
    if let Some(stderr) = stderr {
        command.stderr(Stdio::from(stderr));
    }
    if let Some(val) = find_program("spirv-val") {
        command.arg("--spirv-val").arg(val);
    } else {
        command.arg("--no-val");
    }

    let status = command
        .status()
        .map_err(|error| Error::Io(format!("cannot run the shader translator: {error}")))?;
    if status.success() {
        Ok(true)
    } else {
        Err(Error::Build(format!(
            "the shader translator failed (log: {})",
            log_path.display()
        )))
    }
}

fn run_container(tools: &Tools, group: &Group, out: &Path) -> Result<bool> {
    let log_path = out.join(format!("{}.log", group.name));
    let log = fs::File::create(&log_path)?;
    let stderr = log.try_clone().ok();
    let mut command = Command::new(&tools.python);
    command
        .arg(tools.root.join("tools/maxwell/containers.py"))
        .arg(&group.path)
        .arg("--extract")
        .arg(out.join(&group.name))
        .arg("--unique")
        .stdout(Stdio::from(log));
    if let Some(stderr) = stderr {
        command.stderr(Stdio::from(stderr));
    }
    let status = command
        .status()
        .map_err(|error| Error::Io(format!("cannot run {}: {error}", tools.python)))?;
    Ok(status.success())
}

fn run_script(tools: &Tools, script: &str, args: &[&OsStr], log_path: &Path) -> Result<bool> {
    let log = fs::File::create(log_path)?;
    let stderr = log.try_clone().ok();
    let mut command = Command::new(&tools.python);
    command
        .arg(tools.root.join(script))
        .args(args)
        .stdout(Stdio::from(log));
    if let Some(stderr) = stderr {
        command.stderr(Stdio::from(stderr));
    }
    let status = command
        .status()
        .map_err(|error| Error::Io(format!("cannot run {}: {error}", tools.python)))?;
    Ok(status.success())
}

fn report(name: &str, out: &Path, failed: &mut usize) {
    *failed += 1;
    println!("FAIL {name}");
    if *failed > 3 {
        return;
    }
    let Ok(text) = fs::read_to_string(out.join(format!("{name}.log"))) else {
        return;
    };
    let text = text.trim();
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(6);
    for line in &lines[start..] {
        println!("    {line}");
    }
}

fn log_has(path: &Path, needle: &str) -> bool {
    fs::read_to_string(path)
        .map(|text| text.contains(needle))
        .unwrap_or(false)
}

fn workers(jobs: u32, groups: usize) -> usize {
    let wanted = if jobs > 0 {
        jobs as usize
    } else {
        thread::available_parallelism()
            .map(|count| count.get() / 2)
            .unwrap_or(1)
            .max(1)
    };
    wanted.max(1).min(groups.max(1))
}

fn count_dirs(out: &Path) -> Result<usize> {
    let mut count = 0;
    for entry in fs::read_dir(out)? {
        if entry?.path().is_dir() {
            count += 1;
        }
    }
    Ok(count)
}

fn count_blocks(dir: &Path) -> Result<usize> {
    let mut count = 0;
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            count += count_blocks(&path)?;
        } else if let Some(name) = path.file_name().and_then(OsStr::to_str) {
            if name.ends_with(".code.bin") || (name.starts_with("code_") && name.ends_with(".bin"))
            {
                count += 1;
            }
        }
    }
    Ok(count)
}

fn files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        paths.push(entry?.path());
    }
    paths.sort();
    Ok(paths)
}

fn system_files(dir: &Path) -> Result<Vec<(PathBuf, Vec<PathBuf>)>> {
    let mut dirs = Vec::new();
    collect_system(dir, &mut dirs)?;
    dirs.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(dirs)
}

fn collect_system(dir: &Path, out: &mut Vec<(PathBuf, Vec<PathBuf>)>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_system(&path, out)?;
        } else {
            names.push(path);
        }
    }
    names.sort();
    out.push((dir.to_path_buf(), names));
    Ok(())
}

fn stem(path: &Path) -> String {
    path.file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .split('.')
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn translator() -> Option<(PathBuf, PathBuf)> {
    let root = repo_root();
    let name = if cfg!(windows) {
        "dotnet.exe"
    } else {
        "dotnet"
    };
    let private = root
        .join("private")
        .join("third_party")
        .join("dotnet")
        .join(name);
    let dotnet = if private.is_file() {
        private
    } else {
        find_program("dotnet")?
    };
    let candidates = [
        root.join("private")
            .join("build")
            .join("spirv_harness")
            .join("bin")
            .join("Release")
            .join("net10.0")
            .join("spirv_harness.dll"),
        root.join("private")
            .join("build")
            .join("spirv_harness")
            .join("out")
            .join("spirv_harness.dll"),
    ];
    let harness = candidates.into_iter().find(|path| path.is_file())?;
    Some((dotnet, harness))
}

fn find_program(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        if cfg!(windows) {
            let candidate = dir.join(format!("{name}.exe"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
