//! The tools stage: fetch and build the tools a prepare needs (pins, downloads, venv).

use super::*;

/// Pinned third-party versions for `Stage::Tools` and `Stage::Build`.
pub(super) const VK_HEADERS: &str = "1.4.357";
pub(super) const LLVM: &str = "23.1.3";
pub(super) const SDL3: &str = "3.4.16";
pub(super) const IMGUI: &str = "1.91.9";
pub(super) const RYUBING: &str = "a82350bb77";
pub(super) const DOTNET: &str = "10.0.401";
pub(super) const FFX: &str = "v2.4.0";
pub(super) const DLSS: &str = "v310.9.16";

pub(super) fn venv_dir(root: &Path) -> PathBuf {
    root.join("private/venv")
}

pub(super) fn venv_bin(root: &Path) -> PathBuf {
    venv_dir(root).join(if cfg!(windows) { "Scripts" } else { "bin" })
}

pub(super) fn venv_python(root: &Path) -> PathBuf {
    venv_bin(root).join(if cfg!(windows) {
        "python.exe"
    } else {
        "python"
    })
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
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

pub(super) fn fetch_archive(
    stage: Stage,
    url: &str,
    dest_dir: &Path,
    strip: bool,
    py: &Path,
) -> Result<()> {
    std::fs::create_dir_all(dest_dir)?;
    let tmp = PathBuf::from(format!("{}.download", dest_dir.display()));
    download_file(stage, url, &tmp)?;
    let cwd = dest_dir;
    if url.ends_with(".tar.zst") {
        let script = "import sys, tarfile, zstandard; \
            d = zstandard.ZstdDecompressor(max_window_size=1<<31).stream_reader(open(sys.argv[1],'rb')); \
            t = tarfile.open(fileobj=d, mode='r|'); \
            strip = sys.argv[3] == '1'; \
            [t.extract(m, sys.argv[2], filter='data') for m in t if (not strip or ('/' in m.name and m.name.split('/',1)[1] and not setattr(m,'name',m.name.split('/',1)[1])))]";
        run_logged(
            stage,
            py,
            &[
                "-c".to_owned(),
                script.to_owned(),
                tmp.display().to_string(),
                dest_dir.display().to_string(),
                if strip { "1" } else { "0" }.to_owned(),
            ],
            cwd,
            &[],
            &[],
            None,
        )?;
    } else if url.ends_with(".zip") {
        let script = "import os, shutil, sys, zipfile; \
            z = zipfile.ZipFile(sys.argv[1]); \
            strip = sys.argv[3] == '1'; \
            names = z.namelist(); \
            top = (names[0].split('/')[0] + '/') if (strip and names) else ''; \
            [None for i in z.infolist() for n in [i.filename[len(top):] if (top and i.filename.startswith(top)) else i.filename] if n and not n.startswith('/') and '..' not in n.split('/') and ((os.makedirs(os.path.join(sys.argv[2], n), exist_ok=True) if i.is_dir() else (os.makedirs(os.path.dirname(os.path.join(sys.argv[2], n)), exist_ok=True), shutil.copyfileobj(z.open(i), open(os.path.join(sys.argv[2], n), 'wb')))) or True)]";
        run_logged(
            stage,
            py,
            &[
                "-c".to_owned(),
                script.to_owned(),
                tmp.display().to_string(),
                dest_dir.display().to_string(),
                if strip { "1" } else { "0" }.to_owned(),
            ],
            cwd,
            &[],
            &[],
            None,
        )?;
    } else {
        let mut args = vec![
            "-xzf".to_owned(),
            tmp.display().to_string(),
            "-C".to_owned(),
            dest_dir.display().to_string(),
        ];
        if strip {
            args.push("--strip-components=1".to_owned());
        }
        let tar = if cfg!(windows) { "tar.exe" } else { "tar" };
        run_logged(stage, Path::new(tar), &args, cwd, &[], &[], None)?;
    }
    let _ = std::fs::remove_file(&tmp);
    Ok(())
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

/// The Rust tools stage: ensures Python venv (for `python3 -m recomp` until #49), fetches
/// Vulkan-Headers, Dear ImGui, Ryubing (and Windows SDKs/LLVM/SDL3/vcpkg), ensures .NET 10,
/// and builds `spirv_harness.dll`.
pub fn rust_tools(req: &Request, root: &Path) -> Result<()> {
    let stage = Stage::Tools;
    let p = root.join("private");
    let t = p.join("third_party");
    let logs = req.out.join("logs");
    std::fs::create_dir_all(&t)?;
    std::fs::create_dir_all(&logs)?;

    let py = venv_python(root);
    if !py.exists() {
        run_logged(
            stage,
            Path::new(script_python(req.target)),
            &[
                "-m".to_owned(),
                "venv".to_owned(),
                venv_dir(root).display().to_string(),
            ],
            root,
            &[],
            &[],
            None,
        )?;
    }
    let check_pkgs = Command::new(&py)
        .args([
            "-c",
            "import capstone, lz4, cmake, ninja, importlib.metadata as m; assert m.version('capstone') == '5.0.7'",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !check_pkgs {
        let mut pkgs = vec![
            "-m".to_owned(),
            "pip".to_owned(),
            "install".to_owned(),
            "-q".to_owned(),
            "capstone==5.0.7".to_owned(),
            "lz4".to_owned(),
            "cmake".to_owned(),
            "ninja".to_owned(),
        ];
        if cfg!(windows) {
            pkgs.push("zstandard".to_owned());
        }
        run_logged(stage, &py, &pkgs, root, &[], &[], None)?;
    }

    let vkh = t.join(format!("Vulkan-Headers-{VK_HEADERS}"));
    if !vkh.is_dir() {
        fetch_archive(
            stage,
            &format!("https://github.com/KhronosGroup/Vulkan-Headers/archive/refs/tags/v{VK_HEADERS}.tar.gz"),
            &vkh,
            true,
            &py,
        )?;
    }
    let imgui = t.join("imgui");
    if !imgui.join("imgui.cpp").exists() {
        fetch_archive(
            stage,
            &format!("https://github.com/ocornut/imgui/archive/refs/tags/v{IMGUI}.tar.gz"),
            &imgui,
            true,
            &py,
        )?;
    }
    let ryubing = t.join("ryubing");
    if !ryubing.join("Ryujinx.sln").exists() {
        fetch_archive(
            stage,
            &format!("https://codeberg.org/Software_arch/Ryubing/archive/{RYUBING}.tar.gz"),
            &ryubing,
            true,
            &py,
        )?;
    }

    if cfg!(windows) {
        rust_tools_windows(req, root, &t, &logs, &py)?;
    }

    let mut dotnet_env = vec![("DOTNET_CLI_TELEMETRY_OPTOUT".to_owned(), "1".to_owned())];
    let dotnet = match find_on_path("dotnet").filter(|p| has_dotnet_10(p)) {
        Some(sys) => sys,
        None => {
            let dn = t.join("dotnet");
            let exe = dn.join(executable(req.target, "dotnet"));
            if !exe.exists() {
                let (rid, ext) = dotnet_rid().ok_or_else(|| {
                    stage_err(stage, "unsupported host architecture for .NET 10 SDK")
                })?;
                fetch_archive(
                    stage,
                    &format!("https://builds.dotnet.microsoft.com/dotnet/Sdk/{DOTNET}/dotnet-sdk-{DOTNET}-{rid}.{ext}"),
                    &dn,
                    false,
                    &py,
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
    Ok(())
}

pub(super) fn rust_tools_windows(
    _req: &Request,
    root: &Path,
    t: &Path,
    logs: &Path,
    py: &Path,
) -> Result<()> {
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
            py,
        )?;
    }
    let sdl = t.join(format!("SDL3-{SDL3}"));
    if !sdl.join("cmake").is_dir() {
        fetch_archive(
            stage,
            &format!("https://github.com/libsdl-org/SDL/releases/download/release-{SDL3}/SDL3-devel-{SDL3}-VC.zip"),
            &sdl,
            true,
            py,
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
                run_logged(
                    stage,
                    Path::new("git"),
                    &[
                        "clone".to_owned(),
                        "-q".to_owned(),
                        "--depth".to_owned(),
                        "1".to_owned(),
                        "https://github.com/microsoft/vcpkg.git".to_owned(),
                        vcpkg.display().to_string(),
                    ],
                    root,
                    &[],
                    &[],
                    None,
                )?;
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
    let venv_scripts = venv_bin(root);
    let joined = std::env::join_paths(
        [llvm_bin, venv_scripts]
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
