# shellcheck shell=bash
# Sourced by tools/package_ios.sh, tools/package_mac.sh and tools/stage_dropbox_game.sh.
#
# remove_tree <path>: remove a folder (or file, or link) that may sit in a Dropbox or Finder-watched folder. Finder and
# Dropbox can write a .DS_Store into it while rm -rf walks it, and rm then stops with "Directory not empty"; try
# again a few times. Returns 0 once the path is gone and 1 if it is still there, so the caller decides whether that
# stops the run (before a swap) or is only a warning (clean-up after the delivery is complete). Safe under set -e.
remove_tree() {
    local tries=0
    while [ -e "$1" ] || [ -L "$1" ]; do
        chmod -R u+w "$1" 2>/dev/null || true
        rm -rf "$1" 2>/dev/null && return 0
        tries=$((tries + 1))
        [ "$tries" -lt "${REMOVE_TREE_TRIES:-5}" ] || return 1
        sleep "${REMOVE_TREE_WAIT:-1}"
    done
    return 0
}
