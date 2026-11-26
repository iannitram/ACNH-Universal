//! Split from the old `saves.rs` (issue #356).

#[allow(unused_imports)]
use super::*;

// ---- detection ------------------------------------------------------------------------------

pub(crate) struct Scan {
    pub(crate) out: Vec<(String, String)>,
    pub(crate) seen: BTreeSet<String>,
    pub(crate) own: Option<PathBuf>,
}

impl Scan {
    pub(crate) fn add(&mut self, label: &str, p: &Path) {
        let c = fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        if let Some(own) = &self.own {
            if c == *own {
                return;
            }
        }
        if !self.seen.insert(path_utf8(&c)) {
            return;
        }
        self.out.push((label.to_string(), path_utf8(p)));
    }
}

pub(crate) fn scan_yuzu(s: &mut Scan, root: &Path, label: &str) {
    let save = root.join("nand").join("user").join("save");
    if !save.is_dir() {
        return;
    }
    let titles = [TITLE_ID_UPPER, TITLE_ID_LOWER];
    for t in titles {
        let p = save.join("device").join(t).join("0");
        if flat_save(&p, None) {
            s.add(label, &p);
        }
    }
    let zero_user = save
        .join("0000000000000000")
        .join("00000000000000000000000000000000");
    for t in titles {
        let p = zero_user.join(t);
        if flat_save(&p, None) {
            s.add(label, &p);
        }
    }
    for user in subdirs(&save.join("0000000000000000")) {
        for t in titles {
            let p = user.join(t);
            if flat_save(&p, None) {
                s.add(label, &p);
            }
        }
    }
}

pub(crate) fn scan_ryujinx(s: &mut Scan, root: &Path, label: &str) {
    let save = root.join("bis").join("user").join("save");
    for d in subdirs(&save) {
        if d.file_name().and_then(|n| n.to_str()).map(|n| n.len()) != Some(16) {
            continue;
        }
        let p0 = d.join("0");
        let p1 = d.join("1");
        if flat_save(&p0, None) {
            s.add(label, &p0);
        } else if flat_save(&p1, None) {
            s.add(label, &p1);
        }
    }
}

pub(crate) fn ends_with_zip(s: &str) -> bool {
    s.len() >= 4 && s[s.len() - 4..].eq_ignore_ascii_case(".zip")
}

pub(crate) fn zip_is_save(zip: &Path) -> bool {
    let mut err = String::new();
    let Some(entries) = zip_list(zip, &mut err) else {
        return false;
    };
    zip_find_root(&entries, &mut err).is_some()
}

pub(crate) fn mtime(p: &Path) -> SystemTime {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .unwrap_or(UNIX_EPOCH)
}

pub(crate) fn add_newest_backup(s: &mut Scan, title_dir: &Path, label: &str) {
    let mut best: Option<PathBuf> = None;
    let mut best_time = UNIX_EPOCH;
    let mut consider = |p: &Path| -> bool {
        let t = if p.is_dir() && flat_save(p, None) {
            mtime(&p.join("main.dat"))
        } else if p.is_file()
            && p.file_name()
                .and_then(|n| n.to_str())
                .map(ends_with_zip)
                .unwrap_or(false)
            && zip_is_save(p)
        {
            mtime(p)
        } else {
            return false;
        };
        if best.is_none() || t > best_time {
            best = Some(p.to_path_buf());
            best_time = t;
        }
        true
    };
    if let Ok(rd) = fs::read_dir(title_dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if consider(&p) || !p.is_dir() {
                continue;
            }
            if let Ok(rd2) = fs::read_dir(&p) {
                for e2 in rd2.flatten() {
                    consider(&e2.path());
                }
            }
        }
    }
    if let Some(b) = best {
        s.add(label, &b);
    }
}

pub(crate) fn scan_volume(s: &mut Scan, v: &Path, name: &str) {
    for t in subdirs(&v.join("JKSV")) {
        let n = t
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if n.contains("animal crossing") || n.contains(TITLE_ID_LOWER) {
            add_newest_backup(s, &t, &format!("JKSV ({name})"));
        }
    }
    let cp_prefix = format!("0x{TITLE_ID_LOWER}");
    for t in subdirs(&v.join("switch").join("Checkpoint").join("saves")) {
        let n = t
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if n.starts_with(&cp_prefix) {
            add_newest_backup(s, &t, &format!("Checkpoint ({name})"));
        }
    }
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    std::env::var(key)
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

pub(crate) fn volumes() -> Vec<PathBuf> {
    let mut v = Vec::new();
    #[cfg(windows)]
    {
        for d in b'D'..=b'Z' {
            let p = PathBuf::from(format!("{}:\\", d as char));
            if p.is_dir() {
                v.push(p);
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        v.extend(subdirs(Path::new("/Volumes")));
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let user = std::env::var("USER").unwrap_or_default();
        if !user.is_empty() {
            v.extend(subdirs(&PathBuf::from(format!("/media/{user}"))));
            v.extend(subdirs(&PathBuf::from(format!("/run/media/{user}"))));
        }
        for base in ["/media", "/mnt"] {
            for p in subdirs(Path::new(base)) {
                if p.file_name().and_then(|s| s.to_str()) != Some(user.as_str()) {
                    v.push(p);
                }
            }
        }
    }
    v
}

pub(crate) fn scan_emu_base(s: &mut Scan, base: &Path, suffix: &str) {
    const EMUS: [(&str, &str, bool); 7] = [
        ("yuzu", "Yuzu", false),
        ("suyu", "Suyu", false),
        ("sudachi", "Sudachi", false),
        ("citron", "Citron", false),
        ("eden", "Eden", false),
        ("torzu", "Torzu", false),
        ("Ryujinx", "Ryujinx", true),
    ];
    for (folder, label, ryujinx) in EMUS {
        let root = base.join(folder);
        if !root.is_dir() {
            continue;
        }
        let full_label = format!("{label}{suffix}");
        if ryujinx {
            scan_ryujinx(s, &root, &full_label);
        } else {
            scan_yuzu(s, &root, &full_label);
        }
    }
}

pub(crate) fn scan_recomps(s: &mut Scan) {
    let Some(h) = home_dir() else {
        return;
    };
    for sub in [
        "",
        "Downloads",
        "Desktop",
        "Documents",
        "Games",
        "Applications",
        "Projects",
    ] {
        let base = if sub.is_empty() {
            h.clone()
        } else {
            h.join(sub)
        };
        for d in subdirs(&base) {
            let fname = d.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !fname.to_ascii_lowercase().contains("acnh") {
                continue;
            }
            let p = d
                .join("private")
                .join("acnh")
                .join("user")
                .join("device_save");
            if flat_save(&p, None) {
                s.add(&format!("ACNHRecomp ({fname})"), &p);
            }
        }
    }
}

// ---- users ----------------------------------------------------------------------------------

pub(crate) fn users_path(data_root: &str) -> PathBuf {
    PathBuf::from(data_root).join("users.txt")
}

pub(crate) fn hex_uid(lo: u64, hi: u64) -> String {
    format!("{hi:016x}{lo:016x}")
}

pub(crate) fn save_users_locked(data_root: &str, users: &[UserRecord]) {
    let mut out = String::new();
    for u in users {
        out.push_str(&format!("{} {}\n", hex_uid(u.lo, u.hi), u.nickname));
    }
    let _ = fs::write(users_path(data_root), out);
}

pub(crate) fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

pub(crate) fn default_user_name() -> String {
    for key in ["NX_USER_NAME", "USER", "USERNAME"] {
        if let Ok(v) = std::env::var(key) {
            if !v.is_empty() {
                return truncate_utf8(&v, 10);
            }
        }
    }
    String::from("Player")
}

pub(crate) fn load_users_locked(data_root: &str, st: &mut UsersState) {
    if st.loaded {
        return;
    }
    st.loaded = true;
    let path = users_path(data_root);
    if let Ok(contents) = fs::read_to_string(&path) {
        for raw_line in contents.lines() {
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            if line.len() < 33 || line.as_bytes()[32] != b' ' {
                continue;
            }
            let Ok(hi) = u64::from_str_radix(&line[..16], 16) else {
                continue;
            };
            let Ok(lo) = u64::from_str_radix(&line[16..32], 16) else {
                continue;
            };
            let nickname = truncate_utf8(&line[33..], 31);
            let rec = UserRecord { lo, hi, nickname };
            if rec.valid() && st.users.len() < ACNH_MAX_USERS {
                st.users.push(rec);
            }
        }
    }
    if st.users.is_empty() {
        st.users.push(UserRecord {
            lo: ACNH_FIRST_USER_LO,
            hi: ACNH_FIRST_USER_HI,
            nickname: default_user_name(),
        });
        save_users_locked(data_root, &st.users);
        host_log(&format!(
            "users: created {} with the first user",
            path_utf8(&path)
        ));
    }
    for u in &st.users {
        host_log(&format!("users: {} {}", hex_uid(u.lo, u.hi), u.nickname));
    }
}

pub(crate) fn users_all_vec() -> Vec<UserRecord> {
    let data_root = host_data_root();
    let mut st = users_lock().lock().unwrap_or_else(|e| e.into_inner());
    load_users_locked(&data_root, &mut st);
    st.users.clone()
}

pub(crate) fn user_last_opened_rec() -> UserRecord {
    let all = users_all_vec();
    let st = users_lock().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(last) = &st.last {
        return last.clone();
    }
    if let Ok(e) = std::env::var("NX_USER") {
        if let Ok(n) = e.trim().parse::<usize>() {
            if n >= 1 && n <= all.len() {
                return all[n - 1].clone();
            }
        }
    }
    all.into_iter().next().unwrap_or(UserRecord {
        lo: ACNH_FIRST_USER_LO,
        hi: ACNH_FIRST_USER_HI,
        nickname: String::from("Player"),
    })
}
