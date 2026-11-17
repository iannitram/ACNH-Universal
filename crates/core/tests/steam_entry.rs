// Locked tests for steam: the shortcut entry and adding it to shortcuts.vdf (SPEC.md M2).
use acnh_core::steam::{shortcut_entry, upsert, Shortcut};
use acnh_core::vdf::Value;
use acnh_core::Error;

fn kv(k: &str, v: Value) -> (String, Value) {
    (k.to_string(), v)
}

fn shortcut(options: &str) -> Shortcut {
    Shortcut {
        name: "Animal Crossing: New Horizons".into(),
        exe: "/home/u/.local/bin/acnh-universal".into(),
        start_dir: "/home/u".into(),
        launch_options: options.into(),
        icon: "/home/u/acnh.png".into(),
    }
}

const ID: u32 = 0xA57E_4FE0;

fn entry(options: &str) -> Value {
    let s = |t: &str| Value::Str(t.to_string());
    Value::Map(vec![
        kv("appid", Value::Int(ID)),
        kv("AppName", s("Animal Crossing: New Horizons")),
        kv("Exe", s("\"/home/u/.local/bin/acnh-universal\"")),
        kv("StartDir", s("\"/home/u\"")),
        kv("icon", s("/home/u/acnh.png")),
        kv("ShortcutPath", s("")),
        kv("LaunchOptions", s(options)),
        kv("IsHidden", Value::Int(0)),
        kv("AllowDesktopConfig", Value::Int(1)),
        kv("AllowOverlay", Value::Int(1)),
        kv("OpenVR", Value::Int(0)),
        kv("Devkit", Value::Int(0)),
        kv("DevkitGameID", s("")),
        kv("DevkitOverrideAppID", Value::Int(0)),
        kv("LastPlayTime", Value::Int(0)),
        kv("FlatpakAppID", s("")),
        kv("tags", Value::Map(vec![])),
    ])
}

fn other() -> Value {
    Value::Map(vec![kv("appid", Value::Int(5))])
}

#[test]
fn entry_fields_in_steams_order() {
    assert_eq!(shortcut_entry(&shortcut("play")), entry("play"));
}

#[test]
fn empty_document_gets_the_list() {
    let mut doc = vec![];
    assert_eq!(upsert(&mut doc, &shortcut("play")), Ok(ID));
    let want = vec![kv("shortcuts", Value::Map(vec![kv("0", entry("play"))]))];
    assert_eq!(doc, want);
}

#[test]
fn new_entry_goes_last_and_renumbers() {
    let list = vec![kv("3", other()), kv("9", other())];
    let mut doc = vec![kv("shortcuts", Value::Map(list))];
    upsert(&mut doc, &shortcut("play")).unwrap();
    let want = vec![kv("0", other()), kv("1", other()), kv("2", entry("play"))];
    assert_eq!(doc, vec![kv("shortcuts", Value::Map(want))]);
}

#[test]
fn same_app_id_is_replaced_in_place() {
    let list = vec![kv("0", entry("old")), kv("1", other())];
    let mut doc = vec![kv("shortcuts", Value::Map(list))];
    assert_eq!(upsert(&mut doc, &shortcut("play")), Ok(ID));
    let want = vec![kv("0", entry("play")), kv("1", other())];
    assert_eq!(doc, vec![kv("shortcuts", Value::Map(want))]);
}

#[test]
fn no_shortcuts_list_is_refused() {
    let mut doc = vec![kv("other", Value::Map(vec![]))];
    assert_eq!(
        upsert(&mut doc, &shortcut("play")),
        Err(Error::Steam("shortcuts.vdf has no shortcuts list".into()))
    );
    let mut doc = vec![kv("shortcuts", Value::Str("x".into()))];
    assert!(upsert(&mut doc, &shortcut("play")).is_err());
}
