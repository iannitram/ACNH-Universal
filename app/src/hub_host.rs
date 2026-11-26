//! The launcher as a build hub: the Fleet page's hub row turns it on and off, and shows what
//! a new device needs (the generated six-digit pairing code) and who is paired, by name.

use acnh_core::hub::{self, HostConfig, HubHost};
use acnh_core::platform::{AppDirs, Os};
use acnh_core::recipe;
use std::net::{IpAddr, TcpListener};

/// What the hub row of the Fleet page shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HubView {
    /// This machine can be a hub (a desktop; not a phone or tablet).
    pub available: bool,
    /// One line: whether the hub is on, and the paired devices by name.
    pub line: String,
    /// The six-digit code a new device types; empty when there is none to show.
    pub code: String,
    /// The label of the button that turns the hub on or off.
    pub button: String,
}

/// The hub this launcher can be, if any: the machine class decides which builds it serves.
pub fn role_for(os: Os) -> Option<recipe::Hub> {
    match os {
        Os::Linux | Os::Windows | Os::Mac => Some(recipe::hub_for(os)),
        Os::Ios | Os::Android => None,
    }
}

/// The paired devices as one phrase: "no devices paired yet" or "paired: a, b".
pub fn paired_line(names: &[String]) -> String {
    if names.is_empty() {
        "no devices paired yet".into()
    } else {
        format!("paired: {}", names.join(", "))
    }
}

pub struct HubController {
    dirs: AppDirs,
    os: Os,
    address: Box<dyn Fn() -> Option<IpAddr>>,
    announce: bool,
    host: Option<HubHost>,
    message: String,
}

impl HubController {
    pub fn new(dirs: AppDirs, os: Os) -> Self {
        Self::with(dirs, os, Box::new(hub::local_home_address), true)
    }

    /// With the address finder and the announcement given (tests use the loopback and no mDNS).
    pub fn with(
        dirs: AppDirs,
        os: Os,
        address: Box<dyn Fn() -> Option<IpAddr>>,
        announce: bool,
    ) -> Self {
        Self {
            dirs,
            os,
            address,
            announce,
            host: None,
            message: String::new(),
        }
    }

    /// Where finished builds are published for this hub to serve.
    pub fn builds_dir(dirs: &AppDirs) -> std::path::PathBuf {
        dirs.data.join("builds")
    }

    pub fn available(&self) -> bool {
        role_for(self.os).is_some()
    }

    pub fn is_on(&self) -> bool {
        self.host.is_some()
    }

    /// Turns the hub on (announces, serves, arms pairing with a new code) or off.
    pub fn toggle(&mut self) {
        self.message.clear();
        if self.host.take().is_some() {
            return;
        }
        if let Err(error) = self.start() {
            self.message = format!("Could not turn the hub on ({})", error.code());
        }
    }

    fn start(&mut self) -> acnh_core::Result<()> {
        let role = role_for(self.os)
            .ok_or_else(|| acnh_core::Error::Hub("this device cannot be a hub".into()))?;
        let ip = (self.address)()
            .ok_or_else(|| acnh_core::Error::Hub("this device is not on a home network".into()))?;
        let builds = Self::builds_dir(&self.dirs);
        std::fs::create_dir_all(&builds)?;
        // A free port on the home address; the announcement tells other devices which.
        let listener = TcpListener::bind((ip, 0))?;
        self.host = Some(HubHost::start(
            self.dirs.clone(),
            listener,
            HostConfig {
                name: role.key().to_owned(),
                role,
                builds,
                prepared: None,
                pair: true,
                announce: self.announce,
            },
        )?);
        Ok(())
    }

    pub fn view(&self) -> HubView {
        if !self.available() {
            return HubView::default();
        }
        let names: Vec<String> = hub::read_peers(&self.dirs)
            .unwrap_or_default()
            .into_iter()
            .map(|peer| peer.device)
            .collect();
        let paired = paired_line(&names);
        match &self.host {
            Some(host) => HubView {
                available: true,
                line: format!("Hub on at {}; {paired}", host.address()),
                code: host.status().code.unwrap_or_default(),
                button: "Turn hub off".into(),
            },
            None => HubView {
                available: true,
                line: if self.message.is_empty() {
                    format!("Hub off; {paired}")
                } else {
                    self.message.clone()
                },
                code: String::new(),
                button: "Turn hub on".into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acnh_core::platform::Vars;

    fn dirs(root: &std::path::Path) -> AppDirs {
        AppDirs::new(&Vars {
            home: root.to_owned(),
            data: root.join("data"),
            config: root.join("config"),
        })
    }

    #[test]
    fn only_desktops_can_be_hubs_and_serve_their_own_builds() {
        assert_eq!(role_for(Os::Mac), Some(recipe::Hub::Mac));
        assert_eq!(role_for(Os::Linux), Some(recipe::Hub::StrixHalo));
        assert_eq!(role_for(Os::Windows), Some(recipe::Hub::Desktop));
        assert_eq!(role_for(Os::Ios), None);
        assert_eq!(role_for(Os::Android), None);
    }

    #[test]
    fn paired_devices_are_listed_by_name() {
        assert_eq!(paired_line(&[]), "no devices paired yet");
        assert_eq!(
            paired_line(&["deck".into(), "phone".into()]),
            "paired: deck, phone"
        );
    }

    #[test]
    fn a_phone_has_no_hub_row() {
        let root = tempfile::tempdir().unwrap();
        let hub = HubController::new(dirs(root.path()), Os::Ios);
        assert!(!hub.available());
        assert_eq!(hub.view(), HubView::default());
    }

    #[test]
    fn turning_the_hub_on_shows_a_six_digit_code_and_off_removes_it() {
        let root = tempfile::tempdir().unwrap();
        let mut hub = HubController::with(
            dirs(root.path()),
            Os::Mac,
            Box::new(|| Some("127.0.0.1".parse().unwrap())),
            false,
        );
        assert_eq!(hub.view().button, "Turn hub on");
        assert_eq!(hub.view().code, "");
        hub.toggle();
        assert!(hub.is_on());
        let start = std::time::Instant::now();
        let view = loop {
            let view = hub.view();
            if !view.code.is_empty() || start.elapsed().as_secs() > 5 {
                break view;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(view.code.len(), 6);
        assert!(view.code.bytes().all(|b| b.is_ascii_digit()));
        assert_eq!(view.button, "Turn hub off");
        assert!(view.line.starts_with("Hub on at 127.0.0.1:"));
        hub.toggle();
        assert!(!hub.is_on());
        assert_eq!(hub.view().code, "");
    }

    #[test]
    fn a_device_off_the_home_network_says_so_instead_of_serving() {
        let root = tempfile::tempdir().unwrap();
        let mut hub = HubController::with(dirs(root.path()), Os::Linux, Box::new(|| None), false);
        hub.toggle();
        assert!(!hub.is_on());
        assert!(hub.view().line.starts_with("Could not turn the hub on"));
    }
}
