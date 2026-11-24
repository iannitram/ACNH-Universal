//! acnh-runtime: the host runtime in Rust (SPEC.md M13; docs/adr/0002-runtime-strangler.md).
//! It takes over upstream's `runtime/src` (C++: memory, the kernel and IPC, the OS services the
//! game calls, input, audio, files, the platform layers) one group of files at a time, each
//! Rust module exporting the same C symbols as the files it replaces, behind the interface the
//! game library already uses (`runtime/include/nx/game.h`, `nx_game_abi()`). The renderer
//! (`runtime/src/nvn`, `vk_*`, `mtl`), `games/acnh` and the `ui/` overlay stay C++.
//! Planned: only the service list is in the skeleton.

/// The runtime's groups, in the order they move to Rust, with the C++ they replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// `config.cpp`: `config.ini` to the `NX_*` settings.
    Config,
    /// `hle_time.cpp`, `svc_time.cpp`: the island clock (`NX_TIME_OFFSET`).
    Time,
    /// `saves.cpp`, `users.cpp`, `svc_account.cpp`: `<data root>/user/device_save`, backups.
    Saves,
    /// `hle_fs.cpp`, `game_folder.cpp`, `mod_fetch.cpp`: the romfs, its packs and mods.
    Files,
    /// `hle_hid.cpp`, `input_script.cpp`, `touch_layout.cpp`: controllers, touch, keyboard.
    Input,
    /// `hle_audio.cpp`: sound out.
    Audio,
    /// `svc_am.cpp`, `svc_set.cpp`, `svc_misc.cpp`, `svc_process.cpp` and the other `hle_*.cpp`
    /// (bcat, friends, ldn, movie, net, nfp, ngc, pl, ro, vi).
    Services,
    /// `kernel.cpp`, `ipc.cpp`, `kernel_watch.cpp`: threads, sync, handles, IPC.
    Kernel,
    /// `memory.cpp`, `memguard_*.cpp`, `write_watch.cpp`: the guest address space (on Apple at
    /// `APPLE_GUEST_SHIFT`; heap up to 6 GiB).
    Memory,
    /// `platform_*.cpp`, `thermal.cpp`, `perf_hint.cpp`: windows, audio and input per OS.
    Platform,
    /// `loader.cpp`, `game_abi.cpp`, `main.cpp`: load the game library and start it. Last.
    Loader,
}

/// Every group, in the order above.
pub const SERVICES: [Service; 11] = [
    Service::Config,
    Service::Time,
    Service::Saves,
    Service::Files,
    Service::Input,
    Service::Audio,
    Service::Services,
    Service::Kernel,
    Service::Memory,
    Service::Platform,
    Service::Loader,
];

/// How far the guest's address space is moved up on Mac (and iOS Simulator) (upstream's `kGuestShift` in
/// `runtime/include/nx/runtime.h`); the address space then starts at `0xC0_0800_0000`.
pub const APPLE_GUEST_SHIFT: u64 = 0xC0_0000_0000;

/// True once a group is served by Rust; false leaves it to upstream's C++.
pub fn ported(service: Service) -> bool {
    matches!(
        service,
        Service::Config | Service::Time | Service::Saves | Service::Files
    )
}

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

pub mod config;
pub mod files;
pub mod saves;
pub mod time;
