//! Split from the old `saves.rs` (issue #356).

#[allow(unused_imports)]
use super::*;

// ---- zip ------------------------------------------------------------------------------------

pub(crate) fn rd16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

pub(crate) fn rd32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

pub(crate) fn wr16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub(crate) fn wr32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub(crate) fn crc32_of(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if (c & 1) != 0 {
                0xEDB8_8320u32 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    c ^ 0xFFFF_FFFFu32
}

pub(crate) struct ZipEntry {
    pub(crate) name: String,
    pub(crate) flags: u16,
    pub(crate) method: u16,
    pub(crate) crc: u32,
    pub(crate) csize: u32,
    pub(crate) usize: u32,
    pub(crate) offset: u32,
}

pub(crate) fn safe_name(raw: &str) -> Option<String> {
    let normalized = raw.replace('\\', "/");
    if normalized.is_empty() || normalized.starts_with('/') || normalized.contains(':') {
        return None;
    }
    if normalized.split('/').any(|seg| seg == "..") {
        return None;
    }
    Some(normalized)
}

pub(crate) fn read_exact_at(f: &mut File, off: u64, len: usize) -> Option<Vec<u8>> {
    f.seek(SeekFrom::Start(off)).ok()?;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

pub(crate) fn zip_list(zip: &Path, err: &mut String) -> Option<Vec<ZipEntry>> {
    let mut f = match File::open(zip) {
        Ok(f) => f,
        Err(_) => {
            set_err(err, format!("cannot open {}", path_utf8(zip)));
            return None;
        }
    };
    let size = match f.seek(SeekFrom::End(0)) {
        Ok(s) => s,
        Err(_) => {
            set_err(err, "cannot read the zip file");
            return None;
        }
    };
    if size < 22 {
        set_err(err, "not a zip file");
        return None;
    }
    let tail = size.min(22 + 0xFFFF) as usize;
    let Some(buf) = read_exact_at(&mut f, size - tail as u64, tail) else {
        set_err(err, "cannot read the zip file");
        return None;
    };
    let mut eocd = None;
    for i in (0..=tail - 22).rev() {
        if rd32(&buf[i..]) == 0x0605_4b50 {
            eocd = Some(i);
            break;
        }
    }
    let Some(eocd) = eocd else {
        set_err(err, "not a zip file");
        return None;
    };
    let count = rd16(&buf[eocd + 10..]);
    let cd_size = rd32(&buf[eocd + 12..]);
    let cd_off = rd32(&buf[eocd + 16..]);
    if count == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_off == 0xFFFF_FFFF {
        set_err(err, "zip64 not supported");
        return None;
    }
    if u64::from(cd_off) + u64::from(cd_size) > size {
        set_err(err, "damaged zip file");
        return None;
    }
    let Some(cd) = read_exact_at(&mut f, u64::from(cd_off), cd_size as usize) else {
        set_err(err, "cannot read the zip file");
        return None;
    };
    let mut pos = 0usize;
    let mut out = Vec::new();
    for _ in 0..count {
        if pos + 46 > cd.len() || rd32(&cd[pos..]) != 0x0201_4b50 {
            set_err(err, "damaged zip file");
            return None;
        }
        let flags = rd16(&cd[pos + 8..]);
        let method = rd16(&cd[pos + 10..]);
        let crc = rd32(&cd[pos + 16..]);
        let csize = rd32(&cd[pos + 20..]);
        let usize_val = rd32(&cd[pos + 24..]);
        let nlen = usize::from(rd16(&cd[pos + 28..]));
        let xlen = usize::from(rd16(&cd[pos + 30..]));
        let clen = usize::from(rd16(&cd[pos + 32..]));
        let offset = rd32(&cd[pos + 42..]);
        if pos + 46 + nlen + xlen + clen > cd.len() {
            set_err(err, "damaged zip file");
            return None;
        }
        let raw_name = String::from_utf8_lossy(&cd[pos + 46..pos + 46 + nlen]).into_owned();
        pos += 46 + nlen + xlen + clen;
        let Some(name) = safe_name(&raw_name) else {
            set_err(err, format!("unsafe path in zip: {raw_name}"));
            return None;
        };
        if name.ends_with('/') || name.starts_with("__MACOSX/") {
            continue;
        }
        out.push(ZipEntry {
            name,
            flags,
            method,
            crc,
            csize,
            usize: usize_val,
            offset,
        });
    }
    Some(out)
}

pub(crate) fn zip_find_root(entries: &[ZipEntry], why: &mut String) -> Option<String> {
    let names: BTreeSet<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    let mut found = Vec::new();
    let mut any_main = false;
    for &n in &names {
        let Some(p) = n.strip_suffix("main.dat") else {
            continue;
        };
        if !p.is_empty() && !p.ends_with('/') {
            continue;
        }
        if p.bytes().filter(|&b| b == b'/').count() > 2 {
            continue;
        }
        any_main = true;
        let hdr = format!("{p}mainHeader.dat");
        let vil = format!("{p}Villager0/personal.dat");
        if names.contains(hdr.as_str()) && names.contains(vil.as_str()) {
            found.push(p.to_string());
        }
    }
    if found.is_empty() {
        if !any_main {
            set_err(why, "main.dat missing");
        } else {
            set_err(why, "mainHeader.dat or Villager0/personal.dat missing");
        }
        return None;
    }
    if found.len() > 1 {
        set_err(
            why,
            format!("{} saves in this zip: it must hold one", found.len()),
        );
        return None;
    }
    Some(found.remove(0))
}

pub(crate) fn zip_inflate_host(
    compressed: &[u8],
    out_len: usize,
    err: &mut String,
) -> Option<Vec<u8>> {
    let Some(inflate_fn) = current_host().zip_inflate else {
        set_err(err, "zip not supported in this build");
        return None;
    };
    let mut plain = vec![0u8; out_len];
    let mut err_buf = vec![0u8; 256];
    let ok = inflate_fn(
        compressed.as_ptr(),
        compressed.len(),
        plain.as_mut_ptr(),
        plain.len(),
        err_buf.as_mut_ptr().cast::<c_char>(),
        err_buf.len(),
    );
    if !ok {
        let nul = err_buf
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(err_buf.len());
        let msg = String::from_utf8_lossy(&err_buf[..nul]).into_owned();
        set_err(
            err,
            if msg.is_empty() {
                String::from("damaged zip file (inflate)")
            } else {
                msg
            },
        );
        return None;
    }
    Some(plain)
}

pub(crate) fn zip_extract(
    zip: &Path,
    entries: &[ZipEntry],
    prefix: &str,
    dest: &Path,
    err: &mut String,
) -> bool {
    let mut f = match File::open(zip) {
        Ok(f) => f,
        Err(_) => {
            set_err(err, format!("cannot open {}", path_utf8(zip)));
            return false;
        }
    };
    let mut total = 0u64;
    for e in entries {
        let Some(rel) = e.name.strip_prefix(prefix) else {
            continue;
        };
        if (e.flags & 1) != 0 {
            set_err(err, "encrypted zip entries are not supported");
            return false;
        }
        if e.method != 0 && e.method != 8 {
            set_err(err, format!("unsupported zip compression in {}", e.name));
            return false;
        }
        total += u64::from(e.usize);
        if total > MAX_ZIP_BYTES {
            set_err(err, "zip too large for a save");
            return false;
        }
        let Some(head) = read_exact_at(&mut f, u64::from(e.offset), 30) else {
            set_err(err, "damaged zip file");
            return false;
        };
        if rd32(&head) != 0x0403_4b50 {
            set_err(err, "damaged zip file");
            return false;
        }
        let at =
            u64::from(e.offset) + 30 + u64::from(rd16(&head[26..])) + u64::from(rd16(&head[28..]));
        let Some(data) = read_exact_at(&mut f, at, e.csize as usize) else {
            set_err(err, "damaged zip file");
            return false;
        };
        let plain = if e.method == 0 {
            if e.csize != e.usize {
                set_err(err, "damaged zip file");
                return false;
            }
            data
        } else {
            let Some(p) = zip_inflate_host(&data, e.usize as usize, err) else {
                return false;
            };
            p
        };
        if crc32_of(&plain) != e.crc {
            set_err(
                err,
                format!("damaged zip file (CRC mismatch in {})", e.name),
            );
            return false;
        }
        let out = dest.join(rel);
        if let Some(parent) = out.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if fs::write(&out, &plain).is_err() {
            set_err(err, format!("cannot write {}", path_utf8(&out)));
            return false;
        }
    }
    true
}

pub(crate) fn collect_files_sorted(
    root: &Path,
    cur: &Path,
    out: &mut Vec<PathBuf>,
    err: &mut String,
) -> bool {
    let entries = match fs::read_dir(cur) {
        Ok(rd) => rd,
        Err(e) => {
            set_err(err, format!("cannot read {}: {e}", path_utf8(root)));
            return false;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                set_err(err, format!("cannot read {}: {e}", path_utf8(root)));
                return false;
            }
        };
        let p = entry.path();
        if p.is_dir() {
            if !collect_files_sorted(root, &p, out, err) {
                return false;
            }
        } else if p.is_file() {
            out.push(p);
        }
    }
    true
}

pub(crate) fn zip_write(src: &Path, zip: &Path, err: &mut String) -> bool {
    let mut files = Vec::new();
    if !collect_files_sorted(src, src, &mut files, err) {
        return false;
    }
    files.sort();

    let lt = local_time_from_posix(unix_now_seconds());
    let dos_time = ((lt.hour as u16) << 11) | ((lt.minute as u16) << 5) | ((lt.second as u16) / 2);
    let dos_date =
        (((lt.year - 1980).max(0) as u16) << 9) | ((lt.month as u16) << 5) | (lt.day as u16);

    let mut o = match File::create(zip) {
        Ok(f) => f,
        Err(_) => {
            set_err(err, format!("cannot write {}", path_utf8(zip)));
            return false;
        }
    };

    let mut central = Vec::new();
    let mut head = Vec::new();
    let mut offset = 0u64;
    for p in &files {
        let rel = match p.strip_prefix(src) {
            Ok(r) => r,
            Err(_) => {
                set_err(err, format!("cannot read {}", path_utf8(p)));
                return false;
            }
        };
        let name = rel.to_string_lossy().replace('\\', "/");
        let data = match fs::read(p) {
            Ok(d) => d,
            Err(_) => {
                set_err(err, format!("cannot read {}", path_utf8(p)));
                return false;
            }
        };
        if data.len() as u64 >= 0xFFFF_FFFF || offset >= 0xFFFF_FFFF {
            set_err(err, "save too large for a zip");
            return false;
        }
        let crc = crc32_of(&data);
        let dlen = data.len() as u32;
        let nlen = name.len() as u16;

        head.clear();
        wr32(&mut head, 0x0403_4b50);
        wr16(&mut head, 20);
        wr16(&mut head, 1 << 11);
        wr16(&mut head, 0);
        wr16(&mut head, dos_time);
        wr16(&mut head, dos_date);
        wr32(&mut head, crc);
        wr32(&mut head, dlen);
        wr32(&mut head, dlen);
        wr16(&mut head, nlen);
        wr16(&mut head, 0);

        wr32(&mut central, 0x0201_4b50);
        wr16(&mut central, 20);
        wr16(&mut central, 20);
        wr16(&mut central, 1 << 11);
        wr16(&mut central, 0);
        wr16(&mut central, dos_time);
        wr16(&mut central, dos_date);
        wr32(&mut central, crc);
        wr32(&mut central, dlen);
        wr32(&mut central, dlen);
        wr16(&mut central, nlen);
        wr16(&mut central, 0);
        wr16(&mut central, 0);
        wr16(&mut central, 0);
        wr16(&mut central, 0);
        wr32(&mut central, 0);
        wr32(&mut central, offset as u32);
        central.extend_from_slice(name.as_bytes());

        if o.write_all(&head).is_err()
            || o.write_all(name.as_bytes()).is_err()
            || o.write_all(&data).is_err()
        {
            set_err(err, format!("cannot write {}", path_utf8(zip)));
            return false;
        }
        offset += (head.len() + name.len() + data.len()) as u64;
    }
    if files.len() > 0xFFFF || offset > 0xFFFF_FFFF {
        set_err(err, "save too large for a zip");
        return false;
    }
    let mut end = Vec::new();
    let count = files.len() as u16;
    wr32(&mut end, 0x0605_4b50);
    wr16(&mut end, 0);
    wr16(&mut end, 0);
    wr16(&mut end, count);
    wr16(&mut end, count);
    wr32(&mut end, central.len() as u32);
    wr32(&mut end, offset as u32);
    wr16(&mut end, 0);

    if o.write_all(&central).is_err() || o.write_all(&end).is_err() || o.flush().is_err() {
        set_err(err, format!("cannot write {}", path_utf8(zip)));
        return false;
    }
    true
}
