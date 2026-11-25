#!/usr/bin/env bash
# Tests for tools/package_ios.sh (#172, 8.03d with the game inside the .ipa). They use a fake ACNH.app (one arm64
# iphoneos executable and ios/Info.plist.in), a prepared folder of empty decoy files, and refusals. Every folder they
# make is under $RUNNER_TEMP (made here when it is not set), so the script's CI rule applies to the whole test.
# Run: bash tests/package_ios_test.sh
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd -P)
script=$root/tools/package_ios.sh
template=$root/ios/Info.plist.in

own_runner_temp=
work=
outside=
cleanup() {
    local d
    for d in "$work" "$outside"; do
        if [ -n "$d" ] && [ -e "$d" ]; then
            chmod -R u+w "$d" 2>/dev/null || true
            rm -rf "$d"
        fi
    done
    if [ -n "$own_runner_temp" ]; then rm -rf "$own_runner_temp"; fi
    return 0
}
trap cleanup EXIT
trap 'exit 130' INT TERM

say() { echo "ok: $*"; }
die() { echo "FAIL: $*" >&2; exit 1; }

if [ -z "${RUNNER_TEMP:-}" ]; then
    own_runner_temp=$(mktemp -d "${TMPDIR:-/tmp}/acnh-package-test.XXXXXXXX")
    RUNNER_TEMP=$own_runner_temp
    export RUNNER_TEMP
fi
work=$(mktemp -d "$RUNNER_TEMP/package-ios-test.XXXXXXXX")

# A fake build: one arm64 iphoneos executable and the real Info.plist template.
build=$work/build
app=$build/ACNH.app
mkdir -p "$app"
printf 'int main(void) { return 0; }\n' > "$work/fake.c"
xcrun --sdk iphoneos clang -arch arm64 -miphoneos-version-min=15.0 -o "$app/ACNH" "$work/fake.c"
# Rendered as CMake's MACOSX_BUNDLE_INFO_PLIST does (configure_file replaces ${VAR}): the names the template uses.
render_plist() { # <short version> <bundle version> <out file>
    cmake -DMACOSX_BUNDLE_SHORT_VERSION_STRING="$1" -DMACOSX_BUNDLE_BUNDLE_VERSION="$2" \
        -DIN="$template" -DOUT="$3" -P "$work/render.cmake"
}
printf 'configure_file("${IN}" "${OUT}")\n' > "$work/render.cmake"
render_plist 0.11.0 1100832 "$app/Info.plist"
plutil -lint "$app/Info.plist" > /dev/null || die "the rendered Info.plist is not valid"

# 0. The stamps build_ios.sh gives: build_version.sh's values, rendered into the template.
[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Info.plist")" = 0.11.0 ] || die "short version not stamped"
[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$app/Info.plist")" = 1100832 ] || die "bundle version not stamped"
grep -q '\${' "$app/Info.plist" && die "the rendered Info.plist still holds a template variable"
stamp=$(cd "$root" && . tools/build_version.sh && s=${BASE_VER%%-*} && echo "${s%%+*} $VCODE")
case "$stamp" in [0-9]*.[0-9]*" "[0-9]*) ;; *) die "build_version.sh gave an unusable stamp: $stamp" ;; esac
case "${stamp#* }" in ''|*[!0-9]*) die "VCODE is not an integer: ${stamp#* }" ;; esac
[ "${stamp#* }" -ge 1100 ] || die "VCODE is below the floor of 1100"
# VCODE rises with the commit count (stamps from a scratch repo, one commit apart).
rise=$work/rise
git init -q "$rise"
mkdir "$rise/tools"
cp "$root/tools/build_version.sh" "$rise/tools/"
git -C "$rise" -c user.name=t -c user.email=t@t.invalid commit -q --allow-empty -m one
first=$(cd "$rise" && . "$rise/tools/build_version.sh" && echo "$VCODE")
git -C "$rise" -c user.name=t -c user.email=t@t.invalid commit -q --allow-empty -m two
second=$(cd "$rise" && . "$rise/tools/build_version.sh" && echo "$VCODE")
[ "$second" -gt "$first" ] || die "VCODE did not rise across commits ($first, $second)"
printf 'x\n' > "$rise/f"
git -C "$rise" add f
dirty=$(cd "$rise" && . "$rise/tools/build_version.sh" && echo "$VNAME")
case "$dirty" in *-dirty) ;; *) die "a dirty tree is not marked: $dirty" ;; esac
# A packaged source has no .git of its own (release.yml's `git archive` copy under the launcher's data folder), and may
# sit inside another repository whose tags mean nothing: git is ignored there, BUILD_VERSION is read, else 0.0.0 / 10000.
outer=$work/outer
git init -q "$outer"
git -C "$outer" -c user.name=t -c user.email=t@t.invalid commit -q --allow-empty -m one
git -C "$outer" tag laptop-2025
mkdir -p "$outer/data/source/abc/tools"
cp "$root/tools/build_version.sh" "$outer/data/source/abc/tools/"
pkg=$outer/data/source/abc
fallback=$(cd "$pkg" && . tools/build_version.sh && echo "$BASE_VER $VCODE")
[ "$fallback" = "0.0.0 10000" ] || die "a packaged source without BUILD_VERSION gave: $fallback"
printf 'BASE_VER=0.12.0\nCOMMITS=345\n' > "$pkg/BUILD_VERSION"
printf 'deadbeefcafe\n' > "$pkg/SOURCE"
packaged=$(cd "$pkg" && . tools/build_version.sh && echo "$BASE_VER $VCODE $VNAME")
[ "$packaged" = "0.12.0 1200345 0.12.0+deadbee" ] || die "a packaged source gave: $packaged"
say "Info.plist stamps: rendered values, VCODE integer that rises, dirty marked, packaged source"

# A prepared game of empty files: the five names that ship, and names that must never ship.
make_decoy() {
    local g=$1
    mkdir -p "$g/analysis" "$g/fonts" "$g/aoc/1" "$g/gen/main" "$g/build" "$g/exefs" "$g/extract/romfs" \
        "$g/user/device_save" "$g/spirv" "$g/shaders_extracted" "$g/mods" "$g/logs"
    touch "$g/spirv/a.spv" "$g/shaders_extracted/a" "$g/mods/a" "$g/logs/nx.log" "$g/nx.log"
    touch "$g/analysis/main.bin" "$g/romfs.pak" "$g/spirv.pak" "$g/fonts/shared.ttf" "$g/aoc/1/data" \
        "$g/gen/main/table.cpp" "$g/build/acnh" "$g/exefs/main" "$g/extract/romfs/a" \
        "$g/user/device_save/main.dat" "$g/config.ini"
}

# A refusal must exit nonzero, say the reason on stderr, and leave the out folder empty or absent.
refuse() { # <label> <reason substring> <command...>
    local label=$1 reason=$2 err out_dir=${3:-}
    shift 3
    if err=$("$@" 2>&1 >/dev/null); then die "$label was not refused"; fi
    case "$err" in *"$reason"*) ;; *) die "$label was refused for another reason: $err" ;; esac
    if [ -n "$out_dir" ] && [ -e "$out_dir" ] && [ -n "$(ls -A "$out_dir")" ]; then
        die "$label wrote to its out folder"
    fi
    say "refused: $label ($reason)"
}

# A template that was not rendered, and a version that is not numeric, are refused.
raw=$work/build-raw
mkdir -p "$raw/ACNH.app"
cp "$app/ACNH" "$raw/ACNH.app/ACNH"
cp "$template" "$raw/ACNH.app/Info.plist"
refuse "an unrendered Info.plist" "CFBundleShortVersionString" "$work/out-raw" \
    env CI=1 bash "$script" raw "$raw" --no-game "$work/out-raw"
badver=$work/build-badver
mkdir -p "$badver/ACNH.app"
cp "$app/ACNH" "$badver/ACNH.app/ACNH"
render_plist 0.11.0 1100832+abc "$badver/ACNH.app/Info.plist"
refuse "a non-numeric CFBundleVersion" "CFBundleVersion" "$work/out-badver" \
    env CI=1 bash "$script" badver "$badver" --no-game "$work/out-badver"

# 1. --no-game packs the app and makes no game/ folder.
nogame_out=$work/out-nogame
env CI=1 bash "$script" nogame "$build" --no-game "$nogame_out" > "$work/nogame.log"
ipa1=$nogame_out/ACNH-Universal-nogame.ipa
[ -f "$ipa1" ] || die "--no-game wrote no .ipa"
[ "$(ls -A "$nogame_out")" = "ACNH-Universal-nogame.ipa" ] || die "--no-game left files other than the .ipa"
zipinfo -1 "$ipa1" > "$work/nogame.list"
grep -qx 'Payload/ACNH.app/ACNH' "$work/nogame.list" || die "--no-game archive has no executable"
if grep -q '^Payload/ACNH\.app/game' "$work/nogame.list"; then die "--no-game archive has a game/ folder"; fi
unzip -tq "$ipa1" > /dev/null || die "--no-game archive fails unzip -t"
bash "$root/tools/check_release_files.sh" < "$work/nogame.list" > /dev/null || die "--no-game list fails the release check"
grep -q '^SHA-256: [0-9a-f]\{64\}$' "$work/nogame.log" || die "--no-game did not print the SHA-256"
say "--no-game packs the app and lists nothing under game/"

# 2. --game with decoys: game/ holds exactly the five names, read-only; the rest passes the release check.
decoy=$work/decoy
make_decoy "$decoy"
game_out=$work/out-game
env CI=1 bash "$script" decoy "$build" --game "$decoy" "$game_out" > "$work/game.log"
ipa2=$game_out/ACNH-Universal-decoy.ipa
[ "$(ls -A "$game_out")" = "ACNH-Universal-decoy.ipa" ] || die "--game left files other than the .ipa"
zipinfo -1 "$ipa2" > "$work/game.list"
grep '^Payload/ACNH\.app/game/' "$work/game.list" | grep -v '/$' | sort > "$work/game.files"
cat > "$work/expected.files" <<'EOF'
Payload/ACNH.app/game/analysis/main.bin
Payload/ACNH.app/game/aoc/1/data
Payload/ACNH.app/game/fonts/shared.ttf
Payload/ACNH.app/game/romfs.pak
Payload/ACNH.app/game/spirv.pak
EOF
diff "$work/expected.files" "$work/game.files" || die "game/ does not hold exactly the five files"
zipinfo "$ipa2" | awk '$NF ~ /^Payload\/ACNH\.app\/game\/.*[^\/]$/ { print $1 }' | sort -u > "$work/game.modes"
[ "$(cat "$work/game.modes")" = "-r--r--r--" ] || die "game/ files are not read-only in the archive: $(cat "$work/game.modes")"
awk '!/^Payload\/ACNH\.app\/game/' "$work/game.list" > "$work/outside.list"
bash "$root/tools/check_release_files.sh" < "$work/outside.list" > /dev/null || die "the archive outside game/ fails the release check"
unzip -tq "$ipa2" > /dev/null || die "--game archive fails unzip -t"
say "--game copies exactly analysis/, romfs.pak, spirv.pak, fonts/ and aoc/, read-only"

# A non-empty file outside the five copied names (the prepared folder holds build and work folders) is not checked.
extra=$work/decoy-extra
make_decoy "$extra"
mkdir -p "$extra/build-ios"
printf 'x' > "$extra/build-ios/x"
printf 'x' > "$extra/gen/main/table.cpp"
printf 'x' > "$extra/.fan-mod-settings"
env CI=1 bash "$script" extra "$build" --game "$extra" "$work/out-extra" > /dev/null
[ -f "$work/out-extra/ACNH-Universal-extra.ipa" ] || die "a non-empty file outside the five names stopped the packaging"
say "under CI only the five copied names are checked for empty files"

# 3. Refusals: each writes nothing to the out folder and says why.
bad=$work/decoy-bad
make_decoy "$bad"
printf 'x' > "$bad/fonts/shared.ttf"
refuse "a non-empty game file under CI" "non-empty" "$work/out-bad" \
    env CI=1 bash "$script" bad "$build" --game "$bad" "$work/out-bad"

git init -q "$work/repo"
refuse "out folder inside a git work tree" "Git work tree" "$work/repo/out" \
    env -u CI -u GITHUB_ACTIONS bash "$script" git "$build" --no-game "$work/repo/out"

outside=$(mktemp -d "${TMPDIR:-/tmp}/acnh-package-outside.XXXXXXXX")
refuse "out folder outside RUNNER_TEMP under CI" "RUNNER_TEMP" "$outside" \
    env CI=1 bash "$script" outside "$build" --no-game "$outside"

refuse "missing out folder" "out folder is required" "" \
    env CI=1 bash "$script" missing "$build" --no-game

refuse "an empty out folder argument" "out folder is required" "" \
    bash "$script" empty "$build" --no-game ""

refuse "out folder inside this checkout" "outside the checkout" "$root/private/package-ios-test-out" \
    env -u CI -u GITHUB_ACTIONS bash "$script" inclone "$build" --no-game "$root/private/package-ios-test-out"

nobin=$work/decoy-nobin
make_decoy "$nobin"
rm "$nobin/analysis/main.bin"
refuse "a game folder without analysis/main.bin" "analysis/main.bin" "$work/out-nobin" \
    env CI=1 bash "$script" nobin "$build" --game "$nobin" "$work/out-nobin"

# 3b. --small (#231): the .ipa has no game/, and the game's files with a manifest are staged for Dropbox. Decoy files with
# made-up bytes (never game data) so the sizes and hashes mean something; not under CI, which allows only empty files.
small=$work/decoy-small
make_decoy "$small"
printf 'analysis decoy bytes' > "$small/analysis/main.bin"
head -c 70000 /dev/zero | tr '\0' 'r' > "$small/romfs.pak"
printf 's' > "$small/spirv.pak"
printf 'font decoy' > "$small/fonts/shared.ttf"
: > "$small/aoc/1/data"
printf 'junk' > "$small/fonts/.DS_Store"
small_out=$work/out-small
build_id=0123456789abcdef0123456789ABCDEF
env -u CI -u GITHUB_ACTIONS bash "$script" smallv "$build" --small "$small" --build-id "$build_id" "$small_out" > "$work/small.log"
ipa3=$small_out/ACNH-Universal-smallv.ipa
[ -f "$ipa3" ] || die "--small wrote no .ipa"
zipinfo -1 "$ipa3" > "$work/small.list"
if grep -q '^Payload/ACNH\.app/game' "$work/small.list"; then die "--small archive has a game/ folder"; fi
unzip -tq "$ipa3" > /dev/null || die "--small archive fails unzip -t"
bash "$root/tools/check_release_files.sh" < "$work/small.list" > /dev/null || die "--small list fails the release check"
files=$small_out/acnh-universal-game/ios
[ -f "$files/manifest.json" ] || die "--small wrote no manifest.json"
small_ls=$(ls -A "$small_out" | LC_ALL=C sort | tr '\n' ' ')
[ "$small_ls" = "ACNH-Universal-smallv.ipa acnh-universal-game " ] || die "--small left other files in the out folder: $small_ls"
python3 - "$files" "$small" "$build_id" <<'PY' || die "the manifest does not match the files"
import hashlib, json, os, sys
files, prepared, build_id = sys.argv[1:4]
m = json.load(open(os.path.join(files, "manifest.json")))
assert m["version"] == 1
assert m["build_id"] == build_id.upper(), m["build_id"]
assert len(m["nx_game_abi"]) == 12 and all(c in "0123456789abcdef" for c in m["nx_game_abi"]), m["nx_game_abi"]
listed = {e["path"]: e for e in m["files"]}
expected = {"analysis/main.bin", "aoc/1/data", "fonts/shared.ttf", "romfs.pak", "spirv.pak"}
assert set(listed) == expected, sorted(listed)
for path, e in listed.items():
    data = open(os.path.join(prepared, path), "rb").read()
    assert e["size"] == len(data), path
    assert e["sha256"] == hashlib.sha256(data).hexdigest(), path
    staged = open(os.path.join(files, path), "rb").read()
    assert staged == data, path
on_disk = set()
for base, _, names in os.walk(files):
    for n in names:
        on_disk.add(os.path.relpath(os.path.join(base, n), files))
assert on_disk == expected | {"manifest.json"}, sorted(on_disk)
# Never anything the prepared folder keeps besides the game (build folders, saves, logs, keys).
for bad in ("gen", "build", "exefs", "user", "config.ini", "nx.log"):
    assert not any(p.split("/")[0] == bad for p in on_disk), bad
PY
grep -q '^Game files: ' "$work/small.log" || die "--small did not say where the game files are"
say "--small: no game/ in the .ipa; the manifest lists every staged file with matching sizes and hashes"

# A second run replaces the staged files; a new ABI can be given.
printf 'newer' > "$small/spirv.pak"
env -u CI -u GITHUB_ACTIONS bash "$script" smallv "$build" --small "$small" --build-id "$build_id" --abi 0123456789ab "$small_out" > /dev/null
python3 -c 'import json,sys; m=json.load(open(sys.argv[1])); assert m["nx_game_abi"]=="0123456789ab"; assert [f["size"] for f in m["files"] if f["path"]=="spirv.pak"]==[5]' "$files/manifest.json" \
    || die "a second --small run did not replace the staged files"
[ ! -e "$small_out/acnh-universal-game/ios.old" ] || die "--small left ios.old behind"
say "--small again replaces the staged files and takes --abi"

# The prepared folder's version.ini (as Prepare writes it) names its build: --build-id may be left out, and a different
# one is refused. A game.ini does the same when there is no version.ini; version.ini wins when both are there.
printf '# Written by acnh_core::prepare: the version this folder holds and its build profile.\nbuild_id = FEDCBA9876543210FEDCBA9876543210\nversion = 3.0.2\ntitle_version = 0\nhooks = -\n' > "$small/version.ini"
refuse "--build-id that is not the prepared folder's build" "is not the build of the prepared folder" "$work/out-small-bad" \
    env -u CI -u GITHUB_ACTIONS bash "$script" smallv "$build" --small "$small" --build-id "$build_id" "$work/out-small-bad"
env -u CI -u GITHUB_ACTIONS bash "$script" smallv "$build" --small "$small" "$work/out-small-ini" > /dev/null
python3 -c 'import json,sys; assert json.load(open(sys.argv[1]))["build_id"]=="FEDCBA9876543210FEDCBA9876543210"' \
    "$work/out-small-ini/acnh-universal-game/ios/manifest.json" || die "--small did not take the build id from version.ini"
printf 'build_id = 11111111111111111111111111111111\n' > "$small/game.ini"
env -u CI -u GITHUB_ACTIONS bash "$script" smallv "$build" --small "$small" "$work/out-small-both" > /dev/null
python3 -c 'import json,sys; assert json.load(open(sys.argv[1]))["build_id"]=="FEDCBA9876543210FEDCBA9876543210"' \
    "$work/out-small-both/acnh-universal-game/ios/manifest.json" || die "game.ini won over version.ini"
rm "$small/version.ini"
env -u CI -u GITHUB_ACTIONS bash "$script" smallv "$build" --small "$small" "$work/out-small-gameini" > /dev/null
python3 -c 'import json,sys; assert json.load(open(sys.argv[1]))["build_id"]=="11111111111111111111111111111111"' \
    "$work/out-small-gameini/acnh-universal-game/ios/manifest.json" || die "--small did not take the build id from game.ini"
rm "$small/game.ini"
say "--small takes the build id from the prepared folder's version.ini (else game.ini) and refuses a different --build-id"

# Under CI, empty decoys: the manifest lists them with size 0 and the hash of nothing.
env CI=1 bash "$script" smallci "$build" --small "$decoy" --build-id "$build_id" "$work/out-small-ci" > /dev/null
python3 - "$work/out-small-ci/acnh-universal-game/ios/manifest.json" <<'PY' || die "the CI manifest is wrong"
import json, sys
m = json.load(open(sys.argv[1]))
assert {e["size"] for e in m["files"]} == {0}
assert {e["sha256"] for e in m["files"]} == {"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}
PY
say "--small under CI lists empty decoys with the hash of nothing"

refuse "--small without a build id" "build id" "$work/out-small-nobid" \
    env CI=1 bash "$script" nobid "$build" --small "$decoy" "$work/out-small-nobid"
refuse "--small with a bad build id" "build id" "$work/out-small-badbid" \
    env CI=1 bash "$script" badbid "$build" --small "$decoy" --build-id xyz "$work/out-small-badbid"
refuse "--small with a non-empty game file under CI" "non-empty" "$work/out-small-bad" \
    env CI=1 bash "$script" smallbad "$build" --small "$bad" --build-id "$build_id" "$work/out-small-bad"

# 4. Past 4 GB: zip -qry makes a ZIP64 archive that unzip -t passes.
big=$work/big
mkdir "$big"
mkfile -n 4200m "$big/sparse.bin"
[ "$(stat -f %z "$big/sparse.bin")" -gt 4294967296 ] || die "the sparse test file is not past 4 GB"
(cd "$work" && zip -qry "$work/big.zip" big)
unzip -tq "$work/big.zip" > /dev/null || die "the 4.2 GB archive fails unzip -t"
rm -f "$big/sparse.bin" "$work/big.zip"
say "zip -qry past 4 GB makes an archive that unzip -t passes"

[ "$(/usr/libexec/PlistBuddy -c 'Print :LSApplicationCategoryType' "$template")" = public.app-category.simulation-games ] \
    || die "ios/Info.plist.in lacks LSApplicationCategoryType simulation-games (Game Mode)"
[ -n "$(/usr/libexec/PlistBuddy -c 'Print :NSLocalNetworkUsageDescription' "$template")" ] \
    || die "ios/Info.plist.in lacks NSLocalNetworkUsageDescription"
say "Info.plist template has the Game Mode category and the local-network text"

echo "package_ios tests passed"
