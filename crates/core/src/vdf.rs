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

use crate::error::{Error, Result};

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

/// Encode a document: each item as above, then 0x08. Returns
/// `Error::Steam("shortcuts.vdf text can't hold a NUL")` if a key or string
/// contains a NUL.
pub fn write(doc: &[(String, Value)]) -> Result<Vec<u8>> {
    fn write_items(out: &mut Vec<u8>, items: &[(String, Value)]) -> Result<()> {
        for (key, value) in items {
            if key.contains('\0') {
                return Err(Error::Steam(
                    "shortcuts.vdf text can't hold a NUL".to_string(),
                ));
            }
            let type_byte = match value {
                Value::Map(_) => 0x00,
                Value::Str(_) => 0x01,
                Value::Int(_) => 0x02,
            };
            out.push(type_byte);
            out.extend_from_slice(key.as_bytes());
            out.push(0x00);
            match value {
                Value::Map(inner) => write_items(out, inner)?,
                Value::Str(text) => {
                    if text.contains('\0') {
                        return Err(Error::Steam(
                            "shortcuts.vdf text can't hold a NUL".to_string(),
                        ));
                    }
                    out.extend_from_slice(text.as_bytes());
                    out.push(0x00);
                }
                Value::Int(n) => out.extend_from_slice(&n.to_le_bytes()),
            }
        }
        out.push(0x08);
        Ok(())
    }

    let mut out = Vec::new();
    write_items(&mut out, doc)?;
    Ok(out)
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
    fn steam(message: &'static str) -> Error {
        Error::Steam(message.to_string())
    }

    fn take_text(bytes: &[u8], cursor: &mut usize) -> Result<String> {
        let remaining = bytes
            .get(*cursor..)
            .ok_or_else(|| steam("shortcuts.vdf is cut short"))?;
        let end = remaining
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| steam("shortcuts.vdf is cut short"))?;
        let text = std::str::from_utf8(&remaining[..end])
            .map_err(|_| steam("shortcuts.vdf has text that is not UTF-8"))?
            .to_string();
        *cursor += end + 1;
        Ok(text)
    }

    fn items(bytes: &[u8], cursor: &mut usize, map_depth: usize) -> Result<Vec<(String, Value)>> {
        let mut result = Vec::new();
        loop {
            let type_byte = *bytes
                .get(*cursor)
                .ok_or_else(|| steam("shortcuts.vdf is cut short"))?;
            *cursor += 1;
            if type_byte == 0x08 {
                return Ok(result);
            }
            if !matches!(type_byte, 0x00..=0x02) {
                return Err(Error::Steam(format!(
                    "shortcuts.vdf has an unknown item type 0x{type_byte:02x}"
                )));
            }

            let key = take_text(bytes, cursor)?;
            let value = match type_byte {
                0x00 => {
                    let depth = map_depth + 1;
                    if depth > MAX_DEPTH {
                        return Err(steam("shortcuts.vdf is nested too deep"));
                    }
                    Value::Map(items(bytes, cursor, depth)?)
                }
                0x01 => Value::Str(take_text(bytes, cursor)?),
                0x02 => {
                    let number = bytes
                        .get(*cursor..cursor.saturating_add(4))
                        .ok_or_else(|| steam("shortcuts.vdf is cut short"))?;
                    let value = u32::from_le_bytes([number[0], number[1], number[2], number[3]]);
                    *cursor += 4;
                    Value::Int(value)
                }
                _ => unreachable!("type byte was validated above"),
            };
            result.push((key, value));
        }
    }

    let mut cursor = 0;
    let doc = items(bytes, &mut cursor, 0)?;
    if cursor != bytes.len() {
        return Err(steam("shortcuts.vdf has bytes after its end"));
    }
    Ok(doc)
}
