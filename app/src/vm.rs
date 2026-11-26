//! View state kept separate from Slint so page behavior can be tested headlessly.

pub mod about;
pub mod continue_page;
pub mod first_run;
pub mod fleet;
pub mod hints;
pub mod relay_setup;
pub mod saves;
pub mod settings;
pub mod text_entry;
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

/// A fresh folder under the system temp dir for one test; the test removes it when done.
#[cfg(test)]
pub(crate) fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("acnh-app-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    dir
}

#[cfg(test)]
mod tests {
    use super::Nav;

    #[test]
    fn visible_page_controls_are_reachable_with_navigation() {
        use super::continue_page::{ContinuePageVm, FocusTarget};
        use super::first_run::{FirstRunVm, Step};
        use super::settings::SettingsFocus;

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

        // The save sync step in each phase, an open keyboard included, through the one focus
        // accessor the page draws from: Right reaches every choice or key in turn, and Back
        // always leaves the step.
        use super::relay_setup::{Event as RelayEvent, Prompt};
        let at_relay = || {
            let mut vm = FirstRunVm::new(std::path::PathBuf::new());
            vm.relay_available = true;
            vm.step = Step::GetBuild;
            vm.event(super::first_run::Event::Finished(Ok(())));
            assert_eq!(vm.step, Step::Relay);
            vm
        };
        let prompt = Prompt {
            url: "https://example.test".into(),
            code: None,
            needs_code: true,
            browser: false,
        };
        type Make = Box<dyn Fn() -> FirstRunVm>;
        let phases: Vec<(&str, Make)> = vec![
            ("choose", Box::new(at_relay)),
            (
                "form",
                Box::new(move || {
                    let mut vm = at_relay();
                    vm.handle(Nav::Accept);
                    vm
                }),
            ),
            (
                "keyboard",
                Box::new(move || {
                    let mut vm = at_relay();
                    vm.handle(Nav::Accept);
                    vm.handle(Nav::Accept);
                    assert!(vm.keypad_open());
                    vm
                }),
            ),
            (
                "waiting",
                Box::new({
                    let prompt = prompt.clone();
                    move || {
                        let mut vm = at_relay();
                        vm.activate(2);
                        vm.relay.take_request();
                        vm.relay
                            .event(RelayEvent::SignInStarted(Ok(prompt.clone())));
                        vm
                    }
                }),
            ),
            (
                "failed",
                Box::new(move || {
                    let mut vm = at_relay();
                    vm.activate(1);
                    vm.relay.take_request();
                    vm.relay.event(RelayEvent::SignInStarted(Err("no".into())));
                    vm
                }),
            ),
        ];
        for (name, make) in &phases {
            let mut vm = make();
            let count = vm.options().len();
            assert!(count > 0, "{name}");
            for _ in 0..count {
                vm.handle(Nav::Left);
            }
            assert_eq!(vm.focus_index(), 0, "{name}");
            for expected in 1..count {
                vm.handle(Nav::Right);
                assert_eq!(vm.focus_index(), expected, "{name}");
            }
            let mut vm = make();
            for _ in 0..3 {
                vm.handle(Nav::Back);
                if vm.step != Step::Relay {
                    break;
                }
            }
            assert_eq!(vm.step, Step::FindGame, "{name}: Back must leave");
        }

        // Settings with each set of save sync rows: Down visits every row, ending on Back,
        // and Up returns to the first.
        use super::settings::{move_in, settings_rows, RelayRows};
        for relay in [
            RelayRows::Hidden,
            RelayRows::SignIn,
            RelayRows::SignInAndOff,
        ] {
            let rows = settings_rows(relay);
            let mut at = rows[0];
            let mut seen = vec![at];
            for _ in 0..rows.len() {
                at = move_in(&rows, at, Nav::Down);
                if seen.last() != Some(&at) {
                    seen.push(at);
                }
            }
            assert_eq!(seen, rows);
            assert_eq!(*rows.last().unwrap(), SettingsFocus::Back);
            for _ in 0..rows.len() {
                at = move_in(&rows, at, Nav::Up);
            }
            assert_eq!(at, rows[0]);
        }
    }
}
