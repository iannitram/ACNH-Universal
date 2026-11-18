//! The Continue page view model, island card, conflict chooser, and drift warnings (SPEC.md M10, M11).

use std::path::Path;

use acnh_core::fleet::{self, DeviceRecord, Drift};
use acnh_core::launch::{self, Game, Keep, Played, Relay, Sync};
use acnh_core::savecard::SaveCard;
use acnh_core::{Error, Result};

use crate::vm::Nav;

/// Current phase of save-relay synchronization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStep {
    Pull,
    Backup,
    Push,
}

impl SyncStep {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pull => "Getting your island",
            Self::Backup => "Backing up your island",
            Self::Push => "Saving your island",
        }
    }
}

/// The mutually exclusive states the Continue page can be in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinueState {
    NeedsSetup,
    NoIsland,
    Ready,
    RelayNewer,
    LocalNewer,
    Conflict,
    Drift { newer_save_refused: bool },
    BuildMissing,
    RelayUnavailable,
    RelayError(String),
    Syncing(SyncStep),
    Starting,
    Playing,
    Saving,
    PlayerFailed { exit: i32, log_hint: String },
    PushFailed(String),
    Updating(String),
    UpdateFailed(String),
}

/// The single action shown on the main action button outside the conflict chooser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayAction {
    Play,
    Syncing,
    Playing,
    Saving,
    Retry,
    ResolveConflict,
    SetUp,
    Prepare,
    Update,
}

impl PlayAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Play => "Play",
            Self::Syncing => "Syncing",
            Self::Playing => "Playing",
            Self::Saving => "Saving",
            Self::Retry => "Retry",
            Self::ResolveConflict => "Resolve conflict",
            Self::SetUp => "Set up",
            Self::Prepare => "Prepare",
            Self::Update => "Update",
        }
    }

    pub fn is_disabled(self) -> bool {
        matches!(self, Self::Syncing | Self::Playing | Self::Saving)
    }
}

/// Side representing a save in the conflict view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    This,
    Relay,
}

/// Phases of conflict resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictPhase {
    Pending,
    Choosing { focused: Side },
    Resolving { selected: Side },
    Failed { selected: Side, reason: String },
}

/// Full view state for an unresolved or resolving conflict between two cards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictView {
    pub this_card: SaveCard,
    pub relay_card: SaveCard,
    pub newer: Side,
    pub longer_play: Side,
    pub versions_differ: bool,
    pub phase: ConflictPhase,
}

impl ConflictView {
    pub fn new(this_card: SaveCard, relay_card: SaveCard) -> Self {
        let newer = if relay_card.saved_unix > this_card.saved_unix {
            Side::Relay
        } else {
            Side::This
        };
        let longer_play = if relay_card.play_seconds > this_card.play_seconds {
            Side::Relay
        } else {
            Side::This
        };
        let versions_differ = this_card.version != relay_card.version;
        Self {
            this_card,
            relay_card,
            newer,
            longer_play,
            versions_differ,
            phase: ConflictPhase::Pending,
        }
    }

    pub fn side_card(&self, side: Side) -> &SaveCard {
        match side {
            Side::This => &self.this_card,
            Side::Relay => &self.relay_card,
        }
    }
}

/// Current navigation focus target on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    Play,
    Keep(Side),
    Update,
    Back,
    Saves,
    Settings,
    Fleet,
    About,
}

/// Controller / input layout for glyph display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputDevice {
    Xbox,
    PlayStation,
    SteamDeck,
    Switch,
    Keyboard,
    Touch,
}

/// View-model for the Continue screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuePageVm {
    pub state: ContinueState,
    pub local_card: Option<SaveCard>,
    pub relay_card: Option<SaveCard>,
    pub play_action: Option<PlayAction>,
    pub conflict: Option<ConflictView>,
    pub focus: FocusTarget,
    pub glyph_set: InputDevice,
    pub last_error: Option<String>,
    pub status_message: Option<String>,
    pub warning_text: Option<String>,
    pub this_device: Option<DeviceRecord>,
    pub saved_by: Option<DeviceRecord>,
    pub auto_resolve: bool,
}

impl Default for ContinuePageVm {
    fn default() -> Self {
        Self::new(None, None)
    }
}

impl ContinuePageVm {
    pub fn new(local_card: Option<SaveCard>, this_device: Option<DeviceRecord>) -> Self {
        let state = if local_card.is_some() {
            ContinueState::Ready
        } else {
            ContinueState::NoIsland
        };
        Self {
            state,
            local_card,
            relay_card: None,
            play_action: Some(PlayAction::Play),
            conflict: None,
            focus: FocusTarget::Play,
            glyph_set: InputDevice::SteamDeck,
            last_error: None,
            status_message: None,
            warning_text: None,
            this_device,
            saved_by: None,
            auto_resolve: true,
        }
    }

    pub fn state(&self) -> &ContinueState {
        &self.state
    }

    pub fn local_card(&self) -> Option<&SaveCard> {
        self.local_card.as_ref()
    }

    pub fn relay_card(&self) -> Option<&SaveCard> {
        self.relay_card.as_ref()
    }

    pub fn play_action(&self) -> Option<PlayAction> {
        self.play_action
    }

    pub fn conflict(&self) -> Option<&ConflictView> {
        self.conflict.as_ref()
    }

    pub fn focus(&self) -> FocusTarget {
        self.focus
    }

    pub fn glyph_set(&self) -> InputDevice {
        self.glyph_set
    }

    pub fn status_message(&self) -> Option<&str> {
        self.status_message.as_deref()
    }

    pub fn warning_text(&self) -> Option<&str> {
        self.warning_text.as_deref()
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn card_status(&self) -> &'static str {
        match self.state {
            ContinueState::Ready => "Up to date",
            ContinueState::RelayNewer => "Newer on another device",
            ContinueState::LocalNewer => "Newer here",
            ContinueState::PushFailed(_) => "Not synced yet",
            ContinueState::NoIsland => "No island yet",
            ContinueState::RelayUnavailable => "Will sync later",
            ContinueState::Conflict => "Unresolved conflict",
            _ => "",
        }
    }

    /// Set an unresolved conflict with both cards.
    pub fn set_conflict(&mut self, this_card: SaveCard, relay_card: SaveCard) {
        let conflict = ConflictView::new(this_card, relay_card);
        self.conflict = Some(conflict);
        self.state = ContinueState::Conflict;
        self.play_action = Some(PlayAction::ResolveConflict);
        self.focus = FocusTarget::Play;
    }

    /// Set the device record that made the latest save and re-evaluate drift.
    pub fn set_saved_by(&mut self, saved_by: DeviceRecord) {
        self.saved_by = Some(saved_by);
        let (drift, warning, newer_refused) = self.check_drift();
        if drift != Drift::None {
            self.warning_text = warning;
            self.state = ContinueState::Drift {
                newer_save_refused: newer_refused,
            };
            if newer_refused {
                self.play_action = Some(PlayAction::Update);
                self.focus = FocusTarget::Update;
            } else {
                self.play_action = Some(PlayAction::Play);
                self.focus = FocusTarget::Play;
            }
        }
    }

    /// Check if there is drift between this device and the latest save's creator.
    pub fn check_drift(&self) -> (Drift, Option<String>, bool) {
        if let (Some(here), Some(by)) = (&self.this_device, &self.saved_by) {
            let drift = fleet::drift(here, by);
            if drift != Drift::None {
                let warning = fleet::warning(&drift, by);
                let newer_refused = match &drift {
                    Drift::Version { theirs, ours } => is_version_newer(theirs, ours),
                    _ => false,
                };
                return (drift, warning, newer_refused);
            }
        }
        (Drift::None, None, false)
    }

    /// Handle controller or keyboard navigation input. Returns true if an action was committed.
    pub fn handle(&mut self, nav: Nav) -> bool {
        match nav {
            Nav::Quit => false,
            Nav::Menu => false,
            Nav::Prev | Nav::Next => false,
            _ => self.handle_screen_nav(nav),
        }
    }

    fn handle_screen_nav(&mut self, nav: Nav) -> bool {
        if let Some(conflict) = &mut self.conflict {
            match conflict.phase {
                ConflictPhase::Pending => {
                    match nav {
                        Nav::Accept | Nav::Activate => {
                            let focused = conflict.newer;
                            conflict.phase = ConflictPhase::Choosing { focused };
                            self.play_action = None;
                            self.focus = FocusTarget::Keep(focused);
                        }
                        Nav::Down => {
                            self.focus = FocusTarget::Saves;
                        }
                        _ => {}
                    }
                    return false;
                }
                ConflictPhase::Choosing { focused } => {
                    match nav {
                        Nav::Left => {
                            conflict.phase = ConflictPhase::Choosing {
                                focused: Side::This,
                            };
                            self.focus = FocusTarget::Keep(Side::This);
                        }
                        Nav::Right => {
                            conflict.phase = ConflictPhase::Choosing {
                                focused: Side::Relay,
                            };
                            self.focus = FocusTarget::Keep(Side::Relay);
                        }
                        Nav::Back => {
                            conflict.phase = ConflictPhase::Pending;
                            self.play_action = Some(PlayAction::ResolveConflict);
                            self.focus = FocusTarget::Play;
                        }
                        Nav::Accept | Nav::Activate => {
                            let selected = focused;
                            conflict.phase = ConflictPhase::Resolving { selected };
                            if self.auto_resolve {
                                self.finish_resolving(Ok(()));
                            }
                            return true;
                        }
                        _ => {}
                    }
                    return false;
                }
                ConflictPhase::Resolving { .. } => return false,
                ConflictPhase::Failed { selected, .. } => {
                    match nav {
                        Nav::Accept | Nav::Activate => {
                            conflict.phase = ConflictPhase::Resolving { selected };
                            if self.auto_resolve {
                                self.finish_resolving(Ok(()));
                            }
                            return true;
                        }
                        Nav::Back => {
                            conflict.phase = ConflictPhase::Pending;
                            self.play_action = Some(PlayAction::ResolveConflict);
                            self.focus = FocusTarget::Play;
                        }
                        _ => {}
                    }
                    return false;
                }
            }
        }

        if let ContinueState::Drift {
            newer_save_refused, ..
        } = self.state
        {
            match nav {
                Nav::Back => {
                    self.state = ContinueState::Ready;
                    self.warning_text = None;
                    self.play_action = Some(PlayAction::Play);
                    self.focus = FocusTarget::Play;
                    return false;
                }
                Nav::Accept | Nav::Activate => {
                    if newer_save_refused {
                        self.state = ContinueState::Updating("Starting update".to_string());
                        self.play_action = Some(PlayAction::Syncing);
                        return true;
                    } else if self.focus == FocusTarget::Play {
                        self.state = ContinueState::Syncing(SyncStep::Pull);
                        self.play_action = Some(PlayAction::Syncing);
                        return true;
                    }
                }
                Nav::Right | Nav::Down
                    if !newer_save_refused && self.focus == FocusTarget::Play =>
                {
                    self.focus = FocusTarget::Update;
                }
                Nav::Left | Nav::Up if !newer_save_refused && self.focus == FocusTarget::Update => {
                    self.focus = FocusTarget::Play;
                }
                _ => {}
            }
            return false;
        }

        match self.focus {
            FocusTarget::Play => match nav {
                Nav::Accept | Nav::Activate => {
                    let (drift, warning, newer_refused) = self.check_drift();
                    if drift != Drift::None {
                        self.warning_text = warning;
                        self.state = ContinueState::Drift {
                            newer_save_refused: newer_refused,
                        };
                        if newer_refused {
                            self.play_action = Some(PlayAction::Update);
                            self.focus = FocusTarget::Update;
                        } else {
                            self.play_action = Some(PlayAction::Play);
                            self.focus = FocusTarget::Play;
                        }
                        false
                    } else {
                        self.state = ContinueState::Syncing(SyncStep::Pull);
                        self.play_action = Some(PlayAction::Syncing);
                        true
                    }
                }
                Nav::Down => {
                    self.focus = FocusTarget::Saves;
                    false
                }
                _ => false,
            },
            FocusTarget::Saves => match nav {
                Nav::Up => {
                    self.focus = FocusTarget::Play;
                    false
                }
                Nav::Right => {
                    self.focus = FocusTarget::Settings;
                    false
                }
                _ => false,
            },
            FocusTarget::Settings => match nav {
                Nav::Up => {
                    self.focus = FocusTarget::Play;
                    false
                }
                Nav::Left => {
                    self.focus = FocusTarget::Saves;
                    false
                }
                Nav::Right => {
                    self.focus = FocusTarget::Fleet;
                    false
                }
                _ => false,
            },
            FocusTarget::Fleet => match nav {
                Nav::Up => {
                    self.focus = FocusTarget::Play;
                    false
                }
                Nav::Left => {
                    self.focus = FocusTarget::Settings;
                    false
                }
                Nav::Right => {
                    self.focus = FocusTarget::About;
                    false
                }
                _ => false,
            },
            FocusTarget::About => match nav {
                Nav::Up => {
                    self.focus = FocusTarget::Play;
                    false
                }
                Nav::Left => {
                    self.focus = FocusTarget::Fleet;
                    false
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Complete resolution of an active conflict choice.
    pub fn resolve(&mut self, result: std::result::Result<(), String>) {
        self.finish_resolving(result);
    }

    fn finish_resolving(&mut self, result: std::result::Result<(), String>) {
        if let Some(conflict) = &mut self.conflict {
            if let ConflictPhase::Resolving { selected } = conflict.phase {
                match result {
                    Ok(()) => {
                        let kept_card = match selected {
                            Side::This => conflict.this_card.clone(),
                            Side::Relay => conflict.relay_card.clone(),
                        };
                        let dev = if kept_card.device.is_empty() {
                            "unknown device"
                        } else {
                            kept_card.device.as_str()
                        };
                        self.status_message =
                            Some(format!("Kept the one from {dev}. The other is in Backups."));
                        self.local_card = Some(kept_card);
                        self.conflict = None;
                        self.state = ContinueState::Ready;
                        self.play_action = Some(PlayAction::Play);
                        self.focus = FocusTarget::Play;
                    }
                    Err(reason) => {
                        conflict.phase = ConflictPhase::Failed { selected, reason };
                        self.focus = FocusTarget::Play;
                    }
                }
            }
        }
    }

    /// Execute the full play flow with fakes: pre-play drift check, pull, run game, push.
    pub fn play(
        &mut self,
        relay: &dyn Relay,
        game: &dyn Game,
        save_dir: &Path,
        env: &[(String, String)],
    ) -> Result<Played> {
        let (drift, warning, newer_refused) = self.check_drift();
        if drift != Drift::None
            && !matches!(
                self.state,
                ContinueState::Drift {
                    newer_save_refused: false
                }
            )
        {
            self.warning_text = warning;
            self.state = ContinueState::Drift {
                newer_save_refused: newer_refused,
            };
            self.play_action = if newer_refused {
                Some(PlayAction::Update)
            } else {
                Some(PlayAction::Play)
            };
            self.focus = if newer_refused {
                FocusTarget::Update
            } else {
                FocusTarget::Play
            };
            return Err(Error::Saves("pre-play drift warning".into()));
        }

        self.state = ContinueState::Syncing(SyncStep::Pull);
        self.play_action = Some(PlayAction::Syncing);

        let played = launch::play(relay, game, save_dir, env, &|sync| match sync {
            Sync::Conflict { .. } => Keep::Local,
            _ => Keep::Local,
        })?;

        self.state = ContinueState::Ready;
        self.play_action = Some(PlayAction::Play);
        Ok(played)
    }
}

fn is_version_newer(theirs: &str, ours: &str) -> bool {
    let parse_parts =
        |s: &str| -> Vec<u32> { s.split('.').filter_map(|p| p.parse::<u32>().ok()).collect() };
    let theirs_parts = parse_parts(theirs);
    let ours_parts = parse_parts(ours);
    theirs_parts > ours_parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use acnh_core::platform::Os;
    use std::path::PathBuf;

    fn fake_card(saved_unix: u64, device: &str, play_seconds: u64, version: &str) -> SaveCard {
        SaveCard {
            saved_unix,
            device: device.to_string(),
            play_seconds,
            version: version.to_string(),
            picture: PathBuf::from("assets/acnhrecomp.png"),
        }
    }

    fn fake_device(device: &str, version: &str, build_id: &str) -> DeviceRecord {
        DeviceRecord {
            device: device.to_string(),
            os: Os::Linux,
            version: version.to_string(),
            build_id: build_id.to_string(),
            last_sync_unix: 1_700_000_000,
            free_bytes: 1_000_000,
        }
    }

    #[test]
    fn a_starts_the_flow_in_one_press() {
        let local = fake_card(100, "handheld", 1000, "3.0.2");
        let dev = fake_device("handheld", "3.0.2", "b1");
        let mut vm = ContinuePageVm::new(Some(local), Some(dev));
        assert_eq!(vm.state(), &ContinueState::Ready);
        assert_eq!(vm.focus(), FocusTarget::Play);
        assert_eq!(vm.play_action(), Some(PlayAction::Play));

        // One press of Accept (A button) starts the flow immediately!
        let started = vm.handle(Nav::Accept);
        assert!(started);
        assert_eq!(vm.state(), &ContinueState::Syncing(SyncStep::Pull));
        assert_eq!(vm.play_action(), Some(PlayAction::Syncing));
    }

    #[test]
    fn forced_conflict_shows_two_cards_and_keeps_the_one_picked() {
        let this_card = fake_card(1000, "AMD handheld", 3600, "3.0.2");
        let relay_card = fake_card(2000, "Strix Halo tablet", 7200, "3.0.2");
        let mut vm = ContinuePageVm::new(Some(this_card.clone()), None);

        // Force conflict:
        vm.set_conflict(this_card.clone(), relay_card.clone());
        assert_eq!(vm.state(), &ContinueState::Conflict);
        assert_eq!(vm.play_action(), Some(PlayAction::ResolveConflict));
        let conflict = vm.conflict().expect("conflict view exists");
        assert_eq!(conflict.phase, ConflictPhase::Pending);
        assert_eq!(conflict.newer, Side::Relay);
        assert_eq!(conflict.longer_play, Side::Relay);
        assert!(!conflict.versions_differ);

        // Accept opens the chooser:
        assert!(!vm.handle(Nav::Accept));
        assert_eq!(vm.play_action(), None);
        let conflict = vm.conflict().expect("conflict view exists");
        assert_eq!(
            conflict.phase,
            ConflictPhase::Choosing {
                focused: Side::Relay
            }
        );
        // Shows both cards side by side:
        assert_eq!(conflict.this_card, this_card);
        assert_eq!(conflict.relay_card, relay_card);

        // Switch focus to This:
        assert!(!vm.handle(Nav::Left));
        let conflict = vm.conflict().expect("conflict view exists");
        assert_eq!(
            conflict.phase,
            ConflictPhase::Choosing {
                focused: Side::This
            }
        );

        // Commit choice of This:
        assert!(vm.handle(Nav::Accept));
        assert_eq!(vm.state(), &ContinueState::Ready);
        assert_eq!(vm.play_action(), Some(PlayAction::Play));
        assert_eq!(vm.focus(), FocusTarget::Play);
        assert_eq!(vm.local_card(), Some(&this_card));
        assert_eq!(
            vm.status_message(),
            Some("Kept the one from AMD handheld. The other is in Backups.")
        );

        // Now test picking Relay instead:
        vm.set_conflict(this_card.clone(), relay_card.clone());
        vm.handle(Nav::Accept); // Open chooser, default focused on newer (Relay)
        assert!(vm.handle(Nav::Accept)); // Commit choice of Relay
        assert_eq!(vm.state(), &ContinueState::Ready);
        assert_eq!(vm.local_card(), Some(&relay_card));
        assert_eq!(
            vm.status_message(),
            Some("Kept the one from Strix Halo tablet. The other is in Backups.")
        );
    }

    #[test]
    fn drift_shows_the_warning_before_play() {
        let local = fake_card(1000, "handheld", 1000, "3.0.2");
        let this_dev = fake_device("handheld", "3.0.2", "b1");
        let mut vm = ContinuePageVm::new(Some(local), Some(this_dev));

        // Save was made on version 3.0.3 by "desktop"
        let saved_by = fake_device("desktop", "3.0.3", "b2");
        vm.saved_by = Some(saved_by);

        assert_eq!(vm.state(), &ContinueState::Ready);

        // Attempting to play with Accept: drift is detected and warning is shown BEFORE play starts!
        let started = vm.handle(Nav::Accept);
        assert!(!started); // Did NOT start play!
        assert_eq!(
            vm.state(),
            &ContinueState::Drift {
                newer_save_refused: true
            }
        );
        assert_eq!(
            vm.warning_text(),
            Some(
                "desktop made this save with version 3.0.3; this device has 3.0.2. \
                 Put the same version on both, or the game may refuse the island."
            )
        );
        // Because save was from a newer version, Play anyway is absent and focus is on Update:
        assert_eq!(vm.play_action(), Some(PlayAction::Update));
        assert_eq!(vm.focus(), FocusTarget::Update);

        // Now test build drift (same version 3.0.2, different build):
        let build_drift_saved_by = fake_device("desktop", "3.0.2", "b2");
        let mut vm2 = ContinuePageVm::new(
            Some(fake_card(1000, "handheld", 1000, "3.0.2")),
            Some(fake_device("handheld", "3.0.2", "b1")),
        );
        vm2.saved_by = Some(build_drift_saved_by);

        let started = vm2.handle(Nav::Accept);
        assert!(!started); // Did NOT start play immediately; warning shown first!
        assert_eq!(
            vm2.state(),
            &ContinueState::Drift {
                newer_save_refused: false
            }
        );
        assert_eq!(
            vm2.warning_text(),
            Some(
                "desktop made this save with another build of ACNH Universal. \
                 Update this device first, or play on."
            )
        );
        // Play anyway is available:
        assert_eq!(vm2.play_action(), Some(PlayAction::Play));
        assert_eq!(vm2.focus(), FocusTarget::Play);

        // Pressing Accept again plays anyway:
        let played_anyway = vm2.handle(Nav::Accept);
        assert!(played_anyway);
        assert_eq!(vm2.state(), &ContinueState::Syncing(SyncStep::Pull));
    }

    #[test]
    fn back_from_chooser_returns_to_pending_without_writes() {
        let this_card = fake_card(1000, "AMD handheld", 3600, "3.0.2");
        let relay_card = fake_card(2000, "Strix Halo tablet", 7200, "3.0.2");
        let mut vm = ContinuePageVm::new(Some(this_card.clone()), None);

        vm.set_conflict(this_card.clone(), relay_card.clone());
        vm.handle(Nav::Accept); // Open chooser
        assert!(matches!(
            vm.conflict().unwrap().phase,
            ConflictPhase::Choosing { .. }
        ));

        // Back returns to Pending:
        vm.handle(Nav::Back);
        assert_eq!(vm.conflict().unwrap().phase, ConflictPhase::Pending);
        assert_eq!(vm.play_action(), Some(PlayAction::ResolveConflict));
        assert_eq!(vm.focus(), FocusTarget::Play);
        assert_eq!(vm.local_card(), Some(&this_card));
    }

    #[test]
    fn conflict_failure_and_retry_flow() {
        let this_card = fake_card(1000, "AMD handheld", 3600, "3.0.2");
        let relay_card = fake_card(2000, "Strix Halo tablet", 7200, "3.0.2");
        let mut vm = ContinuePageVm::new(Some(this_card.clone()), None);
        vm.auto_resolve = false; // test manual resolution transitions

        vm.set_conflict(this_card.clone(), relay_card.clone());
        vm.handle(Nav::Accept); // Open chooser
        vm.handle(Nav::Accept); // Choose Side::Relay -> enters Resolving
        assert_eq!(
            vm.conflict().unwrap().phase,
            ConflictPhase::Resolving {
                selected: Side::Relay
            }
        );

        // Simulate failure:
        vm.resolve(Err("disk full".to_string()));
        assert_eq!(
            vm.conflict().unwrap().phase,
            ConflictPhase::Failed {
                selected: Side::Relay,
                reason: "disk full".to_string()
            }
        );

        // Retry re-enters Resolving:
        vm.handle(Nav::Accept);
        assert_eq!(
            vm.conflict().unwrap().phase,
            ConflictPhase::Resolving {
                selected: Side::Relay
            }
        );

        // Now succeed:
        vm.resolve(Ok(()));
        assert_eq!(vm.state(), &ContinueState::Ready);
        assert_eq!(vm.local_card(), Some(&relay_card));
    }

    #[test]
    fn navigation_between_play_and_page_buttons() {
        let mut vm = ContinuePageVm::new(Some(fake_card(1, "dev", 1, "3.0.2")), None);
        assert_eq!(vm.focus(), FocusTarget::Play);

        vm.handle(Nav::Down);
        assert_eq!(vm.focus(), FocusTarget::Saves);

        vm.handle(Nav::Right);
        assert_eq!(vm.focus(), FocusTarget::Settings);

        vm.handle(Nav::Right);
        assert_eq!(vm.focus(), FocusTarget::Fleet);

        vm.handle(Nav::Right);
        assert_eq!(vm.focus(), FocusTarget::About);

        vm.handle(Nav::Up);
        assert_eq!(vm.focus(), FocusTarget::Play);
    }
}
