# Decisions

Ours, in English; upstream's own docs (`docs/*.md`, some in Portuguese) are kept as upstream has them.

| ADR | Decision |
|---|---|
| [0001](0001-rust-workspace.md) | One Rust workspace for our code; the renderer, game code and overlay stay C++ |
| [0002](0002-runtime-strangler.md) | The prep tools, then the recompiler, then the runtime move to Rust, one piece at a time |
| [0003](0003-launcher-slint.md) | The launcher is Rust + Slint |
| [0004](0004-build-hubs.md) | Three home build hubs |
| [0005](0005-android-upstream-route.md) | Android keeps upstream's route |
| [0006](0006-apple.md) | Apple: the same recompiled code on Mac and iOS |
| [0007](0007-recompiler-names-and-capstone.md) | The Rust recompiler binary is `acnh-recompiler`; Capstone stays 5.0.6 in Rust and 5.0.7 in the C++ copy |
