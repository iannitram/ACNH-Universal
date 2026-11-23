// Locked tests for platform (issue 1.01: OS keys and folders from environment variables).
use acnh_core::platform::{AppDirs, Os, Vars, ALL_OS};
use acnh_core::Error;
use std::path::PathBuf;

fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
    move |k| {
        pairs
            .iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| v.to_string())
    }
}

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

#[test]
fn keys_round_trip() {
    let keys: Vec<&str> = ALL_OS.iter().map(|o| o.key()).collect();
    assert_eq!(keys, ["linux", "windows", "mac", "ios", "android"]);
    for os in ALL_OS {
        assert_eq!(Os::from_key(os.key()), Some(os));
    }
    assert_eq!(Os::from_key("Linux"), None);
    assert_eq!(Os::from_key("macos"), None);
    assert_eq!(Os::from_key(""), None);
}

#[test]
fn current_matches_the_build() {
    let want = if cfg!(target_os = "windows") {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::Mac
    } else if cfg!(target_os = "ios") {
        Os::Ios
    } else if cfg!(target_os = "android") {
        Os::Android
    } else {
        Os::Linux
    };
    assert_eq!(Os::current(), want);
}

#[test]
fn linux_defaults_under_home() {
    let v = Vars::from_lookup(Os::Linux, env(&[("HOME", "/home/u")])).unwrap();
    assert_eq!(v.home, p("/home/u"));
    assert_eq!(v.data, p("/home/u").join(".local/share"));
    assert_eq!(v.config, p("/home/u").join(".config"));
}

#[test]
fn linux_xdg_wins() {
    const E: &[(&str, &str)] = &[
        ("HOME", "/home/u"),
        ("XDG_DATA_HOME", "/d"),
        ("XDG_CONFIG_HOME", "/c"),
    ];
    let v = Vars::from_lookup(Os::Linux, env(E)).unwrap();
    assert_eq!((v.data, v.config), (p("/d"), p("/c")));
}

#[test]
fn linux_empty_xdg_counts_as_unset() {
    const E: &[(&str, &str)] = &[("HOME", "/home/u"), ("XDG_DATA_HOME", "")];
    let v = Vars::from_lookup(Os::Linux, env(E)).unwrap();
    assert_eq!(v.data, p("/home/u").join(".local/share"));
}

#[test]
fn windows_folders() {
    const E: &[(&str, &str)] = &[
        ("USERPROFILE", r"C:\Users\u"),
        ("LOCALAPPDATA", r"C:\Users\u\AppData\Local"),
        ("APPDATA", r"C:\Users\u\AppData\Roaming"),
    ];
    let v = Vars::from_lookup(Os::Windows, env(E)).unwrap();
    assert_eq!(v.home, p(r"C:\Users\u"));
    assert_eq!(v.data, p(r"C:\Users\u\AppData\Local"));
    assert_eq!(v.config, p(r"C:\Users\u\AppData\Roaming"));
}

#[test]
fn mac_application_support() {
    let v = Vars::from_lookup(Os::Mac, env(&[("HOME", "/Users/u")])).unwrap();
    let support = p("/Users/u").join("Library/Application Support");
    assert_eq!((v.data, v.config), (support.clone(), support));
}

#[test]
fn missing_home_is_env_error() {
    assert_eq!(
        Vars::from_lookup(Os::Linux, env(&[])),
        Err(Error::Env("HOME is not set".into()))
    );
    assert_eq!(
        Vars::from_lookup(Os::Mac, env(&[("HOME", "")])),
        Err(Error::Env("HOME is not set".into()))
    );
}

#[test]
fn windows_first_missing_is_named() {
    const E: &[(&str, &str)] = &[("USERPROFILE", r"C:\Users\u"), ("APPDATA", r"C:\x")];
    assert_eq!(
        Vars::from_lookup(Os::Windows, env(E)),
        Err(Error::Env("LOCALAPPDATA is not set".into()))
    );
}

#[test]
fn mobile_is_unsupported() {
    assert_eq!(
        Vars::from_lookup(Os::Ios, env(&[("HOME", "/x")])),
        Err(Error::Unsupported(
            "the app supplies its folders on ios".into()
        ))
    );
    assert_eq!(
        Vars::from_lookup(Os::Android, env(&[])),
        Err(Error::Unsupported(
            "the app supplies its folders on android".into()
        ))
    );
}

#[test]
fn app_dirs_follow_vars() {
    let v = Vars::from_lookup(Os::Linux, env(&[("HOME", "/home/u")])).unwrap();
    let d = AppDirs::for_os(Os::Linux, &v);
    assert_eq!(
        d.data,
        p("/home/u").join(".local/share").join("acnh-universal")
    );
    assert_eq!(
        d.config,
        p("/home/u").join(".config").join("acnh-universal")
    );
    assert_eq!(d.install(), d.data.join("install"));
    assert_eq!(d.data_root(), d.data.join("install").join("root"));
    assert_eq!(d.logs(), d.data.join("install").join("logs"));
}

#[test]
fn mac_data_root_and_settings_share_application_support() {
    let v = Vars::from_lookup(Os::Mac, env(&[("HOME", "/Users/user")])).unwrap();
    let d = AppDirs::for_os(Os::Mac, &v);
    let folder = p("/Users/user/Library/Application Support/ACNH-Universal");
    assert_eq!(d.config, folder);
    assert_eq!(d.data_root(), folder);
    assert_eq!(d.logs(), folder.join("logs"));
}

#[test]
fn linux_and_windows_keep_their_folders() {
    let v = Vars::from_lookup(Os::Linux, env(&[("HOME", "/home/user")])).unwrap();
    assert_eq!(
        AppDirs::for_os(Os::Linux, &v).data_root(),
        p("/home/user/.local/share/acnh-universal/install/root")
    );
    const E: &[(&str, &str)] = &[
        ("USERPROFILE", r"C:\Users\u"),
        ("LOCALAPPDATA", r"C:\Users\u\AppData\Local"),
        ("APPDATA", r"C:\Users\u\AppData\Roaming"),
    ];
    let v = Vars::from_lookup(Os::Windows, env(E)).unwrap();
    let d = AppDirs::for_os(Os::Windows, &v);
    assert_eq!(
        d.data_root(),
        p(r"C:\Users\u\AppData\Local").join("acnh-universal/install/root")
    );
    assert_eq!(
        d.config,
        p(r"C:\Users\u\AppData\Roaming").join("acnh-universal")
    );
}
