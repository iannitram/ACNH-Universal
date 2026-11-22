//! The fonts stage. Generated fonts and archives stay in the prepared folder.

pub mod button_font;
mod geometry;
pub mod truetype;

use crate::error::{Error, Result};
use crate::sarc;
use std::fs;
use std::io::Write;
use std::path::Path;

mod controller;
mod variants;

const MAGIC: u32 = 0x1802_9a7f;
const EXT_FONT: &str = "ParkExt.bfttf";

fn malformed() -> Error {
    Error::Io("invalid encrypted font container".to_owned())
}

/// Decode a BFTTF container and return the font bytes and its XOR key.
pub fn decode_bfttf(data: &[u8]) -> Result<(Vec<u8>, u32)> {
    if data.len() < 8 || !(data.len() - 8).is_multiple_of(4) {
        return Err(malformed());
    }
    let word = |offset: usize| u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
    let key = word(0) ^ MAGIC;
    let size = u32::from_be_bytes((word(4) ^ key).to_le_bytes()) as usize;
    if size > data.len() - 8 {
        return Err(malformed());
    }
    let mut font = Vec::with_capacity(data.len() - 8);
    for chunk in data[8..].as_chunks::<4>().0 {
        let value = u32::from_le_bytes(*chunk) ^ key;
        font.extend_from_slice(&value.to_le_bytes());
    }
    font.truncate(size);
    Ok((font, key))
}

/// Wrap a font in BFTTF using the original container's XOR key.
pub fn encode_bfttf(font: &[u8], key: u32) -> Result<Vec<u8>> {
    let size = u32::try_from(font.len()).map_err(|_| malformed())?;
    let padded = font.len().checked_add(3).ok_or_else(malformed)? & !3;
    let mut out = Vec::with_capacity(padded.checked_add(8).ok_or_else(malformed)?);
    out.extend_from_slice(&(MAGIC ^ key).to_le_bytes());
    out.extend_from_slice(&(u32::from_le_bytes(size.to_be_bytes()) ^ key).to_le_bytes());
    for chunk in font.chunks(4) {
        let mut bytes = [0; 4];
        bytes[..chunk.len()].copy_from_slice(chunk);
        out.extend_from_slice(&(u32::from_le_bytes(bytes) ^ key).to_le_bytes());
    }
    Ok(out)
}

/// Replace the extension font in a decompressed SARC, retaining all other members.
pub fn replace_extension(archive: &[u8], font: &[u8]) -> Result<Vec<u8>> {
    let sarc = sarc::parse(archive)?;
    let members = sarc.members()?;
    let matches: Vec<_> = members
        .iter()
        .enumerate()
        .filter(|(_, m)| m.name == EXT_FONT)
        .collect();
    let [(index, member)] = matches.as_slice() else {
        return Err(Error::Io(
            "archive must contain exactly one extension font".to_owned(),
        ));
    };
    let (_, key) = decode_bfttf(member.data)?;
    let mut replacements = vec![None; members.len()];
    replacements[*index] = Some(encode_bfttf(font, key)?);
    sarc.replace_members(&replacements)
}

/// Write the shared TrueType fonts and the controller-specific ParkExt archives.
///
/// The stage accepts a host font selected by the caller. Some hosts have none
/// of the supported fonts installed. They can still build the glyph archives.
///
/// The output contains game data. Call this only with a private Prepare folder.
pub fn stage(romfs: &Path, out: &Path, host_font: Option<&Path>) -> Result<()> {
    fs::create_dir_all(out)?;
    if let Some(host) = host_font {
        let shared = out.join("shared.ttf");
        let buttons = out.join("shared_5.ttf");
        if !shared.is_file() || !buttons.is_file() {
            let data = fs::read(host)?;
            if !shared.is_file() {
                write_font(&shared, &data)?;
            }
            if !buttons.is_file() {
                write_font(&buttons, &button_font::build(&data)?)?;
            }
        }
    }
    variants::build(romfs, out)
}

fn write_font(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Io("font output has no parent directory".into()))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(data)?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}
