//! acnh-runtime: the host runtime in Rust (SPEC.md M13; docs/adr/0002-runtime-strangler.md).
//! It has taken over upstream's `runtime/src` (C++: memory, the kernel and IPC, the OS services the
//! game calls, input, audio, files, the platform layers) one group of files at a time (4.02 to
//! 4.12, all 11 groups), each Rust module exporting the same C symbols as the files it replaces,
//! behind the interface the
//! game library already uses (`runtime/include/nx/game.h`, `nx_game_abi()`). The renderer
//! (`runtime/src/nvn`, `vk_*`, `mtl`), `games/acnh` and the `ui/` overlay stay C++.

// ADR 0002: every `unsafe` block says why it is sound.
#![warn(clippy::undocumented_unsafe_blocks)]

/// How far the guest's address space is moved up on Mac (and iOS Simulator) (upstream's `kGuestShift` in
/// `runtime/include/nx/runtime.h`); the address space then starts at `0xC0_0800_0000`.
pub const APPLE_GUEST_SHIFT: u64 = 0xC0_0000_0000;

/// Version of the scaffold's C interface, checked by the player at boot.
#[no_mangle]
pub extern "C" fn acnh_runtime_abi_version() -> u32 {
    1
}

/// save-relay inside the iPad's player and the Android app: pull before the game, push after,
/// the setup, the conflict and held prompts (issues #172 and #81; `relay.rs`, UIKit and the
/// Keychain in `relay_ios.rs`, the JNI bridge and the file credential store in
/// `relay_android.rs`). The tests run the platform-free half on every runner
/// (`tests/ios_relay.rs`, `tests/android_relay.rs`).
#[cfg(any(target_os = "ios", target_os = "android"))]
pub mod relay;

/// The iPhone and iPad game download (issue #231): the plan and the checks (`appgame.rs`,
/// platform-free, tested on every runner in `tests/ios_appgame.rs`) and the Dropbox sign-in and
/// C exports (`appgame_ios.rs`) the player's Objective-C++ calls.
#[cfg(target_os = "ios")]
pub mod appgame;
#[cfg(target_os = "ios")]
pub mod appgame_ios;
pub mod audio;
pub mod config;
pub mod files;
#[cfg(target_os = "android")]
pub mod hub_android;
pub mod input;
pub mod kernel;
pub mod loader;
pub mod memory;
pub mod platform;
pub mod saves;
pub mod services;
pub mod sync;
pub mod time;
