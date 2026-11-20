#!/usr/bin/env bash
# Package an iphoneos build for SideStore. The app is ad-hoc signed without entitlements;
# SideStore replaces that signature with the user's Apple ID signature on installation.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd -P)
usage() { echo "usage: tools/package_ios.sh <version> [<build dir>] [<out folder>]" >&2; exit 2; }
[ "$#" -ge 1 ] && [ "$#" -le 3 ] || usage
version=$1
case "$version" in ''|*[!a-zA-Z0-9._-]*|.|..) usage ;; esac
build=${2:-"$root/private/ios/build-iphoneos"}
out=${3:-"$HOME/Dropbox/_transfers/ACNH-Universal/ios"}
# Resolve symlinks and nonexistent path components before creating anything in the output directory.
out=$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$out")
case "$out/" in "$root/"*) echo "error: output folder must be outside the checkout" >&2; exit 1 ;; esac
existing=$out
while [ ! -d "$existing" ]; do existing=$(dirname "$existing"); done
git_location=$(git -C "$existing" rev-parse --is-inside-work-tree --is-inside-git-dir 2>/dev/null || true)
if printf '%s\n' "$git_location" | grep -qx true; then
    echo "error: output folder must be outside Git work trees and metadata directories" >&2; exit 1
fi
ipa=$out/ACNH-Universal-$version.ipa
if [ -L "$ipa" ] || [ -d "$ipa" ]; then
    echo "error: IPA destination must not be a symlink or directory" >&2; exit 1
fi
app=$build/ACNH.app
[ ! -L "$app" ] && [ -d "$app" ] || { echo "error: ACNH.app must be a directory, not a symlink" >&2; exit 1; }
[ ! -L "$app/Info.plist" ] || { echo "error: Info.plist must not be a symlink" >&2; exit 1; }
[ -f "$app/Info.plist" ] || { echo "error: ACNH.app/Info.plist not found" >&2; exit 1; }
exe=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$app/Info.plist")
case "$exe" in ''|.|..|*/*) echo "error: invalid CFBundleExecutable" >&2; exit 1 ;; esac
[ ! -L "$app/$exe" ] && [ -f "$app/$exe" ] || { echo "error: app executable must be a regular file, not a symlink" >&2; exit 1; }
# Require every Mach-O slice to be for iOS devices; Simulator and macOS are not installable on a device.
[ "$(lipo -archs "$app/$exe")" = arm64 ] || { echo "error: executable must be arm64" >&2; exit 1; }
platforms=$(vtool -show-build "$app/$exe" | awk '$1 == "platform" {print $2}')
if [ "$platforms" != IOS ]; then
    echo "error: executable must target iphoneos (not Simulator)" >&2; exit 1;
fi

# /tmp is outside the checkout even when TMPDIR has been set to a directory in it.
tmp=$(mktemp -d /tmp/acnh-ios-package.XXXXXXXX)
stage=
cleanup() { rm -rf "$tmp"; [ -z "$stage" ] || rm -rf "$stage"; }
trap cleanup EXIT
mkdir -p "$tmp/Payload" "$out"
cp -R "$app" "$tmp/Payload/ACNH.app"
copy=$tmp/Payload/ACNH.app
xattr -cr "$copy"
rm -rf "$copy/_CodeSignature" "$copy/embedded.mobileprovision"
# Sign nested frameworks first; no entitlement file or provisioning profile is supplied.
while IFS= read -r -d '' framework; do
    codesign --force -s - "$framework"
done < <(find "$copy" -depth -type d -name '*.framework' -print0)
codesign --force -s - "$copy/$exe"
# Signing the executable directly can create a bundle seal; SideStore will create its own.
rm -rf "$copy/_CodeSignature"
stage=$(mktemp -d "$out/.acnh-ios-package.XXXXXXXX")
(cd "$tmp" && zip -qry "$stage/package.ipa" Payload)
size=$(stat -f %z "$stage/package.ipa")
sha=$(shasum -a 256 "$stage/package.ipa" | awk '{print $1}')
# os.replace uses an exact destination and is atomic because the staging directory shares the output filesystem.
python3 -c 'import os,sys; os.replace(sys.argv[1],sys.argv[2])' "$stage/package.ipa" "$ipa"
printf 'IPA: %s\nSize: %s bytes\nSHA-256: %s\n' "$ipa" "$size" "$sha"
