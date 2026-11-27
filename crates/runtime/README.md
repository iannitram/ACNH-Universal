The runtime (the Rust cores of all 11 groups of upstream's `runtime/src`, with the renderer, `games/acnh` and the overlay still C++: ADR 0002, `docs/code-map.md`) builds as `rlib` for Rust callers and `staticlib` for the C++ player. `build.rs` generates `acnh_runtime.h` with cbindgen; header generation failures stop the build.

Build the folder-loaded player with CMake and Ninja installed, plus SDL3 and Vulkan development headers:

```sh
ACNH_BUILD_PLAYER=1 cargo build -j2 -p acnh-runtime --features player
```

A Cargo build script runs before its own library exists. The player build therefore first builds this crate's static library with no features in an isolated target directory under `OUT_DIR/runtime-static`, then passes the archive and generated include directory to CMake. The inner build cannot build the player and does not contend for the outer target directory's lock. The player calls `acnh_runtime_abi_version()` at boot, so its link requires the Rust object. CMake leaves the player in `OUT_DIR/player/build/acnh` (`acnh.exe` on Windows). Ordinary workspace lint/test builds do not request the native player.

On Linux x86-64, run the same gate as CI:

```sh
bash tools/boot_smoke.sh
```

It needs Python 3, Xvfb, xauth, and a Vulkan driver; Mesa lavapipe works without a physical GPU. CMake builds the fake shared library with the player's actual `NX_GAME_ABI`, in `game/linux-x64`. The fixture has authored module metadata and host callbacks for startup, initializers, SDK builder queries, and `nnMain`. It submits a red clear and presents one 16×16 texture through the real NVN/Vulkan code. The script requires the Rust boot marker, the first present, an exact red GPU readback, and exit status zero. Timeouts and crashes fail. Generated images, frames and logs stay under `target/` and contain no game assets.

The original CMake path remains available without Rust or the fixture:

```sh
cmake -S . -B build -G Ninja -DNX_GAME=acnh -DNX_GAME_FROM_FOLDER=ON -DNX_TESTS=ON
cmake --build build --target acnh gpu_pick_test tiling_test astc_test --parallel 2
ctest --test-dir build --output-on-failure
```
