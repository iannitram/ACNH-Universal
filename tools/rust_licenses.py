#!/usr/bin/env python3
"""The licence notices for the Rust crates linked into a shipped build, for the package's licence folder.

By default: the crates linked into the Android app's lib<game>.so (crates/runtime for aarch64-linux-android), for the
APK's assets/licenses (tools/build_android.sh). With --target and --package: the crates linked into those packages for
that target, for the desktop releases (.github/workflows/release.yml: acnh-app and acnh-cli for each host).

Writes <out>/rust-crates.txt (each crate, its version and its licence expression) and copies each crate's own licence
texts (LICENSE*, LICENCE*, COPYING*, NOTICE*, UNLICENSE* in its folder, and every file in its LICENSES/ folder) into
<out>/rust/<crate>-<version>/. Many crates publish no licence file at all: for those whose licence is MIT, Apache-2.0
or BSL-1.0 the standard text from tools/licence-templates/ is written instead (an MIT copyright line names the crate's
Cargo.toml authors, without their e-mail addresses), and a crate under Slint's licence is covered by the slint crate's
own texts. Offline (cargo's own lock and its downloaded sources), and no local path is written. This repository's own
crates (acnh-*) are under its own licence and are listed only. Any other crate with no text fails the run, so a package
never ships without a text it needs.

usage: tools/rust_licenses.py <out folder> [--target <triple>] [--package <name>]...
  --target   the Rust target triple (default aarch64-linux-android)
  --package  a package whose normal dependencies are listed (default acnh-runtime; repeat for more)
"""
import json
import os
import re
import shutil
import subprocess
import sys

DEFAULT_TARGET = "aarch64-linux-android"
DEFAULT_PACKAGES = ["acnh-runtime"]
OWN = re.compile(r"^acnh-")
NOTICE = re.compile(r"^(LICEN[CS]E|COPYING|NOTICE|UNLICENSE)", re.IGNORECASE)
TEMPLATES = os.path.join(os.path.dirname(os.path.abspath(__file__)), "licence-templates")
TEMPLATE_FILES = {"MIT": "MIT.txt", "Apache-2.0": "Apache-2.0.txt", "BSL-1.0": "BSL-1.0.txt"}
MIT_HOLDER = "<year> <copyright holders>"
OPERATORS = {"OR", "AND", "WITH"}


def run(args):
    return subprocess.run(args, check=True, capture_output=True, text=True).stdout


def terms(expression):
    """The licence identifiers in a licence expression (MIT OR Apache-2.0, MIT/Apache-2.0, ...)."""
    return [t for t in re.findall(r"[A-Za-z0-9.+-]+", expression or "") if t not in OPERATORS]


def holders(package):
    """The copyright holders for an MIT text: the Cargo.toml authors' names, without their e-mail addresses."""
    names = [re.sub(r"<[^>]*>", "", a).strip() for a in package.get("authors") or []]
    names = [n for n in names if n]
    return ", ".join(names) if names else "the authors of %s" % package["name"]


def copy_texts(package, folder, dest):
    """Copies the licence texts a crate ships: its licence files and its LICENSES/ folder. Returns how many."""
    found = {}
    for f in os.listdir(folder):
        path = os.path.join(folder, f)
        if NOTICE.match(f) and os.path.isfile(path):
            found[f] = path
    if package.get("license_file"):
        extra = os.path.join(folder, package["license_file"])
        if os.path.isfile(extra):
            found.setdefault(os.path.basename(extra), extra)
    reuse = os.path.join(folder, "LICENSES")
    if os.path.isdir(reuse):
        for f in os.listdir(reuse):
            path = os.path.join(reuse, f)
            if os.path.isfile(path):
                found.setdefault(os.path.join("LICENSES", f).replace(os.sep, "-"), path)
    if not found:
        return 0
    os.makedirs(dest, exist_ok=True)
    for f, source in sorted(found.items()):
        shutil.copyfile(source, os.path.join(dest, f if f.endswith(".txt") else f + ".txt"))
    return len(found)


def write_template_texts(package, chosen, dest):
    """Writes the standard text of each of the chosen licences (MIT, Apache-2.0 or BSL-1.0) the crate ships no text of."""
    os.makedirs(dest, exist_ok=True)
    for term in chosen:
        with open(os.path.join(TEMPLATES, TEMPLATE_FILES[term]), encoding="utf-8") as src:
            text = src.read()
        if term == "MIT":
            text = text.replace(MIT_HOLDER, holders(package))
        with open(os.path.join(dest, "LICENSE-%s.txt" % term), "w", encoding="utf-8") as out:
            out.write(text)


def main():
    args = sys.argv[1:]
    target = DEFAULT_TARGET
    roots = []
    positional = []
    i = 0
    while i < len(args):
        if args[i] in ("--target", "--package"):
            if i + 1 >= len(args) or not args[i + 1]:
                sys.exit(__doc__)
            if args[i] == "--target":
                target = args[i + 1]
            else:
                roots.append(args[i + 1])
            i += 2
        elif args[i].startswith("--"):
            sys.exit(__doc__)
        else:
            positional.append(args[i])
            i += 1
    if len(positional) != 1:
        sys.exit(__doc__)
    out = positional[0]
    roots = roots or DEFAULT_PACKAGES
    # The crates actually linked (normal dependencies for the target), as name and version.
    wanted = set()
    for root_package in roots:
        tree = run(["cargo", "tree", "-p", root_package, "--target", target, "-e", "normal", "--prefix", "none",
                    "--format", "{p}", "--offline"])
        for line in tree.splitlines():
            parts = line.split()
            if len(parts) >= 2 and parts[1].startswith("v"):
                wanted.add((parts[0], parts[1][1:]))
    if not wanted:
        sys.exit("rust_licenses: cargo tree listed no crates")
    # Every package in the lock (not filtered to one platform), so a crate of another target's tree is still found.
    meta = json.loads(run(["cargo", "metadata", "--offline", "--format-version", "1"]))
    by_id = {}
    for p in meta["packages"]:
        by_id.setdefault((p["name"], p["version"]), p)
    os.makedirs(os.path.join(out, "rust"), exist_ok=True)
    lines = ["The Rust crates linked into this build (%s: %s), with their licences." % (target, ", ".join(roots)),
             "Each crate's licence texts are in rust/<crate>-<version>/.", ""]
    missing = []
    for name, version in sorted(wanted):
        p = by_id.get((name, version))
        if p is None:
            missing.append("%s %s (not in cargo metadata)" % (name, version))
            continue
        expression = p.get("license") or ""
        lines.append("%s %s %s" % (name, version, expression or "(see its licence file)"))
        if OWN.match(name):
            continue
        folder = os.path.dirname(p["manifest_path"])
        dest = os.path.join(out, "rust", "%s-%s" % (name, version))
        if copy_texts(p, folder, dest):
            continue
        # A choice ("MIT OR Zlib", "MIT/Apache-2.0"): the texts of the alternatives this repo takes (MIT, Apache-2.0,
        # BSL-1.0) are enough. An expression of several licences that all need a text: every one of them.
        chosen = [t for t in terms(expression) if t in TEMPLATE_FILES]
        choice = " OR " in expression or "/" in expression
        if chosen and (choice or len(chosen) == len(terms(expression))):
            write_template_texts(p, chosen, dest)
            continue
        if expression and any(t.startswith("LicenseRef-Slint") for t in terms(expression)):
            # Slint's own sub-crates ship no texts: the slint crate's LICENSES/ folder is the one copy for all of them.
            continue
        missing.append("%s %s" % (name, version))
    with open(os.path.join(out, "rust-crates.txt"), "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
    if missing:
        sys.exit("rust_licenses: no licence text for: " + ", ".join(missing))


if __name__ == "__main__":
    main()
