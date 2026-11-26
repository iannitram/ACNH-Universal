//! Unpacking the archives Prepare downloads (`.tar.gz`, `.tar.zst`, `.zip`) and the fan-mod
//! zip, in Rust. No host Python, `tar` or `zstd` binary is needed (SPEC.md M13).
//!
//! Every entry path is checked before anything is written: an absolute path or a `..` is
//! refused, and a link that points outside the destination is skipped, so a hostile archive
//! cannot write beside the destination.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};

/// The archive kind, taken from the download's URL or file name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    TarGz,
    TarZst,
    Zip,
}

impl Kind {
    /// `.tar.zst` and `.zip` by name; any other name is a `.tar.gz`, as before.
    pub fn of(name: &str) -> Self {
        if name.ends_with(".tar.zst") {
            Kind::TarZst
        } else if name.ends_with(".zip") {
            Kind::Zip
        } else {
            Kind::TarGz
        }
    }
}

fn bad(msg: impl std::fmt::Display) -> Error {
    Error::Build(format!("archive: {msg}"))
}

/// The entry's path below the destination: `None` for an entry that is only the stripped top
/// folder, an error for a path that could leave the destination.
fn safe_rel(path: &Path, strip: bool) -> Result<Option<PathBuf>> {
    let mut parts = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(p) => parts.push(p),
            Component::CurDir => {}
            _ => return Err(bad(format!("unsafe path {}", path.display()))),
        }
    }
    if strip && !parts.is_empty() {
        parts.remove(0);
    }
    if parts.is_empty() {
        return Ok(None);
    }
    Ok(Some(parts.iter().collect()))
}

/// Whether a link at `rel` (below the destination) pointing at `target` stays inside the
/// destination, judged on the names alone.
fn link_stays_inside(rel: &Path, target: &Path) -> bool {
    if target.has_root() {
        return false;
    }
    let mut depth = rel.components().count().saturating_sub(1) as i64;
    for c in target.components() {
        match c {
            Component::Normal(_) => depth += 1,
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            Component::CurDir => {}
            _ => return false,
        }
    }
    true
}

/// Unpack `archive` of `kind` into `dest` (made if missing). `strip` drops the first path
/// component of every entry, as `tar --strip-components=1` does.
pub fn extract(kind: Kind, archive: &Path, dest: &Path, strip: bool) -> Result<()> {
    fs::create_dir_all(dest)?;
    let file = File::open(archive)?;
    match kind {
        Kind::TarGz => untar(flate2::read::GzDecoder::new(file), dest, strip),
        Kind::TarZst => {
            let dec = ruzstd::decoding::StreamingDecoder::new(io::BufReader::new(file))
                .map_err(|e| bad(format!("zstd: {e}")))?;
            untar(dec, dest, strip)
        }
        Kind::Zip => unzip(file, dest, strip),
    }
}

fn untar<R: Read>(reader: R, dest: &Path, strip: bool) -> Result<()> {
    let mut ar = tar::Archive::new(reader);
    ar.set_preserve_permissions(true);
    ar.set_unpack_xattrs(false);
    for entry in ar.entries().map_err(bad)? {
        let mut entry = entry.map_err(bad)?;
        let path = entry.path().map_err(bad)?.into_owned();
        let Some(rel) = safe_rel(&path, strip)? else {
            continue;
        };
        let kind = entry.header().entry_type();
        let out = dest.join(&rel);
        if kind.is_symlink() || kind.is_hard_link() {
            let Some(target) = entry.link_name().map_err(bad)?.map(|t| t.into_owned()) else {
                continue;
            };
            if kind.is_hard_link() {
                // A hard link names another entry of the archive, from its root.
                if let Ok(Some(t)) = safe_rel(&target, strip) {
                    make_parent(&out)?;
                    let _ = fs::remove_file(&out);
                    fs::hard_link(dest.join(&t), &out)
                        .or_else(|_| fs::copy(dest.join(&t), &out).map(|_| ()))?;
                }
            } else if link_stays_inside(&rel, &target) {
                make_parent(&out)?;
                let _ = fs::remove_file(&out);
                symlink(&target, &out)?;
            }
            continue;
        }
        if !(kind.is_file() || kind.is_dir()) {
            continue;
        }
        make_parent(&out)?;
        entry.unpack(&out).map_err(bad)?;
    }
    Ok(())
}

fn make_parent(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) => fs::create_dir_all(parent),
        None => Ok(()),
    }
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    // Symlinks need a privilege on Windows; a copy of the target is what a build needs.
    let src = link.parent().unwrap_or_else(|| Path::new(".")).join(target);
    fs::copy(src, link).map(|_| ())
}

fn entry_name<R: Read>(entry: &zip::read::ZipFile<'_, R>) -> Result<String> {
    entry
        .name()
        .map(|n| n.into_owned())
        .map_err(|e| bad(format!("zip: {e}")))
}

fn unzip(file: File, dest: &Path, strip: bool) -> Result<()> {
    let mut z = zip::ZipArchive::new(file).map_err(|e| bad(format!("zip: {e}")))?;
    let top = if strip {
        match z.len() {
            0 => None,
            _ => {
                let first = z.by_index_raw(0).map_err(|e| bad(format!("zip: {e}")))?;
                let name = entry_name(&first)?;
                name.split('/').next().map(|s| format!("{s}/"))
            }
        }
    } else {
        None
    };
    for i in 0..z.len() {
        let mut entry = z.by_index(i).map_err(|e| bad(format!("zip: {e}")))?;
        let full = entry
            .name()
            .map_err(|e| bad(format!("zip: {e}")))?
            .into_owned();
        let name = match &top {
            Some(t) if full.starts_with(t.as_str()) => &full[t.len()..],
            _ => full.as_str(),
        };
        if name.is_empty() {
            continue;
        }
        if name.starts_with('/') || name.split('/').any(|p| p == "..") || name.contains('\\') {
            return Err(bad(format!("unsafe path {full}")));
        }
        let out = dest.join(name);
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
            continue;
        }
        make_parent(&out)?;
        let mut f = File::create(&out)?;
        io::copy(&mut entry, &mut f)?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&out, fs::Permissions::from_mode(mode & 0o777))?;
        }
    }
    Ok(())
}

/// A name from a mod archive that a Windows or Unix filesystem takes as is.
fn mod_name_ok(n: &str) -> bool {
    !n.is_empty()
        && n != "."
        && n != ".."
        && !n.ends_with([' ', '.'])
        && !n.chars().any(|c| {
            matches!(c, '\\' | '/' | ':' | '<' | '>' | '"' | '|' | '?' | '*') || (c as u32) < 0x20
        })
}

/// Unpack the `romfs/` folder of a fan-mod zip into `ready` (so `ready/romfs/...`), where the
/// folder may sit under one top folder. Refuses unsafe names and an archive with no RomFS files.
pub fn extract_mod_romfs(zpath: &Path, ready: &Path) -> Result<()> {
    let file = File::open(zpath)?;
    let mut z = zip::ZipArchive::new(file).map_err(|e| bad(format!("zip: {e}")))?;
    let names: Vec<String> = (0..z.len())
        .map(|i| {
            z.by_index_raw(i)
                .and_then(|e| e.name().map(|n| n.into_owned()))
        })
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| bad(format!("zip: {e}")))?;
    for path in &names {
        let trimmed = path.trim_end_matches('/');
        if !trimmed.is_empty() && !trimmed.split('/').all(mod_name_ok) {
            return Err(bad("unsafe path in the zip"));
        }
    }
    let mut prefix = String::new();
    if !names.iter().any(|p| p.starts_with("romfs/")) {
        let mut tops: Vec<&str> = names
            .iter()
            .filter(|p| p.contains("/romfs/"))
            .filter_map(|p| p.split('/').next())
            .collect();
        tops.sort_unstable();
        tops.dedup();
        if tops.len() != 1 {
            return Err(bad("no romfs/ folder in the zip"));
        }
        prefix = format!("{}/", tops[0]);
    }
    let wanted = format!("{prefix}romfs/");
    for i in 0..z.len() {
        let mut entry = z.by_index(i).map_err(|e| bad(format!("zip: {e}")))?;
        if entry.is_dir() || !entry_name(&entry)?.starts_with(&wanted) {
            continue;
        }
        let out = ready.join(&entry_name(&entry)?[prefix.len()..]);
        make_parent(&out)?;
        let mut f = File::create(&out)?;
        io::copy(&mut entry, &mut f)?;
    }
    if !ready.join("romfs").is_dir() {
        return Err(bad("the mod archive contains no RomFS files"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            b.append_data(&mut h, name, *data).unwrap();
        }
        let raw = b.into_inner().unwrap();
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        e.write_all(&raw).unwrap();
        e.finish().unwrap()
    }

    fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for (name, data) in entries {
            w.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn tar_gz_strips_the_top_folder() {
        let dir = tempfile::tempdir().unwrap();
        let ar = dir.path().join("a.tar.gz");
        fs::write(&ar, tar_gz(&[("top/x/y.txt", b"hi"), ("top/z", b"z")])).unwrap();
        let out = dir.path().join("out");
        extract(Kind::of("a.tar.gz"), &ar, &out, true).unwrap();
        assert_eq!(fs::read(out.join("x/y.txt")).unwrap(), b"hi");
        assert_eq!(fs::read(out.join("z")).unwrap(), b"z");
        let out2 = dir.path().join("out2");
        extract(Kind::TarGz, &ar, &out2, false).unwrap();
        assert!(out2.join("top/z").is_file());
    }

    #[test]
    fn tar_refuses_a_path_that_leaves_the_destination() {
        let mut raw = vec![0u8; 512];
        // A header written by hand: tar::Builder refuses `..` itself.
        raw[..8].copy_from_slice(b"../evil\0");
        raw[100..108].copy_from_slice(b"0000644\0");
        raw[124..136].copy_from_slice(b"00000000002\0");
        raw[156] = b'0';
        raw[257..263].copy_from_slice(b"ustar\0");
        raw[148..156].copy_from_slice(b"        ");
        let sum: u32 = raw.iter().map(|b| u32::from(*b)).sum();
        raw[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        raw.extend_from_slice(b"hi");
        raw.resize(1024, 0);
        raw.extend_from_slice(&[0u8; 1024]);
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        e.write_all(&raw).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ar = dir.path().join("a.tar.gz");
        fs::write(&ar, e.finish().unwrap()).unwrap();
        let out = dir.path().join("out");
        assert!(extract(Kind::TarGz, &ar, &out, false).is_err());
        assert!(!dir.path().join("evil").exists());
    }

    #[test]
    fn tar_zst_unpacks() {
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(2);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, "top/f.txt", &b"ok"[..]).unwrap();
        let raw = b.into_inner().unwrap();
        let zst = ruzstd::encoding::compress_to_vec(
            &raw[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        let dir = tempfile::tempdir().unwrap();
        let ar = dir.path().join("a.tar.zst");
        fs::write(&ar, zst).unwrap();
        let out = dir.path().join("out");
        extract(Kind::of("a.tar.zst"), &ar, &out, true).unwrap();
        assert_eq!(fs::read(out.join("f.txt")).unwrap(), b"ok");
    }

    #[test]
    fn zip_strips_and_skips_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let ar = dir.path().join("a.zip");
        fs::write(&ar, make_zip(&[("top/a.txt", b"a"), ("top/d/b.txt", b"b")])).unwrap();
        let out = dir.path().join("out");
        extract(Kind::of("a.zip"), &ar, &out, true).unwrap();
        assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"a");
        assert_eq!(fs::read(out.join("d/b.txt")).unwrap(), b"b");
    }

    #[test]
    fn link_check_is_lexical() {
        assert!(link_stays_inside(Path::new("a/b"), Path::new("../c")));
        assert!(!link_stays_inside(Path::new("a/b"), Path::new("../../c")));
        assert!(!link_stays_inside(Path::new("a"), Path::new("/etc/passwd")));
    }

    #[test]
    fn mod_zip_finds_romfs_under_one_top_folder() {
        let dir = tempfile::tempdir().unwrap();
        let ar = dir.path().join("m.zip");
        fs::write(
            &ar,
            make_zip(&[("Mod/romfs/a/b.bin", b"1"), ("Mod/readme.txt", b"r")]),
        )
        .unwrap();
        let ready = dir.path().join("ready");
        fs::create_dir_all(&ready).unwrap();
        extract_mod_romfs(&ar, &ready).unwrap();
        assert_eq!(fs::read(ready.join("romfs/a/b.bin")).unwrap(), b"1");
        assert!(!ready.join("readme.txt").exists());
    }

    #[test]
    fn mod_zip_refuses_unsafe_names_and_missing_romfs() {
        let dir = tempfile::tempdir().unwrap();
        let ready = dir.path().join("ready");
        fs::create_dir_all(&ready).unwrap();
        for (name, entries) in [
            ("bad.zip", vec![("romfs/../x", &b"1"[..])]),
            ("colon.zip", vec![("romfs/a:b", &b"1"[..])]),
            ("none.zip", vec![("readme.txt", &b"1"[..])]),
        ] {
            let ar = dir.path().join(name);
            fs::write(&ar, make_zip(&entries)).unwrap();
            assert!(extract_mod_romfs(&ar, &ready).is_err(), "{name}");
        }
    }
}
