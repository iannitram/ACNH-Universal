#!/usr/bin/env bash
# Tests for tools/stage_dropbox_game.sh (#230): a decoy prepared folder and library (made-up bytes, never game data,
# SPEC.md M14) staged twice into a temporary "Dropbox" folder, with Finder or Dropbox writing into the folders the
# script removes (the race tools/remove_tree.sh handles): the delivery still succeeds and nothing is left behind.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd -P)
script=$root/tools/stage_dropbox_game.sh
work=$(mktemp -d "${TMPDIR:-/tmp}/stage-dropbox-test.XXXXXX")
trap 'chmod -R u+w "$work" 2>/dev/null || true; rm -rf "$work"' EXIT
die() { echo "FAIL: $*" >&2; exit 1; }
say() { echo "ok: $*"; }
# shellcheck source=tests/race_rm.sh
. "$root/tests/race_rm.sh"

prep=$work/prepared
mkdir -p "$prep/analysis" "$prep/fonts"
printf 'analysis decoy' > "$prep/analysis/main.bin"
printf 'romfs decoy' > "$prep/romfs.pak"
printf 'spirv decoy' > "$prep/spirv.pak"
printf 'font decoy' > "$prep/fonts/shared.ttf"
lib=$work/libacnh_game.so
printf 'library decoy' > "$lib"
dest=$work/dropbox
game=$dest/acnh-universal-game
build_id=0123456789ABCDEF0123456789ABCDEF
abi=0123456789ab
# The one manifest writer: a built acnh-universal ($ACNH_UNIVERSAL_CLI), or cargo builds it.
if [ -z "${ACNH_UNIVERSAL_CLI:-}" ]; then
    (cd "$root" && cargo build -q --locked -p acnh-cli --bin acnh-universal)
    ACNH_UNIVERSAL_CLI=$(cd "$root" && cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')/debug/acnh-universal
fi
export ACNH_UNIVERSAL_CLI
stage() { bash "$script" --build-id "$build_id" --abi "$abi" "$@"; }

race_bin=$work/race-bin
make_race_rm "$race_bin"
export RACE_MARKS=$work/race-marks
mkdir -p "$RACE_MARKS"
for run in 1 2; do # the second run replaces the first, so android.old is removed while being written to
    mkdir -p "$game/android.new"; : > "$game/android.new/leftover" # an interrupted run's staging folder
    PATH="$race_bin:$PATH" stage "$prep" "$lib" "$dest" > "$work/run$run.log" 2>&1 \
        || { cat "$work/run$run.log" >&2; die "a write during removal failed the staging (run $run)"; }
done
[ -e "$RACE_MARKS/android.old" ] || die "the racing rm never ran on android.old"
[ -e "$RACE_MARKS/android.new" ] || die "the racing rm never ran on android.new"
[ "$(ls -A "$game")" = android ] || die "staging left other folders: $(ls -A "$game")"
m=$game/android/manifest.json
[ -f "$m" ] || die "no manifest.json"
python3 - "$m" <<'PY' || die "the manifest does not list exactly the staged files"
import json, sys
names = sorted(f["path"] for f in json.load(open(sys.argv[1]))["files"])
want = ["analysis/main.bin", "fonts/shared.ttf", "libacnh_game.so", "romfs.pak", "spirv.pak"]
sys.exit(0 if names == want else 1)
PY
# The manifest is valid JSON with the real build id and ABI, and one file's size and SHA-256 match the bytes.
python3 - "$m" "$game/android" "$build_id" "$abi" <<'PY' || die "the manifest is not valid or its size or hash is wrong"
import hashlib, json, os, sys
m, folder, build_id, abi = sys.argv[1:5]
doc = json.load(open(m))
assert doc["version"] == 1 and doc["build_id"] == build_id and doc["nx_game_abi"] == abi, doc
by_path = {f["path"]: f for f in doc["files"]}
entry = by_path["romfs.pak"]
data = open(os.path.join(folder, "romfs.pak"), "rb").read()
assert entry["size"] == len(data) == len(b"romfs decoy"), entry
assert entry["sha256"] == hashlib.sha256(data).hexdigest(), entry
for f in doc["files"]:
    p = os.path.join(folder, f["path"])
    assert os.path.getsize(p) == f["size"] and hashlib.sha256(open(p, "rb").read()).hexdigest() == f["sha256"], f
PY
say "the manifest holds the build id and ABI, and a size and a SHA-256 that match the file"
say "a write into android.new or android.old during removal does not fail the staging, nor leave a leftover in it"

# No build id: refused with nothing written.
fresh=$work/fresh
if bash "$script" "$prep" "$lib" "$fresh" > "$work/nobuild.log" 2>&1; then die "staging without --build-id was not refused"; fi
grep -q -- '--build-id' "$work/nobuild.log" || { cat "$work/nobuild.log" >&2; die "no clear message for a missing build id"; }
[ ! -e "$fresh" ] || die "a refused run wrote $fresh"
say "staging without a build id is refused, and writes nothing"

# The writer itself refuses an empty ABI or build id, and writes nothing.
if "$ACNH_UNIVERSAL_CLI" manifest --out "$prep" --build-id "$build_id" --abi "" > "$work/noabi.log" 2>&1; then die "an empty --abi was not refused"; fi
if "$ACNH_UNIVERSAL_CLI" manifest --out "$prep" --build-id "" --abi "$abi" > "$work/noid.log" 2>&1; then die "an empty --build-id was not refused"; fi
[ ! -e "$prep/manifest.json" ] || die "a refused run wrote a manifest"
say "the manifest writer refuses an empty ABI or build id"

# Linux mode: the same manifest, written by the same writer.
install=$work/install
mkdir -p "$install"
printf 'acnh decoy' > "$install/acnh"
printf 'lib decoy' > "$install/libfoo.so.1"
stage --os linux --install "$install" "$prep" "$dest" > "$work/linux.log" 2>&1 || { cat "$work/linux.log" >&2; die "Linux staging failed"; }
python3 - "$game/linux" "$build_id" "$abi" <<'PY' || die "the Linux manifest is not valid or its size or hash is wrong"
import hashlib, json, os, sys
folder, build_id, abi = sys.argv[1:4]
doc = json.load(open(os.path.join(folder, "manifest.json")))
assert doc["build_id"] == build_id and doc["nx_game_abi"] == abi, doc
names = sorted(f["path"] for f in doc["files"])
assert names == ["acnh", "libfoo.so.1", "root/analysis/main.bin", "root/fonts/shared.ttf", "root/romfs.pak", "root/spirv.pak"], names
for f in doc["files"]:
    p = os.path.join(folder, f["path"])
    assert os.path.getsize(p) == f["size"] and hashlib.sha256(open(p, "rb").read()).hexdigest() == f["sha256"], f
PY
say "Linux mode writes a valid manifest, with a size and a SHA-256 for every file"

# A leftover .old that cannot be removed stops the run before the swap, with the delivery in place untouched.
cat > "$race_bin/rm" <<'SHIM'
#!/bin/bash
for a in "$@"; do case "$a" in */android.old) echo "rm: $a: Directory not empty" >&2; exit 1 ;; esac; done
exec /bin/rm "$@"
SHIM
mkdir -p "$game/android.old"
if PATH="$race_bin:$PATH" REMOVE_TREE_WAIT=0 stage "$prep" "$lib" "$dest" > "$work/stuck.log" 2>&1; then
    die "a leftover android.old that cannot be removed did not stop the run"
fi
grep -q 'android.old is left from an earlier run' "$work/stuck.log" || { cat "$work/stuck.log" >&2; die "no clear message"; }
[ -f "$game/android/manifest.json" ] && [ ! -e "$game/android/android" ] || die "the delivery in place was changed"
say "a leftover android.old that cannot be removed stops the run before the swap"
# Run through a symlink in another folder (and a relative link to that), it still finds tools/remove_tree.sh.
mkdir -p "$work/links/deeper"
ln -s "$script" "$work/links/stage"
ln -s ../stage "$work/links/deeper/stage"
for link in "$work/links/stage" "$work/links/deeper/stage"; do
    out=$(bash "$link" 2>&1 || true)
    case "$out" in *remove_tree.sh*) die "run through $link, it cannot find remove_tree.sh: $out" ;; esac
    case "$out" in *usage:*) ;; *) die "run through $link with no arguments, it did not print the usage: $out" ;; esac
done
say "run through a symlink, it still finds tools/remove_tree.sh"
echo "stage_dropbox_game tests passed"
