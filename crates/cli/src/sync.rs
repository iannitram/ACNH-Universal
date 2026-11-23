//! `acnh-universal sync`: set save sync up, check it and turn it off, over `acnh_core::relay`.
//! The app password reaches the credential store only: it is read from standard input, never
//! taken from an argument, and never printed or written to a file.

use crate::{Context, Options};
use acnh_core::launch::RelayStatus;
use acnh_core::relay;
use acnh_core::saves;
use acnh_core::{Error, Result};
use save_relay::creds::CredentialStore;
use save_relay::http::Transport;
use save_relay::local::LocalStore;
use save_relay::oauth;
use save_relay::platform::{AppDirs, PathVars};
use save_relay::service::Service;
use std::io::{BufRead, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a browser sign-in waits for the redirect.
const REDIRECT_WAIT: Duration = Duration::from_secs(300);

/// What `sync` and `play` need from this machine: save-relay's folders, its credential store,
/// the network and the browser. The real ones are `SyncEnv::real`; a test passes fakes.
pub struct SyncEnv<'a> {
    /// save-relay's config and data folders (shared with its own CLI), or why they are unknown.
    pub dirs: std::result::Result<AppDirs, String>,
    pub creds: &'a dyn CredentialStore,
    pub transport: Arc<dyn Transport>,
    /// Opens a desktop sign-in page and waits for the browser to come back: gives the
    /// redirect's request target (`/?code=..&state=..`).
    pub redirect: &'a dyn Fn(&str, &mut dyn Write) -> Result<String>,
}

impl<'a> SyncEnv<'a> {
    pub(crate) fn dirs(&self) -> Result<AppDirs> {
        self.dirs.clone().map_err(|reason| {
            Error::Saves(format!("no folder for the save sync settings: {reason}"))
        })
    }

    /// The real machine: the system credential store, the network and the system browser.
    pub fn real(creds: &'a dyn CredentialStore) -> SyncEnv<'a> {
        SyncEnv {
            dirs: PathVars::current()
                .map(|vars| AppDirs::from_vars(&vars))
                .map_err(|error| error.to_string()),
            creds,
            transport: Arc::new(save_relay::http::UreqTransport::new()),
            redirect: &browser_redirect,
        }
    }
}

/// The system credential store, as `real` needs it.
pub fn real_credentials() -> save_relay::creds::KeyringStore {
    save_relay::creds::KeyringStore::new()
}

pub(crate) fn command(options: &Options, cx: &mut Context<'_>) -> Result<i32> {
    match options.hub_action.as_deref() {
        Some("setup") => setup(options, cx),
        Some("sign-in") => sign_in(options, cx),
        Some("status") => status(cx),
        Some("off") => off(cx),
        Some("push") => push(options, cx),
        _ => Err(Error::Config(
            "usage: sync setup --server <url> --user <name> | sign-in gdrive|dropbox | status | off"
                .into(),
        )),
    }
}

fn first_line(stdin: &mut dyn BufRead) -> Result<String> {
    let mut line = String::new();
    stdin.read_line(&mut line)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

fn setup(options: &Options, cx: &mut Context<'_>) -> Result<i32> {
    let server = crate::required_text(&options.server, "--server")?;
    let user = crate::required_text(&options.user, "--user")?;
    let password = first_line(cx.stdin)?;
    if password.is_empty() {
        return Err(Error::Saves(
            "give the app password on standard input (one line)".into(),
        ));
    }
    let device = options
        .device
        .clone()
        .or_else(|| {
            ["HOSTNAME", "COMPUTERNAME"]
                .iter()
                .find_map(|name| (cx.var)(name).filter(|value| !value.is_empty()))
        })
        .unwrap_or_else(|| "device".to_owned());
    let dirs = cx.sync.dirs()?;
    relay::setup_nextcloud(
        server,
        user,
        &password,
        &device,
        &dirs,
        cx.sync.creds,
        cx.sync.transport.clone(),
    )?;
    writeln!(cx.out, "save sync is on: {user} at {server}")?;
    Ok(0)
}

fn service_named(name: Option<&str>) -> Result<Service> {
    match name {
        Some("gdrive") => Ok(Service::Gdrive),
        Some("dropbox") => Ok(Service::Dropbox),
        _ => Err(Error::Config("sync sign-in takes gdrive or dropbox".into())),
    }
}

fn sign_in(options: &Options, cx: &mut Context<'_>) -> Result<i32> {
    let service = service_named(options.sync_arg.as_deref())?;
    let dirs = cx.sync.dirs()?;
    let (creds, transport) = (cx.sync.creds, cx.sync.transport.clone());
    // Game Mode has no browser to count on: the code and the URL, then a wait.
    if oauth::in_game_mode(&|name| std::env::var(name).ok()) {
        let prompt = relay::start_sign_in(service, &dirs, creds, transport.clone())?;
        writeln!(cx.out, "{}", prompt.message)?;
        writeln!(cx.out, "page: {}", prompt.url)?;
        if let Some(code) = &prompt.code {
            writeln!(cx.out, "code: {code}")?;
        }
        let answer = if service == Service::Dropbox {
            write!(cx.out, "Code from Dropbox: ")?;
            cx.out.flush()?;
            first_line(cx.stdin)?
        } else {
            String::new()
        };
        relay::finish_sign_in(service, &dirs, creds, transport, &answer)?;
    } else {
        let prompt = relay::start_sign_in(service, &dirs, creds, transport.clone())?;
        let target = (cx.sync.redirect)(&prompt.url, cx.out)?;
        relay::finish_sign_in(service, &dirs, creds, transport, &target)?;
    }
    writeln!(cx.out, "save sync is on: {}", service.label())?;
    Ok(0)
}

fn status(cx: &mut Context<'_>) -> Result<i32> {
    let dirs = cx.sync.dirs()?;
    let config = LocalStore::new(dirs.clone())
        .load_config()
        .map_err(|error| Error::Saves(error.to_string()))?;
    let Some(config) = config else {
        writeln!(cx.out, "save sync is off")?;
        return Ok(0);
    };
    let service = Service::from_server(&config.server)
        .map(|service| service.label().to_owned())
        .unwrap_or_else(|_| config.server.clone());
    writeln!(
        cx.out,
        "save sync is on: {service}, {} (this device: {})",
        config.user, config.device
    )?;
    let Some(opened) = relay::open(&dirs, cx.sync.creds, cx.sync.transport.clone())? else {
        writeln!(
            cx.out,
            "the sign-in is missing: run sync setup or sync sign-in again"
        )?;
        return Ok(0);
    };
    let vars = acnh_core::platform::Vars::from_lookup(cx.os, cx.var)?;
    let save_dir = saves::save_dir(&acnh_core::platform::AppDirs::new(&vars).data_root());
    let line = match acnh_core::launch::Relay::status(&opened, &save_dir)? {
        RelayStatus::Unavailable => "the relay cannot be reached",
        RelayStatus::Conflict { .. } => "the island changed here and on the relay",
        RelayStatus::Held { .. } => "the island is open on another device",
        RelayStatus::RelayNewer(_) => "the relay has a newer island",
        RelayStatus::LocalNewer => "the island here is newer",
        RelayStatus::Ready | RelayStatus::Off => "the island is in step",
    };
    writeln!(cx.out, "{line}")?;
    Ok(0)
}

/// `sync push [--keep relay|here]`: send the island here once, as after a `play` whose push
/// conflicted (exit 6). `--keep relay` backs the island here up before the relay's replaces it.
fn push(options: &Options, cx: &mut Context<'_>) -> Result<i32> {
    use crate::{Guard, Stop};
    use acnh_core::launch::Relay;
    let vars = acnh_core::platform::Vars::from_lookup(cx.os, cx.var)?;
    let root = acnh_core::platform::AppDirs::for_os(cx.os, &vars).data_root();
    let (save_dir, backups) = (saves::save_dir(&root), saves::backups_dir(&root));
    let opened;
    let relay: &dyn Relay = match cx.relay {
        crate::RelaySource::Fixed(relay) => relay,
        crate::RelaySource::Open => {
            opened = crate::open_relay(cx)?;
            &*opened
        }
    };
    let guard = Guard {
        inner: relay,
        keep: options.keep,
        anyway: false,
        backups: &backups,
        stopped: std::cell::RefCell::new(None),
    };
    match guard.push(&save_dir, None) {
        Ok(acnh_core::launch::Sync::Offline) => {
            Ok(crate::fail(options, cx, &Error::RelayUnreachable, 5))
        }
        Ok(sync) => {
            crate::report_sync(cx.out, "push", &sync)?;
            Ok(0)
        }
        Err(error) => match guard.stopped.take() {
            Some(Stop::Conflict { server, local, .. }) => {
                if !options.json {
                    writeln!(cx.out, "Both have a newer island. Nothing was sent.")?;
                    crate::print_card(cx.out, "relay", &server)?;
                    crate::print_card(cx.out, "here", &local)?;
                    writeln!(cx.out, "Run sync push with --keep relay or --keep here.")?;
                }
                let error = Error::Saves("the island changed here and on the relay".into());
                Ok(crate::fail(options, cx, &error, 6))
            }
            _ => Err(error),
        },
    }
}

fn off(cx: &mut Context<'_>) -> Result<i32> {
    relay::forget(&cx.sync.dirs()?, cx.sync.creds)?;
    writeln!(cx.out, "save sync is off")?;
    Ok(0)
}

/// The desktop sign-in: open the page in the browser and wait on the loopback address the
/// sign-in comes back to (the one save-relay's registrations allow).
fn browser_redirect(url: &str, out: &mut dyn Write) -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:53710").map_err(|_| {
        Error::Saves("port 53710 is in use: close what holds it and try again".into())
    })?;
    listener.set_nonblocking(true)?;
    if oauth::open_browser(url).is_err() {
        writeln!(out, "Open this page in your browser: {url}")?;
    } else {
        writeln!(out, "Finish signing in in your browser.")?;
    }
    let started = Instant::now();
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut line = String::new();
                if std::io::BufReader::new(&stream)
                    .read_line(&mut line)
                    .is_err()
                {
                    continue;
                }
                let target = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                let answered = target.split(['?', '#']).next() == Some("/")
                    && (target.contains("code=") || target.contains("error="));
                let reply: &[u8] = if answered {
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<!DOCTYPE html><html><body><p>You can close this page and go back to ACNH Universal.</p></body></html>"
                } else {
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                };
                let _ = stream.write_all(reply);
                let _ = stream.flush();
                if answered {
                    return Ok(target);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if started.elapsed() >= REDIRECT_WAIT {
                    return Err(Error::Saves("the sign-in did not finish in time".into()));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(Error::Io(error.to_string())),
        }
    }
}
