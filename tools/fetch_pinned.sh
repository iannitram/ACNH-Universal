#!/usr/bin/env bash
# Download a URL to a file and check it against the SHA-256 recorded in tools/downloads.sha256 (#328).
# Usage: tools/fetch_pinned.sh <url> <output-file>
# A URL with no recorded checksum is refused; a mismatch deletes the download and fails.
# The same table feeds Prepare (crates/core/src/prepare/tools.rs) and tools/build_ios.sh.
set -euo pipefail

[ $# -eq 2 ] || { echo "usage: $0 <url> <output-file>" >&2; exit 2; }
url=$1
out=$2
table=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/downloads.sha256

want=$(awk -v u="$url" '$2 == u { print $1; exit }' "$table")
if [ -z "$want" ]; then
    echo "error: no recorded SHA-256 for $url; add it to tools/downloads.sha256" >&2
    exit 1
fi

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{ print $1 }'
    else
        shasum -a 256 "$1" | awk '{ print $1 }'
    fi
}

tmp="$out.part"
curl --fail --silent --show-error --location --retry 3 --output "$tmp" "$url"
got=$(sha256 "$tmp")
if [ "$got" != "$want" ]; then
    rm -f "$tmp"
    echo "error: SHA-256 mismatch for $url: expected $want, got $got" >&2
    exit 1
fi
mv "$tmp" "$out"
