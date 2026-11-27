# Rules for agents in this repo (our pipeline)

This repo is a copy of Nycolazs/ACNHRecomp (MIT), not a GitHub fork, built our way under the -universal plan. Our pipeline files sit beside upstream's; upstream owns `README.md` (partly in Portuguese), `Jogar ACNH.command` and its own code and docs, so never edit those for pipeline reasons. Our pipeline docs are in English. This repo is public (MIT). The full pipeline and rules live in the pipeline repo (private): `PIPELINE.md`, `RULES.md`, `RUBRIC.md`, `roles/`; this is the short copy.

1. **Find the stage** in `PIPELINE.md`. The spec is `SPEC.md`, the plan `ROADMAP.md`, the code map `docs/code-map.md`, the decisions `docs/adr/`, the words `GLOSSARY.md`.
2. **Build on `main`.** It is the integration branch; open PRs against it. Upstream is the `upstream` remote (fetch only), merged into `main` by the daily sync; a conflict becomes an `upstream-sync` issue, taken first.
3. **Work from an issue.** Take one that is not `in-progress` and not blocked; add `in-progress` before you start. Its body is your brief; an Easy issue has a "Qwen brief" and its locked tests: commit the test file first and never edit it.
4. **Your own platform's issues first.** Linux, Windows, macOS, Android and iOS all matter: say which machine class you built on.
5. **Prove it** (R40): `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and the CMake build (`cmake -B build -G Ninja -DNX_GAME=acnh -DNX_TESTS=ON && ninja -C build`, then `ctest`, as the C++ unit tests job in `.github/workflows/check.yml` does; Prepare, `cargo run -p acnh-cli -- build`, makes a whole game build; `tools/setup_acnh.py` is retired) when you touched C++, `CMakeLists.txt` or `runtime/`; put the output in the PR body.
6. **Label the PR** `by:<machine>` and `model:<model>`, and write `Closes #N`.
7. **A failed job** gets a comment on the issue and the issue moves up one size (Easy -> Medium -> Hard).
8. **No game files, ever** (SPEC.md M14): nothing from a dump, firmware, keys, the recompiled code (`gen/`), shaders or any game-binary analysis in code, tests, logs, issues or releases. Keep the pre-commit hook `tools/check_no_game_data.sh` installed. No private details in code, commits or PR text: no names, e-mail addresses, home paths or machine names. Commit as the no-reply identity.
9. **Upstream:** a solid bug report or fix offered upstream needs my word first, says it is AI-assisted, and goes as a patch file or from a throwaway fork of upstream (this repo is public, so it is a patch file or a fork of its own, never a branch here). Never post to upstream from a satellite.

**Upstream and peers:** see `PEERS.md` at the repo root. When I say "upstream", I mean the repo in its Upstream section; reuse a peer's code only as that file allows.
