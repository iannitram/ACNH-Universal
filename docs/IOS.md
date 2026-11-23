# iOS and iPadOS

For a smoke build on a Mac, install Xcode, CMake, Ninja, Python 3 and the Vulkan headers. Build with the handwritten stub and package it in a temporary folder.

```sh
tools/build_ios.sh iphoneos
out="$(mktemp -d)"
tools/package_ios.sh 0.1 private/ios/build-iphoneos "$out"
```

The packaging script takes a version, an optional build directory, and an optional output folder. It defaults to `private/ios/build-iphoneos` and `~/Dropbox/_transfers/ACNH-Universal/ios/`. It accepts arm64 device builds and requires a destination outside Git work trees.

The executable uses an ad-hoc signature without entitlements. The package omits the app's `_CodeSignature` resource seal. SideStore creates the seal when it re-signs the app for installation. Until then, full `codesign --verify` reports a missing or invalid resource directory.

For the device run, open the IPA from `Dropbox/_transfers/ACNH-Universal/ios/` on the iPad and install it with SideStore using a free Apple ID. SideStore signs the app and must refresh it every seven days. [SideStore FAQ](https://docs.sidestore.io/docs/faq)

Roadmap step 8.05a (#172) adds the prepared game (about 12 GB) to the IPA. The player log is `Documents/logs/nx.log`, with the saves and settings, so the Files app shows it.

## Save sync on the iPad

The player carries save-relay through its Rust API (`crates/runtime`, module `apple`). `tools/build_ios.sh` also builds the Rust library (`cargo build -p acnh-runtime --release --target aarch64-apple-ios`; `rustup target add aarch64-apple-ios` once) and fails unless the iOS SDL3 it built defines `SDL_JOYSTICK_MFI`, which the controllers need.

- **First start.** An alert asks for the Nextcloud server, user and an app password (Nextcloud: Settings, Security). The app password is kept only in the iOS Keychain. Not now leaves save sync off and is remembered as `relay.ask = 0` in `Documents/config.ini`. save-relay's own settings are under `Library/`, not Documents, so Files does not show them.
- **Every start.** The game boots only after the island has synced. If save-relay cannot be reached, the alert says so and offers Retry (a push still waiting from the last game is sent first). A conflict shows both islands (device, saved time, play time) and nothing plays until one is picked. A held island names the other device and plays only on Play anyway. A refused sign-in (a revoked app password) shows the sign-in alert again with the server and user filled in; there is no Not now then.
- **Quit and background.** The island is pushed when the game quits and when the app goes to the background, inside a background task so iOS lets it finish (on going to the background it runs on a worker thread, so the app stays responsive). Nothing is pushed before the game has booted. A push that cannot go, or cannot read the app password, is sent first at the next start.

Not verified on a device: whether a controller can answer the alerts, whether a push on going to the background can catch the game between two writes, and the sign-in again after coming back from the background (#76).

A free Apple ID does not provide the extended virtual addressing or increased memory limit entitlements. This package supplies neither; available address space and memory may limit whether the game runs. See [the iOS address-space finding](apple-ios.md) for details. Device installation and gameplay still need to be tested.
