//! The iPad-only half of `apple.rs` (issue #172): the Keychain credential store, the UIKit
//! alerts, the background task around the push, and the C exports the player calls.
//!
//! The alerts are `UIAlertController`s presented on the main thread from the game's thread,
//! which blocks until one is answered. The main thread keeps the run loop (the player's event
//! pump calls `SDL_PumpEvents`), so the alerts, the on-screen keyboard and touches work before
//! the game's own window exists. The alert gets a window of its own, hidden again once it is
//! answered. Not verified on a device: whether a controller can answer the alerts (docs/IOS.md).

use super::{
    Blocked, BlockedAnswer, Connector, HeldAnswer, Live, Player, Prompt, Roots, SignInAnswer,
    SignInForm,
};
use acnh_core::launch::Keep;
use acnh_core::savecard::SaveCard;
use block2::RcBlock;
use dispatch2::run_on_main;
use objc2::rc::Retained;
use objc2_foundation::{NSArray, NSString};
use objc2_ui_kit::{
    UIAlertAction, UIAlertActionStyle, UIAlertController, UIAlertControllerStyle, UIApplication,
    UIScreen, UITextAutocapitalizationType, UITextAutocorrectionType, UITextField,
    UITextInputTraits, UIViewController, UIWindow,
};
use save_relay::creds::{CredentialStore, Secret};
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};
use std::ffi::{c_char, CStr, CString};
use std::path::Path;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;

/// The Keychain service the app password is filed under: the app's own, in its own Keychain
/// access group. This is the only place the password is kept.
const SERVICE: &str = "io.github.acnhrecomp.save-relay";

/// `errSecItemNotFound`.
const NOT_FOUND: i32 = -25300;

/// save-relay's `CredentialStore` on the iOS Keychain (`security-framework`).
pub struct KeychainStore;

fn credentials(error: security_framework::base::Error) -> save_relay::Error {
    save_relay::Error::Credentials(format!("the Keychain refused: code {}", error.code()))
}

impl CredentialStore for KeychainStore {
    fn get(&self, account: &str) -> save_relay::Result<Option<Secret>> {
        match get_generic_password(SERVICE, account) {
            Ok(bytes) => Ok(Some(Secret::new(
                String::from_utf8_lossy(&bytes).into_owned(),
            ))),
            Err(error) if error.code() == NOT_FOUND => Ok(None),
            Err(error) => Err(credentials(error)),
        }
    }

    fn set(&self, account: &str, secret: &Secret) -> save_relay::Result<()> {
        set_generic_password(SERVICE, account, secret.expose().as_bytes()).map_err(credentials)
    }

    fn delete(&self, account: &str) -> save_relay::Result<()> {
        match delete_generic_password(SERVICE, account) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == NOT_FOUND => Ok(()),
            Err(error) => Err(credentials(error)),
        }
    }

    fn kind(&self) -> &'static str {
        "ios-keychain"
    }
}

// ---- the alerts ------------------------------------------------------------------------------

struct Field {
    placeholder: &'static str,
    text: String,
    secure: bool,
}

struct Alert {
    title: String,
    message: String,
    fields: Vec<Field>,
    /// Each button's title and whether it is the cancel style.
    buttons: Vec<(String, bool)>,
}

/// What the person tapped and what the text fields held, in order.
type Answer = (usize, Vec<String>);

/// Show `alert` and block until it is answered.
fn show(alert: Alert) -> Answer {
    let (tx, rx) = channel();
    run_on_main(move |_mtm| present(alert, tx));
    rx.recv().unwrap_or((0, Vec::new()))
}

// The player has no UIScene (ios/Info.plist.in has no scene manifest), so a window made from a
// frame is right here.
#[allow(deprecated)]
fn present(alert: Alert, tx: Sender<Answer>) {
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        return;
    };
    let window = UIWindow::initWithFrame(mtm.alloc(), UIScreen::mainScreen(mtm).bounds());
    let root = UIViewController::new(mtm);
    window.setRootViewController(Some(&root));
    window.makeKeyAndVisible();

    let controller = UIAlertController::alertControllerWithTitle_message_preferredStyle(
        Some(&NSString::from_str(&alert.title)),
        Some(&NSString::from_str(&alert.message)),
        UIAlertControllerStyle::Alert,
        mtm,
    );
    for field in &alert.fields {
        let (placeholder, text, secure) = (field.placeholder, field.text.clone(), field.secure);
        let configure = RcBlock::new(move |field: NonNull<UITextField>| {
            // SAFETY: UIKit passes a live text field, on the main thread.
            let field = unsafe { field.as_ref() };
            field.setPlaceholder(Some(&NSString::from_str(placeholder)));
            field.setText(Some(&NSString::from_str(&text)));
            field.setSecureTextEntry(secure);
            field.setAutocorrectionType(UITextAutocorrectionType::No);
            field.setAutocapitalizationType(UITextAutocapitalizationType::None);
        });
        controller.addTextFieldWithConfigurationHandler(Some(&configure));
    }
    for (index, (title, cancel)) in alert.buttons.iter().enumerate() {
        let (tx, window, alert_controller) = (tx.clone(), window.clone(), controller.clone());
        let tapped = RcBlock::new(move |_: NonNull<UIAlertAction>| {
            let fields: Retained<NSArray<UITextField>> =
                alert_controller.textFields().unwrap_or_else(NSArray::new);
            let texts = fields
                .iter()
                .map(|field| field.text().map(|t| t.to_string()).unwrap_or_default())
                .collect();
            window.setHidden(true);
            let _ = tx.send((index, texts));
        });
        let style = if *cancel {
            UIAlertActionStyle::Cancel
        } else {
            UIAlertActionStyle::Default
        };
        let action = UIAlertAction::actionWithTitle_style_handler(
            Some(&NSString::from_str(title)),
            style,
            Some(&tapped),
            mtm,
        );
        controller.addAction(&action);
    }
    root.presentViewController_animated_completion(&controller, false, None);
}

/// The alerts, as the `Prompt` of `apple.rs`.
pub struct UiPrompt;

impl Prompt for UiPrompt {
    fn sign_in(&self, form: &SignInForm) -> SignInAnswer {
        let mut message = String::from(
            "Sign in to your Nextcloud so your island follows you between devices. \
             Use an app password (Nextcloud: Settings, Security), not your own password.",
        );
        if let Some(why) = &form.message {
            message = format!("{why}\n\n{message}");
        }
        let mut buttons = vec![("Sign in".to_string(), false)];
        buttons.push(if form.allow_not_now {
            ("Not now".to_string(), true)
        } else {
            ("Cancel".to_string(), true)
        });
        let (tapped, texts) = show(Alert {
            title: "Save sync".into(),
            message,
            fields: vec![
                Field {
                    placeholder: "Server (https://...)",
                    text: form.server.clone(),
                    secure: false,
                },
                Field {
                    placeholder: "User",
                    text: form.user.clone(),
                    secure: false,
                },
                Field {
                    placeholder: "App password",
                    text: String::new(),
                    secure: true,
                },
            ],
            buttons,
        });
        match (tapped, texts.as_slice()) {
            (0, [server, user, password]) => SignInAnswer::Submit {
                server: server.trim().to_string(),
                user: user.trim().to_string(),
                password: password.clone(),
            },
            _ if form.allow_not_now => SignInAnswer::NotNow,
            _ => SignInAnswer::Cancel,
        }
    }

    fn conflict(&self, server: &SaveCard, local: &SaveCard) -> Keep {
        let (tapped, _) = show(Alert {
            title: "Two different islands".into(),
            message: format!(
                "This iPad: {}\nSave sync: {}\n\nThe one you do not keep stays in save sync's history.",
                super::describe_card(local),
                super::describe_card(server)
            ),
            fields: Vec::new(),
            buttons: vec![
                ("Keep this iPad's island".into(), false),
                ("Keep save sync's island".into(), false),
            ],
        });
        if tapped == 0 {
            Keep::Local
        } else {
            Keep::Server
        }
    }

    fn held(&self, device: &str, since: &str) -> HeldAnswer {
        let (tapped, _) = show(Alert {
            title: "Island open on another device".into(),
            message: format!(
                "{device} has had this island open since {since}. Playing anyway can make two different islands."
            ),
            fields: Vec::new(),
            buttons: vec![
                ("Check again".into(), false),
                ("Play anyway".into(), false),
            ],
        });
        if tapped == 1 {
            HeldAnswer::PlayAnyway
        } else {
            HeldAnswer::CheckAgain
        }
    }

    fn blocked(&self, why: &Blocked) -> BlockedAnswer {
        let (title, message, again) = match why {
            Blocked::Offline => (
                "Save sync could not be reached",
                "The game starts once the island has synced. Check the network, then Retry."
                    .to_string(),
                false,
            ),
            Blocked::Error(text) => (
                "Save sync stopped",
                format!("{text}\n\nThe game starts once the island has synced."),
                false,
            ),
            Blocked::SignInCancelled => (
                "Sign in to play",
                "Save sync is set up, so the game starts once the island has synced.".to_string(),
                true,
            ),
        };
        let mut buttons = vec![("Retry".to_string(), false)];
        if again {
            buttons.push(("Sign in again".to_string(), false));
        }
        let (tapped, _) = show(Alert {
            title: title.into(),
            message,
            fields: Vec::new(),
            buttons,
        });
        if again && tapped == 1 {
            BlockedAnswer::SignInAgain
        } else {
            BlockedAnswer::Retry
        }
    }
}

// ---- the background task ---------------------------------------------------------------------

/// Run `work` inside `beginBackgroundTaskWithExpirationHandler:`, so iOS lets a push finish
/// after the app has gone to the background.
fn in_background_task(work: impl FnOnce()) {
    let task = Arc::new(AtomicUsize::new(0));
    let handler_task = task.clone();
    run_on_main(|mtm| {
        let app = UIApplication::sharedApplication(mtm);
        let expired = RcBlock::new(move || {
            let id = handler_task.swap(0, Ordering::SeqCst);
            if id != 0 {
                run_on_main(|mtm| UIApplication::sharedApplication(mtm).endBackgroundTask(id as _));
            }
        });
        let id = app.beginBackgroundTaskWithExpirationHandler(Some(&expired));
        task.store(id, Ordering::SeqCst);
    });
    work();
    let id = task.swap(0, Ordering::SeqCst);
    if id != 0 {
        run_on_main(|mtm| UIApplication::sharedApplication(mtm).endBackgroundTask(id as _));
    }
}

// ---- the C exports ---------------------------------------------------------------------------

extern "C" {
    /// `runtime/src/platform_mac.cpp`: one line into the player's log (`nx::log`).
    fn nx_relay_log(line: *const c_char);
}

fn log(line: &str) {
    if let Ok(line) = CString::new(line.replace('\0', " ")) {
        // SAFETY: a NUL-terminated string that lives for the call.
        unsafe { nx_relay_log(line.as_ptr()) };
    }
}

fn data_root(raw: *const c_char) -> Option<&'static Path> {
    if raw.is_null() {
        return None;
    }
    // SAFETY: the player passes a NUL-terminated path that it keeps for the process's life.
    let text = unsafe { CStr::from_ptr(raw) }.to_str().ok()?;
    Some(Path::new(text))
}

fn with_player(raw: *const c_char, work: impl FnOnce(&Player<'_>) + std::panic::UnwindSafe) {
    let Some(data_root) = data_root(raw) else {
        log("relay: no data root");
        return;
    };
    let result = std::panic::catch_unwind(|| {
        let connector: &dyn Connector = &Live;
        let player = Player {
            roots: Roots::for_data_root(data_root),
            creds: &KeychainStore,
            connector,
            prompt: &UiPrompt,
            log: Box::new(log),
        };
        work(&player);
    });
    if result.is_err() {
        log("relay: stopped by a panic");
    }
}

/// Before `nx::boot()`, on the game's thread: returns when the game may boot (a pull has
/// succeeded, save sync is not set up, or Play anyway was chosen on a held island).
#[no_mangle]
pub extern "C" fn acnh_relay_before_game(data_root: *const c_char) {
    with_player(data_root, |player| player.before_game());
}

/// When the game quits and when the app goes to the background: pushes the island inside a
/// background task. Returns when the push has finished or been left waiting for the next start.
#[no_mangle]
pub extern "C" fn acnh_relay_after_game(data_root: *const c_char) {
    with_player(data_root, |player| {
        in_background_task(|| player.after_game())
    });
}
