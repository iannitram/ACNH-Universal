#!/usr/bin/env bash
# Package an iphoneos build as an .ipa for SideStore (8.03d, and the game inside it, #172). The app is ad-hoc signed
# without entitlements; SideStore replaces that signature with the user's Apple ID signature on installation.
#
#   tools/package_ios.sh <version> <build dir> --game <prepared folder> <out folder>
#   tools/package_ios.sh <version> <build dir> --no-game <out folder>
#
# --game copies only analysis/, romfs.pak, spirv.pak and, when present, fonts/ and aoc/ from the prepared folder into
# Payload/ACNH.app/game/, as real files, and makes them read-only. --no-game makes no game/ folder.
# The out folder is required and has no default. Every refusal writes nothing to it.
# The .ipa is staged in <out>/.ACNH-Universal-<version>.partial/ and moved into <out> when complete.
# Under CI (CI or GITHUB_ACTIONS set) the out folder must be inside $RUNNER_TEMP, and the game folder may hold only
# empty files.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd -P)

usage() {
    echo "usage: tools/package_ios.sh <version> <build dir> --game <prepared folder> <out folder>" >&2
    echo "       tools/package_ios.sh <version> <build dir> --no-game <out folder>" >&2
    exit 2
}
fail() { echo "error: $*" >&2; exit 1; }
realpath_py() { python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$1"; }

[ "$#" -ge 3 ] || usage
version=$1
build=$2
shift 2
case "$version" in ''|*[!a-zA-Z0-9._-]*|.|..) usage ;; esac
game=
case "$1" in
    --game)
        [ "$#" -ge 2 ] || usage
        game=$2
        shift 2
        ;;
    --no-game)
        shift
        ;;
    *) usage ;;
esac
[ "$#" -ge 1 ] || fail "the out folder is required: give it as the last argument (it has no default)"
[ "$#" -eq 1 ] || usage
[ -n "$1" ] || fail "the out folder is required: give it as the last argument (it has no default)"
out=$(realpath_py "$1")
case "$out/" in "$root/"*) fail "out folder must be outside the checkout: $out" ;; esac

# Refusals come before anything is created, so they leave the out folder untouched.
[ ! -e "$out" ] || [ -d "$out" ] || fail "out folder is not a folder: $out"
existing=$out
while [ ! -d "$existing" ]; do existing=$(dirname "$existing"); done
# Fail closed: any git error other than "not a git repository" refuses the folder.
if git_location=$(git -C "$existing" rev-parse --is-inside-work-tree --is-inside-git-dir 2>"${TMPDIR:-/tmp}/.package_ios_git.$$"); then
    git_err=
else
    git_err=$(cat "${TMPDIR:-/tmp}/.package_ios_git.$$")
    case "$git_err" in
        *"not a git repository"*) git_location= ;;
        *) rm -f "${TMPDIR:-/tmp}/.package_ios_git.$$"; fail "cannot tell whether the out folder is inside a Git work tree: $git_err" ;;
    esac
fi
rm -f "${TMPDIR:-/tmp}/.package_ios_git.$$"
if printf '%s\n' "$git_location" | grep -qx true; then
    fail "out folder is inside a Git work tree (or its metadata): choose a folder outside every checkout"
fi
ci=
if [ -n "${CI:-}" ] || [ -n "${GITHUB_ACTIONS:-}" ]; then ci=1; fi
if [ -n "$ci" ]; then
    [ -n "${RUNNER_TEMP:-}" ] || fail "CI is set but RUNNER_TEMP is not, so the out folder cannot be checked"
    runner_temp=$(realpath_py "$RUNNER_TEMP")
    case "$out/" in
        "$runner_temp/"*) ;;
        *) fail "under CI the out folder must be inside \$RUNNER_TEMP ($RUNNER_TEMP)" ;;
    esac
fi

name=ACNH-Universal-$version
ipa=$out/$name.ipa
if [ -L "$ipa" ] || [ -d "$ipa" ]; then
    fail "IPA destination must not be a symlink or directory"
fi
stage_dir=$out/.ACNH-Universal-$version.partial
if [ -e "$stage_dir" ] || [ -L "$stage_dir" ]; then
    fail "$stage_dir is left from an earlier run: run chmod -R u+w on it, then remove it"
fi

app=$build/ACNH.app
[ ! -L "$app" ] && [ -d "$app" ] || fail "ACNH.app must be a directory, not a symlink"
[ ! -L "$app/Info.plist" ] || fail "Info.plist must not be a symlink"
[ -f "$app/Info.plist" ] || fail "ACNH.app/Info.plist not found"
exe=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$app/Info.plist")
case "$exe" in ''|.|..|*/*) fail "invalid CFBundleExecutable" ;; esac
[ ! -L "$app/$exe" ] && [ -f "$app/$exe" ] || fail "app executable must be a regular file, not a symlink"
# Require every Mach-O slice to be for iOS devices; Simulator and macOS are not installable on a device.
[ "$(lipo -archs "$app/$exe")" = arm64 ] || fail "executable must be arm64"
platforms=$(vtool -show-build "$app/$exe" | awk '$1 == "platform" {print $2}')
[ "$platforms" = IOS ] || fail "executable must target iphoneos (not Simulator)"

if [ -n "$game" ]; then
    [ -d "$game" ] || fail "game folder not found: $game"
    for need in analysis/main.bin romfs.pak spirv.pak; do
        [ -f "$game/$need" ] || fail "game folder lacks $need, so it is not a prepared game: $game"
    done
    if [ -n "$ci" ] && [ -n "$(find -L "$game" -type f -size +0c -print -quit)" ]; then
        fail "under CI the game folder may hold only empty files, and this one has a non-empty file"
    fi
fi

stage=
made_out=
[ -d "$out" ] || made_out=1
cleanup() {
    if [ -n "$stage" ] && [ -e "$stage" ]; then
        chmod -R u+w "$stage" 2>/dev/null || true
        rm -rf "$stage"
    fi
    if [ -n "$made_out" ]; then rmdir "$out" 2>/dev/null || true; fi
    return 0
}
trap cleanup EXIT
trap 'exit 130' INT TERM
mkdir -p "$out"
stage=$stage_dir
mkdir "$stage"
mkdir "$stage/Payload"

copy=$stage/Payload/ACNH.app
cp -R "$app" "$copy"
xattr -cr "$copy"
rm -rf "$copy/_CodeSignature" "$copy/embedded.mobileprovision"
if [ -n "$game" ]; then
    mkdir "$copy/game"
    for item in analysis romfs.pak spirv.pak fonts aoc; do
        if [ -e "$game/$item" ]; then
            cp -RL "$game/$item" "$copy/game/"
        fi
    done
    chmod -R a-w "$copy/game"
fi
# Sign nested frameworks first; no entitlement file or provisioning profile is supplied. game/ holds no frameworks.
while IFS= read -r -d '' framework; do
    codesign --force -s - "$framework"
done < <(find "$copy" -path "$copy/game" -prune -o -depth -type d -name '*.framework' -print0)
codesign --force -s - "$copy/$exe"
# Signing the executable directly can create a bundle seal; SideStore will create its own.
rm -rf "$copy/_CodeSignature"

partial=$name.ipa.partial
(cd "$stage" && zip -qry "$partial" Payload)

# Check the archive itself before it gets its final name: game/ holds only the five names, and the rest passes the
# release check. The file list is never printed.
list=$stage/entries.txt
zipinfo -1 "$stage/$partial" > "$list"
game_bad=$(awk -F/ '$1 == "Payload" && $2 == "ACNH.app" && $3 == "game" {
    if ($4 == "") { if ($0 != "Payload/ACNH.app/game/") print }
    else if ($4 != "analysis" && $4 != "romfs.pak" && $4 != "spirv.pak" && $4 != "fonts" && $4 != "aoc") print
}' "$list")
[ -z "$game_bad" ] || fail "game/ holds a name outside analysis, romfs.pak, spirv.pak, fonts and aoc"
if [ -n "$game" ]; then
    for need in analysis/main.bin romfs.pak spirv.pak; do
        grep -Fqx "Payload/ACNH.app/game/$need" "$list" || fail "the archive has no game/$need"
    done
elif grep -q '^Payload/ACNH\.app/game' "$list"; then
    fail "the archive has a game/ folder although --no-game was given"
fi
outside=$(awk -F/ '!($1 == "Payload" && $2 == "ACNH.app" && $3 == "game") { print }' "$list")
if ! check=$(printf '%s\n' "$outside" | bash "$root/tools/check_release_files.sh" 2>&1); then
    printf '%s\n' "$check" >&2
    fail "the archive outside game/ does not pass tools/check_release_files.sh"
fi

mv -f "$stage/$partial" "$ipa"
size=$(stat -f %z "$ipa")
sha=$(shasum -a 256 "$ipa" | awk '{print $1}')
printf 'IPA: %s\nSize: %s bytes\nSHA-256: %s\n' "$ipa" "$size" "$sha"
