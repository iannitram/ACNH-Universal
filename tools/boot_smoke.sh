#!/usr/bin/env bash
# Linux boot/frame gate: Cargo -> CMake -> Rust-linked player -> synthetic game -> Vulkan readback.
# Needs SDL3, Vulkan (Mesa lavapipe is sufficient), Python 3, Xvfb and xauth.
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_BUILD_JOBS=2
export CMAKE_BUILD_PARALLEL_LEVEL=2
# All generated fixtures and logs stay in the worktree, including on failure.
mkdir -p target/boot-smoke
work=$(cd target/boot-smoke && pwd)
if ! ACNH_GAME=acnh ACNH_BUILD_PLAYER=1 ACNH_BOOT_SMOKE=1 cargo build --locked -p acnh-runtime --features player \
    --message-format=json >"$work/build.json"; then
    cat "$work/build.json"
    exit 1
fi
player=$(python3 - "$work/build.json" <<'PY'
import json
import pathlib
import sys

players = []
for line in pathlib.Path(sys.argv[1]).read_text().splitlines():
    message = json.loads(line)
    if message.get("reason") == "build-script-executed":
        candidate = pathlib.Path(message["out_dir"]) / "player/build/acnh"
        if candidate.is_file():
            players.append(candidate.resolve())
if len(players) != 1:
    sys.exit(f"boot_smoke: expected one player from this Cargo build, found {len(players)}")
print(players[0])
PY
)
fixture="$(dirname "$player")/boot-smoke"
python3 crates/runtime/tests/fake_game/images.py "$fixture/analysis"
rm -rf "$fixture/frames"
set +e
xvfb-run -a env SDL_VIDEO_DRIVER=x11 SDL_AUDIO_DRIVER=dummy \
    NX_HEADLESS=1 NX_MUTE=1 NX_DUMP_FRAMES=1 NX_REPLAY_THREAD=0 \
    NX_DATA_ROOT="$fixture" NX_LOG="$work/runtime.log" \
    timeout 60 "$player" >"$work/player.log" 2>&1
code=$?
set -e
cat "$work/player.log"
if [ "$code" -ne 0 ]; then
    echo "boot_smoke: player failed (exit $code)" >&2
    exit 1
fi
grep -q 'runtime: Rust ABI 1' "$work/player.log"
grep -q 'nvn: present #1 ' "$work/player.log"
# A marker alone cannot pass: require the first frame's GPU readback, with the synthetic clear.
python3 crates/runtime/tests/fake_game/images.py --check-frame "$fixture/frames/00001.ppm"
echo "boot_smoke: ok (first frame read back, player exited 0)"
