#!/usr/bin/env bash
# Compute distinct build version values for Android (versionName, versionCode) and iOS (CFBundleShortVersionString,
# CFBundleVersion) from git state and optional NX_RELEASE_VERSION.
#
# Sets:
#   BASE_VER  - base semver without leading 'v' (e.g. 0.11.0, or 0.0.0 when no git tags exist)
#   VNAME     - display version: clean <BASE_VER> for an exact release tag or clean NX_RELEASE_VERSION,
#               otherwise <BASE_VER>+<short-sha>[-dirty]
#   VCODE     - monotonically rising integer >= 1100 (and <= 2100000000):
#               BASE_CODE * 1000 + COMMIT_COUNT when BASE_CODE > 0, else 10000 + COMMIT_COUNT

# Git is trusted only when this script's own folder is the top of a work tree. A packaged source (release.yml's
# `git archive` copy, which the launcher copies under its data folder) has no .git, and a parent folder may be some
# other repository: there BASE_VER and the commit count come from the BUILD_VERSION file the release wrote beside
# SOURCE (BASE_VER=... and COMMITS=... lines), else the fallbacks 0.0.0 and 10000.
_bv_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
_bv_top=$(git -C "$_bv_root" rev-parse --show-toplevel 2>/dev/null || true)
[ -z "$_bv_top" ] || _bv_top=$(cd "$_bv_top" 2>/dev/null && pwd -P || true)
_bv_git=0
[ "$_bv_top" = "$_bv_root" ] && _bv_git=1
_bv_g() { if [ "$_bv_git" = 1 ]; then git -C "$_bv_root" "$@"; else return 1; fi; }
_bv_file_ver=""
_bv_file_commits=""
if [ "$_bv_git" = 0 ] && [ -f "$_bv_root/BUILD_VERSION" ]; then
  _bv_file_ver=$(sed -n 's/^BASE_VER=//p' "$_bv_root/BUILD_VERSION" | head -n 1)
  _bv_file_commits=$(sed -n 's/^COMMITS=//p' "$_bv_root/BUILD_VERSION" | head -n 1)
  [[ $_bv_file_ver =~ ^[0-9]+(\.[0-9]+){1,2}$ ]] || _bv_file_ver=""
  [[ $_bv_file_commits =~ ^[0-9]+$ ]] || _bv_file_commits=""
fi

_bv_tag=$(_bv_g describe --tags --abbrev=0 2>/dev/null || true)
# A tag that is not a version (this repo's `mods`, a stray `laptop`) is not a base version.
[[ $_bv_tag =~ ^v?[0-9]+(\.[0-9]+){1,2}([-+].*)?$ ]] || _bv_tag=""
BASE_VER=${_bv_tag#v}
BASE_VER=${BASE_VER:-${_bv_file_ver:-0.0.0}}

_bv_is_release=0
if [[ ${NX_RELEASE_VERSION:-} =~ ^v?[0-9]+\.[0-9]+ ]]; then
  BASE_VER=${NX_RELEASE_VERSION#v}
  # Keep a clean versionName only for an exact release version (no pre-release/dev suffix).
  if [[ $BASE_VER =~ ^[0-9]+\.[0-9]+(\.[0-9]+)?$ ]]; then
    _bv_is_release=1
  fi
elif [ -n "$_bv_tag" ] \
  && _bv_g describe --tags --exact-match HEAD >/dev/null 2>&1 \
  && _bv_g diff-index --quiet HEAD -- 2>/dev/null; then
  _bv_is_release=1
fi

_bv_sha=$(_bv_g rev-parse --short HEAD 2>/dev/null || true)
[ -n "$_bv_sha" ] || _bv_sha=$(head -c 7 "$_bv_root/SOURCE" 2>/dev/null || true)
_bv_sha=${_bv_sha:-unknown}
_bv_dirty=""
if [ "$_bv_git" = 1 ] && ! _bv_g diff-index --quiet HEAD -- 2>/dev/null; then
  _bv_dirty="-dirty"
fi

if [ "$_bv_is_release" = 1 ]; then
  VNAME=$BASE_VER
else
  VNAME="${BASE_VER%%+*}+${_bv_sha}${_bv_dirty}"
fi

_bv_core=${BASE_VER%%-*}
_bv_core=${_bv_core%%+*}
IFS=. read -r _bv_maj _bv_min _bv_pat <<<"$_bv_core"
_bv_maj=$(( 10#${_bv_maj:-0} ))
_bv_min=$(( 10#${_bv_min:-0} ))
_bv_pat=$(( 10#${_bv_pat:-0} ))
_bv_base_code=$(( _bv_maj * 10000 + _bv_min * 100 + _bv_pat ))
_bv_commits=$(_bv_g rev-list --count HEAD 2>/dev/null || echo "${_bv_file_commits:-0}")
_bv_commits=$(( 10#${_bv_commits:-0} ))

if [ "$_bv_base_code" -gt 0 ]; then
  VCODE=$(( _bv_base_code * 1000 + _bv_commits ))
else
  VCODE=$(( 10000 + _bv_commits ))
fi

if [ "${BASH_SOURCE[0]:-}" = "$0" ]; then
  printf 'BASE_VER=%s\nVNAME=%s\nVCODE=%s\n' "$BASE_VER" "$VNAME" "$VCODE"
fi
