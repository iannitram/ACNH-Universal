#!/usr/bin/env python3
"""The licence notices for the Rust crates linked into the Android app's lib<game>.so (crates/runtime for
aarch64-linux-android), for the APK's assets/licenses (tools/build_android.sh).

Writes <out>/rust-crates.txt (each crate, its version and its licence expression) and copies each crate's own licence
files (LICENSE*, LICENCE*, COPYING*, NOTICE*, UNLICENSE*: the MIT and Apache-2.0 texts the crates carry) into
<out>/rust/<crate>-<version>/. Offline (cargo's own lock and its downloaded sources), and no local path is written.
This repository's own crates (acnh-*) are under its own licence and are listed only. A crate from elsewhere with no
licence file fails the run, so the APK never ships without a text it needs.

usage: tools/rust_licenses.py <out folder>
"""
import json
import os
import re
import shutil
import subprocess
import sys

TARGET = "aarch64-linux-android"
OWN = re.compile(r"^acnh-")
NOTICE = re.compile(r"^(LICEN[CS]E|COPYING|NOTICE|UNLICENSE)", re.IGNORECASE)


def run(args):
    return subprocess.run(args, check=True, capture_output=True, text=True).stdout


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    out = sys.argv[1]
    # The crates actually linked (normal dependencies for the Android target), as name and version.
    tree = run(["cargo", "tree", "-p", "acnh-runtime", "--target", TARGET, "-e", "normal", "--prefix", "none",
                "--format", "{p}", "--offline"])
    wanted = set()
    for line in tree.splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[1].startswith("v"):
            wanted.add((parts[0], parts[1][1:]))
    if not wanted:
        sys.exit("rust_licenses: cargo tree listed no crates")
    meta = json.loads(run(["cargo", "metadata", "--offline", "--format-version", "1", "--filter-platform", TARGET]))
    packages = {}
    for p in meta["packages"]:
        packages.setdefault((p["name"], p["version"]), p)
    os.makedirs(os.path.join(out, "rust"), exist_ok=True)
    lines = ["The Rust crates in the app's native library (crates/runtime for %s), with their licences." % TARGET,
             "Each crate's licence texts are in rust/<crate>-<version>/.", ""]
    missing = []
    for name, version in sorted(wanted):
        p = packages.get((name, version))
        if p is None:
            missing.append("%s %s (not in cargo metadata)" % (name, version))
            continue
        lines.append("%s %s %s" % (name, version, p.get("license") or "(see its licence file)"))
        if OWN.match(name):
            continue
        folder = os.path.dirname(p["manifest_path"])
        # The crate's licence files by the name they are copied as, from its folder and its `license-file`.
        notices = {f: os.path.join(folder, f) for f in os.listdir(folder)
                   if NOTICE.match(f) and os.path.isfile(os.path.join(folder, f))}
        if p.get("license_file"):
            extra = os.path.join(folder, p["license_file"])
            if os.path.isfile(extra):
                notices.setdefault(os.path.basename(extra), extra)
        if not notices:
            missing.append("%s %s" % (name, version))
            continue
        dest = os.path.join(out, "rust", "%s-%s" % (name, version))
        os.makedirs(dest, exist_ok=True)
        for f, source in sorted(notices.items()):
            shutil.copyfile(source, os.path.join(dest, f if f.endswith(".txt") else f + ".txt"))
    with open(os.path.join(out, "rust-crates.txt"), "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
    if missing:
        sys.exit("rust_licenses: no licence file for: " + ", ".join(missing))


if __name__ == "__main__":
    main()
