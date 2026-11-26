#!/bin/bash
# Source this file for write_manifest <game folder> <build id> <abi>: writes <game folder>/manifest.json (every file's
# path, size and SHA-256, and the game's build id and NX_GAME_ABI) with `acnh-universal manifest`, the one writer
# (acnh_core::appgame::Manifest, #368). It refuses an empty build id or ABI and checks the manifest against the files
# before it renames it into place. $ACNH_UNIVERSAL_CLI names a built acnh-universal; without it, cargo builds one.
write_manifest() {
  if [ -n "${ACNH_UNIVERSAL_CLI:-}" ]; then
    "$ACNH_UNIVERSAL_CLI" manifest --out "$1" --build-id "$2" --abi "$3"
  else
    (cd "$(dirname "${BASH_SOURCE[0]}")/.." && cargo run -q --locked -p acnh-cli --bin acnh-universal -- \
      manifest --out "$1" --build-id "$2" --abi "$3")
  fi
}

# The first 12 hex digits of the SHA-1 of the runtime headers, as CMake's NX_GAME_ABI computes it.
default_game_abi() {
  local dir
  dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/../runtime/include/nx" && pwd -P)
  cat "$dir/cpu.h" "$dir/module.h" "$dir/runtime.h" | { sha1sum 2>/dev/null || shasum -a 1; } | cut -c1-12
}
