# The one list of game-data file types and folders (SPEC.md M14), sourced by tools/check_no_game_data.sh (the commit
# and CI check) and tools/check_release_files.sh (the release check). Not run on its own. bash 3.2 safe.
# Both functions take a lowercased path with a leading "/" and return 0 when the path is refused.

# Game, dump, key, shader and pack file types, and the fonts the game ships.
game_data_ext_blocked() {
  case "$1" in
    *.nsp|*.xci|*.nsz|*.xcz|*.nca|*.nso|*.npdm|*.keys|*.tik|*.cert|*.spv|*.bin|*.bfsha|*.bntx|*.szs|*.zs|*.sarc|*.bfres|*.ttf|*.otf|*.pack|*.pak|*.sav|*.byml|*.msbt|*.bars|*.bwav) return 0 ;;
  esac
  return 1
}

# Folders that hold game files, recompiled code or local-only material, at any depth.
game_data_dir_blocked() {
  case "$1" in
    */gen/*|*/exefs/*|*/romfs/*|*/aoc/*|*/private/*|*/refs/*) return 0 ;;
  esac
  return 1
}
