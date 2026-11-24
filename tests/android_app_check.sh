#!/usr/bin/env bash
# Android private-storage checks (#192):
# 1. android/AndroidManifest.xml sets android:allowBackup="false" and android:dataExtractionRules.
# 2. android/res/xml/data_extraction_rules.xml excludes game/ from both cloud-backup and device-transfer.
# 3. runtime/src/platform_android_wrapper.cpp (and platform.rs) does not reference data_root.txt or /storage/emulated/0/.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root="$here/.."

manifest="$root/android/AndroidManifest.xml"
rules="$root/android/res/xml/data_extraction_rules.xml"
plat="$root/runtime/src/platform_android_wrapper.cpp"
plat_rs="$root/crates/runtime/src/platform.rs"

python3 - "$manifest" "$rules" <<'PY'
import sys
import xml.etree.ElementTree as ET

manifest_path, rules_path = sys.argv[1], sys.argv[2]
ns = {"android": "http://schemas.android.com/apk/res/android"}

m_tree = ET.parse(manifest_path)
app = m_tree.getroot().find("application")
if app is None:
    sys.exit("AndroidManifest.xml: missing <application>")

allow_backup = app.get(f"{{{ns['android']}}}allowBackup")
if allow_backup != "false":
    sys.exit(f"AndroidManifest.xml: android:allowBackup must be 'false', got {allow_backup!r}")

extraction_rules = app.get(f"{{{ns['android']}}}dataExtractionRules")
if not extraction_rules:
    sys.exit("AndroidManifest.xml: missing android:dataExtractionRules")

r_tree = ET.parse(rules_path)
r_root = r_tree.getroot()
for section in ("cloud-backup", "device-transfer"):
    el = r_root.find(section)
    if el is None:
        sys.exit(f"data_extraction_rules.xml: missing <{section}>")
    excludes = [
        (c.get("domain"), c.get("path"))
        for c in el.findall("exclude")
    ]
    if not any(d == "file" and p in ("game", "game/", ".") for d, p in excludes):
        sys.exit(f"data_extraction_rules.xml: <{section}> does not exclude game/")
    if not any(d == "file" and p in ("game", "game/") for d, p in excludes):
        sys.exit(f"data_extraction_rules.xml: <{section}> must explicitly exclude domain='file' path='game'")
PY

if bad=$(grep -nE 'data_root\.txt|/storage/emulated/0/' "$plat" "$plat_rs"); then
    printf 'platform Android files still reference outside storage:\n%s\n' "$bad" >&2
    exit 1
fi

printf 'android_app_check: private storage manifest, rules, and platform_android_wrapper.cpp OK\n'
