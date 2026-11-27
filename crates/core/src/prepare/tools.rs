//! The tools stage: fetch and build the tools a prepare needs (pins, downloads).

use super::*;

/// `KEY=value` lines: the versions CI and the scripts read too (#328).
const TOOL_VERSIONS: &str = include_str!("../../../../tools/versions.env");

/// The value of `key` in `tools/versions.env`; a missing key stops the build at compile time.
const fn tool_version(key: &str) -> &'static str {
    let text = TOOL_VERSIONS.as_bytes();
    let key = key.as_bytes();
    let mut i = 0;
    while i < text.len() {
        let start = i;
        while i < text.len() && text[i] != b'\n' {
            i += 1;
        }
        let mut end = i;
        i += 1;
        // a Windows checkout may turn the line ends into CRLF: the value stops before the `\r`
        if end > start && text[end - 1] == b'\r' {
            end -= 1;
        }
        // a line is `KEY=value`; comment lines start with `#` and never match
        if end - start > key.len() && text[start + key.len()] == b'=' {
            let mut k = 0;
            let mut same = true;
            while k < key.len() {
                if text[start + k] != key[k] {
                    same = false;
                }
                k += 1;
            }
            if same {
                let (_, rest) = text.split_at(start + key.len() + 1);
                let (value, _) = rest.split_at(end - start - key.len() - 1);
                return match core::str::from_utf8(value) {
                    Ok(v) => v,
                    Err(_) => panic!("tools/versions.env is not UTF-8"),
                };
            }
        }
    }
    panic!("tools/versions.env lacks a key Prepare needs")
}

/// Pinned third-party versions for `Stage::Tools` and `Stage::Build`.
pub(super) const VK_HEADERS: &str = tool_version("VK_HEADERS");
pub(super) const LLVM: &str = tool_version("LLVM");
pub(super) const SDL3: &str = tool_version("SDL3");
pub(super) const IMGUI: &str = tool_version("IMGUI");
pub(super) const RYUBING: &str = tool_version("RYUBING");
pub(super) const DOTNET: &str = tool_version("DOTNET");
pub(super) const CMAKE: &str = tool_version("CMAKE");
pub(super) const NINJA: &str = tool_version("NINJA");
pub(super) const FFX: &str = tool_version("FFX");
pub(super) const DLSS: &str = tool_version("DLSS");
/// vcpkg is fetched at this commit, never at whatever `HEAD` is on the day (#328).
const VCPKG_COMMIT: &str = "e6d62cc9ce8f8d9687df14521118f2ec3c10ee6a";
/// `<sha256>  <url>` per line: every download Prepare makes must be listed here and match (#328).
/// The same file feeds CI and `tools/build_ios.sh`; `tools/check_pins.sh` guards it.
const DOWNLOAD_PINS: &str = include_str!("../../../../tools/downloads.sha256");

/// The recorded SHA-256 of `url`, if the pin table has one.
fn pinned_sha256(url: &str) -> Option<&'static str> {
    DOWNLOAD_PINS.lines().find_map(|line| {
        let (sum, u) = line.split_once("  ")?;
        (u.trim() == url).then_some(sum.trim())
    })
}

/// Where Prepare keeps the CMake it fetched (`private/third_party/cmake`).
pub(super) fn cmake_dir(root: &Path) -> PathBuf {
    root.join("private/third_party/cmake")
}

/// Where Prepare keeps the Ninja it fetched (`private/third_party/ninja`).
pub(super) fn ninja_dir(root: &Path) -> PathBuf {
    root.join("private/third_party/ninja")
}

/// The folders holding the fetched `cmake` and `ninja` (the macOS CMake keeps its binaries
/// inside the `.app`).
pub(super) fn build_tool_dirs(root: &Path) -> [PathBuf; 2] {
    let cmake = cmake_dir(root);
    let cmake = if cfg!(target_os = "macos") {
        cmake.join("CMake.app/Contents/bin")
    } else {
        cmake.join("bin")
    };
    [cmake, ninja_dir(root)]
}

/// The pinned CMake and Ninja downloads for this host: `(cmake url, ninja url)`. `None` where
/// the projects publish no binary for the host; Prepare then uses the ones on the PATH.
pub(super) fn build_tool_urls() -> Option<(String, String)> {
    let cmake = |name: &str| {
        format!("https://github.com/Kitware/CMake/releases/download/v{CMAKE}/cmake-{CMAKE}-{name}")
    };
    let ninja = |name: &str| {
        format!("https://github.com/ninja-build/ninja/releases/download/v{NINJA}/{name}")
    };
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some((cmake("linux-x86_64.tar.gz"), ninja("ninja-linux.zip"))),
        ("linux", "aarch64") => Some((
            cmake("linux-aarch64.tar.gz"),
            ninja("ninja-linux-aarch64.zip"),
        )),
        ("macos", _) => Some((cmake("macos-universal.tar.gz"), ninja("ninja-mac.zip"))),
        ("windows", "x86_64") => Some((cmake("windows-x86_64.zip"), ninja("ninja-win.zip"))),
        _ => None,
    }
}

/// Whether the fetched CMake and Ninja are in place (true where none is published).
fn build_tools_present(root: &Path) -> bool {
    if build_tool_urls().is_none() {
        return true;
    }
    let [cmake, ninja] = build_tool_dirs(root);
    cmake.join(executable(Os::current(), "cmake")).is_file()
        && ninja.join(executable(Os::current(), "ninja")).is_file()
}

pub(super) fn stage_err(stage: Stage, message: impl std::fmt::Display) -> Error {
    Error::Build(format!("stage {}: {message}", stage.key()))
}

pub(super) fn run_logged(
    stage: Stage,
    program: &Path,
    args: &[String],
    cwd: &Path,
    env: &[(String, String)],
    remove_env: &[&str],
    log_path: Option<&Path>,
) -> Result<()> {
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(cwd).stdin(Stdio::null());
    for key in remove_env {
        cmd.env_remove(key);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let output = cmd
        .output()
        .map_err(|e| stage_err(stage, format!("cannot run {}: {e}", program.display())))?;
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&stderr);
    }
    if let Some(path) = log_path {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, &combined);
    }
    if !output.status.success() {
        let lines: Vec<&str> = combined.lines().collect();
        let tail = lines[lines.len().saturating_sub(20)..].join("\n");
        let log_note = log_path
            .map(|p| format!(" (log: {})", p.display()))
            .unwrap_or_default();
        return Err(stage_err(
            stage,
            format!("failed: {}{log_note}\n{tail}", program.display()),
        ));
    }
    Ok(())
}

pub(super) fn download_file(stage: Stage, url: &str, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = dest.with_extension("download");
    let curl = if cfg!(windows) { "curl.exe" } else { "curl" };
    run_logged(
        stage,
        Path::new(curl),
        &[
            "-fsSL".to_owned(),
            "-o".to_owned(),
            tmp.display().to_string(),
            url.to_owned(),
        ],
        dest.parent().unwrap_or_else(|| Path::new(".")),
        &[],
        &[],
        None,
    )?;
    let Some(want) = pinned_sha256(url) else {
        let _ = std::fs::remove_file(&tmp);
        return Err(stage_err(
            stage,
            format!("no pinned SHA-256 for {url}; record it in tools/downloads.sha256"),
        ));
    };
    let got = crate::appgame::sha256_file(&tmp)?;
    if !got.eq_ignore_ascii_case(want) {
        let _ = std::fs::remove_file(&tmp);
        return Err(stage_err(
            stage,
            format!("SHA-256 mismatch for {url}: expected {want}, got {got}"),
        ));
    }
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

pub(super) fn fetch_archive(stage: Stage, url: &str, dest_dir: &Path, strip: bool) -> Result<()> {
    std::fs::create_dir_all(dest_dir)?;
    let tmp = PathBuf::from(format!("{}.download", dest_dir.display()));
    download_file(stage, url, &tmp)?;
    let unpacked = crate::archive::extract(crate::archive::Kind::of(url), &tmp, dest_dir, strip);
    let _ = std::fs::remove_file(&tmp);
    unpacked.map_err(|e| stage_err(stage, e))
}

pub(crate) fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let name = executable(Os::current(), program);
    std::env::split_paths(&path)
        .map(|dir| dir.join(&name))
        .find(|p| p.is_file())
}

pub(crate) fn has_dotnet_10(dotnet: &Path) -> bool {
    Command::new(dotnet)
        .arg("--list-sdks")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|line| line.starts_with("10."))
        })
}

pub(super) fn dotnet_rid() -> Option<(&'static str, &'static str)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some(("win-x64", "zip")),
        ("linux", "x86_64") => Some(("linux-x64", "tar.gz")),
        ("linux", "aarch64") => Some(("linux-arm64", "tar.gz")),
        ("macos", "aarch64") => Some(("osx-arm64", "tar.gz")),
        ("macos", "x86_64") => Some(("osx-x64", "tar.gz")),
        _ => None,
    }
}

/// The Rust tools stage: fetches CMake and Ninja,
/// Vulkan-Headers, Dear ImGui, Ryubing (and Windows SDKs/LLVM/SDL3/vcpkg), ensures .NET 10,
/// and builds `spirv_harness.dll`.
pub fn rust_tools(req: &Request, root: &Path) -> Result<()> {
    let stage = Stage::Tools;
    let p = root.join("private");
    let t = p.join("third_party");
    let logs = req.out.join("logs");
    std::fs::create_dir_all(&t)?;
    std::fs::create_dir_all(&logs)?;

    let old_stamp = read_tools_stamp(root);
    if let Some((cmake_url, ninja_url)) = build_tool_urls() {
        let [cmake_bin, _] = build_tool_dirs(root);
        if old_stamp.get("cmake").map(String::as_str) != Some(CMAKE) && cmake_dir(root).exists() {
            std::fs::remove_dir_all(cmake_dir(root))?;
        }
        if !cmake_bin.join(executable(req.target, "cmake")).is_file() {
            fetch_archive(stage, &cmake_url, &cmake_dir(root), true)?;
        }
        if old_stamp.get("ninja").map(String::as_str) != Some(NINJA) && ninja_dir(root).exists() {
            std::fs::remove_dir_all(ninja_dir(root))?;
        }
        if !ninja_dir(root)
            .join(executable(req.target, "ninja"))
            .is_file()
        {
            fetch_archive(stage, &ninja_url, &ninja_dir(root), false)?;
        }
    }

    let pins = tools_pins(root);
    let stale = |k: &str| old_stamp.get(k) != pins.get(k);

    let vkh = t.join(format!("Vulkan-Headers-{VK_HEADERS}"));
    if !vkh.is_dir() {
        fetch_archive(
            stage,
            &format!("https://github.com/KhronosGroup/Vulkan-Headers/archive/refs/tags/v{VK_HEADERS}.tar.gz"),
            &vkh,
            true,
        )?;
    }
    let imgui = t.join("imgui");
    if stale("imgui") && imgui.exists() {
        std::fs::remove_dir_all(&imgui)?;
    }
    if !imgui.join("imgui.cpp").exists() {
        fetch_archive(
            stage,
            &format!("https://github.com/ocornut/imgui/archive/refs/tags/v{IMGUI}.tar.gz"),
            &imgui,
            true,
        )?;
    }
    let ryubing = t.join("ryubing");
    if stale("ryubing") && ryubing.exists() {
        std::fs::remove_dir_all(&ryubing)?;
    }
    if !ryubing.join("Ryujinx.sln").exists() {
        fetch_archive(
            stage,
            &format!("https://codeberg.org/Software_arch/Ryubing/archive/{RYUBING}.tar.gz"),
            &ryubing,
            true,
        )?;
    }

    if cfg!(windows) {
        rust_tools_windows(req, root, &t, &logs)?;
    }

    let mut private_dotnet = false;
    let mut dotnet_env = vec![("DOTNET_CLI_TELEMETRY_OPTOUT".to_owned(), "1".to_owned())];
    let dotnet = match find_on_path("dotnet").filter(|p| has_dotnet_10(p)) {
        Some(sys) => sys,
        None => {
            let dn = t.join("dotnet");
            let exe = dn.join(executable(req.target, "dotnet"));
            private_dotnet = true;
            if old_stamp.get("dotnet").map(String::as_str) != Some(DOTNET) && dn.exists() {
                std::fs::remove_dir_all(&dn)?;
            }
            if !exe.exists() {
                let (rid, ext) = dotnet_rid().ok_or_else(|| {
                    stage_err(stage, "unsupported host architecture for .NET 10 SDK")
                })?;
                fetch_archive(
                    stage,
                    &format!("https://builds.dotnet.microsoft.com/dotnet/Sdk/{DOTNET}/dotnet-sdk-{DOTNET}-{rid}.{ext}"),
                    &dn,
                    false,
                )?;
            }
            dotnet_env.push(("DOTNET_ROOT".to_owned(), dn.display().to_string()));
            let cur_path = std::env::var_os("PATH").unwrap_or_default();
            let joined =
                std::env::join_paths(std::iter::once(dn).chain(std::env::split_paths(&cur_path)))
                    .map_err(|e| stage_err(stage, e))?;
            dotnet_env.push(("PATH".to_owned(), joined.to_string_lossy().into_owned()));
            exe
        }
    };

    let harness = p.join("build/spirv_harness/bin/Release/net10.0/spirv_harness.dll");
    let alt_dir = p.join("build/spirv_harness/out");
    let alt = alt_dir.join("spirv_harness.dll");
    if stale("harness") && p.join("build/spirv_harness").exists() {
        std::fs::remove_dir_all(p.join("build/spirv_harness"))?;
    }
    if !harness.exists() && !alt.exists() {
        let proj = "tools/maxwell/spirv_harness/SpirvHarness.csproj";
        let hlog = logs.join("harness.log");
        run_logged(
            stage,
            &dotnet,
            &[
                "build".to_owned(),
                proj.to_owned(),
                "-c".to_owned(),
                "Release".to_owned(),
                "-m:4".to_owned(),
            ],
            root,
            &dotnet_env,
            &["Platform", "PLATFORM"],
            Some(&hlog),
        )?;
        if !harness.exists() {
            run_logged(
                stage,
                &dotnet,
                &[
                    "build".to_owned(),
                    proj.to_owned(),
                    "-c".to_owned(),
                    "Release".to_owned(),
                    "-m:1".to_owned(),
                    "-o".to_owned(),
                    alt_dir.display().to_string(),
                ],
                root,
                &dotnet_env,
                &["Platform", "PLATFORM"],
                Some(&hlog),
            )?;
            if !alt.exists() {
                return Err(stage_err(
                    stage,
                    format!(
                        "the shader translator was built but spirv_harness.dll is missing (log: {})",
                        hlog.display()
                    ),
                ));
            }
        }
    }
    let mut stamp = old_stamp;
    stamp.extend(pins);
    if private_dotnet {
        stamp.insert("dotnet".to_owned(), DOTNET.to_owned());
    }
    let text: String = stamp.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    std::fs::write(tools_stamp_file(root), text)?;
    Ok(())
}

pub(super) fn rust_tools_windows(_req: &Request, root: &Path, t: &Path, logs: &Path) -> Result<()> {
    let stage = Stage::Tools;
    let base = format!("https://raw.githubusercontent.com/GPUOpen-LibrariesAndSDKs/FidelityFX-SDK/{FFX}/Kits/FidelityFX");
    let nv = format!("https://raw.githubusercontent.com/NVIDIA/DLSS/{DLSS}");
    let mut files = Vec::new();
    for h in [
        "api/include/ffx_api.h",
        "api/include/ffx_api_types.h",
        "api/include/vk/ffx_api_vk.h",
        "upscalers/include/ffx_upscale.h",
        "framegeneration/include/ffx_framegeneration.h",
        "framegeneration/include/ffx_framegeneration_api_types.h",
        "framegeneration/include/vk/ffx_api_framegeneration_vk.h",
    ] {
        files.push(("ffx", h.to_owned(), format!("{base}/{h}")));
    }
    files.push((
        "ffx",
        "LICENSE.md".to_owned(),
        format!("{base}/docs/license.md"),
    ));
    for d in [
        "amd_fidelityfx_loader_vk.dll",
        "amd_fidelityfx_upscaler_vk.dll",
        "amd_fidelityfx_framegeneration_vk.dll",
    ] {
        files.push(("ffx", format!("bin/{d}"), format!("{base}/signedbin/{d}")));
    }
    for h in [
        "nvsdk_ngx.h",
        "nvsdk_ngx_defs.h",
        "nvsdk_ngx_defs_vk.h",
        "nvsdk_ngx_helpers.h",
        "nvsdk_ngx_helpers_vk.h",
        "nvsdk_ngx_params.h",
        "nvsdk_ngx_vk.h",
    ] {
        files.push(("dlss", format!("include/{h}"), format!("{nv}/include/{h}")));
    }
    files.push((
        "dlss",
        "LICENSE.txt".to_owned(),
        format!("{nv}/LICENSE.txt"),
    ));
    files.push((
        "dlss",
        "lib/nvsdk_ngx_d.lib".to_owned(),
        format!("{nv}/lib/Windows_x86_64/x64/nvsdk_ngx_d.lib"),
    ));
    files.push((
        "dlss",
        "bin/nvngx_dlss.dll".to_owned(),
        format!("{nv}/lib/Windows_x86_64/rel/nvngx_dlss.dll"),
    ));
    for (top, rel, url) in files {
        let out = t.join(top).join(rel);
        if !out.exists() {
            download_file(stage, &url, &out)?;
        }
    }
    let llvm = t.join(format!("llvm-{LLVM}"));
    if !llvm.join("bin/clang-cl.exe").exists() {
        fetch_archive(
            stage,
            &format!("https://github.com/llvm/llvm-project/releases/download/llvmorg-{LLVM}/clang+llvm-{LLVM}-x86_64-pc-windows-msvc.tar.zst"),
            &llvm,
            true,
        )?;
    }
    let sdl = t.join(format!("SDL3-{SDL3}"));
    if !sdl.join("cmake").is_dir() {
        fetch_archive(
            stage,
            &format!("https://github.com/libsdl-org/SDL/releases/download/release-{SDL3}/SDL3-devel-{SDL3}-VC.zip"),
            &sdl,
            true,
        )?;
    }
    let movie_libs = windows_movie_libs(t);
    if (!movie_libs.join("lib/vpx.lib").exists() || !movie_libs.join("lib/zs.lib").exists())
        && find_on_path("git").is_some()
    {
        let vcpkg = t.join("vcpkg");
        let vcpkg_exe = vcpkg.join("vcpkg.exe");
        if !vcpkg_exe.exists() {
            if !vcpkg.is_dir() {
                let dir = vcpkg.display().to_string();
                let steps: [Vec<String>; 3] = [
                    vec!["init".to_owned(), "-q".to_owned(), dir.clone()],
                    vec![
                        "-C".to_owned(),
                        dir.clone(),
                        "fetch".to_owned(),
                        "-q".to_owned(),
                        "--depth".to_owned(),
                        "1".to_owned(),
                        "https://github.com/microsoft/vcpkg.git".to_owned(),
                        VCPKG_COMMIT.to_owned(),
                    ],
                    vec![
                        "-C".to_owned(),
                        dir,
                        "checkout".to_owned(),
                        "-q".to_owned(),
                        "FETCH_HEAD".to_owned(),
                    ],
                ];
                for args in &steps {
                    run_logged(stage, Path::new("git"), args, root, &[], &[], None)?;
                }
            }
            run_logged(
                stage,
                &vcpkg.join("bootstrap-vcpkg.bat"),
                &["-disableMetrics".to_owned()],
                &vcpkg,
                &[],
                &[],
                Some(&logs.join("vcpkg.log")),
            )?;
        }
        if vcpkg_exe.exists() {
            let env = msvc_env(stage, root, t)?;
            let triplet = "x64-windows-static-md-release";
            run_logged(
                stage,
                &vcpkg_exe,
                &[
                    "install".to_owned(),
                    format!("libvpx:{triplet}"),
                    format!("libvorbis:{triplet}"),
                    format!("zlib:{triplet}"),
                    "--disable-metrics".to_owned(),
                ],
                &vcpkg,
                &env,
                &["Platform", "PLATFORM"],
                Some(&logs.join("vcpkg.log")),
            )?;
        }
    }
    Ok(())
}

pub(super) fn windows_movie_libs(t: &Path) -> PathBuf {
    let release = t.join("vcpkg/installed/x64-windows-static-md-release");
    let legacy = t.join("vcpkg/installed/x64-windows-static-md");
    if !release.join("lib/vpx.lib").exists() && legacy.join("lib/vpx.lib").exists() {
        legacy
    } else {
        release
    }
}

pub(super) fn msvc_env(stage: Stage, root: &Path, t: &Path) -> Result<Vec<(String, String)>> {
    let prog_files = std::env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"));
    let vswhere = prog_files.join(r"Microsoft Visual Studio\Installer\vswhere.exe");
    let inst = if vswhere.is_file() {
        Command::new(&vswhere)
            .args([
                "-latest",
                "-products",
                "*",
                "-requires",
                "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
                "-property",
                "installationPath",
            ])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default()
    } else {
        String::new()
    };
    let bat = PathBuf::from(&inst).join(r"VC\Auxiliary\Build\vcvars64.bat");
    if inst.is_empty() || !bat.is_file() {
        return Err(stage_err(
            stage,
            "missing: the Visual Studio C++ build tools (the Windows SDK and C runtime)",
        ));
    }
    let out = Command::new("cmd")
        .args([
            "/s",
            "/c",
            &format!("\"{}\" >nul 2>&1 && set", bat.display()),
        ])
        .output()
        .map_err(|e| stage_err(stage, format!("vcvars64.bat failed: {e}")))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut env: Vec<(String, String)> = Vec::new();
    let mut path_val = String::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.is_empty() || k.eq_ignore_ascii_case("PLATFORM") {
            continue;
        }
        if k.eq_ignore_ascii_case("PATH") {
            path_val = v.to_owned();
        } else {
            env.push((k.to_owned(), v.to_owned()));
        }
    }
    let llvm_bin = t.join(format!("llvm-{LLVM}")).join("bin");
    let [cmake_bin, ninja_bin] = build_tool_dirs(root);
    let joined = std::env::join_paths(
        [llvm_bin, cmake_bin, ninja_bin]
            .into_iter()
            .chain(std::env::split_paths(&path_val)),
    )
    .map_err(|e| stage_err(stage, e))?;
    env.push(("PATH".to_owned(), joined.to_string_lossy().into_owned()));
    Ok(env)
}

pub(super) fn mtime_newer(a: &Path, b: &Path) -> bool {
    let Ok(ma) = std::fs::metadata(a).and_then(|m| m.modified()) else {
        return true;
    };
    let Ok(mb) = std::fs::metadata(b).and_then(|m| m.modified()) else {
        return true;
    };
    ma > mb
}

/// Where the pins of the tools in `<root>/private` are recorded (one `name=value` per line):
/// what each tool folder was fetched or built from, so a bumped pin or a changed harness
/// source is noticed on a hub that already has the tool.
fn tools_stamp_file(root: &Path) -> PathBuf {
    root.join("private/.tools-stamp")
}

fn read_tools_stamp(root: &Path) -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(tools_stamp_file(root))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// SHA-256 over the relative names and bytes of the files under `dir` (sorted; `bin` and
/// `obj` build folders skipped), so a change to the shader harness source shows.
fn tree_hash(dir: &Path) -> String {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if !matches!(entry.file_name().to_str(), Some("bin" | "obj")) {
                    walk(base, &path, out);
                }
            } else if let Ok(relative) = path.strip_prefix(base) {
                out.push(relative.to_path_buf());
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files);
    files.sort();
    let mut hasher = Sha256::new();
    for relative in files {
        hasher.update(relative.to_string_lossy().replace('\\', "/").as_bytes());
        hasher.update(b"\0");
        hasher.update(std::fs::read(dir.join(&relative)).unwrap_or_default());
        hasher.update(b"\0");
    }
    format!("{:x}", hasher.finalize())
}

/// The pins the tools in `<root>/private` should carry now.
fn tools_pins(root: &Path) -> std::collections::BTreeMap<String, String> {
    [
        ("imgui", IMGUI.to_owned()),
        ("ryubing", RYUBING.to_owned()),
        ("cmake", CMAKE.to_owned()),
        ("ninja", NINJA.to_owned()),
        (
            "harness",
            tree_hash(&root.join("tools/maxwell/spirv_harness")),
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect()
}

fn harness_built(root: &Path) -> bool {
    let p = root.join("private");
    p.join("build/spirv_harness/bin/Release/net10.0/spirv_harness.dll")
        .exists()
        || p.join("build/spirv_harness/out/spirv_harness.dll").exists()
}

/// Lays down empty stand-ins for every tool `rust_tools` leaves in `<root>/private`, with the
/// current pins, so a test of the stage runner sees the Tools stage as done. Not for the app.
#[doc(hidden)]
pub fn seed_tools_for_test(root: &Path) -> std::io::Result<()> {
    let p = root.join("private");
    let t = p.join("third_party");
    let [cmake_bin, ninja_bin] = build_tool_dirs(root);
    let files = [
        cmake_bin.join(executable(Os::current(), "cmake")),
        ninja_bin.join(executable(Os::current(), "ninja")),
        t.join(format!("Vulkan-Headers-{VK_HEADERS}/include/stand-in")),
        t.join("imgui/imgui.cpp"),
        t.join("ryubing/Ryujinx.sln"),
        t.join("dotnet").join(executable(Os::current(), "dotnet")),
        p.join("build/spirv_harness/out/spirv_harness.dll"),
    ];
    for file in files {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(file, "stand-in")?;
    }
    let mut stamp = tools_pins(root);
    stamp.insert("dotnet".to_owned(), DOTNET.to_owned());
    let text: String = stamp.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    std::fs::write(tools_stamp_file(root), text)
}

/// Whether the tools `rust_tools` leaves in `<root>/private` are all there and built from the
/// current pins. The record in `out` alone is not enough: a refreshed source copy or an app
/// update leaves `out` standing without them.
pub(crate) fn tools_current(root: &Path) -> bool {
    let t = root.join("private/third_party");
    let stamp = read_tools_stamp(root);
    let pins = tools_pins(root);
    let pinned = |k: &str| stamp.get(k) == pins.get(k);
    let dotnet_ok = find_on_path("dotnet").is_some_and(|p| has_dotnet_10(&p))
        || (t
            .join("dotnet")
            .join(executable(Os::current(), "dotnet"))
            .exists()
            && stamp.get("dotnet").map(String::as_str) == Some(DOTNET));
    build_tools_present(root)
        && pinned("cmake")
        && pinned("ninja")
        && t.join(format!("Vulkan-Headers-{VK_HEADERS}")).is_dir()
        && t.join("imgui/imgui.cpp").exists()
        && t.join("ryubing/Ryujinx.sln").exists()
        && harness_built(root)
        && pinned("imgui")
        && pinned("ryubing")
        && pinned("harness")
        && dotnet_ok
}

#[cfg(test)]
mod tools_cache_tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("acnh-tools-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "x").unwrap();
    }

    #[test]
    fn a_path_with_dotdot_is_the_same_folder_as_its_collapsed_form() {
        let root = temp("same");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        let dotted = root.join("a/b/../b");
        assert!(dotted != root.join("a/b"));
        assert!(same_folder(&dotted, &root.join("a/b")));
        assert!(!same_folder(&root.join("a"), &root.join("a/b")));
        assert!(!same_folder(&root.join("none"), &root.join("a")));
    }

    #[test]
    fn the_harness_hash_follows_its_source_and_ignores_build_folders() {
        let root = temp("hash");
        let dir = root.join("h");
        touch(&dir.join("A.cs"));
        let first = tree_hash(&dir);
        touch(&dir.join("obj/x.dll"));
        assert_eq!(tree_hash(&dir), first);
        std::fs::write(dir.join("A.cs"), "y").unwrap();
        assert_ne!(tree_hash(&dir), first);
    }

    #[test]
    fn tools_are_not_current_when_missing_or_unstamped() {
        let root = temp("current");
        assert!(!tools_current(&root));
        // The build tools alone are not the whole set.
        let [cmake, ninja] = super::build_tool_dirs(&root);
        touch(&cmake.join(executable(Os::current(), "cmake")));
        touch(&ninja.join(executable(Os::current(), "ninja")));
        assert!(!tools_current(&root));
        // Without a stamp, built tools are of unknown pins and are not trusted.
        let p = root.join("private");
        touch(&p.join(format!(
            "third_party/Vulkan-Headers-{}/include/x",
            super::VK_HEADERS
        )));
        touch(&p.join("third_party/imgui/imgui.cpp"));
        touch(&p.join("third_party/ryubing/Ryujinx.sln"));
        touch(&p.join("build/spirv_harness/out/spirv_harness.dll"));
        assert!(!tools_current(&root));
    }

    #[test]
    fn the_harness_pin_changes_with_its_source() {
        let root = temp("stamp");
        touch(&root.join("tools/maxwell/spirv_harness/A.cs"));
        let before = tools_pins(&root);
        std::fs::write(root.join("tools/maxwell/spirv_harness/A.cs"), "changed").unwrap();
        assert_ne!(tools_pins(&root).get("harness"), before.get("harness"));
        assert_eq!(tools_pins(&root).get("imgui"), before.get("imgui"));
    }

    #[test]
    fn refreshing_the_source_copy_keeps_the_tools_and_an_update_inherits_them() {
        let data = temp("refresh");
        let source = data.join("pkg");
        touch(&source.join("CMakeLists.txt"));
        let old = data.join("source/old");
        touch(&old.join("private/third_party/ninja/ninja"));
        touch(&old.join("stale.txt"));
        // Same commit again: the copy is replaced, private/ stays.
        refresh_source_copy(&source, &old).unwrap();
        assert!(old.join("private/third_party/ninja/ninja").is_file());
        assert!(!old.join("stale.txt").exists());
        assert!(old.join("CMakeLists.txt").is_file());
        // A new commit takes over the tools of the earlier one.
        let new = data.join("source/new");
        refresh_source_copy(&source, &new).unwrap();
        assert!(new.join("private/third_party/ninja/ninja").is_file());
        assert!(new.join("CMakeLists.txt").is_file());
        assert!(!data.join("source/.private-keep-new").exists());
    }
}

#[cfg(test)]
mod download_pin_tests {
    use super::{
        pinned_sha256, DLSS, DOTNET, DOWNLOAD_PINS, FFX, IMGUI, LLVM, RYUBING, SDL3, VK_HEADERS,
    };

    #[test]
    fn every_pin_line_is_a_sha256_then_an_https_url() {
        for line in DOWNLOAD_PINS.lines().filter(|l| !l.trim().is_empty()) {
            let (sum, url) = line
                .split_once("  ")
                .expect("two spaces between sum and url");
            assert_eq!(sum.len(), 64, "{line}");
            assert!(sum.bytes().all(|b| b.is_ascii_hexdigit()), "{line}");
            assert!(url.starts_with("https://"), "{line}");
        }
    }

    #[test]
    fn every_download_prepare_builds_has_a_recorded_checksum() {
        let mut urls = vec![
            format!("https://github.com/KhronosGroup/Vulkan-Headers/archive/refs/tags/v{VK_HEADERS}.tar.gz"),
            format!("https://github.com/ocornut/imgui/archive/refs/tags/v{IMGUI}.tar.gz"),
            format!("https://codeberg.org/Software_arch/Ryubing/archive/{RYUBING}.tar.gz"),
            format!("https://github.com/llvm/llvm-project/releases/download/llvmorg-{LLVM}/clang+llvm-{LLVM}-x86_64-pc-windows-msvc.tar.zst"),
            format!("https://github.com/libsdl-org/SDL/releases/download/release-{SDL3}/SDL3-devel-{SDL3}-VC.zip"),
            format!("https://raw.githubusercontent.com/GPUOpen-LibrariesAndSDKs/FidelityFX-SDK/{FFX}/Kits/FidelityFX/api/include/ffx_api.h"),
            format!("https://raw.githubusercontent.com/NVIDIA/DLSS/{DLSS}/LICENSE.txt"),
        ];
        for (rid, ext) in [
            ("win-x64", "zip"),
            ("linux-x64", "tar.gz"),
            ("linux-arm64", "tar.gz"),
            ("osx-arm64", "tar.gz"),
            ("osx-x64", "tar.gz"),
        ] {
            urls.push(format!(
                "https://builds.dotnet.microsoft.com/dotnet/Sdk/{DOTNET}/dotnet-sdk-{DOTNET}-{rid}.{ext}"
            ));
        }
        for url in urls {
            assert!(
                pinned_sha256(&url).is_some(),
                "no pinned checksum for {url}"
            );
        }
    }

    #[test]
    fn tool_versions_carry_no_line_end_bytes() {
        for v in [VK_HEADERS, SDL3, LLVM, IMGUI, RYUBING, DOTNET, FFX, DLSS] {
            assert!(!v.contains(['\r', '\n']), "{v:?}");
        }
    }

    #[test]
    fn an_unlisted_url_has_no_pin() {
        assert!(pinned_sha256("https://example.invalid/x.tar.gz").is_none());
    }
}
