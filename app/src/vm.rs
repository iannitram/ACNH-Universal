//! View state kept separate from Slint so page behavior can be tested headlessly.

pub mod about;
pub mod continue_page;
pub mod first_run;
pub mod fleet;
pub mod saves;
pub mod settings;
pub use about::AboutVm;
pub use continue_page::ContinuePageVm;

/// The input device whose hints the launcher currently displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum InputDevice {
    #[default]
    Keyboard = 0,
    Touch = 1,
    Generic = 2,
    Xbox = 3,
    PlayStation = 4,
    Switch = 5,
    SteamDeck = 6,
}

/// Navigation actions shared by keyboard and gamepad input (docs/design/continue-screen.md,
/// "Controller map").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nav {
    Up,
    Down,
    Left,
    Right,
    Accept,
    Back,
    Menu,
    Prev,
    Next,
    Quit,
}

#[allow(non_upper_case_globals)]
impl Nav {
    /// The first shell's name for `Prev`: the same value, not another action.
    pub const Previous: Nav = Nav::Prev;
    /// The first shell's name for `Accept`: the same value, not another action.
    pub const Activate: Nav = Nav::Accept;
}

/// The first shell's page list. The Continue page itself is `ContinuePageVm`.
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
            Nav::Prev => self.selected = self.selected.saturating_sub(1),
            Nav::Next => self.selected = (self.selected + 1) % SECTIONS.len(),
            Nav::Accept => return true,
            Nav::Back => self.status = "You are on the Continue screen.".to_owned(),
            Nav::Up | Nav::Down | Nav::Left | Nav::Right | Nav::Menu | Nav::Quit => {}
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
    fn visible_page_controls_are_reachable_with_navigation() {
        use super::continue_page::{ContinuePageVm, FocusTarget};
        use super::first_run::{FirstRunVm, Step};
        use super::settings::{move_focus, SettingsFocus};

        let mut page = ContinuePageVm::new(None, None);
        page.handle(Nav::Down);
        assert_eq!(page.focus(), FocusTarget::Saves);
        page.handle(Nav::Accept);
        assert!(page.saves_open());
        assert_eq!(super::saves::navigate(0, 5, Nav::Down), (1, false));
        assert_eq!(super::saves::navigate(1, 5, Nav::Back), (1, true));
        page.handle(Nav::Back);
        assert!(!page.saves_open());
        for (nav, target) in [
            (Nav::Right, FocusTarget::Settings),
            (Nav::Right, FocusTarget::Fleet),
            (Nav::Right, FocusTarget::About),
        ] {
            page.handle(nav);
            assert_eq!(page.focus(), target);
        }
        page.handle(Nav::Accept);
        assert!(page.about_open());
        page.handle(Nav::Back);
        assert!(!page.about_open());

        let mut focus = SettingsFocus::Ptbr;
        focus = move_focus(focus, Nav::Down);
        assert_eq!(focus, SettingsFocus::Speedfix);
        focus = move_focus(focus, Nav::Down);
        assert_eq!(focus, SettingsFocus::Back);
        focus = move_focus(focus, Nav::Up);
        assert_eq!(focus, SettingsFocus::Speedfix);

        let (offset, closed) = super::fleet::navigate(0, 5, Nav::Down);
        assert_eq!((offset, closed), (1, false));
        assert_eq!(super::fleet::navigate(offset, 5, Nav::Back), (1, true));

        let mut first = FirstRunVm::new(std::path::PathBuf::new());
        for step in [
            Step::Welcome,
            Step::FindGame,
            Step::PickFolder,
            Step::FindHub,
            Step::NoHubFound,
            Step::EnterAddress,
            Step::Pair,
            Step::DeviceAndSteam,
            Step::ChooseProfile,
            Step::SteamFailed(String::new()),
        ] {
            first.step = step;
            first.focus = 0;
            let count = first.options().len();
            for expected in 1..count {
                first.handle(Nav::Right);
                assert_eq!(first.focus, expected);
            }
            first.handle(Nav::Left);
            assert_eq!(first.focus, count.saturating_sub(2));
        }
    }

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
