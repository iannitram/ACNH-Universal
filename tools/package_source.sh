#!/usr/bin/env bash
# The source package release.yml puts in every launcher build (SPEC.md M13): the files a hub's Prepare builds the game
# from, made with `git archive` (so no .git, no untracked file), plus SOURCE (the commit) and BUILD_VERSION.
# One script for all three OSes, so the three packages cannot drift.
# usage: tools/package_source.sh <dest folder>          (run from a git checkout; the folder is made if absent)
# env:   NX_RELEASE_VERSION  the release's version (a tag such as v0.12.0, or dev-<n>); as for tools/build_version.sh
#        ACNH_SOURCE_COMMIT  the commit the package is made from (default: HEAD)
# BUILD_VERSION holds BASE_VER, COMMITS (the commit count, which a package without .git cannot count), VNAME and VCODE.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
die() { echo "package_source.sh: $*" >&2; exit 1; }
[ $# -eq 1 ] && [ -n "$1" ] || die "usage: tools/package_source.sh <dest folder>"
dest=$1
cd "$root"
commit=${ACNH_SOURCE_COMMIT:-$(git rev-parse HEAD)} || die "run it from a git checkout"

# What CMakeLists.txt and the tools read from the source tree: the engine (runtime, games, assets), the launcher
# (Cargo.*, crates, app), the preparation (prepare/, which includes prepare/recomp/recomp.cmake over
# third_party/capstone), third_party/save-relay and android/ (the Android build), and tools/. ios/ is optional.
# git archive fails when a name here is not tracked, so a deleted folder cannot go unnoticed again.
paths=(CMakeLists.txt runtime games prepare third_party android tools assets Cargo.toml Cargo.lock crates app)
if git cat-file -e "$commit:ios" 2>/dev/null; then paths+=(ios); fi

mkdir -p "$dest"
git archive "$commit" "${paths[@]}" | tar -x -f - -C "$dest"
printf '%s\n' "$commit" > "$dest/SOURCE"

# The source has no .git: Prepare reads the package's build version from here (BASE_VER, VCODE; tools/build_version.sh
# reads BASE_VER and COMMITS). Same fields on every OS.
v=$(NX_RELEASE_VERSION="${NX_RELEASE_VERSION:-}" bash tools/build_version.sh)
base=$(sed -n 's/^BASE_VER=//p' <<<"$v")
{
  printf 'BASE_VER=%s\nCOMMITS=%s\n' "${base%%[-+]*}" "$(git rev-list --count "$commit")"
  grep -E '^(VNAME|VCODE)=' <<<"$v"
} > "$dest/BUILD_VERSION"

for f in CMakeLists.txt SOURCE BUILD_VERSION Cargo.lock crates/core/Cargo.toml assets \
         prepare/prepare.cmake prepare/io/io.cmake prepare/recomp/recomp.cmake \
         third_party/capstone third_party/save-relay android/AndroidManifest.xml tools/build_version.sh; do
  test -e "$dest/$f" || die "$f is missing from the package"
done
