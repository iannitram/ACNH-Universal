# The Continue screen: design brief

Status: approved as written by the owner on 2026-11-18 ("Approve as written (Recommended)"), as amended for the relay states and the owner's choices of 2026-11-18 (SPEC.md M6, M10); the approval is recorded in PR #176: https://github.com/iannitram/ACNH-Universal/pull/176#issuecomment-6985705361. Shared with totk-universal: the layout, states and input rules are the same; only the game's icon, name and version list differ. Words follow `GLOSSARY.md`. The view models live in `app/src/vm/`, input in `app/src/input.rs`, pages in `app/ui/` (ADR 0003, `docs/code-map.md#faces`). `.slint` files only draw; every state below is a plain Rust value in a view model, so each is tested with fake saves and no window.

## Principles

1. **One press to play.** From Game Mode, Accept (the south face button, A on the handhelds) on the opened launcher syncs and starts the game.
2. **Controller first, mouse works too.** Every action is reachable with D-pad/stick, Accept, Back, and Menu. Touch and mouse click the same targets. No hover-only information; no text entry that needs a keyboard (pairing uses a code shown, not typed, where possible; address entry uses an on-screen pad, and the relay's server, user and app password use the on-screen keyboard in *Text entry*).
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
| `Accept` | south face button: Xbox/Steam Deck A, PlayStation Cross, Switch-layout B | Enter, Space | press the focused button |
| `Back` | east face button: Xbox/Steam Deck B, PlayStation Circle, Switch-layout A | Esc, Backspace (outside text entry) | leave a sub-page or close the conflict chooser; on ordinary Continue, nothing (never quits by accident) |
| `Menu` | Start | F1 | open Settings from anywhere |
| `Prev/Next` | LB/RB | PgUp/PgDn | switch pages when on a page |
| `Quit` | Steam handles it in Game Mode | Alt+F4, window close | close the launcher (desktop only) |

Navigation uses physical position on every controller: south accepts and east goes back, including Switch-layout pads. Button glyphs follow the last device used (Xbox, PlayStation, Steam Deck, Switch-layout, keyboard, a `Generic` glyph set, or touch) and show the label printed at that position. Changing glyphs never swaps the actions. Any reference to A for one-press play means the south button on the handhelds. In a text field, keyboard Space enters a space and Backspace deletes; Enter submits and Esc goes back. A controller's Accept activates the focused on-screen key or button, and Back leaves the editor.

**Glyph sets.** The `Generic` glyph set is for a pad the launcher cannot name: its buttons are drawn by position (south, east, north, west, shoulders, sticks, Start) with no printed letter, so the prompt never claims a label the pad may not have. Touch draws the same targets for a finger. The set follows a pad connected or disconnected while the launcher is open, on every page; only the pictures change, never the actions. 2.04b builds the sets, and 9.01a builds them on Android.

Text entry: an on-screen keyboard in the launcher, driven by the controller, with the address pad's focus rules (see *Controller map* and *Manual hub address*); the owner chose it on 2026-11-18. Every key is reachable with the D-pad and Accept, Back leaves the editor, and keyboard and touch edit the same text. The relay's server address, user name and app password are typed with this keyboard only. save-relay stays as it is: Nextcloud Login Flow v2 is not used, and no save-relay change is filed for it. This is the one allowed exception to rule 2.

Offline: while save sync is set up and save-relay cannot be reached, Play is refused, so a device never runs on an older save. `RelayUnavailable` says plainly that save-relay could not be reached and that the game starts once the island has synced; its button is Retry, and there is no Play anyway. Save sync that is off (Skip in the first run, or Turn off save sync) is not offline: `SyncOff` lets Play play without sync, and its card says that save sync is off and where to turn it on. A push still waiting from the last game is sent when the launcher opens, before anything else syncs, and Play stays refused until that push and the pull succeed (2.06a).

## The island card

Shows the **save card** (`savecard::SaveCard`):

- the game's icon (from the game folder, else `assets/acnhrecomp.png`; this is the "picture");
- **when**: the save's file time, as a relative phrase ("Yesterday, 21:14") with the full date and time under it;
- **device**: the device that last played it ("Strix Halo tablet");
- **play time**: "42 h 10 min";
- **version**: the game version that saved it (3.0.2 ...);
- after 1.0, the island's name as the card title; until then the title is the game's name.

A card has a status line under it, one of: *Up to date*, *Newer on another device*, *Newer here*, *Not synced yet*, *Open on another device*. (See the states below.)

## The one Play button

One large button whose label and action follow the state. In the normal state it reads **Play** and runs `launch::play`: pull the island from save-relay, back it up if it changes, start the player with the device profile, and when the player exits, push. While it runs, the same button area shows the step ("Getting your island", "Starting the game", "Saving your island") with Back disabled except for **Cancel** where cancelling is safe (before the player starts).

Outside the open conflict chooser, the button is always one of these (`PlayAction`): Play, Syncing, Playing, Saving, Retry, Wait (re-check a held island), Play anyway (override a hold), Resolve conflict (opens the conflict view), Set up (first run), Prepare (build missing), Update. Only one is ever shown. While the chooser is open, `PlayAction` is absent and `ConflictPhase` owns its Keep/Retry controls and progress instead.

## The conflict view

When the relay reports both sides changed since the last sync (`launch` returns a conflict with both cards), Continue enters `Conflict` with `ConflictPhase::Pending`: the local card, a relay summary and an unresolved-conflict warning. The one Play button reads **Resolve conflict**, with focus on it; it cannot launch the player. Accept opens `ConflictPhase::Choosing` in the same card area. The chooser replaces the Play button with the two Keep buttons; there is no additional Play action while it is open.

- **Two cards side by side** (`savecard::side_by_side`): left "This device", right "On the relay" (named by device). Each shows when, device, play time and version, exactly as in the island card. The newer is marked "Newer" in words, not only colour; the one with more play time is marked "Longer play".
- Under each card, one button: **Keep this one**. Focus starts on the newer card's Keep button (This device on a time tie); Left/Right switches sides. Opening the chooser never selects a save. Only a fresh Accept on a Keep button commits the choice; the press that opened the chooser is consumed. A second small "Are you sure?" is not used, because the other island is always kept as a backup (`backup`, 10 newest) and the line under the buttons says so.
- **Back** from the chooser returns to `Pending` with both cards retained, nothing changed and focus on **Resolve conflict**. Back while already pending does nothing. Opening it again shows the same two cards and resets focus to the newer side.
- If the two cards are from different versions, each shows its version and any drift from the installed build (below). The initial focus rule stays the same; it never chooses a save automatically. Resolving the conflict does not bypass the pre-play warning: a save from a newer version requires Update before play.
- After the choice, `Resolving { selected }` backs up before any overwrite and syncs with `Keep::This` or `Keep::Relay`. Both cards remain visible, Keep buttons and navigation are disabled while the operation runs. Success returns to `Ready` with Play focused and the message "Kept the one from <device>. The other is in Backups." A backup or sync failure enters `Failed { selected, reason }`, keeps both cards and shows the redacted reason with Retry for that choice and Back to `Pending`; it never starts the player or discards an island without its backup.

## The held island

save-relay reports `Held` in `api::Held` when the game is open on another device (device, since). The adapter currently turns that into a conflict (`crates/core/src/relay.rs:158`); 1.26b maps it to `Held { device, since }` instead. The card area shows the local island card and the line "Open on <device> since <time>"; the status line reads *Open on another device*.

- **Wait** (retry) re-checks the hold and is the default focus. If it has cleared, Continue returns to `Ready` or the newer-side state.
- **Play anyway** is the only path that overrides the hold. Like the conflict chooser, it needs a fresh Accept (the press that revealed the state is consumed); it backs the local island up first, then starts the player here and leaves the other device's session to sync later.
- **Back** leaves the held view without playing and without writing anything.

## First run

A guided setup, one question per screen, driven by the controller; Back goes to the previous step, and progress shows "Step 2 of 6". It is skipped when everything is done. Steps, with the state that decides whether each is needed (`FirstRunStep`):

1. **Welcome**: what this is, that the game is not included, one Accept.
2. **Where is the game?** Two branches:
   - *This device has the game folder* (hub devices: Strix Halo tablet, desktop, Mac): pick the folder from a list of found folders (no typing), `gamefolder::check` names the first missing file; the version gate (`version::from_build_id`) shows the build's version or names the four allowed versions.
   - *No game folder* (the AMD handheld, Android, others): find a hub on the home network (mDNS). Shows found hubs and **Enter address**; none found shows "No hub found on this network" with Retry and **Enter address**. Home network only (`hub::is_home_network`).
3. **Pair** (no-game-folder path only): a six-digit code shown on both screens; the user confirms they match with Accept on both. A wrong or expired code shows "Codes did not match" with Retry.
4. **Get the build**: prepare (on a hub) or pull (elsewhere), with the stage list (tools, game, version, recompile, shaders, fonts, build) as a checklist and a progress bar; long, so it can be left running, and Cancel keeps finished stages. Failure shows the stage that failed and a short redacted reason, with Retry and "Copy log".
5. **Save relay** (`RelaySetup`): sign in so the island can sync, or skip. It offers:
   - *Nextcloud*: the server address, user name and app password, typed with the on-screen keyboard (see *Text entry*).
   - *Google Drive* or *Dropbox*: on a desktop the browser opens to sign in; in Game Mode a code and a URL are shown to open on a phone, then a wait that finishes on its own when the sign-in completes.
   - *Skip*: play without sync (`SyncOff`); save sync is off until it is turned on, and the relay can be set up later from Settings.

   Signing in shows progress; a failure shows a redacted reason with Retry, another service and Skip. Back goes to the previous step as everywhere else. The step is skipped when a relay account is already set up.
6. **Device and Steam**: show the detected device profile and let it be changed; on Game Mode devices, "Add to Steam" writes the shortcut with artwork (Steam must be closed: if open, say so and offer "Try again" and "Skip"). Then **Done** goes to Continue with Play focused.

**Manual hub address** is a sub-step of step 2 (`EnterHubAddress`), so it does not add a numbered setup step. It edits an IP literal (IPv4 or IPv6, without brackets) and a separate port field prefilled with the hub service's default. Hostnames and URLs are rejected. The on-screen pad has digits, hexadecimal letters, dot, colon, cursor left/right, delete, field selection, **Connect** and **Cancel**; every key and button has D-pad focus and Accept activation. Keyboard and touch edit the same fields.

Connect enters `Validating`: parse the IP and port (1–65535), then check `hub::is_home_network` before attempting a connection. Empty or malformed fields and non-home-network addresses enter `Invalid { field, reason }`, focus that field and keep the input for correction. A permitted endpoint enters `Connecting { endpoint }`; connection failure enters `Unreachable { endpoint, reason }` with Retry, Edit and Cancel. Retry validates again. Editing clears the previous result. Success records the selected hub and advances to Pair; entering an address never bypasses pairing. Back/Cancel returns to the discovery state that opened the editor without changing the selected hub. During validation or connection, it cancels the attempt and ignores late results. Back from Pair after manual entry restores the editor and its text.

## Saves, Settings, Fleet, About

Each is one button from Continue, opens as a page with Back returning to Continue, Prev/Next moving between them.

- **Saves:** the backups list (`backup::list`, newest first, with stamp, size), **Restore** (always backs the current island up first), the relay's service and account, the last sync, **Sync now**, and a push that is still waiting (shown with the last sync and retried on the next Sync now or Play; a push still waiting is sent when the launcher opens, before anything else syncs, and Play is refused until that push and the pull succeed, 2.06a). Empty list shows "No backups yet".
- **Settings:** device profile (resolution, frame cap, native 60 where the version has it), the save relay (service and account, on or off, the server address when there is one, **Sign in again**, **Turn off save sync**), "Add to Steam", data folder, hub choices. Changes are written to `config.ini` / `launcher.ini` on leaving a row, with the file's own comments kept (`config::Config`).
- **Fleet:** one row per paired device (`fleet::DeviceRecord`: device, OS, version, build, last sync, free space), a warning mark on any device with drift, "Pair a device", and for a hub "Share this build". Empty shows "Only this device so far".
- **About:** project and upstream credit, licence and Slint attribution (ADR 0003), the list of fixes offered upstream with links (M8), version and build id of the launcher, a link to the repository.

## Pre-play warnings

Before Play starts the game, in place on the Continue screen (not a modal):

- **Drift** (`fleet::drift`, `fleet::warning`): "This device is on 3.0.2; the latest save was made on 3.0.3 on the desktop." Buttons: **Play anyway** (Accept), **Update** (when a build can be pulled), **Back**. Play anyway is not the default focus when the game would refuse the save (save from a newer version): there, **Update** has focus and Play anyway is absent.
- **Low free space** during prepare or pull.
- **Battery low** on handhelds is advisory only.

## States the view models must hold

Each is a variant of a plain enum or field, constructible from fake saves, fake relay, fake hub and fake game, and asserted in view-model tests. This list is the contract for `app/src/vm/`.

### `continue_page`

`ContinueState` (exactly one at a time):

| State | Card shown | Play button | Notes |
|---|---|---|---|
| `NoIsland` | empty card "No island yet" | Play | a new island starts when the game does; relay may still have one |
| `Ready` | island card | Play | in sync |
| `RelayNewer` | local card, relay card summary | Play (pulls first) | status "Newer on another device" |
| `LocalNewer` | island card | Play (pushes after) | status "Newer here" |
| `Conflict` | local card + relay summary in `Pending`; two cards in the chooser | Resolve conflict in `Pending`; Keep buttons in `Choosing`; disabled in `Resolving`; Retry in `Failed` | `ConflictView.phase` distinguishes the unresolved warning from the open chooser; no launch until resolved |
| `Held { device, since }` | island card + hold summary | Wait (retry) / Play anyway / Back | save-relay says the game is open on another device; only Play anyway overrides the hold |
| `Drift { newer_save_refused: bool }` | island card + warning | Play anyway / Update | see Pre-play warnings |
| `BuildMissing` | island card | Prepare (hub) or Pull (other) | build not installed or bundle fails `verify` |
| `RelayUnavailable` | last known card | Retry | save-relay could not be reached while save sync is set up; says the game starts once the island has synced, and there is no Play anyway |
| `SyncOff` | island card, status "Save sync is off" | Play | save sync is off (Skip or Turn off save sync): plays without sync, and the card says so and where to turn it on |
| `RelayError(reason)` | last known card | Retry | auth or server failure, with redacted reason |
| `Syncing(step)` | island card | disabled, shows step | step: Pull, Backup, Push |
| `Playing` | island card | disabled | player running (desktop window stays; Game Mode shows the game) |
| `Saving` | island card | disabled | push after exit |
| `PlayerFailed { exit, log_hint }` | island card | Retry | the player exited with an error or crashed; the island is untouched and the next push is skipped if no save changed |
| `PushFailed(reason)` | island card, marked "Not synced" | Retry sync | played, but the push failed; the local island is kept and backed up |
| `Updating(stage)` | island card | disabled | back up, rebuild, check island still there (`update`) |
| `UpdateFailed(reason)` | island card | Retry | island verified intact |

Also held: the `SaveCard` (local), the optional relay `SaveCard`, `Option<PlayAction>` (absent while the conflict chooser is open), the focus target, the glyph set, and the last-error text.

### `conflict` (part of `continue_page`)

`ConflictView { this_card, relay_card, newer: Side, longer_play: Side, versions_differ: bool, phase: ConflictPhase }`, where `Side` is `This | Relay` and `ConflictPhase` is `Pending | Choosing { focused: Side } | Resolving { selected: Side } | Failed { selected: Side, reason }`. Both cards are retained in every phase; focus and a committed choice are distinct values. `Pending` opens `Choosing` on Accept and `Choosing` closes to `Pending` on Back without a write. Only accepting a Keep button enters `Resolving`; success clears the conflict and enters `Ready` with the outcome message, failure retains it in `Failed`. Retry re-enters `Resolving` for the selected side; Back from `Failed` returns to `Pending`.

### `held` (part of `continue_page`)

`Held { device, since }` comes from save-relay's `api::Held`; the adapter maps it there in 1.26b, not to `Sync::Conflict` (`crates/core/src/relay.rs:158`). The view holds the device, the since time, the local `SaveCard`, and `HeldPhase` (`Waiting | Overriding | Failed(reason)`). Wait re-checks and leaves on a clear; Play anyway enters `Overriding`, which backs up and starts the player, and is the only path that overrides the hold. A backup failure enters `Failed` with the redacted reason and Retry. Back returns to Continue with nothing written.

### `first_run`

`FirstRunStep`: `Welcome`, `FindGame`, `PickFolder`, `FolderRejected(reason)` (missing file or `version` gate message), `FindHub`, `NoHubFound`, `EnterHubAddress(HubAddressEntry)`, `Pair { code, status: Waiting | Matched | Mismatch | Expired }`, `GetBuild { stages: [(Stage, Pending|Running|Done|Failed(reason)|Skipped)], progress }`, `RelaySetup { service: None | Nextcloud | GoogleDrive | Dropbox, phase: Choose | EditingField(Field) | SigningIn | WaitingForBrowser { code, url } | Failed(reason) | Skipped }`, `DeviceAndSteam { profile, steam: NotApplicable | Ready | SteamRunning | Added | Failed(reason) }`, `Done`. `RelaySetup` is skipped when an account is already set up; `None` and `Skipped` both mean save sync is off (`SyncOff`) and can be revisited from Settings. Also: the step number and total (six), whether Back is allowed, and whether the flow was resumed from a partial state (each prepare stage is skipped when its output exists, so a restart resumes).

`HubAddressEntry { host_text, port_text, active_field: Host | Port, caret, focus: Field(Host | Port) | PadKey(AddressKey) | Connect | Retry | Edit | Cancel, return_to: FindHub | NoHubFound, status: Editing | Validating | Invalid { field, reason } | Connecting { endpoint } | Unreachable { endpoint, reason } }`. `AddressKey` identifies a digit, hexadecimal letter, dot, colon, cursor-left, cursor-right, delete or field-selection key. Empty input is `Editing` with empty text; errors retain the text and focus so it can be corrected with the controller. `endpoint` is a parsed IP and port that passed the home-network check. The first-run model also retains the editor while on Pair, the selected hub, and an attempt id so cancelled or superseded validation/connection results cannot advance setup. No persistent hub setting changes until pairing succeeds.

### `saves`

`SavesState { backups: Vec<BackupRow>, selected, restore: Idle | Confirming | Restoring | Done | Failed(reason), relay: Off | Ok(last_sync) | PendingPush(last_sync) | Unavailable | Error(reason), sync: Idle | Running | Done | Conflict }`. `Empty` is `backups` empty. `PendingPush` is a push that has not reached the relay yet: it shows the last sync, is retried on the next **Sync now** or Play, and becomes `Ok` or `Error` when it lands.

### `settings`

`SettingsState { profile: DeviceProfile, profile_is_detected: bool, native60: Unavailable | Off | On, relay: { enabled, service: None | Nextcloud | GoogleDrive | Dropbox, account, address, sign_in: Idle | SigningIn | Failed(reason) }, steam: NotApplicable | NotAdded | Added | NeedsSteamClosed | Failed, data_dir, rows (focus), dirty, save_error }`. The relay row shows the service and account, not only the address; its actions are **Sign in again** (`sign_in` re-enters `SigningIn`) and **Turn off save sync** (a confirm that sets `enabled` false and keeps the account, so Sign in again is one press).

### `fleet`

`FleetState { devices: Vec<{ record, drift: Option<Drift>, is_this_device, reachable }>, hub_role: None | Serving | Pairing{code}, pairing: Idle | Searching | Code | Paired | Failed, empty }`.

### `about`

`AboutState { version, build_id, credits, upstream_fixes: Vec<{ title, link, merged_upstream: bool }>, licences }`.

### Cross-cutting

`Nav` (above), `InputDevice` (gamepad kind, keyboard, touch) for glyphs, `WindowMode` (`FullScreen` | `Window`), `Layout` (`Wide` | `Narrow`), `Page` (`Continue`, `Saves`, `Settings`, `Fleet`, `About`, `FirstRun`), and a toast/error line (one at a time, text only, cleared by Accept).

## Not in this brief

- The in-game overlay (`runtime/src/ui/`), which stays C++.
- iOS: the iPad's `.ipa` is the one contained app (the launcher's 1.0 pieces, the player and the prepared game), and the same 1.0 pieces run on every platform. The iPad's app shows the same relay states (`RelaySetup`, `Conflict` with both cards, `Held`, and offline refused with Retry: the player does not boot the game until its pull has succeeded), driven by the controller with prompts for the pad in hand. 8.05a builds them there as prompts in the game app; Google Drive and Dropbox sign-in there stay #76's, and #75 and #76 are the device runs (ADR 0003).
- The island's name: after 1.0.

## Unknowns to settle before the app pages (2.03 onward)

1. **Relay conflict detection:** how `launch` learns that both sides changed (relay manifest times against the last-synced stamp). The `Conflict` and `RelayNewer`/`LocalNewer` states hold either way; the source of truth is save-relay's `sync::status` before Play. Settled: save-relay#49 is closed, and `RelayNewer`/`LocalNewer` come from `sync::status` (1.26b).
2. **Offline play policy:** while save sync is set up and save-relay cannot be reached, Play is refused so a device never runs on an older save. `RelayUnavailable` says that save-relay could not be reached and that the game starts once the island has synced, with Retry and no Play anyway; save sync that is off is `SyncOff` and plays without sync. Settled: refuse until synced (the owner's choice on 2026-11-18); never play an older local save.
3. **Slint on-screen address pad:** whether Slint's focus handling is enough or `app/` needs its own pad. The controller-reachable keys, editing/validation states and transitions above are required either way.
4. **Gamepad access in Game Mode:** Steam's own input layer may hide the pad from gilrs. The shortcut may need a controller template that sends plain pad events; the AMD handheld run checks it. Settled: checked by 2.04b, then the device run #68.
5. **Android:** key events for the D-pad and south/east face buttons on common controllers; the touch layout of the Narrow form. The physical Accept/Back convention above is fixed.
6. **Play time and the card picture** are read from `launcher-card.txt` (`savecard::read_card`). Settled: by 1.26b; save-relay writes the card on each push and the launcher writes `launcher-card.txt` from it (code-map `savecard`).
7. **"Play anyway" on drift:** whether the older-version warning should ever allow play when the game would refuse; this brief says no for a save from a newer version.

## Done when

For #36 (2.02), this brief names every state the view models must hold and the owner approved it as written on 2026-11-18 ("Approve as written (Recommended)"), recorded in PR #176: https://github.com/iannitram/ACNH-Universal/pull/176#issuecomment-6985705361. The later app pieces implement those values and test their transitions with fake saves, relay, hub and game (M10), including manual address correction/retry/cancel, pending conflict → chooser → Back without writes, the held island's Wait/Play anyway/Back, offline refused with Retry and `SyncOff` playing without sync, the on-screen keyboard for the relay fields, and south/east navigation with matching glyphs on each controller layout and the `Generic` set.
