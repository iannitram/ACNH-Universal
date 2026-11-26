#!/usr/bin/env bash
# Checks that what the build depends on is pinned, and pinned the same everywhere (#328):
#   1. rust-toolchain.toml names an exact version, and every `toolchain:` in the workflows is that version;
#   2. every save-relay dependency in the manifests names the same `rev`, Cargo.lock resolves to it, and the
#      vendored copies (third_party/save-relay, android/java/org/saverelay) say they come from it;
#   3. every GitHub Action is pinned to a full commit SHA;
#   4. every cargo build, test, clippy, check and tree in CI and in tools/ passes --locked;
#   5. tools/downloads.sha256 is well formed, with one entry per URL.
# Run from anywhere: bash tools/check_pins.sh. Exit status 1 and one line per problem when something drifted.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

fail=0
bad() { echo "check_pins: $*" >&2; fail=1; }

# 1. Rust toolchain
channel=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)
if ! printf '%s' "$channel" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
    bad "rust-toolchain.toml channel must be an exact version like 1.100.0 (got '${channel:-none}')"
fi
for f in .github/workflows/*.yml; do
    while IFS= read -r v; do
        [ "$v" = "$channel" ] || bad "$f: toolchain $v differs from rust-toolchain.toml ($channel)"
    done < <(sed -n 's/^ *toolchain: *"\{0,1\}\([^"]*\)"\{0,1\} *$/\1/p' "$f")
    # each rust-toolchain step must carry a toolchain: input
    n_uses=$(grep -c 'dtolnay/rust-toolchain@' "$f")
    n_tc=$(grep -c '^ *toolchain: ' "$f")
    [ "$n_uses" -eq "$n_tc" ] || bad "$f: $n_uses dtolnay/rust-toolchain steps but $n_tc toolchain: inputs"
done

# 2. save-relay
rev=$(grep -h 'save-relay = {' app/Cargo.toml crates/*/Cargo.toml | sed -n 's/.*rev = "\([0-9a-f]\{40\}\)".*/\1/p' | sort -u)
if [ "$(printf '%s\n' "$rev" | grep -c .)" -ne 1 ]; then
    bad "save-relay manifests must all name one 40-digit rev (found: $(echo $rev))"
fi
if grep -h 'save-relay = {' app/Cargo.toml crates/*/Cargo.toml | grep -v 'rev = "[0-9a-f]\{40\}"' | grep -q .; then
    bad "a save-relay dependency has no rev"
fi
if ! grep -q "source = \"git+https://github.com/iannitram/save-relay?rev=$rev#$rev\"" Cargo.lock; then
    bad "Cargo.lock does not resolve save-relay to rev $rev"
fi
for f in third_party/save-relay/save_relay.h third_party/save-relay/save_relay_jni.c android/java/org/saverelay/*.java; do
    # the revision may wrap onto the next comment line, so look at the whole header
    head -n 4 "$f" | tr -d '\n' | grep -q "$rev" || bad "$f is not from save-relay rev $rev"
done

# 3. Actions by commit SHA
if grep -hn 'uses: ' .github/workflows/*.yml | grep -v 'uses: \./' | grep -v 'uses: [^ @]*@[0-9a-f]\{40\}\( \|$\)' | grep -q .; then
    grep -Hn 'uses: ' .github/workflows/*.yml | grep -v 'uses: \./' | grep -v 'uses: [^ @]*@[0-9a-f]\{40\}\( \|$\)' >&2
    bad "an Action is not pinned to a full commit SHA"
fi

# 4. --locked
if grep -En '\bcargo (build|test|clippy|check|tree)\b' .github/workflows/*.yml tools/*.sh \
    | grep -v -e '--locked' -e '^[^:]*:[0-9]*: *#' | grep -q .; then
    grep -En '\bcargo (build|test|clippy|check|tree)\b' .github/workflows/*.yml tools/*.sh \
        | grep -v -e '--locked' -e '^[^:]*:[0-9]*: *#' >&2
    bad "a cargo command lacks --locked"
fi

# 5. download checksums
table=tools/downloads.sha256
if ! awk 'NF { if (NF != 2 || $1 !~ /^[0-9a-f]{64}$/ || $2 !~ /^https:\/\//) exit 1 }' "$table"; then
    bad "$table has a line that is not '<sha256>  <https url>'"
fi
dups=$(awk 'NF { print $2 }' "$table" | sort | uniq -d)
[ -z "$dups" ] || bad "$table lists a URL twice: $dups"

# 6. tool versions live in tools/versions.env only
vf=tools/versions.env
if ! grep -v -e '^#' -e '^$' "$vf" | awk 'NF { if ($0 !~ /^[A-Z0-9_]+=[A-Za-z0-9._-]+$/) exit 1 }'; then
    bad "$vf has a line that is not KEY=value"
fi
dupk=$(grep -v -e '^#' -e '^$' "$vf" | cut -d= -f1 | sort | uniq -d)
[ -z "$dupk" ] || bad "$vf names a key twice: $dupk"
for k in SDL3 SDL3_IOS MOLTENVK VK_HEADERS LLVM IMGUI RYUBING DOTNET FFX DLSS; do
    grep -q "^$k=" "$vf" || bad "$vf lacks $k"
done
# a version spelled out elsewhere would drift from the file: none may appear in Prepare, CI or the scripts
while IFS== read -r k v; do
    case "$k" in "" | \#*) continue ;; esac
    # short or generic values (a bare commit prefix) are only checked in their quoted form
    for f in crates/core/src/prepare.rs .github/workflows/check.yml tools/build_ios.sh tools/build_android.sh; do
        if grep -nE "(^|[^0-9.])${v//./\\.}([^0-9]|\\.[^0-9]|$)" "$f" | grep -v -e '^[0-9]*: *#' -e '^[0-9]*: *//' | grep -q .; then
            grep -nE "(^|[^0-9.])${v//./\\.}([^0-9]|\\.[^0-9]|$)" "$f" | grep -v -e '^[0-9]*: *#' -e '^[0-9]*: *//' >&2
            bad "$f spells out $k=$v; read it from $vf"
        fi
    done
done < "$vf"

[ "$fail" -eq 0 ] && echo "check_pins: ok"
exit "$fail"
