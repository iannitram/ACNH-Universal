//! The save-relay adapter (SPEC.md M6, feature `relay`): connects to save-relay's sync
//! engine, implementing `launch::Relay` for Animal Crossing: New Horizons using the
//! manifest from `saves::relay_manifest`. Outcomes from save-relay are mapped to
//! `launch::Sync`.

use crate::error::{Error, Result};
use crate::launch::{Keep, Relay, RelayStatus, Sync};
use crate::savecard::SaveCard;
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
    version: Option<crate::version::Version>,
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
            version: None,
        }
    }

    /// Override the root folder under which game saves are stored on the server.
    pub fn with_root(mut self, root: impl Into<String>) -> Self {
        self.root = root.into();
        self
    }

    /// Record the selected game version on cards produced by this device.
    pub fn with_version(mut self, version: crate::version::Version) -> Self {
        self.version = Some(version);
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

impl SaveRelay {
    /// A pull with both knobs: the player's answer to a conflict (`resolve`) and Play anyway on
    /// an island another device holds (`anyway`). After Play anyway a conflict can still come
    /// back, and this settles it.
    pub fn pull_with(&self, save_dir: &Path, resolve: Option<Keep>, anyway: bool) -> Result<Sync> {
        let manifest = self.parse_manifest()?;
        let layout = Layout::new(&self.root).map_err(|e| Error::Saves(e.to_string()))?;
        let session = self.session(layout);
        let game = Game {
            manifest,
            saves_dir: save_dir.to_path_buf(),
        };
        let resolve_sync = map_resolve(resolve);
        let report =
            sync::pull(&session, &game, &self.store, resolve_sync, anyway).map_err(map_err)?;
        persist_card(save_dir, &report, self.version)?;
        Ok(map_sync_with_local(report, save_dir))
    }
}

impl Relay for SaveRelay {
    fn pull(&self, save_dir: &Path, resolve: Option<Keep>) -> Result<Sync> {
        self.pull_with(save_dir, resolve, false)
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
        persist_card(save_dir, &report, self.version)?;
        Ok(map_sync_with_local(report, save_dir))
    }
    fn pull_anyway(&self, save_dir: &Path, resolve: Option<Keep>) -> Result<Sync> {
        self.pull_with(save_dir, resolve, true)
    }

    fn status(&self, save_dir: &Path) -> Result<RelayStatus> {
        use save_relay::plan::Decision;
        let game = Game {
            manifest: self.parse_manifest()?,
            saves_dir: save_dir.to_path_buf(),
        };
        let session = self.session(Layout::new(&self.root).map_err(map_err)?);
        let status = sync::status(&session, &game, &self.store).map_err(map_err)?;
        Ok(match status.would {
            Decision::Noop | Decision::Adopt => RelayStatus::Ready,
            Decision::Push => RelayStatus::LocalNewer,
            Decision::Pull => {
                let card = current_card(&session, &game.manifest.id, status.server.as_ref());
                RelayStatus::RelayNewer(make_card(card.as_ref(), status.server.as_ref()))
            }
            Decision::Conflict => {
                let card = current_card(&session, &game.manifest.id, status.server.as_ref());
                let mut local = make_card(None, status.local.as_ref());
                restore_local_card_details(&mut local, status.local.as_ref(), save_dir);
                RelayStatus::Conflict {
                    server: make_card(card.as_ref(), status.server.as_ref()),
                    local,
                }
            }
            Decision::Offline => RelayStatus::Unavailable,
            Decision::Held => {
                let held = status.held.ok_or_else(|| {
                    Error::Saves("the relay did not identify the device holding the island".into())
                })?;
                RelayStatus::Held {
                    device: held.device,
                    since: held.since,
                }
            }
        })
    }
}

fn current_card(
    session: &Session<'_>,
    game: &str,
    side: Option<&save_relay::api::Side>,
) -> Option<save_relay::card::Card> {
    let side = side?;
    let history = sync::history(session, game).ok()?;
    history
        .entries
        .into_iter()
        .find(|entry| entry.current && entry.device == side.device && entry.time == side.time)?
        .card
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
        save_relay::Error::AuthFailed => Error::RelaySignIn,
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
    finish_sign_in_cancellable(service, dirs, creds, transport, answer, &|| false)
}

/// A transport that stops answering once the person backed out, so a device-flow poll that keeps
/// getting `authorization_pending` ends at its next request.
struct Cancelling<'a> {
    inner: &'a dyn Transport,
    cancelled: &'a dyn Fn() -> bool,
}

impl Transport for Cancelling<'_> {
    fn send(
        &self,
        request: &save_relay::http::Request,
    ) -> save_relay::Result<save_relay::http::Response> {
        if (self.cancelled)() {
            return Err(save_relay::Error::SignInFailed(CANCELLED.into()));
        }
        self.inner.send(request)
    }
}

/// What a sign-in that was backed out of says.
pub const CANCELLED: &str = "the sign-in was cancelled";

/// `finish_sign_in`, ending early when `cancelled` turns true: the device-flow poll sleeps in
/// short slices and checks it, every request checks it, and it is checked again before
/// anything is written. A cancelled sign-in writes no config and no token, and deletes only its
/// own pending code secret.
pub fn finish_sign_in_cancellable(
    service: Service,
    dirs: &AppDirs,
    creds: &dyn CredentialStore,
    transport: Arc<dyn Transport>,
    answer: &str,
    cancelled: &dyn Fn() -> bool,
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
        if cancelled() {
            return Err(Error::Saves(format!("sign-in failed: {CANCELLED}")));
        }
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
        if cancelled() {
            return Err(Error::Saves(format!("sign-in failed: {CANCELLED}")));
        }
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
    let guarded = Cancelling {
        inner: &*transport,
        cancelled,
    };
    let finished = oauth::finish_code_with_sleep(
        provider,
        client,
        &pending,
        typed,
        &guarded,
        clock,
        &|secs| {
            // Short slices, so backing out is noticed within a moment.
            let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
            while std::time::Instant::now() < until && !cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        },
    );
    if matches!(
        finished,
        Ok(_) | Err(save_relay::Error::ReauthNeeded(_) | save_relay::Error::SignInFailed(_))
    ) {
        creds.delete(&account).map_err(map_err)?;
    }
    let tokens = finished.map_err(map_err)?;
    if cancelled() {
        return Err(Error::Saves(format!("sign-in failed: {CANCELLED}")));
    }
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
fn make_card(
    card: Option<&save_relay::card::Card>,
    side: Option<&save_relay::api::Side>,
) -> SaveCard {
    let device = card
        .map(|c| c.device.clone())
        .or_else(|| side.map(|s| s.device.clone()))
        .unwrap_or_default();
    let time = card
        .map(|c| c.time.as_str())
        .or_else(|| side.map(|s| s.time.as_str()))
        .unwrap_or("");
    SaveCard {
        saved_unix: save_relay::stamp::parse_rfc3339(time).unwrap_or(0),
        device,
        play_seconds: card.and_then(|c| c.playtime_secs).unwrap_or(0),
        version: String::new(),
        picture: Default::default(),
    }
}

fn persist_card(
    save_dir: &Path,
    report: &SyncReport,
    version: Option<crate::version::Version>,
) -> Result<()> {
    if matches!(
        report.sync.action,
        Action::Pulled | Action::Pushed | Action::Adopted
    ) {
        if let Some(card) = &report.server_card {
            let old = crate::savecard::read_card(save_dir, Path::new("")).ok();
            let version = version
                .map(|v| v.name().to_string())
                .or_else(|| old.map(|c| c.version))
                .unwrap_or_default();
            let modified_ns = std::fs::metadata(save_dir.join("main.dat"))?
                .modified()?
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let content = format!(
                "device {}\nplay {}\nversion {}\nsaved {}\nfile_modified_ns {}\n",
                card.device.replace(['\r', '\n'], " "),
                card.playtime_secs.unwrap_or(0),
                version,
                save_relay::stamp::parse_rfc3339(&card.time).unwrap_or(0),
                modified_ns
            );
            use std::io::Write;
            let parent = save_dir.parent().unwrap_or_else(|| Path::new("."));
            let mut temp = tempfile::NamedTempFile::new_in(parent)?;
            temp.write_all(content.as_bytes())?;
            temp.persist(parent.join("launcher-card.txt"))
                .map_err(|e| Error::Io(e.error.to_string()))?;
        }
    }
    Ok(())
}

fn restore_local_card_details(
    local: &mut SaveCard,
    side: Option<&save_relay::api::Side>,
    save_dir: &Path,
) {
    let side_time = side.and_then(|s| save_relay::stamp::parse_rfc3339(&s.time));
    let file_time = std::fs::metadata(save_dir.join("main.dat"))
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    if file_time.is_some() && file_time == side_time && local.play_seconds == 0 {
        if let Ok(saved) = crate::savecard::read_card(save_dir, Path::new("")) {
            if saved.device == local.device {
                local.play_seconds = saved.play_seconds;
                local.version = saved.version;
            }
        }
    }
}

fn map_sync_with_local(report: SyncReport, save_dir: &Path) -> Sync {
    let side = report.sync.local.clone();
    let mut result = map_sync(report);
    if let Sync::Conflict { local, .. } = &mut result {
        restore_local_card_details(local, side.as_ref(), save_dir);
    }
    result
}

/// Map save-relay's report without losing either side or its backup.
pub fn map_sync(report: SyncReport) -> Sync {
    match report.sync.action {
        Action::Noop | Action::Adopted | Action::Pulled | Action::Pushed => {
            match report.sync.backup {
                Some(backup) => Sync::Resolved {
                    message: report.sync.message,
                    backup,
                },
                None => Sync::Done(report.sync.message),
            }
        }
        Action::Offline | Action::Deferred => Sync::Offline,
        Action::Conflict => Sync::Conflict {
            server: make_card(report.server_card.as_ref(), report.sync.server.as_ref()),
            local: make_card(report.local_card.as_ref(), report.sync.local.as_ref()),
            backup: report.sync.backup,
        },
        Action::Held => {
            let held = report.held.unwrap_or(HeldFallback::held());
            Sync::Held {
                device: held.device,
                since: held.since,
            }
        }
    }
}
struct HeldFallback;
impl HeldFallback {
    fn held() -> save_relay::api::Held {
        save_relay::api::Held {
            device: String::new(),
            since: String::new(),
        }
    }
}
