use acnh_app::{
    input::poll_nav,
    vm::{AboutVm, ContinueVm},
    CreditEntry, FixEntry, MainWindow,
};
use slint::{ComponentHandle, Timer, TimerMode};
use std::{cell::RefCell, rc::Rc};

fn main() -> Result<(), slint::PlatformError> {
    let window = MainWindow::new()?;
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
    let view_model = Rc::new(RefCell::new(ContinueVm::default()));
    let window_weak = window.as_weak();

    let activate_window = window_weak.clone();
    let activate_view_model = Rc::clone(&view_model);
    window.on_activate(move || {
        if let Some(window) = activate_window.upgrade() {
            let mut view_model = activate_view_model.borrow_mut();
            view_model.activate();
            window.set_status(view_model.status().into());
        }
    });

    let mut gilrs = gilrs::Gilrs::new().ok();
    let timer = Timer::default();
    let timer_window = window_weak.clone();
    let timer_view_model = Rc::clone(&view_model);
    timer.start(
        TimerMode::Repeated,
        std::time::Duration::from_millis(16),
        move || {
            let nav = gilrs.as_mut().and_then(poll_nav);
            if let (Some(nav), Some(window)) = (nav, timer_window.upgrade()) {
                let mut view_model = timer_view_model.borrow_mut();
                if view_model.handle(nav) {
                    view_model.activate();
                }
                window.set_selected(view_model.selected() as i32);
                window.set_selection(view_model.selection().into());
                window.set_status(view_model.status().into());
            }
        },
    );

    window.run()
}
