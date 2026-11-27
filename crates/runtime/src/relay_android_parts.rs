//! The Android app's parts of save-relay that need no JNI (`relay_android.rs` uses them): the file credential store, the prompt bridge and the wire format of its network answers.

use super::*;

// ---- the Android app's parts that need no JNI (relay_android.rs uses them) -------------------

/// save-relay's `CredentialStore` as one JSON file (`{account: secret}`) in the Android app's
/// private files folder: Android's per-app sandbox keeps other apps out, and the file is
/// `0600` besides. It is the only place the Dropbox refresh token is written. Every change is a
/// whole new file renamed over the old, so a crash leaves the old one or the new one. A file
/// that does not read as that JSON is an error, never quietly emptied. `Debug` shows the path
/// only.
pub struct FileStore {
    path: PathBuf,
}

/// One change at a time within the process (the sign-in on the home screen, a push on the
/// game's background worker).
pub(super) static FILE_STORE_LOCK: Mutex<()> = Mutex::new(());

impl std::fmt::Debug for FileStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStore")
            .field("path", &self.path)
            .finish()
    }
}

pub(super) fn store_error(what: &str) -> save_relay::Error {
    save_relay::Error::Credentials(format!("the app's credential file {what}"))
}

impl FileStore {
    pub fn new(path: impl Into<PathBuf>) -> FileStore {
        FileStore { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The secret of the first account (by name) starting with `prefix`.
    pub fn first_with_prefix(&self, prefix: &str) -> save_relay::Result<Option<Secret>> {
        let _one = FILE_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        Ok(self
            .load()?
            .into_iter()
            .find(|(account, _)| account.starts_with(prefix))
            .map(|(_, secret)| Secret::new(secret)))
    }

    /// Keeps only `accounts` (the download's token on a turn-off whose forget left something). A
    /// file that does not read is an error here, for the caller to remove.
    pub fn retain(&self, accounts: &[&str]) -> save_relay::Result<()> {
        let _one = FILE_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.load()?;
        let before = all.len();
        all.retain(|account, _| accounts.contains(&account.as_str()));
        if all.len() == before {
            return Ok(());
        }
        self.save(&all)
    }

    /// Removes the whole file (every secret in it): "Turn off save sync" when the file cannot be
    /// read, so a damaged file does not keep save sync on. A missing file is fine.
    pub fn remove(&self) -> save_relay::Result<()> {
        let _one = FILE_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(store_error("cannot be removed")),
        }
    }

    fn load(&self) -> save_relay::Result<BTreeMap<String, String>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| store_error("does not read")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(_) => Err(store_error("cannot be read")),
        }
    }

    fn save(&self, all: &BTreeMap<String, String>) -> save_relay::Result<()> {
        use std::io::Write;
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(|_| store_error("folder cannot be made"))?;
        let partial = self.path.with_extension("json.partial");
        let _ = std::fs::remove_file(&partial);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let written = (|| {
            let mut file = options.open(&partial)?;
            let text = serde_json::to_vec(all).map_err(std::io::Error::other)?;
            file.write_all(&text)?;
            file.sync_all()?;
            std::fs::rename(&partial, &self.path)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&partial);
            return Err(store_error("cannot be written"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

impl CredentialStore for FileStore {
    fn get(&self, account: &str) -> save_relay::Result<Option<Secret>> {
        let _one = FILE_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        Ok(self.load()?.remove(account).map(Secret::new))
    }

    fn set(&self, account: &str, secret: &Secret) -> save_relay::Result<()> {
        let _one = FILE_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.load()?;
        all.insert(account.to_string(), secret.expose().to_string());
        self.save(&all)
    }

    fn delete(&self, account: &str) -> save_relay::Result<()> {
        let _one = FILE_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.load()?;
        if all.remove(account).is_none() {
            return Ok(());
        }
        self.save(&all)
    }

    fn kind(&self) -> &'static str {
        "android-app-file"
    }
}

/// Where the Android app keeps the Dropbox refresh token for the game download alone, once save
/// sync is turned off (the same `FileStore`): one sign-in serves both, and turning sync off
/// stops the syncing only.
pub const DOWNLOAD_ACCOUNT: &str = "download:dropbox";

/// The Android app's "Turn off save sync" (the home screen's button and the blocked screens'):
/// save sync stops on this device, the Dropbox sign-in stays for the game download. The token
/// moves from save-relay's account to `DOWNLOAD_ACCOUNT` first (when it can be read), then
/// `acnh_core::relay::forget` runs, best effort. When a secret could not be deleted (the credential
/// file does not read), the whole credential file goes: it holds save-relay's and the download's
/// secrets only, and a file that does not read serves neither. An error comes back only when
/// something is still left behind.
pub fn forget_android(dirs: &AppDirs, creds: &FileStore) -> Result<()> {
    keep_for_download(dirs, creds);
    clear_waiting(dirs);
    forget_android_all(dirs, creds, false)
}

/// "Sign out of Dropbox": save sync and the game download both end on this device (the
/// download's token goes too).
pub fn sign_out_android(dirs: &AppDirs, creds: &FileStore) -> Result<()> {
    clear_waiting(dirs);
    let forgot = forget_android_all(dirs, creds, true);
    let download = creds
        .delete(DOWNLOAD_ACCOUNT)
        .or_else(|_| creds.remove())
        .map_err(|e| Error::Saves(e.to_string()));
    forgot.and(download)
}

/// A push or a mark an older setup left waiting goes with it: the next sign-in starts with a
/// pull, never with that push.
pub(super) fn clear_waiting(dirs: &AppDirs) {
    let _ = std::fs::remove_file(push_waiting_file(dirs));
    let _ = std::fs::remove_file(dirs.data.join("mark-waiting"));
}

/// save-relay's Dropbox refresh token, kept for the download under `DOWNLOAD_ACCOUNT`: the one
/// the config names, or, when the config does not read (or names none), the Dropbox token in the
/// credential file itself (save-relay files it as `dropbox:<account>`).
pub(super) fn keep_for_download(dirs: &AppDirs, creds: &FileStore) {
    let named = LocalStore::new(dirs.clone())
        .load_config()
        .ok()
        .flatten()
        .filter(|config| config.server == "dropbox")
        .and_then(|config| {
            save_relay::service::token_account(save_relay::service::Service::Dropbox, &config.user)
                .ok()
        });
    let token = match named.map(|account| creds.get(&account)) {
        Some(Ok(Some(token))) => Some(token),
        _ => creds.first_with_prefix("dropbox:").ok().flatten(),
    };
    if let Some(token) = token {
        let _ = creds.set(DOWNLOAD_ACCOUNT, &token);
    }
}

/// `acnh_core::relay::forget`, then, when that left something (a secret it could not delete, a
/// config it could not read for the accounts), what is left in the credential file: every
/// account but the download's on a turn-off, and the whole file on a sign-out or when it does
/// not read at all.
pub(super) fn forget_android_all(
    dirs: &AppDirs,
    creds: &FileStore,
    everything: bool,
) -> Result<()> {
    let Err(error) = acnh_core::relay::forget(dirs, creds) else {
        return Ok(());
    };
    let cleared = if everything {
        creds.remove()
    } else {
        creds
            .retain(&[DOWNLOAD_ACCOUNT])
            .or_else(|_| creds.remove())
    };
    cleared.map_err(|e| Error::Saves(format!("{error}; {e}")))?;
    match std::fs::metadata(dirs.config.join("config.json")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(error),
    }
}

/// The home screen's side of `BridgePrompt`: a request's JSON in, the answer's JSON out.
pub(super) type Ask<'a> = dyn Fn(&str) -> Option<String> + std::marker::Sync + 'a;

/// The prompts as one JSON request each, answered by the app's own screens (Android: dialogs on
/// the home screen, `Relay.ask` in android/java, reached through JNI). `ask` blocks until the
/// person answers and gives the answer's JSON, or `None` when the screens cannot be reached;
/// nothing is then decided for the person (the request is asked again a moment later, or the
/// harmless answer is taken: Cancel, Check again, Retry). Requests never hold a secret, the
/// account's user name or a path.
///
/// - `{"kind": "sign_in", "again": bool, "allow_not_now": bool, "message": text or null}`:
///   `signed_in`, `not_now`, `cancel`
/// - `{"kind": "conflict", "server": text, "local": text}` (`describe_card`): `server`, `local`
/// - `{"kind": "held", "device": text, "since": text}`: `play_anyway`, `check_again`
/// - `{"kind": "blocked", "why": "offline" or "error" or "sign_in_cancelled", "message": text
///   or null, "can_turn_off": bool}`: `retry`, `sign_in_again` (after a cancelled sign-in),
///   `turn_off` (where `can_turn_off`)
///
/// The reply is `{"answer": "<one of those>"}`.
pub struct BridgePrompt<'a> {
    ask: Box<Ask<'a>>,
    /// How long to wait before asking again when the screens could not be reached.
    pub pause: std::time::Duration,
}

impl<'a> BridgePrompt<'a> {
    pub fn new(ask: impl Fn(&str) -> Option<String> + std::marker::Sync + 'a) -> BridgePrompt<'a> {
        BridgePrompt {
            ask: Box::new(ask),
            pause: std::time::Duration::from_secs(1),
        }
    }

    /// The `answer` field of the reply to `request`, or `None`.
    fn answer(&self, request: &serde_json::Value) -> Option<String> {
        let reply = (self.ask)(&request.to_string())?;
        let reply: serde_json::Value = serde_json::from_str(&reply).ok()?;
        reply
            .get("answer")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    }
}

impl Prompt for BridgePrompt<'_> {
    fn sign_in(&self, form: &SignInForm) -> SignInAnswer {
        let request = serde_json::json!({
            "kind": "sign_in",
            "again": !form.allow_not_now,
            "allow_not_now": form.allow_not_now,
            "message": form.message,
        });
        match self.answer(&request).as_deref() {
            Some("signed_in") => SignInAnswer::SignedIn,
            Some("not_now") if form.allow_not_now => SignInAnswer::NotNow,
            _ => SignInAnswer::Cancel,
        }
    }

    fn conflict(&self, server: &SaveCard, local: &SaveCard) -> Keep {
        let request = serde_json::json!({
            "kind": "conflict",
            "server": describe_card(server),
            "local": describe_card(local),
        });
        // Which island to keep is the person's choice alone: ask until one is picked.
        loop {
            match self.answer(&request).as_deref() {
                Some("server") => return Keep::Server,
                Some("local") => return Keep::Local,
                _ => std::thread::sleep(self.pause),
            }
        }
    }

    fn held(&self, device: &str, since: &str) -> HeldAnswer {
        let request = serde_json::json!({"kind": "held", "device": device, "since": since});
        match self.answer(&request).as_deref() {
            Some("play_anyway") => HeldAnswer::PlayAnyway,
            Some(_) => HeldAnswer::CheckAgain,
            None => {
                std::thread::sleep(self.pause);
                HeldAnswer::CheckAgain
            }
        }
    }

    fn blocked(&self, why: &Blocked) -> BlockedAnswer {
        let (kind, message) = match why {
            Blocked::Offline => ("offline", None),
            Blocked::Error(message) => ("error", Some(message.as_str())),
            Blocked::SignInCancelled => ("sign_in_cancelled", None),
        };
        let can_turn_off = *why != Blocked::Offline;
        let request = serde_json::json!({"kind": "blocked", "why": kind, "message": message,
                                         "can_turn_off": can_turn_off});
        match self.answer(&request).as_deref() {
            Some("sign_in_again") if *why == Blocked::SignInCancelled => BlockedAnswer::SignInAgain,
            Some("turn_off") if *why != Blocked::Offline => BlockedAnswer::TurnOff,
            Some(_) => BlockedAnswer::Retry,
            None => {
                std::thread::sleep(self.pause);
                BlockedAnswer::Retry
            }
        }
    }
}

/// A request's headers as save-relay's C interface hands them to the app's `http` callback:
/// `Name: value` lines, each ended by `\n`.
pub fn header_lines(request: &save_relay::http::Request) -> String {
    let mut lines = String::new();
    for (name, value) in &request.headers {
        lines.push_str(name);
        lines.push_str(": ");
        lines.push_str(value);
        lines.push('\n');
    }
    lines
}

/// The app's answer to one request in save-relay's Android wire format (`SaveRelayResponse.toWire`
/// in its bindings): the status and the headers' length as big-endian `u32`s, the `Name: value`
/// header lines, then the body. Anything shorter or malformed is `Unreachable`, as a network
/// failure is.
pub fn parse_wire(wire: &[u8]) -> save_relay::Result<save_relay::http::Response> {
    let bad = || save_relay::Error::Unreachable("the app's network answer is malformed".into());
    if wire.len() < 8 {
        return Err(bad());
    }
    let status = u32::from_be_bytes([wire[0], wire[1], wire[2], wire[3]]);
    let header_len = u32::from_be_bytes([wire[4], wire[5], wire[6], wire[7]]) as usize;
    let status = u16::try_from(status).map_err(|_| bad())?;
    let rest = &wire[8..];
    if rest.len() < header_len {
        return Err(bad());
    }
    let headers = std::str::from_utf8(&rest[..header_len]).map_err(|_| bad())?;
    let mut response = save_relay::http::Response::new(status, rest[header_len..].to_vec());
    for line in headers.lines() {
        if let Some((name, value)) = line.split_once(':') {
            response = response.header(name.trim(), value.trim());
        }
    }
    Ok(response)
}

/// A request as Android's `HttpURLConnection` must send it. A POST without a `Content-Type` (save-
/// relay's Dropbox download and list calls: an empty body, the arguments in a header) would go out
/// with `application/x-www-form-urlencoded`, which Dropbox refuses with 400 ("Bad HTTP
/// Content-Type header"): it gets `application/octet-stream`. Every request asks for
/// `Accept-Encoding: identity`, so the body comes back as sent and its length is the real one.
pub fn for_android(request: &save_relay::http::Request) -> save_relay::http::Request {
    let mut request = request.clone();
    if request.method.eq_ignore_ascii_case("POST") && request.header_value("Content-Type").is_none()
    {
        request = request.header("Content-Type", "application/octet-stream");
    }
    if request.header_value("Accept-Encoding").is_none() {
        request = request.header("Accept-Encoding", "identity");
    }
    request
}

/// save-relay's `Transport` over the app's own network call (Android: the Java host's `http`,
/// the same one save-relay's C interface uses for the sign-in), given the request and its
/// `header_lines`, answering in `parse_wire`'s format, or `None` for a connection failure.
pub struct CallbackTransport<F>(pub F);

impl<F> save_relay::http::Transport for CallbackTransport<F>
where
    F: Fn(&save_relay::http::Request, &str) -> Option<Vec<u8>> + Send + std::marker::Sync,
{
    fn send(
        &self,
        request: &save_relay::http::Request,
    ) -> save_relay::Result<save_relay::http::Response> {
        let request = &for_android(request);
        let wire = (self.0)(request, &header_lines(request)).ok_or_else(|| {
            save_relay::Error::Unreachable("the app's network call failed".into())
        })?;
        let response = parse_wire(&wire)?;
        match request.max_response_bytes {
            Some(max) if response.body.len() > max => Err(save_relay::Error::Unreachable(
                "the answer is larger than allowed".into(),
            )),
            _ => Ok(response),
        }
    }
}
