//! Split from the old `saves.rs` (issue #356).

#[allow(unused_imports)]
use super::*;

// ---- save_storage equivalents ---------------------------------------------------------------

pub(crate) fn flat_save(d: &Path, why: Option<&mut String>) -> bool {
    if !d.is_dir() {
        if let Some(w) = why {
            *w = String::from("folder not found");
        }
        return false;
    }
    if !d.join("main.dat").is_file() {
        if let Some(w) = why {
            *w = String::from("main.dat missing");
        }
        return false;
    }
    if !d.join("mainHeader.dat").is_file() {
        if let Some(w) = why {
            *w = String::from("mainHeader.dat missing");
        }
        return false;
    }
    if !d.join("Villager0").join("personal.dat").is_file() {
        if let Some(w) = why {
            *w = String::from("Villager0/personal.dat missing");
        }
        return false;
    }
    true
}

pub(crate) fn non_empty_dir(d: &Path) -> bool {
    if !d.is_dir() {
        return false;
    }
    match fs::read_dir(d) {
        Ok(mut rd) => rd.next().is_some(),
        Err(_) => false,
    }
}

pub(crate) fn recover(data_root: &str) {
    let dir = device_dir(data_root);
    let old = PathBuf::from(format!("{}.old", path_utf8(&dir)));
    if !dir.exists() && old.is_dir() && fs::rename(&old, &dir).is_ok() {
        host_log(&format!(
            "save_storage: restored {} after an interrupted swap",
            path_utf8(&dir)
        ));
    }
}

pub(crate) fn swap_in(data_root: &str, staging: &Path, err: &mut String) -> bool {
    let dir = device_dir(data_root);
    let old = PathBuf::from(format!("{}.old", path_utf8(&dir)));
    let _ = fs::remove_dir_all(&old);
    let had = dir.exists();
    if had {
        if let Err(e) = fs::rename(&dir, &old) {
            set_err(err, format!("cannot move the current save aside: {e}"));
            return false;
        }
    }
    if let Err(e) = fs::rename(staging, &dir) {
        if had {
            let _ = fs::rename(&old, &dir);
        }
        set_err(err, format!("cannot move the new save in: {e}"));
        return false;
    }
    let _ = fs::remove_dir_all(&old);
    true
}

pub(crate) fn copy_tree_inner(src: &Path, dst: &Path, err: &mut String) -> bool {
    if let Err(e) = fs::create_dir_all(dst) {
        set_err(err, format!("cannot create {}: {e}", path_utf8(dst)));
        return false;
    }
    let entries = match fs::read_dir(src) {
        Ok(rd) => rd,
        Err(e) => {
            set_err(err, format!("cannot read {}: {e}", path_utf8(src)));
            return false;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                set_err(err, format!("cannot read {}: {e}", path_utf8(src)));
                return false;
            }
        };
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                set_err(err, format!("cannot copy {}: {e}", path_utf8(&path)));
                return false;
            }
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        let to = dst.join(entry.file_name());
        if meta.is_dir() {
            if !copy_tree_inner(&path, &to, err) {
                return false;
            }
        } else if meta.is_file() {
            if let Err(e) = fs::copy(&path, &to) {
                set_err(err, format!("cannot copy {}: {e}", path_utf8(&path)));
                return false;
            }
        }
    }
    true
}

pub(crate) fn island_display_name(save: &Path) -> String {
    let land = save.join("landname.dat");
    let mut f = match File::open(&land) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let mut buf = [0u8; 20];
    let n = f.read(&mut buf).unwrap_or(0) / 2;
    let mut units = Vec::with_capacity(n);
    for i in 0..n {
        let c = u16::from_le_bytes([buf[2 * i], buf[2 * i + 1]]);
        if c == 0 {
            break;
        }
        units.push(c);
    }
    String::from_utf16_lossy(&units)
}

pub(crate) fn last_modified_str(device_save: &Path) -> String {
    let meta = match fs::metadata(device_save.join("main.dat")) {
        Ok(m) => m,
        Err(_) => return String::new(),
    };
    let modified = match meta.modified() {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    let posix = match modified.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs().min(i64::MAX as u64) as i64,
        Err(e) => -(e.duration().as_secs().min(i64::MAX as u64) as i64),
    };
    let lt = local_time_from_posix(posix);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        lt.year, lt.month, lt.day, lt.hour, lt.minute
    )
}

// ---- saves helpers --------------------------------------------------------------------------

pub(crate) fn subdirs(d: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(d) else {
        return out;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            out.push(p);
        }
    }
    out.sort();
    out
}

pub(crate) fn island_folder(save: &Path) -> String {
    let mut n: String = island_display_name(save)
        .chars()
        .map(|ch| {
            if (ch as u32) < 0x20
                || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
            {
                '_'
            } else {
                ch
            }
        })
        .collect();
    while n.ends_with(' ') || n.ends_with('.') {
        n.pop();
    }
    while n.starts_with(' ') {
        n.remove(0);
    }
    if n.is_empty() {
        return String::from("Unnamed island");
    }
    const RESERVED: [&str; 22] = [
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    let lower = n.to_ascii_lowercase();
    if RESERVED.contains(&lower.as_str()) {
        return format!("{n}_");
    }
    n
}

pub(crate) fn landname_hash(save: &Path) -> u32 {
    let mut h: u32 = 2_166_136_261;
    if let Ok(bytes) = fs::read(save.join("landname.dat")) {
        for b in bytes {
            h = (h ^ u32::from(b)).wrapping_mul(16_777_619);
        }
    }
    h
}

pub(crate) fn island_dir_for(data_root: &str, save: &Path) -> PathBuf {
    backups_root(data_root).join(format!(
        "{} {:08x}",
        island_folder(save),
        landname_hash(save)
    ))
}

pub(crate) fn backups_dir(data_root: &str) -> PathBuf {
    island_dir_for(data_root, &device_dir(data_root))
}

pub(crate) fn unique_in(dir: &Path, base: &str, ext: &str) -> PathBuf {
    let mut p = dir.join(format!("{base}{ext}"));
    for n in 2..1000 {
        if !p.exists() {
            break;
        }
        p = dir.join(format!("{base}-{n}{ext}"));
    }
    p
}

pub(crate) fn find_root(d: &Path, err: &mut String) -> Option<PathBuf> {
    let mut own_why = String::new();
    if flat_save(d, Some(&mut own_why)) {
        return Some(d.to_path_buf());
    }
    if !d.is_dir() {
        set_err(err, own_why);
        return None;
    }
    let mut found = Vec::new();
    for a in subdirs(d) {
        if flat_save(&a, None) {
            found.push(a);
            continue;
        }
        for b in subdirs(&a) {
            if flat_save(&b, None) {
                found.push(b);
            }
        }
    }
    if found.len() == 2
        && found[0].file_name().and_then(|s| s.to_str()) == Some("0")
        && found[1].file_name().and_then(|s| s.to_str()) == Some("1")
        && found[0].parent() == found[1].parent()
    {
        found.pop();
    }
    if found.is_empty() {
        set_err(err, own_why);
        return None;
    }
    if found.len() > 1 {
        set_err(
            err,
            format!("{} saves in this folder: choose one of them", found.len()),
        );
        return None;
    }
    Some(found.remove(0))
}
