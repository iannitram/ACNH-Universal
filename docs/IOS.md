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

Roadmap step 8.05a (#172) adds the prepared game (about 12 GB) to the IPA and moves the player log into Documents, where the app already keeps saves and settings.

A free Apple ID does not provide the extended virtual addressing or increased memory limit entitlements. This package supplies neither; available address space and memory may limit whether the game runs. See [the iOS address-space finding](apple-ios.md) for details. Device installation and gameplay still need to be tested.
