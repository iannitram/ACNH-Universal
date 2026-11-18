#!/usr/bin/env bash
# The game's files are read from plat::game_root(); only the player's own files, and the mods' own romfs/, stay on
# plat::data_root(). Fails when a line of runtime/src names data_root() with a game read (tests/game_root_test.cpp
# checks the rule itself).
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
src="$here/../runtime/src"

game_read='romfs\.pak|spirv\.pak|"/spirv/"|"/analysis"|"fonts"|"/fonts/"|"aoc"|"extract"|"romfs"'

bad=$(grep -rnE 'data_root\(\)' "$src" | grep -E "$game_read" | grep -vE 'mods' || true)
if [ -n "$bad" ]; then
    printf 'Game files read from the data root (use plat::game_root()):\n%s\n' "$bad" >&2
    exit 1
fi
printf 'game_root_check: no game read on the data root\n'
