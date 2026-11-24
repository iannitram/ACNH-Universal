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

_bv_tag=$(git describe --tags --abbrev=0 2>/dev/null || true)
BASE_VER=${_bv_tag#v}
BASE_VER=${BASE_VER:-0.0.0}

_bv_is_release=0
if [[ ${NX_RELEASE_VERSION:-} =~ ^v?[0-9]+\.[0-9]+ ]]; then
  BASE_VER=${NX_RELEASE_VERSION#v}
  # Keep a clean versionName only for an exact release version (no pre-release/dev suffix).
  if [[ $BASE_VER =~ ^[0-9]+\.[0-9]+(\.[0-9]+)?$ ]]; then
    _bv_is_release=1
  fi
elif [ -n "$_bv_tag" ] \
  && git describe --tags --exact-match HEAD >/dev/null 2>&1 \
  && git diff-index --quiet HEAD -- 2>/dev/null; then
  _bv_is_release=1
fi

_bv_sha=$(git rev-parse --short HEAD 2>/dev/null || echo unknown)
_bv_dirty=""
if ! git diff-index --quiet HEAD -- 2>/dev/null; then
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
_bv_commits=$(git rev-list --count HEAD 2>/dev/null || echo 0)
_bv_commits=$(( 10#${_bv_commits:-0} ))

if [ "$_bv_base_code" -gt 0 ]; then
  VCODE=$(( _bv_base_code * 1000 + _bv_commits ))
else
  VCODE=$(( 10000 + _bv_commits ))
fi

if [ "${BASH_SOURCE[0]:-}" = "$0" ]; then
  printf 'BASE_VER=%s\nVNAME=%s\nVCODE=%s\n' "$BASE_VER" "$VNAME" "$VCODE"
fi
