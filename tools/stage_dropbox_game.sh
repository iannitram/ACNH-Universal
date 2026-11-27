#!/bin/bash
# Puts the prepared Android game where the app's in-app download finds it (issue #230; SPEC M18 and the Dropbox
# exception): <dest>/acnh-universal-game/android/ with the game library, romfs.pak, spirv.pak, analysis/, fonts/,
# aoc/ and manifest.json (each file's path, size and SHA-256). <dest> is the local copy of save-relay's Dropbox app
# folder (Dropbox/Apps/<app>), which the Dropbox client uploads; the app signs in to that same app and reads only
# that folder. Only for the owner's own Dropbox: never a shared or public link, never the repo or CI (M14).
#
# usage:
#   Android (default): tools/stage_dropbox_game.sh --build-id <id> [--abi <abi>] <prepared folder> <game library> <dest>
#   Linux:             tools/stage_dropbox_game.sh --build-id <id> [--abi <abi>] --os linux --install <install folder> <prepared folder> <dest>
# --build-id (the game's 32 hex digit build id) is required; --abi defaults to the headers' NX_GAME_ABI. The manifest is written by `acnh-universal manifest` (tools/write_manifest.sh) and checked before the swap.
#   <prepared folder>: Prepare's work folder (analysis/, fonts/, aoc/, romfs.pak, spirv.pak)
#   <game library>:    libacnh_game.so from tools/build_android.sh (private/acnh/apk/libacnh_game.so)
set -euo pipefail
# The script's own folder, through any symlink to it, for the helper beside it.
self=$0
while [ -L "$self" ]; do
  link=$(readlink "$self")
  case "$link" in /*) self=$link ;; *) self=$(dirname "$self")/$link ;; esac
done
# shellcheck source=tools/remove_tree.sh
. "$(cd "$(dirname "$self")" && pwd -P)/remove_tree.sh"
# shellcheck source=tools/write_manifest.sh
. "$(cd "$(dirname "$self")" && pwd -P)/write_manifest.sh"
OS=android
INSTALL=""
BUILD_ID=""
ABI=""
while [ $# -gt 0 ]; do
  case "$1" in
    --os) OS=$2; shift 2 ;;
    --install) INSTALL=$2; shift 2 ;;
    --build-id) BUILD_ID=$2; shift 2 ;;
    --abi) ABI=$2; shift 2 ;;
    -*) echo "unknown option: $1"; exit 2 ;;
    *) break ;;
  esac
done

[ -n "$BUILD_ID" ] || { echo "--build-id <the game's 32 hex digit build id> is required: the app rejects a manifest without it" >&2; sed -n 2,13p "$0"; exit 2; }
[ -n "$ABI" ] || ABI=$(default_game_abi)

copy() { cp -c "$1" "$2" 2>/dev/null || cp "$1" "$2"; }

if [ "$OS" = "linux" ]; then
  [ $# -eq 2 ] && [ -n "$INSTALL" ] || { sed -n 2,13p "$0"; exit 2; }
  SRC=$1 DEST=$2/acnh-universal-game/linux
  [ -f "$INSTALL/acnh" ] || { echo "missing $INSTALL/acnh"; exit 1; }
  for f in analysis/main.bin romfs.pak spirv.pak; do [ -f "$SRC/$f" ] || { echo "missing $SRC/$f"; exit 1; }; done
  remove_tree "$DEST.new" || { echo "error: cannot remove $DEST.new, left from an earlier run" >&2; exit 1; }
  mkdir -p "$DEST.new/root"
  copy "$INSTALL/acnh" "$DEST.new/acnh"
  find "$INSTALL" -maxdepth 1 -type f -name 'lib*.so*' | while read -r lib; do
    copy "$lib" "$DEST.new/$(basename "$lib")"
  done
  for f in romfs.pak spirv.pak; do copy "$SRC/$f" "$DEST.new/root/$f"; done
  for d in analysis fonts aoc; do
    [ -d "$SRC/$d" ] || continue
    (cd "$SRC" && find -L "$d" -type f ! -name .DS_Store) | while read -r f; do
      mkdir -p "$DEST.new/root/$(dirname "$f")"
      copy "$SRC/$f" "$DEST.new/root/$f"
    done
  done
else
  [ $# -eq 3 ] || { sed -n 2,13p "$0"; exit 2; }
  SRC=$1 LIB=$2 DEST=$3/acnh-universal-game/android
  for f in analysis/main.bin romfs.pak spirv.pak; do [ -f "$SRC/$f" ] || { echo "missing $SRC/$f"; exit 1; }; done
  [ -f "$LIB" ] || { echo "missing $LIB"; exit 1; }
  # leftovers of an interrupted run would land in the manifest
  remove_tree "$DEST.new" || { echo "error: cannot remove $DEST.new, left from an earlier run" >&2; exit 1; }
  mkdir -p "$DEST.new"
  # On APFS a clone (cp -c) costs no space until either copy changes.
  copy "$LIB" "$DEST.new/libacnh_game.so"
  for f in romfs.pak spirv.pak; do copy "$SRC/$f" "$DEST.new/$f"; done
  # mods/ (the pt-BR translation, the SpeedFix) is not staged: the APK carries them (tools/bundle_mods.sh).
  for d in analysis fonts aoc; do
    [ -d "$SRC/$d" ] || continue
    # -L: Prepare links aoc/ (the DLC) to the game folder
    (cd "$SRC" && find -L "$d" -type f ! -name .DS_Store) | while read -r f; do
      mkdir -p "$DEST.new/$(dirname "$f")"
      copy "$SRC/$f" "$DEST.new/$f"
    done
  done
fi
# The manifest last, so a half-copied folder is never mistaken for a finished one (on this computer: the Dropbox
# client may still upload it before the big files; the app then says the files are not there yet).
# It refuses an empty build id or ABI and checks every listed file's size before it writes, and nothing is swapped
# in if it fails.
write_manifest "$DEST.new" "$BUILD_ID" "$ABI" || { echo "error: no manifest.json written, $DEST left as it was" >&2; exit 1; }
chmod 644 "$DEST.new/manifest.json"
# A leftover .old must go first (mv would put DEST inside it); after the swap, failing to remove it is only a warning.
remove_tree "$DEST.old" || { echo "error: $DEST.old is left from an earlier run and cannot be removed: remove it, then run again" >&2; exit 1; }
if [ -d "$DEST" ]; then mv "$DEST" "$DEST.old"; fi
mv "$DEST.new" "$DEST"
remove_tree "$DEST.old" || echo "warning: could not remove $DEST.old (something keeps writing into it): remove it" >&2
