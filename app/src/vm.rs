//! View state kept separate from Slint so page behavior can be tested headlessly.

pub mod saves;
pub mod settings;

/// Navigation actions shared by keyboard and gamepad input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nav {
    Previous,
    Next,
    Activate,
    Back,
}

/// State for the initial Continue screen.
#[derive(Debug, PartialEq, Eq)]
pub struct ContinueVm {
    selected: usize,
    status: String,
}

const SECTIONS: [&str; 5] = ["Continue", "Saves", "Settings", "Fleet", "About"];

impl Default for ContinueVm {
    fn default() -> Self {
        Self {
            selected: 0,
            status: "Ready to sync and play.".to_owned(),
        }
    }
}

impl ContinueVm {
    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn selection(&self) -> &'static str {
        SECTIONS[self.selected]
    }

    /// Apply a navigation action and return whether activation was requested.
    pub fn handle(&mut self, nav: Nav) -> bool {
        match nav {
            Nav::Previous => self.selected = self.selected.saturating_sub(1),
            Nav::Next => self.selected = (self.selected + 1) % SECTIONS.len(),
            Nav::Activate => return true,
            Nav::Back => self.status = "You are on the Continue screen.".to_owned(),
        }
        false
    }

    pub fn activate(&mut self) {
        self.status = if self.selected == 0 {
            "Sync and play is coming soon.".to_owned()
        } else {
            format!("{} is coming soon.", self.selection())
        };
    }
}

/// A fresh folder under the system temp dir for one test; the test removes it when done.
#[cfg(test)]
pub(crate) fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("acnh-app-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    dir
}

#[cfg(test)]
mod tests {
    use super::{ContinueVm, Nav};

    #[test]
    fn continue_view_model_handles_navigation_without_a_window() {
        let mut vm = ContinueVm::default();
        assert_eq!(vm.selected(), 0);
        assert!(!vm.handle(Nav::Previous));
        assert_eq!(vm.selected(), 0);
        assert!(!vm.handle(Nav::Next));
        assert_eq!(vm.selection(), "Saves");
        assert!(!vm.handle(Nav::Previous));
        assert!(vm.handle(Nav::Activate));

        vm.activate();
        assert_eq!(vm.status(), "Sync and play is coming soon.");
    }
}
