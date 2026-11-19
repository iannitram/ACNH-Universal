//! The save-relay adapter (SPEC.md M6, feature `relay`): connects to save-relay's sync
//! engine, implementing `launch::Relay` for Animal Crossing: New Horizons using the
//! manifest from `saves::relay_manifest`. Outcomes from save-relay are mapped to
//! `launch::Sync`.

use crate::error::{Error, Result};
use crate::launch::{Keep, Relay, Sync};
use save_relay::api::{Action, SignInPrompt, SyncReport};
use save_relay::clients::{Client, ClientSet};
use save_relay::creds::{self, CredentialStore, Secret};
use save_relay::http::Transport;
use save_relay::layout::Layout;
use save_relay::local::{Config, LocalStore};
use save_relay::manifest::Manifest;
use save_relay::oauth::{self, Pending, PlatformSignIn};
use save_relay::platform::AppDirs;
use save_relay::remote::Remote;
use save_relay::service::{self, Service};
use save_relay::sync::{self, Game, Resolve, Session};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// The save-relay adapter implementing `launch::Relay`.
pub struct SaveRelay {
    remote: Arc<dyn Remote>,
    store: LocalStore,
    device: String,
    root: String,
    clock: Option<Arc<dyn Fn() -> u64 + Send + core::marker::Sync>>,
}

impl SaveRelay {
    /// Create a new `SaveRelay` adapter with the given remote, local store, and device name.
    pub fn new(remote: Arc<dyn Remote>, store: LocalStore, device: impl Into<String>) -> Self {
        Self {
            remote,
            store,
            device: device.into(),
            root: save_relay::cli::DEFAULT_ROOT.to_string(),
            clock: None,
        }
    }

    /// Override the root folder under which game saves are stored on the server.
    pub fn with_root(mut self, root: impl Into<String>) -> Self {
        self.root = root.into();
        self
    }

    /// Set a custom clock for testing.
    pub fn with_clock(
        mut self,
        clock: impl Fn() -> u64 + Send + core::marker::Sync + 'static,
    ) -> Self {
        self.clock = Some(Arc::new(clock));
        self
    }

    fn now(&self) -> u64 {
        if let Some(clock) = &self.clock {
            clock()
        } else {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        }
    }

    fn parse_manifest(&self) -> Result<Manifest> {
        Manifest::from_json(&crate::saves::relay_manifest())
            .map_err(|e| Error::Saves(e.to_string()))
    }

    fn session<'a>(&'a self, layout: Layout) -> Session<'a> {
        Session {
            remote: &*self.remote,
            layout,
            device: self.device.clone(),
            now: self.now(),
        }
    }
}

impl Relay for SaveRelay {
    fn pull(&self, save_dir: &Path, resolve: Option<Keep>) -> Result<Sync> {
        let manifest = self.parse_manifest()?;
        let layout = Layout::new(&self.root).map_err(|e| Error::Saves(e.to_string()))?;
        let session = self.session(layout);
        let game = Game {
            manifest,
            saves_dir: save_dir.to_path_buf(),
        };
        let resolve_sync = map_resolve(resolve);
        let anyway = resolve.is_some();
        let report =
            sync::pull(&session, &game, &self.store, resolve_sync, anyway).map_err(map_err)?;
        Ok(map_sync(report))
    }

    fn push(&self, save_dir: &Path, resolve: Option<Keep>) -> Result<Sync> {
        let manifest = self.parse_manifest()?;
        let layout = Layout::new(&self.root).map_err(|e| Error::Saves(e.to_string()))?;
        let session = self.session(layout);
        let game = Game {
            manifest,
            saves_dir: save_dir.to_path_buf(),
        };
        let resolve_sync = map_resolve(resolve);
        let report = sync::push(&session, &game, &self.store, resolve_sync).map_err(map_err)?;
        Ok(map_sync(report))
    }
}

fn map_resolve(keep: Option<Keep>) -> Option<Resolve> {
    keep.map(|k| match k {
        Keep::Server => Resolve::KeepServer,
        Keep::Local => Resolve::KeepLocal,
    })
}

fn map_err(err: save_relay::Error) -> Error {
    match err {
        save_relay::Error::Io(msg) => Error::Io(msg),
        other => Error::Saves(other.to_string()),
    }
}

/// A browser sign-in waiting for the app's redirect (desktop and Mac; Game Mode's code flow
/// waits in the credential store instead).
struct PendingPlatform {
    service: Service,
    sign_in: PlatformSignIn,
    device: String,
    root: String,
}

static PLATFORM_SIGN_IN: Mutex<Option<PendingPlatform>> = Mutex::new(None);

fn platform_lock() -> Result<std::sync::MutexGuard<'static, Option<PendingPlatform>>> {
    PLATFORM_SIGN_IN
        .lock()
        .map_err(|_| Error::Saves("the save sync sign-in is unavailable".into()))
}

/// Open the save-relay machine setup this app shares with the `save-relay` CLI: its config
/// through its `LocalStore`, and its app password or stored tokens through `creds`. Nothing
/// set up (no config, or no secret yet) is `Ok(None)`, never an error. A credential store that
/// cannot be reached is a plain error, so the launcher can play on with the local island.
pub fn open(
    dirs: &AppDirs,
    creds: &dyn CredentialStore,
    transport: Arc<dyn Transport>,
) -> Result<Option<SaveRelay>> {
    let store = LocalStore::new(dirs.clone());
    let config = match store.load_config().map_err(map_err)? {
        Some(config) => config,
        None => return Ok(None),
    };
    if config.format != save_relay::local::FORMAT {
        return Err(Error::Saves(format!(
            "save sync config format {} is not {}",
            config.format,
            save_relay::local::FORMAT
        )));
    }
    let service = Service::from_server(&config.server).map_err(map_err)?;

    if !service.uses_oauth() {
        let (config, password) = match save_relay::cli::connection(&store, creds) {
            Ok(connection) => connection,
            Err(save_relay::Error::NotSetUp) => return Ok(None),
            Err(error) => return Err(map_err(error)),
        };
        let remote =
            save_relay::cli::connect_with(&config.server, &config.user, password, transport)
                .map_err(map_err)?;
        return Ok(Some(
            SaveRelay::new(Arc::from(remote), store, config.device).with_root(config.root),
        ));
    }

    let opened = match service::open_with(
        &config,
        creds,
        transport,
        save_relay::stamp::now,
        &ClientSet::shipped(),
    ) {
        Ok(opened) => opened,
        Err(save_relay::Error::NotSetUp) => return Ok(None),
        Err(error) => return Err(map_err(error)),
    };
    Ok(Some(
        SaveRelay::new(Arc::from(opened.remote), store, config.device).with_root(config.root),
    ))
}

/// Set this machine up for a Nextcloud server: check that it answers, then write save-relay's
/// config and file the app password in `creds` only. The password is never written to a file,
/// a log line or an error message.
pub fn setup_nextcloud(
    server: &str,
    user: &str,
    app_password: &str,
    device: &str,
    dirs: &AppDirs,
    creds: &dyn CredentialStore,
    transport: Arc<dyn Transport>,
) -> Result<()> {
    let password = Secret::new(app_password);
    let account = creds::account(server, user).map_err(map_err)?;
    let layout = Layout::new(save_relay::cli::DEFAULT_ROOT).map_err(map_err)?;
    let device = save_relay::layout::device_slug(device);
    let remote = save_relay::cli::connect_with(server, user, password.clone(), transport)
        .map_err(map_err)?;
    remote.list("").map_err(map_err)?;
    save_relay::remote::ensure_dir(&*remote, layout.root()).map_err(map_err)?;

    let store = LocalStore::new(dirs.clone());
    if let Some(old) = store.load_config().map_err(map_err)? {
        if old.server != server || old.user != user {
            for stale in service::credential_accounts(&old).map_err(map_err)? {
                creds.delete(&stale).map_err(map_err)?;
            }
        }
    }
    creds.set(&account, &password).map_err(map_err)?;
    let config = Config {
        format: save_relay::local::FORMAT,
        server: server.to_string(),
        user: user.to_string(),
        root: layout.root().to_string(),
        device,
    };
    store.save_config(&config).map_err(map_err)?;
    Ok(())
}

/// Start a Google Drive or Dropbox sign-in in the shape the launcher's page can draw: the
/// browser flow on a desktop, or save-relay's code flow in Game Mode (`oauth::in_game_mode`).
/// Nothing is set up until `finish_sign_in` succeeds.
pub fn start_sign_in(
    service: Service,
    dirs: &AppDirs,
    creds: &dyn CredentialStore,
    transport: Arc<dyn Transport>,
) -> Result<SignInPrompt> {
    if !service.uses_oauth() {
        return Err(Error::Saves(
            "Nextcloud is set up with an app password, not a sign-in".into(),
        ));
    }
    let provider = oauth::provider(service)
        .ok_or_else(|| Error::Saves("this service has no sign-in".into()))?;
    let clients = ClientSet::shipped();
    let store = LocalStore::new(dirs.clone());
    let now = save_relay::stamp::now();

    if oauth::in_game_mode(&|name| std::env::var(name).ok()) {
        let client = code_client(service, &clients);
        let pending = oauth::start_code(provider, client, &*transport, now).map_err(map_err)?;
        creds
            .set(&service::pending_account(service), &pending.to_secret())
            .map_err(map_err)?;
        return Ok(code_prompt(service, &pending));
    }

    let client = platform_client(service, &clients);
    let sign_in =
        oauth::start_platform_sign_in(provider, client, save_relay::clients::DROPBOX_REDIRECT, now)
            .map_err(map_err)?;
    let (device, root) = setup_identity(&store)?;
    let prompt = SignInPrompt {
        service: service.key().to_string(),
        url: sign_in.url.clone(),
        code: None,
        expires_at: save_relay::stamp::rfc3339(sign_in.expires_at),
        message: format!(
            "Open this page in your browser and sign in to {}. When the browser returns to the app, the sign-in finishes.",
            service.label()
        ),
    };
    *platform_lock()? = Some(PendingPlatform {
        service,
        sign_in,
        device,
        root,
    });
    Ok(prompt)
}

/// Finish the sign-in `start_sign_in` began. `answer` is the app's redirect URL on a desktop,
/// or the code Dropbox shows in Game Mode (ignored for Google's device flow). The refresh token
/// goes to `creds` only, and the config is written before this returns.
pub fn finish_sign_in(
    service: Service,
    dirs: &AppDirs,
    creds: &dyn CredentialStore,
    transport: Arc<dyn Transport>,
    answer: &str,
) -> Result<()> {
    if !service.uses_oauth() {
        return Err(Error::Saves(
            "Nextcloud is set up with an app password, not a sign-in".into(),
        ));
    }
    let provider = oauth::provider(service)
        .ok_or_else(|| Error::Saves("this service has no sign-in".into()))?;
    let clients = ClientSet::shipped();
    let store = LocalStore::new(dirs.clone());
    let clock = save_relay::stamp::now;

    if let Some(pending) = take_platform_sign_in(service)? {
        let client = platform_client(service, &clients);
        let tokens = match oauth::finish_platform_sign_in(
            provider,
            client,
            &pending.sign_in,
            answer,
            &*transport,
            clock,
        ) {
            Ok(tokens) => tokens,
            Err(error) => {
                if matches!(
                    error,
                    save_relay::Error::Unreachable(_) | save_relay::Error::Server(_)
                ) {
                    *platform_lock()? = Some(pending);
                }
                return Err(map_err(error));
            }
        };
        return finish_setup(
            service,
            tokens,
            client,
            "desktop",
            &pending.device,
            &pending.root,
            dirs,
            creds,
            transport,
        );
    }

    let account = service::pending_account(service);
    let secret = creds
        .get(&account)
        .map_err(map_err)?
        .ok_or_else(|| Error::Saves("no save sync sign-in is waiting: start it first".into()))?;
    let pending = Pending::from_secret(&secret).map_err(map_err)?;
    if pending.service != service {
        return Err(Error::Saves(
            "the waiting save sync sign-in is for another service: start it again".into(),
        ));
    }
    let typed = if service == Service::Dropbox {
        Some(answer)
    } else {
        None
    };
    let client = code_client(service, &clients);
    let finished = oauth::finish_code(provider, client, &pending, typed, &*transport, clock);
    if matches!(
        finished,
        Ok(_) | Err(save_relay::Error::ReauthNeeded(_) | save_relay::Error::SignInFailed(_))
    ) {
        creds.delete(&account).map_err(map_err)?;
    }
    let tokens = finished.map_err(map_err)?;
    let kind = if service == Service::Gdrive {
        "device"
    } else {
        ""
    };
    let (device, root) = setup_identity(&store)?;
    finish_setup(
        service, tokens, client, kind, &device, &root, dirs, creds, transport,
    )
}

/// Remove save-relay's config and every secret this machine stored for it: Settings' "Turn off
/// save sync". A machine that was never set up is `Ok(())`.
pub fn forget(dirs: &AppDirs, creds: &dyn CredentialStore) -> Result<()> {
    let store = LocalStore::new(dirs.clone());
    if let Some(config) = store.load_config().map_err(map_err)? {
        for account in service::credential_accounts(&config).map_err(map_err)? {
            creds.delete(&account).map_err(map_err)?;
        }
    }
    *platform_lock()? = None;
    match std::fs::remove_file(dirs.config.join("config.json")) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::Io(error.to_string())),
    }
}

fn take_platform_sign_in(service: Service) -> Result<Option<PendingPlatform>> {
    let mut pending = platform_lock()?;
    if pending
        .as_ref()
        .is_some_and(|waiting| waiting.service == service)
    {
        Ok(pending.take())
    } else {
        Ok(None)
    }
}

fn platform_client(service: Service, clients: &ClientSet) -> Client {
    match service {
        Service::Gdrive => clients.google_desktop,
        Service::Dropbox => clients.dropbox,
        Service::Nextcloud => clients.dropbox,
    }
}

fn code_client(service: Service, clients: &ClientSet) -> Client {
    match service {
        Service::Gdrive => clients.google_device,
        Service::Dropbox => clients.dropbox,
        Service::Nextcloud => clients.dropbox,
    }
}

fn code_prompt(service: Service, pending: &Pending) -> SignInPrompt {
    let message = match (service, &pending.user_code) {
        (Service::Gdrive, Some(code)) => format!(
            "On a phone or computer, open {} and enter {code}. Then finish the sign-in here.",
            pending.url
        ),
        _ => format!(
            "On a phone or computer, open {} and sign in to Dropbox. Then type the code Dropbox shows here.",
            pending.url
        ),
    };
    SignInPrompt {
        service: service.key().to_string(),
        url: pending.url.clone(),
        code: pending.user_code.clone(),
        expires_at: save_relay::stamp::rfc3339(pending.expires_at),
        message,
    }
}

fn setup_identity(store: &LocalStore) -> Result<(String, String)> {
    if let Some(config) = store.load_config().map_err(map_err)? {
        return Ok((config.device, config.root));
    }
    Ok((
        save_relay::layout::device_slug(&default_device_name()),
        save_relay::cli::DEFAULT_ROOT.to_string(),
    ))
}

fn default_device_name() -> String {
    ["HOSTNAME", "COMPUTERNAME"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "device".to_string())
}

#[allow(clippy::too_many_arguments)]
fn finish_setup(
    service: Service,
    tokens: oauth::Tokens,
    client: Client,
    kind: &str,
    device: &str,
    root: &str,
    dirs: &AppDirs,
    creds: &dyn CredentialStore,
    transport: Arc<dyn Transport>,
) -> Result<()> {
    let store = LocalStore::new(dirs.clone());
    save_relay::cli::finish_platform_setup(
        service,
        tokens,
        client,
        kind,
        Some(device),
        root,
        creds,
        &store,
        transport,
        save_relay::stamp::now,
    )
    .map_err(map_err)?;
    Ok(())
}

/// Map save-relay's `SyncReport` outcome to `launch::Sync`.
pub fn map_sync(report: SyncReport) -> Sync {
    match report.sync.action {
        Action::Noop | Action::Adopted | Action::Pulled | Action::Pushed => {
            Sync::Done(report.sync.message)
        }
        Action::Offline | Action::Deferred => Sync::Offline,
        Action::Conflict => {
            let server = report
                .server_card
                .as_ref()
                .map(|c| c.to_json())
                .or_else(|| {
                    report
                        .sync
                        .server
                        .as_ref()
                        .map(|s| format!("{}, {}", s.device, s.time))
                })
                .unwrap_or_else(|| "server".to_string());
            let local = report
                .local_card
                .as_ref()
                .map(|c| c.to_json())
                .or_else(|| {
                    report
                        .sync
                        .local
                        .as_ref()
                        .map(|s| format!("{}, {}", s.device, s.time))
                })
                .unwrap_or_else(|| "local".to_string());
            Sync::Conflict { server, local }
        }
        Action::Held => {
            let server = report
                .held
                .as_ref()
                .map(|h| format!("held by {} since {}", h.device, h.since))
                .unwrap_or_else(|| "held by another machine".to_string());
            let local = report
                .local_card
                .as_ref()
                .map(|c| c.to_json())
                .unwrap_or_default();
            Sync::Conflict { server, local }
        }
    }
}
