#!/usr/bin/env bash
# tools/check_pins.sh passes on the tree and catches each kind of drift it guards (#328).
# It edits a throwaway copy of the files it reads, never the tree.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

bash tools/check_pins.sh >/dev/null || { echo "FAIL: check_pins fails on the tree" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# expect_fail <name> <file> <perl substitution, applied once per line>
expect_fail() {
    local name=$1 file=$2 expr=$3
    rm -rf "$work/t"
    mkdir -p "$work/t"
    cp -R rust-toolchain.toml Cargo.lock app crates tools third_party android .github "$work/t/" 2>/dev/null
    perl -pi -e "$expr" "$work/t/$file"
    if (cd "$work/t" && bash tools/check_pins.sh >/dev/null 2>&1); then
        echo "FAIL: check_pins did not catch: $name" >&2
        exit 1
    fi
    echo "ok: caught $name"
}

expect_fail "a floating toolchain" rust-toolchain.toml 's/^channel = .*/channel = "stable"/'
expect_fail "a toolchain that differs from CI" rust-toolchain.toml 's/^channel = "[0-9.]*"/channel = "1.1.1"/'
expect_fail "a save-relay dependency with no rev" crates/cli/Cargo.toml 's/, rev = "[0-9a-f]*"//'
expect_fail "a Cargo.lock that moved" Cargo.lock 's#save-relay\?rev=[0-9a-f]+#save-relay?rev=0000000000000000000000000000000000000000#'
expect_fail "a vendored copy from another revision" third_party/save-relay/save_relay.h 's/132b9e5/0000000/ if $. == 1'
expect_fail "an Action pinned by tag" .github/workflows/check.yml 's/actions\/checkout@[0-9a-f]{40}/actions\/checkout\@v4/'
expect_fail "a cargo command without --locked" .github/workflows/check.yml 's/cargo test --locked --workspace/cargo test --workspace/'
expect_fail "a malformed checksum" tools/downloads.sha256 's/^[0-9a-f]/Z/ if $. == 1'
echo "check_pins_test: ok"
