#!/usr/bin/env bash
# The player-facing name is "ACNH Universal" (#401): no string a player reads says "ACNHRecomp", and nothing points
# at upstream's Discord server. Credits, the "ACNHRecomp-Importer" compatibility name, upstream URLs and code comments
# stay (a comment line starts with //, /// or //! after its indentation).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

fail=0
files="runtime/src/ui/i18n.cpp runtime/src/ui/i18n_launcher.inc runtime/src/platform_sdl_wrapper.cpp
runtime/src/platform_linux_wrapper.cpp runtime/src/platform_mac_wrapper.cpp runtime/src/files_wrapper.cpp
runtime/src/nextendo.cpp crates/runtime/src/files/ffi_extra.rs crates/runtime/src/files/mod.rs
android/java/io/github/acnhrecomp/Home.java android/java/io/github/acnhrecomp/Prepare.java ios/Info.plist.in"
for f in $files; do
    # Drop the names that stay (upstream URLs, the Importer's name), then look for what is left.
    hits=$(grep -n "ACNHRecomp" "$f" \
        | grep -v -E '^[0-9]+:[[:space:]]*(//|\*|<!--)' \
        | grep -v "launcher.credits" \
        | sed -e 's#Nycolazs/ACNHRecomp##g' -e 's#ACNHRecomp-Importer##g' \
        | grep "ACNHRecomp" || true)
    if [ -n "$hits" ]; then
        echo "FAIL: $f still names the product ACNHRecomp:" >&2
        echo "$hits" | cut -c1-140 >&2
        fail=1
    fi
done

if grep -rn "discord.gg" runtime/src crates app android/java ios 2>/dev/null | grep -v -E ':[0-9]+:[[:space:]]*//'; then
    echo "FAIL: a link to a Discord server is back in the code" >&2
    fail=1
fi
if ! grep -q 'config_get_bool("discord.enabled", false)' runtime/src/discord_presence.cpp; then
    echo "FAIL: Rich Presence is not off by default" >&2
    fail=1
fi
[ "$fail" = 0 ] && echo "user_facing_name_test: ok"
exit "$fail"
