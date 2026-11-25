#!/usr/bin/env bash
# Tests for tools/package_mac.sh (#171): a fake ACNH-Universal.app, empty decoy game files and
# the script's refusals. macOS only. No real game file is read or written (SPEC.md M14).
# Folders are made under $RUNNER_TEMP (a temporary folder of its own when that is unset).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd -P)
script=$root/tools/package_mac.sh
[ "$(uname -s)" = Darwin ] || { echo "skipped: macOS only"; exit 0; }

own_runner_temp=
if [ -z "${RUNNER_TEMP:-}" ]; then
  RUNNER_TEMP=$(mktemp -d "${TMPDIR:-/tmp}/package-mac-runner.XXXXXX")
  own_runner_temp=$RUNNER_TEMP
fi
export RUNNER_TEMP
RUNNER_TEMP=$(cd "$RUNNER_TEMP" && pwd -P)
work=$(mktemp -d "$RUNNER_TEMP/package-mac-test.XXXXXX")
elsewhere=$(mktemp -d /tmp/package-mac-elsewhere.XXXXXX)
cleanup() {
  chmod -R u+w "$work" "$elsewhere" 2>/dev/null || true
  rm -rf "$work" "$elsewhere" "$root/package-mac-test-out"
  if [ -n "$own_runner_temp" ]; then rm -rf "$own_runner_temp"; fi
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "ok: $*"; }
# Run the script under CI, as the macOS job does.
ci() { CI=1 "$script" "$@"; }
# `refused <reason text> <script args>`: the script exits non-zero and its message has the reason.
refused() {
  local reason=$1; shift
  local log=$work/refusal.log
  if ci "$@" >"$log" 2>&1; then fail "was not refused: $reason"; fi
  grep -qi -- "$reason" "$log" || { cat "$log" >&2; fail "refusal does not say: $reason"; }
}
make_mods() { # <folder>: decoy mods in the shape tools/bundle_mods.sh makes (empty files; never the real mods)
  local m=$1
  mkdir -p "$m/speedfix60/romfs/Pack" "$m/speedfix60/romfs/Bcsv" "$m/ptbr/romfs/Message"
  : >"$m/speedfix60/romfs/Pack/Decoy.pack"
  : >"$m/speedfix60/romfs/Bcsv/Decoy.bcsv"
  : >"$m/ptbr/romfs/Message/Decoy.sarc.zs"
  { echo decoybundle; (cd "$m" && find . -type f ! -name bundle.txt | sed 's|^\./||' | LC_ALL=C sort); } >"$m/bundle.txt"
}
empty_or_missing() { [ ! -e "$1" ] || [ -z "$(ls -A "$1")" ]; }

# ---- the fake app: three arm64 executables, a dylib, an Info.plist, an ad-hoc signature ----
src=$work/src
mkdir -p "$src"
printf 'int main(void){return 0;}\n' >"$src/main.c"
printf 'int fake(void){return 1;}\n' >"$src/fake.c"
app=$work/app/ACNH-Universal.app
mkdir -p "$app/Contents/MacOS" "$app/Contents/Frameworks" "$app/Contents/Resources"
for exe in acnh-app acnh-universal acnh; do
  cc -arch arm64 -o "$app/Contents/MacOS/$exe" "$src/main.c"
done
cc -arch arm64 -dynamiclib -o "$app/Contents/Frameworks/libfake.dylib" "$src/fake.c"
cat >"$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>acnh-app</string>
<key>CFBundleIdentifier</key><string>test.acnh.universal</string>
<key>CFBundleName</key><string>ACNH-Universal</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>1</string>
</dict></plist>
PLIST
plutil -lint "$app/Contents/Info.plist" >/dev/null
for f in "$app/Contents/Frameworks/libfake.dylib" "$app"/Contents/MacOS/*; do codesign --force -s - "$f" >/dev/null 2>&1; done
codesign --force -s - "$app" >/dev/null 2>&1

# ---- the stub: --no-game ----
out=$work/out-stub
ci t1 "$app" --no-game "$out" >"$work/stub.log"
grep -q '^Zip: ' "$work/stub.log" || fail "no path printed"
grep -q '^SHA-256: ' "$work/stub.log" || fail "no hash printed"
[ "$(wc -l <"$work/stub.log" | tr -d ' ')" = 3 ] || fail "output is more than path, size and SHA-256"
zip=$out/ACNH-Universal-t1.zip
[ -f "$zip" ] || fail "no zip"
[ ! -e "$out/.ACNH-Universal-t1.partial" ] && [ ! -e "$zip.partial" ] || fail "partial files left behind"
zipinfo -1 "$zip" >"$work/stub.list"
ACNH_ALLOW_BUNDLED_MODS=1 bash "$root/tools/check_release_files.sh" <"$work/stub.list" >/dev/null || fail "stub list fails the release check"
! grep -q 'Contents/Resources/game/' "$work/stub.list" || fail "stub zip holds a game folder"
grep -q '^ACNH-Universal.app/Contents/MacOS/acnh$' "$work/stub.list" || fail "stub zip is missing the player"
pass "stub zip"

# ---- empty decoys: the game's names, and others that must never be copied ----
prep=$work/prepared
mkdir -p "$prep"
for f in analysis/main.bin romfs.pak spirv.pak fonts/shared.ttf aoc/1/data \
         gen/main/table.cpp build/acnh exefs/main extract/romfs/a user/device_save/main.dat config.ini; do
  mkdir -p "$prep/$(dirname "$f")"
  : >"$prep/$f"
done
out=$work/out-decoy
ci t2 "$app" "$prep" "$out" >"$work/decoy.log"
zip=$out/ACNH-Universal-t2.zip
[ -f "$zip" ] || fail "no decoy zip"
[ ! -e "$out/.ACNH-Universal-t2.partial" ] || fail "staging folder left behind"
zipinfo -1 "$zip" >"$work/decoy.list"
grep '^ACNH-Universal.app/Contents/Resources/game/.' "$work/decoy.list" | grep -v '/$' | sort >"$work/game.list"
printf '%s\n' \
  ACNH-Universal.app/Contents/Resources/game/analysis/main.bin \
  ACNH-Universal.app/Contents/Resources/game/aoc/1/data \
  ACNH-Universal.app/Contents/Resources/game/fonts/shared.ttf \
  ACNH-Universal.app/Contents/Resources/game/romfs.pak \
  ACNH-Universal.app/Contents/Resources/game/spirv.pak | sort >"$work/game.want"
diff "$work/game.want" "$work/game.list" || fail "game/ does not hold exactly the five names"
for other in gen/ build/ exefs/ extract/ user/ config.ini; do
  ! grep -q "$other" "$work/decoy.list" || fail "a decoy outside the five names was copied: $other"
done
grep -v 'Contents/Resources/game/' "$work/decoy.list" | ACNH_ALLOW_BUNDLED_MODS=1 bash "$root/tools/check_release_files.sh" >/dev/null ||
  fail "decoy zip's list outside game/ fails the release check"
unpacked=$work/unpacked
mkdir "$unpacked"
ditto -x -k "$zip" "$unpacked"
[ -z "$(find "$unpacked/ACNH-Universal.app/Contents/Resources/game" -perm +222)" ] || fail "game/ holds a writable entry"
codesign --verify --deep --strict "$unpacked/ACNH-Universal.app" || fail "signature does not verify"
pass "empty decoys"

# ---- refusals: each names its reason and writes nothing to the out folder ----
printf 'x' >"$prep/analysis/main.bin"
out=$work/out-nonempty
refused "not empty" t3 "$app" "$prep" "$out"
empty_or_missing "$out" || fail "a non-empty decoy wrote to the out folder"
: >"$prep/analysis/main.bin"

git init -q "$work/repo"
out=$work/repo/new
refused "git" t3 "$app" --no-game "$out"
[ ! -e "$out" ] || fail "wrote inside a git work tree"
out=$root/package-mac-test-out
refused "git" t3 "$app" --no-game "$out"
[ ! -e "$out" ] || fail "wrote inside this clone"
out=$elsewhere/out
refused "RUNNER_TEMP" t3 "$app" --no-game "$out"
[ ! -e "$out" ] || fail "wrote outside RUNNER_TEMP under CI"
out=$app/Contents/out
refused "inside the app" t3 "$app" --no-game "$out"
[ ! -e "$out" ] || fail "wrote inside the app"
out=$prep/out
refused "inside the game folder" t3 "$app" "$prep" "$out"
[ ! -e "$out" ] || fail "wrote inside the game folder"
out=$work/out-no-main
rm "$prep/analysis/main.bin"
refused "analysis/main.bin" t3 "$app" "$prep" "$out"
: >"$prep/analysis/main.bin"
empty_or_missing "$out" || fail "an incomplete game folder wrote to the out folder"

x86=$work/x86/ACNH-Universal.app
mkdir -p "$work/x86"
cp -R "$app" "$x86"
cc -arch x86_64 -o "$x86/Contents/MacOS/acnh" "$src/main.c"
out=$work/out-x86
refused "arm64" t3 "$x86" --no-game "$out"
empty_or_missing "$out" || fail "an x86-64 executable wrote to the out folder"
pass "refusals"

# ---- the bundled mods: --mods puts them in Contents/Resources/mods (the SpeedFix's 60 fps files), nothing else passes ----
mods=$work/bundled/mods
make_mods "$mods"
res=ACNH-Universal.app/Contents/Resources
out=$work/out-mods
ci t4 "$app" "$prep" "$out" --mods "$mods" >"$work/mods.log"
zip=$out/ACNH-Universal-t4.zip
zipinfo -1 "$zip" >"$work/mods.list"
grep "^$res/mods/." "$work/mods.list" | grep -v '/$' | sort >"$work/mods.got"
printf '%s\n' "$res/mods/bundle.txt" "$res/mods/ptbr/romfs/Message/Decoy.sarc.zs" \
  "$res/mods/speedfix60/romfs/Bcsv/Decoy.bcsv" "$res/mods/speedfix60/romfs/Pack/Decoy.pack" | sort >"$work/mods.want"
diff "$work/mods.want" "$work/mods.got" || fail "the zip's mods/ is not exactly the bundle"
grep -v "^$res/game/" "$work/mods.list" | ACNH_ALLOW_BUNDLED_MODS=1 bash "$root/tools/check_release_files.sh" >/dev/null ||
  fail "the zip with mods fails the release check outside game/"
! grep -q "^$res/game/.*mods" "$work/mods.list" || fail "mods went into game/"
mkdir "$work/unpacked-mods"
ditto -x -k "$zip" "$work/unpacked-mods"
codesign --verify --deep --strict "$work/unpacked-mods/ACNH-Universal.app" || fail "signature with mods does not verify"
# what the player reads (acnh_files_mods_fetch): bundle.txt's first line is the version, then the files
bt=$work/unpacked-mods/$res/mods/bundle.txt
[ "$(head -n1 "$bt")" = decoybundle ] || fail "bundle.txt lost its version line"
grep -qx 'speedfix60/romfs/Pack/Decoy.pack' "$bt" || fail "bundle.txt does not list the SpeedFix file"
# the prepared folder's own mods/ is never taken, and a stub carries the mods too
mkdir -p "$prep/mods/speedfix60/romfs"; : >"$prep/mods/speedfix60/romfs/FromPrepared.bcsv"
ci t5 "$app" "$prep" "$work/out-mods2" --mods "$mods" >/dev/null
zipinfo -1 "$work/out-mods2/ACNH-Universal-t5.zip" >"$work/mods2.list"
! grep -q FromPrepared "$work/mods2.list" || fail "the prepared folder's mods/ was copied"
ci t5 "$app" --no-game "$work/out-mods3" --mods "$mods" >/dev/null
zipinfo -1 "$work/out-mods3/ACNH-Universal-t5.zip" >"$work/mods3.list"
grep -q "$res/mods/speedfix60/romfs/Pack/Decoy.pack" "$work/mods3.list" || fail "a stub with --mods lost the mods"
# without --mods the zip has no mods/ folder (and romfs/ stays refused outside game/)
! grep -q "$res/mods" "$work/decoy.list" || fail "a zip made without --mods holds mods/"
# refusals leave the out folder empty
badm=$work/bundled-bad/mods
make_mods "$badm"; mkdir -p "$badm/other/romfs"; : >"$badm/other/romfs/x.bcsv"
refused "not allowed" t6 "$app" --no-game "$work/out-badmods" --mods "$badm"
rm -rf "$badm/other"; mkdir -p "$badm/ptbr/romfs/gen"; : >"$badm/ptbr/romfs/gen/x.zs"
refused "check_release_files" t6 "$app" --no-game "$work/out-badmods" --mods "$badm"
rm -rf "$badm/ptbr/romfs/gen"; ln -s /etc/hosts "$badm/ptbr/romfs/link"
refused "link" t6 "$app" --no-game "$work/out-badmods" --mods "$badm"
rm -f "$badm/ptbr/romfs/link"; rm -rf "$badm/speedfix60"
refused "speedfix60" t6 "$app" --no-game "$work/out-badmods" --mods "$badm"
refused "not a folder" t6 "$app" --no-game "$work/out-badmods" --mods "$work/bundled-missing"
empty_or_missing "$work/out-badmods" || fail "a refused mods folder wrote to the out folder"
# check_release_files.sh lets exactly the bundled mods' romfs/ through, in either package, and nothing like it
ok_list=$work/release-ok.list
printf '%s\n' "$res/mods/speedfix60/romfs/Pack/Decoy.pack" "$res/mods/ptbr/romfs/Message/Decoy.sarc.zs" \
  Payload/ACNH.app/mods/speedfix60/romfs/Bcsv/Decoy.bcsv >"$ok_list"
ACNH_ALLOW_BUNDLED_MODS=1 bash "$root/tools/check_release_files.sh" <"$ok_list" >/dev/null || fail "the release check refuses the bundled mods"
for bad in "$res/game/romfs/Decoy.bcsv" "$res/mods/other/romfs/Decoy.bcsv" "$res/mods/ptbr/romfs/Decoy.bin" \
           "$res/mods/ptbr/romfs/gen/Decoy.zs" "$res/mods/ptbr/romfs/../x/romfs/Decoy.zs" "$res/other/romfs/Decoy.zs" \
           "$res/mods/ptbr/Decoy.zs" "$res/Decoy.pack" Payload/ACNH.app/game/mods/ptbr/romfs/Decoy.bcsv \
           Payload/ACNH.app/x/mods/ptbr/romfs/Decoy.bcsv; do
  if printf '%s\n' "$bad" | ACNH_ALLOW_BUNDLED_MODS=1 bash "$root/tools/check_release_files.sh" >/dev/null 2>&1; then fail "the release check lets through: $bad"; fi
done
# without the opt-in (the public release check) the very same list is refused
if bash "$root/tools/check_release_files.sh" <"$ok_list" >/dev/null 2>&1; then fail "the release check lets the bundled mods through without ACNH_ALLOW_BUNDLED_MODS"; fi
pass "bundled mods"

# ---- past 4 GB: a ZIP64 archive that lists the full size and unpacks with ditto ----
# `ditto -c -k` stores a file past 4 GiB with its size modulo 4 GiB in the central directory
# (zipinfo and Python then list 109051904 bytes for this file) although `ditto -x -k` unpacks it
# in full, so package_mac.sh zips with `zip -qry`. Both are checked here.
big=$work/big
mkdir "$big"
mkfile -n 4200m "$big/sparse"
want=$((4200 * 1024 * 1024))
(cd "$work" && zip -qry big.zip big)
zipinfo "$work/big.zip" | grep -q " $want " || fail "zipinfo does not list the full size"
mkdir "$work/big-out"
ditto -x -k "$work/big.zip" "$work/big-out"
[ "$(stat -f %z "$work/big-out/big/sparse")" = "$want" ] || fail "unpacked size differs"
rm -rf "$work/big.zip" "$work/big-out"
ditto -c -k "$big" "$work/big.zip"
mkdir "$work/big-out"
ditto -x -k "$work/big.zip" "$work/big-out"
[ "$(stat -f %z "$work/big-out/sparse")" = "$want" ] || fail "ditto's unpacked size differs"
rm -rf "$big" "$work/big.zip" "$work/big-out"
pass "ZIP64 past 4 GB"

# ---- the release workflow never names the script ----
! grep -q 'package_mac' "$root/.github/workflows/release.yml" || fail "release.yml names package_mac.sh"
pass "release.yml does not name the script"
echo "all package_mac tests passed"
