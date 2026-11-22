use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use acnh_app::input::{key_nav, poll_nav, Stick};
use acnh_app::play::{Player, RelayOff, Runner, Session};
use acnh_app::ui::{from_slint, to_slint};
use acnh_app::vm::settings::SettingsVm;
use acnh_app::vm::{AboutVm, ContinuePageVm, Nav};
use acnh_app::MainWindow;
use acnh_app::{CreditEntry, FixEntry, SettingsControl};
use acnh_core::platform::{AppDirs, Os, Vars};
use acnh_core::{assemble, backup, savecard, saves};
use slint::{ComponentHandle, SharedString, Timer, TimerMode, Weak};

/// The picture on the island card.
const PICTURE: &str = "assets/acnhrecomp.png";

/// The Continue page for this device: its island, its player and its folders.
fn session() -> acnh_core::Result<Session> {
    let os = Os::current();
    let vars = Vars::from_lookup(os, |name| std::env::var(name).ok())?;
    let dirs = AppDirs::new(&vars);
    let root = dirs.data_root();
    let save_dir = saves::save_dir(&root);
    let picture = PathBuf::from(PICTURE);

    // `this_device` and `set_saved_by` take the fleet's records once the app keeps them
    // (the Fleet page); with neither, no drift is reported.
    let mut vm = ContinuePageVm::new(savecard::read_card(&save_dir, &picture).ok(), None);
    vm.home = vars.home.clone();

    // On the Mac the player is beside this executable in the one contained app.
    let launcher = std::env::current_exe().unwrap_or_default();
    let program = assemble::player(os, &launcher, &dirs.install());
    let backups = saves::backups_dir(&root);
    let runner = Runner {
        // No relay is set up from the app yet (Settings): the island here is played, and
        // nothing is pulled or pushed.
        relay: Arc::new(RelayOff),
        game: Arc::new(Player { program }),
        save_dir,
        env: vec![("NX_DATA_ROOT".to_string(), root.display().to_string())],
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
        window.set_view(to_slint(&session.vm().view()));
    }
}

fn move_settings_focus(focus: SettingsControl, nav: Nav) -> SettingsControl {
    use SettingsControl::{Back, Ptbr, Speedfix};

    match (focus, nav) {
        (Ptbr, Nav::Down | Nav::Right) | (Back, Nav::Up | Nav::Left) => Speedfix,
        (Speedfix, Nav::Down | Nav::Right) => Back,
        (Speedfix, Nav::Up | Nav::Left) => Ptbr,
        _ => focus,
    }
}

fn navigate(window: &Weak<MainWindow>, session: &mut Session, nav: Nav) {
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let window = MainWindow::new()?;
    set_about(&window);
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
    let session = Rc::new(RefCell::new(session()?));
    render(&window.as_weak(), &session.borrow());

    window.on_clicked({
        let weak = window.as_weak();
        let session = Rc::clone(&session);
        move |target| {
            let mut session = session.borrow_mut();
            session.click(from_slint(target));
            render(&weak, &session);
        }
    });

    window.on_key({
        let weak = window.as_weak();
        let session = Rc::clone(&session);
        move |text: SharedString| {
            let Some(nav) = key_nav(&text) else {
                return false;
            };
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
    timer.start(TimerMode::Repeated, Duration::from_millis(16), move || {
        let navs = match gilrs.as_mut() {
            Some(gilrs) => poll_nav(gilrs, &mut stick),
            None => Vec::new(),
        };
        let mut session = session.borrow_mut();
        for nav in &navs {
            navigate(&weak, &mut session, *nav);
        }
        if session.pump() || !navs.is_empty() {
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
    fn settings_focus_reaches_both_switches_and_back() {
        use SettingsControl::{Back, Ptbr, Speedfix};

        assert_eq!(move_settings_focus(Ptbr, Nav::Down), Speedfix);
        assert_eq!(move_settings_focus(Speedfix, Nav::Down), Back);
        assert_eq!(move_settings_focus(Back, Nav::Down), Back);
        assert_eq!(move_settings_focus(Back, Nav::Up), Speedfix);
        assert_eq!(move_settings_focus(Speedfix, Nav::Up), Ptbr);
    }
}
