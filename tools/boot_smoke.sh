#!/usr/bin/env bash
# Boot smoke test (SPEC.md M13, M15): build the player through cargo (feature `player`, CMake from
# crates/runtime/build.rs), point it at a fake game library, run it headless, and require that it
# loads the library and starts booting. No game files. Linux; needs cmake, ninja, SDL3, Vulkan headers.
set -euo pipefail
cd "$(dirname "$0")/.."
ACNH_BUILD_PLAYER=1 cargo build -p acnh-runtime --features player
player=$(find target -path '*build/script-output' -prune -o -type f -name acnh -perm -u+x -print | head -n1)
[ -n "$player" ] || player=$(find target -type f -name acnh -perm -u+x | head -n1)
[ -n "$player" ] || { echo "boot_smoke: player not found" >&2; exit 1; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/game/linux" "$tmp/analysis"
abi=$("$player" --abi 2>/dev/null || true)
c++ -std=c++20 -shared -fPIC -Iruntime/include ${abi:+-DNX_GAME_ABI="\"$abi\""} \
    crates/runtime/tests/fake_game/fake_game.cpp -o "$tmp/game/linux/libacnh_game.so"
set +e
NX_HEADLESS=1 NX_DATA_ROOT="$tmp" timeout 60 "$player" >"$tmp/log" 2>&1
code=$?
set -e
cat "$tmp/log"
# The fake library has no code, so a clean exit or a stop after loading is expected; a refusal of
# the library is the failure.
if grep -q "not a game library\|abi " "$tmp/log"; then echo "boot_smoke: library refused" >&2; exit 1; fi
grep -q "entering nnMain\|first frame" "$tmp/log" || { echo "boot_smoke: no boot marker (exit $code)" >&2; exit 1; }
echo "boot_smoke: ok"
