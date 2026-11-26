#!/usr/bin/env bash
# Test for tools/package_source.sh, the source package release.yml puts in every launcher build (#323): the package
# makes from HEAD, holds the folders a hub's Prepare configures from (prepare/, third_party/, android/), has the same
# BUILD_VERSION fields as on any OS, reports the same version as the checkout, and CMake configures from it.
# No game file is read or written (SPEC.md M14). Runs on Linux, macOS and Windows (Git Bash).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd -P)
script=$root/tools/package_source.sh
work=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/package-source-test.XXXXXX")
trap 'rm -rf "$work"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "ok: $*"; }

# A release version, as release.yml gives it on a tag, and a dev one, as on workflow_dispatch.
for version in v0.12.0 dev-7; do
  src=$work/$version/source
  NX_RELEASE_VERSION=$version bash "$script" "$src" || fail "the package for $version was not made"
  for f in CMakeLists.txt SOURCE BUILD_VERSION Cargo.lock crates/core/Cargo.toml assets tools/build_version.sh \
           prepare/prepare.cmake prepare/io/io.cmake prepare/recomp/recomp.cmake third_party/capstone \
           third_party/save-relay android/AndroidManifest.xml android/java android/res; do
    [ -e "$src/$f" ] || fail "$f is missing from the package ($version)"
  done
  [ ! -e "$src/.git" ] || fail "the package holds a .git"
  [ "$(cat "$src/SOURCE")" = "$(git -C "$root" rev-parse HEAD)" ] || fail "SOURCE is not HEAD"
  # The same fields, in the same order, on every OS: BASE_VER, COMMITS, VNAME, VCODE.
  keys=$(sed -n 's/^\([A-Z_]*\)=.*/\1/p' "$src/BUILD_VERSION" | tr '\n' ' ')
  [ "$keys" = "BASE_VER COMMITS VNAME VCODE " ] || fail "BUILD_VERSION fields ($version): $keys"
  commits=$(sed -n 's/^COMMITS=//p' "$src/BUILD_VERSION")
  [ "$commits" = "$(git -C "$root" rev-list --count HEAD)" ] || fail "COMMITS is not the commit count: $commits"
  [ "$commits" -gt 0 ] || fail "COMMITS is 0"
done
[ "$(sed -n 's/^BASE_VER=//p' "$work/v0.12.0/source/BUILD_VERSION")" = 0.12.0 ] || fail "a tag version is not BASE_VER"
[ "$(sed -n 's/^VNAME=//p' "$work/v0.12.0/source/BUILD_VERSION")" = 0.12.0 ] || fail "a tag version is not VNAME"
pass "package holds prepare/, third_party/, android/ and a BUILD_VERSION with four fields"

# The packaged source (no .git) computes the same version as the checkout does.
want=$(cd "$root" && NX_RELEASE_VERSION=v0.12.0 bash tools/build_version.sh | sed -n 's/^VCODE=//p')
got=$(cd "$work/v0.12.0/source" && bash tools/build_version.sh | sed -n 's/^VCODE=//p')
[ "$got" = "$want" ] || fail "a packaged source's VCODE is $got, the checkout's $want"
pass "packaged VCODE matches the checkout's"

# A name that is not tracked is refused, not skipped.
if ACNH_SOURCE_COMMIT=4b825dc642cb6eb9a060e54bf8d69288fbee4904 bash "$script" "$work/empty" >/dev/null 2>&1; then
  fail "an empty commit made a package"
fi
pass "a commit without the source folders is refused"

# CMake configures from the packaged source, as the launcher's Prepare does (the unit tests are not packaged).
if command -v cmake >/dev/null 2>&1; then
  gen=()
  if command -v ninja >/dev/null 2>&1; then gen=(-G Ninja); fi
  cmake -S "$work/dev-7/source" -B "$work/build" "${gen[@]}" -DNX_GAME=acnh -DCMAKE_BUILD_TYPE=Release >"$work/cmake.log" 2>&1 ||
    { tail -30 "$work/cmake.log" >&2; fail "CMake does not configure from the packaged source"; }
  pass "CMake configures from the packaged source"
else
  echo "skipped: cmake is not installed"
fi
