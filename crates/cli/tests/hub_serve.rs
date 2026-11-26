//! `hub publish` puts a build where `hub serve` finds it, and `hub serve --pair` shows a
//! generated six-digit code a new device types. Synthetic files only.
use acnh_core::platform::{AppDirs, Os, Vars};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};

fn dirs(home: &Path) -> AppDirs {
    AppDirs::new(
        &Vars::from_lookup(Os::current(), |key| match key {
            "HOME" | "USERPROFILE" | "LOCALAPPDATA" | "APPDATA" => Some(home.display().to_string()),
            "XDG_DATA_HOME" => Some(home.join("data").display().to_string()),
            "XDG_CONFIG_HOME" => Some(home.join("config").display().to_string()),
            _ => None,
        })
        .unwrap(),
    )
}

fn base(home: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_acnh-universal"));
    command
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("LOCALAPPDATA", home)
        .env("APPDATA", home)
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CONFIG_HOME", home.join("config"));
    command
}

fn run(home: &Path, args: &[&str], input: Option<&str>) -> std::process::Output {
    let mut child = base(home, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    if let Some(input) = input {
        stdin.write_all(input.as_bytes()).unwrap();
    }
    drop(stdin);
    child.wait_with_output().unwrap()
}

#[test]
fn publish_then_serve_shows_a_generated_code_that_pairs_a_device() {
    let root = std::env::temp_dir().join(format!("acnh-hub-serve-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let install = root.join("install");
    fs::create_dir_all(install.join("root")).unwrap();
    fs::write(install.join("player"), b"synthetic player").unwrap();
    fs::write(install.join("root/save.sav"), b"a save").unwrap();
    let builds = root.join("builds");
    let server_home = root.join("server");
    let out = run(
        &server_home,
        &[
            "hub",
            "publish",
            "--out",
            builds.to_str().unwrap(),
            "--install",
            install.to_str().unwrap(),
            "--target",
            "linux",
            "--version",
            "3.0.2",
        ],
        None,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let published: Vec<_> = fs::read_dir(&builds).unwrap().collect();
    assert_eq!(published.len(), 1);
    let build = published[0].as_ref().unwrap().path();
    assert!(build.join("bundle.txt").is_file() && build.join("player").is_file());
    assert!(!build.join("root").exists());

    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let address = format!("127.0.0.1:{port}");
    let mut hub_process = base(
        &server_home,
        &[
            "hub",
            "serve",
            "--address",
            address.as_str(),
            "--name",
            "hub",
            "--hub",
            "mac",
            "--out",
            builds.to_str().unwrap(),
            "--pair",
        ],
    )
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit())
    .spawn()
    .unwrap();
    let mut lines = BufReader::new(hub_process.stdout.take().unwrap()).lines();
    let code = loop {
        let line = lines
            .next()
            .expect("hub exited before showing a code")
            .unwrap();
        if let Some(code) = line.strip_prefix("Pairing code: ") {
            break code.to_owned();
        }
    };
    assert_eq!(code.len(), 6);
    assert!(code.bytes().all(|b| b.is_ascii_digit()));

    let client_home = root.join("client");
    let common = [
        "--address",
        address.as_str(),
        "--name",
        "client",
        "--peer",
        "hub",
        "--target",
        "linux",
    ];
    let mut args = vec!["hub", "pair"];
    args.extend(common);
    let paired = run(&client_home, &args, Some(&format!("{code}\n")));
    let mut args = vec!["hub", "pull"];
    args.extend(common);
    let pulled = run(&client_home, &args, None);
    hub_process.kill().unwrap();
    let _ = hub_process.wait();
    assert!(
        paired.status.success(),
        "{}",
        String::from_utf8_lossy(&paired.stderr)
    );
    assert!(
        pulled.status.success(),
        "{}",
        String::from_utf8_lossy(&pulled.stderr)
    );
    assert_eq!(
        fs::read(dirs(&client_home).install().join("player")).unwrap(),
        b"synthetic player"
    );
    fs::remove_dir_all(root).unwrap();
}
