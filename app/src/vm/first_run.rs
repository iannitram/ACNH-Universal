//! Guided setup state, independent of Slint and the operating system.
use crate::vm::relay_setup::{self, RelaySetupVm};
use crate::vm::Nav;
use acnh_core::hub::Found;
use acnh_core::profile;
use acnh_core::recipe::Stage;
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Welcome,
    FindGame,
    PickFolder,
    FolderRejected(String),
    FindHub,
    NoHubFound,
    EnterAddress,
    Pair,
    GetBuild,
    BuildFailed(String),
    /// Step 5: set up save sync or skip it (`vm::relay_setup`).
    Relay,
    DeviceAndSteam,
    ChooseProfile,
    SteamFailed(String),
    Done,
}

#[derive(Clone, PartialEq, Eq)]
pub enum Request {
    Folders(PathBuf),
    Check(PathBuf),
    Discover,
    Connect(SocketAddr),
    Pair(SocketAddr, String, String),
    Build(PathBuf),
    Pull(SocketAddr, String),
    Finish {
        profile: usize,
        steam: Option<PathBuf>,
    },
    CopyLog(String),
}

#[derive(Clone, Debug)]
pub enum Event {
    Folders(Vec<PathBuf>),
    Checked(Result<String, String>),
    Hubs(Result<Vec<Found>, String>),
    Connected(Result<(), String>),
    Paired(Result<(), String>),
    Stage(Stage),
    Finished(Result<(), String>),
    FinishedSetup {
        steam: bool,
        result: Result<(), String>,
    },
}

pub struct FirstRunVm {
    pub step: Step,
    pub focus: usize,
    pub folder: PathBuf,
    pub folders: Vec<PathBuf>,
    pub hubs: Vec<Found>,
    pub endpoint: Option<SocketAddr>,
    pending_endpoint: Option<SocketAddr>,
    pub hub_name: String,
    pub host: String,
    pub port: String,
    pub editing_port: bool,
    pub host_caret: usize,
    pub port_caret: usize,
    pub code: String,
    pub stage: Option<Stage>,
    version: String,
    pub busy: bool,
    pub message: String,
    pub steam_applicable: bool,
    /// Save sync can be set up on this build, and is not set up yet: step 5 is shown.
    pub relay_available: bool,
    pub relay_set_up: bool,
    /// The relay step's pages.
    pub relay: RelaySetupVm,
    pub profile: usize,
    pulling: bool,
    request: Option<Request>,
    retry: Option<Request>,
    address_return: Step,
    address_unreachable: bool,
    pair_from_address: bool,
}

impl FirstRunVm {
    pub fn new(home: PathBuf) -> Self {
        Self {
            step: Step::Welcome,
            focus: 0,
            folder: home,
            folders: vec![],
            hubs: vec![],
            endpoint: None,
            pending_endpoint: None,
            hub_name: String::new(),
            host: String::new(),
            port: "47865".into(),
            editing_port: false,
            host_caret: 0,
            port_caret: 5,
            code: String::new(),
            stage: None,
            version: String::new(),
            busy: false,
            message: String::new(),
            steam_applicable: cfg!(target_os = "linux"),
            relay_available: false,
            relay_set_up: false,
            relay: RelaySetupVm::new(true),
            profile: profile::builtin().len() - 1,
            pulling: false,
            request: None,
            retry: None,
            address_return: Step::NoHubFound,
            address_unreachable: false,
            pair_from_address: false,
        }
    }
    pub fn take_request(&mut self) -> Option<Request> {
        self.request.take()
    }
    fn request(&mut self, request: Request) {
        self.busy = true;
        self.message.clear();
        if matches!(request, Request::Build(_)) {
            self.stage = None;
        }
        if !matches!(request, Request::Pair(..)) {
            self.retry = Some(request.clone());
        }
        self.request = Some(request);
    }
    fn go(&mut self, step: Step) {
        self.step = step;
        self.focus = 0;
    }
    pub fn step_number(&self) -> u8 {
        match self.step {
            Step::Welcome => 1,
            Step::FindGame
            | Step::PickFolder
            | Step::FolderRejected(_)
            | Step::FindHub
            | Step::NoHubFound
            | Step::EnterAddress => 2,
            Step::Pair => 3,
            Step::GetBuild | Step::BuildFailed(_) => 4,
            Step::Relay => 5,
            Step::DeviceAndSteam | Step::ChooseProfile | Step::SteamFailed(_) | Step::Done => 6,
        }
    }
    pub fn title(&self) -> String {
        match &self.step {
            Step::Welcome => "Welcome. The game is not included.".into(),
            Step::FindGame => "Where is the game?".into(),
            Step::PickFolder => format!(
                "Choose a game folder: {}",
                self.folder
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ),
            Step::FolderRejected(reason) => reason.clone(),
            Step::FindHub => "Choose a hub".into(),
            Step::NoHubFound => "No hub found on this network".into(),
            Step::EnterAddress => format!(
                "Hub address: {}  Port: {}",
                Self::with_caret(&self.host, self.host_caret, !self.editing_port),
                Self::with_caret(&self.port, self.port_caret, self.editing_port)
            ),
            Step::Pair => format!("Enter the six-digit code used on the hub: {}", self.code),
            Step::GetBuild if self.pulling => "Getting the player build".into(),
            Step::GetBuild => {
                let current = self.stage;
                let list = acnh_core::recipe::STAGES
                    .iter()
                    .map(|stage| {
                        let state = match current {
                            Some(now) if *stage < now => "Done or already available",
                            Some(now) if *stage == now => "Running",
                            _ => "Pending",
                        };
                        format!("{}: {}", stage.key(), state)
                    })
                    .collect::<Vec<_>>()
                    .join("  •  ");
                format!("Preparing version {}\n{list}", self.version)
            }
            Step::BuildFailed(reason) => reason.lines().next().unwrap_or("Build failed").into(),
            Step::Relay => self.relay.title(),
            Step::DeviceAndSteam => {
                format!("Device profile: {}", profile::builtin()[self.profile].name)
            }
            Step::ChooseProfile => "Choose a device profile".into(),
            Step::SteamFailed(reason) => reason.clone(),
            Step::Done => "Setup complete".into(),
        }
    }
    pub fn options(&self) -> Vec<String> {
        if self.step == Step::Relay {
            return self.relay.options();
        }
        if self.busy {
            return vec![];
        }
        match self.step {
            Step::Welcome => vec!["Continue".into()],
            Step::FindGame => vec![
                "This device has the game folder".into(),
                "Find a hub".into(),
            ],
            Step::PickFolder => {
                let mut out = vec!["Use this folder".into(), "Parent folder".into()];
                out.extend(self.folders.iter().map(|p| {
                    p.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                }));
                out
            }
            Step::FolderRejected(_) => vec!["Choose another folder".into()],
            Step::FindHub => {
                let mut out: Vec<_> = self.hubs.iter().map(|h| h.hub.clone()).collect();
                out.push("Retry".into());
                out.push("Enter address".into());
                out
            }
            Step::NoHubFound => vec!["Retry".into(), "Enter address".into()],
            Step::EnterAddress if self.address_unreachable => {
                vec!["Retry".into(), "Edit".into(), "Cancel".into()]
            }
            Step::EnterAddress => "0123456789abcdef.:"
                .chars()
                .map(|c| c.to_string())
                .chain([
                    "Delete".into(),
                    "Cursor left".into(),
                    "Cursor right".into(),
                    "Switch field".into(),
                    "Connect".into(),
                    "Cancel".into(),
                ])
                .collect(),
            Step::Pair => "0123456789"
                .chars()
                .map(|c| c.to_string())
                .chain(["Delete".into(), "Pair".into(), "Back".into()])
                .collect(),
            Step::GetBuild => vec![],
            Step::BuildFailed(_) => vec!["Retry".into(), "Copy log".into(), "Back".into()],
            Step::DeviceAndSteam if self.steam_applicable => {
                vec![
                    "Change profile".into(),
                    "Add to Steam".into(),
                    "Done".into(),
                ]
            }
            Step::DeviceAndSteam => vec!["Change profile".into(), "Done".into()],
            Step::ChooseProfile => profile::builtin()
                .iter()
                .map(|item| item.name.into())
                .collect(),
            Step::SteamFailed(_) => vec!["Try again".into(), "Skip".into()],
            Step::Relay => self.relay.options(),
            Step::Done => vec!["Continue".into()],
        }
    }
    pub fn handle(&mut self, nav: Nav) {
        if self.step == Step::Relay {
            let outcome = self.relay.handle(nav);
            self.relay_outcome(outcome);
            return;
        }
        if self.busy {
            return;
        }
        let count = self.options().len();
        let rows = if self.keypad_open() { 6 } else { 1 };
        match nav {
            Nav::Up => self.focus = self.focus.saturating_sub(rows),
            Nav::Down => self.focus = (self.focus + rows).min(count.saturating_sub(1)),
            Nav::Left => self.focus = self.focus.saturating_sub(1),
            Nav::Right => self.focus = (self.focus + 1).min(count.saturating_sub(1)),
            Nav::Back => self.back(),
            Nav::Accept => self.activate(self.focus),
            _ => {}
        }
    }
    pub fn activate(&mut self, index: usize) {
        if self.step == Step::Relay {
            let outcome = self.relay.activate(index);
            self.relay_outcome(outcome);
            return;
        }
        if self.busy || index >= self.options().len() {
            return;
        }
        match self.step.clone() {
            Step::Welcome => self.go(Step::FindGame),
            Step::FindGame if index == 0 => {
                self.pulling = false;
                self.go(Step::PickFolder);
                self.request(Request::Folders(self.folder.clone()));
            }
            Step::FindGame => {
                self.go(Step::FindHub);
                self.request(Request::Discover);
            }
            Step::PickFolder if index == 0 => self.request(Request::Check(self.folder.clone())),
            Step::PickFolder => {
                if index == 1 {
                    self.folder.pop();
                } else {
                    self.folder = self.folders[index - 2].clone();
                }
                self.request(Request::Folders(self.folder.clone()));
            }
            Step::FolderRejected(_) => self.go(Step::PickFolder),
            Step::FindHub if index < self.hubs.len() => {
                let hub = &self.hubs[index];
                self.endpoint = Some(SocketAddr::new(hub.addr, hub.port));
                self.hub_name = hub.hub.clone();
                self.pair_from_address = false;
                self.code.clear();
                self.go(Step::Pair);
            }
            Step::FindHub if index == self.hubs.len() => self.request(Request::Discover),
            Step::FindHub | Step::NoHubFound
                if index == 0 && matches!(self.step, Step::NoHubFound) =>
            {
                self.request(Request::Discover)
            }
            Step::FindHub | Step::NoHubFound => {
                self.address_return = self.step.clone();
                self.address_unreachable = false;
                self.message.clear();
                self.go(Step::EnterAddress);
            }
            Step::EnterAddress if self.address_unreachable => match index {
                0 => self.connect_address(),
                1 => self.edit_address(),
                _ => self.back(),
            },
            Step::EnterAddress if index < 18 => {
                let c = "0123456789abcdef.:".chars().nth(index).unwrap();
                self.insert_address(c);
            }
            Step::EnterAddress if index == 18 => self.delete_address(),
            Step::EnterAddress if index == 19 => {
                if self.editing_port {
                    self.port_caret = self.port_caret.saturating_sub(1);
                } else {
                    self.host_caret = self.host_caret.saturating_sub(1);
                }
            }
            Step::EnterAddress if index == 20 => {
                if self.editing_port {
                    self.port_caret = (self.port_caret + 1).min(self.port.len());
                } else {
                    self.host_caret = (self.host_caret + 1).min(self.host.len());
                }
            }
            Step::EnterAddress if index == 21 => self.editing_port = !self.editing_port,
            Step::EnterAddress if index == 22 => self.connect_address(),
            Step::EnterAddress => self.go(self.address_return.clone()),
            Step::Pair if index < 10 => {
                if self.code.len() < 6 {
                    self.code.push(char::from(b'0' + index as u8));
                }
            }
            Step::Pair if index == 10 => {
                self.code.pop();
            }
            Step::Pair if index == 11 && self.code.len() == 6 => self.request(Request::Pair(
                self.endpoint.unwrap(),
                self.hub_name.clone(),
                self.code.clone(),
            )),
            Step::Pair if index == 12 => self.back(),
            Step::BuildFailed(_) if index == 0 => {
                if let Some(request) = self.retry.clone() {
                    self.request(request);
                }
            }
            Step::BuildFailed(reason) if index == 1 => {
                self.request = Some(Request::CopyLog(reason));
            }
            Step::BuildFailed(_) => self.go(Step::FindGame),
            Step::DeviceAndSteam if index == 0 => self.go(Step::ChooseProfile),
            Step::DeviceAndSteam => self.finish_setup(index == 1 && self.steam_applicable),
            Step::ChooseProfile => {
                self.profile = index;
                self.go(Step::DeviceAndSteam);
            }
            Step::SteamFailed(_) => self.finish_setup(index == 0),
            _ => {}
        }
    }

    fn finish_setup(&mut self, add_steam: bool) {
        self.message.clear();
        self.request(Request::Finish {
            profile: self.profile,
            steam: add_steam.then(|| self.folder.clone()),
        });
    }

    /// How the relay step was left: set up or skipped goes on to the device and Steam, Back
    /// goes to the step before it.
    pub fn relay_outcome(&mut self, outcome: relay_setup::Outcome) {
        match outcome {
            relay_setup::Outcome::Open => {}
            relay_setup::Outcome::Done(_) => {
                self.relay_set_up = true;
                self.go(Step::DeviceAndSteam);
            }
            relay_setup::Outcome::Skipped => self.go(Step::DeviceAndSteam),
            relay_setup::Outcome::Back => self.go(Step::FindGame),
        }
    }

    /// Keys across one row of the open keypad.
    pub fn columns(&self) -> usize {
        if self.step == Step::Relay {
            crate::vm::text_entry::COLUMNS
        } else {
            6
        }
    }

    pub fn keypad_open(&self) -> bool {
        if self.step == Step::Relay {
            return self.relay.keyboard_open();
        }
        !self.busy
            && (self.step == Step::Pair
                || (self.step == Step::EnterAddress && !self.address_unreachable))
    }

    fn connect_address(&mut self) {
        match (self.host.parse(), self.port.parse::<u16>()) {
            (Ok(ip), Ok(port)) if port != 0 && acnh_core::hub::is_home_network(ip) => {
                let endpoint = SocketAddr::new(ip, port);
                self.pending_endpoint = Some(endpoint);
                self.request(Request::Connect(endpoint));
            }
            _ => self.message = "Enter a valid home-network IP and port.".into(),
        }
    }

    fn edit_address(&mut self) {
        self.address_unreachable = false;
        self.message.clear();
        self.focus = 0;
    }
    fn with_caret(text: &str, at: usize, show: bool) -> String {
        let mut text = text.to_owned();
        if show {
            text.insert(at.min(text.len()), '|');
        }
        text
    }
    pub fn insert_address(&mut self, c: char) {
        if self.busy || self.step != Step::EnterAddress {
            return;
        }
        if self.address_unreachable {
            self.edit_address();
        }
        if self.editing_port {
            if c.is_ascii_digit() && self.port.len() < 5 {
                self.port.insert(self.port_caret, c);
                self.port_caret += 1;
            }
        } else if (c.is_ascii_hexdigit() || matches!(c, '.' | ':')) && self.host.len() < 45 {
            self.host.insert(self.host_caret, c);
            self.host_caret += 1;
        }
        self.message.clear();
    }
    pub fn delete_address(&mut self) {
        if self.address_unreachable {
            self.edit_address();
        }
        let (text, caret) = if self.editing_port {
            (&mut self.port, &mut self.port_caret)
        } else {
            (&mut self.host, &mut self.host_caret)
        };
        if *caret > 0 {
            *caret -= 1;
            text.remove(*caret);
        }
        self.message.clear();
    }
    fn back(&mut self) {
        self.message.clear();
        match self.step {
            Step::FindGame => self.go(Step::Welcome),
            Step::PickFolder => self.go(Step::FindGame),
            Step::FolderRejected(_) => self.go(Step::PickFolder),
            Step::FindHub | Step::NoHubFound | Step::BuildFailed(_) => self.go(Step::FindGame),
            Step::Pair if self.pair_from_address => self.go(Step::EnterAddress),
            Step::Pair => self.go(Step::FindHub),
            Step::EnterAddress => self.go(self.address_return.clone()),
            Step::DeviceAndSteam => self.go(Step::FindGame),
            Step::ChooseProfile => self.go(Step::DeviceAndSteam),
            Step::SteamFailed(_) => self.go(Step::DeviceAndSteam),
            _ => {}
        }
    }
    pub fn event(&mut self, event: Event) {
        self.busy = false;
        match event {
            Event::Folders(folders) => self.folders = folders,
            Event::Checked(Ok(version)) => {
                self.version = version;
                self.go(Step::GetBuild);
                self.request(Request::Build(self.folder.clone()));
            }
            Event::Checked(Err(reason)) => self.go(Step::FolderRejected(reason)),
            Event::Hubs(Ok(hubs)) => {
                self.hubs = hubs;
                self.go(if self.hubs.is_empty() {
                    Step::NoHubFound
                } else {
                    Step::FindHub
                });
            }
            Event::Hubs(Err(reason)) => self.go(Step::BuildFailed(reason)),
            Event::Connected(Ok(())) => {
                self.address_unreachable = false;
                self.endpoint = self.pending_endpoint.take();
                self.hub_name = "hub".into();
                self.pair_from_address = true;
                self.message.clear();
                self.code.clear();
                self.go(Step::Pair);
            }
            Event::Connected(Err(reason)) => {
                self.address_unreachable = true;
                self.message = reason;
                self.go(Step::EnterAddress);
            }
            Event::Paired(Ok(())) => {
                self.pulling = true;
                self.message.clear();
                self.go(Step::GetBuild);
                self.request(Request::Pull(self.endpoint.unwrap(), self.hub_name.clone()));
            }
            Event::Paired(Err(_)) => {
                self.message = "Codes did not match. Try again.".into();
                self.code.clear();
                self.go(Step::Pair);
            }
            Event::Stage(stage) => {
                self.stage = Some(stage);
                self.busy = true;
            }
            Event::Finished(Ok(())) if self.pulling => {
                self.pulling = false;
                self.message =
                    "Player received. Prepare the game on this device to finish setup.".into();
                self.go(Step::FindGame);
            }
            Event::Finished(Ok(())) => {
                if self.relay_available && !self.relay_set_up {
                    self.relay.reset();
                    self.go(Step::Relay);
                } else {
                    self.go(Step::DeviceAndSteam);
                }
            }
            Event::Finished(Err(reason)) => self.go(Step::BuildFailed(reason)),
            Event::FinishedSetup { result: Ok(()), .. } => self.go(Step::Done),
            Event::FinishedSetup {
                steam: true,
                result: Err(reason),
            } => self.go(Step::SteamFailed(reason)),
            Event::FinishedSetup {
                steam: false,
                result: Err(reason),
            } => {
                self.go(Step::DeviceAndSteam);
                self.message = reason;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acnh_core::platform::Os;
    fn vm() -> FirstRunVm {
        FirstRunVm::new(PathBuf::from("/fake"))
    }
    #[test]
    fn manual_address_retains_errors_and_edits_at_caret() {
        let mut vm = vm();
        vm.step = Step::NoHubFound;
        vm.activate(1);
        for c in "127.0.0.1".chars() {
            vm.insert_address(c);
        }
        vm.host_caret = 3;
        vm.insert_address('f');
        assert_eq!(vm.host, "127f.0.0.1");
        vm.delete_address();
        assert_eq!(vm.host, "127.0.0.1");
        vm.host = "8.8.8.8".into();
        vm.host_caret = vm.host.len();
        vm.activate(22);
        assert_eq!(vm.step, Step::EnterAddress);
        assert!(vm.message.contains("home-network"));
        assert_eq!(vm.host, "8.8.8.8");
        vm.handle(Nav::Back);
        assert_eq!(vm.step, Step::NoHubFound);
    }

    #[test]
    fn failed_connection_can_retry_or_edit_without_losing_the_address() {
        let mut vm = vm();
        vm.step = Step::NoHubFound;
        vm.activate(1);
        for c in "127.0.0.1".chars() {
            vm.insert_address(c);
        }
        vm.activate(22);
        assert!(matches!(vm.take_request(), Some(Request::Connect(_))));
        vm.event(Event::Connected(Err("Hub unavailable".into())));
        assert_eq!(vm.options(), ["Retry", "Edit", "Cancel"]);
        vm.activate(0);
        assert!(matches!(vm.take_request(), Some(Request::Connect(_))));
        vm.event(Event::Connected(Err("Hub unavailable".into())));
        vm.activate(1);
        assert!(vm.keypad_open());
        assert_eq!(vm.host, "127.0.0.1");
    }
    fn after_build(relay_available: bool, relay_set_up: bool) -> FirstRunVm {
        let mut vm = vm();
        vm.relay_available = relay_available;
        vm.relay_set_up = relay_set_up;
        vm.step = Step::GetBuild;
        vm.event(Event::Finished(Ok(())));
        vm
    }

    #[test]
    fn the_relay_step_comes_after_the_build_and_before_add_to_steam() {
        let mut vm = after_build(true, false);
        assert_eq!(vm.step, Step::Relay);
        assert_eq!(vm.step_number(), 5);
        assert_eq!(
            vm.options(),
            ["Nextcloud", "Google Drive", "Dropbox", "Skip"]
        );
        assert!(!vm.keypad_open());
        // Skip goes on to the device and Steam with save sync off.
        vm.handle(Nav::Down);
        vm.handle(Nav::Down);
        vm.handle(Nav::Down);
        assert_eq!(vm.relay.options()[vm.relay.focus], "Skip");
        vm.handle(Nav::Accept);
        assert_eq!(vm.step, Step::DeviceAndSteam);
        assert_eq!(vm.step_number(), 6);
        assert!(!vm.relay_set_up, "skipped: save sync stays off");
        assert!(vm.options().contains(&"Done".to_string()));
    }

    #[test]
    fn the_relay_step_is_skipped_when_set_up_or_not_available() {
        assert_eq!(after_build(true, true).step, Step::DeviceAndSteam);
        assert_eq!(after_build(false, false).step, Step::DeviceAndSteam);
    }

    #[test]
    fn back_from_the_relay_step_goes_to_the_step_before_it() {
        let mut vm = after_build(true, false);
        vm.handle(Nav::Back);
        assert_eq!(vm.step, Step::FindGame);
        // From the Nextcloud form, Back goes to the choice first, then out.
        let mut vm = after_build(true, false);
        vm.handle(Nav::Accept);
        assert_eq!(vm.title().split(':').next(), Some("Nextcloud"));
        vm.handle(Nav::Back);
        assert_eq!(vm.step, Step::Relay);
        assert_eq!(vm.relay.options()[0], "Nextcloud");
        vm.handle(Nav::Back);
        assert_eq!(vm.step, Step::FindGame);
    }

    #[test]
    fn a_finished_setup_goes_on_and_remembers_the_account() {
        let mut vm = after_build(true, false);
        let account = relay_setup::Account {
            service: relay_setup::Service::Nextcloud,
            name: "alex".into(),
            address: Some("https://cloud.test".into()),
        };
        vm.relay_outcome(relay_setup::Outcome::Done(account));
        assert_eq!(vm.step, Step::DeviceAndSteam);
        assert!(vm.relay_set_up);
    }

    #[test]
    fn nav_alone_reaches_every_control_of_the_relay_step_and_the_keyboard() {
        let mut vm = after_build(true, false);
        let count = vm.options().len();
        let mut seen = vec![vm.relay.focus];
        for _ in 0..count {
            vm.handle(Nav::Down);
            seen.push(vm.relay.focus);
        }
        assert!((0..count).all(|i| seen.contains(&i)));
        // The keyboard opens from the Nextcloud form with ten keys to a row; text_entry's own
        // test walks every key of every layer with the D-pad.
        for _ in 0..count {
            vm.handle(Nav::Up);
        }
        vm.handle(Nav::Accept);
        vm.activate(0);
        assert!(vm.keypad_open());
        assert_eq!(vm.columns(), 10);
        assert!(vm.options().len() > 40);
        // Back leaves the editor, not the step.
        vm.handle(Nav::Back);
        assert!(!vm.keypad_open());
        assert_eq!(vm.step, Step::Relay);
    }

    #[test]
    fn local_path_names_errors_and_builds_after_check() {
        let mut vm = vm();
        vm.handle(Nav::Accept);
        vm.handle(Nav::Accept);
        assert!(matches!(vm.take_request(), Some(Request::Folders(_))));
        vm.event(Event::Folders(vec![]));
        vm.handle(Nav::Accept);
        assert!(matches!(vm.take_request(), Some(Request::Check(_))));
        vm.event(Event::Checked(Err("exefs/sdk is missing".into())));
        assert!(vm.title().contains("exefs/sdk"));
        vm.handle(Nav::Accept);
        vm.handle(Nav::Accept);
        vm.take_request();
        vm.event(Event::Checked(Err(
            "Supported versions: 3.0.2, 3.0.3, 2.0.7, 1.0.0".into(),
        )));
        for version in ["3.0.2", "3.0.3", "2.0.7", "1.0.0"] {
            assert!(vm.title().contains(version));
        }
        vm.handle(Nav::Accept);
        vm.handle(Nav::Accept);
        vm.take_request();
        vm.event(Event::Checked(Ok("3.0.2".into())));
        assert!(matches!(vm.take_request(), Some(Request::Build(_))));
        vm.event(Event::Stage(Stage::Build));
        vm.event(Event::Finished(Ok(())));
        assert_eq!(vm.step, Step::DeviceAndSteam);
        vm.activate(0);
        assert_eq!(vm.step, Step::ChooseProfile);
        vm.activate(2);
        vm.activate(if vm.steam_applicable { 2 } else { 1 });
        assert!(matches!(
            vm.take_request(),
            Some(Request::Finish {
                profile: 2,
                steam: None
            })
        ));
        vm.event(Event::FinishedSetup {
            steam: false,
            result: Ok(()),
        });
        assert_eq!(vm.step, Step::Done);
    }
    #[test]
    fn client_discovers_pairs_pulls_then_requires_local_preparation() {
        let mut vm = vm();
        vm.handle(Nav::Accept);
        vm.activate(1);
        assert!(matches!(vm.take_request(), Some(Request::Discover)));
        vm.event(Event::Hubs(Ok(vec![Found {
            hub: "test".into(),
            addr: "127.0.0.1".parse().unwrap(),
            port: 1234,
            targets: vec![Os::Linux],
        }])));
        vm.activate(0);
        for digit in [1, 2, 3, 4, 5, 6] {
            vm.activate(digit);
        }
        vm.activate(11);
        assert!(matches!(vm.take_request(), Some(Request::Pair(..))));
        vm.event(Event::Paired(Err("mismatch".into())));
        assert!(vm.message.contains("Codes did not match"));
        for digit in [1, 2, 3, 4, 5, 6] {
            vm.activate(digit);
        }
        vm.activate(11);
        vm.take_request();
        vm.event(Event::Paired(Ok(())));
        assert!(matches!(vm.take_request(), Some(Request::Pull(..))));
        vm.event(Event::Finished(Err("offline".into())));
        vm.activate(0);
        assert!(matches!(vm.take_request(), Some(Request::Pull(..))));
        vm.event(Event::Finished(Ok(())));
        assert_eq!(vm.step, Step::FindGame);
        assert!(vm.message.contains("Prepare the game"));
    }
}
