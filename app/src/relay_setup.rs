//! Setting up save sync from the launcher: the seam the pages work through (`RelaySetup`), the
//! runner that does the work off the window thread, and this machine's implementation over
//! `acnh_core::relay` (`setup_nextcloud`, `start_sign_in`, `finish_sign_in`, `forget`).
//!
//! Nothing here keeps or prints the app password: it goes from the page's request into
//! `setup_nextcloud` and from there into the system credential store only.

use crate::vm::relay_setup::{Account, Event, Outcome, Prompt, RelaySetupVm, Request, Service};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;

/// What the pages need from save-relay. Errors are the plain sentence the page shows.
pub trait RelaySetup: Send + Sync {
    /// Whether this build can set save sync up at all (Android and the iPad do it elsewhere).
    fn available(&self) -> bool;
    /// The account save sync is set up with, if any. Reads the config; never the network.
    fn account(&self) -> Option<Account>;
    fn setup_nextcloud(
        &self,
        server: &str,
        user: &str,
        app_password: &str,
    ) -> Result<Account, String>;
    fn start_sign_in(&self, service: Service) -> Result<Prompt, String>;
    /// Wait for the sign-in that `start_sign_in` began. `answer` is the code Dropbox shows in
    /// Game Mode, else empty. `cancelled` turns true when the person backed out.
    fn finish_sign_in(
        &self,
        service: Service,
        answer: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Account, String>;
    /// "Turn off save sync": remove save-relay's config and every secret it stored.
    fn forget(&self) -> Result<(), String>;
}

/// A physical keyboard key in a text field (docs/design/continue-screen.md, "Controller map"):
/// Space enters a space and Backspace deletes, Enter submits and Esc goes back. Returns whether
/// the key was a text key; false leaves it to the controller map. Only while the keyboard is
/// open.
pub fn text_key(vm: &mut RelaySetupVm, text: &str) -> bool {
    use slint::platform::Key;
    if !vm.keyboard_open() {
        return false;
    }
    let mut chars = text.chars();
    let (Some(key), None) = (chars.next(), chars.next()) else {
        return false;
    };
    if key == char::from(Key::Backspace) {
        vm.backspace();
    } else if key == char::from(Key::Return) {
        vm.submit();
    } else if key == char::from(Key::Escape) {
        vm.leave_editor();
    } else if !key.is_control() && !('\u{f700}'..='\u{f8ff}').contains(&key) {
        vm.type_text(key);
    } else {
        return false;
    }
    true
}

/// A build without save sync setup (Android and iOS): the relay step is skipped and the
/// Settings row is hidden.
pub struct Unavailable;

impl RelaySetup for Unavailable {
    fn available(&self) -> bool {
        false
    }
    fn account(&self) -> Option<Account> {
        None
    }
    fn setup_nextcloud(&self, _: &str, _: &str, _: &str) -> Result<Account, String> {
        Err("Save sync is not available here.".into())
    }
    fn start_sign_in(&self, _: Service) -> Result<Prompt, String> {
        Err("Save sync is not available here.".into())
    }
    fn finish_sign_in(&self, _: Service, _: &str, _: &dyn Fn() -> bool) -> Result<Account, String> {
        Err("Save sync is not available here.".into())
    }
    fn forget(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Runs a page's requests on threads of their own and hands the answers back to it. One job at
/// a time: a newer request, or Cancel, makes the older one's answer unwelcome.
pub struct RelayRunner {
    setup: Arc<dyn RelaySetup>,
    sender: Sender<(u64, Event)>,
    receiver: Receiver<(u64, Event)>,
    generation: u64,
    cancel: Arc<AtomicBool>,
}

impl RelayRunner {
    pub fn new(setup: Arc<dyn RelaySetup>) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            setup,
            sender,
            receiver,
            generation: 0,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn setup(&self) -> &Arc<dyn RelaySetup> {
        &self.setup
    }

    /// Start whatever the page asked for.
    pub fn dispatch(&mut self, vm: &mut RelaySetupVm) {
        while let Some(request) = vm.take_request() {
            self.run(request);
        }
    }

    fn run(&mut self, request: Request) {
        // A newer job, or Cancel, retires the one before it.
        self.generation += 1;
        self.cancel.store(true, Ordering::SeqCst);
        if request == Request::Cancel {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Arc::clone(&cancel);
        let generation = self.generation;
        let setup = Arc::clone(&self.setup);
        let sender = self.sender.clone();
        let had_account = setup.account().is_some();
        thread::spawn(move || {
            let event = match request {
                Request::Nextcloud {
                    server,
                    user,
                    password,
                } => Event::Connected(setup.setup_nextcloud(&server, &user, &password)),
                Request::StartSignIn(service) => Event::SignInStarted(setup.start_sign_in(service)),
                Request::FinishSignIn(service, answer) => {
                    Event::SignedIn(
                        setup.finish_sign_in(service, &answer, &|| cancel.load(Ordering::SeqCst)),
                    )
                }
                Request::Cancel => return,
            };
            let set_up = matches!(&event, Event::Connected(Ok(_)) | Event::SignedIn(Ok(_)));
            if set_up && cancel.load(Ordering::SeqCst) && !had_account {
                // Backed out, but the work finished anyway: leave save sync off, as the
                // person chose.
                let _ = setup.forget();
                return;
            }
            let _ = sender.send((generation, event));
        });
    }

    /// Take in what finished. `None` when nothing came in, else how the page was left
    /// (`Open` when it goes on).
    pub fn pump(&mut self, vm: &mut RelaySetupVm) -> Option<Outcome> {
        let mut outcome = None;
        let events: Vec<_> = self.receiver.try_iter().collect();
        for (generation, event) in events {
            if generation != self.generation {
                continue;
            }
            let result = vm.event(event);
            if outcome.is_none() || result != Outcome::Open {
                outcome = Some(result);
            }
        }
        self.dispatch(vm);
        outcome
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub use system::{plain_error, SystemSetup};

#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod system {
    use super::RelaySetup;
    use crate::vm::relay_setup::{Account, Prompt, Service};
    use save_relay::creds::{CredentialStore, KeyringStore};
    use save_relay::http::{Transport, UreqTransport};
    use save_relay::local::LocalStore;
    use save_relay::platform::{AppDirs, PathVars};
    use save_relay::service::Service as Relay;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    /// A credential store and transport for one step; save-relay's are not `Send`, so each
    /// step opens its own.
    type Open = dyn Fn() -> (Box<dyn CredentialStore>, Arc<dyn Transport>) + Send + Sync;

    /// How long the browser has to come back.
    const BROWSER_WAIT: Duration = Duration::from_secs(300);

    /// This machine's save sync setup: save-relay's own folders, credential store and network.
    pub struct SystemSetup {
        dirs: AppDirs,
        open: Arc<Open>,
        game_mode: Arc<dyn Fn() -> bool + Send + Sync>,
        open_browser: Arc<dyn Fn(&str) -> bool + Send + Sync>,
        device: String,
        listener: Mutex<Option<TcpListener>>,
    }

    impl SystemSetup {
        /// The real machine.
        pub fn system() -> Option<Self> {
            let vars = PathVars::current().ok()?;
            Some(Self::with(
                AppDirs::from_vars(&vars),
                Arc::new(|| {
                    (
                        Box::new(KeyringStore::new()) as Box<dyn CredentialStore>,
                        Arc::new(UreqTransport::new()) as Arc<dyn Transport>,
                    )
                }),
                Arc::new(|| save_relay::oauth::in_game_mode(&|name| std::env::var(name).ok())),
                Arc::new(|url| save_relay::oauth::open_browser(url).is_ok()),
                machine_name(),
            ))
        }

        /// Folders, stores and the browser given: what tests and `system` build from.
        pub fn with(
            dirs: AppDirs,
            open: Arc<Open>,
            game_mode: Arc<dyn Fn() -> bool + Send + Sync>,
            open_browser: Arc<dyn Fn(&str) -> bool + Send + Sync>,
            device: String,
        ) -> Self {
            Self {
                dirs,
                open,
                game_mode,
                open_browser,
                device,
                listener: Mutex::new(None),
            }
        }

        fn account_of(&self) -> Option<Account> {
            let config = LocalStore::new(self.dirs.clone()).load_config().ok()??;
            let service = Relay::from_server(&config.server).ok()?;
            Some(match service {
                Relay::Nextcloud => Account {
                    service: Service::Nextcloud,
                    name: config.user,
                    address: Some(config.server),
                },
                Relay::Gdrive => Account {
                    service: Service::GoogleDrive,
                    name: config.user,
                    address: None,
                },
                Relay::Dropbox => Account {
                    service: Service::Dropbox,
                    name: config.user,
                    address: None,
                },
            })
        }
    }

    fn relay_service(service: Service) -> Relay {
        match service {
            Service::Nextcloud => Relay::Nextcloud,
            Service::GoogleDrive => Relay::Gdrive,
            Service::Dropbox => Relay::Dropbox,
        }
    }

    /// The machine's own name, for save-relay's history entries.
    fn machine_name() -> String {
        ["HOSTNAME", "COMPUTERNAME"]
            .iter()
            .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
            .or_else(|| {
                std::process::Command::new("hostname")
                    .output()
                    .ok()
                    .and_then(|out| String::from_utf8(out.stdout).ok())
                    .map(|name| name.trim().to_owned())
                    .filter(|name| !name.is_empty())
            })
            .unwrap_or_else(|| "device".into())
    }

    /// A sentence for the page from a save-relay failure: no addresses of ours, no secrets.
    pub fn plain_error(error: &acnh_core::Error) -> String {
        let text = error.to_string();
        if text.contains("refused the user name or app password") {
            "The server did not accept that user name and app password.".into()
        } else if text.contains("unreachable") {
            "Could not reach the server. Check the address and your connection.".into()
        } else if text.contains("credential store") {
            "The system password store could not be used.".into()
        } else if let Some((_, rest)) = text.split_once("sign-in failed: ") {
            format!("The sign-in did not finish: {rest}")
        } else {
            "Save sync could not be set up.".into()
        }
    }

    const PAGE: &str = "<!doctype html><title>Signed in</title><p>You are signed in. \
You can close this tab and go back to ACNH Universal.</p>";

    impl RelaySetup for SystemSetup {
        fn available(&self) -> bool {
            true
        }

        fn account(&self) -> Option<Account> {
            self.account_of()
        }

        fn setup_nextcloud(
            &self,
            server: &str,
            user: &str,
            app_password: &str,
        ) -> Result<Account, String> {
            let (creds, transport) = (self.open)();
            acnh_core::relay::setup_nextcloud(
                server,
                user,
                app_password,
                &self.device,
                &self.dirs,
                &*creds,
                transport,
            )
            .map_err(|error| plain_error(&error))?;
            self.account_of()
                .ok_or_else(|| "Save sync could not be set up.".into())
        }

        fn start_sign_in(&self, service: Service) -> Result<Prompt, String> {
            let (creds, transport) = (self.open)();
            let game_mode = (self.game_mode)();
            // The browser comes back to this app on a loopback address: listen before opening.
            let listener = if game_mode {
                None
            } else {
                Some(TcpListener::bind("127.0.0.1:53710").map_err(|_| {
                    "Another program is using the sign-in port. Close it and try again.".to_owned()
                })?)
            };
            let prompt = acnh_core::relay::start_sign_in(
                relay_service(service),
                &self.dirs,
                &*creds,
                transport,
            )
            .map_err(|error| plain_error(&error))?;
            let browser = !game_mode && (self.open_browser)(&prompt.url);
            *self
                .listener
                .lock()
                .map_err(|_| "Save sync sign-in is unavailable.".to_owned())? = listener;
            Ok(Prompt {
                url: prompt.url,
                code: prompt.code,
                needs_code: game_mode && service == Service::Dropbox,
                browser,
            })
        }

        fn finish_sign_in(
            &self,
            service: Service,
            answer: &str,
            cancelled: &dyn Fn() -> bool,
        ) -> Result<Account, String> {
            let (creds, transport) = (self.open)();
            let listener = self
                .listener
                .lock()
                .map_err(|_| "Save sync sign-in is unavailable.".to_owned())?
                .take();
            let answer = match listener {
                Some(listener) => wait_for_redirect(&listener, cancelled)?,
                None => answer.to_owned(),
            };
            if cancelled() {
                return Err("The sign-in was cancelled.".into());
            }
            acnh_core::relay::finish_sign_in(
                relay_service(service),
                &self.dirs,
                &*creds,
                transport,
                &answer,
            )
            .map_err(|error| plain_error(&error))?;
            self.account_of()
                .ok_or_else(|| "Save sync could not be set up.".into())
        }

        fn forget(&self) -> Result<(), String> {
            let (creds, _) = (self.open)();
            acnh_core::relay::forget(&self.dirs, &*creds).map_err(|error| plain_error(&error))
        }
    }

    /// Wait for the browser's redirect and return its full address.
    fn wait_for_redirect(
        listener: &TcpListener,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<String, String> {
        listener
            .set_nonblocking(true)
            .map_err(|_| "The sign-in could not wait for the browser.".to_owned())?;
        let started = Instant::now();
        loop {
            if cancelled() {
                return Err("The sign-in was cancelled.".into());
            }
            if started.elapsed() > BROWSER_WAIT {
                return Err("The sign-in took too long. Try again.".into());
            }
            let mut stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(_) => {
                    thread::sleep(Duration::from_millis(200));
                    continue;
                }
            };
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut buffer = [0_u8; 4096];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]);
            let target = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("");
            let answered = target.contains("code=") || target.contains("error=");
            let (status, body) = if answered {
                ("200 OK", PAGE)
            } else {
                ("404 Not Found", "")
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            if answered {
                return Ok(format!("http://127.0.0.1:53710{target}"));
            }
        }
    }
}

/// The save sync rows of Settings with the work they start: Sign in again opens the setup
/// pages, Turn off save sync forgets the account.
pub struct RelaySettings {
    pub vm: crate::vm::settings::SettingsRelay,
    runner: RelayRunner,
}

impl RelaySettings {
    pub fn new(setup: Arc<dyn RelaySetup>) -> Self {
        let vm = crate::vm::settings::SettingsRelay::new(setup.available(), setup.account());
        Self {
            vm,
            runner: RelayRunner::new(setup),
        }
    }

    /// Read the account again (a setup outside the app is seen on the next visit).
    pub fn refresh(&mut self) {
        if self.vm.flow.is_none() {
            self.vm.account = self.runner.setup().account();
        }
        self.vm.confirming_off = false;
    }

    pub fn flow_open(&self) -> bool {
        self.vm.flow.is_some()
    }

    pub fn press_sign_in(&mut self) {
        self.vm.open_sign_in();
    }

    /// The first press asks, the second turns save sync off: `relay::forget`, after which this
    /// device plays without syncing.
    pub fn press_turn_off(&mut self) {
        if self.vm.account.is_none() {
            return;
        }
        if !self.vm.confirming_off {
            self.vm.confirming_off = true;
            return;
        }
        self.vm.confirming_off = false;
        match self.runner.setup().forget() {
            Ok(()) => {
                self.vm.account = None;
                self.vm.message = "Save sync is off. This device plays without syncing.".into();
            }
            Err(reason) => self.vm.message = reason,
        }
    }

    pub fn cancel_confirm(&mut self) {
        self.vm.confirming_off = false;
    }

    fn step(&mut self, act: impl FnOnce(&mut RelaySetupVm) -> Outcome) {
        let Some(flow) = self.vm.flow.as_mut() else {
            return;
        };
        let outcome = act(flow);
        self.runner.dispatch(flow);
        self.vm.flow_outcome(outcome);
    }

    pub fn nav(&mut self, nav: crate::vm::Nav) {
        self.step(|flow| flow.handle(nav));
    }

    pub fn click(&mut self, index: usize) {
        self.step(|flow| flow.activate(index));
    }

    pub fn text_key(&mut self, text: &str) -> bool {
        let Some(flow) = self.vm.flow.as_mut() else {
            return false;
        };
        let handled = text_key(flow, text);
        self.runner.dispatch(flow);
        handled
    }

    /// Take in what finished; true when the page changed.
    pub fn pump(&mut self) -> bool {
        let Some(flow) = self.vm.flow.as_mut() else {
            return false;
        };
        let Some(outcome) = self.runner.pump(flow) else {
            return false;
        };
        self.vm.flow_outcome(outcome);
        true
    }
}

/// A scripted save sync setup for tests: no network, no credential store. It accepts one app
/// password, hands out one sign-in prompt, and waits in `finish_sign_in` until `release`.
pub mod fake {
    use super::RelaySetup;
    use crate::vm::relay_setup::{Account, Prompt, Service};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Default)]
    struct State {
        account: Option<Account>,
        calls: Vec<String>,
        saw_cancel: bool,
    }

    pub struct FakeSetup {
        accepts: String,
        prompt: Prompt,
        state: Mutex<State>,
        released: AtomicBool,
    }

    impl FakeSetup {
        /// A fake whose Nextcloud takes `accepts` as the app password and whose sign-ins show
        /// `prompt`.
        pub fn new(accepts: &str, prompt: Prompt) -> Arc<FakeSetup> {
            Arc::new(FakeSetup {
                accepts: accepts.to_owned(),
                prompt,
                state: Mutex::new(State::default()),
                released: AtomicBool::new(false),
            })
        }

        /// The person finished the sign-in on the phone.
        pub fn release(&self) {
            self.released.store(true, Ordering::SeqCst);
        }

        /// Every call it took, with the secrets left out.
        pub fn calls(&self) -> Vec<String> {
            self.state.lock().unwrap().calls.clone()
        }

        /// A waiting sign-in noticed that the person backed out.
        pub fn saw_cancel(&self) -> bool {
            self.state.lock().unwrap().saw_cancel
        }

        pub fn set_account(&self, account: Option<Account>) {
            self.state.lock().unwrap().account = account;
        }

        fn note(&self, call: String) {
            self.state.lock().unwrap().calls.push(call);
        }
    }

    impl RelaySetup for FakeSetup {
        fn available(&self) -> bool {
            true
        }

        fn account(&self) -> Option<Account> {
            self.state.lock().unwrap().account.clone()
        }

        fn setup_nextcloud(
            &self,
            server: &str,
            user: &str,
            app_password: &str,
        ) -> Result<Account, String> {
            self.note(format!("nextcloud {server} {user}"));
            if app_password != self.accepts {
                return Err("The server did not accept that user name and app password.".into());
            }
            let account = Account {
                service: Service::Nextcloud,
                name: user.to_owned(),
                address: Some(server.to_owned()),
            };
            self.set_account(Some(account.clone()));
            Ok(account)
        }

        fn start_sign_in(&self, service: Service) -> Result<Prompt, String> {
            self.note(format!("start {}", service.label()));
            Ok(self.prompt.clone())
        }

        fn finish_sign_in(
            &self,
            service: Service,
            answer: &str,
            cancelled: &dyn Fn() -> bool,
        ) -> Result<Account, String> {
            self.note(format!(
                "finish {} typed={}",
                service.label(),
                !answer.is_empty()
            ));
            loop {
                if cancelled() {
                    self.state.lock().unwrap().saw_cancel = true;
                    return Err("The sign-in was cancelled.".into());
                }
                if self.released.load(Ordering::SeqCst) {
                    let account = Account {
                        service,
                        name: "name@example.test".into(),
                        address: None,
                    };
                    self.set_account(Some(account.clone()));
                    return Ok(account);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        fn forget(&self) -> Result<(), String> {
            self.note("forget".into());
            self.set_account(None);
            Ok(())
        }
    }
}
