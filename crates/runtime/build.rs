//! Generate the C interface and optionally build the C++ player through CMake.

use std::env;
use std::error::Error;
use std::path::{Path, PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=cbindgen.toml");
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let out = PathBuf::from(env::var("OUT_DIR")?);
    header(&manifest, &out)?;
    #[cfg(feature = "player")]
    player(&manifest, &out)?;
    Ok(())
}

fn header(manifest: &Path, out: &Path) -> Result<(), Box<dyn Error>> {
    let dir = out.join("include");
    std::fs::create_dir_all(&dir)?;
    let config = cbindgen::Config::from_file(manifest.join("cbindgen.toml"))?;
    let bindings = cbindgen::Builder::new()
        .with_crate(manifest)
        .with_config(config)
        .generate()?;
    let mut bytes = Vec::new();
    bindings.write(&mut bytes);
    std::fs::write(dir.join("acnh_runtime.h"), bytes)?;
    Ok(())
}

#[cfg(feature = "player")]
fn player(manifest: &Path, out: &Path) -> Result<(), Box<dyn Error>> {
    use std::process::Command;

    // Workspace lint/test runs do not require a native player toolchain.
    println!("cargo:rerun-if-env-changed=ACNH_BUILD_PLAYER");
    if env::var("ACNH_BUILD_PLAYER").as_deref() != Ok("1") {
        return Ok(());
    }
    println!("cargo:rerun-if-env-changed=ACNH_GAME");
    println!("cargo:rerun-if-env-changed=ACNH_BOOT_SMOKE");
    println!("cargo:rerun-if-env-changed=CMAKE_PREFIX_PATH");
    println!("cargo:rerun-if-changed=tests/fake_game");
    for path in ["CMakeLists.txt", "runtime", "games", "assets"] {
        println!("cargo:rerun-if-changed=../../{path}");
    }
    println!("cargo:rerun-if-changed=../../Cargo.lock");
    println!("cargo:rerun-if-changed=Cargo.toml");

    // Cargo has not compiled this crate when its build script runs. Build the staticlib first,
    // without `player`, in an isolated target directory: no recursive player build or Cargo lock
    // deadlock. Using Cargo also keeps future runtime dependencies and target flags intact.
    let target = env::var("TARGET")?;
    let profile = env::var("PROFILE")?;
    let archive_dir = out.join("runtime-static");
    let status = Command::new(env::var_os("CARGO").ok_or("CARGO is missing")?)
        .args([
            "build",
            "--locked",
            "--lib",
            "--no-default-features",
            "--manifest-path",
        ])
        .arg(manifest.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&archive_dir)
        .args([
            "--target",
            &target,
            "--profile",
            if profile == "debug" { "dev" } else { &profile },
        ])
        .args(["--jobs", &env::var("NUM_JOBS")?])
        .env_remove("ACNH_BUILD_PLAYER")
        .status()?;
    if !status.success() {
        return Err(format!("runtime staticlib build failed: {status}").into());
    }
    let archive = archive_dir
        .join(&target)
        .join(&profile)
        .join(if target.contains("msvc") {
            "acnh_runtime.lib"
        } else {
            "libacnh_runtime.a"
        });
    let game = match env::var("ACNH_GAME") {
        Ok(game) => game,
        Err(_) => String::from("acnh"),
    };
    cmake::Config::new(manifest.join("../.."))
        .out_dir(out.join("player"))
        .generator("Ninja")
        .define("NX_GAME", &game)
        .define("NX_GAME_FROM_FOLDER", "ON")
        .define(
            "NX_BOOT_SMOKE",
            if env::var("ACNH_BOOT_SMOKE").as_deref() == Ok("1") {
                "ON"
            } else {
                "OFF"
            },
        )
        .define("NX_RUNTIME_ARCHIVE", archive)
        .define("NX_RUNTIME_INCLUDE", out.join("include"))
        .build_target(&game)
        .build();
    Ok(())
}
