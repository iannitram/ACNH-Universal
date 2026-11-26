//! Split from the old `saves.rs` (issue #356).

#[allow(unused_imports)]
use super::*;

// ---- backups --------------------------------------------------------------------------------

pub(crate) fn backup_name(n: &str) -> bool {
    let b = n.as_bytes();
    if b.len() < 15 || b[8] != b'-' || n.contains(".tmp") {
        return false;
    }
    b[..15]
        .iter()
        .enumerate()
        .all(|(i, &c)| i == 8 || c.is_ascii_digit())
}

pub(crate) fn auto_backup(n: &str) -> bool {
    n.len() > 5 && n.ends_with(".auto")
}

pub(crate) fn parse_suffix(n: &str) -> i32 {
    if n.len() <= 16 {
        return 0;
    }
    let rest = &n[16..];
    let digits_end = rest
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..digits_end].parse::<i32>().unwrap_or(0)
}

pub(crate) fn backups_sorted(dir: &Path) -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                    if backup_name(name) {
                        v.push(name.to_string());
                    }
                }
            }
        }
    }
    v.sort_by(|a, b| {
        let ka = (&a[..15], parse_suffix(a));
        let kb = (&b[..15], parse_suffix(b));
        kb.cmp(&ka)
    });
    v
}

pub(crate) fn migrate_flat_backups(data_root: &str) {
    let root = backups_root(data_root);
    let mut flat = Vec::new();
    if let Ok(rd) = fs::read_dir(&root) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                    if backup_name(name) {
                        flat.push(p);
                    }
                }
            }
        }
    }
    for p in flat {
        let Some(fname) = p.file_name() else {
            continue;
        };
        let dest = island_dir_for(data_root, &p).join(fname);
        if let Some(parent) = dest.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if !dest.exists() && fs::rename(&p, &dest).is_err() {
            host_log(&format!(
                "saves: cannot move the backup {} into its island folder",
                path_utf8(&p)
            ));
        }
    }
}

pub(crate) fn backup_locked(data_root: &str, err: &mut String, automatic: bool) -> String {
    let dir = device_dir(data_root);
    if !non_empty_dir(&dir) {
        set_err(err, "no save to back up");
        return String::new();
    }
    let bdir = backups_dir(data_root);
    let _ = fs::create_dir_all(&bdir);
    let base = stamp_now();
    let mut next = 0i32;
    for n in backups_sorted(&bdir) {
        if n.starts_with(&base) {
            let candidate = if n.len() > 16 && n.as_bytes()[15] == b'-' {
                parse_suffix(&n)
            } else {
                1
            };
            next = next.max(candidate);
        }
    }
    let stem = if next > 0 {
        format!("{base}-{}", next + 1)
    } else {
        base
    };
    let final_path = unique_in(&bdir, &stem, if automatic { ".auto" } else { "" });
    let tmp = PathBuf::from(format!("{}.tmp", path_utf8(&final_path)));
    let _ = fs::remove_dir_all(&tmp);
    if !copy_tree_inner(&dir, &tmp, err) {
        let _ = fs::remove_dir_all(&tmp);
        return String::new();
    }
    if let Err(e) = fs::rename(&tmp, &final_path) {
        let _ = fs::remove_dir_all(&tmp);
        set_err(err, format!("cannot finish the backup: {e}"));
        return String::new();
    }
    let name = format!(
        "{}/{}",
        bdir.file_name().and_then(|s| s.to_str()).unwrap_or(""),
        final_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
    );
    let mut kept = 0usize;
    let limit = if automatic {
        KEEP_AUTO_BACKUPS
    } else {
        KEEP_BACKUPS
    };
    for n in backups_sorted(&bdir) {
        if auto_backup(&n) == automatic {
            kept += 1;
            if kept > limit {
                let _ = fs::remove_dir_all(bdir.join(&n));
            }
        }
    }
    if let Ok(rd) = fs::read_dir(&bdir) {
        for entry in rd.flatten() {
            if let Some(fname) = entry.file_name().to_str() {
                if fname.contains(".tmp") {
                    let _ = fs::remove_dir_all(entry.path());
                }
            }
        }
    }
    host_log(&format!("saves: backup {name}"));
    name
}

pub(crate) fn all_backups(data_root: &str) -> Vec<String> {
    migrate_flat_backups(data_root);
    let mut dirs: Vec<PathBuf> = subdirs(&backups_root(data_root))
        .into_iter()
        .filter(|d| {
            d.file_name()
                .and_then(|s| s.to_str())
                .map(|n| !backup_name(n))
                .unwrap_or(false)
        })
        .collect();
    let current = backups_dir(data_root);
    dirs.sort_by_key(|d| usize::from(*d != current));
    let mut v = Vec::new();
    for d in dirs {
        let folder = d.file_name().and_then(|s| s.to_str()).unwrap_or("");
        for n in backups_sorted(&d) {
            v.push(format!("{folder}/{n}"));
        }
    }
    v
}

pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

pub(crate) fn import_locked(data_root: &str, from: &Path, err: &mut String) -> bool {
    recover(data_root);
    let staging = PathBuf::from(format!("{}.new", path_utf8(&device_dir(data_root))));
    let _ = fs::create_dir_all(user_dir(data_root));
    let _ = fs::remove_dir_all(&staging);
    if from.is_file() {
        let Some(entries) = zip_list(from, err) else {
            return false;
        };
        let Some(prefix) = zip_find_root(&entries, err) else {
            return false;
        };
        if !zip_extract(from, &entries, &prefix, &staging, err) {
            let _ = fs::remove_dir_all(&staging);
            return false;
        }
    } else {
        let Some(root) = find_root(from, err) else {
            return false;
        };
        if same_path(&root, &device_dir(data_root)) {
            set_err(err, "this is the current save");
            return false;
        }
        if !copy_tree_inner(&root, &staging, err) {
            let _ = fs::remove_dir_all(&staging);
            return false;
        }
    }
    if !flat_save(&staging, Some(err)) {
        let _ = fs::remove_dir_all(&staging);
        return false;
    }
    if non_empty_dir(&device_dir(data_root)) {
        let mut berr = String::new();
        if backup_locked(data_root, &mut berr, false).is_empty() {
            let _ = fs::remove_dir_all(&staging);
            set_err(err, format!("cannot back up the current save: {berr}"));
            return false;
        }
    }
    if !swap_in(data_root, &staging, err) {
        let _ = fs::remove_dir_all(&staging);
        return false;
    }
    host_log(&format!("saves: imported {}", path_utf8(from)));
    true
}
