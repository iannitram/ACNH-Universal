//! Guided setup state, independent of Slint and the operating system.
use crate::vm::Nav;
use acnh_core::hub::Found;
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
    DeviceAndSteam,
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
    AddSteam,
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
    Steam(Result<(), String>),
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
    pub busy: bool,
    pub message: String,
    pub steam_applicable: bool,
    pulling: bool,
    request: Option<Request>,
    retry: Option<Request>,
    address_return: Step,
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
            busy: false,
            message: String::new(),
            steam_applicable: cfg!(target_os = "linux"),
            pulling: false,
            request: None,
            retry: None,
            address_return: Step::NoHubFound,
            pair_from_address: false,
        }
    }
    pub fn take_request(&mut self) -> Option<Request> {
        self.request.take()
    }
    fn request(&mut self, request: Request) {
        self.busy = true;
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
            Step::DeviceAndSteam | Step::SteamFailed(_) | Step::Done => 6,
        }
    }
    pub fn title(&self) -> String {
        match &self.step {
            Step::Welcome => "Welcome. The game is not included.".into(),
            Step::FindGame => {
                if self.message.is_empty() {
                    "Where is the game?".into()
                } else {
                    self.message.clone()
                }
            }
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
                "{} Hub address: {}  Port: {}",
                self.message,
                Self::with_caret(&self.host, self.host_caret, !self.editing_port),
                Self::with_caret(&self.port, self.port_caret, self.editing_port)
            ),
            Step::Pair => format!(
                "{} Enter the six-digit code used on the hub: {}",
                self.message, self.code
            ),
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
                format!(
                    "{}\n{}",
                    if self.message.is_empty() {
                        "Preparing. Back requests cancellation after this work finishes."
                    } else {
                        &self.message
                    },
                    list
                )
            }
            Step::BuildFailed(reason) => reason.clone(),
            Step::DeviceAndSteam => "Device setup".into(),
            Step::SteamFailed(reason) => reason.clone(),
            Step::Done => "Setup complete".into(),
        }
    }
    pub fn options(&self) -> Vec<String> {
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
            Step::BuildFailed(_) => vec!["Retry".into(), "Back".into()],
            Step::DeviceAndSteam if self.steam_applicable => {
                vec!["Add to Steam".into(), "Done".into()]
            }
            Step::DeviceAndSteam => vec!["Done".into()],
            Step::SteamFailed(_) => vec!["Try again".into(), "Skip".into()],
            Step::Done => vec!["Continue".into()],
        }
    }
    pub fn handle(&mut self, nav: Nav) {
        if self.busy {
            return;
        }
        let count = self.options().len();
        match nav {
            Nav::Up | Nav::Left => self.focus = self.focus.saturating_sub(1),
            Nav::Down | Nav::Right => self.focus = (self.focus + 1).min(count.saturating_sub(1)),
            Nav::Back => self.back(),
            Nav::Accept => self.activate(self.focus),
            _ => {}
        }
    }
    pub fn activate(&mut self, index: usize) {
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
                self.message.clear();
                self.go(Step::EnterAddress);
            }
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
            Step::EnterAddress if index == 22 => {
                match (self.host.parse(), self.port.parse::<u16>()) {
                    (Ok(ip), Ok(port)) if port != 0 && acnh_core::hub::is_home_network(ip) => {
                        let endpoint = SocketAddr::new(ip, port);
                        self.pending_endpoint = Some(endpoint);
                        self.request(Request::Connect(endpoint));
                    }
                    _ => self.message = "Enter a valid home-network IP and port.".into(),
                }
            }
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
            Step::BuildFailed(_) => self.go(Step::FindGame),
            Step::DeviceAndSteam if index == 0 && self.steam_applicable => {
                self.request(Request::AddSteam)
            }
            Step::DeviceAndSteam => self.go(Step::Done),
            Step::SteamFailed(_) if index == 0 => self.request(Request::AddSteam),
            Step::SteamFailed(_) => self.go(Step::Done),
            _ => {}
        }
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
        match self.step {
            Step::FindGame => self.go(Step::Welcome),
            Step::PickFolder => self.go(Step::FindGame),
            Step::Pair if self.pair_from_address => self.go(Step::EnterAddress),
            Step::Pair => self.go(Step::FindHub),
            Step::EnterAddress => self.go(self.address_return.clone()),
            Step::DeviceAndSteam => self.go(Step::FindGame),
            _ => {}
        }
    }
    pub fn event(&mut self, event: Event) {
        self.busy = false;
        match event {
            Event::Folders(folders) => self.folders = folders,
            Event::Checked(Ok(_version)) => {
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
                self.endpoint = self.pending_endpoint.take();
                self.hub_name = "hub".into();
                self.pair_from_address = true;
                self.message.clear();
                self.code.clear();
                self.go(Step::Pair);
            }
            Event::Connected(Err(reason)) => {
                self.message = reason;
                self.go(Step::EnterAddress);
                self.focus = 22;
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
            Event::Finished(Ok(())) => self.go(Step::DeviceAndSteam),
            Event::Finished(Err(reason)) => self.go(Step::BuildFailed(reason)),
            Event::Steam(Ok(())) => self.go(Step::Done),
            Event::Steam(Err(reason)) => self.go(Step::SteamFailed(reason)),
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
        vm.activate(if vm.steam_applicable { 1 } else { 0 });
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
        assert!(vm.title().contains("Codes did not match"));
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
        assert!(vm.title().contains("Prepare the game"));
    }
}
