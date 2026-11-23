//! Setting up save sync (the brief's `RelaySetup`), shared by the guided first run (step 5) and
//! Settings' "Sign in again". Nextcloud takes a server, a user name and an app password, each
//! typed on the on-screen keyboard (`text_entry`). Google Drive and Dropbox sign in through the
//! browser on a desktop; in Game Mode the page shows a code and a URL for a phone and waits.
//!
//! The model never reaches the network: it asks for work with a `Request` and takes the
//! answer as an `Event`, so every state is tested with a fake setup. The app password is held
//! only until the request that carries it is taken, and is in no field afterwards.

use crate::vm::text_entry::{self, TextEntry};
use crate::vm::Nav;
use std::fmt;

/// A service the island can be kept on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    Nextcloud,
    GoogleDrive,
    Dropbox,
}

impl Service {
    pub const ALL: [Service; 3] = [Service::Nextcloud, Service::GoogleDrive, Service::Dropbox];

    pub fn label(self) -> &'static str {
        match self {
            Service::Nextcloud => "Nextcloud",
            Service::GoogleDrive => "Google Drive",
            Service::Dropbox => "Dropbox",
        }
    }
}

/// The account save sync is set up with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub service: Service,
    /// The Nextcloud user, or the e-mail address Google Drive or Dropbox reports.
    pub name: String,
    /// The Nextcloud server address; none for Google Drive and Dropbox.
    pub address: Option<String>,
}

impl Account {
    /// "Nextcloud, alex at https://cloud.example.org" or "Dropbox, name@example.org".
    pub fn summary(&self) -> String {
        match &self.address {
            Some(address) => format!("{}, {} at {address}", self.service.label(), self.name),
            None => format!("{}, {}", self.service.label(), self.name),
        }
    }
}

/// What a started Google Drive or Dropbox sign-in asks the person to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    /// The page to open (in the browser here, or on a phone).
    pub url: String,
    /// Google Drive in Game Mode: the code to enter on that page.
    pub code: Option<String>,
    /// Dropbox in Game Mode: the person types back the code Dropbox shows.
    pub needs_code: bool,
    /// The browser on this device was opened and will return on its own.
    pub browser: bool,
}

/// Work the page asks for. It carries the password, so it never prints it.
#[derive(Clone, PartialEq, Eq)]
pub enum Request {
    Nextcloud {
        server: String,
        user: String,
        password: String,
    },
    StartSignIn(Service),
    /// Wait for the sign-in to finish; `answer` is the code Dropbox shows in Game Mode, else
    /// empty.
    FinishSignIn(Service, String),
    /// Stop waiting; whatever finishes later is ignored.
    Cancel,
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Request::Nextcloud { server, user, .. } => f
                .debug_struct("Nextcloud")
                .field("server", server)
                .field("user", user)
                .field("password", &"<hidden>")
                .finish(),
            Request::StartSignIn(service) => f.debug_tuple("StartSignIn").field(service).finish(),
            Request::FinishSignIn(service, _) => {
                f.debug_tuple("FinishSignIn").field(service).finish()
            }
            Request::Cancel => f.write_str("Cancel"),
        }
    }
}

impl Request {
    /// The same request with the secret left out, for bookkeeping that outlives the call.
    pub fn without_secret(&self) -> Request {
        match self {
            Request::Nextcloud { server, user, .. } => Request::Nextcloud {
                server: server.clone(),
                user: user.clone(),
                password: String::new(),
            },
            Request::FinishSignIn(service, _) => Request::FinishSignIn(*service, String::new()),
            other => other.clone(),
        }
    }
}

/// What the work answered.
#[derive(Clone, Debug)]
pub enum Event {
    Connected(Result<Account, String>),
    SignInStarted(Result<Prompt, String>),
    SignedIn(Result<Account, String>),
}

/// How a key press or an event leaves the setup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Still on the setup pages.
    Open,
    /// Save sync is set up.
    Done(Account),
    /// Skip (first run) or Cancel (Settings): nothing was set up.
    Skipped,
    /// Back from the first page: the previous step of whoever hosts the setup.
    Back,
}

/// A text field of the Nextcloud form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Server,
    User,
    Password,
}

impl Field {
    fn label(self) -> &'static str {
        match self {
            Field::Server => "Server address",
            Field::User => "User name",
            Field::Password => "App password",
        }
    }
}

/// What work is under way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Work {
    Connecting,
    StartingSignIn,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Choose,
    Form,
    Editing(Field),
    Working(Work),
    /// A Google Drive or Dropbox sign-in is under way and the page waits for it.
    Waiting,
    /// Typing the code Dropbox shows (Game Mode).
    EnteringCode,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct RelaySetupVm {
    pub service: Option<Service>,
    pub phase: Phase,
    pub focus: usize,
    /// A line under the title (a plain error, or what to do next).
    pub message: String,
    server: String,
    user: String,
    password: String,
    entry: Option<TextEntry>,
    prompt: Option<Prompt>,
    skippable: bool,
    request: Option<Request>,
}

impl RelaySetupVm {
    /// `skippable`: the last choice reads Skip (first run) rather than Cancel (Settings).
    pub fn new(skippable: bool) -> Self {
        Self {
            service: None,
            phase: Phase::Choose,
            focus: 0,
            message: String::new(),
            server: String::new(),
            user: String::new(),
            password: String::new(),
            entry: None,
            prompt: None,
            skippable,
            request: None,
        }
    }

    /// Start over at the first page, keeping nothing typed.
    pub fn reset(&mut self) {
        *self = Self::new(self.skippable);
    }

    pub fn take_request(&mut self) -> Option<Request> {
        self.request.take()
    }

    /// The setup is waiting for work to finish (no choices to show).
    pub fn busy(&self) -> bool {
        matches!(self.phase, Phase::Working(_))
    }

    /// The on-screen keyboard is open.
    pub fn keyboard_open(&self) -> bool {
        self.entry.is_some()
    }

    pub fn entry(&self) -> Option<&TextEntry> {
        self.entry.as_ref()
    }

    pub fn prompt(&self) -> Option<&Prompt> {
        self.prompt.as_ref()
    }

    fn leave_label(&self) -> &'static str {
        if self.skippable {
            "Skip"
        } else {
            "Cancel"
        }
    }

    pub fn title(&self) -> String {
        match &self.phase {
            Phase::Choose => "Keep your island in sync: choose where it is kept. Skip to play without save sync."
                .into(),
            Phase::Form => "Nextcloud: enter the server address, your user name and an app password."
                .into(),
            Phase::Editing(_) | Phase::EnteringCode => self
                .entry
                .as_ref()
                .map(|entry| format!("{}\n{}", entry.label(), entry.shown()))
                .unwrap_or_default(),
            Phase::Working(Work::Connecting) => "Connecting to the server…".into(),
            Phase::Working(Work::StartingSignIn) => "Starting the sign-in…".into(),
            Phase::Waiting => match (&self.prompt, self.service) {
                (Some(prompt), Some(service)) if prompt.code.is_some() => format!(
                    "Sign in to {} on a phone: open {} and enter the code {}. This page goes on by itself.",
                    service.label(),
                    prompt.url,
                    prompt.code.as_deref().unwrap_or_default()
                ),
                (Some(prompt), Some(service)) if prompt.needs_code => format!(
                    "Sign in to {} on a phone: open {}, then enter the code {} shows.",
                    service.label(),
                    prompt.url,
                    service.label()
                ),
                (Some(prompt), Some(service)) => format!(
                    "Sign in to {} in your browser. This page goes on by itself. If it did not open, go to {}",
                    service.label(),
                    prompt.url
                ),
                _ => "Waiting for the sign-in…".into(),
            },
            Phase::Failed(reason) => reason.clone(),
        }
    }

    /// The choices of the page, one per line; the keyboard's keys while it is open.
    pub fn options(&self) -> Vec<String> {
        if let Some(entry) = &self.entry {
            return entry.labels();
        }
        match &self.phase {
            Phase::Choose => Service::ALL
                .iter()
                .map(|s| s.label().to_owned())
                .chain([self.leave_label().to_owned()])
                .collect(),
            Phase::Form => vec![
                format!("Server address: {}", shown_or(&self.server, "not entered")),
                format!("User name: {}", shown_or(&self.user, "not entered")),
                format!(
                    "App password: {}",
                    if self.password.is_empty() {
                        "not entered"
                    } else {
                        "entered"
                    }
                ),
                "Connect".into(),
                "Back".into(),
            ],
            Phase::Waiting if self.prompt.as_ref().is_some_and(|p| p.needs_code) => {
                vec!["Enter the code".into(), "Cancel".into()]
            }
            Phase::Waiting => vec!["Cancel".into()],
            Phase::Failed(_) => vec![
                "Retry".into(),
                "Another service".into(),
                self.leave_label().into(),
            ],
            Phase::Working(_) | Phase::Editing(_) | Phase::EnteringCode => vec![],
        }
    }

    pub fn handle(&mut self, nav: Nav) -> Outcome {
        if let Some(entry) = self.entry.as_mut() {
            let outcome = entry.handle(nav);
            self.keyboard_outcome(outcome);
            return Outcome::Open;
        }
        match nav {
            Nav::Up | Nav::Left => self.focus = self.focus.saturating_sub(1),
            Nav::Down | Nav::Right => {
                self.focus = (self.focus + 1).min(self.options().len().saturating_sub(1));
            }
            Nav::Back => return self.back(),
            Nav::Accept => return self.activate(self.focus),
            _ => {}
        }
        Outcome::Open
    }

    /// A key typed on a physical keyboard while the editor is open.
    pub fn type_text(&mut self, c: char) {
        if let Some(entry) = self.entry.as_mut() {
            entry.type_text(c);
        }
    }

    pub fn backspace(&mut self) {
        if let Some(entry) = self.entry.as_mut() {
            entry.backspace();
        }
    }

    /// Enter in a text field.
    pub fn submit(&mut self) {
        if let Some(entry) = self.entry.as_ref() {
            let outcome = entry.submit();
            self.keyboard_outcome(outcome);
        }
    }

    /// Esc in a text field.
    pub fn leave_editor(&mut self) {
        if let Some(entry) = self.entry.as_ref() {
            let outcome = entry.leave();
            self.keyboard_outcome(outcome);
        }
    }

    /// Back, as a click or a key: one step towards the first page.
    pub fn back(&mut self) -> Outcome {
        self.message.clear();
        match self.phase.clone() {
            Phase::Choose => return Outcome::Back,
            Phase::Form | Phase::Failed(_) => self.go_to_choose(),
            Phase::Waiting | Phase::Working(_) => {
                self.request = Some(Request::Cancel);
                if self.service == Some(Service::Nextcloud) {
                    self.phase = Phase::Form;
                    self.focus = 0;
                } else {
                    self.go_to_choose();
                }
            }
            Phase::Editing(_) | Phase::EnteringCode => self.leave_editor(),
        }
        Outcome::Open
    }

    fn go_to_choose(&mut self) {
        self.phase = Phase::Choose;
        self.focus = 0;
        self.prompt = None;
    }

    /// A choice pressed (Accept on it, or a click or tap).
    pub fn activate(&mut self, index: usize) -> Outcome {
        if let Some(entry) = self.entry.as_mut() {
            let outcome = entry.press(index);
            self.keyboard_outcome(outcome);
            return Outcome::Open;
        }
        if index >= self.options().len() {
            return Outcome::Open;
        }
        self.message.clear();
        match self.phase.clone() {
            Phase::Choose => match Service::ALL.get(index) {
                Some(Service::Nextcloud) => {
                    self.service = Some(Service::Nextcloud);
                    self.phase = Phase::Form;
                    self.focus = 0;
                }
                Some(service) => self.start_sign_in(*service),
                None => return Outcome::Skipped,
            },
            Phase::Form => match index {
                0 => self.open_editor(Field::Server),
                1 => self.open_editor(Field::User),
                2 => self.open_editor(Field::Password),
                3 => self.connect(),
                _ => {
                    self.go_to_choose();
                }
            },
            Phase::Waiting => {
                if index == 0 && self.prompt.as_ref().is_some_and(|p| p.needs_code) {
                    self.entry = Some(TextEntry::new("Code from Dropbox", "", false));
                    self.phase = Phase::EnteringCode;
                } else {
                    return self.back();
                }
            }
            Phase::Failed(_) => match index {
                0 => match self.service {
                    Some(Service::Nextcloud) | None => {
                        self.phase = Phase::Form;
                        self.focus = 3;
                    }
                    Some(service) => self.start_sign_in(service),
                },
                1 => self.go_to_choose(),
                _ => return Outcome::Skipped,
            },
            Phase::Working(_) | Phase::Editing(_) | Phase::EnteringCode => {}
        }
        Outcome::Open
    }

    fn start_sign_in(&mut self, service: Service) {
        self.service = Some(service);
        self.prompt = None;
        self.phase = Phase::Working(Work::StartingSignIn);
        self.request = Some(Request::StartSignIn(service));
    }

    fn open_editor(&mut self, field: Field) {
        let entry = match field {
            Field::Server => TextEntry::new(
                field.label(),
                if self.server.is_empty() {
                    "https://"
                } else {
                    &self.server
                },
                false,
            ),
            Field::User => TextEntry::new(field.label(), &self.user, false),
            Field::Password => TextEntry::new(field.label(), "", true),
        };
        self.entry = Some(entry);
        self.phase = Phase::Editing(field);
    }

    fn keyboard_outcome(&mut self, outcome: text_entry::Outcome) {
        let Some(entry) = self.entry.take() else {
            return;
        };
        match (outcome, self.phase.clone()) {
            (text_entry::Outcome::Editing, _) => self.entry = Some(entry),
            (text_entry::Outcome::Leave, Phase::EnteringCode) => self.phase = Phase::Waiting,
            (text_entry::Outcome::Leave, _) => self.phase = Phase::Form,
            (text_entry::Outcome::Done, Phase::Editing(field)) => {
                let text = entry.take();
                match field {
                    Field::Server => self.server = normalize_server(&text),
                    Field::User => self.user = text.trim().to_owned(),
                    Field::Password => self.password = text,
                }
                self.phase = Phase::Form;
                self.focus = match field {
                    Field::Server => 1,
                    Field::User => 2,
                    Field::Password => 3,
                };
            }
            (text_entry::Outcome::Done, Phase::EnteringCode) => {
                let code = entry.take().trim().to_owned();
                match self.service {
                    Some(service) if !code.is_empty() => {
                        self.phase = Phase::Waiting;
                        self.request = Some(Request::FinishSignIn(service, code));
                        self.message = "Checking the code…".into();
                    }
                    _ => self.phase = Phase::Waiting,
                }
            }
            (text_entry::Outcome::Done, _) => {}
        }
    }

    fn connect(&mut self) {
        if self.server.is_empty() {
            self.message = "Enter the server address first.".into();
            self.focus = 0;
        } else if self.user.is_empty() {
            self.message = "Enter the user name first.".into();
            self.focus = 1;
        } else if self.password.is_empty() {
            self.message = "Enter the app password first.".into();
            self.focus = 2;
        } else {
            self.request = Some(Request::Nextcloud {
                server: self.server.clone(),
                user: self.user.clone(),
                // The password leaves the model with the request.
                password: std::mem::take(&mut self.password),
            });
            self.phase = Phase::Working(Work::Connecting);
        }
    }

    /// The answer to a request. A late answer for work that was cancelled is ignored.
    pub fn event(&mut self, event: Event) -> Outcome {
        match (event, self.phase.clone()) {
            (Event::Connected(Ok(account)), Phase::Working(Work::Connecting)) => {
                return self.finished(account);
            }
            (Event::Connected(Err(reason)), Phase::Working(Work::Connecting)) => {
                // The server and user stay; the password was handed over and is typed again.
                self.phase = Phase::Form;
                self.message = reason;
                self.focus = 2;
            }
            (Event::SignInStarted(Ok(prompt)), Phase::Working(Work::StartingSignIn)) => {
                let service = self.service;
                if !prompt.needs_code {
                    if let Some(service) = service {
                        self.request = Some(Request::FinishSignIn(service, String::new()));
                    }
                }
                self.prompt = Some(prompt);
                self.phase = Phase::Waiting;
                self.focus = 0;
            }
            (Event::SignInStarted(Err(reason)), Phase::Working(Work::StartingSignIn)) => {
                self.phase = Phase::Failed(reason);
                self.focus = 0;
            }
            (Event::SignedIn(Ok(account)), Phase::Waiting | Phase::EnteringCode) => {
                self.entry = None;
                return self.finished(account);
            }
            (Event::SignedIn(Err(reason)), Phase::Waiting | Phase::EnteringCode) => {
                self.entry = None;
                self.phase = Phase::Failed(reason);
                self.focus = 0;
            }
            _ => {}
        }
        Outcome::Open
    }

    fn finished(&mut self, account: Account) -> Outcome {
        self.reset();
        Outcome::Done(account)
    }
}

fn shown_or<'a>(text: &'a str, empty: &'a str) -> &'a str {
    if text.is_empty() {
        empty
    } else {
        text
    }
}

/// A server address as typed: `https://` when no scheme was typed, no trailing slash.
pub fn normalize_server(text: &str) -> String {
    let text = text.trim().trim_end_matches('/');
    if text.is_empty() || text.eq_ignore_ascii_case("https:") || text.eq_ignore_ascii_case("http:")
    {
        return String::new();
    }
    if text.eq_ignore_ascii_case("https://") || text.eq_ignore_ascii_case("http://") {
        return String::new();
    }
    if text.contains("://") {
        text.to_owned()
    } else {
        format!("https://{text}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::text_entry::Key;

    /// Type `text` on the open keyboard the way a controller does: D-pad to the key, Accept,
    /// switching layers with the Shift and 123 keys when a character is not on this one.
    fn pad_type(vm: &mut RelaySetupVm, text: &str) {
        for c in text.chars() {
            let want = Key::Char(c);
            for _ in 0..3 {
                let keys = vm.entry().expect("keyboard open").keys();
                if let Some(index) = keys.iter().position(|k| *k == want) {
                    walk_to(vm, index);
                    vm.handle(Nav::Accept);
                    break;
                }
                let layer = vm.entry().unwrap().layer();
                let switch = if c.is_alphabetic() && layer != text_entry::Layer::Symbols {
                    Key::Shift
                } else {
                    Key::Layers
                };
                press_key(vm, switch);
            }
        }
    }

    /// Move focus to `index` using only D-pad presses.
    fn walk_to(vm: &mut RelaySetupVm, index: usize) {
        let columns = text_entry::COLUMNS;
        for _ in 0..200 {
            let at = vm.entry().unwrap().focus();
            if at == index {
                return;
            }
            let nav = if at / columns < index / columns {
                Nav::Down
            } else if at / columns > index / columns {
                Nav::Up
            } else if at < index {
                Nav::Right
            } else {
                Nav::Left
            };
            vm.handle(nav);
        }
        panic!("focus never reached key {index}");
    }

    fn press_key(vm: &mut RelaySetupVm, key: Key) {
        let index = vm
            .entry()
            .unwrap()
            .keys()
            .iter()
            .position(|k| *k == key)
            .unwrap();
        walk_to(vm, index);
        vm.handle(Nav::Accept);
    }

    fn enter_nextcloud(vm: &mut RelaySetupVm, server: &str, user: &str, password: &str) {
        vm.handle(Nav::Accept); // Nextcloud
        vm.activate(0);
        for _ in 0.."https://".len() {
            vm.backspace();
        }
        pad_type(vm, &server["https://".len()..]);
        // The prefilled scheme was deleted; the address gets its https:// when it is stored.
        press_key(vm, Key::Done);
        vm.activate(1);
        pad_type(vm, user);
        press_key(vm, Key::Done);
        vm.activate(2);
        pad_type(vm, password);
        press_key(vm, Key::Done);
    }

    fn account() -> Account {
        Account {
            service: Service::Nextcloud,
            name: "alex".into(),
            address: Some("https://cloud.test".into()),
        }
    }

    #[test]
    fn nextcloud_is_typed_with_the_controller_and_submitted() {
        let mut vm = RelaySetupVm::new(true);
        enter_nextcloud(&mut vm, "https://cloud.test", "alex", "secret-App1");
        assert_eq!(vm.phase, Phase::Form);
        assert_eq!(
            vm.options()[..3],
            [
                "Server address: https://cloud.test",
                "User name: alex",
                "App password: entered"
            ]
        );
        vm.activate(3);
        let request = vm.take_request().unwrap();
        assert_eq!(
            request,
            Request::Nextcloud {
                server: "https://cloud.test".into(),
                user: "alex".into(),
                password: "secret-App1".into()
            }
        );
        assert!(vm.busy());
        assert_eq!(
            vm.event(Event::Connected(Ok(account()))),
            Outcome::Done(account())
        );
        assert_eq!(vm.phase, Phase::Choose);
    }

    #[test]
    fn the_password_is_in_no_field_once_it_is_submitted() {
        let mut vm = RelaySetupVm::new(true);
        enter_nextcloud(&mut vm, "https://cloud.test", "alex", "hunter2-Pass");
        assert!(
            format!("{vm:?}").contains("hunter2-Pass"),
            "held while typing"
        );
        vm.activate(3);
        let request = vm.take_request().unwrap();
        assert!(!format!("{vm:?}").contains("hunter2-Pass"));
        assert!(!format!("{request:?}").contains("hunter2-Pass"));
        assert!(!format!("{:?}", request.without_secret()).contains("hunter2-Pass"));
        vm.event(Event::Connected(Err("bad".into())));
        assert!(!format!("{vm:?}").contains("hunter2-Pass"));
        assert_eq!(vm.options()[2], "App password: not entered");
    }

    #[test]
    fn a_wrong_password_shows_the_error_and_keeps_the_other_fields() {
        let mut vm = RelaySetupVm::new(true);
        enter_nextcloud(&mut vm, "https://cloud.test", "alex", "wrong");
        vm.activate(3);
        vm.take_request();
        let reason = "The server did not accept that user name and app password.";
        assert_eq!(
            vm.event(Event::Connected(Err(reason.into()))),
            Outcome::Open
        );
        assert_eq!(vm.phase, Phase::Form);
        assert_eq!(vm.message, reason);
        assert_eq!(vm.options()[0], "Server address: https://cloud.test");
        assert_eq!(vm.options()[1], "User name: alex");
        assert_eq!(vm.focus, 2);
        // A new password and Connect go again.
        vm.activate(2);
        pad_type(&mut vm, "right");
        press_key(&mut vm, Key::Done);
        vm.activate(3);
        assert!(matches!(vm.take_request(), Some(Request::Nextcloud { .. })));
    }

    #[test]
    fn missing_fields_are_named_and_nothing_is_sent() {
        let mut vm = RelaySetupVm::new(true);
        vm.handle(Nav::Accept);
        vm.activate(3);
        assert!(vm.message.contains("server"));
        assert_eq!(vm.take_request(), None);
        assert_eq!(vm.focus, 0);
    }

    #[test]
    fn skip_leaves_sync_off_and_back_walks_to_the_previous_step() {
        let mut vm = RelaySetupVm::new(true);
        assert_eq!(
            vm.options(),
            ["Nextcloud", "Google Drive", "Dropbox", "Skip"]
        );
        assert_eq!(vm.activate(3), Outcome::Skipped);
        assert_eq!(vm.back(), Outcome::Back);
        vm.handle(Nav::Accept);
        assert_eq!(vm.phase, Phase::Form);
        assert_eq!(vm.handle(Nav::Back), Outcome::Open);
        assert_eq!(vm.phase, Phase::Choose);
        assert_eq!(vm.handle(Nav::Back), Outcome::Back);
        // Settings has no Skip.
        assert_eq!(RelaySetupVm::new(false).options()[3], "Cancel");
    }

    #[test]
    fn the_editor_back_leaves_without_keeping_text_and_keyboard_keys_work() {
        let mut vm = RelaySetupVm::new(true);
        vm.handle(Nav::Accept);
        vm.activate(1);
        vm.type_text('b');
        vm.type_text('o');
        vm.type_text('b');
        vm.backspace();
        assert!(vm.title().contains("bo|"));
        vm.leave_editor();
        assert_eq!(vm.phase, Phase::Form);
        assert_eq!(vm.options()[1], "User name: not entered");
        vm.activate(1);
        vm.type_text('a');
        vm.submit();
        assert_eq!(vm.options()[1], "User name: a");
        vm.activate(0);
        assert_eq!(vm.handle(Nav::Back), Outcome::Open);
        assert_eq!(vm.phase, Phase::Form);
        assert!(!vm.keyboard_open());
    }

    #[test]
    fn a_server_without_a_scheme_gets_https() {
        assert_eq!(normalize_server("cloud.test/"), "https://cloud.test");
        assert_eq!(normalize_server("http://nas.local"), "http://nas.local");
        assert_eq!(normalize_server("https://"), "");
    }

    fn prompt(code: Option<&str>, needs_code: bool) -> Prompt {
        Prompt {
            url: "https://example.test/device".into(),
            code: code.map(Into::into),
            needs_code,
            browser: false,
        }
    }

    #[test]
    fn google_drive_in_game_mode_shows_code_and_url_and_finishes_by_itself() {
        let mut vm = RelaySetupVm::new(true);
        vm.activate(1);
        assert_eq!(
            vm.take_request(),
            Some(Request::StartSignIn(Service::GoogleDrive))
        );
        assert!(vm.busy());
        vm.event(Event::SignInStarted(Ok(prompt(Some("ABCD-EFGH"), false))));
        assert_eq!(vm.phase, Phase::Waiting);
        let title = vm.title();
        assert!(title.contains("ABCD-EFGH") && title.contains("https://example.test/device"));
        assert_eq!(
            vm.take_request(),
            Some(Request::FinishSignIn(Service::GoogleDrive, String::new()))
        );
        assert_eq!(vm.options(), ["Cancel"]);
        let done = Account {
            service: Service::GoogleDrive,
            name: "name@example.test".into(),
            address: None,
        };
        assert_eq!(
            vm.event(Event::SignedIn(Ok(done.clone()))),
            Outcome::Done(done)
        );
    }

    #[test]
    fn back_cancels_a_waiting_sign_in_and_a_late_answer_is_ignored() {
        let mut vm = RelaySetupVm::new(true);
        vm.activate(2);
        vm.take_request();
        vm.event(Event::SignInStarted(Ok(prompt(Some("CODE"), false))));
        vm.take_request();
        assert_eq!(vm.handle(Nav::Back), Outcome::Open);
        assert_eq!(vm.take_request(), Some(Request::Cancel));
        assert_eq!(vm.phase, Phase::Choose);
        let late = Account {
            service: Service::Dropbox,
            name: "x".into(),
            address: None,
        };
        assert_eq!(vm.event(Event::SignedIn(Ok(late))), Outcome::Open);
        assert_eq!(vm.phase, Phase::Choose);
    }

    #[test]
    fn dropbox_in_game_mode_asks_for_the_code_it_shows() {
        let mut vm = RelaySetupVm::new(true);
        vm.activate(2);
        vm.take_request();
        vm.event(Event::SignInStarted(Ok(prompt(None, true))));
        assert_eq!(vm.take_request(), None, "no finish until the code is typed");
        assert_eq!(vm.options(), ["Enter the code", "Cancel"]);
        vm.activate(0);
        assert!(vm.keyboard_open());
        pad_type(&mut vm, "Zq9");
        press_key(&mut vm, Key::Done);
        assert_eq!(
            vm.take_request(),
            Some(Request::FinishSignIn(Service::Dropbox, "Zq9".into()))
        );
        assert_eq!(vm.phase, Phase::Waiting);
    }

    #[test]
    fn a_failed_sign_in_offers_retry_another_service_and_skip() {
        let mut vm = RelaySetupVm::new(true);
        vm.activate(1);
        vm.take_request();
        vm.event(Event::SignInStarted(Err("Could not reach Google.".into())));
        assert_eq!(vm.title(), "Could not reach Google.");
        assert_eq!(vm.options(), ["Retry", "Another service", "Skip"]);
        vm.activate(0);
        assert_eq!(
            vm.take_request(),
            Some(Request::StartSignIn(Service::GoogleDrive))
        );
        vm.event(Event::SignInStarted(Err("Could not reach Google.".into())));
        vm.activate(1);
        assert_eq!(vm.phase, Phase::Choose);
        vm.activate(2);
        vm.take_request();
        vm.event(Event::SignInStarted(Err("no".into())));
        assert_eq!(vm.activate(2), Outcome::Skipped);
    }

    #[test]
    fn every_control_of_every_phase_is_reached_with_nav_alone() {
        // The first-run relay step and the keyboard: Up/Down/Left/Right plus Accept reach each
        // choice, and Back always has a way out.
        let mut phases: Vec<RelaySetupVm> = Vec::new();
        phases.push(RelaySetupVm::new(true));
        let mut form = RelaySetupVm::new(true);
        form.handle(Nav::Accept);
        phases.push(form.clone());
        let mut waiting = RelaySetupVm::new(true);
        waiting.activate(2);
        waiting.take_request();
        waiting.event(Event::SignInStarted(Ok(prompt(None, true))));
        phases.push(waiting);
        let mut failed = RelaySetupVm::new(true);
        failed.activate(1);
        failed.take_request();
        failed.event(Event::SignInStarted(Err("x".into())));
        phases.push(failed);
        for vm in phases {
            let count = vm.options().len();
            let mut at = vm.clone();
            let mut seen = vec![at.focus];
            for _ in 0..count {
                at.handle(Nav::Down);
                seen.push(at.focus);
            }
            for index in 0..count {
                assert!(
                    seen.contains(&index),
                    "{:?} never reached {index}",
                    vm.phase
                );
            }
            let mut back = vm.clone();
            let _ = back.handle(Nav::Back);
        }
        // The keyboard is covered in text_entry's own test; here it opens from the form and
        // closes with Back.
        form.activate(0);
        assert!(form.keyboard_open());
        assert_eq!(form.handle(Nav::Back), Outcome::Open);
        assert!(!form.keyboard_open());
    }
}
