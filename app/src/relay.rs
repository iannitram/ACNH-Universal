//! The relay the Continue page plays with (SPEC.md M6): save-relay's setup on this machine when
//! there is one, `RelayOff` when save sync was never set up or was turned off, and an error
//! that says why when it is set up and cannot be opened. There is no quiet fall back from the
//! last to the one before it. Desktop builds only link save-relay: Android gets it through its
//! own app (#81) and the iPad's player through its Rust API (#172).

use crate::play::RelayOff;
use acnh_core::launch::{Keep, Relay, RelayStatus, Sync};
use acnh_core::{Error, Result};
use std::path::Path;
use std::sync::Arc;

/// The relay as the page's jobs hold it.
pub type SharedRelay = Arc<dyn Relay + Send + std::marker::Sync>;

/// Save sync is set up but could not be opened (a credential store that is locked, a config
/// that does not read): every step says why, so the page shows `RelayError` with the reason
/// instead of playing as if sync were off.
pub struct RelayFailed(pub Error);

impl Relay for RelayFailed {
    fn pull(&self, _save_dir: &Path, _resolve: Option<Keep>) -> Result<Sync> {
        Err(self.0.clone())
    }

    fn push(&self, _save_dir: &Path, _resolve: Option<Keep>) -> Result<Sync> {
        Err(self.0.clone())
    }

    fn status(&self, _save_dir: &Path) -> Result<RelayStatus> {
        Err(self.0.clone())
    }
}

/// A relay opened afresh for each step. save-relay's remote and credential store are not
/// `Send`, and the page's jobs run on threads of their own, so each step opens what it needs
/// on its own thread. It also means turning save sync on or off outside the app (the CLI's
/// `sync` command) is seen by the next step, with no restart.
struct Opened<F>(F);

impl<F> Relay for Opened<F>
where
    F: Fn() -> Box<dyn Relay> + Send + std::marker::Sync,
{
    fn pull(&self, save_dir: &Path, resolve: Option<Keep>) -> Result<Sync> {
        (self.0)().pull(save_dir, resolve)
    }

    fn push(&self, save_dir: &Path, resolve: Option<Keep>) -> Result<Sync> {
        (self.0)().push(save_dir, resolve)
    }

    fn pull_anyway(&self, save_dir: &Path) -> Result<Sync> {
        (self.0)().pull_anyway(save_dir)
    }

    fn status(&self, save_dir: &Path) -> Result<RelayStatus> {
        (self.0)().status(save_dir)
    }
}

/// A relay that is opened by `open` for every step.
pub fn opened_each_step<F>(open: F) -> SharedRelay
where
    F: Fn() -> Box<dyn Relay> + Send + std::marker::Sync + 'static,
{
    Arc::new(Opened(open))
}

/// The relay for what `acnh_core::relay::open` finds: the adapter when set up, `RelayOff` when
/// not, `RelayFailed` when opening went wrong.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn choose(
    dirs: &save_relay::platform::AppDirs,
    creds: &dyn save_relay::creds::CredentialStore,
    transport: Arc<dyn save_relay::http::Transport>,
) -> Box<dyn Relay> {
    match acnh_core::relay::open(dirs, creds, transport) {
        Ok(Some(relay)) => Box::new(relay),
        Ok(None) => Box::new(RelayOff),
        Err(error) => Box::new(RelayFailed(error)),
    }
}

/// This machine's relay: save-relay's own folders, the system credential store and the network.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn system() -> SharedRelay {
    opened_each_step(|| match save_relay::platform::PathVars::current() {
        Ok(vars) => choose(
            &save_relay::platform::AppDirs::from_vars(&vars),
            &save_relay::creds::KeyringStore::new(),
            Arc::new(save_relay::http::UreqTransport::new()),
        ),
        Err(error) => Box::new(RelayFailed(Error::Saves(error.to_string()))),
    })
}

/// Android and the iPad's save sync is not this adapter: the island here is played.
#[cfg(any(target_os = "android", target_os = "ios"))]
pub fn system() -> SharedRelay {
    Arc::new(RelayOff)
}
