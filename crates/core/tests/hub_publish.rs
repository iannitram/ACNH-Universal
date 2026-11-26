#![cfg(feature = "hub")]
//! A build is published, served by a running hub, paired with a generated code and pulled
//! again after an interruption. Synthetic files only.

use acnh_core::bundle;
use acnh_core::hub::{self, HostConfig, HubHost};
use acnh_core::platform::{AppDirs, Os, Vars};
use acnh_core::recipe::Hub;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Folders(PathBuf);

impl Folders {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/hub-publish-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn dirs(&self, name: &str) -> AppDirs {
        AppDirs::new(&Vars {
            home: self.0.clone(),
            data: self.path(&format!("{name}/data")),
            config: self.path(&format!("{name}/config")),
        })
    }
}

impl Drop for Folders {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// A made-up player: an executable, a library folder and the device-owned entries a build never carries.
fn install_folder(folders: &Folders, name: &str, big: usize, marker: u8) -> PathBuf {
    let root = folders.path(name);
    fs::create_dir_all(root.join("lib")).unwrap();
    fs::create_dir_all(root.join("root")).unwrap();
    fs::create_dir_all(root.join("logs")).unwrap();
    fs::write(root.join("player"), vec![marker; 1000]).unwrap();
    fs::write(root.join("lib/big.bin"), vec![marker; big]).unwrap();
    fs::write(root.join("root/save.sav"), b"a save").unwrap();
    fs::write(root.join("logs/run.log"), b"a log").unwrap();
    root
}

fn host(folders: &Folders, builds: &Path) -> HubHost {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    HubHost::start(
        folders.dirs("hub"),
        listener,
        HostConfig {
            name: "hub".into(),
            role: Hub::Mac,
            builds: builds.into(),
            prepared: None,
            pair: true,
            announce: false,
        },
    )
    .unwrap()
}

fn wait_for_code(host: &HubHost) -> String {
    let start = Instant::now();
    loop {
        if let Some(code) = host.status().code {
            return code;
        }
        assert!(start.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn publishing_writes_bundle_text_beside_files_without_device_owned_entries() {
    let folders = Folders::new();
    let install = install_folder(&folders, "install", 10, 1);
    let builds = folders.path("builds");
    let b = hub::publish_build(&builds, &install, Os::Linux, "3.0.2", "abc").unwrap();
    let dir = builds.join(&b.build_id);
    let text = fs::read_to_string(dir.join("bundle.txt")).unwrap();
    assert_eq!(bundle::from_text(&text).unwrap(), b);
    assert_eq!(fs::read(dir.join("lib/big.bin")).unwrap(), vec![1; 10]);
    assert!(!dir.join("root").exists() && !dir.join("logs").exists());
    assert_eq!(b.files.len(), 2);
    // Publishing the same build again changes nothing.
    let again = hub::publish_build(&builds, &install, Os::Linux, "3.0.2", "abc").unwrap();
    assert_eq!(again, b);
    assert_eq!(fs::read_dir(&builds).unwrap().count(), 1);
}

#[test]
fn publishing_refuses_game_data_and_leaves_nothing_behind() {
    let folders = Folders::new();
    let install = install_folder(&folders, "install", 10, 1);
    fs::write(install.join("data.pak"), b"x").unwrap();
    let builds = folders.path("builds");
    assert!(hub::publish_build(&builds, &install, Os::Linux, "3.0.2", "abc").is_err());
    assert!(!builds.exists() || fs::read_dir(&builds).unwrap().next().is_none());
}

#[test]
fn only_the_newest_builds_of_a_target_are_kept() {
    let folders = Folders::new();
    let builds = folders.path("builds");
    let mut names = Vec::new();
    for marker in 1..=3 {
        let install = install_folder(&folders, &format!("install{marker}"), 10, marker);
        let b = hub::publish_build(&builds, &install, Os::Linux, "3.0.2", "abc").unwrap();
        names.push(b.build_id);
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!builds.join(&names[0]).exists());
    assert!(builds.join(&names[1]).exists() && builds.join(&names[2]).exists());
}

#[test]
fn a_hub_shows_a_generated_code_pairs_a_device_and_serves_the_published_build() {
    let folders = Folders::new();
    let install = install_folder(&folders, "install", 200_000, 7);
    let builds = folders.path("builds");
    let published = hub::publish_build(&builds, &install, Os::Mac, "3.0.2", "abc").unwrap();
    let host = host(&folders, &builds);
    let code = wait_for_code(&host);
    assert_eq!(code.len(), 6);
    assert!(code.bytes().all(|b| b.is_ascii_digit()));

    let client = folders.dirs("client");
    // A wrong code does not pair, and the right one still does after it.
    let wrong = if code == "000000" { "111111" } else { "000000" };
    assert!(hub::pair_hub(host.address(), &client, "phone", "hub", wrong, Os::Mac).is_err());
    hub::pair_hub(host.address(), &client, "phone", "hub", &code, Os::Mac).unwrap();
    assert_eq!(hub::read_peers(&client).unwrap()[0].device, "hub");

    // The used-up code is replaced by a new one, and the paired device is listed by name
    // (the hub saves it just after the client returns).
    let start = Instant::now();
    while host.status().code.as_deref() == Some(code.as_str()) {
        assert!(start.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(
        hub::read_peers(&folders.dirs("hub")).unwrap()[0].device,
        "phone"
    );

    let staged = hub::resume_dir(&client).unwrap();
    let pulled = hub::pull_hub(host.address(), &client, "phone", "hub", Os::Mac, &staged).unwrap();
    assert_eq!(pulled, published);
    let target = folders.path("target");
    bundle::install(&pulled, &staged, &target).unwrap();
    assert_eq!(
        fs::read(target.join("lib/big.bin")).unwrap(),
        vec![7; 200_000]
    );
    assert!(!staged.exists());
    host.stop();
}

// Forwards a connection to `to`, cutting what comes back after `limit` bytes.
fn cutting_proxy(to: std::net::SocketAddr, limit: usize) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        let Ok((mut client, _)) = listener.accept() else {
            return;
        };
        let mut server = TcpStream::connect(to).unwrap();
        let (mut client_in, mut server_in) =
            (client.try_clone().unwrap(), server.try_clone().unwrap());
        thread::spawn(move || {
            let mut buffer = [0; 4096];
            while let Ok(n) = client_in.read(&mut buffer) {
                if n == 0 || server_in.write_all(&buffer[..n]).is_err() {
                    break;
                }
            }
        });
        let mut buffer = [0; 4096];
        let mut sent = 0;
        while sent < limit {
            match server.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let n = n.min(limit - sent);
                    if client.write_all(&buffer[..n]).is_err() {
                        break;
                    }
                    sent += n;
                }
            }
        }
        let _ = client.shutdown(std::net::Shutdown::Both);
        let _ = server.shutdown(std::net::Shutdown::Both);
    });
    address
}

fn chunk_files(staged: &Path) -> usize {
    let cache = staged.join(".hub-transfer");
    fs::read_dir(cache)
        .map(|dirs| {
            dirs.flatten()
                .map(|d| fs::read_dir(d.path()).map(|f| f.count()).unwrap_or(0))
                .sum()
        })
        .unwrap_or(0)
}

#[test]
fn an_interrupted_pull_continues_from_the_chunks_it_has() {
    let folders = Folders::new();
    let install = install_folder(&folders, "install", 3_000_000, 9);
    let builds = folders.path("builds");
    let published = hub::publish_build(&builds, &install, Os::Mac, "3.0.2", "abc").unwrap();
    let host = host(&folders, &builds);
    let code = wait_for_code(&host);
    let client = folders.dirs("client");
    hub::pair_hub(host.address(), &client, "phone", "hub", &code, Os::Mac).unwrap();

    let staged = hub::resume_dir(&client).unwrap();
    let cut = cutting_proxy(host.address(), 1_200_000);
    assert!(hub::pull_hub(cut, &client, "phone", "hub", Os::Mac, &staged).is_err());
    let kept = chunk_files(&staged);
    assert!(kept > 5, "chunks kept: {kept}");

    // A leftover of another build never reaches the install.
    fs::write(staged.join("stale.bin"), b"old build").unwrap();
    let served_before = host.status().served;
    let staged_again = hub::resume_dir(&client).unwrap();
    assert_eq!(staged_again, staged);
    let pulled = hub::pull_hub(host.address(), &client, "phone", "hub", Os::Mac, &staged).unwrap();
    assert_eq!(pulled, published);
    assert_eq!(host.status().served, served_before + 1);
    let target = folders.path("target");
    bundle::install(&pulled, &staged, &target).unwrap();
    assert!(!target.join("stale.bin").exists());
    assert_eq!(
        fs::read(target.join("lib/big.bin")).unwrap().len(),
        3_000_000
    );
}

#[test]
fn generated_codes_are_six_digits_and_differ() {
    let codes: Vec<String> = (0..20).map(|_| hub::generate_code().unwrap()).collect();
    assert!(codes
        .iter()
        .all(|c| c.len() == 6 && c.bytes().all(|b| b.is_ascii_digit())));
    assert!(codes.iter().any(|c| *c != codes[0]));
}
