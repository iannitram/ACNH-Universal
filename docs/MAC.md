# The Mac app: delivery note

Prepare's `mac` target (`acnh-universal build --target mac`) delivers one file to your Dropbox:
`_transfers/ACNH-Universal/mac/ACNH-Universal-<version>.zip`. It holds `ACNH-Universal.app` with your prepared game
inside, for Apple silicon only.

## Opening it

The app is signed ad hoc (`codesign -s -`): there is no Developer ID and no notarization. Nothing in the package or in
Prepare removes the quarantine flag macOS puts on a file that came from a download or a sync client, so the first open
is checked by Gatekeeper.

1. Unzip the file with Finder (double-click) or `ditto -x -k ACNH-Universal-<version>.zip <folder>`.
2. Move `ACNH-Universal.app` out of the Dropbox folder, for example to Applications.
3. Double-click it. On macOS 15 (Sequoia) and later the "Open" item of the right-click menu no longer bypasses the check;
   macOS says it cannot verify the app. Click *Done*, open *System Settings* > *Privacy & Security*, scroll to the
   message about `ACNH-Universal`, click *Open Anyway* and confirm with your password or Touch ID. This is asked once;
   later starts open normally.
4. Or remove the flag yourself before the first open: `xattr -dr com.apple.quarantine /Applications/ACNH-Universal.app`.

Do not edit anything inside the app: its signature seals the game files too, and a changed app fails the check again.
Saves, settings and logs live in `~/Library/Application Support`, not in the app.

## How the package is staged

`tools/package_mac.sh` (and `tools/package_ios.sh` for the .ipa) build the package in a folder of their own under
`$ACNH_STAGE_ROOT`, `$TMPDIR` or `~/Library/Caches`, on the same volume as the delivery folder, so the full game copy
is never written into the synced folder. Only the finished zip (or .ipa and game folder) is moved in, with one rename.
A stage left by a killed run is cleared by the next run. When no such folder is on the delivery folder's volume, the
scripts stage inside it, as before.
