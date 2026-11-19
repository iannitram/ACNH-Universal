//! SARC archives (issue 1.29): the reader and the big-endian rewrite of upstream's
//! `tools/sarc_endian.py`. ACNH's own archives are big-endian (`\xFE\xFF`); a tool that wrote
//! little-endian ones (`\xFF\xFE`) crashed the game at boot, which sizes its buffer from the
//! zstd frame. `big_endian` changes the headers and compression only: the members stay where
//! they were, byte for byte.
//!
//! The archive layout read here is the one `tools/sarc_endian.py` uses: the SARC header
//! (`0x14` bytes), then SFAT (its own header plus `count` nodes of `0x10`), then SFNT (its
//! header plus the NUL-terminated names, padded until `data_offset`), then the member data.

use crate::error::{Error, Result};
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The byte order of an archive's fields, from the BOM at `0x06`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    Little,
    Big,
}

impl Endian {
    /// `Little` for the `\xFF\xFE` BOM, `Big` otherwise (as upstream's script: only the
    /// little-endian marker is special).
    pub fn of(data: &[u8]) -> Endian {
        if data.get(6..8) == Some(&[0xff, 0xfe][..]) {
            Endian::Little
        } else {
            Endian::Big
        }
    }
}

/// One SFAT node: the name hash, attributes (the name offset, when bit 24 is set), and the
/// member's start and end relative to the data section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    pub hash: u32,
    pub attr: u32,
    pub start: u32,
    pub end: u32,
}

/// A parsed archive. `names` is the raw SFNT names section, padding included.
#[derive(Debug, Clone)]
pub struct Archive<'a> {
    pub endian: Endian,
    pub version: u16,
    pub hash_key: u32,
    pub nodes: Vec<Node>,
    pub names: &'a [u8],
    pub data_offset: u32,
    data: &'a [u8],
}

/// One member of an archive: its name and its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member<'a> {
    pub name: String,
    pub data: &'a [u8],
}

/// Parse `data` as a SARC archive. A file that is not one, or whose header, node table or
/// names section does not fit, is `Error::Io` with a plain message.
pub fn parse(data: &[u8]) -> Result<Archive<'_>> {
    if data.len() < 0x14 {
        return Err(malformed("the header is shorter than 0x14 bytes"));
    }
    if &data[..4] != b"SARC" {
        return Err(Error::Io("not a SARC archive".to_owned()));
    }

    let endian = Endian::of(data);
    let header_len = u16_at(data, 4, endian)? as usize;
    let data_offset = u32_at(data, 12, endian)? as usize;
    let version = u16_at(data, 16, endian)?;
    if header_len < 0x14 || header_len > data.len() {
        return Err(malformed("the header length is outside the file"));
    }
    if data.get(header_len..header_len + 4) != Some(b"SFAT") {
        return Err(malformed("there is no SFAT section"));
    }
    let sfat_len = u16_at(data, header_len + 4, endian)? as usize;
    let count = u16_at(data, header_len + 6, endian)? as usize;
    let hash_key = u32_at(data, header_len + 8, endian)?;
    if sfat_len < 0x0C {
        return Err(malformed("the SFAT section is shorter than its header"));
    }

    let nodes_start = header_len
        .checked_add(sfat_len)
        .ok_or_else(|| malformed("the node table does not fit"))?;
    let nodes_len = count
        .checked_mul(0x10)
        .ok_or_else(|| malformed("the node table does not fit"))?;
    let nodes_end = nodes_start
        .checked_add(nodes_len)
        .ok_or_else(|| malformed("the node table does not fit"))?;
    if nodes_end > data.len() {
        return Err(malformed("the node table lies past the end of the file"));
    }

    let mut nodes = Vec::with_capacity(count);
    for index in 0..count {
        let at = nodes_start + index * 0x10;
        nodes.push(Node {
            hash: u32_at(data, at, endian)?,
            attr: u32_at(data, at + 4, endian)?,
            start: u32_at(data, at + 8, endian)?,
            end: u32_at(data, at + 12, endian)?,
        });
    }

    let sfnt = header_len
        .checked_add(sfat_len)
        .and_then(|value| value.checked_add(nodes_len))
        .ok_or_else(|| malformed("the SFNT section does not fit"))?;
    let names_start = sfnt
        .checked_add(8)
        .ok_or_else(|| malformed("the names section does not fit"))?;
    if names_start > data_offset || data_offset > data.len() {
        return Err(malformed(
            "the names section does not reach the data offset",
        ));
    }

    Ok(Archive {
        endian,
        version,
        hash_key,
        nodes,
        names: &data[names_start..data_offset],
        data_offset: data_offset as u32,
        data,
    })
}

impl<'a> Archive<'a> {
    /// Every member: its SFNT name (or `<index>.bin` when the node has none) and its bytes.
    pub fn members(&self) -> Result<Vec<Member<'a>>> {
        let base = self.data_offset as usize;
        let mut members = Vec::with_capacity(self.nodes.len());
        for (index, node) in self.nodes.iter().enumerate() {
            let name = if node.attr >> 24 != 0 {
                let offset = ((node.attr & 0xFFFF) as usize)
                    .checked_mul(4)
                    .ok_or_else(|| malformed("a member name lies outside the names section"))?;
                cstr(self.names, offset)?
            } else {
                format!("{index:04}.bin")
            };
            let start = base
                .checked_add(node.start as usize)
                .ok_or_else(|| malformed("a member starts past the end of the file"))?;
            let end = base
                .checked_add(node.end as usize)
                .ok_or_else(|| malformed("a member ends past the end of the file"))?;
            let data = self
                .data
                .get(start..end)
                .ok_or_else(|| malformed("a member lies past the end of the file"))?;
            members.push(Member { name, data });
        }
        Ok(members)
    }
}

/// Rewrite `data` the way ACNH's own archives are: big-endian headers. An archive that is
/// already big-endian is returned unchanged. The members are not touched.
pub fn big_endian(data: &[u8]) -> Result<Vec<u8>> {
    let archive = parse(data)?;
    if archive.endian == Endian::Big {
        return Ok(data.to_vec());
    }

    let total = u32::try_from(data.len())
        .map_err(|_| malformed("the archive is larger than a SARC can describe"))?;
    let mut out = Vec::with_capacity(data.len());
    out.extend_from_slice(b"SARC");
    out.extend_from_slice(&0x14u16.to_be_bytes());
    out.extend_from_slice(&[0xfe, 0xff]);
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(&archive.data_offset.to_be_bytes());
    out.extend_from_slice(&archive.version.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());

    out.extend_from_slice(b"SFAT");
    out.extend_from_slice(&0x0Cu16.to_be_bytes());
    out.extend_from_slice(&(archive.nodes.len() as u16).to_be_bytes());
    out.extend_from_slice(&archive.hash_key.to_be_bytes());
    for node in &archive.nodes {
        out.extend_from_slice(&node.hash.to_be_bytes());
        out.extend_from_slice(&node.attr.to_be_bytes());
        out.extend_from_slice(&node.start.to_be_bytes());
        out.extend_from_slice(&node.end.to_be_bytes());
    }

    out.extend_from_slice(b"SFNT");
    out.extend_from_slice(&0x08u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(archive.names);

    let data_offset = archive.data_offset as usize;
    if out.len() != data_offset {
        return Err(malformed(format!(
            "layout changed ({} != {data_offset})",
            out.len()
        )));
    }
    out.extend_from_slice(&data[data_offset..]);
    Ok(out)
}

/// The bytes of `path`, decompressed first when it ends in `.zs` (the `zstd` command, as
/// upstream's script does).
pub fn read(path: &Path) -> Result<Vec<u8>> {
    let data = fs::read(path)?;
    if ends_with(path, ".zs") {
        zstd_decompress(path)
    } else {
        Ok(data)
    }
}

/// Rewrite `src` into `dst`, compressing again when `dst` ends in `.zs` (from a file, not a
/// pipe, so the frame records its decompressed size and has no checksum).
pub fn rewrite(src: &Path, dst: &Path) -> Result<()> {
    let data = read(src)?;
    let out = big_endian(&data)?;
    if ends_with(dst, ".zs") {
        let raw = raw_path(dst);
        fs::write(&raw, &out)?;
        let status = Command::new("zstd")
            .args(["-19", "-q", "-f", "--no-check"])
            .arg(&raw)
            .arg("-o")
            .arg(dst)
            .status();
        let _ = fs::remove_file(&raw);
        match status {
            Ok(status) if status.success() => Ok(()),
            Ok(_) => Err(Error::Io(format!("zstd could not write {}", dst.display()))),
            Err(error) => Err(Error::Io(format!("cannot run zstd: {error}"))),
        }
    } else {
        fs::write(dst, &out)?;
        Ok(())
    }
}

/// Upstream's `--check`: the byte order of `path`, for a plain message.
pub fn check(path: &Path) -> Result<Endian> {
    let data = read(path)?;
    Ok(parse(&data)?.endian)
}

/// Rewrite every `.sarc.zs` of a mod's RomFS the way the game reads it. Files that already
/// record their size in the zstd frame and are big-endian are left alone; the rest go through
/// [`rewrite`] in place. Returns how many were rewritten.
pub fn fix_archives(romfs: &Path) -> Result<usize> {
    let mut files = Vec::new();
    collect_sarc_zs(romfs, &mut files)?;
    files.sort();
    if !files.is_empty() && Command::new("zstd").arg("-V").output().is_err() {
        eprintln!("zstd not found: the mod archives are left as they are");
        return Ok(0);
    }
    let mut fixed = 0;
    for path in files {
        let data = read(&path)?;
        let big = data.get(6..8) == Some(&[0xfe, 0xff][..]);
        let frame = zstd_frame_ok(&path);
        if big && frame {
            continue;
        }
        rewrite(&path, &path)?;
        fixed += 1;
    }
    Ok(fixed)
}

fn collect_sarc_zs(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            collect_sarc_zs(&path, out)?;
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".sarc.zs"))
        {
            out.push(path);
        }
    }
    Ok(())
}

/// `zstd -l` says the frame has a decompressed size and no checksum, as ACNH's own do.
fn zstd_frame_ok(path: &Path) -> bool {
    let output = Command::new("zstd")
        .args(["-l", "-v"])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return false;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    text.contains("Decompressed Size") && !text.contains("XXH64")
}

fn zstd_decompress(path: &Path) -> Result<Vec<u8>> {
    let output = Command::new("zstd")
        .args(["-d", "-q", "-c"])
        .arg(path)
        .output()
        .map_err(|error| Error::Io(format!("cannot run zstd: {error}")))?;
    if !output.status.success() {
        return Err(Error::Io(format!("zstd could not read {}", path.display())));
    }
    Ok(output.stdout)
}

/// `path` with `.raw` appended (upstream's `dst + ".raw"`, an OS string, not a new extension).
fn raw_path(path: &Path) -> PathBuf {
    let mut raw: OsString = path.as_os_str().to_owned();
    raw.push(".raw");
    PathBuf::from(raw)
}

fn ends_with(path: &Path, suffix: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(suffix))
}

fn cstr(bytes: &[u8], offset: usize) -> Result<String> {
    let rest = bytes
        .get(offset..)
        .ok_or_else(|| malformed("a member name lies outside the names section"))?;
    let end = rest
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| malformed("a member name has no terminator"))?;
    Ok(String::from_utf8_lossy(&rest[..end]).into_owned())
}

fn u16_at(data: &[u8], offset: usize, endian: Endian) -> Result<u16> {
    let bytes = slice_at(data, offset, 2)?;
    let value = [bytes[0], bytes[1]];
    Ok(match endian {
        Endian::Little => u16::from_le_bytes(value),
        Endian::Big => u16::from_be_bytes(value),
    })
}

fn u32_at(data: &[u8], offset: usize, endian: Endian) -> Result<u32> {
    let bytes = slice_at(data, offset, 4)?;
    let value = [bytes[0], bytes[1], bytes[2], bytes[3]];
    Ok(match endian {
        Endian::Little => u32::from_le_bytes(value),
        Endian::Big => u32::from_be_bytes(value),
    })
}

fn slice_at(data: &[u8], offset: usize, len: usize) -> Result<&[u8]> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| malformed("a field lies past the end of the file"))?;
    data.get(offset..end)
        .ok_or_else(|| malformed("a field lies past the end of the file"))
}

fn malformed(message: impl fmt::Display) -> Error {
    Error::Io(format!("SARC archive: {message}"))
}
