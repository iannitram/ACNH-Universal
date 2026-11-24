//! The Android app's save-relay flow (issue #81) with fakes, on every runner: the loop of
//! `relay.rs` as the Android app drives it, through `BridgePrompt` (the home screen's dialogs,
//! here a fake "Java side" answering the JSON requests), `FileStore` (the app's credential file)
//! and `Roots::android`, against save-relay's in-memory remote behind a fake Dropbox sign-in. Only
//! the JNI glue and the C exports are Android-only (`relay_android.rs`, `relay_android.cpp`).
#![cfg(not(any(target_os = "ios", target_os = "android")))]

#[allow(dead_code)] // `Live` is the iPad's connector; the tests bring a fake
#[path = "../src/relay.rs"]
mod relay;

use acnh_core::launch::{Keep, Relay as _};
use acnh_core::relay::SaveRelay;
use acnh_core::{saves, Error};
use relay::{
    android_credentials, android_vars, describe_card, for_android, forget_android, header_lines,
    parse_wire, sign_in_again_message, sign_out_android, BridgePrompt, CallbackTransport,
    Connector, FileSettings, FileStore, Player, Prompt, Roots, Settings, SignInAnswer, SignInForm,
    ANDROID_DEVICE, DOWNLOAD_ACCOUNT,
};
use save_relay::creds::{CredentialStore, Secret};
use save_relay::http::{Request, Transport};
use save_relay::local::{Config, LocalStore};
use save_relay::platform::{AppDirs, PathVars};
use save_relay::remote::{MemoryRemote, Precondition, Remote, RemoteEntry};
use save_relay::service::{token_account, Service};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const EMAIL: &str = "player@example.test";
const REFRESH: &str = "refresh-token-SECRET-7731";
const SECOND: &str = "refresh-token-SECRET-9902";

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "acnh-android-relay-{}-{name}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn text_of(dir: &Path) -> String {
    let mut text = String::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in fs::read_dir(&next).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                text.push_str(&String::from_utf8_lossy(&fs::read(&path).unwrap()));
            }
        }
    }
    text
}

// ---- the credential file ---------------------------------------------------------------------

#[test]
fn the_credential_file_keeps_secrets_across_stores_and_deletes_them() {
    let dir = temp("store");
    let path = android_credentials(&dir);
    let store = FileStore::new(&path);
    assert_eq!(store.get("dropbox:a").unwrap(), None);
    store.set("dropbox:a", &Secret::new(REFRESH)).unwrap();
    store.set("pending:dropbox", &Secret::new("other")).unwrap();
    // A new store on the same file (the next start, or the Java host's calls) reads them.
    let again = FileStore::new(&path);
    assert_eq!(again.get("dropbox:a").unwrap().unwrap().expose(), REFRESH);
    again.set("dropbox:a", &Secret::new(SECOND)).unwrap();
    assert_eq!(store.get("dropbox:a").unwrap().unwrap().expose(), SECOND);
    store.delete("dropbox:a").unwrap();
    store.delete("dropbox:a").unwrap(); // a missing one is fine
    assert_eq!(store.get("dropbox:a").unwrap(), None);
    assert_eq!(
        store.get("pending:dropbox").unwrap().unwrap().expose(),
        "other"
    );
    assert_eq!(store.kind(), "android-app-file");
    // No partial file is left beside it.
    let names: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn the_credential_file_is_readable_by_the_app_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp("mode");
    let path = android_credentials(&dir);
    let store = FileStore::new(&path);
    store.set("dropbox:a", &Secret::new(REFRESH)).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // Still 0600 after a rewrite, even over a file that was opened up.
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    store.set("dropbox:b", &Secret::new(SECOND)).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_damaged_credential_file_is_an_error_and_is_left_as_it_is() {
    let dir = temp("damaged");
    let path = android_credentials(&dir);
    fs::write(&path, b"not json").unwrap();
    let store = FileStore::new(&path);
    assert!(store.get("dropbox:a").is_err());
    assert!(store.set("dropbox:a", &Secret::new(REFRESH)).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"not json");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_credential_file_never_shows_a_secret_in_debug_or_errors() {
    let dir = temp("leak");
    let path = android_credentials(&dir);
    let store = FileStore::new(&path);
    store.set("dropbox:a", &Secret::new(REFRESH)).unwrap();
    assert!(!format!("{store:?}").contains(REFRESH));
    let secret = store.get("dropbox:a").unwrap().unwrap();
    assert!(!format!("{secret:?}").contains(REFRESH));
    // An error names the file's trouble, never what is in it.
    let mut bytes = fs::read(&path).unwrap();
    bytes.push(b'{');
    fs::write(&path, &bytes).unwrap();
    let error = store.get("dropbox:a").unwrap_err().to_string();
    assert!(!error.contains(REFRESH), "{error}");
    assert!(!error.contains(&dir.display().to_string()), "{error}");
    let _ = fs::remove_dir_all(&dir);
}

// ---- the folders -----------------------------------------------------------------------------

#[test]
fn the_loop_and_save_relays_c_interface_use_the_same_folders() {
    let files = PathBuf::from("/data/user/0/app/files");
    let (config, data) = android_vars(&files);
    let vars = PathVars {
        home: files.clone(),
        data,
        config,
        documents: None,
    };
    let roots = Roots::android(Path::new("/storage/game"), &files);
    assert_eq!(roots.dirs(), AppDirs::from_vars(&vars));
    assert_eq!(roots.library, files);
    assert_eq!(roots.data_root, PathBuf::from("/storage/game"));
    // save-relay's folders and the credential file are in the private files folder, never in the
    // game folder (which may be on shared storage).
    assert!(roots.dirs().config.starts_with(&files));
    assert!(roots.dirs().data.starts_with(&files));
    assert!(android_credentials(&files).starts_with(&files));
    assert_eq!(save_relay::layout::device_slug(ANDROID_DEVICE), "android");
}

// ---- the network wire ------------------------------------------------------------------------

fn wire(status: u32, headers: &str, body: &[u8]) -> Vec<u8> {
    let mut out = status.to_be_bytes().to_vec();
    out.extend((headers.len() as u32).to_be_bytes());
    out.extend(headers.as_bytes());
    out.extend(body);
    out
}

#[test]
fn the_java_hosts_answers_read_back_as_responses() {
    let response = parse_wire(&wire(
        207,
        "Content-Type: text/xml\nETag: \"x\"\n",
        b"<body/>",
    ))
    .unwrap();
    assert_eq!(response.status, 207);
    assert_eq!(response.header_value("content-type"), Some("text/xml"));
    assert_eq!(response.header_value("ETag"), Some("\"x\""));
    assert_eq!(response.body, b"<body/>");
    let empty = parse_wire(&wire(204, "", b"")).unwrap();
    assert_eq!((empty.status, empty.body.len()), (204, 0));
    for bad in [vec![0u8; 7], wire(70_000, "", b""), {
        let mut w = wire(200, "A: b\n", b"");
        w.truncate(10);
        w
    }] {
        assert!(matches!(
            parse_wire(&bad),
            Err(save_relay::Error::Unreachable(_))
        ));
    }
}

#[test]
fn the_transport_sends_header_lines_and_bounds_the_answer() {
    let seen = Mutex::new(String::new());
    let transport = CallbackTransport(|request: &Request, headers: &str| {
        *seen.lock().unwrap() = format!("{} {} {headers}", request.method, request.url);
        Some(wire(200, "", b"0123456789"))
    });
    let request = Request::new("GET", "https://api.example.test/x")
        .header("Authorization", "Bearer abc")
        .header("Accept", "*/*");
    assert_eq!(
        header_lines(&request),
        "Authorization: Bearer abc\nAccept: */*\n"
    );
    assert_eq!(transport.send(&request).unwrap().body, b"0123456789");
    assert_eq!(
        *seen.lock().unwrap(),
        "GET https://api.example.test/x Authorization: Bearer abc\nAccept: */*\nAccept-Encoding: identity\n"
    );
    let mut bounded = request.clone();
    bounded.max_response_bytes = Some(4);
    assert!(matches!(
        transport.send(&bounded),
        Err(save_relay::Error::Unreachable(_))
    ));
    let down = CallbackTransport(|_: &Request, _: &str| None);
    assert!(matches!(
        down.send(&request),
        Err(save_relay::Error::Unreachable(_))
    ));
}

// ---- the prompt bridge -----------------------------------------------------------------------

/// The home screen's side of the bridge: records each request and answers from a script.
struct FakeJava {
    requests: Mutex<Vec<Value>>,
    answers: Mutex<Vec<Option<&'static str>>>,
    /// Run when a sign-in is answered `signed_in` (the Java side's save-relay sign-in).
    on_sign_in: Box<dyn Fn() + Sync>,
}

impl FakeJava {
    fn new(answers: &[Option<&'static str>]) -> FakeJava {
        FakeJava {
            requests: Mutex::new(Vec::new()),
            answers: Mutex::new(answers.iter().rev().copied().collect()),
            on_sign_in: Box::new(|| {}),
        }
    }

    fn ask(&self, request: &str) -> Option<String> {
        let parsed: Value = serde_json::from_str(request).expect("the request is JSON");
        self.requests.lock().unwrap().push(parsed);
        let answer = self
            .answers
            .lock()
            .unwrap()
            .pop()
            .expect("a request the test did not expect")?;
        if answer == "signed_in" {
            (self.on_sign_in)();
        }
        Some(serde_json::json!({ "answer": answer }).to_string())
    }

    fn kinds(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r["kind"].as_str().unwrap().to_string())
            .collect()
    }

    fn all(&self) -> String {
        format!("{:?}", self.requests.lock().unwrap())
    }
}

fn bridge(java: &FakeJava) -> BridgePrompt<'_> {
    let mut prompt = BridgePrompt::new(|request| java.ask(request));
    prompt.pause = Duration::ZERO;
    prompt
}

fn card(device: &str, saved: u64, play: u64) -> acnh_core::savecard::SaveCard {
    acnh_core::savecard::SaveCard {
        saved_unix: saved,
        device: device.into(),
        play_seconds: play,
        version: String::new(),
        picture: PathBuf::new(),
    }
}

#[test]
fn each_screen_goes_to_the_home_screen_as_its_json_request() {
    let java = FakeJava::new(&[
        Some("signed_in"),
        Some("server"),
        Some("play_anyway"),
        Some("sign_in_again"),
    ]);
    let prompt = bridge(&java);
    let form = SignInForm {
        server: "dropbox".into(),
        user: EMAIL.into(),
        allow_not_now: false,
        message: Some("Dropbox refused".into()),
    };
    assert_eq!(prompt.sign_in(&form), SignInAnswer::SignedIn);
    let server = card("switch", 1_763_450_100, 7_200);
    let local = card("android", 1_763_450_000, 60);
    assert_eq!(prompt.conflict(&server, &local), Keep::Server);
    assert_eq!(
        prompt.held("switch", "2026-11-18T07:15:00Z"),
        relay::HeldAnswer::PlayAnyway
    );
    assert_eq!(
        prompt.blocked(&relay::Blocked::SignInCancelled),
        relay::BlockedAnswer::SignInAgain
    );
    let requests = java.requests.lock().unwrap().clone();
    assert_eq!(
        requests[0],
        serde_json::json!({"kind": "sign_in", "again": true, "allow_not_now": false,
                           "message": "Dropbox refused"})
    );
    assert_eq!(
        requests[1],
        serde_json::json!({"kind": "conflict", "server": describe_card(&server),
                           "local": describe_card(&local)})
    );
    assert_eq!(
        requests[2],
        serde_json::json!({"kind": "held", "device": "switch", "since": "2026-11-18T07:15:00Z"})
    );
    assert_eq!(
        requests[3],
        serde_json::json!({"kind": "blocked", "why": "sign_in_cancelled", "message": null,
                           "can_turn_off": true})
    );
    // The account's user name never goes to the screens.
    assert!(!java.all().contains(EMAIL));
}

#[test]
fn an_answer_the_screen_could_not_give_decides_nothing_for_the_person() {
    // A conflict is asked again until a side is picked; nothing else plays the game.
    let java = FakeJava::new(&[None, Some("garbage"), Some("local")]);
    let prompt = bridge(&java);
    assert_eq!(
        prompt.conflict(&card("a", 1, 0), &card("b", 2, 0)),
        Keep::Local
    );
    assert_eq!(java.kinds(), ["conflict", "conflict", "conflict"]);

    let java = FakeJava::new(&[None, None, Some("not_now"), None, Some("sign_in_again")]);
    let prompt = bridge(&java);
    let first = SignInForm {
        server: "https://".into(),
        user: String::new(),
        allow_not_now: true,
        message: None,
    };
    assert_eq!(prompt.sign_in(&first), SignInAnswer::Cancel);
    assert_eq!(prompt.held("switch", "now"), relay::HeldAnswer::CheckAgain);
    // Not now only where the form offers it.
    let again = SignInForm {
        allow_not_now: false,
        ..first.clone()
    };
    assert_eq!(prompt.sign_in(&again), SignInAnswer::Cancel);
    assert_eq!(
        prompt.blocked(&relay::Blocked::Offline),
        relay::BlockedAnswer::Retry
    );
    // Sign in again is an answer to a cancelled sign-in only.
    assert_eq!(
        prompt.blocked(&relay::Blocked::Error("x".into())),
        relay::BlockedAnswer::Retry
    );
}

#[test]
fn the_sign_in_again_text_asks_for_the_services_own_sign_in() {
    assert_eq!(
        sign_in_again_message("dropbox"),
        "Dropbox refused this device's sign-in. Sign in again to play."
    );
    assert!(sign_in_again_message("https://cloud.test").contains("app password"));
    assert!(sign_in_again_message("").contains("app password"));
}

// ---- the loop, as the Android app drives it --------------------------------------------------

/// The in-memory remote behind a token check, as Dropbox: a refresh token Dropbox no longer
/// accepts answers `AuthFailed` (a 401 on the wire).
struct Gate {
    inner: Arc<MemoryRemote>,
    token_ok: bool,
    offline: Arc<AtomicBool>,
    fail_mark_puts: Arc<AtomicUsize>,
}

impl Gate {
    fn check(&self) -> save_relay::Result<()> {
        if self.offline.load(Ordering::SeqCst) {
            return Err(save_relay::Error::Unreachable("no network".into()));
        }
        if !self.token_ok {
            return Err(save_relay::Error::AuthFailed);
        }
        Ok(())
    }
}

impl Remote for Gate {
    fn list(&self, dir: &str) -> save_relay::Result<Vec<RemoteEntry>> {
        self.check()?;
        self.inner.list(dir)
    }
    fn get(&self, path: &str) -> save_relay::Result<(Vec<u8>, String)> {
        self.check()?;
        self.inner.get(path)
    }
    fn put(&self, path: &str, bytes: &[u8], pre: &Precondition) -> save_relay::Result<String> {
        let failing = self.fail_mark_puts.load(Ordering::SeqCst);
        if path.ends_with("playing.json")
            && failing > 0
            && self
                .fail_mark_puts
                .compare_exchange(failing, failing - 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            return Err(save_relay::Error::Unreachable(
                "the connection dropped".into(),
            ));
        }
        self.check()?;
        self.inner.put(path, bytes, pre)
    }
    fn mkcol(&self, dir: &str) -> save_relay::Result<()> {
        self.check()?;
        self.inner.mkcol(dir)
    }
    fn copy(&self, from: &str, to: &str) -> save_relay::Result<()> {
        self.check()?;
        self.inner.copy(from, to)
    }
    fn move_to(&self, from: &str, to: &str) -> save_relay::Result<()> {
        self.check()?;
        self.inner.move_to(from, to)
    }
    fn delete(&self, path: &str) -> save_relay::Result<()> {
        self.check()?;
        self.inner.delete(path)
    }
}

/// Dropbox as save-relay's `open` sees it: the config names `dropbox`, the refresh token is in
/// the credential store under `dropbox:<account>`.
struct FakeDropbox {
    remote: Arc<MemoryRemote>,
    accepted: Mutex<String>,
    offline: Arc<AtomicBool>,
    /// The next puts of `playing.json` fail as a dropped connection.
    fail_mark_puts: Arc<AtomicUsize>,
}

impl Connector for FakeDropbox {
    fn open(
        &self,
        dirs: &AppDirs,
        creds: &dyn CredentialStore,
    ) -> acnh_core::Result<Option<SaveRelay>> {
        let store = LocalStore::new(dirs.clone());
        let Some(config) = store
            .load_config()
            .map_err(|e| Error::Saves(e.to_string()))?
        else {
            return Ok(None);
        };
        let account = token_account(Service::Dropbox, &config.user).unwrap();
        let Some(token) = creds
            .get(&account)
            .map_err(|e| Error::Saves(e.to_string()))?
        else {
            return Ok(None);
        };
        let gate = Gate {
            inner: self.remote.clone(),
            token_ok: token.expose() == *self.accepted.lock().unwrap(),
            offline: self.offline.clone(),
            fail_mark_puts: self.fail_mark_puts.clone(),
        };
        Ok(Some(
            SaveRelay::new(Arc::new(gate), store, config.device).with_root(config.root),
        ))
    }
}

/// What save-relay's `save_relay_sign_in_finish` leaves behind: the config and the refresh token.
fn sign_in(dirs: &AppDirs, creds: &dyn CredentialStore, token: &str) {
    creds
        .set(
            &token_account(Service::Dropbox, EMAIL).unwrap(),
            &Secret::new(token),
        )
        .unwrap();
    LocalStore::new(dirs.clone())
        .save_config(&Config {
            format: save_relay::local::FORMAT,
            server: "dropbox".into(),
            user: EMAIL.into(),
            root: save_relay::cli::DEFAULT_ROOT.to_string(),
            device: save_relay::layout::device_slug(ANDROID_DEVICE),
        })
        .unwrap();
}

struct Fx {
    root: PathBuf,
    roots: Roots,
    creds: FileStore,
    connector: FakeDropbox,
    booted: AtomicBool,
    pulled: AtomicBool,
    settings: FileSettings,
    log: Mutex<Vec<String>>,
}

impl Fx {
    fn new(name: &str) -> Fx {
        let root = temp(name);
        // The game folder (shared storage) and the app's private files folder.
        let game = root.join("storage/ACNH");
        let files = root.join("data/files");
        fs::create_dir_all(&game).unwrap();
        fs::create_dir_all(&files).unwrap();
        Fx {
            roots: Roots::android(&game, &files),
            creds: FileStore::new(android_credentials(&files)),
            connector: FakeDropbox {
                remote: Arc::new(MemoryRemote::new()),
                accepted: Mutex::new(REFRESH.into()),
                offline: Arc::new(AtomicBool::new(false)),
                fail_mark_puts: Arc::new(AtomicUsize::new(0)),
            },
            booted: AtomicBool::new(false),
            pulled: AtomicBool::new(false),
            settings: FileSettings(game.join("config.ini")),
            log: Mutex::new(Vec::new()),
            root,
        }
    }

    fn player<'a>(&'a self, prompt: &'a dyn Prompt) -> Player<'a> {
        Player {
            roots: self.roots.clone(),
            creds: &self.creds,
            connector: &self.connector,
            prompt,
            booted: &self.booted,
            pulled: &self.pulled,
            settings: &self.settings,
            log: Box::new(|line| self.log.lock().unwrap().push(line.to_string())),
        }
    }

    fn saves(&self) -> PathBuf {
        saves::save_dir(&self.roots.data_root)
    }

    /// The Java side: a `signed_in` answer runs save-relay's sign-in with `token`.
    fn java(&self, answers: &[Option<&'static str>], token: &'static str) -> FakeJava {
        let mut java = FakeJava::new(answers);
        let dirs = self.roots.dirs();
        let path = self.creds.path().to_path_buf();
        java.on_sign_in = Box::new(move || sign_in(&dirs, &FileStore::new(&path), token));
        java
    }

    /// Another device's island on the server.
    fn other_pushes(&self, marker: &str) {
        let dir = self.root.join("switch");
        let store = LocalStore::new(AppDirs {
            config: dir.join("config"),
            data: dir.join("data"),
        });
        let other = SaveRelay::new(self.connector.remote.clone(), store, "switch");
        island(&dir.join("saves"), marker);
        other.push(&dir.join("saves"), None).unwrap();
    }

    fn log_text(&self) -> String {
        self.log.lock().unwrap().join("\n")
    }

    /// Nothing outside the credential file holds a token or the account.
    fn assert_no_leaks(&self) {
        let creds = fs::read_to_string(self.creds.path()).unwrap_or_default();
        let mut all = text_of(&self.root);
        all = all.replace(&creds, "");
        for secret in [REFRESH, SECOND] {
            assert!(!all.contains(secret), "a token outside the credential file");
            assert!(!self.log_text().contains(secret), "a token in the log");
        }
        assert!(!self.log_text().contains(EMAIL), "the account in the log");
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn island(dir: &Path, marker: &str) {
    fs::create_dir_all(dir.join("Villager0")).unwrap();
    fs::write(dir.join("main.dat"), format!("main {marker}")).unwrap();
    fs::write(dir.join("mainHeader.dat"), format!("header {marker}")).unwrap();
    fs::write(
        dir.join("Villager0/personal.dat"),
        format!("villager {marker}"),
    )
    .unwrap();
}

fn read_main(dir: &Path) -> String {
    fs::read_to_string(dir.join("main.dat")).unwrap()
}

#[test]
fn the_first_start_signs_in_to_dropbox_pulls_and_boots_then_the_quit_pushes() {
    let fx = Fx::new("first");
    fx.other_pushes("from the switch");
    let java = fx.java(&[Some("signed_in")], REFRESH);
    let prompt = bridge(&java);
    let player = fx.player(&prompt);
    player.before_game();
    assert!(fx.booted.load(Ordering::SeqCst));
    assert_eq!(java.kinds(), ["sign_in"]);
    let first = &java.requests.lock().unwrap()[0];
    assert_eq!(first["allow_not_now"], true);
    assert_eq!(first["again"], false);
    assert_eq!(read_main(&fx.saves()), "main from the switch");

    // Played on the phone; the quit pushes it as "android".
    island(&fx.saves(), "from the phone");
    player.after_game();
    assert!(fx.log_text().contains("relay: pushed the island"));
    let dir = fx.root.join("switch2");
    let other = SaveRelay::new(
        fx.connector.remote.clone(),
        LocalStore::new(AppDirs {
            config: dir.join("config"),
            data: dir.join("data"),
        }),
        "switch",
    );
    other.pull(&dir.join("saves"), None).unwrap();
    assert_eq!(read_main(&dir.join("saves")), "main from the phone");
    fx.assert_no_leaks();
}

#[test]
fn not_now_boots_with_sync_off_and_is_never_asked_again() {
    let fx = Fx::new("not-now");
    let java = fx.java(&[Some("not_now")], REFRESH);
    let prompt = bridge(&java);
    fx.player(&prompt).before_game();
    assert!(fx.booted.load(Ordering::SeqCst));
    assert!(fx.settings.ask_off());
    let ini = fs::read_to_string(fx.roots.data_root.join("config.ini")).unwrap();
    assert!(ini.contains("relay.ask = 0"), "{ini}");
    // The next start asks nothing.
    let none = FakeJava::new(&[]);
    let prompt = bridge(&none);
    fx.booted.store(false, Ordering::SeqCst);
    fx.player(&prompt).before_game();
    assert!(fx.booted.load(Ordering::SeqCst));
    assert!(none.kinds().is_empty());
}

#[test]
fn a_refused_dropbox_token_asks_for_the_browser_sign_in_again_and_boots_after_it() {
    let fx = Fx::new("refused");
    sign_in(&fx.roots.dirs(), &fx.creds, REFRESH);
    // Dropbox stops accepting the old token; the new sign-in brings one it accepts.
    *fx.connector.accepted.lock().unwrap() = SECOND.into();
    let java = fx.java(&[Some("signed_in")], SECOND);
    let prompt = bridge(&java);
    fx.player(&prompt).before_game();
    assert!(fx.booted.load(Ordering::SeqCst));
    let requests = java.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["kind"], "sign_in");
    assert_eq!(requests[0]["again"], true);
    assert_eq!(requests[0]["allow_not_now"], false);
    assert_eq!(
        requests[0]["message"],
        "Dropbox refused this device's sign-in. Sign in again to play."
    );
    fx.assert_no_leaks();
}

#[test]
fn a_read_only_install_without_save_relays_setup_is_offered_the_sign_in() {
    // An install signed in for the game download only (the old read-only scopes): its token is
    // the download's, not save-relay's, so save sync is not set up and the offer comes.
    let fx = Fx::new("read-only");
    let java = fx.java(&[Some("cancel")], REFRESH);
    let prompt = bridge(&java);
    fx.player(&prompt).before_game();
    assert_eq!(java.kinds(), ["sign_in"]);
    assert!(fx.booted.load(Ordering::SeqCst));
    // Cancel is not Not now: the next start offers it again.
    assert!(!fx.settings.ask_off());
}

#[test]
fn offline_refuses_the_game_until_retry_reaches_dropbox() {
    let fx = Fx::new("offline");
    sign_in(&fx.roots.dirs(), &fx.creds, REFRESH);
    fx.connector.offline.store(true, Ordering::SeqCst);
    let offline = fx.connector.offline.clone();
    let answered = AtomicBool::new(false);
    let java = FakeJava::new(&[Some("retry")]);
    let prompt = BridgePrompt::new(|request| {
        let answer = java.ask(request);
        // Back online before the retry.
        offline.store(false, Ordering::SeqCst);
        answered.store(true, Ordering::SeqCst);
        answer
    });
    fx.player(&prompt).before_game();
    assert!(answered.load(Ordering::SeqCst));
    let requests = java.requests.lock().unwrap().clone();
    assert_eq!(
        requests[0],
        serde_json::json!({"kind": "blocked", "why": "offline", "message": null,
                           "can_turn_off": false})
    );
    assert!(fx.booted.load(Ordering::SeqCst));
}

#[test]
fn a_push_before_the_game_booted_does_nothing_and_a_failed_push_waits() {
    let fx = Fx::new("push");
    sign_in(&fx.roots.dirs(), &fx.creds, REFRESH);
    island(&fx.saves(), "phone");
    let none = FakeJava::new(&[]);
    let prompt = bridge(&none);
    let player = fx.player(&prompt);
    player.after_game();
    assert!(fx.log_text().contains("the game has not booted"));
    player.before_game();
    fx.connector.offline.store(true, Ordering::SeqCst);
    island(&fx.saves(), "phone, played");
    player.after_game();
    assert!(fx.roots.dirs().data.join("push-waiting").exists());
    // The next start sends it first.
    fx.connector.offline.store(false, Ordering::SeqCst);
    fx.booted.store(false, Ordering::SeqCst);
    player.before_game();
    assert!(!fx.roots.dirs().data.join("push-waiting").exists());
    assert!(fx.log_text().contains("sent the push that was waiting"));
    assert!(none.kinds().is_empty());
    fx.assert_no_leaks();
}

// ---- the review's fixes: the outgoing headers, the background push, the ways out ---------------

#[test]
fn a_post_without_a_content_type_goes_out_as_octet_stream_for_dropbox() {
    // save-relay's Dropbox download: POST, empty body, arguments in a header. Android would add
    // `application/x-www-form-urlencoded`, which Dropbox refuses with 400.
    let download = Request::new("POST", "https://content.dropboxapi.com/2/files/download")
        .header("Dropbox-API-Arg", "{\"path\":\"/x\"}");
    let sent = for_android(&download);
    assert_eq!(
        sent.header_value("Content-Type"),
        Some("application/octet-stream")
    );
    assert_eq!(sent.header_value("Accept-Encoding"), Some("identity"));
    // A Content-Type already set (the token form, an upload) is kept; a GET gets none.
    let form = Request::new("POST", "https://api.dropboxapi.com/oauth2/token")
        .header("Content-Type", "application/x-www-form-urlencoded");
    assert_eq!(
        for_android(&form).header_value("Content-Type"),
        Some("application/x-www-form-urlencoded")
    );
    let get = Request::new("GET", "https://example.test/");
    assert_eq!(for_android(&get).header_value("Content-Type"), None);
    // And that is what the Java host is handed.
    let seen = Mutex::new(String::new());
    let transport = CallbackTransport(|_: &Request, headers: &str| {
        *seen.lock().unwrap() = headers.to_string();
        Some(wire(200, "", b""))
    });
    transport.send(&download).unwrap();
    let headers = seen.lock().unwrap().clone();
    assert!(
        headers.contains("Content-Type: application/octet-stream\n"),
        "{headers}"
    );
    assert!(headers.contains("Accept-Encoding: identity\n"), "{headers}");
}

impl Fx {
    fn mark(&self) -> Option<save_relay::presence::Mark> {
        let manifest = save_relay::manifest::Manifest::from_json(&saves::relay_manifest()).unwrap();
        let layout = save_relay::layout::Layout::new(save_relay::cli::DEFAULT_ROOT).unwrap();
        let (bytes, _) = self
            .connector
            .remote
            .get(&layout.playing_path(&manifest.id))
            .ok()?;
        save_relay::presence::Mark::from_json(&String::from_utf8_lossy(&bytes)).ok()
    }

    fn session_start(&self) -> Option<u64> {
        let manifest = save_relay::manifest::Manifest::from_json(&saves::relay_manifest()).unwrap();
        LocalStore::new(self.roots.dirs())
            .load_session(&manifest.id)
            .unwrap()
            .map(|s| s.started)
    }
}

#[test]
fn the_background_push_keeps_this_device_playing_and_the_quit_ends_it() {
    let fx = Fx::new("checkpoint");
    sign_in(&fx.roots.dirs(), &fx.creds, REFRESH);
    island(&fx.saves(), "phone");
    let none = FakeJava::new(&[]);
    let prompt = bridge(&none);
    let player = fx.player(&prompt);
    player.before_game();
    let mark = fx.mark().expect("the pull marks this device as playing");
    assert_eq!(mark.device, "android");
    let started = fx.session_start().expect("the pull starts the session");

    // The app goes to the background: the island goes up, the game goes on.
    island(&fx.saves(), "phone, played a bit");
    player.checkpoint();
    assert!(fx
        .log_text()
        .contains("pushed the island (the game goes on)"));
    let kept = fx
        .mark()
        .expect("still marked as playing after the background push");
    assert_eq!(kept.device, "android");
    assert_eq!(fx.session_start(), Some(started), "the session goes on");
    // Another device sees the island held.
    let dir = fx.root.join("switch3");
    let other = SaveRelay::new(
        fx.connector.remote.clone(),
        LocalStore::new(AppDirs {
            config: dir.join("config"),
            data: dir.join("data"),
        }),
        "switch",
    );
    assert!(matches!(
        other.pull(&dir.join("saves"), None).unwrap(),
        acnh_core::launch::Sync::Held { .. }
    ));

    // The real quit: pushed, the mark and the session end.
    island(&fx.saves(), "phone, played more");
    player.after_game();
    assert!(fx.mark().is_none());
    assert_eq!(fx.session_start(), None);
    other.pull(&dir.join("saves"), None).unwrap();
    assert_eq!(read_main(&dir.join("saves")), "main phone, played more");
    assert!(none.kinds().is_empty());
}

#[test]
fn a_lost_sign_in_can_be_turned_off_on_the_blocked_screen_and_the_game_boots() {
    let fx = Fx::new("turn-off");
    sign_in(&fx.roots.dirs(), &fx.creds, REFRESH);
    // Dropbox no longer takes the token; the person cancels the new sign-in, then turns sync off.
    *fx.connector.accepted.lock().unwrap() = SECOND.into();
    let java = FakeJava::new(&[Some("cancel"), Some("turn_off")]);
    let prompt = bridge(&java);
    fx.player(&prompt).before_game();
    assert!(fx.booted.load(Ordering::SeqCst));
    assert_eq!(java.kinds(), ["sign_in", "blocked"]);
    assert_eq!(java.requests.lock().unwrap()[1]["can_turn_off"], true);
    assert!(!fx.roots.dirs().config.join("config.json").exists());
    assert!(
        fx.settings.ask_off(),
        "the offer does not come straight back"
    );
    assert!(fx
        .log_text()
        .contains("save sync turned off on this device"));
}

#[test]
fn turning_sync_off_keeps_the_dropbox_sign_in_for_the_download_and_sign_out_ends_both() {
    let fx = Fx::new("keep-download");
    let dirs = fx.roots.dirs();
    sign_in(&dirs, &fx.creds, REFRESH);
    forget_android(&dirs, &fx.creds).unwrap();
    assert!(!dirs.config.join("config.json").exists());
    let account = token_account(Service::Dropbox, EMAIL).unwrap();
    assert_eq!(
        fx.creds.get(&account).unwrap(),
        None,
        "save-relay's account is gone"
    );
    assert_eq!(
        fx.creds.get(DOWNLOAD_ACCOUNT).unwrap().unwrap().expose(),
        REFRESH,
        "the download keeps the same sign-in"
    );
    sign_out_android(&dirs, &fx.creds).unwrap();
    assert_eq!(fx.creds.get(DOWNLOAD_ACCOUNT).unwrap(), None);
    fx.assert_no_leaks();
}

#[test]
fn turning_off_with_a_damaged_credential_file_still_turns_sync_off() {
    let fx = Fx::new("damaged-off");
    let dirs = fx.roots.dirs();
    sign_in(&dirs, &fx.creds, REFRESH);
    fs::write(fx.creds.path(), b"not json").unwrap();
    forget_android(&dirs, &fx.creds).unwrap();
    assert!(!dirs.config.join("config.json").exists());
    assert!(!fx.creds.path().exists(), "the unreadable file is cleared");
    // Sign-out over a damaged file too.
    sign_in(&dirs, &fx.creds, REFRESH);
    fs::write(fx.creds.path(), b"{").unwrap();
    sign_out_android(&dirs, &fx.creds).unwrap();
    assert!(!fx.creds.path().exists());
    assert!(!dirs.config.join("config.json").exists());
}

#[test]
fn offline_offers_no_turn_off_and_the_blocked_answer_maps() {
    let java = FakeJava::new(&[Some("turn_off"), Some("turn_off"), Some("turn_off")]);
    let prompt = bridge(&java);
    assert_eq!(
        prompt.blocked(&relay::Blocked::Offline),
        relay::BlockedAnswer::Retry
    );
    assert_eq!(
        prompt.blocked(&relay::Blocked::Error("x".into())),
        relay::BlockedAnswer::TurnOff
    );
    assert_eq!(
        prompt.blocked(&relay::Blocked::SignInCancelled),
        relay::BlockedAnswer::TurnOff
    );
    let requests = java.requests.lock().unwrap().clone();
    assert_eq!(requests[0]["can_turn_off"], false);
    assert_eq!(requests[1]["can_turn_off"], true);
}

// ---- the second review: the mark, a failed re-mark, waiting pushes, broken settings --------------

impl Fx {
    /// Another device on the same server, by name, with its own saves folder.
    fn device(&self, name: &str) -> (SaveRelay, PathBuf) {
        let dir = self.root.join(name);
        let relay = SaveRelay::new(
            self.connector.remote.clone(),
            LocalStore::new(AppDirs {
                config: dir.join("config"),
                data: dir.join("data"),
            }),
            name,
        );
        (relay, dir.join("saves"))
    }
}

#[test]
fn a_checkpoint_never_overwrites_another_devices_mark_and_says_it_was_taken_over() {
    let fx = Fx::new("takeover");
    sign_in(&fx.roots.dirs(), &fx.creds, REFRESH);
    island(&fx.saves(), "phone");
    let none = FakeJava::new(&[]);
    let prompt = bridge(&none);
    let player = fx.player(&prompt);
    player.before_game();
    player.checkpoint();
    assert_eq!(fx.mark().unwrap().device, "android");
    // The Switch takes the island over with Play anyway while the phone's game is paused.
    let (switch, dir) = fx.device("switch");
    assert!(matches!(
        switch.pull(&dir, None).unwrap(),
        acnh_core::launch::Sync::Held { .. }
    ));
    switch.pull_anyway(&dir, None).unwrap();
    assert_eq!(fx.mark().unwrap().device, "switch");
    // The phone comes back and goes to the background again: the Switch's mark stays.
    player.checkpoint();
    assert_eq!(fx.mark().unwrap().device, "switch");
    assert!(
        fx.log_text().contains("another device (switch"),
        "{}",
        fx.log_text()
    );
}

#[test]
fn a_mark_that_cannot_be_written_back_still_reports_the_push_and_is_retried_on_resume() {
    let fx = Fx::new("remark-failed");
    sign_in(&fx.roots.dirs(), &fx.creds, REFRESH);
    island(&fx.saves(), "phone");
    let none = FakeJava::new(&[]);
    let prompt = bridge(&none);
    let player = fx.player(&prompt);
    player.before_game();
    let started = fx.session_start().unwrap();
    island(&fx.saves(), "phone, played");
    // Both tries at the mark fail after the push went.
    fx.connector.fail_mark_puts.store(2, Ordering::SeqCst);
    player.checkpoint();
    let log = fx.log_text();
    assert!(
        log.contains("could not be marked as playing again"),
        "{log}"
    );
    assert!(!log.contains("the push is waiting"), "{log}");
    assert!(!fx.roots.dirs().data.join("push-waiting").exists());
    assert!(fx.roots.dirs().data.join("mark-waiting").exists());
    assert_eq!(fx.session_start(), Some(started), "the session is kept");
    assert!(fx.mark().is_none());
    // The push did go: another device gets the island.
    let (switch, dir) = fx.device("switch");
    switch.pull_anyway(&dir, None).unwrap();
    assert_eq!(read_main(&dir), "main phone, played");
    // Another device's mark is now there: the retry leaves it alone.
    player.resume();
    assert_eq!(fx.mark().unwrap().device, "switch");
    assert!(!fx.roots.dirs().data.join("mark-waiting").exists());
}

#[test]
fn a_failed_mark_is_written_back_when_the_app_is_back() {
    let fx = Fx::new("remark-resume");
    sign_in(&fx.roots.dirs(), &fx.creds, REFRESH);
    island(&fx.saves(), "phone");
    let none = FakeJava::new(&[]);
    let prompt = bridge(&none);
    let player = fx.player(&prompt);
    player.before_game();
    // One try fails, the second goes: no retry needed.
    fx.connector.fail_mark_puts.store(1, Ordering::SeqCst);
    player.checkpoint();
    assert_eq!(fx.mark().unwrap().device, "android");
    // Both fail; the app comes back to the front and writes it.
    fx.connector.fail_mark_puts.store(2, Ordering::SeqCst);
    player.checkpoint();
    assert!(fx.mark().is_none());
    player.resume();
    assert_eq!(fx.mark().unwrap().device, "android");
    assert!(!fx.roots.dirs().data.join("mark-waiting").exists());
}

#[test]
fn a_checkpoint_with_nothing_new_keeps_the_mark_and_one_against_a_newer_server_touches_nothing() {
    let fx = Fx::new("noop-newer");
    sign_in(&fx.roots.dirs(), &fx.creds, REFRESH);
    island(&fx.saves(), "phone");
    let none = FakeJava::new(&[]);
    let prompt = bridge(&none);
    let player = fx.player(&prompt);
    player.before_game();
    let started = fx.session_start().unwrap();
    // Nothing changed: a no-op push, the mark and the session stay.
    player.checkpoint();
    assert_eq!(fx.mark().unwrap().device, "android");
    assert_eq!(fx.session_start(), Some(started));
    // Another device pushes a newer island meanwhile, and the phone changes its own: a conflict,
    // nothing is pushed, the phone stays marked, the push waits for the next start.
    let (switch, dir) = fx.device("switch");
    switch.pull_anyway(&dir, None).unwrap();
    island(&dir, "switch, newer");
    switch.push(&dir, None).unwrap();
    island(&fx.saves(), "phone, also changed");
    player.checkpoint();
    assert!(fx.log_text().contains("the push is waiting"));
    assert_eq!(fx.session_start(), Some(started));
    let (other, odir) = fx.device("pc");
    other.pull_anyway(&odir, None).unwrap();
    assert_eq!(
        read_main(&odir),
        "main switch, newer",
        "the newer island is not overwritten"
    );
}

#[test]
fn turning_off_or_signing_out_drops_a_waiting_push_so_the_next_sign_in_pulls_first() {
    let fx = Fx::new("waiting-dropped");
    let dirs = fx.roots.dirs();
    sign_in(&dirs, &fx.creds, REFRESH);
    fs::create_dir_all(&dirs.data).unwrap();
    fs::write(relay::push_waiting_file(&dirs), b"1").unwrap();
    fs::write(dirs.data.join("mark-waiting"), b"1").unwrap();
    forget_android(&dirs, &fx.creds).unwrap();
    assert!(!relay::push_waiting_file(&dirs).exists());
    assert!(!dirs.data.join("mark-waiting").exists());
    sign_in(&dirs, &fx.creds, REFRESH);
    fs::write(relay::push_waiting_file(&dirs), b"1").unwrap();
    sign_out_android(&dirs, &fx.creds).unwrap();
    assert!(!relay::push_waiting_file(&dirs).exists());
}

#[test]
fn the_first_sync_after_a_sign_in_is_a_pull_whatever_push_was_left_waiting() {
    let fx = Fx::new("pull-first");
    // The server has a newer island; this phone has an old one and a push left from an older setup.
    let (switch, dir) = fx.device("switch");
    island(&dir, "on the server");
    switch.push(&dir, None).unwrap();
    island(&fx.saves(), "old phone");
    let dirs = fx.roots.dirs();
    fs::create_dir_all(&dirs.data).unwrap();
    fs::write(relay::push_waiting_file(&dirs), b"1").unwrap();
    // The pull finds both islands changed: the conflict screen, never a push over the server's.
    let java = fx.java(&[Some("signed_in"), Some("server")], REFRESH);
    let prompt = bridge(&java);
    fx.player(&prompt).before_game();
    let log = fx.log_text();
    assert!(!log.contains("sent the push that was waiting"), "{log}");
    assert_eq!(java.kinds(), ["sign_in", "conflict"]);
    assert_eq!(read_main(&fx.saves()), "main on the server");
}

#[test]
fn settings_that_do_not_read_offer_turn_off_and_keep_the_download_token() {
    let fx = Fx::new("broken-config");
    let dirs = fx.roots.dirs();
    sign_in(&dirs, &fx.creds, REFRESH);
    fx.creds
        .set(DOWNLOAD_ACCOUNT, &Secret::new(SECOND))
        .unwrap();
    fs::write(dirs.config.join("config.json"), b"{ not json").unwrap();
    // The start shows the error with Turn off (never "sync off" by itself), and Turn off boots.
    let java = FakeJava::new(&[Some("turn_off")]);
    let prompt = bridge(&java);
    fx.player(&prompt).before_game();
    let requests = java.requests.lock().unwrap().clone();
    assert_eq!(requests[0]["kind"], "blocked");
    assert_eq!(requests[0]["why"], "error");
    assert_eq!(requests[0]["can_turn_off"], true);
    assert!(fx.booted.load(Ordering::SeqCst));
    assert!(!dirs.config.join("config.json").exists());
    // The Android turn-off over such settings (forget_android): the relay's token goes (the config
    // no longer names it), the download's stays.
    sign_in(&dirs, &fx.creds, REFRESH);
    fs::write(dirs.config.join("config.json"), b"{ not json").unwrap();
    forget_android(&dirs, &fx.creds).unwrap();
    assert!(!dirs.config.join("config.json").exists());
    let account = token_account(Service::Dropbox, EMAIL).unwrap();
    assert_eq!(fx.creds.get(&account).unwrap(), None);
    assert_eq!(
        fx.creds.get(DOWNLOAD_ACCOUNT).unwrap().unwrap().expose(),
        SECOND
    );
}

#[test]
fn a_game_started_with_sync_off_never_pushes_even_if_sync_is_set_up_while_it_runs() {
    let fx = Fx::new("armed-later");
    island(&fx.saves(), "phone");
    let java = FakeJava::new(&[Some("cancel")]);
    let prompt = bridge(&java);
    let player = fx.player(&prompt);
    player.before_game();
    assert!(fx.booted.load(Ordering::SeqCst));
    assert!(!fx.pulled.load(Ordering::SeqCst));
    // The home screen signs in while the game runs; the pause and the quit push nothing.
    sign_in(&fx.roots.dirs(), &fx.creds, REFRESH);
    player.checkpoint();
    player.after_game();
    assert!(fx
        .log_text()
        .contains("this game started with save sync off"));
    assert!(fx.mark().is_none());
    let (switch, dir) = fx.device("switch");
    assert!(matches!(
        switch.pull(&dir, None).unwrap(),
        acnh_core::launch::Sync::Done(_)
    ));
    assert!(!dir.join("main.dat").exists(), "nothing was pushed");
}
