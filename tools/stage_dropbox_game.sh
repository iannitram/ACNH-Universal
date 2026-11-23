#!/bin/bash
# Puts the prepared Android game where the app's in-app download finds it (issue #230; SPEC M18 and the Dropbox
# exception): <dest>/acnh-universal-game/android/ with the game library, romfs.pak, spirv.pak, analysis/, fonts/,
# aoc/ and manifest.json (each file's path, size and SHA-256). <dest> is the local copy of save-relay's Dropbox app
# folder (Dropbox/Apps/<app>), which the Dropbox client uploads; the app signs in to that same app and reads only
# that folder. Only for the owner's own Dropbox: never a shared or public link, never the repo or CI (M14).
#
# usage: tools/stage_dropbox_game.sh <prepared folder> <game library> <dest>
#   <prepared folder>: Prepare's work folder (analysis/, fonts/, aoc/, romfs.pak, spirv.pak)
#   <game library>:    libacnh_game.so from tools/build_android.sh (private/acnh/apk/libacnh_game.so)
set -euo pipefail
[ $# -eq 3 ] || { sed -n 2,11p "$0"; exit 2; }
SRC=$1 LIB=$2 DEST=$3/acnh-universal-game/android
for f in analysis/main.bin romfs.pak spirv.pak; do [ -f "$SRC/$f" ] || { echo "missing $SRC/$f"; exit 1; }; done
[ -f "$LIB" ] || { echo "missing $LIB"; exit 1; }
mkdir -p "$DEST.new"
# On APFS a clone (cp -c) costs no space until either copy changes.
copy() { cp -c "$1" "$2" 2>/dev/null || cp "$1" "$2"; }
copy "$LIB" "$DEST.new/libacnh_game.so"
for f in romfs.pak spirv.pak; do copy "$SRC/$f" "$DEST.new/$f"; done
for d in analysis fonts aoc; do
  [ -d "$SRC/$d" ] || continue
  (cd "$SRC" && find "$d" -type f ! -name .DS_Store) | while read -r f; do
    mkdir -p "$DEST.new/$(dirname "$f")"
    copy "$SRC/$f" "$DEST.new/$f"
  done
done
# The manifest last, so a half-copied folder is never mistaken for a finished one.
(
  cd "$DEST.new"
  echo '{"version":1,"files":['
  first=1
  find . -type f ! -name manifest.json ! -name .DS_Store | sed 's|^\./||' | sort | while read -r f; do
    size=$(stat -f %z "$f" 2>/dev/null || stat -c %s "$f")
    sha=$(shasum -a 256 "$f" | cut -d' ' -f1)
    [ $first = 1 ] || echo ','
    first=0
    printf '{"path":"%s","size":%s,"sha256":"%s"}' "$f" "$size" "$sha"
  done
  echo ']}'
) > "$DEST.new/manifest.json.tmp"
mv "$DEST.new/manifest.json.tmp" "$DEST.new/manifest.json"
rm -rf "$DEST.old"
[ -d "$DEST" ] && mv "$DEST" "$DEST.old"
mv "$DEST.new" "$DEST"
rm -rf "$DEST.old"
python3 -c 'import json,sys; m=json.load(open(sys.argv[1])); print(len(m["files"]), "files,", round(sum(f["size"] for f in m["files"])/1e9, 2), "GB")' "$DEST/manifest.json"
