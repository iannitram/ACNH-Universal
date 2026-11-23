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
cp "$template" "$app/Info.plist"

# A prepared game of empty files: the five names that ship, and names that must never ship.
make_decoy() {
    local g=$1
    mkdir -p "$g/analysis" "$g/fonts" "$g/aoc/1" "$g/gen/main" "$g/build" "$g/exefs" "$g/extract/romfs" \
        "$g/user/device_save"
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

nobin=$work/decoy-nobin
make_decoy "$nobin"
rm "$nobin/analysis/main.bin"
refuse "a game folder without analysis/main.bin" "analysis/main.bin" "$work/out-nobin" \
    env CI=1 bash "$script" nobin "$build" --game "$nobin" "$work/out-nobin"

# 4. Past 4 GB: zip -qry makes a ZIP64 archive that unzip -t passes.
big=$work/big
mkdir "$big"
mkfile -n 4200m "$big/sparse.bin"
[ "$(stat -f %z "$big/sparse.bin")" -gt 4294967296 ] || die "the sparse test file is not past 4 GB"
(cd "$work" && zip -qry "$work/big.zip" big)
unzip -tq "$work/big.zip" > /dev/null || die "the 4.2 GB archive fails unzip -t"
rm -f "$big/sparse.bin" "$work/big.zip"
say "zip -qry past 4 GB makes an archive that unzip -t passes"

echo "package_ios tests passed"
