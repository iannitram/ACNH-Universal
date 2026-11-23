#![cfg(feature = "relay")]

use acnh_core::launch::{Keep, Relay, Sync};
use acnh_core::relay::{
    finish_sign_in, forget, map_sync, open, setup_nextcloud, start_sign_in, SaveRelay,
};
use save_relay::api::{Action, Held, Side, SyncData, SyncReport};
use save_relay::clients::DROPBOX_REDIRECT;
use save_relay::creds::{self, CredentialStore, MemoryStore};
use save_relay::fake::{FakeDrive, FakeDropbox, REFRESH_TOKEN, SIGN_IN_CODE};
use save_relay::http::{Request, Response, Transport};
use save_relay::local::LocalStore;
use save_relay::platform::{AppDirs, PathVars};
use save_relay::remote::{MemoryRemote, Precondition, Remote};
use save_relay::service::{self, Service};
use save_relay::transfer::Counts;
use save_relay::webdav::{encode_path, encode_segment};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

fn temp_fixture_dir(name: &str) -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "acnh-core-relay-test-{}-{}-{}",
        std::process::id(),
        name,
        id
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn create_store(root: &std::path::Path) -> LocalStore {
    let config = root.join("config");
    let data = root.join("data");
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&data).unwrap();
    LocalStore::new(AppDirs { config, data })
}

fn populate_island(save_dir: &std::path::Path, marker: &str) {
    fs::create_dir_all(save_dir.join("Villager0")).unwrap();
    fs::write(
        save_dir.join("main.dat"),
        format!("main island data: {marker}"),
    )
    .unwrap();
    fs::write(
        save_dir.join("mainHeader.dat"),
        format!("mainHeader: {marker}"),
    )
    .unwrap();
    fs::write(
        save_dir.join("Villager0/personal.dat"),
        format!("villager: {marker}"),
    )
    .unwrap();
}

const PASSWORD: &str = "app-password";
const WRONG_PASSWORD: &str = "wrong-password";
const DAV_PREFIX: &str = "/remote.php/dav/files/alex/";

fn temp_dirs(name: &str) -> (PathBuf, AppDirs) {
    let root = temp_fixture_dir(name);
    let dirs = AppDirs {
        config: root.join("config"),
        data: root.join("data"),
    };
    fs::create_dir_all(&dirs.config).unwrap();
    fs::create_dir_all(&dirs.data).unwrap();
    (root, dirs)
}

fn read_all_text(dir: &Path) -> String {
    let mut text = String::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let entries = match fs::read_dir(&next) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(bytes) = fs::read(&path) {
                text.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
    }
    text
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).map_or(0, |b| u32::from(*b));
        let b2 = chunk.get(2).map_or(0, |b| u32::from(*b));
        let bits = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((bits >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((bits >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((bits >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(bits & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn dav_path(url: &str) -> String {
    let rest = url.split_once(DAV_PREFIX).map_or("", |(_, rest)| rest);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    percent_decode(rest)
}

fn error_response(error: save_relay::Error) -> Response {
    match error {
        save_relay::Error::NotFound(_) => Response::new(404, ""),
        save_relay::Error::AuthFailed => Response::new(401, ""),
        save_relay::Error::Changed => Response::new(412, ""),
        save_relay::Error::Server(message) if message == "exists" => Response::new(412, ""),
        _ => Response::new(500, ""),
    }
}

struct FakeDav {
    remote: MemoryRemote,
    auth: String,
    log: Mutex<Vec<Request>>,
}

impl FakeDav {
    fn new(password: &str) -> FakeDav {
        FakeDav {
            remote: MemoryRemote::new(),
            auth: format!("Basic {}", base64(format!("alex:{password}").as_bytes())),
            log: Mutex::new(Vec::new()),
        }
    }

    fn file_paths(&self) -> Vec<String> {
        self.remote.file_paths()
    }

    fn sent(&self) -> Vec<Request> {
        self.log.lock().unwrap().clone()
    }

    fn propfind(&self, dir: &str) -> save_relay::Result<Response> {
        let entries = match self.remote.list(dir) {
            Ok(entries) => entries,
            Err(save_relay::Error::NotFound(path)) => {
                return Ok(Response::new(404, path));
            }
            Err(error) => return Ok(error_response(error)),
        };
        let own = format!("{DAV_PREFIX}{}", encode_path(dir));
        let own = own.trim_end_matches('/');
        let mut xml =
            String::from(r#"<?xml version="1.0" encoding="utf-8"?><d:multistatus xmlns:d="DAV:">"#);
        for entry in entries {
            let href = if entry.is_dir {
                format!("{own}/{}/", encode_segment(&entry.name))
            } else {
                format!("{own}/{}", encode_segment(&entry.name))
            };
            xml.push_str("<d:response><d:href>");
            xml.push_str(&href);
            xml.push_str("</d:href><d:propstat><d:prop><d:resourcetype>");
            if entry.is_dir {
                xml.push_str("<d:collection/>");
            }
            xml.push_str("</d:resourcetype>");
            if !entry.is_dir {
                xml.push_str("<d:getcontentlength>");
                xml.push_str(&entry.size.to_string());
                xml.push_str("</d:getcontentlength>");
            }
            if let Some(etag) = entry.etag {
                xml.push_str("<d:getetag>");
                xml.push_str(&etag);
                xml.push_str("</d:getetag>");
            }
            xml.push_str("</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>");
        }
        xml.push_str("</d:multistatus>");
        Ok(Response::new(207, xml).header("Content-Type", "application/xml"))
    }
}

impl Transport for FakeDav {
    fn send(&self, request: &Request) -> save_relay::Result<Response> {
        self.log.lock().unwrap().push(request.clone());
        if request.header_value("Authorization") != Some(self.auth.as_str()) {
            return Ok(Response::new(401, ""));
        }
        let path = dav_path(&request.url);
        match request.method.as_str() {
            "PROPFIND" => self.propfind(&path),
            "GET" => match self.remote.get(&path) {
                Ok((body, etag)) => Ok(Response::new(200, body).header("ETag", &etag)),
                Err(error) => Ok(error_response(error)),
            },
            "PUT" => {
                let pre = match request.header_value("If-None-Match") {
                    Some("*") => Precondition::Absent,
                    Some(etag) => Precondition::Match(etag.to_string()),
                    None => match request.header_value("If-Match") {
                        Some(etag) => Precondition::Match(etag.to_string()),
                        None => Precondition::Any,
                    },
                };
                match self.remote.put(&path, &request.body, &pre) {
                    Ok(etag) => Ok(Response::new(201, "").header("ETag", &etag)),
                    Err(error) => Ok(error_response(error)),
                }
            }
            "MKCOL" => match self.remote.mkcol(&path) {
                Ok(()) => Ok(Response::new(201, "")),
                Err(save_relay::Error::NotFound(_)) => Ok(Response::new(409, "")),
                Err(error) => Ok(error_response(error)),
            },
            "COPY" | "MOVE" => {
                let to = request
                    .header_value("Destination")
                    .map(dav_path)
                    .unwrap_or_default();
                let result = if request.method == "COPY" {
                    self.remote.copy(&path, &to)
                } else {
                    self.remote.move_to(&path, &to)
                };
                match result {
                    Ok(()) => Ok(Response::new(201, "")),
                    Err(save_relay::Error::NotFound(from)) if from == path => {
                        Ok(Response::new(404, ""))
                    }
                    Err(save_relay::Error::NotFound(_)) => Ok(Response::new(409, "")),
                    Err(error) => Ok(error_response(error)),
                }
            }
            "DELETE" => match self.remote.delete(&path) {
                Ok(()) => Ok(Response::new(204, "")),
                Err(error) => Ok(error_response(error)),
            },
            _ => Ok(Response::new(405, "")),
        }
    }
}

fn query_param(url: &str, name: &str) -> Option<String> {
    let query = url.split_once('?').map_or("", |(_, query)| query);
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        if key == name {
            Some(percent_decode(value))
        } else {
            None
        }
    })
}

#[test]
fn push_and_pull_between_devices() {
    let root = temp_fixture_dir("push-pull");
    let remote = Arc::new(MemoryRemote::new());

    // Device A (desktop) creates an island and pushes it
    let desktop_dir = root.join("desktop");
    let desktop_saves = desktop_dir.join("saves");
    populate_island(&desktop_saves, "v1-desktop");
    let desktop_store = create_store(&desktop_dir);
    let desktop_relay = SaveRelay::new(remote.clone(), desktop_store, "desktop");

    let push_result = desktop_relay.push(&desktop_saves, None).unwrap();
    match push_result {
        Sync::Done(msg) => {
            assert!(
                msg.contains("Pushed revision 1") || msg.contains("desktop"),
                "{msg}"
            );
        }
        other => panic!("expected Sync::Done, got {other:?}"),
    }

    // Device B (handheld) pulls the island
    let handheld_dir = root.join("handheld");
    let handheld_saves = handheld_dir.join("saves");
    let handheld_store = create_store(&handheld_dir);
    let handheld_relay = SaveRelay::new(remote, handheld_store, "handheld");

    let pull_result = handheld_relay.pull(&handheld_saves, None).unwrap();
    match pull_result {
        Sync::Done(msg) => {
            assert!(
                msg.contains("Pulled revision 1") || msg.contains("Adopted"),
                "{msg}"
            );
        }
        other => panic!("expected Sync::Done, got {other:?}"),
    }

    // Verify pulled files match
    let content = fs::read_to_string(handheld_saves.join("main.dat")).unwrap();
    assert_eq!(content, "main island data: v1-desktop");
    let villager = fs::read_to_string(handheld_saves.join("Villager0/personal.dat")).unwrap();
    assert_eq!(villager, "villager: v1-desktop");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn offline_pull_and_push() {
    let root = temp_fixture_dir("offline");
    let remote = Arc::new(MemoryRemote::new());

    let device_dir = root.join("device");
    let saves_dir = device_dir.join("saves");
    populate_island(&saves_dir, "offline-test");
    let store = create_store(&device_dir);
    let relay = SaveRelay::new(remote.clone(), store, "handheld");

    // Make server unreachable
    remote.set_reachable(false);

    // Pull offline maps to Sync::Offline
    let pull_result = relay.pull(&saves_dir, None).unwrap();
    assert_eq!(pull_result, Sync::Offline);

    // Push offline maps to Sync::Offline
    let push_result = relay.push(&saves_dir, None).unwrap();
    assert_eq!(push_result, Sync::Offline);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn conflict_detected_and_resolved() {
    let root = temp_fixture_dir("conflict");
    let remote = Arc::new(MemoryRemote::new());

    // Step 1: Device A creates initial island and pushes it
    let dev_a_dir = root.join("dev_a");
    let dev_a_saves = dev_a_dir.join("saves");
    populate_island(&dev_a_saves, "initial");
    let store_a = create_store(&dev_a_dir);
    let relay_a = SaveRelay::new(remote.clone(), store_a, "device-a");
    assert!(matches!(
        relay_a.push(&dev_a_saves, None).unwrap(),
        Sync::Done(_)
    ));

    // Step 2: Device B pulls initial island
    let dev_b_dir = root.join("dev_b");
    let dev_b_saves = dev_b_dir.join("saves");
    let store_b = create_store(&dev_b_dir);
    let relay_b = SaveRelay::new(remote.clone(), store_b, "device-b");
    assert!(matches!(
        relay_b.pull(&dev_b_saves, None).unwrap(),
        Sync::Done(_)
    ));

    // Step 3: Device A makes changes and pushes revision 2
    fs::write(
        dev_a_saves.join("main.dat"),
        "main island data: revision-2-a",
    )
    .unwrap();
    assert!(matches!(
        relay_a.push(&dev_a_saves, None).unwrap(),
        Sync::Done(_)
    ));

    // Step 4: Device B also makes changes locally without pulling
    fs::write(
        dev_b_saves.join("main.dat"),
        "main island data: conflicting-b",
    )
    .unwrap();

    // Step 5: Device B tries to pull -> Conflict!
    let conflict = relay_b.pull(&dev_b_saves, None).unwrap();
    match &conflict {
        Sync::Conflict { server, local, .. } => {
            assert!(
                !server.device.is_empty(),
                "server description should not be empty"
            );
            assert!(
                !local.device.is_empty(),
                "local description should not be empty"
            );
        }
        other => panic!("expected Sync::Conflict, got {other:?}"),
    }

    // Step 6: Device B resolves conflict with Keep::Server
    let resolved = relay_b.pull(&dev_b_saves, Some(Keep::Server)).unwrap();
    match resolved {
        Sync::Resolved { backup, .. } => assert!(!backup.is_empty()),
        other => panic!("expected a retained backup, got {other:?}"),
    }

    // Verify Device B now has Device A's changes
    let content = fs::read_to_string(dev_b_saves.join("main.dat")).unwrap();
    assert_eq!(content, "main island data: revision-2-a");

    let _ = fs::remove_dir_all(&root);
}

struct DummyGame;
impl acnh_core::launch::Game for DummyGame {
    fn run(&self, _env: &[(String, String)]) -> acnh_core::Result<i32> {
        Ok(0)
    }
}

#[test]
fn play_flow_with_save_relay() {
    let root = temp_fixture_dir("play-flow");
    let remote = Arc::new(MemoryRemote::new());

    let dev_dir = root.join("device");
    let saves_dir = dev_dir.join("saves");
    populate_island(&saves_dir, "play-flow-v1");
    let store = create_store(&dev_dir);
    let relay = SaveRelay::new(remote, store, "player-device");
    let game = DummyGame;

    let played = acnh_core::launch::play(&relay, &game, &saves_dir, &[], &|_| {
        panic!("choose should not be called")
    })
    .unwrap();

    assert!(matches!(played.pulled, Sync::Done(_)));
    assert_eq!(played.exit_code, 0);
    assert!(matches!(played.pushed, Sync::Done(_)));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn open_with_nothing_set_up_is_none() {
    let (root, dirs) = temp_dirs("open-empty");
    let creds = MemoryStore::new();
    let fake = Arc::new(FakeDav::new(PASSWORD));

    assert!(open(&dirs, &creds, fake).unwrap().is_none());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn setup_nextcloud_then_open_pulls_and_pushes_to_the_fake_server() {
    let (root, dirs) = temp_dirs("nextcloud");
    let creds = MemoryStore::new();
    let fake = Arc::new(FakeDav::new(PASSWORD));

    setup_nextcloud(
        "https://cloud.test",
        "alex",
        PASSWORD,
        "desktop",
        &dirs,
        &creds,
        fake.clone(),
    )
    .unwrap();

    let account = creds::account("https://cloud.test", "alex").unwrap();
    assert_eq!(creds.get(&account).unwrap().unwrap().expose(), PASSWORD);

    let relay = open(&dirs, &creds, fake.clone()).unwrap().expect("set up");
    let saves = root.join("saves");
    populate_island(&saves, "v1");
    assert!(matches!(relay.push(&saves, None).unwrap(), Sync::Done(_)));
    assert!(!fake.file_paths().is_empty());
    assert!(matches!(relay.pull(&saves, None).unwrap(), Sync::Done(_)));

    let files = read_all_text(&root);
    assert!(!files.contains(PASSWORD), "the password reached a file");
    assert!(!fake.sent().is_empty());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_wrong_password_is_a_plain_error_that_stores_nothing() {
    let (root, dirs) = temp_dirs("wrong-password");
    let creds = MemoryStore::new();
    let fake = Arc::new(FakeDav::new(PASSWORD));

    let error = setup_nextcloud(
        "https://cloud.test",
        "alex",
        WRONG_PASSWORD,
        "desktop",
        &dirs,
        &creds,
        fake.clone(),
    )
    .unwrap_err();
    let message = error.to_string();
    assert!(!message.contains(WRONG_PASSWORD), "{message}");
    assert!(!message.contains(PASSWORD), "{message}");
    assert!(!dirs.config.join("config.json").exists());
    assert!(open(&dirs, &creds, fake).unwrap().is_none());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_config_written_by_save_relay_setup_opens_the_same_way() {
    let root = temp_fixture_dir("cli-setup");
    let vars = PathVars {
        home: root.clone(),
        data: root.join("data"),
        config: root.join("config"),
        documents: None,
    };
    let dirs = AppDirs::from_vars(&vars);
    fs::create_dir_all(&dirs.config).unwrap();
    fs::create_dir_all(&dirs.data).unwrap();
    let store = LocalStore::new(dirs.clone());
    let creds = MemoryStore::new();
    let remote = MemoryRemote::new();
    let args = vec![
        "setup".to_string(),
        "--service".to_string(),
        "nextcloud".to_string(),
        "--server".to_string(),
        "https://cloud.test".to_string(),
        "--user".to_string(),
        "alex".to_string(),
    ];
    let mut stdin = Cursor::new(format!("{PASSWORD}\n").into_bytes());
    let mut stdout = Vec::new();
    let code = save_relay::cli::run_with(
        &args,
        &mut stdin,
        &mut stdout,
        &remote,
        &creds,
        &store,
        &vars,
        1_000,
    );
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stdout));

    let fake = Arc::new(FakeDav::new(PASSWORD));
    assert!(open(&dirs, &creds, fake).unwrap().is_some());

    let _ = fs::remove_dir_all(&root);
}

#[test]
#[allow(clippy::arc_with_non_send_sync)]
fn sign_in_uses_a_code_prompt_in_game_mode_and_a_browser_url_on_desktop() {
    let (root, dirs) = temp_dirs("sign-in");
    let creds = MemoryStore::new();

    std::env::set_var("XDG_CURRENT_DESKTOP", "gamescope");
    let drive = Arc::new(FakeDrive::new());
    let prompt = start_sign_in(Service::Gdrive, &dirs, &creds, drive.clone()).unwrap();
    assert!(prompt.code.is_some());
    assert!(prompt.url.contains("google.com/device"), "{}", prompt.url);
    finish_sign_in(Service::Gdrive, &dirs, &creds, drive, "").unwrap();
    let google = service::token_account(Service::Gdrive, "fake@example.com").unwrap();
    assert_eq!(creds.get(&google).unwrap().unwrap().expose(), REFRESH_TOKEN);
    assert!(LocalStore::new(dirs.clone())
        .load_config()
        .unwrap()
        .is_some());

    std::env::remove_var("XDG_CURRENT_DESKTOP");
    std::env::remove_var("SteamGamepadUI");
    let dropbox = Arc::new(FakeDropbox::new());
    let prompt = start_sign_in(Service::Dropbox, &dirs, &creds, dropbox.clone()).unwrap();
    assert!(prompt.code.is_none());
    assert!(prompt.url.contains("redirect_uri"), "{}", prompt.url);
    let state = query_param(&prompt.url, "state").unwrap();
    let redirect = format!("{DROPBOX_REDIRECT}?code={SIGN_IN_CODE}&state={state}");
    finish_sign_in(Service::Dropbox, &dirs, &creds, dropbox, &redirect).unwrap();
    let dropbox_account = service::token_account(Service::Dropbox, "fake@example.com").unwrap();
    assert_eq!(
        creds.get(&dropbox_account).unwrap().unwrap().expose(),
        REFRESH_TOKEN
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn forget_removes_the_config_and_the_secret() {
    let (root, dirs) = temp_dirs("forget");
    let creds = MemoryStore::new();
    let fake = Arc::new(FakeDav::new(PASSWORD));
    setup_nextcloud(
        "https://cloud.test",
        "alex",
        PASSWORD,
        "desktop",
        &dirs,
        &creds,
        fake.clone(),
    )
    .unwrap();
    assert!(open(&dirs, &creds, fake.clone()).unwrap().is_some());

    forget(&dirs, &creds).unwrap();

    assert!(open(&dirs, &creds, fake).unwrap().is_none());
    assert!(!dirs.config.join("config.json").exists());
    let account = creds::account("https://cloud.test", "alex").unwrap();
    assert!(creds.get(&account).unwrap().is_none());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn map_sync_actions() {
    let base_data = SyncData {
        game: "acnh".into(),
        action: Action::Noop,
        revision: Some(1),
        server: Some(Side {
            device: "deck".into(),
            time: "2026-11-17T12:00:00Z".into(),
            revision: Some(1),
        }),
        local: Some(Side {
            device: "pc".into(),
            time: "2026-11-17T11:00:00Z".into(),
            revision: None,
        }),
        backup: None,
        counts: Counts::default(),
        message: "Already in sync.".into(),
    };

    // Done actions
    for action in [
        Action::Noop,
        Action::Adopted,
        Action::Pulled,
        Action::Pushed,
    ] {
        let mut data = base_data.clone();
        data.action = action;
        data.message = format!("action {action:?}");
        let report = SyncReport {
            sync: data,
            held: None,
            server_card: None,
            local_card: None,
        };
        assert_eq!(map_sync(report), Sync::Done(format!("action {action:?}")));
    }

    // Offline / Deferred
    for action in [Action::Offline, Action::Deferred] {
        let mut data = base_data.clone();
        data.action = action;
        let report = SyncReport {
            sync: data,
            held: None,
            server_card: None,
            local_card: None,
        };
        assert_eq!(map_sync(report), Sync::Offline);
    }

    // Conflict
    let mut conflict_data = base_data.clone();
    conflict_data.action = Action::Conflict;
    let conflict_report = SyncReport {
        sync: conflict_data,
        held: None,
        server_card: None,
        local_card: None,
    };
    match map_sync(conflict_report) {
        Sync::Conflict { server, local, .. } => {
            assert!(server.device.contains("deck"));
            assert!(local.device.contains("pc"));
        }
        other => panic!("expected Sync::Conflict, got {other:?}"),
    }

    // Held
    let mut held_data = base_data;
    held_data.action = Action::Held;
    let held_report = SyncReport {
        sync: held_data,
        held: Some(Held {
            device: "switch".into(),
            since: "2026-11-17T10:00:00Z".into(),
        }),
        server_card: None,
        local_card: None,
    };
    match map_sync(held_report) {
        Sync::Held { device, .. } => {
            assert!(device.contains("switch"));
        }
        other => panic!("expected Sync::Conflict, got {other:?}"),
    }
}

#[test]
fn hold_requires_an_explicit_override_and_status_tracks_changes() {
    use acnh_core::launch::RelayStatus;
    let root = temp_fixture_dir("hold-status");
    let remote = Arc::new(MemoryRemote::new());
    let a = root.join("a");
    let b = root.join("b");
    let a_saves = a.join("saves");
    let b_saves = b.join("saves");
    populate_island(&a_saves, "initial");
    let ra = SaveRelay::new(remote.clone(), create_store(&a), "a").with_clock(|| 1000);
    let rb = SaveRelay::new(remote, create_store(&b), "b").with_clock(|| 1010);
    ra.push(&a_saves, None).unwrap();
    assert!(matches!(
        rb.status(&b_saves).unwrap(),
        RelayStatus::RelayNewer(_)
    ));
    rb.pull(&b_saves, None).unwrap();
    fs::write(b_saves.join("main.dat"), "changed").unwrap();
    assert_eq!(rb.status(&b_saves).unwrap(), RelayStatus::LocalNewer);
    rb.push(&b_saves, None).unwrap();
    assert!(matches!(ra.pull(&a_saves, None).unwrap(), Sync::Done(_)));
    assert!(matches!(
        rb.pull(&b_saves, Some(Keep::Local)).unwrap(),
        Sync::Held { .. }
    ));
    assert!(matches!(
        rb.pull_anyway(&b_saves, None).unwrap(),
        Sync::Done(_)
    ));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn session_card_is_written_after_push() {
    let root = temp_fixture_dir("session-card");
    let saves = root.join("saves");
    populate_island(&saves, "before");
    let time = Arc::new(AtomicUsize::new(1000));
    let clock = time.clone();
    let relay = SaveRelay::new(Arc::new(MemoryRemote::new()), create_store(&root), "device")
        .with_clock(move || clock.load(Ordering::Relaxed) as u64);
    relay.pull(&saves, None).unwrap();
    time.store(1300, Ordering::Relaxed);
    fs::write(saves.join("main.dat"), "after").unwrap();
    relay.push(&saves, None).unwrap();
    let card = acnh_core::savecard::read_card(&saves, Path::new("")).unwrap();
    assert_eq!(
        (card.device.as_str(), card.saved_unix, card.play_seconds),
        ("device", 1300, 300)
    );
    fs::write(saves.join("main.dat"), "changed after the session").unwrap();
    let changed = acnh_core::savecard::read_card(&saves, Path::new("")).unwrap();
    assert_eq!(changed.device, "");
    assert_eq!(changed.play_seconds, 0);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_refused_sign_in_is_relay_sign_in_and_never_holds_the_password() {
    let (root, dirs) = temp_dirs("sign-in-refused");
    let creds = MemoryStore::new();
    let fake = Arc::new(FakeDav::new(PASSWORD));
    setup_nextcloud(
        "https://cloud.test",
        "alex",
        PASSWORD,
        "desktop",
        &dirs,
        &creds,
        fake.clone(),
    )
    .unwrap();

    // The server now answers 401 to the stored password (a revoked app password).
    let revoked = Arc::new(FakeDav::new("another-password"));
    let relay = open(&dirs, &creds, revoked).unwrap().expect("set up");
    let saves = root.join("saves");
    populate_island(&saves, "v1");
    let error = relay.pull(&saves, None).unwrap_err();
    assert_eq!(error.code(), "relay_sign_in", "{error}");
    assert!(error.to_string().contains("sign in"), "{error}");
    assert!(!error.to_string().contains(PASSWORD), "{error}");

    // The same at setup: a wrong password is the sign-in error, not a generic one.
    let error = setup_nextcloud(
        "https://cloud.test",
        "alex",
        WRONG_PASSWORD,
        "desktop",
        &dirs,
        &MemoryStore::new(),
        fake,
    )
    .unwrap_err();
    assert_eq!(error.code(), "relay_sign_in");
    assert!(!error.to_string().contains(WRONG_PASSWORD));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn pull_anyway_answers_a_conflict_with_the_given_keep() {
    let root = temp_fixture_dir("anyway-conflict");
    let remote = Arc::new(MemoryRemote::new());
    let a = root.join("a");
    let b = root.join("b");
    populate_island(&a.join("saves"), "initial");
    let ra = SaveRelay::new(remote.clone(), create_store(&a), "a").with_clock(|| 1000);
    let rb = SaveRelay::new(remote, create_store(&b), "b").with_clock(|| 1010);
    ra.push(&a.join("saves"), None).unwrap();
    rb.pull(&b.join("saves"), None).unwrap();
    fs::write(a.join("saves/main.dat"), "from a").unwrap();
    ra.push(&a.join("saves"), None).unwrap();
    fs::write(b.join("saves/main.dat"), "from b").unwrap();

    let first = rb.pull_anyway(&b.join("saves"), None).unwrap();
    assert!(matches!(first, Sync::Conflict { .. }), "{first:?}");
    let resolved = rb
        .pull_anyway(&b.join("saves"), Some(Keep::Server))
        .unwrap();
    assert!(matches!(resolved, Sync::Resolved { .. }), "{resolved:?}");
    assert_eq!(
        fs::read_to_string(b.join("saves/main.dat")).unwrap(),
        "from a"
    );
    let _ = fs::remove_dir_all(root);
}
