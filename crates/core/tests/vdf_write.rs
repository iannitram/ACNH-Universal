// Locked tests for vdf::write: Steam's binary KeyValues (shortcuts.vdf).
use acnh_core::vdf::{write, Value};

fn kv(k: &str, v: Value) -> (String, Value) {
    (k.to_string(), v)
}

#[test]
fn empty_document_is_the_end_byte() {
    assert_eq!(write(&[]), vec![0x08]);
}

#[test]
fn string_item() {
    let doc = [kv("AppName", Value::Str("ACNH".into()))];
    let mut want = vec![0x01];
    want.extend_from_slice(b"AppName\0ACNH\0");
    want.push(0x08);
    assert_eq!(write(&doc), want);
}

#[test]
fn int_item_is_little_endian() {
    let doc = [kv("appid", Value::Int(0x8123_4567))];
    let mut want = vec![0x02];
    want.extend_from_slice(b"appid\0");
    want.extend_from_slice(&[0x67, 0x45, 0x23, 0x81]);
    want.push(0x08);
    assert_eq!(write(&doc), want);
}

#[test]
fn shortcuts_shape_ends_with_two_end_bytes() {
    let entry = Value::Map(vec![
        kv("appid", Value::Int(1)),
        kv("tags", Value::Map(vec![])),
    ]);
    let doc = [kv("shortcuts", Value::Map(vec![kv("0", entry)]))];
    let mut want = vec![0x00];
    want.extend_from_slice(b"shortcuts\0");
    want.push(0x00);
    want.extend_from_slice(b"0\0");
    want.push(0x02);
    want.extend_from_slice(b"appid\0");
    want.extend_from_slice(&[1, 0, 0, 0]);
    want.push(0x00);
    want.extend_from_slice(b"tags\0");
    want.extend_from_slice(&[0x08, 0x08, 0x08, 0x08]);
    assert_eq!(write(&doc), want);
}

#[test]
fn items_keep_their_order_and_utf8() {
    let doc = [
        kv("b", Value::Str("é".into())),
        kv("a", Value::Str(String::new())),
    ];
    let mut want = vec![0x01, b'b', 0, 0xC3, 0xA9, 0, 0x01, b'a', 0, 0];
    want.push(0x08);
    assert_eq!(write(&doc), want);
}
