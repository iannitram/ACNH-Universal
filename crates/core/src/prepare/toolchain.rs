//! The toolchain check: what a hub is missing before a prepare starts.

use super::*;

/// A build tool a hub is missing, and how to install it. Required pieces stop a prepare
/// before its first stage; a warning names what the build loses without them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    /// The tool or package, e.g. "clang" or "molten-vk".
    pub what: String,
    /// A plain message naming what is missing and the command that installs it.
    pub message: String,
    /// The command that installs it (also part of `message`).
    pub install: String,
    /// Only a warning: the build goes on, without what this piece gives it.
    pub warning: bool,
}

impl std::fmt::Display for Missing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// The build tools `os` needs before its first stage, searched for in `path` (and, on the Mac,
/// in `homebrew`). Required pieces stop a prepare; a warning names what the build loses
/// without it. The script fetches CMake and Ninja, the Vulkan headers, .NET, and LLVM and
/// SDL3 on Windows itself, so none of those is checked here.
pub fn toolchain(os: Os, path: impl AsRef<OsStr>) -> Vec<Missing> {
    let homebrew = [PathBuf::from("/opt/homebrew"), PathBuf::from("/usr/local")];
    let mut missing = toolchain_with(os, path, &homebrew);
    // cargo is often only in the rustup home, which a GUI's PATH lacks.
    if cargo_in_homes(os) {
        missing.retain(|piece| piece.what != "cargo");
    }
    missing
}

pub(super) fn cargo_in_homes(os: Os) -> bool {
    let name = executable(os, "cargo");
    [
        std::env::var_os("CARGO_HOME").map(PathBuf::from),
        std::env::var_os(if os == Os::Windows {
            "USERPROFILE"
        } else {
            "HOME"
        })
        .map(|h| PathBuf::from(h).join(".cargo")),
    ]
    .into_iter()
    .flatten()
    .any(|h| h.join("bin").join(&name).is_file())
        // The same Homebrew places `find_build_tool` tries (`brew install rust`): an app opened
        // from the Finder has no Homebrew on its PATH, but the build stage still finds cargo there.
        || (os != Os::Windows
            && ["/opt/homebrew/bin", "/usr/local/bin"]
                .iter()
                .any(|d| Path::new(d).join(&name).is_file()))
}

pub(super) fn missing_cargo(dirs: &[PathBuf], os: Os) -> Option<Missing> {
    find_tool(dirs, os, "cargo").is_none().then(|| {
        missing_piece(
            "cargo",
            "the player links the Rust runtime it builds.",
            if os == Os::Windows {
                "winget install Rustlang.Rustup"
            } else {
                "curl https://sh.rustup.rs -sSf | sh"
            },
            false,
        )
    })
}

pub(super) fn missing_zstd(dirs: &[PathBuf], os: Os) -> Option<Missing> {
    find_tool(dirs, os, "zstd").is_none().then(|| {
        missing_piece(
            "zstd",
            "mod archives cannot be fixed and the dialogue reflow is skipped without it.",
            install_command(os, "zstd"),
            true,
        )
    })
}

/// `toolchain` with the Homebrew prefixes to try (the seam the tests use).
pub fn toolchain_with(os: Os, path: impl AsRef<OsStr>, homebrew: &[PathBuf]) -> Vec<Missing> {
    let dirs: Vec<PathBuf> = std::env::split_paths(path.as_ref())
        .filter(|dir| !dir.as_os_str().is_empty())
        .collect();
    let mut missing = Vec::new();

    if os == Os::Windows {
        if !has_msvc_tools(&dirs) {
            missing.push(missing_piece(
                "the Visual Studio C++ build tools",
                "the Windows SDK and C runtime come from them.",
                "winget install Microsoft.VisualStudio.2022.BuildTools --override \"--quiet --wait \
                 --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended\"",
                false,
            ));
        }
        if find_tool(&dirs, os, "git").is_none() {
            missing.push(missing_piece(
                "git",
                "without it the script builds no libvpx and the game's movies are black.",
                "winget install Git.Git",
                true,
            ));
        }
        if find_tool(&dirs, os, "python").is_none() {
            missing.push(missing_piece(
                "python",
                "Prepare runs the shader tools and unpacks zip archives with it.",
                "winget install Python.Python.3.12",
                false,
            ));
        }
        missing.extend(missing_zstd(&dirs, os));
        missing.extend(missing_cargo(&dirs, os));
        return missing;
    }

    missing.extend(missing_cargo(&dirs, os));
    for (tool, why) in [
        ("clang", "the C compiler the build uses."),
        ("clang++", "the C++ compiler the build uses."),
        ("curl", "the downloader the script uses."),
    ] {
        if find_tool(&dirs, os, tool).is_none() {
            missing.push(missing_piece(tool, why, install_command(os, tool), false));
        }
    }

    match find_tool(&dirs, os, "python3") {
        Some(python) => {
            if has_venv(&python) == Some(false) {
                missing.push(missing_piece(
                    "python3's venv module",
                    "the script makes its private venv with it.",
                    install_command(os, "venv"),
                    false,
                ));
            }
        }
        None => missing.push(missing_piece(
            "python3",
            "Prepare runs the shader tools, unpacks mod zips and makes its private venv with it.",
            install_command(os, "python3"),
            false,
        )),
    }

    if find_tool(&dirs, os, "tar").is_none() {
        missing.push(missing_piece(
            "tar",
            "Prepare unpacks its downloads with it.",
            install_command(os, "tar"),
            false,
        ));
    }
    missing.extend(missing_zstd(&dirs, os));
    if let Some(python) = find_tool(&dirs, os, "python3") {
        // Only a python that runs and says no is reported: one that cannot be started is
        // left to the checks above.
        let pillow = Command::new(python)
            .args(["-c", "import PIL"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if pillow.is_ok_and(|status| !status.success()) {
            missing.push(missing_piece(
                "Pillow",
                "without it the translation's long dialogue lines are not reflowed.",
                install_command(os, "pillow"),
                true,
            ));
        }
    }
    if os == Os::Linux && find_tool(&dirs, os, "patchelf").is_none() {
        missing.push(missing_piece(
            "patchelf",
            "without it the packaged player cannot be given its library path.",
            install_command(os, "patchelf"),
            true,
        ));
    }

    if os == Os::Mac {
        if !has_xcode(&dirs) {
            missing.push(missing_piece(
                "Xcode's command-line tools",
                "they give clang and git for CMake's SPIRV-Cross fetch.",
                "xcode-select --install",
                false,
            ));
        }
        match homebrew_prefix(&dirs, homebrew) {
            Some(prefix) => {
                for package in ["sdl3", "molten-vk", "vulkan-loader"] {
                    if !formula_installed(&prefix, package) {
                        missing.push(missing_piece(
                            package,
                            "the Mac build finds it through Homebrew.",
                            &format!("brew install {package}"),
                            false,
                        ));
                    }
                }
                for (package, why) in [
                    ("libvpx", "without it the game's movies are black."),
                    (
                        "libvorbis",
                        "without it the SDK's own Vorbis decoder is used.",
                    ),
                ] {
                    if !formula_installed(&prefix, package) {
                        missing.push(missing_piece(
                            package,
                            why,
                            &format!("brew install {package}"),
                            true,
                        ));
                    }
                }
            }
            None => {
                missing.push(missing_piece(
                    "Homebrew",
                    "the Mac build takes SDL3, MoltenVK and the Vulkan loader from it.",
                    install_command(os, "brew"),
                    false,
                ));
            }
        }
    } else {
        if !has_sdl3(&dirs) {
            missing.push(missing_piece(
                "the SDL3 development files",
                "CMake asks for SDL3.",
                install_command(os, "sdl3"),
                false,
            ));
        }
        for (package, why) in [
            ("libvpx", "without it the game's movies are black."),
            (
                "libvorbis",
                "without it the SDK's own Vorbis decoder is used.",
            ),
        ] {
            if !has_library(&dirs, package) {
                missing.push(missing_piece(
                    package,
                    why,
                    install_command(os, package),
                    true,
                ));
            }
        }
    }

    missing
}

pub(super) fn missing_piece(what: &str, why: &str, install: &str, warning: bool) -> Missing {
    Missing {
        what: what.to_owned(),
        message: format!("Missing {what}: {why} Install it with: {install}"),
        install: install.to_owned(),
        warning,
    }
}

pub(super) fn executable(os: Os, name: &str) -> String {
    if os == Os::Windows {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

pub(super) fn find_tool(dirs: &[PathBuf], os: Os, name: &str) -> Option<PathBuf> {
    let file = executable(os, name);
    dirs.iter()
        .map(|dir| dir.join(&file))
        .find(|path| path.is_file())
}

pub(super) fn prefix_of(dir: &Path) -> &Path {
    if dir.file_name() == Some(OsStr::new("bin")) {
        dir.parent().unwrap_or(dir)
    } else {
        dir
    }
}

pub(super) fn homebrew_prefix(dirs: &[PathBuf], prefixes: &[PathBuf]) -> Option<PathBuf> {
    for prefix in prefixes {
        if prefix.join("bin").join("brew").is_file() {
            return Some(prefix.clone());
        }
        if prefix.file_name() == Some(OsStr::new("bin")) && prefix.join("brew").is_file() {
            return prefix.parent().map(Path::to_path_buf);
        }
    }
    for dir in dirs {
        if dir.join("brew").is_file() {
            return Some(prefix_of(dir).to_path_buf());
        }
        if dir.join("bin").join("brew").is_file() {
            return Some(dir.clone());
        }
    }
    None
}

pub(super) fn formula_installed(prefix: &Path, package: &str) -> bool {
    prefix.join("opt").join(package).exists() || prefix.join("Cellar").join(package).is_dir()
}

/// `setup_acnh.py` finds the build tools through vswhere, at its Program Files place even
/// when the Visual Studio prompt added nothing to PATH.
pub(super) fn has_msvc_tools(dirs: &[PathBuf]) -> bool {
    find_tool(dirs, Os::Windows, "vswhere").is_some()
        || std::env::var_os("ProgramFiles(x86)")
            .map(PathBuf::from)
            .is_some_and(|programs| {
                programs
                    .join("Microsoft Visual Studio/Installer/vswhere.exe")
                    .is_file()
            })
}

pub(super) fn has_xcode(dirs: &[PathBuf]) -> bool {
    find_tool(dirs, Os::Mac, "xcode-select").is_some()
        || Path::new("/usr/bin/xcode-select").is_file()
        || Path::new("/Library/Developer/CommandLineTools/usr/bin/clang").is_file()
}

/// `Some(false)` when the interpreter's standard library is found and has no `venv` module;
/// `None` when it cannot be told from the folder (a fake in a test). The `lib` folders of both
/// the path as found and the file it links to are looked in: Homebrew's `bin/python3` is a link
/// into its Cellar, and the `lib/python3.N` beside the link holds only `site-packages`.
pub(super) fn has_venv(python: &Path) -> Option<bool> {
    let mut prefixes = vec![prefix_of(python.parent()?).to_path_buf()];
    if let Some(dir) = std::fs::canonicalize(python)
        .ok()
        .and_then(|real| real.parent().map(Path::to_path_buf))
    {
        let prefix = prefix_of(&dir).to_path_buf();
        if !prefixes.contains(&prefix) {
            prefixes.push(prefix);
        }
    }
    let mut roots = Vec::new();
    for prefix in &prefixes {
        if let Ok(entries) = std::fs::read_dir(prefix.join("lib")) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("python") {
                    roots.push(entry.path());
                }
            }
        }
    }
    if roots.is_empty() {
        return None;
    }
    Some(
        roots
            .iter()
            .any(|root| root.join("venv").join("__init__.py").is_file()),
    )
}

pub(super) fn has_sdl3(dirs: &[PathBuf]) -> bool {
    dirs.iter().any(|dir| {
        let prefix = prefix_of(dir);
        prefix.join("include/SDL3/SDL.h").is_file()
            || prefix.join("lib/cmake/SDL3/SDL3Config.cmake").is_file()
            || prefix
                .join("lib")
                .join("SDL3")
                .join("SDL3Config.cmake")
                .is_file()
    })
}

pub(super) fn has_library(dirs: &[PathBuf], package: &str) -> bool {
    let module = package.trim_start_matches("lib");
    dirs.iter().any(|dir| library_in(prefix_of(dir), module))
}

pub(super) fn library_in(prefix: &Path, module: &str) -> bool {
    let lib = prefix.join("lib");
    let pkg_config = format!("{module}.pc");
    let shared = [format!("lib{module}.so"), format!("lib{module}.dylib")];
    if lib.join("pkgconfig").join(&pkg_config).is_file()
        || shared.iter().any(|file| lib.join(file).is_file())
    {
        return true;
    }
    // Debian and Ubuntu put the development files under a multiarch folder.
    let Ok(entries) = std::fs::read_dir(&lib) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let dir = entry.path();
        dir.join("pkgconfig").join(&pkg_config).is_file()
            || shared.iter().any(|file| dir.join(file).is_file())
    })
}

pub(super) fn install_command(os: Os, piece: &str) -> &'static str {
    if os == Os::Mac {
        match piece {
            "clang" | "clang++" => "xcode-select --install",
            "curl" => "brew install curl",
            "python3" => "brew install python",
            "venv" => "brew reinstall python",
            "sdl3" => "brew install sdl3",
            "molten-vk" => "brew install molten-vk",
            "vulkan-loader" => "brew install vulkan-loader",
            "libvpx" => "brew install libvpx",
            "libvorbis" => "brew install libvorbis",
            "zstd" => "brew install zstd",
            "tar" => "xcode-select --install",
            "pillow" => "python3 -m pip install pillow",
            "brew" => {
                "/bin/bash -c \"$(curl -fsSL \
                 https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\""
            }
            _ => "install it with Homebrew",
        }
    } else {
        match piece {
            "clang" | "clang++" => "sudo apt install clang",
            "curl" => "sudo apt install curl",
            "python3" => "sudo apt install python3 python3-venv",
            "venv" => "sudo apt install python3-venv",
            "sdl3" => "sudo apt install libsdl3-dev",
            "libvpx" => "sudo apt install libvpx-dev",
            "libvorbis" => "sudo apt install libvorbis-dev",
            "zstd" => "sudo apt install zstd  # SteamOS: sudo pacman -S zstd",
            "tar" => "sudo apt install tar  # SteamOS: sudo pacman -S tar",
            "pillow" => "sudo apt install python3-pil  # SteamOS: sudo pacman -S python-pillow",
            "patchelf" => "sudo apt install patchelf  # SteamOS: sudo pacman -S patchelf",
            _ => "install it with your package manager",
        }
    }
}
