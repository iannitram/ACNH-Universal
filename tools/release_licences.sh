#!/usr/bin/env bash
# The licence texts a desktop release carries (issue #329): the repo's LICENSE and NOTICE, the texts of the C code the
# launcher and the command line link (Capstone) and of save-relay, and the licence texts of every Rust crate linked into
# acnh-app and acnh-cli for the target. Run by release.yml on each runner, after its cargo build (the host target).
# usage: tools/release_licences.sh <folder to write into> <rust target triple>
# Writes <folder>/LICENSE.txt, <folder>/NOTICE.txt and <folder>/licences/ (rust-crates.txt, rust/, capstone/, save-relay/).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd -P)
usage() { echo "usage: tools/release_licences.sh <folder to write into> <rust target triple>" >&2; exit 2; }
[ "$#" -eq 2 ] || usage
out=$1
target=$2
case "$target" in ''|*[!a-zA-Z0-9._-]*) usage ;; esac
[ -d "$out" ] || { echo "error: the folder to write into does not exist: $out" >&2; exit 1; }

# The Capstone and save-relay texts from the repo's own copies (the APK ships the same ones).
cp "$root/LICENSE" "$out/LICENSE.txt"
cp "$root/NOTICE" "$out/NOTICE.txt"
mkdir -p "$out/licences/capstone" "$out/licences/save-relay"
cp "$root/third_party/capstone/LICENSE.TXT" "$out/licences/capstone/LICENSE-capstone.txt"
cp "$root/third_party/capstone/LICENSE_LLVM.TXT" "$out/licences/capstone/LICENSE-capstone-llvm.txt"
cp "$root/third_party/save-relay/LICENSE" "$out/licences/save-relay/LICENSE-save-relay.txt"

# Python: the runner's python3, or its python where python3 is a stub (Windows).
py=
for candidate in python3 python; do
  if "$candidate" -c 'import sys' >/dev/null 2>&1; then py=$candidate; break; fi
done
[ -n "$py" ] || { echo "error: python is needed for the Rust licence list" >&2; exit 1; }

# Every crate of the lock must be downloaded before the offline licence listing (cargo fetch covers all targets).
(cd "$root" && cargo fetch --locked >/dev/null)
(cd "$root" && "$py" tools/rust_licenses.py "$out/licences" --target "$target" --package acnh-app --package acnh-cli)
echo "ok: licence texts for $target in $out"
