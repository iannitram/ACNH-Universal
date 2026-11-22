use acnh_core::platform::{AppDirs, Os, Vars};
use acnh_core::{bundle, fleet, hub};
use std::fs;
use std::io::Write;
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
fn command(home: &Path, args: &[&str], input: Option<&str>) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_acnh-universal"))
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("LOCALAPPDATA", home)
        .env("APPDATA", home)
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    child.wait_with_output().unwrap()
}
#[test]
fn commands_pair_pull_and_report_to_fake_hub() {
    let root = std::env::temp_dir().join(format!("acnh-hub-cli-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let server = dirs(&root.join("server"));
    let client_home = root.join("client");
    let builds = root.join("builds");
    let build = builds.join("one");
    fs::create_dir_all(&build).unwrap();
    fs::write(build.join("player"), b"synthetic player").unwrap();
    let manifest = bundle::manifest(&build, Os::Android, "3.0.2", "test").unwrap();
    fs::write(build.join("bundle.txt"), bundle::to_text(&manifest)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    std::thread::scope(|s| {
        let worker = s.spawn(|| {
            let mut session = hub::PairingSession::new("123456").unwrap();
            for _ in 0..6 {
                let (stream, _) = listener.accept().unwrap();
                hub::serve_request(
                    &mut hub::TcpTransport::from_stream(stream).unwrap(),
                    &server,
                    &builds,
                    None,
                    Some(&mut session),
                )
                .unwrap();
            }
        });
        let common = [
            "--address",
            address.as_str(),
            "--name",
            "client",
            "--peer",
            "hub",
            "--target",
            "android",
        ];
        let mut args = vec!["hub", "pair"];
        args.extend(common);
        let out = command(&client_home, &args, Some("123456\n"));
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mut args = vec!["hub", "pull"];
        args.extend(common);
        let out = command(&client_home, &args, None);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            fs::read(dirs(&client_home).install().join("player")).unwrap(),
            b"synthetic player"
        );
        let reported = hub::read_fleet(&server).unwrap();
        assert_eq!(reported.len(), 1);
        assert_eq!(reported[0].version, manifest.version);
        assert_eq!(reported[0].build_id, manifest.build_id);
        assert_eq!(reported[0].last_sync_unix, 0);

        let mut args = vec!["hub", "report"];
        args.extend(common);
        args.extend([
            "--version",
            "3.0.2",
            "--build-id",
            "test",
            "--last-sync",
            "12",
            "--free",
            "34",
        ]);
        let out = command(&client_home, &args, None);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mut args = vec!["hub", "pull"];
        args.extend(common);
        let out = command(&client_home, &args, None);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        worker.join().unwrap();
    });
    let records = hub::read_fleet(&server).unwrap();
    assert_eq!(
        records,
        [fleet::DeviceRecord {
            device: "client".into(),
            os: Os::Android,
            version: "3.0.2".into(),
            build_id: manifest.build_id,
            last_sync_unix: 12,
            free_bytes: 34
        }]
    );
    let out = command(&root.join("server"), &["fleet"], None);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("client\tandroid\t3.0.2"));
    fs::remove_dir_all(root).unwrap();
}
