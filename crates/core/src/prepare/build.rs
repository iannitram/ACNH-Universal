//! The build stage: the runtime player, with the recompiled code in it.

use super::*;

pub(super) fn find_build_tool(root: &Path, name: &str) -> PathBuf {
    for dir in build_tool_dirs(root) {
        let fetched = dir.join(executable(Os::current(), name));
        if fetched.is_file() {
            return fetched;
        }
    }
    if let Some(on_path) = find_on_path(name) {
        return on_path;
    }
    for dir in ["/opt/homebrew/bin", "/usr/local/bin"] {
        let candidate = Path::new(dir).join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(name)
}

pub(super) fn brew_prefix() -> String {
    let brew = find_on_path("brew").or_else(|| {
        ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"]
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
    });
    brew.and_then(|exe| {
        Command::new(exe)
            .arg("--prefix")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
    })
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| "/opt/homebrew".to_owned())
}

/// Builds `libacnh_runtime` (release, host target) into `<out>/runtime-target` and returns the
/// static library and the folder holding the `acnh_runtime.h` cbindgen generated for it.
pub(super) fn build_runtime(
    req: &Request,
    root: &Path,
    env: &[(String, String)],
    logs: &Path,
) -> Result<(PathBuf, PathBuf)> {
    let stage = Stage::Build;
    let target = req.out.join("runtime-target");
    let cargo = find_cargo(root);
    // A user's `build.build-dir` would move the build scripts' out dirs away from the target dir.
    let mut cargo_env = env.to_vec();
    cargo_env.push((
        "CARGO_BUILD_BUILD_DIR".to_owned(),
        target.display().to_string(),
    ));
    run_logged(
        stage,
        &cargo,
        &[
            "build".to_owned(),
            "-p".to_owned(),
            "acnh-runtime".to_owned(),
            "--release".to_owned(),
            "--target-dir".to_owned(),
            target.display().to_string(),
        ],
        root,
        &cargo_env,
        &[],
        Some(&logs.join("runtime.log")),
    )?;
    let release = target.join("release");
    let lib = release.join(if cfg!(windows) {
        "acnh_runtime.lib"
    } else {
        "libacnh_runtime.a"
    });
    if !lib.is_file() {
        return Err(stage_err(stage, format!("{} was not built", lib.display())));
    }
    // Cargo's build-script out dir is `build/<package>/<hash>/out` (newer cargo) or
    // `build/<package>-<hash>/out` (older): search for the header under any `acnh-runtime*`
    // directory, and take the newest if there are several.
    let header = find_runtime_header(&release.join("build"));
    let include = header
        .and_then(|h| h.parent().map(Path::to_path_buf))
        .ok_or_else(|| stage_err(stage, "cbindgen did not generate acnh_runtime.h".to_owned()))?;
    Ok((lib, include))
}

/// The newest `<dir>/**/acnh-runtime*/**/out/include/acnh_runtime.h`.
pub(super) fn find_runtime_header(dir: &Path) -> Option<PathBuf> {
    fn walk(dir: &Path, depth: u32, in_runtime: bool, found: &mut Vec<PathBuf>) {
        if depth > 4 {
            return;
        }
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let runtime = in_runtime
                || entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("acnh-runtime");
            if runtime {
                let header = path.join("out/include/acnh_runtime.h");
                if header.is_file() {
                    found.push(header);
                    continue;
                }
            }
            walk(&path, depth + 1, runtime, found);
        }
    }
    let mut found = Vec::new();
    walk(dir, 0, false, &mut found);
    found
        .into_iter()
        .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
}

/// `cargo`: the build tools' search, then the rustup home, which is not always on a GUI's PATH.
pub(super) fn find_cargo(root: &Path) -> PathBuf {
    let found = find_build_tool(root, "cargo");
    if found.is_file() {
        return found;
    }
    let name = executable(Os::current(), "cargo");
    let homes = [
        std::env::var_os("CARGO_HOME").map(PathBuf::from),
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(|h| PathBuf::from(h).join(".cargo")),
    ];
    homes
        .into_iter()
        .flatten()
        .map(|h| h.join("bin").join(&name))
        .find(|p| p.is_file())
        .unwrap_or(found)
}

/// Whether two paths name the same folder. CMake records a collapsed absolute path, while a
/// checkout's root keeps its `..`, and Rust's path comparison does not collapse those.
pub(super) fn same_folder(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}

/// The Rust build stage: configures CMake with Ninja and builds the `acnh` target.
pub fn rust_build(req: &Request, root: &Path, jobs: u32) -> Result<()> {
    let stage = Stage::Build;
    let t = root.join("private/third_party");
    let logs = req.out.join("logs");
    std::fs::create_dir_all(&logs)?;
    let cmake = find_build_tool(root, "cmake");
    let ninja = find_build_tool(root, "ninja");
    let vkh = t.join(format!("Vulkan-Headers-{VK_HEADERS}"));
    let build = req.out.join("build");
    let jobs = default_jobs(jobs);

    let cache = build.join("CMakeCache.txt");
    if let Ok(text) = std::fs::read_to_string(&cache) {
        if let Some(home) = text
            .lines()
            .find_map(|l| l.strip_prefix("CMAKE_HOME_DIRECTORY:"))
            .and_then(|rest| rest.split_once('='))
            .map(|(_, val)| val.trim())
        {
            if !same_folder(Path::new(home), root) {
                let _ = std::fs::remove_file(&cache);
                let _ = std::fs::remove_dir_all(build.join("CMakeFiles"));
            }
        }
    }

    let mut cmake_args = vec![
        "-S".to_owned(),
        ".".to_owned(),
        "-B".to_owned(),
        build.display().to_string(),
        "-G".to_owned(),
        "Ninja".to_owned(),
        format!("-DCMAKE_MAKE_PROGRAM={}", ninja.display()),
        "-DNX_GAME=acnh".to_owned(),
        "-DCMAKE_BUILD_TYPE=Release".to_owned(),
        format!("-DNX_VULKAN_INCLUDE={}", vkh.join("include").display()),
        format!("-DNX_GEN_DIR={}", req.out.join("gen").display()),
    ];

    let env = if cfg!(windows) {
        cmake_args.push("-DCMAKE_C_COMPILER=clang-cl".to_owned());
        cmake_args.push("-DCMAKE_CXX_COMPILER=clang-cl".to_owned());
        let sdl = t.join(format!("SDL3-{SDL3}"));
        let movie = windows_movie_libs(&t);
        cmake_args.push(format!(
            "-DCMAKE_PREFIX_PATH={};{}",
            sdl.display(),
            movie.display()
        ));
        msvc_env(stage, root, &t)?
    } else {
        if cfg!(target_os = "macos") {
            cmake_args.push(format!("-DCMAKE_PREFIX_PATH={}", brew_prefix()));
        }
        vec![
            ("CC".to_owned(), "clang".to_owned()),
            ("CXX".to_owned(), "clang++".to_owned()),
        ]
    };

    // The C++ player needs the Rust runtime (config, saves, files, kernel ...): build it for the
    // host, then hand CMake the archive and cbindgen's header, as tools/build_ios.sh does.
    let (runtime_lib, runtime_include) = build_runtime(req, root, &env, &logs)?;
    cmake_args.push(format!("-DNX_RUNTIME_ARCHIVE={}", runtime_lib.display()));
    cmake_args.push(format!(
        "-DNX_RUNTIME_INCLUDE={}",
        runtime_include.display()
    ));

    run_logged(
        stage,
        &cmake,
        &cmake_args,
        root,
        &env,
        &[],
        Some(&logs.join("cmake.log")),
    )?;
    run_logged(
        stage,
        &ninja,
        &[
            "-C".to_owned(),
            build.display().to_string(),
            format!("-j{jobs}"),
            "acnh".to_owned(),
        ],
        root,
        &env,
        &[],
        None,
    )?;
    Ok(())
}
