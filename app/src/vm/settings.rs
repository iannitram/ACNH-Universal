//! The Settings page's view model (SPEC.md M2): the device profile and the `config.ini`
//! values it writes. Writing keeps every line of the player's file that is not changed,
//! comments and unknown keys included; a profile only fills keys the player has not set.

use acnh_core::{config::Config, profile::DeviceProfile, Error, Result};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// A control on the launcher Settings page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsFocus {
    Ptbr,
    Speedfix,
    /// "Sign in again", or "Set up save sync" when save sync is off.
    RelaySignIn,
    /// "Turn off save sync", only while save sync is set up.
    RelayOff,
    Back,
}

/// Which save sync controls the page has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayRows {
    /// This build has no save sync setup.
    Hidden,
    /// Save sync is off: only the setup row.
    SignIn,
    /// Save sync is set up: sign in again and turn off.
    SignInAndOff,
}

/// The controls in focus order.
pub fn settings_rows(relay: RelayRows) -> Vec<SettingsFocus> {
    use SettingsFocus::{Back, Ptbr, RelayOff, RelaySignIn, Speedfix};
    match relay {
        RelayRows::Hidden => vec![Ptbr, Speedfix, Back],
        RelayRows::SignIn => vec![Ptbr, Speedfix, RelaySignIn, Back],
        RelayRows::SignInAndOff => vec![Ptbr, Speedfix, RelaySignIn, RelayOff, Back],
    }
}

/// Move among `rows` with a navigation action; the ends hold.
pub fn move_in(rows: &[SettingsFocus], focus: SettingsFocus, nav: crate::vm::Nav) -> SettingsFocus {
    use crate::vm::Nav;
    let Some(at) = rows.iter().position(|row| *row == focus) else {
        return rows.first().copied().unwrap_or(focus);
    };
    match nav {
        Nav::Down | Nav::Right => rows[(at + 1).min(rows.len() - 1)],
        Nav::Up | Nav::Left => rows[at.saturating_sub(1)],
        _ => focus,
    }
}

/// Move between Settings controls using a shared navigation action.
pub fn move_focus(focus: SettingsFocus, nav: crate::vm::Nav) -> SettingsFocus {
    move_in(&settings_rows(RelayRows::Hidden), focus, nav)
}

/// The save sync rows of Settings: what is set up, and the actions on it. The account comes
/// from `RelaySetup::account`; Sign in again opens the same pages as the first run's step 5.
#[derive(Debug, Clone)]
pub struct SettingsRelay {
    pub available: bool,
    pub account: Option<crate::vm::relay_setup::Account>,
    /// "Turn off save sync" was pressed once and waits for a second press.
    pub confirming_off: bool,
    /// The sign-in pages, while they are open.
    pub flow: Option<crate::vm::relay_setup::RelaySetupVm>,
    pub message: String,
}

impl SettingsRelay {
    pub fn new(available: bool, account: Option<crate::vm::relay_setup::Account>) -> SettingsRelay {
        SettingsRelay {
            available,
            account,
            confirming_off: false,
            flow: None,
            message: String::new(),
        }
    }

    pub fn rows(&self) -> RelayRows {
        match (self.available, &self.account) {
            (false, _) => RelayRows::Hidden,
            (true, None) => RelayRows::SignIn,
            (true, Some(_)) => RelayRows::SignInAndOff,
        }
    }

    /// The row's text: the service and account, or that save sync is off.
    pub fn summary(&self) -> String {
        match &self.account {
            Some(account) => format!("Save sync: {}", account.summary()),
            None => "Save sync is off. This device plays without syncing.".into(),
        }
    }

    pub fn sign_in_label(&self) -> &'static str {
        if self.account.is_some() {
            "Sign in again"
        } else {
            "Set up save sync"
        }
    }

    pub fn turn_off_label(&self) -> &'static str {
        if self.confirming_off {
            "Press again to turn off save sync"
        } else {
            "Turn off save sync"
        }
    }

    /// Open the sign-in pages (Cancel instead of Skip).
    pub fn open_sign_in(&mut self) {
        self.confirming_off = false;
        self.message.clear();
        self.flow = Some(crate::vm::relay_setup::RelaySetupVm::new(false));
    }

    /// How the pages were left: set up shows the new account; Cancel and Back change nothing.
    pub fn flow_outcome(&mut self, outcome: crate::vm::relay_setup::Outcome) {
        use crate::vm::relay_setup::Outcome;
        match outcome {
            Outcome::Open => {}
            Outcome::Done(account) => {
                self.message = format!("Save sync is on: {}.", account.summary());
                self.account = Some(account);
                self.flow = None;
            }
            Outcome::Skipped | Outcome::Back => self.flow = None,
        }
    }
}

/// The runtime's settings file, in the data root.
pub const CONFIG_FILE: &str = "config.ini";

/// The Settings page for one data root and one device profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsVm {
    data_root: PathBuf,
    profile: &'static DeviceProfile,
}

impl SettingsVm {
    pub fn new(data_root: impl Into<PathBuf>, profile: &'static DeviceProfile) -> Self {
        Self {
            data_root: data_root.into(),
            profile,
        }
    }

    pub fn profile(&self) -> &'static DeviceProfile {
        self.profile
    }

    /// `<data root>/config.ini`.
    pub fn config_path(&self) -> PathBuf {
        self.data_root.join(CONFIG_FILE)
    }

    /// The file as parsed; a missing file is an empty one.
    pub fn config(&self) -> Result<Config> {
        read_config(&self.config_path())
    }

    /// Fan mods are opt-in, including when the setting is absent.
    pub fn fan_mods(&self) -> Result<(bool, bool)> {
        let config = self.config()?;
        let enabled = |key| config.enabled(key);
        Ok((enabled("mods.ptbr"), enabled("mods.speedfix")))
    }

    /// Write the profile's settings for keys the player has not set; keys already in the file
    /// keep their values.
    pub fn apply_profile(&self) -> Result<()> {
        let mut config = self.config()?;
        for (key, value) in self.profile.settings {
            if config.get(key).is_none() {
                config.set(key, value);
            }
        }
        write_config(&self.config_path(), &config)
    }

    /// Set one value in `config.ini`, leaving every other line as it is.
    pub fn set_value(&self, key: &str, value: &str) -> Result<()> {
        if key.trim().is_empty() || key.contains(['=', '\n', '\r']) {
            return Err(Error::Config(format!("not a setting name: {key}")));
        }
        if value.contains(['\n', '\r']) {
            return Err(Error::Config(format!(
                "the value of {key} has a line break"
            )));
        }
        let mut config = self.config()?;
        config.set(key.trim(), value.trim());
        write_config(&self.config_path(), &config)
    }
}

fn read_config(path: &Path) -> Result<Config> {
    match fs::read_to_string(path) {
        Ok(text) => Config::parse(&text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
        Err(error) => Err(Error::Io(format!(
            "cannot read {}: {error}",
            path.display()
        ))),
    }
}

fn write_config(path: &Path, config: &Config) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| Error::Io(format!("cannot create {}: {error}", parent.display())))?;
    }
    fs::write(path, config.to_text())
        .map_err(|error| Error::Io(format!("cannot write {}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::{SettingsVm, CONFIG_FILE};
    use crate::vm::scratch;
    use acnh_core::profile;
    use std::fs;

    #[test]
    fn fan_mod_switches_start_off_and_persist_independently() {
        let root = scratch("fan-mod-settings");
        let vm = SettingsVm::new(&root, &profile::builtin()[2]);
        assert_eq!(vm.fan_mods().unwrap(), (false, false));
        vm.set_value("mods.ptbr", "1").unwrap();
        assert_eq!(vm.fan_mods().unwrap(), (true, false));
        vm.set_value("mods.speedfix", "1").unwrap();
        vm.set_value("mods.ptbr", "0").unwrap();
        assert_eq!(vm.fan_mods().unwrap(), (false, true));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn changing_a_setting_keeps_my_own_lines() {
        let root = scratch("settings-keep-lines");
        fs::create_dir_all(&root).ok();
        let original = "# my notes\n[video]\nmy.key = 7\nvideo.window = 800x600\n";
        fs::write(root.join(CONFIG_FILE), original).ok();

        let vm = SettingsVm::new(
            &root,
            profile::by_id("desktop").unwrap_or(&profile::builtin()[2]),
        );
        assert!(vm.set_value("video.frame_cap", "45").is_ok());

        let text = fs::read_to_string(root.join(CONFIG_FILE)).unwrap_or_default();
        assert!(text.starts_with("# my notes\n[video]\nmy.key = 7\nvideo.window = 800x600\n"));
        assert!(text.ends_with("video.frame_cap = 45\n"));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn profile_fills_unset_keys_and_keeps_mine() {
        let root = scratch("settings-profile");
        fs::create_dir_all(&root).ok();
        fs::write(root.join(CONFIG_FILE), "video.window = 800x600\n").ok();

        let profile = profile::by_id("amd-handheld").unwrap_or(&profile::builtin()[0]);
        let vm = SettingsVm::new(&root, profile);
        assert!(vm.apply_profile().is_ok());

        let config = vm.config().ok();
        let get = |key: &str| config.as_ref().and_then(|c| c.get(key).map(str::to_owned));
        assert_eq!(get("video.window").as_deref(), Some("800x600"));
        assert_eq!(get("video.fullscreen").as_deref(), Some("1"));
        assert_eq!(get("video.frame_cap").as_deref(), Some("30"));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn setting_rejects_a_line_break_in_the_value() {
        let root = scratch("settings-reject");
        let vm = SettingsVm::new(&root, &profile::builtin()[2]);
        assert!(vm.set_value("video.frame_cap", "30\nx = 1").is_err());
        assert!(!root.join(CONFIG_FILE).exists());
    }
}
