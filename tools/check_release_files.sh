#!/usr/bin/env bash
# The game-data check over a release's file list (SPEC.md M14): reads one path per line on stdin
# (an archive listing, an AppDir's find output) and refuses game data, keys, packs, shaders and gen/.
# usage: <list files> | tools/check_release_files.sh
bad=0
while IFS= read -r f; do
  lf=$(printf '%s' "$f" | tr '[:upper:]' '[:lower:]')
  case "$lf" in
    *.nsp|*.xci|*.nca|*.nso|*.npdm|*.keys|*.tik|*.cert|*.spv|*.bin|*.bfsha|*.bntx|*.szs|*.zs|*.sarc|*.bfres|*.ttf|*.otf|*.pack|*.sav)
      echo "BLOCKED (game/derived file type): $f"; bad=1 ;;
    */gen/*|gen/*|*/exefs/*|*/romfs/*|*/private/*|*/user/device_save*)
      echo "BLOCKED (game folder, recompiled code or saves): $f"; bad=1 ;;
  esac
done
[ $bad = 0 ] && echo "ok: no game data in the release file list" || { echo "release refused"; exit 1; }
