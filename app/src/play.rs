//! Runs what the Continue page asks for (`vm::continue_page::Request`) and tells it what
//! happened (`PlayEvent`). No window here: `main.rs` owns a `Session`, sends it every press and
//! draws its view model.
//!
//! A conflict is never answered here. `launch::play` asks its `choose` for an answer and then
//! repeats the step with it; `Watched` refuses that repeat, so the flow stops before the game
//! and the page asks the player. The pick comes back as a `Request::Sync` with the `Keep`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;

use acnh_core::launch::{self, Game, Keep, Relay};
use acnh_core::savecard;
use acnh_core::{Error, NotBuilt, Result};

use crate::vm::continue_page::{ContinuePageVm, FocusTarget, PlayEvent, Request, SyncStep};
use crate::vm::Nav;

/// The relay as `launch::play` sees it: every step is reported, a conflict is kept for the
/// page, and a step that carries an answer is refused (the player has not given one).
struct Watched<'a> {
    inner: &'a dyn Relay,
    report: &'a dyn Fn(PlayEvent),
    conflict: RefCell<Option<PlayEvent>>,
}

impl Watched<'_> {
    fn step(
        &self,
        step: SyncStep,
        resolve: Option<Keep>,
        call: impl FnOnce() -> Result<launch::Sync>,
    ) -> Result<launch::Sync> {
        if resolve.is_some() {
            return Err(Error::Saves(
                "the island changed here and on the relay".to_string(),
            ));
        }
        (self.report)(PlayEvent::Step(step));
        let result = call()?;
        if let launch::Sync::Conflict { server, local } = &result {
            self.conflict.replace(Some(PlayEvent::Conflict {
                step,
                server: server.clone(),
                local: local.clone(),
            }));
        }
        Ok(result)
    }
}

impl Relay for Watched<'_> {
    fn pull(&self, save_dir: &Path, resolve: Option<Keep>) -> Result<launch::Sync> {
        self.step(SyncStep::Pull, resolve, || self.inner.pull(save_dir, None))
    }

    fn push(&self, save_dir: &Path, resolve: Option<Keep>) -> Result<launch::Sync> {
        self.step(SyncStep::Push, resolve, || self.inner.push(save_dir, None))
    }
}

/// The game, reported as it starts.
struct Announced<'a> {
    inner: &'a dyn Game,
    report: &'a dyn Fn(PlayEvent),
}

impl Game for Announced<'_> {
    fn run(&self, env: &[(String, String)]) -> Result<i32> {
        (self.report)(PlayEvent::Playing);
        self.inner.run(env)
    }
}

/// `Request::Play`: `launch::play`, each step reported as it begins. Ends with
/// `PlayEvent::Played`, or with `PlayEvent::Conflict` when the relay found one: then the flow
/// has stopped there (before the game, for a conflict at the pull) and the relay was given no
/// answer.
pub fn play(
    relay: &dyn Relay,
    game: &dyn Game,
    save_dir: &Path,
    env: &[(String, String)],
    report: &dyn Fn(PlayEvent),
) {
    let watched = Watched {
        inner: relay,
        report,
        conflict: RefCell::new(None),
    };
    let announced = Announced {
        inner: game,
        report,
    };
    // `launch::play` must be handed an answer to go on after a conflict. This one is never
    // sent: `Watched` refuses the step that would carry it, which ends the flow.
    let unsent = |_: &launch::Sync| Keep::Local;
    let result = launch::play(&watched, &announced, save_dir, env, &unsent);
    match watched.conflict.take() {
        Some(conflict) => report(conflict),
        None => report(PlayEvent::Played(result)),
    }
}

/// `Request::Sync`: one relay step. With `keep`, the player's answer to a conflict: keeping
/// the relay's island replaces the one on this device, so `backup` copies that one away first,
/// and a backup that fails stops before the relay is called. Without `keep`, a push tried
/// again; a conflict found then goes to the page as `PlayEvent::Conflict`. Everything else
/// ends with `PlayEvent::Synced`.
pub fn sync(
    relay: &dyn Relay,
    save_dir: &Path,
    step: SyncStep,
    keep: Option<Keep>,
    backup: &dyn Fn(&Path) -> Result<()>,
    report: &dyn Fn(PlayEvent),
) {
    let run = || -> Result<launch::Sync> {
        if keep == Some(Keep::Server) {
            report(PlayEvent::Step(SyncStep::Backup));
            backup(save_dir)?;
        }
        report(PlayEvent::Step(step));
        match step {
            SyncStep::Push => relay.push(save_dir, keep),
            SyncStep::Pull | SyncStep::Backup => relay.pull(save_dir, keep),
        }
    };
    match run() {
        Ok(launch::Sync::Conflict { server, local }) if keep.is_none() => {
            report(PlayEvent::Conflict {
                step,
                server,
                local,
            });
        }
        result => report(PlayEvent::Synced(result)),
    }
}

/// Copies the island on this device into Backups before it is replaced.
pub type Backup = dyn Fn(&Path) -> Result<()> + Send + Sync;

/// What the jobs run against on this device.
#[derive(Clone)]
pub struct Runner {
    pub relay: Arc<dyn Relay + Send + Sync>,
    pub game: Arc<dyn Game + Send + Sync>,
    pub save_dir: PathBuf,
    /// Added to the game's environment (`NX_DATA_ROOT`).
    pub env: Vec<(String, String)>,
    pub backup: Arc<Backup>,
}

/// The Continue page joined to the play flow: presses go into the view model, what it asks
/// for runs on a thread of its own (the game runs for hours; the window must stay live), and
/// the results come back through `pump`.
pub struct Session {
    vm: ContinuePageVm,
    runner: Runner,
    picture: PathBuf,
    sender: Sender<PlayEvent>,
    events: Receiver<PlayEvent>,
}

impl Session {
    /// `picture` is the island card's picture (`savecard::read_card`).
    pub fn new(vm: ContinuePageVm, runner: Runner, picture: PathBuf) -> Self {
        let (sender, events) = mpsc::channel();
        Self {
            vm,
            runner,
            picture,
            sender,
            events,
        }
    }

    /// Use the locally prepared player and game root for subsequent launches.
    pub fn set_prepared_player(&mut self, program: PathBuf, root: &Path) {
        self.runner.game = Arc::new(Player { program });
        self.runner.env.retain(|(name, _)| name != "NX_GAME_ROOT");
        self.runner
            .env
            .push(("NX_GAME_ROOT".into(), root.display().to_string()));
    }

    pub fn vm(&self) -> &ContinuePageVm {
        &self.vm
    }

    /// A controller or keyboard press. True if it committed an action.
    pub fn nav(&mut self, nav: Nav) -> bool {
        let committed = self.vm.handle(nav);
        self.dispatch();
        committed
    }

    /// A click or tap on a button. True if it committed an action.
    pub fn click(&mut self, target: FocusTarget) -> bool {
        let committed = self.vm.click(target);
        self.dispatch();
        committed
    }

    /// Take in what the jobs reported since the last call. True when there was anything, so
    /// the page is drawn again.
    pub fn pump(&mut self) -> bool {
        let events: Vec<PlayEvent> = self.events.try_iter().collect();
        let any = !events.is_empty();
        for event in events {
            let finished = matches!(event, PlayEvent::Played(_) | PlayEvent::Synced(_));
            self.vm.on_play_event(event);
            if finished {
                // The game or the relay may have written the island: show it as it is now.
                if let Ok(card) = savecard::read_card(&self.runner.save_dir, &self.picture) {
                    self.vm.set_local_card(Some(card));
                }
            }
        }
        self.dispatch();
        any
    }

    /// Start the job the view model asked for, if any.
    fn dispatch(&mut self) {
        let Some(request) = self.vm.take_request() else {
            return;
        };
        let runner = self.runner.clone();
        let sender = self.sender.clone();
        // A send fails only when the session is gone; then nobody is left to tell.
        let report = move |event: PlayEvent| {
            let _ = sender.send(event);
        };
        match request {
            Request::Play => {
                thread::spawn(move || {
                    play(
                        &*runner.relay,
                        &*runner.game,
                        &runner.save_dir,
                        &runner.env,
                        &report,
                    );
                });
            }
            Request::Sync { step, keep } => {
                thread::spawn(move || {
                    sync(
                        &*runner.relay,
                        &runner.save_dir,
                        step,
                        keep,
                        &*runner.backup,
                        &report,
                    );
                });
            }
            // The update flow is joined to the app with the Fleet page; until then the page
            // says so instead of waiting.
            Request::Update => self
                .vm
                .on_play_event(PlayEvent::Updated(Err(Error::NotBuilt(NotBuilt(
                    "update from the Continue page",
                ))))),
        }
    }
}

/// No relay is set up on this device: every step is `Offline`, which plays on with the island
/// here (`launch::play`). `main.rs` uses it until the save-relay adapter is joined to the app.
pub struct RelayOff;

impl Relay for RelayOff {
    fn pull(&self, _save_dir: &Path, _resolve: Option<Keep>) -> Result<launch::Sync> {
        Ok(launch::Sync::Offline)
    }

    fn push(&self, _save_dir: &Path, _resolve: Option<Keep>) -> Result<launch::Sync> {
        Ok(launch::Sync::Offline)
    }
}

/// The built player on this device, run as a child process until it quits.
pub struct Player {
    pub program: PathBuf,
}

impl Game for Player {
    fn run(&self, env: &[(String, String)]) -> Result<i32> {
        let status = Command::new(&self.program)
            .envs(env.iter().map(|(name, value)| (name, value)))
            .status()
            .map_err(|error| {
                Error::Io(format!("cannot start {}: {error}", self.program.display()))
            })?;
        // No exit code means a signal ended it: not a clean quit.
        Ok(status.code().unwrap_or(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::continue_page::{ContinueState, Side};
    use acnh_core::launch::Played;
    use acnh_core::savecard::SaveCard;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    type Log = Arc<Mutex<Vec<String>>>;

    fn note(log: &Log, line: String) {
        log.lock().unwrap().push(line);
    }

    fn lines(log: &Log) -> Vec<String> {
        log.lock().unwrap().clone()
    }

    /// A relay that writes down every call. A step without an answer finds a conflict at
    /// `conflict_at` until an answer has been given.
    struct FakeRelay {
        log: Log,
        conflict_at: Option<SyncStep>,
        answered: AtomicBool,
    }

    impl FakeRelay {
        fn new(log: &Log, conflict_at: Option<SyncStep>) -> Self {
            Self {
                log: Arc::clone(log),
                conflict_at,
                answered: AtomicBool::new(false),
            }
        }

        fn step(&self, name: &str, step: SyncStep, resolve: Option<Keep>) -> Result<launch::Sync> {
            note(&self.log, format!("{name} {resolve:?}"));
            if resolve.is_some() {
                self.answered.store(true, Ordering::SeqCst);
                return Ok(launch::Sync::Done(format!("kept {resolve:?}")));
            }
            if self.conflict_at == Some(step) && !self.answered.load(Ordering::SeqCst) {
                return Ok(conflict());
            }
            Ok(launch::Sync::Done(name.to_string()))
        }
    }

    impl Relay for FakeRelay {
        fn pull(&self, _save_dir: &Path, resolve: Option<Keep>) -> Result<launch::Sync> {
            self.step("pull", SyncStep::Pull, resolve)
        }

        fn push(&self, _save_dir: &Path, resolve: Option<Keep>) -> Result<launch::Sync> {
            self.step("push", SyncStep::Push, resolve)
        }
    }

    struct FakeGame {
        log: Log,
    }

    impl Game for FakeGame {
        fn run(&self, env: &[(String, String)]) -> Result<i32> {
            note(&self.log, format!("game {}", env[0].1));
            Ok(0)
        }
    }

    fn conflict() -> launch::Sync {
        launch::Sync::Conflict {
            server: "device Strix Halo tablet\nplay 7200\nversion 3.0.2\nsaved 2000".to_string(),
            local: "device AMD handheld".to_string(),
        }
    }

    fn conflict_event(step: SyncStep) -> PlayEvent {
        PlayEvent::Conflict {
            step,
            server: "device Strix Halo tablet\nplay 7200\nversion 3.0.2\nsaved 2000".to_string(),
            local: "device AMD handheld".to_string(),
        }
    }

    fn env() -> Vec<(String, String)> {
        vec![("NX_DATA_ROOT".to_string(), "/home/user/root".to_string())]
    }

    fn save_dir() -> PathBuf {
        PathBuf::from("/home/user/root/no-island-here")
    }

    /// Run `play` here and return what it reported.
    fn played(relay: &FakeRelay, log: &Log) -> Vec<PlayEvent> {
        let events = RefCell::new(Vec::new());
        let game = FakeGame {
            log: Arc::clone(log),
        };
        play(relay, &game, &save_dir(), &env(), &|event| {
            events.borrow_mut().push(event)
        });
        events.into_inner()
    }

    /// Run `sync` here with a backup that writes itself down (and fails when told to).
    fn synced(
        relay: &FakeRelay,
        log: &Log,
        step: SyncStep,
        keep: Option<Keep>,
        backup_fails: bool,
    ) -> Vec<PlayEvent> {
        let events = RefCell::new(Vec::new());
        let backup = |_: &Path| {
            note(log, "backup".to_string());
            if backup_fails {
                Err(Error::Saves("disk full".to_string()))
            } else {
                Ok(())
            }
        };
        sync(relay, &save_dir(), step, keep, &backup, &|event| {
            events.borrow_mut().push(event)
        });
        events.into_inner()
    }

    #[test]
    fn play_pulls_runs_the_game_and_pushes_reporting_each_step() {
        let log = Log::default();
        let events = played(&FakeRelay::new(&log, None), &log);
        assert_eq!(
            lines(&log),
            ["pull None", "game /home/user/root", "push None"]
        );
        assert_eq!(
            events,
            [
                PlayEvent::Step(SyncStep::Pull),
                PlayEvent::Playing,
                PlayEvent::Step(SyncStep::Push),
                PlayEvent::Played(Ok(Played {
                    pulled: launch::Sync::Done("pull".to_string()),
                    exit_code: 0,
                    pushed: launch::Sync::Done("push".to_string()),
                })),
            ]
        );
    }

    #[test]
    fn a_conflict_at_the_pull_stops_before_the_game_and_sends_no_answer() {
        let log = Log::default();
        let events = played(&FakeRelay::new(&log, Some(SyncStep::Pull)), &log);
        // The relay was asked once, without an answer; the game did not start.
        assert_eq!(lines(&log), ["pull None"]);
        assert_eq!(
            events,
            [
                PlayEvent::Step(SyncStep::Pull),
                conflict_event(SyncStep::Pull)
            ]
        );
    }

    #[test]
    fn a_conflict_at_the_push_is_handed_to_the_page_unanswered() {
        let log = Log::default();
        let events = played(&FakeRelay::new(&log, Some(SyncStep::Push)), &log);
        assert_eq!(
            lines(&log),
            ["pull None", "game /home/user/root", "push None"]
        );
        assert_eq!(events.last(), Some(&conflict_event(SyncStep::Push)));
    }

    #[test]
    fn a_relay_error_at_the_pull_ends_the_flow_without_the_game() {
        struct Broken;
        impl Relay for Broken {
            fn pull(&self, _: &Path, _: Option<Keep>) -> Result<launch::Sync> {
                Err(Error::Saves("bad manifest".to_string()))
            }
            fn push(&self, _: &Path, _: Option<Keep>) -> Result<launch::Sync> {
                Err(Error::Saves("bad manifest".to_string()))
            }
        }
        let log = Log::default();
        let events = RefCell::new(Vec::new());
        let game = FakeGame {
            log: Arc::clone(&log),
        };
        play(&Broken, &game, &save_dir(), &env(), &|event| {
            events.borrow_mut().push(event)
        });
        assert!(lines(&log).is_empty());
        assert_eq!(
            events.into_inner(),
            [
                PlayEvent::Step(SyncStep::Pull),
                PlayEvent::Played(Err(Error::Saves("bad manifest".to_string()))),
            ]
        );
    }

    #[test]
    fn keeping_the_relays_island_backs_up_first_then_sends_keep_server() {
        let log = Log::default();
        let relay = FakeRelay::new(&log, Some(SyncStep::Pull));
        let events = synced(&relay, &log, SyncStep::Pull, Some(Keep::Server), false);
        assert_eq!(lines(&log), ["backup", "pull Some(Server)"]);
        assert_eq!(
            events,
            [
                PlayEvent::Step(SyncStep::Backup),
                PlayEvent::Step(SyncStep::Pull),
                PlayEvent::Synced(Ok(launch::Sync::Done("kept Some(Server)".to_string()))),
            ]
        );
    }

    #[test]
    fn a_failed_backup_stops_before_the_relay_overwrites_anything() {
        let log = Log::default();
        let relay = FakeRelay::new(&log, Some(SyncStep::Push));
        let events = synced(&relay, &log, SyncStep::Push, Some(Keep::Server), true);
        assert_eq!(lines(&log), ["backup"]);
        assert_eq!(
            events.last(),
            Some(&PlayEvent::Synced(Err(Error::Saves(
                "disk full".to_string()
            ))))
        );
    }

    #[test]
    fn keeping_this_devices_island_sends_keep_local_at_the_step_asked() {
        let log = Log::default();
        let relay = FakeRelay::new(&log, Some(SyncStep::Push));
        let events = synced(&relay, &log, SyncStep::Push, Some(Keep::Local), false);
        assert_eq!(lines(&log), ["push Some(Local)"]);
        assert_eq!(
            events,
            [
                PlayEvent::Step(SyncStep::Push),
                PlayEvent::Synced(Ok(launch::Sync::Done("kept Some(Local)".to_string()))),
            ]
        );
    }

    #[test]
    fn a_push_tried_again_that_finds_a_conflict_asks_the_page() {
        let log = Log::default();
        let relay = FakeRelay::new(&log, Some(SyncStep::Push));
        let events = synced(&relay, &log, SyncStep::Push, None, false);
        assert_eq!(lines(&log), ["push None"]);
        assert_eq!(events.last(), Some(&conflict_event(SyncStep::Push)));
    }

    #[test]
    fn relay_off_plays_on_with_the_island_here() {
        let log = Log::default();
        let events = RefCell::new(Vec::new());
        let game = FakeGame {
            log: Arc::clone(&log),
        };
        play(&RelayOff, &game, &save_dir(), &env(), &|event| {
            events.borrow_mut().push(event)
        });
        assert_eq!(lines(&log), ["game /home/user/root"]);
        assert_eq!(
            events.into_inner().last(),
            Some(&PlayEvent::Played(Ok(Played {
                pulled: launch::Sync::Offline,
                exit_code: 0,
                pushed: launch::Sync::Offline,
            })))
        );
    }

    #[test]
    fn a_player_that_is_not_there_is_an_error_not_a_crash() {
        let player = Player {
            program: PathBuf::from("/home/user/install/no-such-player"),
        };
        assert!(matches!(player.run(&env()), Err(Error::Io(_))));
    }

    fn session(log: &Log, conflict_at: Option<SyncStep>) -> Session {
        let card = SaveCard {
            saved_unix: 1000,
            device: "AMD handheld".to_string(),
            play_seconds: 3600,
            version: "3.0.2".to_string(),
            picture: PathBuf::from("assets/acnhrecomp.png"),
        };
        let backup_log = Arc::clone(log);
        let runner = Runner {
            relay: Arc::new(FakeRelay::new(log, conflict_at)),
            game: Arc::new(FakeGame {
                log: Arc::clone(log),
            }),
            save_dir: save_dir(),
            env: env(),
            backup: Arc::new(move |_: &Path| {
                note(&backup_log, "backup".to_string());
                Ok(())
            }),
        };
        Session::new(
            ContinuePageVm::new(Some(card), None),
            runner,
            PathBuf::from("assets/acnhrecomp.png"),
        )
    }

    /// Pump until the page is in `state`, as the window's timer does.
    fn wait_for(session: &mut Session, state: &ContinueState) {
        let started = Instant::now();
        while session.vm().state() != state {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "still {:?}",
                session.vm().state()
            );
            session.pump();
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn one_press_on_the_page_runs_the_whole_flow() {
        let log = Log::default();
        let mut session = session(&log, None);
        assert!(session.nav(Nav::Accept));
        assert!(matches!(
            session.vm().state(),
            ContinueState::Syncing(_) | ContinueState::Playing | ContinueState::Saving
        ));
        wait_for(&mut session, &ContinueState::Ready);
        assert_eq!(
            lines(&log),
            ["pull None", "game /home/user/root", "push None"]
        );
    }

    #[test]
    fn a_relay_conflict_reaches_the_page_and_the_pick_reaches_the_relay() {
        let log = Log::default();
        let mut session = session(&log, Some(SyncStep::Pull));
        assert!(session.click(FocusTarget::Play));
        wait_for(&mut session, &ContinueState::Conflict);
        // Asked once, no answer sent, game not started: the page holds both cards.
        assert_eq!(lines(&log), ["pull None"]);
        let conflict = session
            .vm()
            .conflict()
            .expect("the conflict is on the page");
        assert_eq!(conflict.this_card.device, "AMD handheld");
        assert_eq!(conflict.relay_card.device, "Strix Halo tablet");
        assert_eq!(conflict.newer, Side::Relay);

        // Open the chooser (focus on the newer, the relay's) and keep that one.
        assert!(!session.nav(Nav::Accept));
        assert_eq!(lines(&log), ["pull None"]);
        assert!(session.nav(Nav::Accept));
        wait_for(&mut session, &ContinueState::Ready);
        assert_eq!(lines(&log), ["pull None", "backup", "pull Some(Server)"]);
        assert_eq!(
            session.vm().status_message(),
            Some("Kept the one from Strix Halo tablet. The other is in Backups.")
        );
        assert_eq!(
            session.vm().local_card().map(|card| card.device.as_str()),
            Some("Strix Halo tablet")
        );

        // Play is one more press, and now the flow runs through.
        assert!(session.nav(Nav::Accept));
        wait_for(&mut session, &ContinueState::Ready);
        assert_eq!(
            lines(&log)[3..],
            ["pull None", "game /home/user/root", "push None"]
        );
    }

    #[test]
    fn keeping_this_one_from_the_page_sends_keep_local_and_no_backup() {
        let log = Log::default();
        let mut session = session(&log, Some(SyncStep::Pull));
        session.nav(Nav::Accept);
        wait_for(&mut session, &ContinueState::Conflict);
        session.nav(Nav::Accept);
        assert!(session.click(FocusTarget::Keep(Side::This)));
        wait_for(&mut session, &ContinueState::Ready);
        assert_eq!(lines(&log), ["pull None", "pull Some(Local)"]);
        assert_eq!(
            session.vm().status_message(),
            Some("Kept the one from AMD handheld. The other is in Backups.")
        );
    }

    #[test]
    fn update_from_the_page_says_it_is_not_built_instead_of_waiting() {
        let log = Log::default();
        let mut session = session(&log, None);
        session.vm.this_device = None;
        session.vm.state = ContinueState::Drift {
            newer_save_refused: true,
        };
        session.vm.focus = FocusTarget::Update;
        assert!(session.click(FocusTarget::Update));
        assert!(matches!(
            session.vm().state(),
            ContinueState::UpdateFailed(_)
        ));
        assert!(lines(&log).is_empty());
    }
}
