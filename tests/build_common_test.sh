#!/usr/bin/env bash
# Tests for tools/build_common.sh (#369): the newest runtime header wins, a build lock whose owner is gone is
# reclaimed (a live owner's is not), and a Turnip patch that neither applies nor is already applied stops the build.
# Made-up files only, never game data (SPEC.md M14).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd -P)
work=$(mktemp -d "${TMPDIR:-/tmp}/build-common-test.XXXXXX")
trap 'rm -rf "$work"' EXIT
die() { echo "FAIL: $*" >&2; exit 1; }
say() { echo "ok: $*"; }
# shellcheck source=tools/build_common.sh
. "$root/tools/build_common.sh"
export BUILD_LOCK_POLL=0.1

# ---- the newest header -------------------------------------------------------------------------------------------
b=$work/build
for h in aaa zzz mmm; do mkdir -p "$b/acnh-runtime-$h/out/include"; echo "$h" > "$b/acnh-runtime-$h/out/include/acnh_runtime.h"; done
touch -t 202001010000 "$b/acnh-runtime-aaa/out/include/acnh_runtime.h"
touch -t 202301010000 "$b/acnh-runtime-zzz/out/include/acnh_runtime.h"
touch -t 202501010000 "$b/acnh-runtime-mmm/out/include/acnh_runtime.h"
[ "$(newest_runtime_header "$b")" = "$b/acnh-runtime-mmm/out/include/acnh_runtime.h" ] || die "did not pick the newest header"
say "the newest header is picked whatever order find lists them in"
[ -z "$(newest_runtime_header "$work/none")" ] || die "a header from nowhere"
say "no header gives an empty answer"

# ---- the lock ----------------------------------------------------------------------------------------------------
lock=$work/build.lock
take_build_lock "$lock"
[ "$(cat "$lock/pid")" = "$$" ] || die "the lock does not name its owner"
release_build_lock "$lock"
[ ! -e "$lock" ] || die "the lock was not released"
say "the lock names its owner and is released"

sleep 300 & dead=$!
kill -9 "$dead"; wait "$dead" 2>/dev/null || true
mkdir "$lock"; echo "$dead" > "$lock/pid"
out=$(take_build_lock "$lock")
[ "$(cat "$lock/pid")" = "$$" ] || die "a lock of a dead owner was not taken"
echo "$out" | grep -q reclaimed || die "no message about the reclaim: $out"
release_build_lock "$lock"
say "a lock whose owner was killed is reclaimed"

mkdir "$lock" # killed between mkdir and writing the pid
out=$(take_build_lock "$lock")
[ "$(cat "$lock/pid")" = "$$" ] || die "an empty lock was not taken"
release_build_lock "$lock"
say "a lock with no recorded owner is reclaimed"

sleep 300 & live=$!
mkdir "$lock"; echo "$live" > "$lock/pid"
bash -c ". '$root/tools/build_common.sh'; take_build_lock '$lock'" >/dev/null 2>&1 & waiter=$!
sleep 2
if ! kill -0 "$waiter" 2>/dev/null; then kill "$live"; die "took a lock held by a live process"; fi
kill "$waiter"; wait "$waiter" 2>/dev/null || true
[ "$(cat "$lock/pid")" = "$live" ] || { kill "$live"; die "a live owner's lock was changed"; }
kill "$live"; wait "$live" 2>/dev/null || true
rm -rf "$lock"
say "a lock held by a live process is waited for, not taken"

# release does not remove a lock owned by someone else
mkdir "$lock"; echo 1 > "$lock/pid"
release_build_lock "$lock"
[ -d "$lock" ] || die "released a lock this process does not own"
rm -rf "$lock"
say "another process's lock is not released"

# ---- the patches -------------------------------------------------------------------------------------------------
src=$work/src
mkdir "$src"
git -C "$src" init -q
printf 'one\ntwo\nthree\n' > "$src/f.txt"
git -C "$src" add f.txt
git -C "$src" -c user.name=t -c user.email=t@example.invalid commit -q -m base
printf 'one\nTWO\nthree\n' > "$work/new.txt"
diff -u --label a/f.txt --label b/f.txt "$src/f.txt" "$work/new.txt" > "$work/good.patch" || true
printf 'x\ny\nz\n' > "$work/other.txt"
printf 'x\nY\nz\n' > "$work/other2.txt"
diff -u --label a/f.txt --label b/f.txt "$work/other.txt" "$work/other2.txt" > "$work/bad.patch" || true
cd "$src"
apply_patch_once "$work/good.patch" | grep -q '^applied' || die "a fresh patch was not applied"
grep -q TWO f.txt || die "the patch changed nothing"
apply_patch_once "$work/good.patch" | grep -q 'already applied' || die "an applied patch was not skipped"
say "a patch is applied once and then skipped"
if apply_patch_once "$work/bad.patch" 2>"$work/err"; then die "a patch that does not apply passed"; fi
grep -q 'bad.patch' "$work/err" || die "the failing patch is not named"
say "a patch that neither applies nor is applied fails and is named"
echo "all passed"
