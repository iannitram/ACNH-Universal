# The Continue screen: design brief

Status: draft for approval (SPEC.md M10). Shared with totk-universal: the layout, states and input rules are the same; only the game's icon, name and version list differ. Words follow `GLOSSARY.md`. The view models live in `app/src/vm/`, input in `app/src/input.rs`, pages in `app/ui/` (ADR 0003, `docs/code-map.md#faces`). `.slint` files only draw; every state below is a plain Rust value in a view model, so each is tested with fake saves and no window.

## Principles

1. **One press to play.** From Game Mode, A on the opened launcher syncs and starts the game.
2. **Controller first, mouse works too.** Every action is reachable with D-pad/stick, A (accept), B (back), and Menu. Touch and mouse click the same targets. No hover-only information; no text entry that needs a keyboard (pairing uses a code shown, not typed, where possible; otherwise an on-screen digit pad).
3. **One screen, many states.** The Continue page never navigates away to ask a question; conflicts, drift and errors replace the card area in place.
4. **Say what happened, in words from the glossary.** No stack traces, no home paths, no hex ids in the UI (`prepare::redact` applies to any log line shown).
5. **Never lose the island.** Anything that could overwrite the island backs it up first (`backup`), and the conflict view keeps the one chosen and the other as a backup.

## Window and layout

- **Game Mode (SteamOS) and Android:** full screen, 16:10 (1280x800) design size scaled to the display; safe margins 5%. Text at least 22 px at 1280x800 (readable at arm's length on a handheld). No window chrome.
- **Desktop (Linux, Windows, macOS):** a normal resizable window, default 960x600, minimum 720x480. Same layout, scaled.
- **Layout (wide):** left, the island card (about 60% width); right or below, the one Play button; along the top or a bottom bar, the four page buttons. **Narrow (portrait Android):** card, Play, then the four buttons stacked.
- **Focus** starts on Play. The focus ring is a 3 px high-contrast outline, never colour alone. Focus order: Play, then the page buttons left to right (Saves, Settings, Fleet, About). D-pad Up/Down moves between Play and the page row; Left/Right moves inside a row.

## Controller map (`Nav`)

`input.rs` turns gilrs events (desktop) and Android key events into one enum so the view models never see a device:

| Nav | Gamepad | Keyboard | Meaning |
|---|---|---|---|
| `Up/Down/Left/Right` | D-pad, left stick (dead zone, repeat after 400 ms then 100 ms) | arrows | move focus |
| `Accept` | A (south), on Switch-layout pads the right button, Enter | Enter, Space | press the focused button |
| `Back` | B (east), Esc | Esc, Backspace | leave a sub-page; on Continue, nothing (never quits by accident) |
| `Menu` | Start | F1 | open Settings from anywhere |
| `Prev/Next` | LB/RB | PgUp/PgDn | switch pages when on a page |
| `Quit` | Steam handles it in Game Mode | Alt+F4, window close | close the launcher (desktop only) |

Button glyphs follow the last device used (Xbox, PlayStation, Steam Deck, keyboard). Accept is always the position of "A" in the design; the glyph shown is the one the user's pad prints there.

## The island card

Shows the **save card** (`savecard::SaveCard`):

- the game's icon (from the game folder, else `assets/acnhrecomp.png`; this is the "picture");
- **when**: the save's file time, as a relative phrase ("Yesterday, 21:14") with the full date and time under it;
- **device**: the device that last played it ("Strix Halo tablet");
- **play time**: "42 h 10 min";
- **version**: the game version that saved it (3.0.2 ...);
- after 1.0, the island's name as the card title; until then the title is the game's name.

A card has a status line under it, one of: *Up to date*, *Newer on another device*, *Newer here*, *Not synced yet*. (See the states below.)

## The one Play button

One large button whose label and action follow the state. In the normal state it reads **Play** and runs `launch::play`: pull the island from save-relay, back it up if it changes, start the player with the device profile, and when the player exits, push. While it runs, the same button area shows the step ("Getting your island", "Starting the game", "Saving your island") with Back disabled except for **Cancel** where cancelling is safe (before the player starts).

The button is always one of these (`PlayAction`): Play, Syncing, Playing, Saving, Retry, Resolve conflict (opens the conflict view), Set up (first run), Prepare (build missing), Update. Only one is ever shown.

## The conflict view

Shown in place of the card when the relay reports both sides changed since the last sync (`launch` returns a conflict with both cards).

- **Two cards side by side** (`savecard::side_by_side`): left "This device", right "On the relay" (named by device). Each shows when, device, play time and version, exactly as in the island card. The newer is marked "Newer" in words, not only colour; the one with more play time is marked "Longer play".
- Under each card, one button: **Keep this one**. Focus starts on neither the card nor a button that destroys data by default: it starts on the card with the *newer* save, but nothing happens until Accept; a second small "Are you sure?" is not used, because the other island is always kept as a backup (`backup`, 10 newest) and the line under the buttons says so.
- **Back** returns to the Continue screen with nothing changed and Play showing "Resolve conflict".
- If the two cards are from different versions, the card of the older version carries the drift warning (below) and picking the newer-version card is the safe default for the game's own rule (it refuses a save from a newer version).
- After the choice, the app syncs with `Keep::This` or `Keep::Relay`, shows "Kept the one from <device>. The other is in Backups.", and returns to Play.

## First run

A guided setup, one question per screen, driven by the controller; Back goes to the previous step, and progress shows "Step 2 of 5". It is skipped when everything is done. Steps, with the state that decides whether each is needed (`FirstRunStep`):

1. **Welcome**: what this is, that the game is not included, one Accept.
2. **Where is the game?** Two branches:
   - *This device has the game folder* (hub devices: Strix Halo tablet, desktop, Mac): pick the folder from a list of found folders (no typing), `gamefolder::check` names the first missing file; the version gate (`version::from_build_id`) shows the build's version or names the four allowed versions.
   - *No game folder* (the AMD handheld, Android, others): find a hub on the home network (mDNS). Shows found hubs; none found shows "No hub found on this network" with Retry and "Enter address". Home network only (`hub::is_home_network`).
3. **Pair** (no-game-folder path only): a six-digit code shown on both screens; the user confirms they match with Accept on both. A wrong or expired code shows "Codes did not match" with Retry.
4. **Get the build**: prepare (on a hub) or pull (elsewhere), with the stage list (tools, game, version, recompile, shaders, fonts, build) as a checklist and a progress bar; long, so it can be left running, and Cancel keeps finished stages. Failure shows the stage that failed and a short redacted reason, with Retry and "Copy log".
5. **Device and Steam**: show the detected device profile and let it be changed; on Game Mode devices, "Add to Steam" writes the shortcut with artwork (Steam must be closed: if open, say so and offer "Try again" and "Skip"). Then **Done** goes to Continue with Play focused.

## Saves, Settings, Fleet, About

Each is one button from Continue, opens as a page with Back returning to Continue, Prev/Next moving between them.

- **Saves:** the backups list (`backup::list`, newest first, with stamp, size), **Restore** (always backs the current island up first), the relay status, and "Sync now". Empty list shows "No backups yet".
- **Settings:** device profile (resolution, frame cap, native 60 where the version has it), the relay on or off and its address, "Add to Steam", data folder, hub choices. Changes are written to `config.ini` / `launcher.ini` on leaving a row, with the file's own comments kept (`config::Config`).
- **Fleet:** one row per paired device (`fleet::DeviceRecord`: device, OS, version, build, last sync, free space), a warning mark on any device with drift, "Pair a device", and for a hub "Share this build". Empty shows "Only this device so far".
- **About:** project and upstream credit, licence and Slint attribution (ADR 0003), the list of fixes offered upstream with links (M8), version and build id of the launcher, a link to the repository.

## Pre-play warnings

Before Play starts the game, in place on the Continue screen (not a modal):

- **Drift** (`fleet::drift`, `fleet::warning`): "This device is on 3.0.2; the latest save was made on 3.0.3 on the desktop." Buttons: **Play anyway** (A), **Update** (when a build can be pulled), **Back**. Play anyway is not the default focus when the game would refuse the save (save from a newer version): there, **Update** has focus and Play anyway is absent.
- **Low free space** during prepare or pull.
- **Battery low** on handhelds is advisory only.

## States the view models must hold

Each is a variant of a plain enum or field, constructible from fake saves, fake relay, fake hub and fake game, and asserted in view-model tests. This list is the contract for `app/src/vm/`.

### `continue_page`

`ContinueState` (exactly one at a time):

| State | Card shown | Play button | Notes |
|---|---|---|---|
| `NeedsSetup` | none | Set up | first run not finished: no install, no game folder, no hub |
| `NoIsland` | empty card "No island yet" | Play | a new island starts when the game does; relay may still have one |
| `Ready` | island card | Play | in sync |
| `RelayNewer` | local card, relay card summary | Play (pulls first) | status "Newer on another device" |
| `LocalNewer` | island card | Play (pushes after) | status "Newer here" |
| `Conflict` | two cards | Resolve conflict | both changed |
| `Drift { newer_save_refused: bool }` | island card + warning | Play anyway / Update | see Pre-play warnings |
| `BuildMissing` | island card | Prepare (hub) or Pull (other) | build not installed or bundle fails `verify` |
| `RelayUnavailable` | last known card | Play (offline) | no network or relay off: plays on the local island, says it will sync later |
| `RelayError(reason)` | last known card | Retry | auth or server failure, with redacted reason |
| `Syncing(step)` | island card | disabled, shows step | step: Pull, Backup, Push |
| `Starting` | island card | disabled | player launching |
| `Playing` | island card | disabled | player running (desktop window stays; Game Mode shows the game) |
| `Saving` | island card | disabled | push after exit |
| `PlayerFailed { exit, log_hint }` | island card | Retry | the player exited with an error or crashed; the island is untouched and the next push is skipped if no save changed |
| `PushFailed(reason)` | island card, marked "Not synced" | Retry sync | played, but the push failed; the local island is kept and backed up |
| `Updating(stage)` | island card | disabled | back up, rebuild, check island still there (`update`) |
| `UpdateFailed(reason)` | island card | Retry | island verified intact |

Also held: the `SaveCard` (local), the optional relay `SaveCard`, the `PlayAction`, the focus target, the glyph set, and the last-error text.

### `conflict` (part of `continue_page`)

`ConflictView { this_card, relay_card, newer: Side, longer_play: Side, versions_differ: bool, selected: Option<Side>, outcome: None | Resolving | Kept(Side) | Failed(reason) }`

### `first_run`

`FirstRunStep`: `Welcome`, `FindGame`, `PickFolder`, `FolderRejected(reason)` (missing file or `version` gate message), `FindHub`, `NoHubFound`, `Pair { code, status: Waiting | Matched | Mismatch | Expired }`, `GetBuild { stages: [(Stage, Pending|Running|Done|Failed(reason)|Skipped)], progress }`, `DeviceAndSteam { profile, steam: NotApplicable | Ready | SteamRunning | Added | Failed(reason) }`, `Done`. Also: the step number and total, whether Back is allowed, and whether the flow was resumed from a partial state (each prepare stage is skipped when its output exists, so a restart resumes).

### `saves`

`SavesState { backups: Vec<BackupRow>, selected, restore: Idle | Confirming | Restoring | Done | Failed(reason), relay: Off | Ok(last_sync) | Unavailable | Error(reason), sync: Idle | Running | Done | Conflict }`. `Empty` is `backups` empty.

### `settings`

`SettingsState { profile: DeviceProfile, profile_is_detected: bool, native60: Unavailable | Off | On, relay: { enabled, address }, steam: NotApplicable | NotAdded | Added | NeedsSteamClosed | Failed, data_dir, rows (focus), dirty, save_error }`.

### `fleet`

`FleetState { devices: Vec<{ record, drift: Option<Drift>, is_this_device, reachable }>, hub_role: None | Serving | Pairing{code}, pairing: Idle | Searching | Code | Paired | Failed, empty }`.

### `about`

`AboutState { version, build_id, credits, upstream_fixes: Vec<{ title, link, merged_upstream: bool }>, licences }`.

### Cross-cutting

`Nav` (above), `InputDevice` (gamepad kind, keyboard, touch) for glyphs, `WindowMode` (`FullScreen` | `Window`), `Layout` (`Wide` | `Narrow`), `Page` (`Continue`, `Saves`, `Settings`, `Fleet`, `About`, `FirstRun`), and a toast/error line (one at a time, text only, cleared by Accept).

## Not in this brief

- The in-game overlay (`runtime/src/ui/`), which stays C++.
- iOS: the game app is the launcher there (ADR 0003).
- The island's name: after 1.0.

## Unknowns to settle before the app pages (2.03 onward)

1. **Relay conflict detection:** how `launch` learns that both sides changed (relay manifest times against the last-synced stamp) depends on iannitram/save-relay#49. The `Conflict` and `RelayNewer`/`LocalNewer` states hold either way; only the source of truth changes.
2. **Offline play policy:** `RelayUnavailable` plays on the local island and syncs later. If both devices play offline, the next sync is a conflict, which the conflict view handles. Confirm this is wanted rather than refusing to play offline.
3. **Slint on-screen digit pad** for the hub address on a pad-only device: whether Slint's focus handling is enough or `app/` needs its own pad.
4. **Gamepad access in Game Mode:** Steam's own input layer may hide the pad from gilrs. The shortcut may need a controller template that sends plain pad events; to be checked on the AMD handheld.
5. **Android:** key events for the D-pad and A/B on common controllers; the touch layout of the Narrow form.
6. **Play time and the card picture** are read from `launcher-card.txt` (`savecard::read_card`); whether the game can write them or the launcher does after exit is open (code-map `savecard`).
7. **"Play anyway" on drift:** whether the older-version warning should ever allow play when the game would refuse; this brief says no for a save from a newer version.

## Done when

The owner approves this brief in the PR, and every state above exists as a view-model value in the pieces that follow (M10).
