//! Android Slint launcher entry point (`android_main`, gated behind `android-launcher`) and
//! host-testable key-event/state routing.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::input::{android_keycode_nav, AndroidDevice, AndroidHotplug, Devices};
use crate::shell::ShellConfig;
use crate::vm::Nav;
use acnh_core::platform::{AppDirs, Os, Vars};

/// Raw key event fields extracted from `android_activity::input::KeyEvent` (or unit tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AndroidKeyInput {
    pub device_id: i32,
    pub keycode: i32,
    pub is_down: bool,
    pub repeat_count: i32,
}

/// Bridges Android `KeyEvent`s and `InputDevice` hot-plug snapshots into `Devices` + `Nav`.
#[derive(Debug, Default)]
pub struct AndroidKeyRouter {
    hotplug: AndroidHotplug,
    pending: Vec<AndroidKeyInput>,
}

impl AndroidKeyRouter {
    /// Queue an Android key event for the next launcher frame.
    pub fn push(&mut self, event: AndroidKeyInput) {
        self.pending.push(event);
    }

    /// Poll `InputDevice` hot-plug changes if due, record `device_id` usage for queued key-down
    /// events, and return the resulting `Nav` actions in arrival order.
    pub fn drain_nav(
        &mut self,
        now: Instant,
        devices: &mut Devices,
        fetch_devices: impl FnOnce() -> Vec<AndroidDevice>,
    ) -> Vec<Nav> {
        self.hotplug.poll_if_due(now, devices, fetch_devices);
        let events = std::mem::take(&mut self.pending);
        let mut navs = Vec::new();
        for event in events {
            if !event.is_down || event.repeat_count > 0 {
                continue;
            }
            self.hotplug.record_used(event.device_id, devices);
            if let Some(nav) = android_keycode_nav(event.keycode) {
                navs.push(nav);
            }
        }
        navs
    }
}

/// Build the launcher `ShellConfig` for Android from the app's private `<files>` directory
/// without calling `Vars::from_lookup(Os::Android, ...)`.
pub fn android_shell_config(files_dir: &Path) -> ShellConfig {
    let vars = Vars {
        home: files_dir.to_path_buf(),
        data: files_dir.to_path_buf(),
        config: files_dir.to_path_buf(),
    };
    let dirs = AppDirs::for_os(Os::Android, &vars);
    let launcher = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("acnh-app"));
    ShellConfig {
        vars,
        dirs,
        os: Os::Android,
        launcher,
        picture: files_dir.join("acnhrecomp.png"),
        fullscreen: true,
    }
}

#[cfg(all(target_os = "android", feature = "android-launcher"))]
#[unsafe(no_mangle)]
pub extern "C" fn android_main(app: slint::android::AndroidApp) {
    use std::cell::RefCell;
    use std::rc::Rc;

    use android_activity::input::{InputEvent, KeyAction};
    use android_activity::{InputStatus, MainEvent, PollEvent};

    let router = Rc::new(RefCell::new(AndroidKeyRouter::default()));
    let queue = Rc::clone(&router);
    let app_for_listener = app.clone();
    if slint::android::init_with_event_listener(app.clone(), move |event| {
        if let PollEvent::Main(MainEvent::InputAvailable) = event {
            if let Ok(mut iter) = app_for_listener.input_events_iter() {
                loop {
                    let read = iter.next(|input_event| {
                        if let InputEvent::KeyEvent(key_event) = input_event {
                            let is_down = key_event.action() == KeyAction::Down;
                            let keycode = u32::from(key_event.key_code()) as i32;
                            let repeat_count = key_event.repeat_count();
                            let device_id = key_event.device_id();
                            if is_down
                                && repeat_count == 0
                                && android_keycode_nav(keycode).is_some()
                            {
                                queue.borrow_mut().push(AndroidKeyInput {
                                    device_id,
                                    keycode,
                                    is_down,
                                    repeat_count,
                                });
                                return InputStatus::Handled;
                            }
                        }
                        InputStatus::Unhandled
                    });
                    if !read {
                        break;
                    }
                }
            }
        }
    })
    .is_err()
    {
        return;
    }

    let files_dir = app
        .internal_data_path()
        .unwrap_or_else(|| PathBuf::from("/data/local/tmp"));
    let config = android_shell_config(&files_dir);
    let _ = crate::shell::run(
        config,
        Box::new(move |devices| {
            router.borrow_mut().drain_nav(
                Instant::now(),
                devices,
                crate::input::android::connected_devices,
            )
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{
        AKEYCODE_BACK, AKEYCODE_BUTTON_A, AKEYCODE_BUTTON_B, AKEYCODE_BUTTON_L1,
        AKEYCODE_BUTTON_R1, AKEYCODE_BUTTON_START, AKEYCODE_DPAD_DOWN, AKEYCODE_DPAD_LEFT,
        AKEYCODE_DPAD_RIGHT, AKEYCODE_DPAD_UP, ANDROID_SOURCE_GAMEPAD,
    };
    use crate::vm::{ContinuePageVm, InputDevice};

    fn switch_pad(id: i32) -> AndroidDevice {
        AndroidDevice {
            id,
            vendor: Some(0x057e),
            product: Some(0x2009),
            name: "Pro Controller".into(),
            sources: ANDROID_SOURCE_GAMEPAD,
            is_virtual: false,
        }
    }

    #[test]
    fn android_shell_config_builds_dirs_without_env_lookup() {
        let files = Path::new("/data/user/0/io.github.acnhrecomp/files");
        let cfg = android_shell_config(files);
        assert_eq!(cfg.os, Os::Android);
        assert!(cfg.fullscreen);
        assert_eq!(cfg.vars.home, files);
        assert_eq!(
            cfg.dirs.data_root(),
            files.join("acnh-universal/install/root")
        );
        assert_eq!(cfg.dirs.config, files.join("acnh-universal"));
    }

    #[test]
    fn android_key_router_maps_pad_buttons_and_tracks_device_id() {
        let mut router = AndroidKeyRouter::default();
        let mut devices = Devices::default();
        assert_eq!(devices.current(), InputDevice::Keyboard);

        let now = Instant::now();
        // Key-up and repeats are ignored.
        router.push(AndroidKeyInput {
            device_id: 7,
            keycode: AKEYCODE_BUTTON_A,
            is_down: false,
            repeat_count: 0,
        });
        router.push(AndroidKeyInput {
            device_id: 7,
            keycode: AKEYCODE_BUTTON_A,
            is_down: true,
            repeat_count: 1,
        });
        let navs = router.drain_nav(now, &mut devices, || vec![switch_pad(7)]);
        assert!(navs.is_empty());
        assert_eq!(devices.current(), InputDevice::Keyboard);

        // Fresh key-down from the known pad updates Devices::current() and emits Nav.
        for (keycode, expected) in [
            (AKEYCODE_DPAD_UP, Nav::Up),
            (AKEYCODE_DPAD_DOWN, Nav::Down),
            (AKEYCODE_DPAD_LEFT, Nav::Left),
            (AKEYCODE_DPAD_RIGHT, Nav::Right),
            (AKEYCODE_BUTTON_A, Nav::Accept),
            (AKEYCODE_BUTTON_B, Nav::Back),
            (AKEYCODE_BACK, Nav::Back),
            (AKEYCODE_BUTTON_START, Nav::Menu),
            (AKEYCODE_BUTTON_L1, Nav::Prev),
            (AKEYCODE_BUTTON_R1, Nav::Next),
        ] {
            router.push(AndroidKeyInput {
                device_id: 7,
                keycode,
                is_down: true,
                repeat_count: 0,
            });
            let navs = router.drain_nav(now, &mut devices, Vec::new);
            assert_eq!(navs, vec![expected]);
            assert_eq!(devices.current(), InputDevice::Switch);
        }
    }

    #[test]
    fn android_key_router_drives_continue_page_vm() {
        let mut router = AndroidKeyRouter::default();
        let mut devices = Devices::default();
        let mut vm = ContinuePageVm::new(None, None);
        assert!(!vm.view().about_open);

        router.push(AndroidKeyInput {
            device_id: 3,
            keycode: AKEYCODE_DPAD_DOWN,
            is_down: true,
            repeat_count: 0,
        });
        router.push(AndroidKeyInput {
            device_id: 3,
            keycode: AKEYCODE_DPAD_RIGHT,
            is_down: true,
            repeat_count: 0,
        });
        let navs = router.drain_nav(Instant::now(), &mut devices, || vec![switch_pad(3)]);
        for nav in navs {
            vm.handle(nav);
        }
        assert_eq!(devices.current(), InputDevice::Switch);
    }
}
