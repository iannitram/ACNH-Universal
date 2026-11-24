//! The iPad-only half of `relay.rs` (issue #172): the Keychain credential store, the UIKit
//! alerts, the background task around the push, and the C exports the player calls.
//!
//! The alerts are `UIAlertController`s presented on the main thread from the game's thread,
//! which blocks until one is answered. The main thread keeps the run loop (the player's event
//! pump calls `SDL_PumpEvents`), so the alerts, the on-screen keyboard and touches work before
//! the game's own window exists. The alert is presented in a window of its own on the app's
//! foreground `UIWindowScene` (SDL3's UIKit driver runs scenes), or on the key window's view
//! controller when there is no scene, and everything is released once it is answered. Not
//! verified on a device: whether a controller can answer the alerts (docs/IOS.md).

use super::{
    Blocked, BlockedAnswer, Connector, HeldAnswer, Live, Player, Prompt, Roots, Settings,
    SignInAnswer, SignInForm,
};
use acnh_core::launch::Keep;
use acnh_core::savecard::SaveCard;
use block2::RcBlock;
use dispatch2::run_on_main;
use objc2::rc::Retained;
use objc2_foundation::{NSArray, NSString};
use objc2_ui_kit::{
    UIAlertAction, UIAlertActionStyle, UIAlertController, UIAlertControllerStyle, UIApplication,
    UISceneActivationState, UIScreen, UITextAutocapitalizationType, UITextAutocorrectionType,
    UITextField, UITextInputTraits, UIViewController, UIWindow, UIWindowScene,
};
use save_relay::creds::{CredentialStore, Secret};
use security_framework::access_control::{ProtectionMode, SecAccessControl};
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password_options,
};
use security_framework::passwords_options::PasswordOptions;
use std::cell::RefCell;
use std::ffi::{c_char, CStr, CString};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
        // Readable after the first unlock since boot, so a push from the background with the iPad
        // locked can still read the app password.
        let mut options = PasswordOptions::new_generic_password(SERVICE, account);
        options.set_access_control(
            SecAccessControl::create_with_protection(
                Some(ProtectionMode::AccessibleAfterFirstUnlock),
                0,
            )
            .map_err(credentials)?,
        );
        set_generic_password_options(secret.expose().as_bytes(), options).map_err(credentials)
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

/// Show `alert` and block until it is answered. With nowhere to present yet (no scene, no
/// window) it tries again until there is: the game does not boot without an answer.
fn show(alert: Alert) -> Answer {
    let (tx, rx) = channel();
    let alert = Arc::new(alert);
    loop {
        let (alert, tx) = (alert.clone(), tx.clone());
        if run_on_main(move |_mtm| present(&alert, tx)) {
            return rx.recv().unwrap_or((0, Vec::new()));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Where the alert goes: the root view controller to present on, and the window made for it.
fn host(
    mtm: objc2::MainThreadMarker,
) -> Option<(Retained<UIViewController>, Option<Retained<UIWindow>>)> {
    let app = UIApplication::sharedApplication(mtm);
    // The app's foreground-active window scene (any window scene when none is active).
    let mut scene: Option<Retained<UIWindowScene>> = None;
    for candidate in app.connectedScenes().to_vec() {
        if let Ok(window_scene) = candidate.downcast::<UIWindowScene>() {
            let active = window_scene.activationState() == UISceneActivationState::ForegroundActive;
            if active || scene.is_none() {
                scene = Some(window_scene);
            }
            if active {
                break;
            }
        }
    }
    if let Some(scene) = scene {
        let window = UIWindow::initWithWindowScene(mtm.alloc(), &scene);
        let root = UIViewController::new(mtm);
        window.setRootViewController(Some(&root));
        window.makeKeyAndVisible();
        return Some((root, Some(window)));
    }
    // No scene: SDL's key window, on top of whatever it already presents.
    #[allow(deprecated)]
    let key = app
        .windows()
        .to_vec()
        .into_iter()
        .find(|window| window.isKeyWindow());
    if let Some(mut top) = key.and_then(|window| window.rootViewController()) {
        while let Some(next) = top.presentedViewController() {
            top = next;
        }
        return Some((top, None));
    }
    // A scene-less app with no window yet: a window from the screen's frame.
    #[allow(deprecated)]
    let window = UIWindow::initWithFrame(mtm.alloc(), UIScreen::mainScreen(mtm).bounds());
    let root = UIViewController::new(mtm);
    window.setRootViewController(Some(&root));
    window.makeKeyAndVisible();
    Some((root, Some(window)))
}

/// What the answering button must let go of: the window made for the alert, and the alert.
type Held = Option<(Option<Retained<UIWindow>>, Retained<UIAlertController>)>;

/// Present `alert` on the main thread; the answer goes to `tx`. False: nowhere to present.
fn present(alert: &Alert, tx: Sender<Answer>) -> bool {
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        return false;
    };
    let Some((root, window)) = host(mtm) else {
        return false;
    };
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
    // The controller owns its actions, whose blocks would own the controller and the window: a
    // cycle. The blocks hold only this slot, and the first tap empties it.
    let held: Rc<RefCell<Held>> = Rc::new(RefCell::new(Some((window, controller.clone()))));
    for (index, (title, cancel)) in alert.buttons.iter().enumerate() {
        let (tx, held) = (tx.clone(), held.clone());
        let tapped = RcBlock::new(move |_: NonNull<UIAlertAction>| {
            let Some((window, controller)) = held.borrow_mut().take() else {
                return;
            };
            let fields: Retained<NSArray<UITextField>> =
                controller.textFields().unwrap_or_else(NSArray::new);
            let texts = fields
                .iter()
                .map(|field| field.text().map(|t| t.to_string()).unwrap_or_default())
                .collect();
            if let Some(window) = window {
                window.setHidden(true);
            }
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
    true
}

/// The alerts, as the `Prompt` of `relay.rs`.
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

/// A `beginBackgroundTaskWithExpirationHandler:` task, ended exactly once: by whoever swaps its
/// identifier out first (the push when it is done, or the expiration handler).
#[derive(Clone)]
struct BackgroundTask(Arc<AtomicUsize>);

impl BackgroundTask {
    fn begin() -> BackgroundTask {
        let task = BackgroundTask(Arc::new(AtomicUsize::new(0)));
        let for_handler = task.clone();
        let for_store = task.clone();
        run_on_main(move |mtm| {
            let expired = RcBlock::new(move || for_handler.end());
            let id = UIApplication::sharedApplication(mtm)
                .beginBackgroundTaskWithExpirationHandler(Some(&expired));
            for_store.0.store(id, Ordering::SeqCst);
        });
        task
    }

    fn end(&self) {
        let id = self.0.swap(0, Ordering::SeqCst);
        if id != 0 {
            run_on_main(move |mtm| UIApplication::sharedApplication(mtm).endBackgroundTask(id));
        }
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

extern "C" {
    /// `runtime/src/platform_mac.cpp`: `relay.ask` is `0` in the player's settings.
    fn nx_relay_ask_off() -> bool;
    /// `runtime/src/platform_mac.cpp`: `config_set("relay.ask", "0")` and `config_save()`.
    fn nx_relay_remember_not_now() -> bool;
}

/// `relay.ask` through the player's settings: the C++ side is the only writer of `config.ini`.
struct PlayerSettings;

impl Settings for PlayerSettings {
    fn ask_off(&self) -> bool {
        // SAFETY: plain calls into the player, no pointers.
        unsafe { nx_relay_ask_off() }
    }
    fn remember_not_now(&self) -> bool {
        // SAFETY: as above.
        unsafe { nx_relay_remember_not_now() }
    }
}

/// True once `before_game` has returned: the push waits for it.
static BOOTED: AtomicBool = AtomicBool::new(false);

/// True when that start pulled (sync on): a game started with sync off never pushes.
static PULLED: AtomicBool = AtomicBool::new(false);

fn data_root(raw: *const c_char) -> Option<PathBuf> {
    if raw.is_null() {
        return None;
    }
    // SAFETY: the player passes a NUL-terminated path.
    let text = unsafe { CStr::from_ptr(raw) }.to_str().ok()?;
    Some(PathBuf::from(text))
}

fn with_player(data_root: &Path, work: impl FnOnce(&Player<'_>) + std::panic::UnwindSafe) {
    let result = std::panic::catch_unwind(|| {
        let connector: &dyn Connector = &Live;
        let player = Player {
            roots: Roots::for_data_root(data_root),
            creds: &KeychainStore,
            connector,
            prompt: &UiPrompt,
            booted: &BOOTED,
            pulled: &PULLED,
            settings: &PlayerSettings,
            log: Box::new(log),
        };
        work(&player);
    });
    if result.is_err() {
        log("relay: stopped by a panic");
    }
}

/// Before `nx::boot()`, on the game's thread: returns when the game may boot (a pull has
/// succeeded, save sync is not set up, or Play anyway was chosen on a held island). A panic
/// inside is shown with Retry; it never boots the game unsynced.
#[no_mangle]
pub extern "C" fn acnh_relay_before_game(data_root: *const c_char) {
    match self::data_root(data_root) {
        Some(root) => with_player(&root, |player| player.before_game()),
        None => {
            // No data root, so no saves to protect: nothing to sync.
            log("relay: no data root");
            BOOTED.store(true, Ordering::SeqCst);
        }
    }
}

/// When the game quits: pushes the island, inside a background task, and returns when the push
/// has finished or been left waiting for the next start. Does nothing before the game has booted.
#[no_mangle]
pub extern "C" fn acnh_relay_after_game(data_root: *const c_char) {
    let Some(root) = self::data_root(data_root) else {
        return;
    };
    let task = BackgroundTask::begin();
    with_player(&root, |player| player.after_game());
    task.end();
}

/// When the app has gone to the background (`SDL_EVENT_DID_ENTER_BACKGROUND`, from SDL's event
/// watch on the main thread): begins a background task here, pushes on a worker thread and
/// returns at once, so the main thread keeps its run loop. The worker, or the expiration
/// handler, ends the task. The game goes on, so it is a checkpoint (`Player::checkpoint`): the
/// iPad stays marked as the one playing. Does nothing before the game has booted.
#[no_mangle]
pub extern "C" fn acnh_relay_background(data_root: *const c_char) {
    let Some(root) = self::data_root(data_root) else {
        return;
    };
    if !BOOTED.load(Ordering::SeqCst) {
        log("relay: no push yet: the game has not booted");
        return;
    }
    let task = BackgroundTask::begin();
    let worker = task.clone();
    let spawned = std::thread::Builder::new()
        .name("relay push".into())
        .spawn(move || {
            with_player(&root, |player| player.checkpoint());
            worker.end();
        });
    if spawned.is_err() {
        task.end();
    }
}
