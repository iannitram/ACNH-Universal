// Locked tests for vdf::read: Steam's binary KeyValues (shortcuts.vdf).
use acnh_core::vdf::{read, Value};
use acnh_core::Error;

fn kv(k: &str, v: Value) -> (String, Value) {
    (k.to_string(), v)
}

fn steam(m: &str) -> Error {
    Error::Steam(m.to_string())
}

// shortcuts { 0 { appid 7, AppName "ACNH", tags {} } }
const FILE: &[u8] = b"\x00shortcuts\x00\x000\x00\x02appid\x00\x07\x00\x00\x00\
\x01AppName\x00ACNH\x00\x00tags\x00\x08\x08\x08\x08";

#[test]
fn reads_the_shortcuts_shape() {
    let entry = Value::Map(vec![
        kv("appid", Value::Int(7)),
        kv("AppName", Value::Str("ACNH".into())),
        kv("tags", Value::Map(vec![])),
    ]);
    let want = vec![kv("shortcuts", Value::Map(vec![kv("0", entry)]))];
    assert_eq!(read(FILE), Ok(want));
}

#[test]
fn empty_document() {
    assert_eq!(read(&[0x08]), Ok(vec![]));
}

#[test]
fn an_empty_file_is_an_empty_document() {
    assert_eq!(read(&[]), Ok(vec![]));
}

#[test]
fn int_is_little_endian_u32() {
    let b = b"\x02n\x00\x67\x45\x23\x81\x08";
    assert_eq!(read(b), Ok(vec![kv("n", Value::Int(0x8123_4567))]));
}

#[test]
fn cut_short_anywhere() {
    for end in 1..FILE.len() {
        assert_eq!(
            read(&FILE[..end]),
            Err(steam("shortcuts.vdf is cut short")),
            "{end}"
        );
    }
}

#[test]
fn unknown_type() {
    assert_eq!(
        read(b"\x07x\x00\x08"),
        Err(steam("shortcuts.vdf has an unknown item type 0x07"))
    );
    assert_eq!(
        read(b"\x00m\x00\x1a\x08\x08"),
        Err(steam("shortcuts.vdf has an unknown item type 0x1a"))
    );
}

#[test]
fn not_utf8() {
    assert_eq!(
        read(b"\x01k\x00\xff\x00\x08"),
        Err(steam("shortcuts.vdf has text that is not UTF-8"))
    );
}

#[test]
fn trailing_bytes() {
    assert_eq!(
        read(b"\x08\x00"),
        Err(steam("shortcuts.vdf has bytes after its end"))
    );
}
