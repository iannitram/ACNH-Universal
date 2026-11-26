# shellcheck shell=bash
# Sourced by tools/package_ios.sh and tools/package_mac.sh: the steps the two packaging scripts share (#362).
#
# Each pkg_* function that can refuse prints the reason on stdout and returns 1; the caller turns that into its own
# die/fail, so both scripts refuse with the same words and neither writes anything before every refusal has passed.

pkg_realpath() { python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$1"; }

# pkg_existing_parent <path>: the nearest folder that exists at or above <path>.
pkg_existing_parent() {
    local p=$1
    while [ ! -d "$p" ]; do p=$(dirname "$p"); done
    printf '%s\n' "$p"
}

# pkg_refuse_git <out folder>: fail closed. Any git error other than "not a git repository" (for example a
# dubious-ownership refusal) refuses the folder, as does a folder inside a work tree or a .git folder.
pkg_refuse_git() {
    local existing git_location
    existing=$(pkg_existing_parent "$1")
    if ! git_location=$(git -C "$existing" rev-parse --is-inside-work-tree --is-inside-git-dir 2>&1); then
        case "$git_location" in
            *"not a git repository"*) git_location= ;;
            *) printf 'cannot tell whether the out folder is inside a Git work tree: %s\n' "$git_location"; return 1 ;;
        esac
    fi
    if printf '%s\n' "$git_location" | grep -qx true; then
        printf 'out folder is inside a Git work tree (or its metadata): choose a folder outside every checkout\n'
        return 1
    fi
    return 0
}

# pkg_refuse_ci_out <out folder>: under CI the out folder must be inside $RUNNER_TEMP.
pkg_refuse_ci_out() {
    [ -n "${CI:-}${GITHUB_ACTIONS:-}" ] || return 0
    if [ -z "${RUNNER_TEMP:-}" ]; then
        printf 'CI is set but RUNNER_TEMP is not, so the out folder cannot be checked\n'
        return 1
    fi
    local temp
    temp=$(pkg_realpath "$RUNNER_TEMP")
    case "$1/" in
        "$temp/"*) return 0 ;;
        *) printf 'under CI the out folder must be inside $RUNNER_TEMP (%s)\n' "$RUNNER_TEMP"; return 1 ;;
    esac
}

# pkg_stage_ok <candidate> <out folder> <volume id of the out folder>: is the candidate a folder the package can
# be staged in: it exists, is writable, is not the out folder or inside it, and is on the out folder's volume.
pkg_stage_ok() {
    [ -d "$1" ] && [ -w "$1" ] || return 1
    local cand
    cand=$(pkg_realpath "$1")
    case "$cand/" in "$2/"*) return 1 ;; esac
    [ "$(stat -f %d "$cand")" = "$3" ]
}

# pkg_stage_path <out folder> <version>: where the package is staged (not created). The out folder is the owner's
# Dropbox delivery folder, so staging there writes the whole game copy into a synced folder before the final rename.
# Stage instead outside the synced tree on the same volume, so the finished archive moves into the out folder with
# one rename: under $ACNH_STAGE_ROOT when set (refused if it is not usable), else $TMPDIR, else ~/Library/Caches,
# the first that exists, is writable, is not inside the out folder and is on the out folder's volume. When none is,
# staging falls back to the out folder (the earlier behaviour). The name is fixed per out folder and version, so a
# run killed before its clean-up leaves a stage that the next run clears.
pkg_stage_path() {
    local out=$1 version=$2 dev tag cand
    dev=$(stat -f %d "$(pkg_existing_parent "$out")")
    tag=$(printf '%s' "$out" | shasum -a 1 | cut -c1-10)
    if [ -n "${ACNH_STAGE_ROOT:-}" ]; then
        pkg_stage_ok "$ACNH_STAGE_ROOT" "$out" "$dev" ||
            { printf 'ACNH_STAGE_ROOT must be a writable folder outside the out folder, on its volume: %s\n' "$ACNH_STAGE_ROOT"; return 1; }
        cand=$(pkg_realpath "$ACNH_STAGE_ROOT")
        printf '%s/acnh-universal-stage-%s/.ACNH-Universal-%s.partial\n' "$cand" "$tag" "$version"
        return 0
    fi
    for cand in "${TMPDIR:-}" "${HOME:-}/Library/Caches"; do
        [ -n "$cand" ] || continue
        if pkg_stage_ok "$cand" "$out" "$dev"; then
            cand=$(pkg_realpath "$cand")
            printf '%s/acnh-universal-stage-%s/.ACNH-Universal-%s.partial\n' "$cand" "$tag" "$version"
            return 0
        fi
    done
    printf '%s/.ACNH-Universal-%s.partial\n' "$out" "$version"
}

# pkg_clear_stage <stage> <out folder> <version>: before a run, clear the stage a killed run left, and the one an older
# script left inside the out folder. Returns 1 when the stage cannot be removed; an old in-out stage that cannot be
# removed is only a warning. Needs tools/remove_tree.sh sourced.
pkg_clear_stage() {
    local stage=$1 out=$2 version=$3 legacy
    legacy=$out/.ACNH-Universal-$version.partial
    remove_tree "$stage" || return 1
    if [ "$legacy" != "$stage" ]; then
        remove_tree "$legacy" || echo "warning: $legacy is left from an older run and cannot be removed: remove it" >&2
    fi
    return 0
}

# pkg_drop_stage <stage> <out folder>: remove the stage after a run, and its folder of its own when it is not the out folder.
pkg_drop_stage() {
    local stage=$1 out=$2 parent
    if ! remove_tree "$stage"; then
        echo "warning: could not remove $stage (something keeps writing into it): remove it before the next run" >&2
    fi
    parent=$(dirname "$stage")
    if [ "$parent" != "$out" ]; then rmdir "$parent" 2>/dev/null || true; fi
    return 0
}
