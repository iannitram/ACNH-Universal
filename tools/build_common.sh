# Helpers the build scripts share (sourced, never run): tools/build_android.sh, tools/build_ios.sh, tools/build_turnip.sh.
# bash only; uses no game data (SPEC.md M14).

# newest_runtime_header <build dir>: the newest acnh_runtime.h (by modification time) under the dir. Cargo can leave
# several build/acnh-runtime-<hash>/out folders; the first one find lists may be a stale one (issue #369).
# Prints nothing when there is none.
newest_runtime_header() {
  local best="" f
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    if [ -z "$best" ] || [ "$f" -nt "$best" ]; then best=$f; fi
  done < <(find "$1" -name acnh_runtime.h -print 2>/dev/null)
  if [ -n "$best" ]; then printf '%s\n' "$best"; fi
  return 0
}

# take_build_lock <lock dir>: waits for the lock (a directory holding its owner's pid) and takes it. A lock whose owner
# process is gone (SIGKILL, OOM kill, power loss) is reclaimed instead of waited on for ever (issue #369).
# release_build_lock <lock dir> gives it back; it only removes a lock this process owns.
take_build_lock() {
  local lock=$1 owner tries=0
  while ! mkdir "$lock" 2>/dev/null; do
    owner=$(cat "$lock/pid" 2>/dev/null || true)
    if [ -z "$owner" ]; then
      # Just created by someone who has not written the pid yet, or left empty by a kill in between: give it a moment.
      tries=$((tries + 1))
      if [ "$tries" -ge 3 ] && [ -d "$lock" ]; then
        reclaim_build_lock "$lock" "no owner recorded"
        tries=0
        continue
      fi
    elif ! kill -0 "$owner" 2>/dev/null; then
      reclaim_build_lock "$lock" "owner $owner is gone"
      tries=0
      continue
    else
      tries=0
      echo "waiting for another build (pid $owner)"
    fi
    sleep "${BUILD_LOCK_POLL:-10}"
  done
  printf '%s\n' "$$" > "$lock/pid"
}

# The rename is atomic, so when several waiters find the same dead lock only one of them wins it.
reclaim_build_lock() {
  local stale="$1.stale.$$"
  if mv "$1" "$stale" 2>/dev/null; then
    echo "reclaimed the build lock ($2)"
    rm -rf "$stale"
  fi
}

release_build_lock() {
  if [ "$(cat "$1/pid" 2>/dev/null || true)" = "$$" ]; then rm -rf "$1"; fi
  return 0
}

# apply_patch_once <patch> (run inside the source tree): applies the patch; a patch that is already applied is skipped,
# and one that neither applies nor is already applied stops the build, naming it (issue #369).
apply_patch_once() {
  local p=$1
  if git apply --check "$p" 2>/dev/null; then
    git apply "$p" && echo "applied $(basename "$p")"
  elif git apply --reverse --check "$p" 2>/dev/null; then
    echo "already applied $(basename "$p")"
  else
    echo "error: $(basename "$p") neither applies to this source nor is already applied; the build would lack it" >&2
    return 1
  fi
}
