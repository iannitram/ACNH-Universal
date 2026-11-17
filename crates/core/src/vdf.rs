//! Steam's binary KeyValues, the format of `userdata/<id>/config/shortcuts.vdf` (SPEC.md M2).
//! Copied from iannitram/totk-universal's `vdf` (MIT) by its issue.
//!
//! A document is a list of items followed by the byte 0x08. An item is a type byte, a
//! NUL-terminated UTF-8 key, then its value:
//!
//! ```text
//! 0x00 key\0 <items> 0x08     a map (nested list of items, closed by 0x08)
//! 0x01 key\0 text\0           a string
//! 0x02 key\0 u32 LE           a 32-bit number
//! ```
//!
//! shortcuts.vdf is one map item "shortcuts" whose items are "0", "1", ... (one map per
//! shortcut), so the file ends 0x08 0x08.

use crate::error::{not_built, Result};

/// Deepest map nesting `read` accepts. Steam's files nest about three deep; the bound
/// keeps untrusted input from overflowing the stack.
pub const MAX_DEPTH: usize = 64;

/// A value; maps keep their items in file order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Map(Vec<(String, Value)>),
    Str(String),
    Int(u32),
}

/// Encode a document: each item as above, then 0x08.
pub fn write(doc: &[(String, Value)]) -> Vec<u8> {
    fn write_items(out: &mut Vec<u8>, items: &[(String, Value)]) {
        for (key, value) in items {
            let type_byte = match value {
                Value::Map(_) => 0x00,
                Value::Str(_) => 0x01,
                Value::Int(_) => 0x02,
            };
            out.push(type_byte);
            out.extend_from_slice(key.as_bytes());
            out.push(0x00);
            match value {
                Value::Map(inner) => write_items(out, inner),
                Value::Str(text) => {
                    out.extend_from_slice(text.as_bytes());
                    out.push(0x00);
                }
                Value::Int(n) => out.extend_from_slice(&n.to_le_bytes()),
            }
        }
        out.push(0x08);
    }

    let mut out = Vec::new();
    write_items(&mut out, doc);
    out
}

/// Decode a document written as above. Errors, all `Error::Steam`:
/// - the bytes end before the document's closing 0x08, inside a key, a string or a number:
///   "shortcuts.vdf is cut short";
/// - a type byte other than 0x00, 0x01, 0x02 where an item starts:
///   "shortcuts.vdf has an unknown item type 0x<hex, two digits>";
/// - a key or string that is not UTF-8: "shortcuts.vdf has text that is not UTF-8";
/// - bytes after the closing 0x08: "shortcuts.vdf has bytes after its end";
/// - maps nested more than 64 deep: "shortcuts.vdf is nested too deep".
pub fn read(bytes: &[u8]) -> Result<Vec<(String, Value)>> {
    not_built("vdf::read")
}
