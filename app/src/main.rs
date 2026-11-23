use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use acnh_app::first_run::FirstRun;
use acnh_app::input::{key_nav, poll_nav, DeviceEvent, Devices, Stick};
use acnh_app::play::{Player, RelayOff, Runner, Session};
use acnh_app::ui::{from_slint, to_slint};
use acnh_app::vm::fleet::FleetVm;
use acnh_app::vm::saves::SavesVm;
use acnh_app::vm::settings::{move_focus, SettingsFocus, SettingsVm};
use acnh_app::vm::{AboutVm, ContinuePageVm, Nav};
use acnh_app::MainWindow;
use acnh_app::{CreditEntry, FixEntry, FleetEntry, SettingsControl};
use acnh_core::platform::{AppDirs, Os, Vars};
use acnh_core::{assemble, backup, savecard, saves};
use slint::{ComponentHandle, SharedString, Timer, TimerMode, Weak};

/// The Continue page for this device: its island, its player and its folders.
fn session(picture: PathBuf) -> acnh_core::Result<Session> {
    let os = Os::current();
    let vars = Vars::from_lookup(os, |name| std::env::var(name).ok())?;
    let dirs = AppDirs::new(&vars);
    let root = dirs.data_root();
    let save_dir = saves::save_dir(&root);

    // `this_device` and `set_saved_by` take the fleet's records once the app keeps them
    // (the Fleet page); with neither, no drift is reported.
    let mut vm = ContinuePageVm::new(savecard::read_card(&save_dir, &picture).ok(), None);
    vm.home = vars.home.clone();

    let backups = saves::backups_dir(&root);
    let mut env = vec![("NX_DATA_ROOT".to_string(), root.display().to_string())];
    let prepared = dirs.data.join("prepare");
    let completed_here = dirs.config.join("first-run-complete").is_file()
        && prepared.join("analysis/main.bin").is_file();
    let launcher = if completed_here {
        env.push(("NX_GAME_ROOT".into(), prepared.display().to_string()));
        PathBuf::new()
    } else {
        std::env::current_exe().unwrap_or_default()
    };
    let program = assemble::player(os, &launcher, &dirs.install());
    let runner = Runner {
        // No relay is set up from the app yet (Settings): the island here is played, and
        // nothing is pulled or pushed.
        relay: Arc::new(RelayOff),
        game: Arc::new(Player { program }),
        save_dir,
        env,
        backup: Arc::new(move |save_dir: &Path| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|since| since.as_secs())
                .unwrap_or(0);
            backup::backup_saves(save_dir, &backups, &backup::stamp(now))?;
            Ok(())
        }),
    };
    Ok(Session::new(vm, runner, picture))
}

fn render(window: &Weak<MainWindow>, session: &Session) {
    if let Some(window) = window.upgrade() {
        if !session.vm().settings_open() && window.get_view().settings_open {
            window.set_settings_focus(SettingsControl::Ptbr);
            window.set_settings_error(SharedString::default());
        }
        let view = session.vm().view();
        if view.saves_open {
            if !window.get_view().saves_open {
                window.set_saves_offset(0);
            }
            if let Ok(vars) = Vars::from_lookup(Os::current(), |name| std::env::var(name).ok()) {
                let saves = SavesVm::new(AppDirs::new(&vars).data_root());
                window.set_saves_island(saves.island_card().text.into());
                let (backups, message) = match saves.backups() {
                    Ok(backups) if backups.is_empty() => (backups, "No backups yet."),
                    Ok(backups) => (backups, ""),
                    Err(_) => (Vec::new(), "Could not read backups."),
                };
                let offset = (window.get_saves_offset().max(0) as usize)
                    .min(backups.len().saturating_sub(3));
                window.set_saves_offset(offset as i32);
                window.set_saves_has_previous(offset > 0);
                window.set_saves_has_next(offset + 3 < backups.len());
                window.set_saves_position(if backups.len() > 3 {
                    format!(
                        "Showing {}–{} of {}",
                        offset + 1,
                        (offset + 3).min(backups.len()),
                        backups.len()
                    )
                    .into()
                } else {
                    "".into()
                });
                let rows: Vec<SharedString> = backups
                    .into_iter()
                    .skip(offset)
                    .take(3)
                    .map(Into::into)
                    .collect();
                window.set_saves_backups(slint::ModelRc::new(slint::VecModel::from(rows)));
                window.set_saves_message(message.into());
            }
        }
        if view.fleet_open {
            if !window.get_view().fleet_open {
                window.set_fleet_offset(0);
            }
            if let Ok(vars) = Vars::from_lookup(Os::current(), |name| std::env::var(name).ok()) {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|v| v.as_secs())
                    .unwrap_or(0);
                let fleet =
                    FleetVm::load(&AppDirs::new(&vars), session.vm().this_device.as_ref(), now);
                let max = fleet.rows.len().saturating_sub(3);
                let offset = (window.get_fleet_offset().max(0) as usize).min(max);
                window.set_fleet_offset(offset as i32);
                window.set_fleet_has_previous(offset > 0);
                window.set_fleet_has_next(offset < max);
                let entries: Vec<FleetEntry> = fleet
                    .rows
                    .iter()
                    .skip(offset)
                    .take(3)
                    .map(|row| FleetEntry {
                        device: row.device.as_str().into(),
                        details: row.details.as_str().into(),
                        status: row.status.as_str().into(),
                        drifting: row.drifting,
                    })
                    .collect();
                window.set_fleet_rows(slint::ModelRc::new(slint::VecModel::from(entries)));
                window.set_fleet_message(fleet.message.into());
                window.set_fleet_comparison(fleet.comparison.into());
                window.set_fleet_position(if fleet.rows.len() > 3 {
                    format!(
                        "Showing {}–{} of {}",
                        offset + 1,
                        (offset + 3).min(fleet.rows.len()),
                        fleet.rows.len()
                    )
                    .into()
                } else {
                    "".into()
                });
            }
        }
        window.set_view(to_slint(&view));
    }
}

fn move_settings_focus(focus: SettingsControl, nav: Nav) -> SettingsControl {
    let focus = match focus {
        SettingsControl::Ptbr => SettingsFocus::Ptbr,
        SettingsControl::Speedfix => SettingsFocus::Speedfix,
        SettingsControl::Back => SettingsFocus::Back,
    };
    match move_focus(focus, nav) {
        SettingsFocus::Ptbr => SettingsControl::Ptbr,
        SettingsFocus::Speedfix => SettingsControl::Speedfix,
        SettingsFocus::Back => SettingsControl::Back,
    }
}

fn navigate(window: &Weak<MainWindow>, session: &mut Session, nav: Nav) {
    if let Some(window) = window.upgrade().filter(|_| session.vm().saves_open()) {
        let offset = window.get_saves_offset().max(0) as usize;
        let rows = if window.get_saves_has_next() {
            offset + 4
        } else {
            offset + 3
        };
        let (offset, close) = acnh_app::vm::saves::navigate(offset, rows, nav);
        window.set_saves_offset(offset as i32);
        if close {
            session.nav(Nav::Back);
        }
        return;
    }
    if let Some(window) = window.upgrade().filter(|_| session.vm().fleet_open()) {
        // The row count is reflected by the existing previous/next flags.
        let offset = window.get_fleet_offset().max(0) as usize;
        let rows = if window.get_fleet_has_next() {
            offset + 4
        } else {
            offset + 3
        };
        let (offset, close) = acnh_app::vm::fleet::navigate(offset, rows, nav);
        window.set_fleet_offset(offset as i32);
        if close {
            session.nav(Nav::Back);
        }
        return;
    }
    let Some(window) = window.upgrade().filter(|_| session.vm().settings_open()) else {
        session.nav(nav);
        return;
    };
    let focus = window.get_settings_focus();
    if nav == Nav::Accept {
        match focus {
            SettingsControl::Ptbr => window.invoke_set_ptbr(!window.get_ptbr()),
            SettingsControl::Speedfix => window.invoke_set_speedfix(!window.get_speedfix()),
            SettingsControl::Back => {
                session.nav(Nav::Back);
            }
        }
    } else if nav == Nav::Back {
        session.nav(nav);
    } else {
        window.set_settings_focus(move_settings_focus(focus, nav));
    }
}

fn save_fan_mod(window: &MainWindow, settings: &SettingsVm, control: SettingsControl, value: bool) {
    let key = match control {
        SettingsControl::Ptbr => "mods.ptbr",
        SettingsControl::Speedfix => "mods.speedfix",
        SettingsControl::Back => return,
    };
    let saved = settings
        .set_value(key, if value { "1" } else { "0" })
        .is_ok();
    let shown = if saved { value } else { !value };
    if control == SettingsControl::Ptbr {
        window.set_ptbr(shown);
    } else {
        window.set_speedfix(shown);
    }
    window.set_settings_focus(control);
    window.set_settings_error(if saved {
        SharedString::default()
    } else {
        "Could not save the setting. Check that config.ini is writable.".into()
    });
}

fn render_first_run(window: &Weak<MainWindow>, first: &mut FirstRun) {
    let Some(window) = window.upgrade() else {
        return;
    };
    if first.done() {
        match first.complete() {
            Ok(()) => {
                window.set_first_run_open(false);
                return;
            }
            Err(_) => first.vm.message = "Could not save setup state.".into(),
        }
    }
    let options = first.vm.options();
    window.set_first_run_title(
        format!("Step {} of 6\n{}", first.vm.step_number(), first.vm.title()).into(),
    );
    window.set_first_run_choice(
        options
            .get(first.vm.focus)
            .map(String::as_str)
            .unwrap_or("")
            .into(),
    );
    window.set_first_run_position(if options.is_empty() {
        "".into()
    } else {
        format!("{} of {}", first.vm.focus + 1, options.len()).into()
    });
    window.set_first_run_busy(first.vm.busy);
    window.set_first_run_message(first.vm.message.as_str().into());
    window.set_first_run_cancel_available(first.can_cancel());
    window.set_first_run_keypad(first.vm.keypad_open());
    window.set_first_run_focus_index(first.vm.focus as i32);
    let choices: Vec<SharedString> = options.into_iter().map(Into::into).collect();
    window.set_first_run_choices(slint::ModelRc::new(slint::VecModel::from(choices)));
    let progress = if first.vm.step == acnh_app::vm::first_run::Step::GetBuild {
        first
            .vm
            .stage
            .and_then(|stage| acnh_core::recipe::STAGES.iter().position(|s| *s == stage))
            .map(|n| (n * 100 / 7) as i32)
            .unwrap_or(0)
    } else {
        -1
    };
    window.set_first_run_progress(progress);
}

fn set_about(window: &MainWindow) {
    let about = AboutVm::new();
    let upstream = about.upstream();
    window.set_about_upstream(format!("{} ({})", upstream.name, upstream.licence).into());
    window.set_about_slint_notice(about.slint_notice().into());
    let credits: Vec<CreditEntry> = about
        .credits()
        .iter()
        .map(|c| CreditEntry {
            name: c.name.into(),
            licence: c.licence.into(),
            role: c.role.into(),
        })
        .collect();
    window.set_about_credits(slint::ModelRc::new(slint::VecModel::from(credits)));
    let fixes: Vec<FixEntry> = about
        .offered_fixes()
        .into_iter()
        .map(|f| FixEntry {
            title: f.title.as_str().into(),
            link: f.link.as_str().into(),
        })
        .collect();
    window.set_about_fixes(slint::ModelRc::new(slint::VecModel::from(fixes)));
}

fn fullscreen(flag: bool, desktop: Option<&str>, gamepad_ui: bool) -> bool {
    flag || desktop == Some("gamescope") || gamepad_ui
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let window = MainWindow::new()?;
    window.window().set_fullscreen(fullscreen(
        std::env::args_os().any(|arg| arg == "--fullscreen"),
        std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
        std::env::var_os("SteamGamepadUI").is_some(),
    ));
    set_about(&window);
    let devices = Rc::new(RefCell::new(Devices::default()));
    window.set_input_device(devices.borrow().current() as i32);
    window.on_pointer_used({
        let devices = Rc::clone(&devices);
        let weak = window.as_weak();
        move || {
            devices.borrow_mut().apply(DeviceEvent::Touch);
            if let Some(window) = weak.upgrade() {
                window.set_input_device(devices.borrow().current() as i32);
            }
        }
    });
    let vars = Vars::from_lookup(Os::current(), |name| std::env::var(name).ok())?;
    let settings = Rc::new(SettingsVm::new(
        AppDirs::new(&vars).data_root(),
        &acnh_core::profile::builtin()[2],
    ));
    let (ptbr, speedfix) = settings.fan_mods()?;
    window.set_ptbr(ptbr);
    window.set_speedfix(speedfix);
    window.on_set_ptbr({
        let settings = Rc::clone(&settings);
        let weak = window.as_weak();
        move |value| {
            if let Some(window) = weak.upgrade() {
                save_fan_mod(&window, &settings, SettingsControl::Ptbr, value);
            }
        }
    });
    window.on_set_speedfix({
        let settings = Rc::clone(&settings);
        let weak = window.as_weak();
        move |value| {
            if let Some(window) = weak.upgrade() {
                save_fan_mod(&window, &settings, SettingsControl::Speedfix, value);
            }
        }
    });
    let first = Rc::new(RefCell::new(FirstRun::new(vars.clone())));
    window.set_first_run_open(first.borrow().needed());
    render_first_run(&window.as_weak(), &mut first.borrow_mut());
    window.on_first_run_choose({
        let first = Rc::clone(&first);
        let weak = window.as_weak();
        move |index| {
            let mut first = first.borrow_mut();
            first.click(index as usize);
            render_first_run(&weak, &mut first);
        }
    });
    window.on_first_run_nav({
        let first = Rc::clone(&first);
        let weak = window.as_weak();
        move |index| {
            let nav = match index {
                0 => Nav::Up,
                1 => Nav::Down,
                2 => Nav::Accept,
                _ => Nav::Back,
            };
            let mut first = first.borrow_mut();
            first.nav(nav);
            render_first_run(&weak, &mut first);
        }
    });
    let picture = acnh_core::steam::assets_dir()?.join("acnhrecomp.png");
    if let Ok(image) = image::open(&picture) {
        let rgba = image.into_rgba8();
        let pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
            rgba.as_raw(),
            rgba.width(),
            rgba.height(),
        );
        window.set_card_picture(slint::Image::from_rgba8(pixels));
    }
    let session = Rc::new(RefCell::new(session(picture)?));
    render(&window.as_weak(), &session.borrow());

    window.on_saves_scroll({
        let weak = window.as_weak();
        let session = Rc::clone(&session);
        move |delta| {
            if let Some(window) = weak.upgrade() {
                window.set_saves_offset(window.get_saves_offset().saturating_add(delta).max(0));
                render(&weak, &session.borrow());
            }
        }
    });

    window.on_fleet_scroll({
        let weak = window.as_weak();
        let session = Rc::clone(&session);
        move |delta| {
            if let Some(window) = weak.upgrade() {
                window.set_fleet_offset(window.get_fleet_offset().saturating_add(delta).max(0));
                render(&weak, &session.borrow());
            }
        }
    });

    window.on_clicked({
        let weak = window.as_weak();
        let session = Rc::clone(&session);
        move |target| {
            if weak
                .upgrade()
                .is_some_and(|window| window.get_first_run_open())
            {
                return;
            }
            let mut session = session.borrow_mut();
            session.click(from_slint(target));
            render(&weak, &session);
        }
    });

    window.on_key({
        let devices = Rc::clone(&devices);
        let weak = window.as_weak();
        let first = Rc::clone(&first);
        let session = Rc::clone(&session);
        move |text: SharedString| {
            devices.borrow_mut().apply(DeviceEvent::Keyboard);
            if let Some(window) = weak.upgrade() {
                window.set_input_device(devices.borrow().current() as i32);
            }
            if weak.upgrade().is_some_and(|w| w.get_first_run_open()) {
                let mut first = first.borrow_mut();
                if first.vm.step == acnh_app::vm::first_run::Step::Pair && !first.vm.busy {
                    if text.starts_with(char::from(slint::platform::Key::Backspace)) {
                        first.vm.activate(10);
                        render_first_run(&weak, &mut first);
                        return true;
                    }
                    if let Some(digit) = text.chars().next().and_then(|c| c.to_digit(10)) {
                        first.vm.activate(digit as usize);
                        render_first_run(&weak, &mut first);
                        return true;
                    }
                }
                if first.vm.step == acnh_app::vm::first_run::Step::EnterAddress && !first.vm.busy {
                    if text.starts_with(char::from(slint::platform::Key::Backspace)) {
                        first.vm.delete_address();
                        render_first_run(&weak, &mut first);
                        return true;
                    }
                    if let Some(c) = text
                        .chars()
                        .next()
                        .filter(|c| c.is_ascii_hexdigit() || matches!(c, '.' | ':'))
                    {
                        first.vm.insert_address(c);
                        render_first_run(&weak, &mut first);
                        return true;
                    }
                }
            }
            let Some(nav) = key_nav(&text) else {
                return false;
            };
            if weak.upgrade().is_some_and(|w| w.get_first_run_open()) {
                let mut first = first.borrow_mut();
                first.nav(nav);
                render_first_run(&weak, &mut first);
                return true;
            }
            let mut session = session.borrow_mut();
            navigate(&weak, &mut session, nav);
            render(&weak, &session);
            true
        }
    });

    // Every frame: the gamepad's presses go in, and what the running job reported comes out.
    let mut gilrs = gilrs::Gilrs::new().ok();
    let mut stick = Stick::default();
    let timer = Timer::default();
    let weak = window.as_weak();
    let mut was_first_open = window.get_first_run_open();
    let mut fleet_tick = 0_u32;
    timer.start(TimerMode::Repeated, Duration::from_millis(16), move || {
        let navs = match gilrs.as_mut() {
            Some(gilrs) => poll_nav(gilrs, &mut stick, &mut devices.borrow_mut()),
            None => Vec::new(),
        };
        if let Some(window) = weak.upgrade() {
            window.set_input_device(devices.borrow().current() as i32);
        }
        let first_open = weak.upgrade().is_some_and(|w| w.get_first_run_open());
        if first_open {
            let mut first = first.borrow_mut();
            for nav in &navs {
                first.nav(*nav);
            }
            if first.pump() || !navs.is_empty() {
                render_first_run(&weak, &mut first);
            }
        }
        let mut session = session.borrow_mut();
        let now_open = weak.upgrade().is_some_and(|w| w.get_first_run_open());
        if was_first_open && !now_open {
            let first = first.borrow();
            session.set_prepared_player(first.prepared_player(), &first.game_root());
        }
        was_first_open = now_open;
        if !first_open {
            for nav in &navs {
                navigate(&weak, &mut session, *nav);
            }
        }
        fleet_tick = fleet_tick.wrapping_add(1);
        let refresh_fleet = fleet_tick.is_multiple_of(60)
            && weak
                .upgrade()
                .is_some_and(|window| window.get_view().fleet_open);
        if session.pump() || !navs.is_empty() || refresh_fleet {
            render(&weak, &session);
        }
    });

    window.run()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullscreen_follows_arguments_and_gamescope() {
        assert!(!fullscreen(false, None, false));
        assert!(fullscreen(true, None, false));
        assert!(fullscreen(false, Some("gamescope"), false));
        assert!(fullscreen(false, None, true));
        assert!(!fullscreen(false, Some("desktop"), false));
    }

    #[test]
    fn settings_focus_reaches_both_switches_and_back() {
        use SettingsControl::{Back, Ptbr, Speedfix};

        assert_eq!(move_settings_focus(Ptbr, Nav::Down), Speedfix);
        assert_eq!(move_settings_focus(Speedfix, Nav::Down), Back);
        assert_eq!(move_settings_focus(Back, Nav::Down), Back);
        assert_eq!(move_settings_focus(Back, Nav::Up), Speedfix);
        assert_eq!(move_settings_focus(Speedfix, Nav::Up), Ptbr);
    }
}
