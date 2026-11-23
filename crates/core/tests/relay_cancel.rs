//! Backing out of a Game Mode sign-in: the poll stops, and nothing is written.
#![cfg(feature = "relay")]

use acnh_core::relay::{finish_sign_in_cancellable, start_sign_in};
use save_relay::creds::{CredentialStore, MemoryStore};
use save_relay::fake::FakeDrive;
use save_relay::http::{Request, Response, Transport};
use save_relay::local::LocalStore;
use save_relay::platform::AppDirs;
use save_relay::service::{self, Service};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

/// The device-code start is answered by the fake Drive; every token poll is "pending", and the
/// first poll turns the cancel flag on, as a person pressing Back would.
struct Pending {
    drive: FakeDrive,
    polls: AtomicUsize,
    cancel: Arc<AtomicBool>,
}

impl Transport for Pending {
    fn send(&self, request: &Request) -> save_relay::Result<Response> {
        if request.url.contains("/token") {
            self.polls.fetch_add(1, Ordering::SeqCst);
            self.cancel.store(true, Ordering::SeqCst);
            return Ok(Response::new(400, r#"{"error":"authorization_pending"}"#));
        }
        self.drive.send(request)
    }
}

#[test]
fn a_cancelled_device_flow_poll_ends_with_an_error_and_writes_nothing() {
    let root = std::env::temp_dir().join(format!("acnh-relay-cancel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dirs = AppDirs {
        config: root.join("config"),
        data: root.join("data"),
    };
    std::fs::create_dir_all(&dirs.config).unwrap();
    std::env::set_var("XDG_CURRENT_DESKTOP", "gamescope");
    let creds = MemoryStore::new();
    let cancel = Arc::new(AtomicBool::new(false));
    #[allow(clippy::arc_with_non_send_sync)]
    let transport = Arc::new(Pending {
        drive: FakeDrive::new(),
        polls: AtomicUsize::new(0),
        cancel: cancel.clone(),
    });
    start_sign_in(Service::Gdrive, &dirs, &creds, transport.clone()).unwrap();
    assert!(creds
        .get(&service::pending_account(Service::Gdrive))
        .unwrap()
        .is_some());

    let flag = cancel.clone();
    let started = std::time::Instant::now();
    let error = finish_sign_in_cancellable(
        Service::Gdrive,
        &dirs,
        &creds,
        transport.clone(),
        "",
        &move || flag.load(Ordering::SeqCst),
    )
    .unwrap_err();
    assert!(error.to_string().contains("cancelled"), "{error}");
    assert!(
        started.elapsed().as_secs() < 3,
        "the poll did not stop at once"
    );
    assert_eq!(transport.polls.load(Ordering::SeqCst), 1);
    // Nothing written: no config, and only the sign-in's own pending secret was removed.
    assert!(LocalStore::new(dirs.clone())
        .load_config()
        .unwrap()
        .is_none());
    assert!(creds
        .get(&service::pending_account(Service::Gdrive))
        .unwrap()
        .is_none());

    std::env::remove_var("XDG_CURRENT_DESKTOP");
    let _ = std::fs::remove_dir_all(&root);
}
