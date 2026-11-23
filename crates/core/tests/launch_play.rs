// Tests for launch::play (issue 1.24; SPEC.md M6, M10): pull, run, push with fakes.
use acnh_core::launch::{play, Game, Keep, Played, Relay, Sync};
use acnh_core::savecard::SaveCard;
use acnh_core::{Error, Result};

fn card(device: &str) -> SaveCard {
    SaveCard {
        saved_unix: 0,
        device: device.into(),
        play_seconds: 0,
        version: String::new(),
        picture: PathBuf::new(),
    }
}
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    Pull { dir: PathBuf, resolve: Option<Keep> },
    Run { env: Vec<(String, String)> },
    Push { dir: PathBuf, resolve: Option<Keep> },
}

struct FakeRelay {
    history: Rc<RefCell<Vec<Action>>>,
    pull_responses: RefCell<Vec<Result<Sync>>>,
    push_responses: RefCell<Vec<Result<Sync>>>,
}

impl FakeRelay {
    fn new(history: Rc<RefCell<Vec<Action>>>) -> Self {
        Self {
            history,
            pull_responses: RefCell::new(Vec::new()),
            push_responses: RefCell::new(Vec::new()),
        }
    }

    fn with_pull(self, resp: Result<Sync>) -> Self {
        self.pull_responses.borrow_mut().push(resp);
        self
    }

    fn with_push(self, resp: Result<Sync>) -> Self {
        self.push_responses.borrow_mut().push(resp);
        self
    }
}

impl Relay for FakeRelay {
    fn pull(&self, save_dir: &Path, resolve: Option<Keep>) -> Result<Sync> {
        self.history.borrow_mut().push(Action::Pull {
            dir: save_dir.to_path_buf(),
            resolve,
        });
        if self.pull_responses.borrow().is_empty() {
            Ok(Sync::Done("default pull".into()))
        } else {
            self.pull_responses.borrow_mut().remove(0)
        }
    }

    fn push(&self, save_dir: &Path, resolve: Option<Keep>) -> Result<Sync> {
        self.history.borrow_mut().push(Action::Push {
            dir: save_dir.to_path_buf(),
            resolve,
        });
        if self.push_responses.borrow().is_empty() {
            Ok(Sync::Done("default push".into()))
        } else {
            self.push_responses.borrow_mut().remove(0)
        }
    }
}

struct FakeGame {
    history: Rc<RefCell<Vec<Action>>>,
    exit_code: Result<i32>,
}

impl FakeGame {
    fn new(history: Rc<RefCell<Vec<Action>>>, exit_code: Result<i32>) -> Self {
        Self { history, exit_code }
    }
}

impl Game for FakeGame {
    fn run(&self, env: &[(String, String)]) -> Result<i32> {
        self.history
            .borrow_mut()
            .push(Action::Run { env: env.to_vec() });
        self.exit_code.clone()
    }
}

#[test]
fn clean_pull_run_push_in_that_order() {
    let history = Rc::new(RefCell::new(Vec::new()));
    let relay = FakeRelay::new(Rc::clone(&history))
        .with_pull(Ok(Sync::Done("pulled 12 files".into())))
        .with_push(Ok(Sync::Done("pushed 12 files".into())));
    let game = FakeGame::new(Rc::clone(&history), Ok(0));
    let save_dir = Path::new("/saves/island");
    let env = [("NX_DATA_ROOT".into(), "/d/root".into())];

    let played = play(&relay, &game, save_dir, &env, &|_| {
        panic!("choose called unexpectedly")
    })
    .unwrap();

    assert_eq!(
        played,
        Played {
            pulled: Sync::Done("pulled 12 files".into()),
            exit_code: 0,
            pushed: Sync::Done("pushed 12 files".into()),
        }
    );

    assert_eq!(
        *history.borrow(),
        vec![
            Action::Pull {
                dir: PathBuf::from("/saves/island"),
                resolve: None,
            },
            Action::Run {
                env: vec![("NX_DATA_ROOT".into(), "/d/root".into())],
            },
            Action::Push {
                dir: PathBuf::from("/saves/island"),
                resolve: None,
            },
        ]
    );
}

#[test]
fn pull_conflict_calls_choose_once_with_both_cards_and_pulls_with_answer() {
    let history = Rc::new(RefCell::new(Vec::new()));
    let server_card = "server-sidecar-or-card";
    let local_card = "local-sidecar-or-card";
    let relay = FakeRelay::new(Rc::clone(&history))
        .with_pull(Ok(Sync::Conflict {
            server: card(server_card),
            local: card(local_card),
            backup: None,
        }))
        .with_pull(Ok(Sync::Done("pulled server card".into())))
        .with_push(Ok(Sync::Done("pushed".into())));
    let game = FakeGame::new(Rc::clone(&history), Ok(0));
    let save_dir = Path::new("/saves/island");

    let choices = RefCell::new(Vec::new());
    let choose = |sync: &Sync| {
        choices.borrow_mut().push(sync.clone());
        Keep::Server
    };

    let played = play(&relay, &game, save_dir, &[], &choose).unwrap();

    assert_eq!(
        *choices.borrow(),
        vec![Sync::Conflict {
            server: card(server_card),
            local: card(local_card),
            backup: None,
        }]
    );
    assert_eq!(played.pulled, Sync::Done("pulled server card".into()));
    assert_eq!(
        *history.borrow(),
        vec![
            Action::Pull {
                dir: PathBuf::from("/saves/island"),
                resolve: None,
            },
            Action::Pull {
                dir: PathBuf::from("/saves/island"),
                resolve: Some(Keep::Server),
            },
            Action::Run { env: vec![] },
            Action::Push {
                dir: PathBuf::from("/saves/island"),
                resolve: None,
            },
        ]
    );
}

#[test]
fn offline_refuses_to_start_and_off_plays() {
    let history = Rc::new(RefCell::new(Vec::new()));
    let relay = FakeRelay::new(Rc::clone(&history)).with_pull(Ok(Sync::Offline));
    let game = FakeGame::new(Rc::clone(&history), Ok(0));
    assert_eq!(
        play(&relay, &game, Path::new("/saves/island"), &[], &|_| {
            Keep::Local
        })
        .unwrap_err()
        .code(),
        "relay_unreachable"
    );
    assert_eq!(history.borrow().len(), 1);
    let relay = FakeRelay::new(Rc::clone(&history))
        .with_pull(Ok(Sync::Off))
        .with_push(Ok(Sync::Offline));
    let played = play(&relay, &game, Path::new("/saves/island"), &[], &|_| {
        Keep::Local
    })
    .unwrap();
    assert_eq!(played.pulled, Sync::Off);
    assert_eq!(played.pushed, Sync::Offline);
}

#[test]
fn pull_error_stops_before_the_game() {
    let history = Rc::new(RefCell::new(Vec::new()));
    let relay = FakeRelay::new(Rc::clone(&history))
        .with_pull(Err(Error::Saves("relay pull unreachable".into())));
    let game = FakeGame::new(Rc::clone(&history), Ok(0));
    let save_dir = Path::new("/saves/island");

    let err = play(&relay, &game, save_dir, &[], &|_| Keep::Server).unwrap_err();
    assert_eq!(err, Error::Saves("relay pull unreachable".into()));

    assert_eq!(
        *history.borrow(),
        vec![Action::Pull {
            dir: PathBuf::from("/saves/island"),
            resolve: None,
        }]
    );
}

#[test]
fn pull_conflict_error_on_resolve_stops_before_the_game() {
    let history = Rc::new(RefCell::new(Vec::new()));
    let relay = FakeRelay::new(Rc::clone(&history))
        .with_pull(Ok(Sync::Conflict {
            server: card("s"),
            local: card("l"),
            backup: None,
        }))
        .with_pull(Err(Error::Saves("resolve failed".into())));
    let game = FakeGame::new(Rc::clone(&history), Ok(0));
    let save_dir = Path::new("/saves/island");

    let err = play(&relay, &game, save_dir, &[], &|_| Keep::Server).unwrap_err();
    assert_eq!(err, Error::Saves("resolve failed".into()));

    assert_eq!(
        *history.borrow(),
        vec![
            Action::Pull {
                dir: PathBuf::from("/saves/island"),
                resolve: None,
            },
            Action::Pull {
                dir: PathBuf::from("/saves/island"),
                resolve: Some(Keep::Server),
            },
        ]
    );
}

#[test]
fn push_error_comes_back_after_the_game_ran() {
    let history = Rc::new(RefCell::new(Vec::new()));
    let relay = FakeRelay::new(Rc::clone(&history))
        .with_pull(Ok(Sync::Done("ok".into())))
        .with_push(Err(Error::Saves("push connection lost".into())));
    let game = FakeGame::new(Rc::clone(&history), Ok(0));
    let save_dir = Path::new("/saves/island");

    let err = play(&relay, &game, save_dir, &[], &|_| Keep::Server).unwrap_err();
    assert_eq!(err, Error::Saves("push connection lost".into()));

    assert_eq!(
        *history.borrow(),
        vec![
            Action::Pull {
                dir: PathBuf::from("/saves/island"),
                resolve: None,
            },
            Action::Run { env: vec![] },
            Action::Push {
                dir: PathBuf::from("/saves/island"),
                resolve: None,
            },
        ]
    );
}

#[test]
fn environment_reaches_game_run_unchanged() {
    let history = Rc::new(RefCell::new(Vec::new()));
    let relay = FakeRelay::new(Rc::clone(&history))
        .with_pull(Ok(Sync::Done("ok".into())))
        .with_push(Ok(Sync::Done("ok".into())));
    let game = FakeGame::new(Rc::clone(&history), Ok(0));
    let save_dir = Path::new("/saves/island");

    let custom_env = vec![
        ("NX_DATA_ROOT".into(), "/tmp/data_root".into()),
        ("ACNH_DEVICE_PROFILE".into(), "amd-handheld".into()),
        ("ACNH_FPS".into(), "30".into()),
    ];

    play(&relay, &game, save_dir, &custom_env, &|_| Keep::Server).unwrap();

    assert_eq!(
        *history.borrow(),
        vec![
            Action::Pull {
                dir: PathBuf::from("/saves/island"),
                resolve: None,
            },
            Action::Run { env: custom_env },
            Action::Push {
                dir: PathBuf::from("/saves/island"),
                resolve: None,
            },
        ]
    );
}

#[test]
fn push_conflict_calls_choose_once_and_pushes_with_answer() {
    let history = Rc::new(RefCell::new(Vec::new()));
    let relay = FakeRelay::new(Rc::clone(&history))
        .with_pull(Ok(Sync::Done("pulled".into())))
        .with_push(Ok(Sync::Conflict {
            server: card("server_card"),
            local: card("local_card"),
            backup: None,
        }))
        .with_push(Ok(Sync::Done("pushed resolved".into())));
    let game = FakeGame::new(Rc::clone(&history), Ok(0));
    let save_dir = Path::new("/saves/island");

    let choices = RefCell::new(Vec::new());
    let choose = |sync: &Sync| {
        choices.borrow_mut().push(sync.clone());
        Keep::Local
    };

    let played = play(&relay, &game, save_dir, &[], &choose).unwrap();

    assert_eq!(
        *choices.borrow(),
        vec![Sync::Conflict {
            server: card("server_card"),
            local: card("local_card"),
            backup: None,
        }]
    );
    assert_eq!(played.pushed, Sync::Done("pushed resolved".into()));
    assert_eq!(
        *history.borrow(),
        vec![
            Action::Pull {
                dir: PathBuf::from("/saves/island"),
                resolve: None,
            },
            Action::Run { env: vec![] },
            Action::Push {
                dir: PathBuf::from("/saves/island"),
                resolve: None,
            },
            Action::Push {
                dir: PathBuf::from("/saves/island"),
                resolve: Some(Keep::Local),
            },
        ]
    );
}

#[test]
fn game_error_stops_before_push() {
    let history = Rc::new(RefCell::new(Vec::new()));
    let relay = FakeRelay::new(Rc::clone(&history)).with_pull(Ok(Sync::Done("ok".into())));
    let game = FakeGame::new(
        Rc::clone(&history),
        Err(Error::Unsupported("binary missing".into())),
    );
    let save_dir = Path::new("/saves/island");

    let err = play(&relay, &game, save_dir, &[], &|_| Keep::Server).unwrap_err();
    assert_eq!(err, Error::Unsupported("binary missing".into()));

    assert_eq!(
        *history.borrow(),
        vec![
            Action::Pull {
                dir: PathBuf::from("/saves/island"),
                resolve: None,
            },
            Action::Run { env: vec![] },
        ]
    );
}
