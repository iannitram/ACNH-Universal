# 0005: Android keeps upstream's route

**Decided:** 2026-11-02 (`SPEC.md`, M9).

## Decision

Upstream's Android work (`docs/ANDROID.md`, `android/`, `tools/build_android.sh`, `tools/build_turnip.sh`) is our base:

- The APK carries the runtime only (`lib<game>.so` loader and `android/java/.../Home.java`); the game library and the game's data go into the game folder on the device.
- The build runs on Linux with NDK 28.2, so the Strix Halo tablet is the Android hub.
- Turnip on Adreno, as upstream builds it. Mali is after 1.0.

What we change: our Slint launcher is built into the same APK and replaces `Home.java`'s folder screen, and it pulls the game library and data from the hub (ADR 0004) instead of `tools/push_android.sh` over `adb`. save-relay runs inside the launcher through its Android library (iannitram/save-relay#49).

## Why

Upstream already plays on Adreno devices; the recompiled code is ARM64 already, so no translation layer is needed.

## Cost

The Rust runtime must cross-build for `aarch64-linux-android` from its first step (stage 4), with the NDK in CI.
