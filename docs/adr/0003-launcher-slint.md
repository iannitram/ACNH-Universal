# 0003: The launcher is Rust + Slint

**Decided:** 2026-11-02, the same decision as totk-universal's ADR 0004 (`SPEC.md`, M10).

## Decision

`app/` is a Rust binary with a Slint UI. View models are plain Rust in `app/` and `crates/core`; `.slint` files only draw. Gamepad input comes through gilrs on desktop and key events on Android. It opens on the Continue screen (M10): the island's card, one button to sync and play; first run is a guided setup driven by the controller; Saves, Settings, Fleet and About are one button away.

Upstream's `launcher/` (C++, SDL3 + ImGui) stays until the Slint app has parity, then goes (planned as piece 2.09; it went with piece 2.10, PR #274, on 2026-11-24). The Android build of the app replaces the folder screen of `android/java/.../Home.java` (ADR 0005). iOS has no launcher: the game app is the launcher there.

## Why

- One UI on Linux, Windows, Mac and Android, tested through its view models.
- totk-universal's brief and modules for the Continue screen copy over (MIT, with credit).

## Cost

Slint's royalty-free licence asks for attribution: the About page carries it.
