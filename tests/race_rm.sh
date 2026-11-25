# shellcheck shell=bash
# Sourced by the packaging and Dropbox staging tests (tests/package_ios_test.sh, tests/package_mac_test.sh,
# tests/stage_dropbox_game_test.sh) to stand in for the race tools/remove_tree.sh handles.
#
make_race_rm() { # <bin folder>: an rm that, once per folder, fails as rm -rf does when Finder or Dropbox writes a
    # .DS_Store into a folder while it is being removed ("Directory not empty"); every other call is the real rm.
    mkdir -p "$1"
    cat > "$1/rm" <<'SHIM'
#!/bin/bash
for a in "$@"; do
    case "$a" in
        */.ACNH-Universal-*.partial|*/ios.old|*/android.old|*/android.new)
            if [ -d "$a" ] && [ ! -e "$RACE_MARKS/$(basename "$a")" ]; then
                : > "$RACE_MARKS/$(basename "$a")"
                : > "$a/.DS_Store" 2>/dev/null || true
                echo "rm: $a: Directory not empty" >&2
                exit 1
            fi ;;
    esac
done
exec /bin/rm "$@"
SHIM
    chmod +x "$1/rm"
}
