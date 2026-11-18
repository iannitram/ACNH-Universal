//! The Saves page's view model (SPEC.md M6, M7): the island card, the backups list (newest
//! first) and the backup folder. Plain Rust, so it is tested without a window.

use acnh_core::{Result, backup, saves};
use std::path::PathBuf;

/// What the island card says about the save folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IslandCard {
    pub found: bool,
    pub text: String,
}

/// The Saves page for one data root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavesVm {
    data_root: PathBuf,
}

impl SavesVm {
    pub fn new(data_root: impl Into<PathBuf>) -> Self {
        Self {
            data_root: data_root.into(),
        }
    }

    /// `<data root>/user/device_save`.
    pub fn save_dir(&self) -> PathBuf {
        saves::save_dir(&self.data_root)
    }

    /// `<data root>/user/backups`: the folder the Saves page opens.
    pub fn backup_folder(&self) -> PathBuf {
        saves::backups_dir(&self.data_root)
    }

    /// The island card: found when the save folder holds an island.
    pub fn island_card(&self) -> IslandCard {
        match saves::check_island(&self.save_dir()) {
            Ok(()) => IslandCard {
                found: true,
                text: "Your island is ready.".to_owned(),
            },
            Err(error) => IslandCard {
                found: false,
                text: error.to_string(),
            },
        }
    }

    /// The backup names, newest first. Names are time stamps (`YYYYmmdd-HHMMSS`), so the
    /// names sort oldest first and the list is reversed.
    pub fn backups(&self) -> Result<Vec<String>> {
        let mut names = backup::list(&self.backup_folder())?;
        names.reverse();
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::SavesVm;
    use crate::vm::scratch;
    use std::fs;

    #[test]
    fn backups_are_listed_newest_first() {
        let root = scratch("backups-newest-first");
        let backups = SavesVm::new(&root).backup_folder();
        for stamp in ["20260102-101500", "20250101-000000", "20260102-093000"] {
            fs::create_dir_all(backups.join(stamp)).ok();
        }
        let vm = SavesVm::new(&root);
        let names = vm.backups().ok();
        assert_eq!(
            names,
            Some(vec![
                "20260102-101500".to_owned(),
                "20260102-093000".to_owned(),
                "20250101-000000".to_owned(),
            ])
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn island_card_says_when_there_is_no_island() {
        let root = scratch("island-card-missing");
        let card = SavesVm::new(&root).island_card();
        assert!(!card.found);
        assert!(card.text.contains("no island"));
        fs::remove_dir_all(&root).ok();
    }
}
