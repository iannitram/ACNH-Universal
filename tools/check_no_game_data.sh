#!/usr/bin/env bash
# Refuses to let game data, keys or recompiled code into a commit. Run before every push
# (or install as a hook: ln -s ../../tools/check_no_game_data.sh .git/hooks/pre-commit).
# Checks the staged files: no gen/, exefs/, romfs/, aoc/, private/ or refs/ folders, no dump/key/shader/binary extensions
# (the folder and extension lists are shared with the release check, tools/game_data_rules.sh), no binary blobs (the
# app's icon, an original drawing, and its desktop forms are the only ones allowed), no pictures of the game, no generated
# recompiler output (function bodies named after guest addresses), no key material.
# Find the shared rules beside this script, through the hook symlink if there is one (before the cd below).
self=${BASH_SOURCE[0]}
while [ -L "$self" ]; do
  link=$(readlink "$self")
  case "$link" in /*) self=$link ;; *) self=$(dirname "$self")/$link ;; esac
done
rules=$(cd "$(dirname "$self")" && pwd -P)/game_data_rules.sh
cd "$(git rev-parse --show-toplevel)" || exit 1
# shellcheck source=tools/game_data_rules.sh
. "$rules" || exit 1
bad=0
while IFS= read -r -d '' f; do
  lf=/$(printf '%s' "$f" | tr '[:upper:]' '[:lower:]')  # (macOS ships bash 3.2: no ${f,,}); the leading / anchors the rules
  if game_data_dir_blocked "$lf"; then
    echo "BLOCKED (game folder, recompiled code or local-only folder): $f"; bad=1; continue
  fi
  if game_data_ext_blocked "$lf"; then
    echo "BLOCKED (game/derived file type): $f"; bad=1; continue
  fi
  case "$lf" in
    *.dat|*.zip|*.7z) echo "BLOCKED (game/derived file type): $f"; bad=1; continue ;;
  esac
  case "$lf" in
    /android/res/mipmap-*/*.png|/android/icon_512.png) continue ;;  # the launcher icon, drawn by tools/make_android_icon.py
    /assets/acnhrecomp.png|/assets/acnhrecomp.ico|/assets/acnhrecomp.icns) continue ;;  # the same, tools/make_icons.sh
  esac
  # A path git lists that is not a file or a symlink cannot be scanned: refuse it, never skip it.
  if [ ! -f "$f" ]; then
    if [ -L "$f" ]; then continue; fi  # a symlink's own name passed the rules above; its target is not a staged file
    echo "BLOCKED (cannot read the path as a file): $f"; bad=1; continue
  fi
  if [ "$(head -c 8000 "$f" | LC_ALL=C tr -d '\000' | wc -c)" -ne "$(head -c 8000 "$f" | wc -c)" ]; then
    echo "BLOCKED (binary content): $f"; bad=1; continue
  fi
  if grep -qE '^void (main|sdk|subsdk[0-9])_[0-9A-F]{8}\(nx::CPU' "$f"; then
    echo "BLOCKED (recompiled game code): $f"; bad=1; continue
  fi
  if grep -qiE '^(master_key|header_key|titlekek|key_area_key|aes_kek_generation)[_0-9a-z]* *= *[0-9a-f]{32}' "$f"; then
    echo "BLOCKED (key material): $f"; bad=1; continue
  fi
done < <(git -c core.quotePath=false diff --cached --name-only -z --diff-filter=ACMR)
[ $bad = 0 ] && echo "ok: no game data staged" || { echo "commit refused"; exit 1; }
