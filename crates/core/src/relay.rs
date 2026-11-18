//! The save-relay adapter (SPEC.md M6, feature `relay`): connects to save-relay's sync
//! engine, implementing `launch::Relay` for Animal Crossing: New Horizons using the
//! manifest from `saves::relay_manifest`. Outcomes from save-relay are mapped to
//! `launch::Sync`.

use crate::error::{Error, Result};
use crate::launch::{Keep, Relay, Sync};
use save_relay::api::{Action, SyncReport};
use save_relay::layout::Layout;
use save_relay::local::LocalStore;
use save_relay::manifest::Manifest;
use save_relay::remote::Remote;
use save_relay::sync::{self, Game, Resolve, Session};
use std::path::Path;
use std::sync::Arc;
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
