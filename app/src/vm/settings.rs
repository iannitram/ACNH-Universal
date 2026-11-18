//! The Settings page's view model (SPEC.md M2): the device profile and the `config.ini`
//! values it writes. Writing keeps every line of the player's file that is not changed,
//! comments and unknown keys included; a profile only fills keys the player has not set.

use acnh_core::{Error, Result, config::Config, profile::DeviceProfile};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

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
    use super::{CONFIG_FILE, SettingsVm};
    use crate::vm::scratch;
    use acnh_core::profile;
    use std::fs;

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
