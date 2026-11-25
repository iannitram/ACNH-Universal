#!/usr/bin/env bash
# Package the Mac's one contained app, ACNH-Universal.app, with the prepared game inside, as
# ACNH-Universal-<version>.zip for the owner's Dropbox (#171, SPEC.md M4, M14).
# Run only from the launcher's prepare (the app's Prepare, or `acnh-universal build --target
# mac`); nobody runs it by hand, and no workflow names it. CI uses it only on stand-ins:
# `--no-game`, or a game folder of empty files.
# usage: tools/package_mac.sh <version> <app> <game folder>|--no-game <out folder> [--mods <mods folder>]
# --mods: the folder tools/bundle_mods.sh makes (bundle.txt, ptbr/, speedfix60/). It goes to Contents/Resources/mods,
# where the player's first start copies the SpeedFix's files into its data root (video.frame_rate = 60 needs them).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd -P)
usage() { echo "usage: tools/package_mac.sh <version> <app> <game folder>|--no-game <out folder> [--mods <mods folder>]" >&2; exit 2; }
die() { echo "error: $*" >&2; exit 1; }
[ "$#" -eq 4 ] || { [ "$#" -eq 6 ] && [ "$5" = --mods ]; } || usage
version=$1
case "$version" in ''|*[!a-zA-Z0-9._-]*|.|..) usage ;; esac
real() { python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$1"; }
[ -n "$4" ] || die "the out folder is required; this script has no default"
app=$(real "$2")
out=$(real "$4")
game=
[ "$3" = --no-game ] || game=$(real "$3")
mods=
[ "$#" -eq 6 ] && { [ -n "$6" ] || die "the mods folder is empty"; mods=$(real "$6"); }
ci=${CI:-}${GITHUB_ACTIONS:-}

# --- the out folder: nothing is written until every refusal has been checked ---
existing=$out
while [ ! -d "$existing" ]; do existing=$(dirname "$existing"); done
git_location=$(git -C "$existing" rev-parse --is-inside-work-tree --is-inside-git-dir 2>/dev/null || true)
if printf '%s\n' "$git_location" | grep -qx true; then
  die "the out folder must be outside every git work tree and git folder"
fi
case "$out/" in "$app/"*) die "the out folder must not be inside the app" ;; esac
if [ -n "$game" ]; then
  case "$out/" in "$game/"*) die "the out folder must not be inside the game folder" ;; esac
fi
if [ -n "$ci" ]; then
  [ -n "${RUNNER_TEMP:-}" ] || die "under CI the out folder must be inside \$RUNNER_TEMP, which is not set"
  temp=$(real "$RUNNER_TEMP")
  case "$out/" in "$temp/"*) ;; *) die "under CI the out folder must be inside \$RUNNER_TEMP" ;; esac
fi
zip=$out/ACNH-Universal-$version.zip
if [ -L "$zip" ] || [ -d "$zip" ]; then die "the zip destination must not be a link or a folder"; fi

# --- the app ---
[ ! -L "$app" ] && [ -d "$app/Contents" ] || die "the app must be a folder with a Contents folder"
[ -f "$app/Contents/Info.plist" ] || die "the app has no Contents/Info.plist"
[ ! -e "$app/Contents/Resources/game" ] || die "the app already holds a Contents/Resources/game folder"
[ ! -e "$app/Contents/Resources/mods" ] || [ -z "$mods" ] || die "the app already holds a Contents/Resources/mods folder"
if [ -n "$mods" ]; then
  case "$out/" in "$mods/"*) die "the out folder must not be inside the mods folder" ;; esac
  bash "$root/tools/check_bundled_mods.sh" "$mods" || die "the mods folder is refused"
fi

# --- the game folder ---
if [ -n "$game" ]; then
  [ -d "$game" ] || die "the game folder is not a folder"
  for need in analysis/main.bin romfs.pak spirv.pak; do
    [ -e "$game/$need" ] || die "the game folder has no $need"
  done
  if [ -n "$ci" ] && [ -n "$(find -L "$game" -type f -size +0 -print -quit)" ]; then
    die "under CI a game folder may hold only empty files; this one holds a file that is not empty"
  fi
fi

# --- every Mach-O in the app is arm64 only; the rest passes the release file check ---
while IFS= read -r -d '' f; do
  if file -b "$f" | grep -q 'Mach-O'; then
    [ "$(lipo -archs "$f" 2>/dev/null)" = arm64 ] || die "a Mach-O file in the app is not arm64 only"
  fi
done < <(find "$app" -type f -print0)
(cd "$app" && find . \( -type f -o -type l \)) | bash "$root/tools/check_release_files.sh" >/dev/null ||
  die "the app's file list fails tools/check_release_files.sh"

# --- stage, sign, zip ---
stage=$out/.ACNH-Universal-$version.partial
partial=$zip.partial
cleanup() {
  [ ! -e "$stage" ] || { chmod -R u+w "$stage" 2>/dev/null || true; rm -rf "$stage"; }
  rm -f "$partial"
}
trap cleanup EXIT
mkdir -p "$out"
cleanup
mkdir "$stage"
bundle=$stage/ACNH-Universal.app
ditto "$app" "$bundle"
rm -rf "$bundle/Contents/_CodeSignature"

if [ -n "$game" ]; then
  dest=$bundle/Contents/Resources/game
  mkdir -p "$dest"
  for name in analysis romfs.pak spirv.pak fonts aoc; do
    # Copied as files (through any link), never as links back into the game folder.
    if [ -e "$game/$name" ]; then cp -RL "$game/$name" "$dest/$name"; fi
  done
  for entry in "$dest"/*; do
    case "$(basename "$entry")" in analysis|romfs.pak|spirv.pak|fonts|aoc) ;; *) die "game/ holds a name that is not allowed" ;; esac
  done
  chmod -R a-w "$dest"
fi

if [ -n "$mods" ]; then
  # Plain copies (no links, no extended attributes); the folder passed check_bundled_mods.sh above.
  mkdir -p "$bundle/Contents/Resources"
  ditto --norsrc --noextattr --noqtn "$mods" "$bundle/Contents/Resources/mods"
  find "$bundle/Contents/Resources/mods" -name .DS_Store -delete
  # The release check again, over what is in the app now (everything but the game): the one path it lets pass is mods/.
  (cd "$stage" && find ACNH-Universal.app -path ACNH-Universal.app/Contents/Resources/game -prune -o \( -type f -o -type l \) -print) |
    bash "$root/tools/check_release_files.sh" >/dev/null || die "the package's file list fails tools/check_release_files.sh"
fi

# Sign each binary first, then the bundle, so the signature seals the game files too.
while IFS= read -r -d '' f; do
  if file -b "$f" | grep -q 'Mach-O'; then codesign --force -s - "$f" >/dev/null 2>&1; fi
done < <(find "$bundle" -path "$bundle/Contents/Resources/game" -prune -o -type f -print0)
codesign --force -s - "$bundle" >/dev/null 2>&1
codesign --verify --deep --strict "$bundle" || die "the signature does not verify"

# `ditto -c -k` unpacks a 4 GB file correctly but writes its size modulo 4 GiB into the zip's
# central directory (tests/package_mac_test.sh), which other readers trust; `zip` writes ZIP64
# sizes, keeps modes (the read-only game) and, with -y, links.
(cd "$stage" && zip -qry "$partial" ACNH-Universal.app)
mv -f "$partial" "$zip"

size=$(stat -f %z "$zip")
sha=$(shasum -a 256 "$zip" | awk '{print $1}')
printf 'Zip: %s\nSize: %s bytes\nSHA-256: %s\n' "$zip" "$size" "$sha"
