//! The Continue page view model: the island card, the one Play button, the conflict chooser and
//! the drift warning (SPEC.md M10, M11; docs/design/continue-screen.md). Plain Rust with no
//! window: a press may leave a `Request` for the driver (`crate::play::Session`), which runs it
//! and feeds the result back as `PlayEvent`s; `ui/continue.slint` draws `view()`.

use std::path::{Path, PathBuf};

use acnh_core::fleet::{self, DeviceRecord, Drift};
use acnh_core::launch::{Keep, Played, Sync};
use acnh_core::prepare::redact;
use acnh_core::savecard::{self, SaveCard};
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

impl Side {
    /// The answer the relay gets when this side is kept.
    pub fn keep(self) -> Keep {
        match self {
            Self::This => Keep::Local,
            Self::Relay => Keep::Server,
        }
    }
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

    /// "Newer" and "Longer play" in words for `side`, when the two cards are known to differ
    /// (a time or play time of 0 is not known); "" otherwise.
    fn badges(&self, side: Side) -> String {
        let differ = |a: u64, b: u64| a != b && a != 0 && b != 0;
        let (this, relay) = (&self.this_card, &self.relay_card);
        let mut words = Vec::new();
        if self.newer == side && differ(this.saved_unix, relay.saved_unix) {
            words.push("Newer");
        }
        if self.longer_play == side && differ(this.play_seconds, relay.play_seconds) {
            words.push("Longer play");
        }
        words.join(", ")
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

/// The page buttons under Play, left to right.
const PAGE_ROW: [(FocusTarget, &str); 4] = [
    (FocusTarget::Saves, "Saves"),
    (FocusTarget::Settings, "Settings"),
    (FocusTarget::Fleet, "Fleet"),
    (FocusTarget::About, "About"),
];

/// What a press asks the driver to run; taken with `ContinuePageVm::take_request`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// `launch::play`: pull the island, run the game, push the island.
    Play,
    /// One relay step on its own (`Pull` or `Push`): with `keep`, the player's answer to a
    /// conflict found at that step; without, a push tried again after the game has run.
    Sync { step: SyncStep, keep: Option<Keep> },
    /// Bring this device's build up to date (`update`).
    Update,
}

/// What a running job tells the page, in the order it happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayEvent {
    /// A relay step began.
    Step(SyncStep),
    /// The player was started; it runs until the next event.
    Playing,
    /// Both sides changed since the last sync, found at `step`. Nothing was touched and no
    /// answer was sent: the job has ended and the page asks the player. `server` and `local`
    /// are the relay's texts for the two sides (`launch::Sync::Conflict`).
    Conflict {
        step: SyncStep,
        server: String,
        local: String,
    },
    /// `launch::play` returned.
    Played(Result<Played>),
    /// A `Request::Sync` returned.
    Synced(Result<Sync>),
    /// A `Request::Update` returned.
    Updated(Result<()>),
}

/// One save card as the page draws it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardLines {
    /// "<device>, <date>" (`savecard::side_by_side`); the device alone when the time is not
    /// known.
    pub saved: String,
    /// "<h> h <mm> min played".
    pub played: String,
    /// "Version <name>".
    pub version: String,
    /// "Newer", "Longer play" or both, in words; "" for neither.
    pub badges: String,
}

/// Everything `ui/continue.slint` draws, as text and flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageView {
    /// The status line of the island card ("Up to date").
    pub status: String,
    /// The island card; `None` is the empty card.
    pub card: Option<CardLines>,
    /// The one line under the card: the conflict's note, an error, or the toast.
    pub message: String,
    /// The one Play button's text (its action, or the step while it runs); "" when the
    /// chooser or the drift warning holds the buttons instead.
    pub button: String,
    pub button_enabled: bool,
    /// This device's card and the relay's, while the chooser is open.
    pub chooser: Option<(CardLines, CardLines)>,
    /// False while the chosen island is being kept.
    pub keep_enabled: bool,
    /// The drift warning; "" for none.
    pub warning: String,
    pub play_anyway: bool,
    /// Back is drawn: the chooser or the drift warning is open and can be left.
    pub back: bool,
    pub pages_enabled: bool,
    /// The About overlay is open over the page; Back closes it.
    pub about_open: bool,
    pub settings_open: bool,
    pub fleet_open: bool,
    pub saves_open: bool,
    pub focus: FocusTarget,
}

/// The card for one side of a conflict, from the text the relay gave for it
/// (`launch::Sync::Conflict`): `savecard`'s sidecar lines (`device <name>`, `play <seconds>`,
/// `version <name>`) and `saved <unix seconds>`, in any order, a later line winning. Text
/// with none of those lines is taken as the device's name, so the card still says whose it is.
pub fn card_from_text(text: &str, picture: &Path) -> SaveCard {
    let mut card = SaveCard {
        saved_unix: 0,
        device: String::new(),
        play_seconds: 0,
        version: String::new(),
        picture: picture.to_path_buf(),
    };
    let mut known = false;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        match key {
            "device" => card.device = value.to_string(),
            "version" => card.version = value.to_string(),
            "play" => card.play_seconds = value.parse().unwrap_or(card.play_seconds),
            "saved" => card.saved_unix = value.parse().unwrap_or(card.saved_unix),
            _ => continue,
        }
        known = true;
    }
    if !known {
        card.device = text.lines().next().unwrap_or("").trim().to_string();
    }
    card
}

fn device_name(card: &SaveCard) -> &str {
    if card.device.is_empty() {
        "unknown device"
    } else {
        &card.device
    }
}

fn card_lines(card: &SaveCard, badges: String) -> CardLines {
    let [saved, played] = savecard::side_by_side(card);
    CardLines {
        saved: if card.saved_unix == 0 {
            device_name(card).to_string()
        } else {
            saved
        },
        played,
        version: if card.version.is_empty() {
            "Version unknown".to_string()
        } else {
            format!("Version {}", card.version)
        },
        badges,
    }
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
    /// The About overlay, opened from the About entry in the page row.
    about_open: bool,
    settings_open: bool,
    fleet_open: bool,
    saves_open: bool,
    pub last_error: Option<String>,
    pub status_message: Option<String>,
    pub warning_text: Option<String>,
    pub this_device: Option<DeviceRecord>,
    pub saved_by: Option<DeviceRecord>,
    /// True while no job waits on the answer to the conflict (one forced with `set_conflict`):
    /// a pick settles it in the view model at once. A conflict a job reports
    /// (`PlayEvent::Conflict`) sets it false: the pick leaves a `Request::Sync` with the
    /// `Keep`, and the relay's result settles it.
    pub auto_resolve: bool,
    /// The home folder, shortened to `~` in any reason shown (`prepare::redact`).
    pub home: PathBuf,
    /// The step the relay reported the conflict at.
    conflict_step: SyncStep,
    request: Option<Request>,
}

impl Default for ContinuePageVm {
    fn default() -> Self {
        Self::new(None, None)
    }
}

impl ContinuePageVm {
    pub fn settings_open(&self) -> bool {
        self.settings_open
    }

    pub fn saves_open(&self) -> bool {
        self.saves_open
    }

    pub fn fleet_open(&self) -> bool {
        self.fleet_open
    }

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
            about_open: false,
            settings_open: false,
            fleet_open: false,
            saves_open: false,
            last_error: None,
            status_message: None,
            warning_text: None,
            this_device,
            saved_by: None,
            auto_resolve: true,
            home: PathBuf::new(),
            conflict_step: SyncStep::Pull,
            request: None,
        }
    }

    /// Whether the About overlay is open.
    pub fn about_open(&self) -> bool {
        self.about_open
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

    /// The job the last press asked for, once: the driver runs it and reports back through
    /// `on_play_event`.
    pub fn take_request(&mut self) -> Option<Request> {
        self.request.take()
    }

    /// The island as it is on this device now (read again after a job changed it).
    pub fn set_local_card(&mut self, card: Option<SaveCard>) {
        self.local_card = card;
        match (&self.state, self.local_card.is_some()) {
            (ContinueState::NoIsland, true) => self.state = ContinueState::Ready,
            (ContinueState::Ready, false) => self.state = ContinueState::NoIsland,
            _ => {}
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
        self.show_drift();
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

    /// Enter `Drift` with its warning when there is drift; true when it did.
    fn show_drift(&mut self) -> bool {
        let (drift, warning, newer_refused) = self.check_drift();
        if drift == Drift::None {
            return false;
        }
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
        true
    }

    /// True while a job runs: presses wait for its result.
    fn busy(&self) -> bool {
        matches!(
            self.state,
            ContinueState::Syncing(_)
                | ContinueState::Starting
                | ContinueState::Playing
                | ContinueState::Saving
                | ContinueState::Updating(_)
        )
    }

    /// Whether `target` can be pressed in this state (what the page draws enabled).
    fn available(&self, target: FocusTarget) -> bool {
        if self.busy() {
            return false;
        }
        if self.about_open || self.settings_open || self.fleet_open || self.saves_open {
            // Only Back works under the About overlay.
            return target == FocusTarget::Back;
        }
        let phase = self.conflict.as_ref().map(|conflict| &conflict.phase);
        let in_drift = matches!(self.state, ContinueState::Drift { .. });
        match target {
            FocusTarget::Keep(_) => matches!(phase, Some(ConflictPhase::Choosing { .. })),
            FocusTarget::Update => in_drift,
            FocusTarget::Back => {
                in_drift
                    || matches!(
                        phase,
                        Some(ConflictPhase::Choosing { .. } | ConflictPhase::Failed { .. })
                    )
            }
            FocusTarget::Play => {
                self.play_action.is_some()
                    && self.state
                        != ContinueState::Drift {
                            newer_save_refused: true,
                        }
            }
            _ => !in_drift && matches!(phase, None | Some(ConflictPhase::Pending)),
        }
    }

    /// A click or tap on a button: the same as moving focus there and pressing Accept (Back
    /// is the Back press). Returns true if an action was committed.
    pub fn click(&mut self, target: FocusTarget) -> bool {
        if !self.available(target) {
            return false;
        }
        match target {
            FocusTarget::Back => self.handle(Nav::Back),
            FocusTarget::Keep(side) => {
                self.focus_keep(side);
                self.handle(Nav::Accept)
            }
            _ => {
                self.focus = target;
                self.handle(Nav::Accept)
            }
        }
    }

    /// Handle controller or keyboard navigation input. Returns true if an action was committed.
    pub fn handle(&mut self, nav: Nav) -> bool {
        if self.busy() {
            return false;
        }
        if self.about_open || self.settings_open || self.fleet_open || self.saves_open {
            if nav == Nav::Back {
                self.about_open = false;
                self.settings_open = false;
                self.fleet_open = false;
                self.saves_open = false;
            }
            return false;
        }
        match nav {
            Nav::Quit => false,
            Nav::Menu => self.click(FocusTarget::Settings),
            Nav::Prev | Nav::Next => false,
            Nav::Accept => {
                // Accept clears the toast; what it starts may set a new one.
                self.status_message = None;
                self.handle_screen_nav(nav)
            }
            _ => self.handle_screen_nav(nav),
        }
    }

    fn handle_screen_nav(&mut self, nav: Nav) -> bool {
        let phase = self
            .conflict
            .as_ref()
            .map(|conflict| conflict.phase.clone());
        match phase {
            Some(ConflictPhase::Pending) => match nav {
                // The press that opens the chooser never picks a save.
                Nav::Accept if self.focus == FocusTarget::Play => {
                    if let Some(newer) = self.conflict.as_ref().map(|conflict| conflict.newer) {
                        self.focus_keep(newer);
                        self.play_action = None;
                    }
                    return false;
                }
                Nav::Back => return false,
                _ => {} // focus moves between Resolve conflict and the page row, below
            },
            Some(ConflictPhase::Choosing { focused }) => {
                match nav {
                    Nav::Left => self.focus_keep(Side::This),
                    Nav::Right => self.focus_keep(Side::Relay),
                    Nav::Back => self.close_chooser(),
                    Nav::Accept => return self.start_resolving(focused),
                    _ => {}
                }
                return false;
            }
            Some(ConflictPhase::Resolving { .. }) => return false,
            Some(ConflictPhase::Failed { selected, .. }) => {
                match nav {
                    Nav::Accept => return self.start_resolving(selected),
                    Nav::Back => self.close_chooser(),
                    _ => {}
                }
                return false;
            }
            None => {}
        }

        if let ContinueState::Drift { newer_save_refused } = self.state {
            match nav {
                Nav::Back => {
                    self.state = ContinueState::Ready;
                    self.warning_text = None;
                    self.play_action = Some(PlayAction::Play);
                    self.focus = FocusTarget::Play;
                }
                Nav::Accept if newer_save_refused || self.focus == FocusTarget::Update => {
                    self.state = ContinueState::Updating("Starting update".to_string());
                    self.play_action = Some(PlayAction::Syncing);
                    self.request = Some(Request::Update);
                    return true;
                }
                // Play anyway.
                Nav::Accept if self.focus == FocusTarget::Play => return self.start_play(),
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

        let in_row = PAGE_ROW
            .iter()
            .position(|(target, _)| *target == self.focus);
        match (in_row, nav) {
            (None, Nav::Accept) if self.focus == FocusTarget::Play => return self.accept_play(),
            (None, Nav::Down) if self.focus == FocusTarget::Play => {
                self.focus = FocusTarget::Saves;
            }
            (Some(_), Nav::Up) => self.focus = FocusTarget::Play,
            (Some(at), Nav::Left) if at > 0 => self.focus = PAGE_ROW[at - 1].0,
            (Some(at), Nav::Right) if at + 1 < PAGE_ROW.len() => self.focus = PAGE_ROW[at + 1].0,
            (Some(at), Nav::Accept) if PAGE_ROW[at].0 == FocusTarget::Saves => {
                self.saves_open = true;
            }
            (Some(at), Nav::Accept) if PAGE_ROW[at].0 == FocusTarget::Fleet => {
                self.fleet_open = true;
            }
            (Some(at), Nav::Accept) if PAGE_ROW[at].0 == FocusTarget::Settings => {
                self.settings_open = true;
            }
            (Some(at), Nav::Accept) if PAGE_ROW[at].0 == FocusTarget::About => {
                self.about_open = true;
            }
            (Some(at), Nav::Accept) => {
                // The other pages come with their own issues (2.05 to 2.08).
                self.status_message = Some(format!("{} is coming soon.", PAGE_ROW[at].1));
            }
            _ => {}
        }
        false
    }

    /// Accept on the one Play button outside the chooser and the drift warning.
    fn accept_play(&mut self) -> bool {
        if matches!(self.state, ContinueState::PushFailed(_)) {
            // The game has already run: only the push is tried again.
            self.state = ContinueState::Saving;
            self.play_action = Some(PlayAction::Saving);
            self.request = Some(Request::Sync {
                step: SyncStep::Push,
                keep: None,
            });
            return true;
        }
        if self.show_drift() {
            return false;
        }
        self.start_play()
    }

    fn start_play(&mut self) -> bool {
        self.state = ContinueState::Syncing(SyncStep::Pull);
        self.play_action = Some(PlayAction::Syncing);
        self.request = Some(Request::Play);
        true
    }

    fn set_phase(&mut self, phase: ConflictPhase) {
        if let Some(conflict) = &mut self.conflict {
            conflict.phase = phase;
        }
    }

    fn focus_keep(&mut self, side: Side) {
        self.set_phase(ConflictPhase::Choosing { focused: side });
        self.focus = FocusTarget::Keep(side);
    }

    /// Back to `Pending`: both cards stay, nothing was written.
    fn close_chooser(&mut self) {
        self.set_phase(ConflictPhase::Pending);
        self.play_action = Some(PlayAction::ResolveConflict);
        self.focus = FocusTarget::Play;
    }

    /// A fresh Accept on a Keep button (or Retry for the same choice): the choice is
    /// committed, and the relay is asked to keep that side.
    fn start_resolving(&mut self, selected: Side) -> bool {
        self.set_phase(ConflictPhase::Resolving { selected });
        self.play_action = None;
        if self.auto_resolve {
            self.finish_resolving(Ok(()));
        } else {
            self.request = Some(Request::Sync {
                step: self.conflict_step,
                keep: Some(selected.keep()),
            });
        }
        true
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
                        let kept_card = conflict.side_card(selected).clone();
                        let dev = device_name(&kept_card);
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
                        self.play_action = Some(PlayAction::Retry);
                        self.focus = FocusTarget::Play;
                    }
                }
            }
        }
    }

    fn reason(&self, error: &Error) -> String {
        redact(&error.to_string(), &self.home)
    }

    fn resolving(&self) -> bool {
        matches!(
            self.conflict.as_ref().map(|conflict| &conflict.phase),
            Some(ConflictPhase::Resolving { .. })
        )
    }

    /// Take in what a running job reports (`Request`, run by the driver).
    pub fn on_play_event(&mut self, event: PlayEvent) {
        match event {
            // While a choice is being kept, the chooser shows its own progress.
            PlayEvent::Step(_) if self.conflict.is_some() => {}
            PlayEvent::Step(SyncStep::Push) => {
                self.state = ContinueState::Saving;
                self.play_action = Some(PlayAction::Saving);
            }
            PlayEvent::Step(step) => {
                self.state = ContinueState::Syncing(step);
                self.play_action = Some(PlayAction::Syncing);
            }
            PlayEvent::Playing => {
                self.state = ContinueState::Playing;
                self.play_action = Some(PlayAction::Playing);
            }
            PlayEvent::Conflict {
                step,
                server,
                local,
            } => {
                let picture = match &self.local_card {
                    Some(card) => card.picture.clone(),
                    None => PathBuf::new(),
                };
                let this_card = match &self.local_card {
                    Some(card) => card.clone(),
                    None => card_from_text(&local, &picture),
                };
                let relay_card = card_from_text(&server, &picture);
                self.relay_card = Some(relay_card.clone());
                self.auto_resolve = false;
                self.conflict_step = step;
                self.status_message = None;
                self.set_conflict(this_card, relay_card);
            }
            PlayEvent::Played(result) => {
                self.status_message = None;
                self.focus = FocusTarget::Play;
                match result {
                    Ok(played) if played.exit_code != 0 => {
                        self.fail(ContinueState::PlayerFailed {
                            exit: played.exit_code,
                            log_hint: "The newest file in the logs folder says why.".to_string(),
                        });
                    }
                    Ok(played) => {
                        let offline =
                            played.pulled == Sync::Offline || played.pushed == Sync::Offline;
                        self.state = if offline {
                            ContinueState::RelayUnavailable
                        } else {
                            ContinueState::Ready
                        };
                        self.play_action = Some(PlayAction::Play);
                    }
                    Err(error) => {
                        let reason = self.reason(&error);
                        self.fail(match self.state {
                            ContinueState::Playing => ContinueState::PlayerFailed {
                                exit: -1,
                                log_hint: reason,
                            },
                            ContinueState::Saving => ContinueState::PushFailed(reason),
                            _ => ContinueState::RelayError(reason),
                        });
                    }
                }
            }
            PlayEvent::Synced(result) => {
                let outcome = match result {
                    Ok(Sync::Done(_)) => Ok(()),
                    Ok(Sync::Offline) => Err("the relay did not answer".to_string()),
                    Ok(Sync::Conflict { .. }) => {
                        Err("the relay still reports a conflict".to_string())
                    }
                    Err(error) => Err(self.reason(&error)),
                };
                if self.resolving() {
                    self.finish_resolving(outcome);
                } else {
                    // A push tried again.
                    self.focus = FocusTarget::Play;
                    match outcome {
                        Ok(()) => {
                            self.state = ContinueState::Ready;
                            self.play_action = Some(PlayAction::Play);
                        }
                        Err(reason) => self.fail(ContinueState::PushFailed(reason)),
                    }
                }
            }
            PlayEvent::Updated(result) => {
                self.focus = FocusTarget::Play;
                match result {
                    Ok(()) => {
                        self.state = ContinueState::Ready;
                        self.play_action = Some(PlayAction::Play);
                    }
                    Err(error) => {
                        let reason = self.reason(&error);
                        self.fail(ContinueState::UpdateFailed(reason));
                    }
                }
            }
        }
    }

    /// Enter a failed state: its reason is shown and the one button reads Retry.
    fn fail(&mut self, state: ContinueState) {
        self.state = state;
        self.play_action = Some(PlayAction::Retry);
    }

    fn button_text(&self) -> String {
        match &self.state {
            // The warning holds the buttons.
            ContinueState::Drift { .. } => String::new(),
            ContinueState::Syncing(step) => step.label().to_string(),
            ContinueState::Starting => "Starting the game".to_string(),
            ContinueState::Saving => SyncStep::Push.label().to_string(),
            ContinueState::Updating(stage) => stage.clone(),
            _ => match self.play_action {
                Some(action) => action.label().to_string(),
                None => String::new(),
            },
        }
    }

    fn message(&self) -> String {
        if let Some(toast) = &self.status_message {
            return toast.clone();
        }
        if let Some(conflict) = &self.conflict {
            let from = |side: Side| device_name(conflict.side_card(side));
            return match &conflict.phase {
                ConflictPhase::Pending => format!(
                    "This device and the relay ({}) both have a newer island. Nothing was \
                     changed: choose which one to keep.",
                    from(Side::Relay)
                ),
                ConflictPhase::Choosing { .. } => {
                    "The other island is kept as a backup in Backups.".to_string()
                }
                ConflictPhase::Resolving { selected } => {
                    format!("Backing up, then keeping the one from {}.", from(*selected))
                }
                ConflictPhase::Failed { selected, reason } => {
                    format!("Could not keep the one from {}: {reason}", from(*selected))
                }
            };
        }
        match &self.state {
            ContinueState::NoIsland => "A new island starts when the game does.".to_string(),
            ContinueState::RelayUnavailable => {
                "The relay did not answer: this device's island is used and will sync later."
                    .to_string()
            }
            ContinueState::RelayError(reason) => format!("The relay could not sync: {reason}"),
            ContinueState::PlayerFailed { exit, log_hint } if *exit < 0 => {
                format!("The game did not start: {log_hint}")
            }
            ContinueState::PlayerFailed { exit, log_hint } => {
                format!("The game stopped with an error (exit {exit}). {log_hint}")
            }
            ContinueState::PushFailed(reason) => {
                format!("The island is safe on this device but not on the relay yet: {reason}")
            }
            ContinueState::UpdateFailed(reason) => format!("The update did not finish: {reason}"),
            _ => String::new(),
        }
    }

    /// What the page draws now.
    pub fn view(&self) -> PageView {
        let chooser = match &self.conflict {
            Some(conflict) if conflict.phase != ConflictPhase::Pending => Some((
                card_lines(&conflict.this_card, conflict.badges(Side::This)),
                card_lines(&conflict.relay_card, conflict.badges(Side::Relay)),
            )),
            _ => None,
        };
        let in_drift = matches!(self.state, ContinueState::Drift { .. });
        PageView {
            status: self.card_status().to_string(),
            card: self
                .local_card
                .as_ref()
                .map(|card| card_lines(card, String::new())),
            message: self.message(),
            button: self.button_text(),
            button_enabled: self.available(FocusTarget::Play),
            chooser,
            keep_enabled: self.available(FocusTarget::Keep(Side::This)),
            warning: match &self.warning_text {
                Some(warning) if in_drift => warning.clone(),
                _ => String::new(),
            },
            play_anyway: in_drift && self.available(FocusTarget::Play),
            back: self.available(FocusTarget::Back),
            pages_enabled: self.available(FocusTarget::Saves),
            about_open: self.about_open,
            settings_open: self.settings_open,
            fleet_open: self.fleet_open,
            saves_open: self.saves_open,
            focus: self.focus,
        }
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

    #[test]
    fn the_about_entry_opens_the_overlay_and_back_returns_to_continue() {
        let mut vm = ContinuePageVm::new(Some(fake_card(1, "dev", 1, "3.0.2")), None);
        for nav in [Nav::Down, Nav::Right, Nav::Right, Nav::Right] {
            vm.handle(nav);
        }
        assert_eq!(vm.focus(), FocusTarget::About);
        assert!(!vm.view().about_open);

        // Accept on About opens the overlay; nothing is asked of the driver.
        assert!(!vm.handle(Nav::Accept));
        assert!(vm.about_open());
        let view = vm.view();
        assert!(view.about_open && view.back && !view.pages_enabled);
        assert_eq!(view.message, "");

        // Under the overlay only Back works.
        vm.handle(Nav::Up);
        vm.handle(Nav::Left);
        vm.handle(Nav::Menu);
        assert!(!vm.click(FocusTarget::Settings));
        assert!(!vm.click(FocusTarget::Play));
        assert!(vm.about_open());
        assert_eq!(vm.focus(), FocusTarget::About);

        vm.handle(Nav::Back);
        assert!(!vm.about_open());
        let view = vm.view();
        assert!(!view.about_open && !view.back && view.pages_enabled);
        assert_eq!(vm.focus(), FocusTarget::About);
    }

    #[test]
    fn fleet_opens_from_the_page_row_and_back_returns() {
        let mut vm = ContinuePageVm::default();
        assert!(!vm.view().fleet_open);
        vm.click(FocusTarget::Fleet);
        assert!(vm.view().fleet_open);
        vm.handle(Nav::Back);
        assert!(!vm.view().fleet_open);
    }

    #[test]
    fn a_click_on_about_opens_the_overlay_and_a_click_on_back_closes_it() {
        let mut vm = ContinuePageVm::new(Some(fake_card(1, "dev", 1, "3.0.2")), None);
        vm.click(FocusTarget::About);
        assert!(vm.about_open());
        assert!(!vm.click(FocusTarget::Back));
        assert!(!vm.about_open());
    }

    // The page joined to the play flow: what a press asks for and what the results do.

    fn done() -> Sync {
        Sync::Done("ok".to_string())
    }

    fn relay_conflict(vm: &mut ContinuePageVm, step: SyncStep) {
        vm.on_play_event(PlayEvent::Conflict {
            step,
            server: "device Strix Halo tablet\nplay 7200\nversion 3.0.2\nsaved 2000".to_string(),
            local: "device AMD handheld".to_string(),
        });
    }

    #[test]
    fn one_press_asks_for_the_play_flow_once_and_follows_it() {
        let mut vm = ContinuePageVm::new(Some(fake_card(100, "handheld", 1000, "3.0.2")), None);
        assert_eq!(vm.view().button, "Play");
        assert_eq!(vm.view().status, "Up to date");
        assert!(vm.handle(Nav::Accept));
        assert_eq!(vm.take_request(), Some(Request::Play));
        assert_eq!(vm.take_request(), None);

        // While the flow runs, a second press starts nothing.
        assert!(!vm.handle(Nav::Accept));
        assert!(!vm.click(FocusTarget::Play));
        assert_eq!(vm.take_request(), None);
        assert_eq!(vm.view().button, "Getting your island");
        assert!(!vm.view().button_enabled);
        assert!(!vm.view().pages_enabled);

        vm.on_play_event(PlayEvent::Step(SyncStep::Pull));
        vm.on_play_event(PlayEvent::Playing);
        assert_eq!(vm.state(), &ContinueState::Playing);
        assert_eq!(vm.view().button, "Playing");
        vm.on_play_event(PlayEvent::Step(SyncStep::Push));
        assert_eq!(vm.state(), &ContinueState::Saving);
        assert_eq!(vm.view().button, "Saving your island");
        vm.on_play_event(PlayEvent::Played(Ok(Played {
            pulled: done(),
            exit_code: 0,
            pushed: done(),
        })));
        assert_eq!(vm.state(), &ContinueState::Ready);
        assert_eq!(vm.view().button, "Play");
        assert!(vm.view().button_enabled);
        assert_eq!(vm.take_request(), None);
    }

    #[test]
    fn a_click_on_play_is_the_same_one_press() {
        let mut vm = ContinuePageVm::new(None, None);
        assert_eq!(vm.state(), &ContinueState::NoIsland);
        assert_eq!(vm.view().card, None);
        assert!(vm.click(FocusTarget::Play));
        assert_eq!(vm.take_request(), Some(Request::Play));
        // Buttons that are not on the page now do nothing.
        assert!(!vm.click(FocusTarget::Keep(Side::This)));
        assert!(!vm.click(FocusTarget::Update));
    }

    #[test]
    fn a_relay_conflict_shows_two_cards_and_sends_the_pick_to_the_relay() {
        let this_card = fake_card(1000, "AMD handheld", 3600, "3.0.2");
        let relay_card = fake_card(2000, "Strix Halo tablet", 7200, "3.0.2");
        let mut vm = ContinuePageVm::new(Some(this_card.clone()), None);
        assert!(vm.handle(Nav::Accept));
        assert_eq!(vm.take_request(), Some(Request::Play));

        relay_conflict(&mut vm, SyncStep::Pull);
        assert_eq!(vm.state(), &ContinueState::Conflict);
        assert_eq!(vm.relay_card(), Some(&relay_card));
        // No answer was chosen for the player.
        assert_eq!(vm.take_request(), None);
        let view = vm.view();
        assert_eq!(view.button, "Resolve conflict");
        assert_eq!(view.status, "Unresolved conflict");
        assert_eq!(view.chooser, None);

        // Accept opens the chooser on the newer save and picks nothing.
        assert!(!vm.handle(Nav::Accept));
        assert_eq!(vm.take_request(), None);
        let view = vm.view();
        let (this, relay) = view.chooser.expect("both cards are shown");
        assert_eq!(this.saved, savecard::side_by_side(&this_card)[0]);
        assert_eq!(this.played, savecard::side_by_side(&this_card)[1]);
        assert_eq!(this.version, "Version 3.0.2");
        assert_eq!(this.badges, "");
        assert_eq!(relay.saved, savecard::side_by_side(&relay_card)[0]);
        assert_eq!(relay.played, savecard::side_by_side(&relay_card)[1]);
        assert_eq!(relay.badges, "Newer, Longer play");
        assert_eq!(view.focus, FocusTarget::Keep(Side::Relay));
        assert_eq!(view.button, "");
        assert!(view.keep_enabled);
        assert!(view.back);
        assert!(!view.pages_enabled);

        // The pick goes to the relay, at the step that found the conflict.
        assert!(vm.handle(Nav::Accept));
        assert_eq!(
            vm.take_request(),
            Some(Request::Sync {
                step: SyncStep::Pull,
                keep: Some(Keep::Server)
            })
        );
        // Not settled until the relay answers; a second press sends nothing.
        assert_eq!(vm.state(), &ContinueState::Conflict);
        assert!(!vm.view().keep_enabled);
        assert!(!vm.handle(Nav::Accept));
        assert!(!vm.click(FocusTarget::Keep(Side::This)));
        assert_eq!(vm.take_request(), None);
        vm.on_play_event(PlayEvent::Step(SyncStep::Backup));
        vm.on_play_event(PlayEvent::Step(SyncStep::Pull));
        assert_eq!(vm.state(), &ContinueState::Conflict);

        vm.on_play_event(PlayEvent::Synced(Ok(done())));
        assert_eq!(vm.state(), &ContinueState::Ready);
        assert_eq!(vm.conflict(), None);
        assert_eq!(vm.focus(), FocusTarget::Play);
        assert_eq!(
            vm.status_message(),
            Some("Kept the one from Strix Halo tablet. The other is in Backups.")
        );
        assert_eq!(
            vm.view().message,
            "Kept the one from Strix Halo tablet. The other is in Backups."
        );
        // Keeping an island does not start the game.
        assert_eq!(vm.take_request(), None);
    }

    #[test]
    fn keeping_this_one_sends_keep_local_and_a_failure_offers_retry_and_back() {
        let mut vm =
            ContinuePageVm::new(Some(fake_card(1000, "AMD handheld", 3600, "3.0.2")), None);
        vm.handle(Nav::Accept);
        vm.take_request();
        // Found after the game, at the push.
        relay_conflict(&mut vm, SyncStep::Push);
        assert!(!vm.click(FocusTarget::Play)); // opens the chooser
        assert!(vm.click(FocusTarget::Keep(Side::This)));
        let keep_this = Some(Request::Sync {
            step: SyncStep::Push,
            keep: Some(Keep::Local),
        });
        assert_eq!(vm.take_request(), keep_this);

        vm.on_play_event(PlayEvent::Synced(Ok(Sync::Offline)));
        assert_eq!(
            vm.conflict().map(|conflict| conflict.phase.clone()),
            Some(ConflictPhase::Failed {
                selected: Side::This,
                reason: "the relay did not answer".to_string()
            })
        );
        let view = vm.view();
        assert_eq!(view.button, "Retry");
        assert_eq!(
            view.message,
            "Could not keep the one from AMD handheld: the relay did not answer"
        );
        assert!(view.back);
        assert!(!view.keep_enabled);

        // Retry sends the same choice again.
        assert!(vm.handle(Nav::Accept));
        assert_eq!(vm.take_request(), keep_this);
        vm.on_play_event(PlayEvent::Synced(Err(Error::Saves(
            "no room in /home/user/backups".to_string(),
        ))));
        assert!(matches!(
            vm.conflict().map(|conflict| &conflict.phase),
            Some(ConflictPhase::Failed { .. })
        ));

        // Back leaves the conflict unresolved, with nothing sent.
        assert!(!vm.click(FocusTarget::Back));
        assert_eq!(
            vm.conflict().map(|conflict| conflict.phase.clone()),
            Some(ConflictPhase::Pending)
        );
        assert_eq!(vm.view().button, "Resolve conflict");
        assert_eq!(vm.take_request(), None);
    }

    #[test]
    fn a_pending_conflict_does_not_trap_focus_or_open_from_another_button() {
        let mut vm =
            ContinuePageVm::new(Some(fake_card(1000, "AMD handheld", 3600, "3.0.2")), None);
        vm.set_conflict(
            fake_card(1000, "AMD handheld", 3600, "3.0.2"),
            fake_card(2000, "Strix Halo tablet", 7200, "3.0.2"),
        );
        vm.handle(Nav::Down);
        assert_eq!(vm.focus(), FocusTarget::Saves);
        assert!(!vm.handle(Nav::Accept));
        assert_eq!(
            vm.conflict().map(|conflict| conflict.phase.clone()),
            Some(ConflictPhase::Pending)
        );
        assert!(vm.view().saves_open);
        vm.handle(Nav::Back);
        vm.handle(Nav::Up);
        assert_eq!(vm.focus(), FocusTarget::Play);
        // Back while the conflict is pending does nothing.
        assert!(!vm.handle(Nav::Back));
        assert_eq!(vm.state(), &ContinueState::Conflict);
    }

    #[test]
    fn the_drift_view_holds_the_warning_and_its_buttons() {
        let mut vm = ContinuePageVm::new(
            Some(fake_card(1000, "handheld", 1000, "3.0.2")),
            Some(fake_device("handheld", "3.0.2", "b1")),
        );
        vm.saved_by = Some(fake_device("desktop", "3.0.2", "b2"));
        assert!(!vm.click(FocusTarget::Play));
        // The warning is on the page and nothing was started.
        assert_eq!(vm.take_request(), None);
        let view = vm.view();
        assert_eq!(
            view.warning,
            "desktop made this save with another build of ACNH Universal. \
             Update this device first, or play on."
        );
        assert!(view.play_anyway);
        assert!(view.back);
        assert_eq!(view.button, "");
        assert!(!view.pages_enabled);

        // Update is one move away, and Accept on it asks for the update.
        vm.handle(Nav::Right);
        assert_eq!(vm.focus(), FocusTarget::Update);
        assert!(vm.handle(Nav::Accept));
        assert_eq!(vm.take_request(), Some(Request::Update));
        vm.on_play_event(PlayEvent::Updated(Err(Error::Hub(
            "no hub is paired".to_string(),
        ))));
        assert!(matches!(vm.state(), ContinueState::UpdateFailed(_)));
        assert_eq!(vm.view().button, "Retry");

        // A save from a newer version: no Play anyway; Back leaves the warning.
        let mut vm = ContinuePageVm::new(
            Some(fake_card(1000, "handheld", 1000, "3.0.2")),
            Some(fake_device("handheld", "3.0.2", "b1")),
        );
        vm.set_saved_by(fake_device("desktop", "3.0.3", "b2"));
        let view = vm.view();
        assert!(!view.warning.is_empty());
        assert!(!view.play_anyway);
        assert!(!vm.click(FocusTarget::Play));
        assert_eq!(vm.take_request(), None);
        assert!(!vm.click(FocusTarget::Back));
        assert_eq!(vm.state(), &ContinueState::Ready);
        assert_eq!(vm.view().warning, "");
    }

    #[test]
    fn a_failed_push_is_tried_again_without_the_game() {
        let mut vm = ContinuePageVm::new(Some(fake_card(100, "handheld", 1000, "3.0.2")), None);
        vm.home = PathBuf::from("/home/user");
        vm.handle(Nav::Accept);
        vm.take_request();
        vm.on_play_event(PlayEvent::Playing);
        vm.on_play_event(PlayEvent::Step(SyncStep::Push));
        vm.on_play_event(PlayEvent::Played(Err(Error::Io(
            "cannot read /home/user/island".to_string(),
        ))));
        assert!(matches!(vm.state(), ContinueState::PushFailed(_)));
        let view = vm.view();
        assert_eq!(view.status, "Not synced yet");
        assert_eq!(view.button, "Retry");
        // The home folder is not shown.
        assert!(view.message.contains("~/island"));
        assert!(!view.message.contains("/home/user"));

        assert!(vm.handle(Nav::Accept));
        assert_eq!(
            vm.take_request(),
            Some(Request::Sync {
                step: SyncStep::Push,
                keep: None
            })
        );
        assert_eq!(vm.state(), &ContinueState::Saving);
        vm.on_play_event(PlayEvent::Synced(Ok(done())));
        assert_eq!(vm.state(), &ContinueState::Ready);
    }

    #[test]
    fn errors_before_and_in_the_game_and_an_absent_relay_are_told_apart() {
        let mut vm = ContinuePageVm::new(Some(fake_card(100, "handheld", 1000, "3.0.2")), None);
        vm.handle(Nav::Accept);
        vm.on_play_event(PlayEvent::Step(SyncStep::Pull));
        vm.on_play_event(PlayEvent::Played(Err(Error::Saves(
            "bad manifest".to_string(),
        ))));
        assert!(matches!(vm.state(), ContinueState::RelayError(_)));
        assert_eq!(vm.play_action(), Some(PlayAction::Retry));

        vm.handle(Nav::Accept);
        assert_eq!(vm.take_request(), Some(Request::Play));
        vm.on_play_event(PlayEvent::Playing);
        vm.on_play_event(PlayEvent::Played(Err(Error::Io("no player".to_string()))));
        assert!(matches!(
            vm.state(),
            ContinueState::PlayerFailed { exit: -1, .. }
        ));

        vm.handle(Nav::Accept);
        vm.on_play_event(PlayEvent::Played(Ok(Played {
            pulled: done(),
            exit_code: 3,
            pushed: done(),
        })));
        assert!(matches!(
            vm.state(),
            ContinueState::PlayerFailed { exit: 3, .. }
        ));

        vm.handle(Nav::Accept);
        vm.on_play_event(PlayEvent::Played(Ok(Played {
            pulled: Sync::Offline,
            exit_code: 0,
            pushed: Sync::Offline,
        })));
        assert_eq!(vm.state(), &ContinueState::RelayUnavailable);
        assert_eq!(vm.view().status, "Will sync later");
        assert_eq!(vm.view().button, "Play");
    }

    #[test]
    fn card_from_text_reads_card_lines_or_takes_the_text_as_a_name() {
        let picture = Path::new("assets/acnhrecomp.png");
        assert_eq!(
            card_from_text(
                "device Strix Halo tablet\nplay 7200\nversion 3.0.2\nsaved 2000",
                picture
            ),
            fake_card(2000, "Strix Halo tablet", 7200, "3.0.2")
        );
        let named = card_from_text("Living room PC", picture);
        assert_eq!(named, fake_card(0, "Living room PC", 0, ""));
        let lines = card_lines(&named, String::new());
        assert_eq!(lines.saved, "Living room PC");
        assert_eq!(lines.version, "Version unknown");
        assert_eq!(
            card_lines(&card_from_text("", picture), String::new()).saved,
            "unknown device"
        );
    }
}
