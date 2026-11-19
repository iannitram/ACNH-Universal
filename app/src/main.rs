use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use acnh_app::input::{key_nav, poll_nav, Stick};
use acnh_app::play::{Player, RelayOff, Runner, Session};
use acnh_app::ui::{from_slint, to_slint};
use acnh_app::vm::{AboutVm, ContinuePageVm};
use acnh_app::MainWindow;
use acnh_app::{CreditEntry, FixEntry};
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
        window.set_view(to_slint(&session.vm().view()));
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let window = MainWindow::new()?;
    set_about(&window);
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
            session.nav(nav);
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
            session.nav(*nav);
        }
        if session.pump() || !navs.is_empty() {
            render(&weak, &session);
        }
    });

    window.run()?;
    Ok(())
}
