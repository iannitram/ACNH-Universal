#!/usr/bin/env bash
# Tests for tools/check_no_game_data.sh and tools/check_release_files.sh (#326): a scratch repo gets an empty or
# plain-text decoy of each kind (never a real game file, SPEC.md M14) and the check must refuse it. Both checks must
# read the one shared list in tools/game_data_rules.sh.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd -P)
check=$root/tools/check_no_game_data.sh
release=$root/tools/check_release_files.sh
work=$(mktemp -d "${TMPDIR:-/tmp}/no-game-data-test.XXXXXX")
trap 'rm -rf "$work"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "ok: $*"; }

repo=$work/repo
git init -q "$repo"
cd "$repo"
git config user.email test@example.invalid
git config user.name test
git config core.quotePath true   # the default: non-ASCII names arrive C-quoted without -z

reset() { git rm -rq --cached . 2>/dev/null || true; find . -mindepth 1 -maxdepth 1 ! -name .git -exec rm -rf {} +; }
# `refused <path> [content]`: a staged decoy at <path> makes the commit check fail.
refused() {
  reset
  mkdir -p "$(dirname "$1")"
  printf '%s\n' "${2:-decoy}" > "$1"
  git add -f -- "$1"
  if bash "$check" >"$work/out.log" 2>&1; then cat "$work/out.log" >&2; fail "the commit check let through: $1"; fi
  grep -q BLOCKED "$work/out.log" || { cat "$work/out.log" >&2; fail "no BLOCKED line for: $1"; }
}
accepted() {
  reset
  mkdir -p "$(dirname "$1")"
  printf '%s\n' "${2:-plain text}" > "$1"
  git add -f -- "$1"
  bash "$check" >"$work/out.log" 2>&1 || { cat "$work/out.log" >&2; fail "the commit check refused: $1"; }
}

accepted notes.txt
accepted docs/ünïcode notes.md
pass "plain files pass, including a non-ASCII name"

for ext in nsp xci nsz xcz nca nso npdm keys tik cert spv bin bfsha bntx szs zs sarc bfres ttf otf pack pak sav \
           byml msbt bars bwav dat zip 7z; do
  refused "decoy.$ext"
  refused "DECOY.$(printf '%s' "$ext" | tr '[:lower:]' '[:upper:]')"
done
pass "every game file type is refused (text content, so only the name can catch it)"

for dir in gen exefs romfs aoc private refs; do
  refused "$dir/listing.txt"
  refused "sub/$dir/listing.txt"
done
pass "every game folder is refused, at the top and nested"

# The blind spot: a name git C-quotes (core.quotePath) used to be skipped.
refused "ünï/decoy.nsp"
refused "日本語/gen/table.txt"
refused 'tab	name.sav'
refused 'quo"te.byml'
pass "quoted names are read with -z and refused"

refused "ünï/body.txt" 'void main_0123ABCD(nx::CPU'
refused "日本語.txt" 'master_key = 00112233445566778899aabbccddeeff'
pass "content rules still see a non-ASCII path"

# A staged path that is not a file (a gitlink: a folder) cannot be scanned and must fail, not be skipped.
reset
git update-index --add --cacheinfo 160000,0123456789abcdef0123456789abcdef01234567,vendor/odd
if bash "$check" >"$work/out.log" 2>&1; then cat "$work/out.log" >&2; fail "an unreadable path was skipped"; fi
grep -q 'cannot read' "$work/out.log" || fail "no 'cannot read' message"
pass "a path that is not a file fails"

# Both checks read the shared list: the release check refuses the same names.
for p in decoy.byml decoy.msbt decoy.bars decoy.bwav decoy.nsz decoy.sav a/gen/x a/exefs/x a/romfs/x a/aoc/x; do
  if printf '%s\n' "$p" | bash "$release" >/dev/null 2>&1; then fail "the release check let through: $p"; fi
done
printf '%s\n' ACNH-Universal.AppImage bin/acnh-app README.txt | bash "$release" >/dev/null || fail "the release check refused clean names"
pass "the release check reads the same list"

# The hook form: a symlink in .git/hooks still finds the shared list.
reset
ln -s "$check" .git/hooks/pre-commit
printf 'x\n' > decoy.nsp
git add decoy.nsp
if .git/hooks/pre-commit >"$work/out.log" 2>&1; then fail "the hook let a decoy through"; fi
grep -q BLOCKED "$work/out.log" || { cat "$work/out.log" >&2; fail "the hook did not run the rules"; }
pass "the hook symlink works"
echo "all passed"
