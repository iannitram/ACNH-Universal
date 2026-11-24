use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use acnh_app::first_run::{route_text, system_relay_setup, FirstRun};
use acnh_app::input::{key_nav, poll_nav, DeviceEvent, Devices, Stick};
use acnh_app::play::{Player, Runner, Session};
use acnh_app::relay_setup::{PageView, RecheckWhenClosed, RelaySettings};
use acnh_app::ui::{from_slint, to_slint};
use acnh_app::vm::fleet::FleetVm;
use acnh_app::vm::saves::SavesVm;
use acnh_app::vm::settings::{move_in, settings_rows, RelayRows, SettingsFocus, SettingsVm};
use acnh_app::vm::{AboutVm, ContinuePageVm, Nav};
use acnh_app::MainWindow;
use acnh_app::{CreditEntry, FixEntry, FleetEntry, SettingsControl};
use acnh_core::platform::{AppDirs, Os, Vars};
use acnh_core::{backup, savecard, saves};
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
    let builds = acnh_app::build_actions::BuildActions { dirs, os };
    let launcher = std::env::current_exe()?;
    let setup = acnh_app::player_env::player_setup(&builds, &launcher)?;
    let (program, env) = (setup.program, setup.env);
    if setup.build_missing {
        vm.set_build_missing(builds.game_folder().ok().flatten().is_some());
    }
    let runner = Runner {
        // Save sync as this machine has it set up: the real relay, `RelayOff` (the page says
        // save sync is off and Play goes on without it), or an error that says why.
        relay: acnh_app::relay::system(),
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
    let mut session = Session::new(vm, runner, picture);
    session.set_build_actions(builds, launcher);
    Ok(session)
}

fn render(window: &Weak<MainWindow>, session: &Session) {
    if let Some(window) = window.upgrade() {
        if !session.vm().settings_open() && window.get_view().settings_open {
            window.set_settings_focus(SettingsControl::Ptbr);
            window.set_settings_error(SharedString::default());
        }
        let view = session.vm().view();
        window.set_saves_relay(view.saves_relay.as_str().into());
        window.set_saves_sync_enabled(view.sync_now);
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

fn to_focus(control: SettingsControl) -> SettingsFocus {
    match control {
        SettingsControl::Ptbr => SettingsFocus::Ptbr,
        SettingsControl::Speedfix => SettingsFocus::Speedfix,
        SettingsControl::RelaySignIn => SettingsFocus::RelaySignIn,
        SettingsControl::RelayOff => SettingsFocus::RelayOff,
        SettingsControl::Back => SettingsFocus::Back,
    }
}

fn to_control(focus: SettingsFocus) -> SettingsControl {
    match focus {
        SettingsFocus::Ptbr => SettingsControl::Ptbr,
        SettingsFocus::Speedfix => SettingsControl::Speedfix,
        SettingsFocus::RelaySignIn => SettingsControl::RelaySignIn,
        SettingsFocus::RelayOff => SettingsControl::RelayOff,
        SettingsFocus::Back => SettingsControl::Back,
    }
}

fn move_settings_focus(focus: SettingsControl, nav: Nav, relay: RelayRows) -> SettingsControl {
    to_control(move_in(&settings_rows(relay), to_focus(focus), nav))
}

fn navigate(window: &Weak<MainWindow>, session: &mut Session, relay: &mut RelaySettings, nav: Nav) {
    if let Some(window) = window.upgrade().filter(|_| session.vm().saves_open()) {
        // Accept presses the highlighted button: Sync now when it can run, otherwise Back.
        if nav == Nav::Accept {
            let sync = session.vm().view().sync_now;
            session.nav(if sync { Nav::Accept } else { Nav::Back });
            return;
        }
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
    if relay.flow_open() {
        // The save sync pages are on top of Settings: they take every key.
        relay.nav(nav);
        return;
    }
    let focus = window.get_settings_focus();
    if nav == Nav::Accept {
        match focus {
            SettingsControl::Ptbr => window.invoke_set_ptbr(!window.get_ptbr()),
            SettingsControl::Speedfix => window.invoke_set_speedfix(!window.get_speedfix()),
            SettingsControl::RelaySignIn => relay.press_sign_in(),
            SettingsControl::RelayOff => relay.press_turn_off(),
            SettingsControl::Back => {
                session.nav(Nav::Back);
            }
        }
    } else if nav == Nav::Back {
        session.nav(nav);
    } else {
        relay.cancel_confirm();
        window.set_settings_focus(move_settings_focus(focus, nav, relay.vm.rows()));
    }
}

fn save_fan_mod(window: &MainWindow, settings: &SettingsVm, control: SettingsControl, value: bool) {
    let key = match control {
        SettingsControl::Ptbr => "mods.ptbr",
        SettingsControl::Speedfix => "mods.speedfix",
        SettingsControl::RelaySignIn | SettingsControl::RelayOff | SettingsControl::Back => return,
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
    draw_page(&window, first.page());
}

fn draw_page(window: &MainWindow, page: PageView) {
    window.set_first_run_heading(page.heading.as_str().into());
    window.set_first_run_title(page.title.as_str().into());
    window.set_first_run_choice(
        page.options
            .get(page.focus)
            .map(String::as_str)
            .unwrap_or("")
            .into(),
    );
    window.set_first_run_position(if page.options.is_empty() {
        "".into()
    } else {
        format!("{} of {}", page.focus + 1, page.options.len()).into()
    });
    window.set_first_run_busy(page.busy);
    window.set_first_run_message(page.message.as_str().into());
    window.set_first_run_cancel_available(page.cancel_available);
    window.set_first_run_keypad(page.keypad);
    window.set_first_run_columns(page.columns as i32);
    window.set_first_run_editor(page.editor);
    window.set_first_run_focus_index(page.focus as i32);
    let choices: Vec<SharedString> = page.options.into_iter().map(Into::into).collect();
    window.set_first_run_choices(slint::ModelRc::new(slint::VecModel::from(choices)));
    window.set_first_run_progress(page.progress);
}

/// Show Settings' save sync rows, and its sign-in pages while they are open.
fn show_relay_settings(window: &MainWindow, relay: &RelaySettings) {
    let vm = &relay.vm;
    window.set_settings_relay_available(vm.available);
    window.set_settings_relay_summary(vm.summary().into());
    window.set_settings_relay_sign_in(vm.sign_in_label().into());
    window.set_settings_relay_has_off(vm.account.is_some());
    window.set_settings_relay_off(vm.turn_off_label().into());
    window.set_settings_relay_message(vm.message.as_str().into());
    // A row that went away (Turn off save sync) must not keep the focus.
    if vm.account.is_none() && window.get_settings_focus() == SettingsControl::RelayOff {
        window.set_settings_focus(SettingsControl::RelaySignIn);
    }
    let page = relay.page().filter(|_| !window.get_first_run_open());
    window.set_relay_flow_open(page.is_some());
    if let Some(page) = page {
        draw_page(window, page);
    }
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
    let relay = Rc::new(RefCell::new(RelaySettings::new(system_relay_setup())));
    window.on_settings_relay_activate({
        let relay = Rc::clone(&relay);
        let weak = window.as_weak();
        move |control| {
            let mut relay = relay.borrow_mut();
            match control {
                SettingsControl::RelaySignIn => relay.press_sign_in(),
                SettingsControl::RelayOff => relay.press_turn_off(),
                _ => {}
            }
            if let Some(window) = weak.upgrade() {
                window.set_settings_focus(control);
                show_relay_settings(&window, &relay);
            }
        }
    });
    let first = Rc::new(RefCell::new(FirstRun::new(vars.clone())));
    window.set_first_run_open(first.borrow().needed());
    render_first_run(&window.as_weak(), &mut first.borrow_mut());
    window.on_first_run_choose({
        let first = Rc::clone(&first);
        let relay = Rc::clone(&relay);
        let weak = window.as_weak();
        move |index| {
            if weak.upgrade().is_some_and(|w| !w.get_first_run_open()) {
                let mut relay = relay.borrow_mut();
                relay.click(index as usize);
                if let Some(window) = weak.upgrade() {
                    show_relay_settings(&window, &relay);
                }
                return;
            }
            let mut first = first.borrow_mut();
            first.click(index as usize);
            render_first_run(&weak, &mut first);
        }
    });
    window.on_first_run_nav({
        let first = Rc::clone(&first);
        let relay = Rc::clone(&relay);
        let weak = window.as_weak();
        move |index| {
            let nav = match index {
                0 => Nav::Up,
                1 => Nav::Down,
                2 => Nav::Accept,
                _ => Nav::Back,
            };
            if weak.upgrade().is_some_and(|w| !w.get_first_run_open()) {
                let mut relay = relay.borrow_mut();
                relay.nav(nav);
                if let Some(window) = weak.upgrade() {
                    show_relay_settings(&window, &relay);
                }
                return;
            }
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

    window.on_saves_sync_now({
        let weak = window.as_weak();
        let session = Rc::clone(&session);
        move || {
            let mut session = session.borrow_mut();
            session.sync_now();
            render(&weak, &session);
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
                .is_some_and(|window| window.get_first_run_open() || window.get_relay_flow_open())
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
        let relay = Rc::clone(&relay);
        let session = Rc::clone(&session);
        move |text: SharedString| {
            devices.borrow_mut().apply(DeviceEvent::Keyboard);
            if let Some(window) = weak.upgrade() {
                window.set_input_device(devices.borrow().current() as i32);
            }
            // A text field takes its keys first: letters and digits, Space, Backspace, Enter
            // and Esc, before the controller map sees them.
            let first_open = weak.upgrade().is_some_and(|w| w.get_first_run_open());
            {
                let mut first = first.borrow_mut();
                let mut relay = relay.borrow_mut();
                if route_text(&mut first, first_open, &mut relay, &text) {
                    if first_open {
                        render_first_run(&weak, &mut first);
                    } else if let Some(window) = weak.upgrade() {
                        show_relay_settings(&window, &relay);
                    }
                    return true;
                }
            }
            if first_open {
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
            let mut relay = relay.borrow_mut();
            navigate(&weak, &mut session, &mut relay, nav);
            if let Some(window) = weak.upgrade() {
                show_relay_settings(&window, &relay);
            }
            render(&weak, &session);
            true
        }
    });

    // Every frame: the gamepad's presses go in, and what the running job reported comes out.
    let mut gilrs = gilrs::Gilrs::new().ok();
    let mut stick = Stick::default();
    #[cfg(target_os = "android")]
    let mut android_hotplug = acnh_app::input::AndroidHotplug::default();
    let timer = Timer::default();
    let weak = window.as_weak();
    let mut was_first_open = window.get_first_run_open();
    let mut was_settings_open = false;
    let mut recheck = RecheckWhenClosed::default();
    let mut fleet_tick = 0_u32;
    timer.start(TimerMode::Repeated, Duration::from_millis(16), move || {
        #[cfg(target_os = "android")]
        android_hotplug.poll_if_due(
            std::time::Instant::now(),
            &mut devices.borrow_mut(),
            acnh_app::input::android::connected_devices,
        );
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
            session.refresh_after_first_run();
        }
        was_first_open = now_open;
        let mut relay = relay.borrow_mut();
        if !first_open {
            for nav in &navs {
                navigate(&weak, &mut session, &mut relay, *nav);
            }
        }
        let settings_open = session.vm().settings_open();
        if settings_open && !was_settings_open {
            relay.refresh();
        }
        let relay_changed = relay.pump();
        // Settings set save sync up or turned it off: Continue checks it again once Settings
        // has closed, so Back is never held up by the check.
        if recheck.update(settings_open, relay.take_changed()) {
            session.recheck_status();
        }
        if (settings_open != was_settings_open
            || relay_changed
            || (!navs.is_empty() && !first_open))
            && (settings_open || relay.flow_open() || was_settings_open)
        {
            if let Some(window) = weak.upgrade() {
                show_relay_settings(&window, &relay);
            }
        }
        was_settings_open = settings_open;
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

        let none = RelayRows::Hidden;
        assert_eq!(move_settings_focus(Ptbr, Nav::Down, none), Speedfix);
        assert_eq!(move_settings_focus(Speedfix, Nav::Down, none), Back);
        assert_eq!(move_settings_focus(Back, Nav::Down, none), Back);
        assert_eq!(move_settings_focus(Back, Nav::Up, none), Speedfix);
        assert_eq!(move_settings_focus(Speedfix, Nav::Up, none), Ptbr);
    }

    #[test]
    fn settings_focus_reaches_the_save_sync_rows() {
        use SettingsControl::{Back, RelayOff, RelaySignIn, Speedfix};

        let set_up = RelayRows::SignInAndOff;
        assert_eq!(
            move_settings_focus(Speedfix, Nav::Down, set_up),
            RelaySignIn
        );
        assert_eq!(
            move_settings_focus(RelaySignIn, Nav::Down, set_up),
            RelayOff
        );
        assert_eq!(move_settings_focus(RelayOff, Nav::Down, set_up), Back);
        assert_eq!(move_settings_focus(Back, Nav::Up, set_up), RelayOff);
        // Save sync off has no Turn off row.
        assert_eq!(
            move_settings_focus(RelaySignIn, Nav::Down, RelayRows::SignIn),
            Back
        );
    }
}
