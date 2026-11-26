#!/usr/bin/env bash
# Android private-storage checks (#192):
# 1. android/AndroidManifest.xml sets android:allowBackup="false" and android:dataExtractionRules.
# 2. android/res/xml/data_extraction_rules.xml excludes game/ from both cloud-backup and device-transfer.
# 3. Home is the only MAIN/LAUNCHER activity and holds the app shortcuts; LauncherActivity (Slint) is not exported (#363).
# 4. runtime/src/platform_android_wrapper.cpp (and platform.rs) does not reference data_root.txt or /storage/emulated/0/.
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

python3 - "$manifest" "$root/android/res/xml/shortcuts.xml" <<'PY'
import sys
import xml.etree.ElementTree as ET

A = "{http://schemas.android.com/apk/res/android}"
app = ET.parse(sys.argv[1]).getroot().find("application")
launchers = []
shortcut_owners = []
for act in app.findall("activity"):
    name = act.get(A + "name")
    for f in act.findall("intent-filter"):
        actions = {a.get(A + "name") for a in f.findall("action")}
        cats = {c.get(A + "name") for c in f.findall("category")}
        if "android.intent.action.MAIN" in actions and "android.intent.category.LAUNCHER" in cats:
            launchers.append(name)
    if any(m.get(A + "name") == "android.app.shortcuts" for m in act.findall("meta-data")):
        shortcut_owners.append(name)
    if name.endswith(".LauncherActivity") and act.get(A + "exported") != "false":
        sys.exit("AndroidManifest.xml: LauncherActivity (Slint) must not be exported (#363)")
home = "io.github.acnhrecomp.Home"
if launchers != [home]:
    sys.exit(f"AndroidManifest.xml: Home must be the only MAIN/LAUNCHER activity, got {launchers!r} (#363)")
if shortcut_owners != [home]:
    sys.exit(f"AndroidManifest.xml: android.app.shortcuts must be on Home only, got {shortcut_owners!r} (#363)")
targets = {i.get(A + "targetClass") for i in ET.parse(sys.argv[2]).getroot().iter("intent")}
if targets != {home}:
    sys.exit(f"shortcuts.xml: every shortcut must target Home, got {targets!r}")
PY

if bad=$(grep -nE 'data_root\.txt|/storage/emulated/0/' "$plat" "$plat_rs"); then
    printf 'platform Android files still reference outside storage:\n%s\n' "$bad" >&2
    exit 1
fi

printf 'android_app_check: private storage manifest, Home as launcher, rules, and platform_android_wrapper.cpp OK\n'
