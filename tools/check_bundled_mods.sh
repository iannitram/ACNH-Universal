#!/usr/bin/env bash
# The bundled mods folder an Apple package carries (tools/bundle_mods.sh makes it; package_mac.sh and package_ios.sh
# copy it beside the player): refuses anything but bundle.txt and the two mods' folders, each holding only romfs/,
# and any link. The mod files are the user's own prepared data: never committed, never named here.
# usage: tools/check_bundled_mods.sh <mods folder>
set -euo pipefail
dir=${1:?usage: tools/check_bundled_mods.sh <mods folder>}
die() { echo "error: $*" >&2; exit 1; }
[ -d "$dir" ] && [ ! -L "$dir" ] || die "the mods folder is not a folder"
[ -s "$dir/bundle.txt" ] && [ ! -L "$dir/bundle.txt" ] || die "the mods folder has no bundle.txt"
[ -z "$(find "$dir" -type l -print -quit)" ] || die "the mods folder holds a link"
for entry in "$dir"/* "$dir"/.[!.]*; do
  [ -e "$entry" ] || continue
  case "$(basename "$entry")" in
    bundle.txt|.DS_Store) ;;
    ptbr|speedfix60)
      [ -d "$entry/romfs" ] || die "the mods folder's $(basename "$entry") has no romfs folder"
      for sub in "$entry"/* "$entry"/.[!.]*; do
        [ -e "$sub" ] || continue
        [ "$(basename "$sub")" = romfs ] || die "the mods folder's $(basename "$entry") holds more than romfs"
      done ;;
    *) die "the mods folder holds a name that is not allowed" ;;
  esac
done
[ -d "$dir/speedfix60/romfs" ] || die "the mods folder has no speedfix60/romfs, so video.frame_rate = 60 would stay at 30"
