#!/usr/bin/env bash
# tools/build_version.sh stamps the same version on every clone (issue #324): the base comes from the workspace
# version in Cargo.toml, never from whichever remote's tags a clone happens to hold.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
die() { echo "FAIL: $*" >&2; exit 1; }
git_() { git -C "$1" -c user.name=t -c user.email=t@t.invalid "${@:2}"; }
stamp() { (cd "$1" && env -u NX_RELEASE_VERSION bash tools/build_version.sh | tr '\n' ' '); }
field() { printf '%s\n' "$1" | tr ' ' '\n' | sed -n "s/^$2=//p"; }

src=$work/src
mkdir -p "$src/tools"
cp "$root/tools/build_version.sh" "$src/tools/"
printf '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "0.12.0"\nedition = "2021"\n\n[package]\nversion = "9.9.9"\n' > "$src/Cargo.toml"
git init -q "$src"
git_ "$src" add -A
git_ "$src" commit -q -m one
git_ "$src" commit -q --allow-empty -m two

# 1. No tags at all: the manifest's version, a rising code, a non-release name.
s=$(stamp "$src")
[ "$(field "$s" BASE_VER)" = 0.12.0 ] || die "base is not the manifest version: $s"
case "$(field "$s" VNAME)" in 0.12.0+*) ;; *) die "off-tag name is not <base>+<sha>: $s" ;; esac
[ "$(field "$s" VCODE)" = 1200002 ] || die "VCODE is not base*1000+commits: $s"

# 2. A clone holding upstream's tags (older and newer-looking names) stamps exactly the same.
git_ "$src" tag v0.11.0
git_ "$src" tag v0.20.0
git_ "$src" tag mods
[ "$(stamp "$src")" = "$s" ] || die "foreign tags changed the stamp: $(stamp "$src") vs $s"

# 3. A clone without tags agrees with it; a shallow clone has the same base.
git clone -q --no-tags "file://$src" "$work/plain"
[ "$(stamp "$work/plain")" = "$s" ] || die "a tagless clone stamps differently: $(stamp "$work/plain")"
git clone -q --depth 1 "file://$src" "$work/shallow"
sb=$(stamp "$work/shallow")
[ "$(field "$sb" BASE_VER)" = 0.12.0 ] || die "a shallow clone has another base: $sb"

# 4. Only our own tag, v<manifest version>, makes a clean release name.
git_ "$src" tag v0.12.0
r=$(stamp "$src")
[ "$(field "$r" VNAME)" = 0.12.0 ] || die "HEAD tagged v0.12.0 is not a clean release: $r"
[ "$(field "$r" VCODE)" = "$(field "$s" VCODE)" ] || die "tagging changed the code"

# 5. NX_RELEASE_VERSION overrides the base; a dev run name (not a version) is ignored.
o=$(cd "$src" && NX_RELEASE_VERSION=v1.0.0 bash tools/build_version.sh | tr '\n' ' ')
[ "$(field "$o" VNAME)" = 1.0.0 ] || die "release version not used: $o"
[ "$(field "$o" VCODE)" = 10000002 ] || die "release code wrong: $o"
d=$(cd "$src" && NX_RELEASE_VERSION=dev-7 bash tools/build_version.sh | tr '\n' ' ')
[ "$(field "$d" BASE_VER)" = 0.12.0 ] || die "dev run name changed the base: $d"

# 6. A packaged source (no .git) takes BASE_VER and COMMITS from its BUILD_VERSION file.
mkdir -p "$work/pkg/tools"
cp "$src/tools/build_version.sh" "$work/pkg/tools/"
cp "$src/Cargo.toml" "$work/pkg/"
printf 'BASE_VER=0.12.0\nCOMMITS=1029\n' > "$work/pkg/BUILD_VERSION"
p=$(stamp "$work/pkg")
[ "$(field "$p" VCODE)" = 1201029 ] || die "packaged source code wrong: $p"

echo "build_version_test: ok"
