//! Assemble (SPEC.md M1, M4, M11, M14; docs/adr/0004-build-hubs.md): after the build stage,
//! the player with the libraries it needs, so it runs on a device that did not build it. The
//! half of upstream's `tools/package_release_{linux,win,mac}.sh` a hub needs, without their
//! launcher parts:
//!
//! - **Linux:** `<install>/acnh` and the libraries a stock system may lack (SDL3, libvpx,
//!   libvorbis, libogg) beside it, found through `$ORIGIN`.
//! - **Windows:** `<install>/acnh.exe` with its DLLs beside it.
//! - **Mac:** `<install>/ACNH-Universal.app`, the one contained app: the launcher's own bundle
//!   (copied, or made here when the launcher runs from a checkout), with the player in
//!   `Contents/MacOS`, its Homebrew libraries, the Vulkan loader and MoltenVK in
//!   `Contents/Frameworks`, MoltenVK's driver file in `Contents/Resources`, an ad-hoc
//!   signature, and nothing in it that is not arm64 only.
//!
//! The build stage links the recompiled code into the player, so the Mac app holds game code:
//! it is never written inside a git work tree. The Linux and Windows results are what
//! `bundle::manifest` seals for a hub's clients.
//!
//! Dependencies are read from the ELF, PE and Mach-O headers here; the three things that
//! change a binary (`patchelf`, `install_name_tool`, `codesign`) are behind `Tools`.

use crate::bundle::is_forbidden;
use crate::error::{Error, Result};
use crate::platform::Os;
use crate::recipe::{output, MAC_APP};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the Mac app's libraries are named from inside its binaries.
const FRAMEWORKS: &str = "@executable_path/../Frameworks/";
/// The bundle's main executable: signed with the bundle, not on its own.
const LAUNCHER: &str = "acnh-app";
/// The libraries a stock Linux system may lack (upstream's list): the start of their names.
const LINUX_LIBS: [&str; 4] = ["libSDL3", "libvpx", "libvorbis", "libogg"];
/// The loader and the driver the Mac player loads at run time: Homebrew formula and file.
const MAC_VULKAN: [(&str, &str); 2] = [
    ("vulkan-loader", "libvulkan.1.dylib"),
    ("molten-vk", "libMoltenVK.dylib"),
];
/// DLLs that are Windows's own (upstream's list), besides `api-ms-win-*` and `ext-ms-*`.
const WINDOWS_DLLS: [&str; 16] = [
    "kernel32.dll",
    "user32.dll",
    "shell32.dll",
    "imm32.dll",
    "winmm.dll",
    "dxgi.dll",
    "ole32.dll",
    "psapi.dll",
    "advapi32.dll",
    "gdi32.dll",
    "setupapi.dll",
    "version.dll",
    "oleaut32.dll",
    "ws2_32.dll",
    "bcrypt.dll",
    "ntdll.dll",
];

/// What an assembly is made from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub os: Os,
    /// The build stage's folder (`<out>/build`), holding the bare player.
    pub build: PathBuf,
    /// Where the assembled player goes (`AppDirs::install`).
    pub install: PathBuf,
    /// Where libraries are looked for, in order. Linux: library folders, after the player's
    /// own run path and `build`. Windows: DLL folders, after `build`. Mac: Homebrew prefixes.
    pub libs: Vec<PathBuf>,
    /// Windows's own DLL folder (`System32`): a DLL found only there stays the system's.
    pub system: Option<PathBuf>,
    /// The running launcher's executable (Mac): inside a `.app`, that bundle is copied; from
    /// a checkout (`target/release/`), the bundle is made from the files beside it.
    pub launcher: PathBuf,
    /// The source the launcher was built from (Mac, from a checkout): `assets/`.
    pub source: PathBuf,
    /// The app's version (Mac, from a checkout: `Info.plist`).
    pub version: String,
}

impl Plan {
    /// The plan on this machine: its library folders, the running executable, this checkout.
    pub fn host(os: Os, build: &Path, install: &Path) -> Plan {
        let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        let folders = |list: &[&str]| list.iter().map(PathBuf::from).collect::<Vec<_>>();
        let mut libs = Vec::new();
        match os {
            Os::Linux => {
                if let Some(paths) = var("LD_LIBRARY_PATH") {
                    libs.extend(std::env::split_paths(&paths));
                }
                libs.extend(folders(&[
                    "/usr/local/lib",
                    "/usr/lib",
                    "/usr/lib64",
                    "/usr/lib/x86_64-linux-gnu",
                    "/usr/lib/aarch64-linux-gnu",
                    "/lib",
                    "/lib64",
                ]));
            }
            Os::Windows => {
                // The Visual C++ runtime, app-local as in upstream's package, when Prepare
                // runs in a Visual Studio environment; else it stays the system's.
                let redist = var("VCToolsRedistDir").map(|dir| PathBuf::from(dir).join("x64"));
                for entry in redist
                    .and_then(|dir| fs::read_dir(dir).ok())
                    .into_iter()
                    .flatten()
                {
                    let Ok(entry) = entry else { continue };
                    let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                    if name.starts_with("microsoft.vc") && name.ends_with(".crt") {
                        libs.push(entry.path());
                    }
                }
            }
            Os::Mac => {
                libs.extend(var("HOMEBREW_PREFIX").map(PathBuf::from));
                libs.extend(folders(&["/opt/homebrew", "/usr/local"]));
            }
            Os::Ios | Os::Android => {}
        }
        Plan {
            os,
            build: build.to_path_buf(),
            install: install.to_path_buf(),
            libs,
            system: var("SystemRoot").map(|root| PathBuf::from(root).join("System32")),
            launcher: std::env::current_exe().unwrap_or_default(),
            source: crate::prepare::repo_root(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

/// The outside programs an assembly runs (the seam the tests use).
pub trait Tools {
    /// `patchelf --set-rpath <rpath> <file>`.
    fn set_rpath(&mut self, file: &Path, rpath: &str) -> Result<()>;
    /// `install_name_tool [-id <id>] [-change <old> <new>]... <file>`.
    fn rename(&mut self, file: &Path, id: Option<&str>, changes: &[(String, String)])
        -> Result<()>;
    /// `codesign --force --sign - <path>`: an ad-hoc signature.
    fn sign(&mut self, path: &Path) -> Result<()>;
}

/// The real tools, from `PATH`.
pub struct HostTools;

fn run(program: &str, args: &[&str], file: &Path) -> Result<()> {
    let output = Command::new(program)
        .args(args)
        .arg(file)
        .output()
        .map_err(|error| Error::Build(format!("cannot run {program}: {error}")))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(Error::Build(format!(
        "{program} failed on {}: {}",
        file.display(),
        stderr.lines().last().unwrap_or("no message")
    )))
}

impl Tools for HostTools {
    fn set_rpath(&mut self, file: &Path, rpath: &str) -> Result<()> {
        run("patchelf", &["--set-rpath", rpath], file)
    }

    fn rename(
        &mut self,
        file: &Path,
        id: Option<&str>,
        changes: &[(String, String)],
    ) -> Result<()> {
        let mut args = Vec::new();
        if let Some(id) = id {
            args.extend(["-id", id]);
        }
        for (old, new) in changes {
            args.extend(["-change", old.as_str(), new.as_str()]);
        }
        run("install_name_tool", &args, file)
    }

    fn sign(&mut self, path: &Path) -> Result<()> {
        run("codesign", &["--force", "--sign", "-"], path)
    }
}

/// The player of `build`, self-contained in `install`, made with this machine's tools.
/// Returns what was made: `<install>/acnh`, `<install>/acnh.exe` or
/// `<install>/ACNH-Universal.app`.
pub fn assemble(os: Os, build: &Path, install: &Path) -> Result<PathBuf> {
    assemble_with(&Plan::host(os, build, install), &mut HostTools)
}

/// `assemble` for a given plan and tools. iOS and Android have their own packages (stage 8,
/// ADR 0005): `Error::Unsupported`.
pub fn assemble_with(plan: &Plan, tools: &mut dyn Tools) -> Result<PathBuf> {
    match plan.os {
        Os::Linux => linux(plan, tools),
        Os::Windows => windows(plan),
        Os::Mac => mac(plan, tools),
        Os::Ios | Os::Android => Err(Error::Unsupported(format!(
            "no stage assembles a player for {} yet",
            plan.os.key()
        ))),
    }
}

/// The program a launcher starts as the player. On the Mac, the one beside the launcher's own
/// executable when there is one (the contained app), else the one in the app under `install`;
/// elsewhere `<install>/<recipe::output>`.
pub fn player(os: Os, launcher: &Path, install: &Path) -> PathBuf {
    if os != Os::Mac {
        return install.join(output(os));
    }
    let beside = launcher.parent().map(|folder| folder.join("acnh"));
    match beside {
        Some(beside) if beside.is_file() => beside,
        _ => install
            .join(MAC_APP)
            .join("Contents")
            .join("MacOS")
            .join("acnh"),
    }
}

// ---- files ---------------------------------------------------------------------------------

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Copy `from` (through any link) over `to`, and leave the copy writable: Homebrew's
/// libraries are read-only, and the tools rewrite them.
fn copy(from: &Path, to: &Path) -> Result<()> {
    if to.exists() {
        fs::remove_file(to)?;
    }
    fs::copy(from, to).map_err(|error| {
        Error::Build(format!(
            "cannot copy {} to {}: {error}",
            from.display(),
            to.display()
        ))
    })?;
    let mut permissions = fs::metadata(to)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(permissions.mode() | 0o200);
    }
    #[cfg(not(unix))]
    #[allow(clippy::permissions_set_readonly_false)] // Windows: only the read-only flag
    permissions.set_readonly(false);
    fs::set_permissions(to, permissions)?;
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            copy(&entry.path(), &target)?;
        }
    }
    Ok(())
}

/// The files of `folder`, sorted by name.
fn files_in(folder: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(folder)? {
        let path = entry?.path();
        if path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// The file called `name` (in any case) in `folder`.
fn find_in(folder: &Path, name: &str) -> Option<PathBuf> {
    files_in(folder)
        .ok()?
        .into_iter()
        .find(|path| name_of(path).eq_ignore_ascii_case(name))
}

/// The bare player the build stage left, copied into `to`.
fn copy_player(plan: &Plan, name: &str, to: &Path) -> Result<PathBuf> {
    let built = plan.build.join(name);
    if !built.is_file() {
        return Err(Error::Build(format!(
            "the build stage left no player at {}",
            built.display()
        )));
    }
    fs::create_dir_all(to)?;
    let player = to.join(name);
    copy(&built, &player)?;
    Ok(player)
}

fn unreadable(file: &Path, kind: &str) -> Error {
    Error::Build(format!("{} is not {kind}", file.display()))
}

// ---- Linux -----------------------------------------------------------------------------------

fn linux(plan: &Plan, tools: &mut dyn Tools) -> Result<PathBuf> {
    let player = copy_player(plan, "acnh", &plan.install)?;
    let (_, run_path) =
        elf_dynamic(&fs::read(&player)?).ok_or_else(|| unreadable(&player, "an ELF program"))?;
    // Where the build's linker found them: the player's run path (`$ORIGIN` is the build
    // folder), the build folder, then the system's.
    let origin = plan.build.display().to_string();
    let mut folders: Vec<PathBuf> = run_path
        .iter()
        .map(|folder| {
            folder
                .replace("${ORIGIN}", &origin)
                .replace("$ORIGIN", &origin)
        })
        .map(PathBuf::from)
        .collect();
    folders.push(plan.build.clone());
    folders.extend(plan.libs.iter().cloned());

    let mut placed = vec![player.clone()];
    let mut next = 0;
    while let Some(file) = placed.get(next).cloned() {
        next += 1;
        let (needed, _) =
            elf_dynamic(&fs::read(&file)?).ok_or_else(|| unreadable(&file, "an ELF file"))?;
        for name in needed {
            let ours = LINUX_LIBS.iter().any(|lib| name.starts_with(lib));
            let target = plan.install.join(&name);
            if !ours || is_forbidden(&name) || placed.contains(&target) {
                continue;
            }
            let found = folders
                .iter()
                .map(|folder| folder.join(&name))
                .find(|path| path.is_file())
                .ok_or_else(|| {
                    Error::Build(format!(
                        "{} needs {name}, which is in none of the library folders",
                        name_of(&file)
                    ))
                })?;
            copy(&found, &target)?;
            placed.push(target);
        }
    }
    // With nothing beside it, the player needs no run path (and the hub no patchelf).
    if placed.len() > 1 {
        for file in &placed {
            tools.set_rpath(file, "$ORIGIN")?;
        }
    }
    Ok(player)
}

// ---- Windows ---------------------------------------------------------------------------------

fn windows(plan: &Plan) -> Result<PathBuf> {
    let player = copy_player(plan, "acnh.exe", &plan.install)?;
    let mut placed = vec![player.clone()];
    // The DLLs the build put beside the player: SDL3, and those it loads by name at run time.
    for built in files_in(&plan.build)? {
        let name = name_of(&built);
        if name.to_ascii_lowercase().ends_with(".dll") && !is_forbidden(&name) {
            let target = plan.install.join(&name);
            copy(&built, &target)?;
            placed.push(target);
        }
    }
    // Each DLL they import is beside them, or Windows's own.
    let mut next = 0;
    while let Some(file) = placed.get(next).cloned() {
        next += 1;
        let imports =
            pe_imports(&fs::read(&file)?).ok_or_else(|| unreadable(&file, "a Windows binary"))?;
        for dll in imports {
            let lower = dll.to_ascii_lowercase();
            let beside = placed
                .iter()
                .any(|path| name_of(path).eq_ignore_ascii_case(&dll));
            if beside
                || lower.starts_with("api-ms-win-")
                || lower.starts_with("ext-ms-")
                || WINDOWS_DLLS.contains(&lower.as_str())
            {
                continue;
            }
            if let Some(found) = plan.libs.iter().find_map(|folder| find_in(folder, &dll)) {
                let target = plan.install.join(name_of(&found));
                copy(&found, &target)?;
                placed.push(target);
            } else if plan
                .system
                .as_deref()
                .and_then(|system| find_in(system, &dll))
                .is_none()
            {
                return Err(Error::Build(format!(
                    "{} imports {dll}, which is neither beside it in the build folder nor part of Windows",
                    name_of(&file)
                )));
            }
        }
    }
    Ok(player)
}

// ---- Mac -------------------------------------------------------------------------------------

/// The work tree `path` is in, if any: the nearest folder above it that holds `.git`.
fn git_work_tree(path: &Path) -> Option<PathBuf> {
    let path = std::path::absolute(path).ok()?;
    path.ancestors()
        .find(|folder| folder.join(".git").exists())
        .map(Path::to_path_buf)
}

/// The `.app` `launcher` runs from (`<name>.app/Contents/MacOS/<launcher>`), if it is in one.
fn enclosing_app(launcher: &Path) -> Option<&Path> {
    let macos = launcher.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension()? == "app")
        .then_some(app)
}

fn mac(plan: &Plan, tools: &mut dyn Tools) -> Result<PathBuf> {
    if let Some(tree) = git_work_tree(&plan.install) {
        return Err(Error::Build(format!(
            "the Mac app holds game code, so it is never written inside a git work tree: {} is inside {}",
            plan.install.display(),
            tree.display()
        )));
    }
    let app = plan.install.join(MAC_APP);
    let contents = app.join("Contents");
    let (macos, frameworks, resources) = (
        contents.join("MacOS"),
        contents.join("Frameworks"),
        contents.join("Resources"),
    );

    // The launcher's bundle: a copy of the one it runs from, or made here from a checkout.
    let running = enclosing_app(&plan.launcher);
    let in_place = running.is_some_and(|bundle| {
        bundle == app || fs::canonicalize(bundle).ok() == fs::canonicalize(&app).ok()
    });
    if !in_place {
        if app.exists() {
            fs::remove_dir_all(&app)?;
        }
        match running {
            Some(bundle) => copy_tree(bundle, &app)?,
            None => bundle_from_checkout(plan, &contents)?,
        }
    }
    for folder in [&macos, &resources] {
        fs::create_dir_all(folder)?;
    }
    if frameworks.exists() {
        fs::remove_dir_all(&frameworks)?;
    }
    fs::create_dir_all(&frameworks)?;

    // The player beside the launcher; the Vulkan loader and MoltenVK, which it loads at run
    // time (`runtime/src/platform_mac.cpp`), with the libraries.
    copy_player(plan, "acnh", &macos)?;
    let mut placed = files_in(&macos)?;
    for (formula, name) in MAC_VULKAN {
        let found = plan
            .libs
            .iter()
            .map(|prefix| prefix.join("opt").join(formula).join("lib").join(name))
            .find(|path| path.is_file())
            .ok_or_else(|| {
                Error::Build(format!(
                    "{name} is not installed: brew install molten-vk vulkan-loader"
                ))
            })?;
        let target = frameworks.join(name);
        copy(&found, &target)?;
        placed.push(target);
    }

    // Each Homebrew library they link, and those link, into Frameworks, named from there.
    let mut next = 0;
    while let Some(file) = placed.get(next).cloned() {
        next += 1;
        let mut changes = Vec::new();
        for link in mach_links(&name_of(&file), &fs::read(&file)?)? {
            let path = Path::new(&link);
            if !plan.libs.iter().any(|prefix| path.starts_with(prefix)) {
                continue;
            }
            let target = frameworks.join(name_of(path));
            if !placed.contains(&target) {
                copy(path, &target)?;
                placed.push(target.clone());
            }
            changes.push((link.clone(), format!("{FRAMEWORKS}{}", name_of(&target))));
        }
        let id = file
            .starts_with(&frameworks)
            .then(|| format!("{FRAMEWORKS}{}", name_of(&file)));
        if id.is_some() || !changes.is_empty() {
            tools.rename(&file, id.as_deref(), &changes)?;
        }
    }

    fs::write(
        resources.join("MoltenVK_icd.json"),
        "{\n    \"file_format_version\" : \"1.0.0\",\n    \"ICD\": {\n        \"library_path\": \"../Frameworks/libMoltenVK.dylib\",\n        \"api_version\" : \"1.4.0\",\n        \"is_portability_driver\" : true\n    }\n}\n",
    )?;

    // Nothing in the app is for another processor, and nothing points outside it but at the
    // system. Then the signatures: each binary, and the bundle last (its seal covers them).
    for file in &placed {
        for link in mach_links(&name_of(file), &fs::read(file)?)? {
            let inside = link
                .strip_prefix(FRAMEWORKS)
                .is_some_and(|name| placed.contains(&frameworks.join(name)));
            if !(inside || link.starts_with("/usr/lib/") || link.starts_with("/System/")) {
                return Err(Error::Build(format!(
                    "{} links {link}, which is neither in the app nor part of macOS",
                    name_of(file)
                )));
            }
        }
    }
    for file in placed.iter().filter(|file| name_of(file) != LAUNCHER) {
        tools.sign(file)?;
    }
    tools.sign(&app)?;
    Ok(app)
}

/// The launcher's bundle when it runs from a checkout (`cargo build`: `acnh-app` and
/// `acnh-universal` bare in `target/release/`): the two executables, the icon and
/// `Info.plist` from the source's `assets/`, as `release.yml`'s Mac job lays them out.
fn bundle_from_checkout(plan: &Plan, contents: &Path) -> Result<()> {
    let beside = plan.launcher.parent().unwrap_or(Path::new(""));
    let (macos, resources) = (contents.join("MacOS"), contents.join("Resources"));
    fs::create_dir_all(&macos)?;
    fs::create_dir_all(&resources)?;
    for name in [LAUNCHER, "acnh-universal"] {
        let built = beside.join(name);
        if !built.is_file() {
            return Err(Error::Build(format!(
                "the launcher's {name} is not beside it in {}: build both (cargo build --release -p acnh-app -p acnh-cli)",
                beside.display()
            )));
        }
        copy(&built, &macos.join(name))?;
    }
    let assets = plan.source.join("assets");
    for name in ["acnhrecomp.icns", "acnhrecomp.png"] {
        copy(&assets.join(name), &resources.join(name))?;
    }
    let template = assets.join("mac").join("Info.plist.in");
    let plist = fs::read_to_string(&template)
        .map_err(|error| Error::Build(format!("cannot read {}: {error}", template.display())))?;
    fs::write(
        contents.join("Info.plist"),
        plist.replace("@VERSION@", &plan.version),
    )?;
    Ok(())
}

// ---- headers ---------------------------------------------------------------------------------

/// `len` little-endian bytes of `bytes` at `at`.
fn le(bytes: &[u8], at: usize, len: usize) -> Option<usize> {
    let part = bytes.get(at..at.checked_add(len)?)?;
    let value = part
        .iter()
        .rev()
        .fold(0u64, |value, byte| (value << 8) | u64::from(*byte));
    usize::try_from(value).ok()
}

/// The text at `at`, up to its NUL.
fn text(bytes: &[u8], at: usize) -> Option<String> {
    let rest = bytes.get(at..)?;
    let end = rest.iter().position(|byte| *byte == 0)?;
    Some(String::from_utf8_lossy(&rest[..end]).into_owned())
}

/// The libraries a Mach-O file links (its `LC_LOAD_DYLIB` commands and their weak, re-export,
/// lazy and upward kinds), after checking it is for arm64 alone: a universal (fat) file, one
/// for Intel or any other processor, or a file that is not a 64-bit Mach-O is `Error::Build`
/// naming `name` (SPEC.md M4: no Rosetta).
pub fn mach_links(name: &str, bytes: &[u8]) -> Result<Vec<String>> {
    const ARM64: usize = 0x0100_000c;
    const X86_64: usize = 0x0100_0007;
    const DYLIB_COMMANDS: [usize; 5] = [0xc, 0x8000_0018, 0x8000_001f, 0x20, 0x8000_0023];
    let refuse = |what: &str| {
        Err(Error::Build(format!(
            "{name} {what}: the Mac app is arm64 only (Apple silicon, no Rosetta)"
        )))
    };
    match bytes.get(..4) {
        Some([0xcf, 0xfa, 0xed, 0xfe]) => {}
        Some([0xca, 0xfe, 0xba, 0xbe | 0xbf]) | Some([0xbe | 0xbf, 0xba, 0xfe, 0xca]) => {
            return refuse("is a universal (fat) binary");
        }
        _ => return refuse("is not a 64-bit Mach-O binary"),
    }
    match le(bytes, 4, 4) {
        Some(ARM64) => {}
        Some(X86_64) => return refuse("is built for Intel (x86-64)"),
        _ => return refuse("is built for another processor"),
    }
    let read = || {
        let mut links = Vec::new();
        let mut at = 32usize;
        for _ in 0..le(bytes, 16, 4)? {
            let (command, size) = (le(bytes, at, 4)?, le(bytes, at + 4, 4)?);
            if DYLIB_COMMANDS.contains(&command) {
                links.push(text(bytes, at.checked_add(le(bytes, at + 8, 4)?)?)?);
            }
            at = at.checked_add(size.max(8))?;
        }
        Some(links)
    };
    read().ok_or_else(|| Error::Build(format!("{name} has a damaged Mach-O header")))
}

/// What a 64-bit little-endian ELF file's dynamic section names: the libraries it needs
/// (`DT_NEEDED`) and the folders of its run path (`DT_RUNPATH`, `DT_RPATH`). `None` when the
/// file is not one.
fn elf_dynamic(bytes: &[u8]) -> Option<(Vec<String>, Vec<String>)> {
    if bytes.get(..6)? != b"\x7fELF\x02\x01" {
        return None;
    }
    let (table, entry, count) = (
        le(bytes, 0x20, 8)?,
        le(bytes, 0x36, 2)?,
        le(bytes, 0x38, 2)?,
    );
    let mut loads = Vec::new();
    let mut dynamic = None;
    for index in 0..count {
        let at = table.checked_add(index.checked_mul(entry)?)?;
        let (offset, address, size) = (
            le(bytes, at.checked_add(8)?, 8)?,
            le(bytes, at.checked_add(16)?, 8)?,
            le(bytes, at.checked_add(32)?, 8)?,
        );
        match le(bytes, at, 4)? {
            1 => loads.push((address, offset, size)),
            2 => dynamic = Some((offset, size)),
            _ => {}
        }
    }
    let mut needed = Vec::new();
    let mut run_path = Vec::new();
    let Some((start, size)) = dynamic else {
        return Some((needed, run_path)); // linked statically: it needs nothing
    };
    let mut names = Vec::new();
    let mut strings = None;
    for index in 0..size / 16 {
        let at = start.checked_add(index * 16)?;
        let value = le(bytes, at.checked_add(8)?, 8)?;
        match le(bytes, at, 8)? {
            0 => break,
            tag @ (1 | 15 | 29) => names.push((tag, value)),
            5 => strings = Some(value),
            _ => {}
        }
    }
    // The string table's address, as an offset in the file through the segment that loads it.
    let strings = strings?;
    let strings = loads
        .iter()
        .find(|(address, _, size)| strings >= *address && strings - address < *size)
        .map(|(address, offset, _)| strings - address + offset)?;
    for (tag, value) in names {
        let name = text(bytes, strings.checked_add(value)?)?;
        if tag == 1 {
            needed.push(name);
        } else {
            run_path.extend(name.split(':').filter(|f| !f.is_empty()).map(str::to_owned));
        }
    }
    Some((needed, run_path))
}

/// The DLLs a PE file (`.exe`, `.dll`) imports, from its import table. `None` when the file
/// is not one.
fn pe_imports(bytes: &[u8]) -> Option<Vec<String>> {
    if bytes.get(..2)? != b"MZ" {
        return None;
    }
    let pe = le(bytes, 0x3c, 4)?;
    if bytes.get(pe..pe.checked_add(4)?)? != b"PE\0\0" {
        return None;
    }
    let sections = le(bytes, pe + 6, 2)?;
    let optional = pe + 24;
    let table = optional + le(bytes, pe + 20, 2)?;
    // The data directories: after 112 bytes of a 64-bit optional header, 96 of a 32-bit one.
    let directories = optional
        + match le(bytes, optional, 2)? {
            0x20b => 112,
            0x10b => 96,
            _ => return None,
        };
    let mut imports = Vec::new();
    let import_table = le(bytes, directories + 8, 4)?;
    if le(bytes, directories - 4, 4)? < 2 || import_table == 0 {
        return Some(imports);
    }
    // An address in the loaded image, as an offset in the file through its section.
    let in_file = |address: usize| {
        (0..sections).find_map(|index| {
            let at = table + index * 40;
            let (size, start, raw) = (
                le(bytes, at + 8, 4)?.max(le(bytes, at + 16, 4)?),
                le(bytes, at + 12, 4)?,
                le(bytes, at + 20, 4)?,
            );
            (address >= start && address - start < size).then(|| address - start + raw)
        })
    };
    let mut at = in_file(import_table)?;
    loop {
        let name = le(bytes, at.checked_add(12)?, 4)?;
        if name == 0 {
            return Some(imports);
        }
        imports.push(text(bytes, in_file(name)?)?);
        at = at.checked_add(20)?;
    }
}
