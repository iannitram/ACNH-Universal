#!/usr/bin/env bash
# The game-data check over a release's file list (SPEC.md M14): reads one path per line on stdin
# (an archive listing, an AppDir's find output) and refuses game data, keys, packs, shaders and gen/.
# usage: <list files> | tools/check_release_files.sh
#
# One exception, off unless ACNH_ALLOW_BUNDLED_MODS=1 (only package_mac.sh and package_ios.sh set it; the public
# release check in release.yml does not, so the release can never carry them): the mods an Apple package carries beside the player (tools/bundle_mods.sh), exactly
# ACNH-Universal.app/Contents/Resources/mods/{ptbr,speedfix60}/romfs/ in the Mac zip and Payload/ACNH.app/mods/
# {ptbr,speedfix60}/romfs/ in the .ipa. There the romfs/ folder and the .zs, .sarc and .pack names (the mods' own
# files) pass; every other rule still applies to the rest of the path, and nothing else under romfs/ passes.
mods_prefix='^/([^/]+\.app/contents/resources|payload/acnh\.app)/mods/(ptbr|speedfix60)/romfs/'
# shellcheck source=tools/game_data_rules.sh
. "$(dirname "${BASH_SOURCE[0]}")/game_data_rules.sh"
bad=0
while IFS= read -r f ||[ -n "$f" ]; do
  lf=/$(printf '%s' "$f" | tr '[:upper:]' '[:lower:]' | tr '\\' '/')
  if [ "${ACNH_ALLOW_BUNDLED_MODS:-}" = 1 ] && [[ $lf =~ $mods_prefix ]]; then
    rest=${lf#"${BASH_REMATCH[0]}"}
    rest=${rest%/}
    case "/$rest/" in
      //) continue ;;
      */../*|*//*) ;;
      *) lf=/$rest
         case "$lf" in *.zs|*.sarc|*.pack) lf=$lf.mod ;; esac ;;
    esac
  fi
  if game_data_ext_blocked "$lf"; then
    echo "BLOCKED (game/derived file type): $f"; bad=1; continue
  fi
  case "$lf" in
    */lib*_game.*|*/acnh_game.*)
      echo "BLOCKED (game/derived file type): $f"; bad=1; continue ;;
  esac
  if game_data_dir_blocked "$lf"; then
    echo "BLOCKED (game folder, recompiled code or saves): $f"; bad=1; continue
  fi
  case "$lf" in
    */game/*|*/device_save/*|*/backups/*)
      echo "BLOCKED (game folder, recompiled code or saves): $f"; bad=1 ;;
  esac
done
[ $bad = 0 ] && echo "ok: no game data in the release file list" || { echo "release refused"; exit 1; }
