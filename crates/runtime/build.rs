//! Build script of acnh-runtime: writes the C header with cbindgen, and with feature `player`
//! builds the C++ player through the `cmake` crate (the root `CMakeLists.txt`, unchanged: the
//! old CMake path keeps working on its own).

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=cbindgen.toml");
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap_or_default());
    header(&manifest, &out);
    #[cfg(feature = "player")]
    player(&manifest, &out);
}

/// `$OUT_DIR/include/acnh_runtime.h`. A failure is a warning: the header is only needed by the
/// C++ side, and only once the runtime exports C symbols.
fn header(manifest: &PathBuf, out: &PathBuf) {
    let dir = out.join("include");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let built = cbindgen::Builder::new()
        .with_crate(manifest)
        .with_config(
            cbindgen::Config::from_file(manifest.join("cbindgen.toml")).unwrap_or_default(),
        )
        .generate();
    match built {
        Ok(bindings) => {
            bindings.write_to_file(dir.join("acnh_runtime.h"));
        }
        Err(e) => println!("cargo:warning=cbindgen: {e}"),
    }
}

/// Configure and build the player with CMake. `ACNH_GAME` picks the game (default `acnh`),
/// The folder-loaded library mode (`NX_GAME_FROM_FOLDER`, always on) loads its code from the game folder, so no generated
/// code is needed to build.
#[cfg(feature = "player")]
fn player(manifest: &PathBuf, out: &PathBuf) {
    // `--all-features` (clippy, tests) must not need SDL3 and Vulkan headers: build only on request.
    println!("cargo:rerun-if-env-changed=ACNH_BUILD_PLAYER");
    if env::var_os("ACNH_BUILD_PLAYER").is_none() {
        println!("cargo:warning=feature `player`: set ACNH_BUILD_PLAYER=1 to build the C++ player");
        return;
    }
    println!("cargo:rerun-if-env-changed=ACNH_GAME");
    println!("cargo:rerun-if-changed=../../CMakeLists.txt");
    println!("cargo:rerun-if-changed=../../runtime");
    println!("cargo:rerun-if-changed=../../games");
    println!("cargo:rerun-if-changed=../../ui");
    let root = manifest.join("../..");
    let game = env::var("ACNH_GAME").unwrap_or_else(|_| "acnh".into());
    let dst = cmake::Config::new(&root)
        .out_dir(out.join("player"))
        .generator("Ninja")
        .define("NX_GAME", &game)
        .define("NX_LAUNCHER", "OFF")
        .define("NX_GAME_FROM_FOLDER", "ON")
        .build_target(&game)
        .build();
    println!(
        "cargo:rustc-env=ACNH_PLAYER_DIR={}",
        dst.join("build").display()
    );
}
