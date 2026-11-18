//! The two packed data files a prepare writes for the runtime (issue 1.27): `romfs.pak`
//! (`runtime/src/hle_fs.cpp`) and `spirv.pak` (`runtime/src/nvn/nvn_shaders.cpp`). Replaces
//! upstream's `tools/pack_romfs.py` and `tools/pack_spirv.py`, byte for byte.
//!
//! `romfs.pak`: "NXRP", u32 version 1, u32 count, then per file u16 path length, the UTF-8
//! path ('/' separated, relative to the RomFS), u64 offset, u64 size; then the files' bytes,
//! each at a 16-byte aligned offset. Files are sorted by path.
//!
//! `spirv.pak`: "SPK1" and the count (u32 each), then per file 16 bytes of its hash (the name
//! without `.spv`), its offset and size (u64 each), then the files. A `results.tsv` file in
//! the folder goes in too, last, under the name "results.tsv". Little-endian.

use crate::error::{Error, Result};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const ROMFS_MAGIC: &[u8; 4] = b"NXRP";
const ROMFS_VERSION: u32 = 1;
const SPIRV_MAGIC: u32 = 0x314B_5053; // "SPK1" little-endian
const SPIRV_NAME: usize = 16;
const ALIGN: u64 = 16;

struct RomfsFile {
    rel: String,
    path: PathBuf,
    size: u64,
    offset: u64,
}

/// Writes `root` as `romfs.pak` at `out`, with the whole pack.
pub fn romfs(root: &Path, out: &Path) -> Result<()> {
    romfs_range(root, out, 0, u64::MAX)
}

/// Writes bytes `from..to` of `root`'s `romfs.pak` at `out`, for sending the pack in parts
/// (`tools/push_android.sh`): a file wholly before `from` is counted but not read.
pub fn romfs_range(root: &Path, out: &Path, from: u64, to: u64) -> Result<()> {
    let files = romfs_layout(root)?;
    let index = romfs_index(&files)?;
    let file = File::create(out)?;
    let mut range = RangeWriter::new(file, from, to);
    range.write_all(&index)?;
    let mut pos = index.len() as u64;
    for entry in &files {
        if range.done() {
            break;
        }
        write_zeros(&mut range, entry.offset - pos)?;
        if entry.offset + entry.size <= from {
            range.skip(entry.size);
            pos = entry.offset + entry.size;
            continue;
        }
        let mut source = File::open(&entry.path)?;
        io::copy(&mut source, &mut range)?;
        pos = entry.offset + entry.size;
    }
    range.flush()?;
    Ok(())
}

/// The size `root`'s `romfs.pak` would have, without writing it (upstream's `--size`).
pub fn romfs_size(root: &Path) -> Result<u64> {
    let files = romfs_layout(root)?;
    Ok(files
        .last()
        .map(|entry| entry.offset + entry.size)
        .unwrap_or(index_len(&files) as u64))
}

fn romfs_layout(root: &Path) -> Result<Vec<RomfsFile>> {
    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    let mut offset = align(index_len(&files) as u64);
    for entry in &mut files {
        entry.offset = offset;
        offset = align(offset + entry.size);
    }
    Ok(files)
}

fn walk(root: &Path, dir: &Path, files: &mut Vec<RomfsFile>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        let is_file = if kind.is_symlink() {
            fs::metadata(&path)
                .map(|meta| meta.is_file())
                .unwrap_or(false)
        } else {
            kind.is_file()
        };
        if is_file {
            let rel = relative(root, &path)?;
            let size = entry.metadata()?.len();
            files.push(RomfsFile {
                rel,
                path,
                size,
                offset: 0,
            });
        } else if kind.is_dir() {
            walk(root, &path, files)?;
        }
    }
    Ok(())
}

fn relative(root: &Path, path: &Path) -> Result<String> {
    let rel = path.strip_prefix(root).map_err(|_| {
        Error::Io(format!(
            "{} is not under {}",
            path.display(),
            root.display()
        ))
    })?;
    let mut name = String::new();
    for part in rel.components() {
        let part = part
            .as_os_str()
            .to_str()
            .ok_or_else(|| Error::Io(format!("{} is not UTF-8", path.display())))?;
        if !name.is_empty() {
            name.push('/');
        }
        name.push_str(part);
    }
    Ok(name)
}

fn index_len(files: &[RomfsFile]) -> usize {
    4 + 4
        + 4
        + files
            .iter()
            .map(|entry| 2 + entry.rel.len() + 8 + 8)
            .sum::<usize>()
}

fn romfs_index(files: &[RomfsFile]) -> Result<Vec<u8>> {
    let mut index = Vec::with_capacity(index_len(files));
    index.extend_from_slice(ROMFS_MAGIC);
    index.extend_from_slice(&ROMFS_VERSION.to_le_bytes());
    index.extend_from_slice(&(files.len() as u32).to_le_bytes());
    for entry in files {
        let len = u16::try_from(entry.rel.len())
            .map_err(|_| Error::Io(format!("RomFS path is too long: {}", entry.rel)))?;
        index.extend_from_slice(&len.to_le_bytes());
        index.extend_from_slice(entry.rel.as_bytes());
        index.extend_from_slice(&entry.offset.to_le_bytes());
        index.extend_from_slice(&entry.size.to_le_bytes());
    }
    Ok(index)
}

fn align(value: u64) -> u64 {
    (value + ALIGN - 1) & !(ALIGN - 1)
}

fn write_zeros(writer: &mut impl Write, count: u64) -> io::Result<()> {
    let zeros = [0u8; ALIGN as usize];
    let mut left = count;
    while left > 0 {
        let take = left.min(zeros.len() as u64) as usize;
        writer.write_all(&zeros[..take])?;
        left -= take as u64;
    }
    Ok(())
}

/// Writes only the bytes of a stream in `[lo, hi)`, counting every byte offered.
struct RangeWriter<W: Write> {
    inner: W,
    pos: u64,
    lo: u64,
    hi: u64,
}

impl<W: Write> RangeWriter<W> {
    fn new(inner: W, lo: u64, hi: u64) -> RangeWriter<W> {
        RangeWriter {
            inner,
            pos: 0,
            lo,
            hi,
        }
    }

    fn skip(&mut self, count: u64) {
        self.pos += count;
    }

    fn done(&self) -> bool {
        self.pos >= self.hi
    }
}

impl<W: Write> Write for RangeWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let start = self.pos.max(self.lo);
        let end = (self.pos + bytes.len() as u64).min(self.hi);
        if start < end {
            let first = (start - self.pos) as usize;
            let last = (end - self.pos) as usize;
            self.inner.write_all(&bytes[first..last])?;
        }
        self.pos += bytes.len() as u64;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Writes the `.spv` files of `src` as `spirv.pak` at `out`, with `results.tsv` last when it
/// is there.
pub fn spirv(src: &Path, out: &Path) -> Result<()> {
    let entries = spirv_entries(src)?;
    let mut offset = 8 + (SPIRV_NAME as u64 + 8 + 8) * entries.len() as u64;
    let mut index = Vec::with_capacity(offset as usize);
    index.extend_from_slice(&SPIRV_MAGIC.to_le_bytes());
    index.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (name, _, size) in &entries {
        let mut key = [0u8; SPIRV_NAME];
        let bytes = name.as_bytes();
        let take = bytes.len().min(SPIRV_NAME);
        key[..take].copy_from_slice(&bytes[..take]);
        index.extend_from_slice(&key);
        index.extend_from_slice(&offset.to_le_bytes());
        index.extend_from_slice(&size.to_le_bytes());
        offset += *size;
    }

    let temp = with_suffix(out, ".tmp");
    let written = write_spirv(&temp, &index, &entries);
    if let Err(error) = written {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    if fs::rename(&temp, out).is_err() {
        fs::remove_file(out)?;
        fs::rename(&temp, out)?;
    }
    Ok(())
}

fn write_spirv(path: &Path, index: &[u8], entries: &[(String, PathBuf, u64)]) -> Result<()> {
    let mut file = File::create(path)?;
    file.write_all(index)?;
    for (_, source, _) in entries {
        let mut source = File::open(source)?;
        io::copy(&mut source, &mut file)?;
    }
    file.flush()?;
    Ok(())
}

fn spirv_entries(src: &Path) -> Result<Vec<(String, PathBuf, u64)>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(Error::Io(format!(
                "{} is not UTF-8",
                entry.path().display()
            )));
        };
        if let Some(key) = name.strip_suffix(".spv") {
            if key.len() <= SPIRV_NAME {
                names.push((key.to_owned(), name.to_owned()));
            }
        }
    }
    names.sort_by(|a, b| a.1.cmp(&b.1)); // by file name with ".spv", as upstream does

    let mut entries = Vec::with_capacity(names.len() + 1);
    for (key, name) in names {
        let path = src.join(name);
        entries.push((key, path.clone(), fs::metadata(path)?.len()));
    }
    let results = src.join("results.tsv");
    if results.is_file() {
        entries.push((
            "results.tsv".to_owned(),
            results.clone(),
            fs::metadata(results)?.len(),
        ));
    }
    Ok(entries)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// `romfs`, under the upstream tool's name.
pub fn pack_romfs(root: &Path, out: &Path) -> Result<()> {
    romfs(root, out)
}

/// `romfs_size`, under the upstream tool's name.
pub fn pack_romfs_size(root: &Path) -> Result<u64> {
    romfs_size(root)
}

/// `spirv`, under the upstream tool's name.
pub fn pack_spirv(src: &Path, out: &Path) -> Result<()> {
    spirv(src, out)
}
