//! Split from the old `saves.rs` (issue #356).

#[allow(unused_imports)]
use super::*;

// ---- FFI exports ----------------------------------------------------------------------------

/// Registers the C++ host callbacks for saves and users.
///
/// # Safety
/// When non-null, `host` must point to a valid `AcnhSavesHost`.
#[no_mangle]
pub unsafe extern "C" fn acnh_saves_set_host(host: *const AcnhSavesHost) {
    let mut guard = host_lock().lock().unwrap_or_else(|e| e.into_inner());
    if host.is_null() {
        *guard = AcnhSavesHost::default();
    } else {
        // SAFETY: caller guarantees `host` points to a valid `AcnhSavesHost`.
        *guard = unsafe { *host };
        crate::ffi::set_log(guard.log);
    }
}

/// Returns `<data root>/user/device_save` via `cb`.
#[no_mangle]
pub extern "C" fn acnh_saves_save_dir(
    cb: Option<extern "C" fn(*const c_char, usize, *mut c_void)>,
    ctx: *mut c_void,
) {
    let s = path_utf8(&device_dir(&host_data_root()));
    emit_string(cb, ctx, &s);
}

/// Returns `true` when a complete ACNH device save exists.
#[no_mangle]
pub extern "C" fn acnh_saves_has_save() -> bool {
    let _lk = saves_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = host_data_root();
    recover(&root);
    flat_save(&device_dir(&root), None)
}

/// Returns `"YYYY-MM-DD HH:MM"` of `main.dat` (or `""` if missing) via `cb`.
#[no_mangle]
pub extern "C" fn acnh_saves_last_modified(
    cb: Option<extern "C" fn(*const c_char, usize, *mut c_void)>,
    ctx: *mut c_void,
) {
    let s = last_modified_str(&device_dir(&host_data_root()));
    emit_string(cb, ctx, &s);
}

/// Detects emulator, recomp, and SD-card ACNH saves, invoking `cb` for each `(label, path)`.
#[no_mangle]
pub extern "C" fn acnh_saves_detect_emulator_saves(
    cb: Option<extern "C" fn(*const c_char, usize, *const c_char, usize, *mut c_void)>,
    ctx: *mut c_void,
) {
    let root = host_data_root();
    let own = fs::canonicalize(device_dir(&root)).ok();
    let mut s = Scan {
        out: Vec::new(),
        seen: BTreeSet::new(),
        own,
    };
    #[cfg(windows)]
    {
        if let Ok(a) = std::env::var("APPDATA") {
            if !a.is_empty() {
                scan_emu_base(&mut s, Path::new(&a), "");
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(h) = home_dir() {
            scan_emu_base(&mut s, &h.join("Library").join("Application Support"), "");
        }
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let h = home_dir();
        let data = std::env::var("XDG_DATA_HOME")
            .ok()
            .filter(|v| !v.is_empty());
        let config = std::env::var("XDG_CONFIG_HOME")
            .ok()
            .filter(|v| !v.is_empty());
        if let Some(d) = data.or_else(|| h.as_ref().map(|p| path_utf8(&p.join(".local/share")))) {
            scan_emu_base(&mut s, Path::new(&d), "");
        }
        if let Some(c) = config.or_else(|| h.as_ref().map(|p| path_utf8(&p.join(".config")))) {
            scan_emu_base(&mut s, Path::new(&c), "");
        }
        if let Some(h) = h {
            for app in subdirs(&h.join(".var").join("app")) {
                scan_emu_base(&mut s, &app.join("data"), " (Flatpak)");
                scan_emu_base(&mut s, &app.join("config"), " (Flatpak)");
            }
        }
    }
    scan_recomps(&mut s);
    for v in volumes() {
        let vname = v
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_string())
            .unwrap_or_else(|| path_utf8(&v));
        scan_volume(&mut s, &v, &vname);
    }
    if let Some(cb) = cb {
        for (label, path) in &s.out {
            cb(
                label.as_ptr().cast::<c_char>(),
                label.len(),
                path.as_ptr().cast::<c_char>(),
                path.len(),
                ctx,
            );
        }
    }
}

/// Checks if `dir` (folder or `.zip`) looks like an ACNH save; writes reason to `why_cb` on failure.
#[no_mangle]
pub extern "C" fn acnh_saves_looks_like_acnh_save(
    dir: *const c_char,
    why_cb: Option<extern "C" fn(*const c_char, usize, *mut c_void)>,
    why_ctx: *mut c_void,
) -> bool {
    let Some(dir_str) = cstr_opt_lossy(dir) else {
        emit_string(why_cb, why_ctx, "folder not found");
        return false;
    };
    let p = PathBuf::from(&dir_str);
    let mut why = String::new();
    let ok = if p.is_file() {
        zip_list(&p, &mut why)
            .and_then(|entries| zip_find_root(&entries, &mut why))
            .is_some()
    } else {
        find_root(&p, &mut why).is_some()
    };
    if !ok {
        emit_string(why_cb, why_ctx, &why);
    }
    ok
}

/// Replaces the current device save with `from` (folder or `.zip`), backing up the current save first.
#[no_mangle]
pub extern "C" fn acnh_saves_import_save(
    from: *const c_char,
    err_cb: Option<extern "C" fn(*const c_char, usize, *mut c_void)>,
    err_ctx: *mut c_void,
) -> bool {
    let Some(from_str) = cstr_opt_lossy(from) else {
        emit_string(err_cb, err_ctx, "invalid path");
        return false;
    };
    let _lk = saves_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = host_data_root();
    let mut err = String::new();
    let ok = import_locked(&root, Path::new(&from_str), &mut err);
    if !ok {
        emit_string(err_cb, err_ctx, &err);
    }
    ok
}

/// Exports the current device save to `to` (a `.zip` path or a parent directory).
#[no_mangle]
pub extern "C" fn acnh_saves_export_save(
    to: *const c_char,
    written_cb: Option<extern "C" fn(*const c_char, usize, *mut c_void)>,
    written_ctx: *mut c_void,
    err_cb: Option<extern "C" fn(*const c_char, usize, *mut c_void)>,
    err_ctx: *mut c_void,
) -> bool {
    let Some(to_str) = cstr_opt_lossy(to) else {
        emit_string(err_cb, err_ctx, "invalid path");
        return false;
    };
    let _lk = saves_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = host_data_root();
    recover(&root);
    let src = device_dir(&root);
    let mut err = String::new();
    if !flat_save(&src, Some(&mut err)) {
        emit_string(err_cb, err_ctx, "no save to export");
        return false;
    }
    let mut dest = PathBuf::from(&to_str);
    if ends_with_zip(&to_str) {
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() && !parent.is_dir() {
                emit_string(
                    err_cb,
                    err_ctx,
                    &format!("folder not found: {}", path_utf8(parent)),
                );
                return false;
            }
        }
        let tmp = PathBuf::from(format!("{to_str}.tmp"));
        if !zip_write(&src, &tmp, &mut err) {
            let _ = fs::remove_file(&tmp);
            emit_string(err_cb, err_ctx, &err);
            return false;
        }
        let _ = fs::remove_file(&dest);
        if fs::rename(&tmp, &dest).is_err() {
            let _ = fs::remove_file(&tmp);
            emit_string(err_cb, err_ctx, &format!("cannot write {to_str}"));
            return false;
        }
    } else {
        let _ = fs::create_dir_all(&dest);
        if !dest.is_dir() {
            emit_string(err_cb, err_ctx, &format!("cannot create {to_str}"));
            return false;
        }
        let final_path = unique_in(&dest, &format!("ACNH-{}", stamp_now()), "");
        let tmp = PathBuf::from(format!("{}.partial", path_utf8(&final_path)));
        let _ = fs::remove_dir_all(&tmp);
        if !copy_tree_inner(&src, &tmp, &mut err) {
            let _ = fs::remove_dir_all(&tmp);
            emit_string(err_cb, err_ctx, &err);
            return false;
        }
        if fs::rename(&tmp, &final_path).is_err() {
            let _ = fs::remove_dir_all(&tmp);
            emit_string(
                err_cb,
                err_ctx,
                &format!("cannot finish {}", path_utf8(&final_path)),
            );
            return false;
        }
        dest = final_path;
    }
    let written = path_utf8(&dest);
    emit_string(written_cb, written_ctx, &written);
    host_log(&format!("saves: exported to {written}"));
    true
}

/// Creates a backup of the current device save; emits `"<island folder>/<backup>"` via `out_cb`
/// (or `""` and an error via `err_cb`).
#[no_mangle]
pub extern "C" fn acnh_saves_backup_now(
    automatic: bool,
    out_cb: Option<extern "C" fn(*const c_char, usize, *mut c_void)>,
    out_ctx: *mut c_void,
    err_cb: Option<extern "C" fn(*const c_char, usize, *mut c_void)>,
    err_ctx: *mut c_void,
) {
    let _lk = saves_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = host_data_root();
    recover(&root);
    let mut err = String::new();
    let name = backup_locked(&root, &mut err, automatic);
    if name.is_empty() && !err.is_empty() {
        emit_string(err_cb, err_ctx, &err);
    }
    emit_string(out_cb, out_ctx, &name);
}

/// Returns `<data root>/user/backups/<island folder>` via `cb`.
#[no_mangle]
pub extern "C" fn acnh_saves_backups_folder(
    cb: Option<extern "C" fn(*const c_char, usize, *mut c_void)>,
    ctx: *mut c_void,
) {
    let s = path_utf8(&backups_dir(&host_data_root()));
    emit_string(cb, ctx, &s);
}

/// Lists all backups across all islands, invoking `cb` for each `"<island folder>/<backup>"`.
#[no_mangle]
pub extern "C" fn acnh_saves_list_backups(
    cb: Option<extern "C" fn(*const c_char, usize, *mut c_void)>,
    ctx: *mut c_void,
) {
    let _lk = saves_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = host_data_root();
    for item in all_backups(&root) {
        emit_string(cb, ctx, &item);
    }
}

/// Restores a backup named `"<island folder>/<backup>"`, backing up the current save first.
#[no_mangle]
pub extern "C" fn acnh_saves_restore_backup(
    name: *const c_char,
    err_cb: Option<extern "C" fn(*const c_char, usize, *mut c_void)>,
    err_ctx: *mut c_void,
) -> bool {
    let Some(name_str) = cstr_opt_lossy(name) else {
        emit_string(err_cb, err_ctx, "invalid backup name");
        return false;
    };
    let _lk = saves_lock().lock().unwrap_or_else(|e| e.into_inner());
    let (folder, backup) = match name_str.split_once('/') {
        Some((f, b)) => (f, b),
        None => (name_str.as_str(), ""),
    };
    if folder.is_empty()
        || folder == "."
        || folder == ".."
        || backup.contains('/')
        || name_str.contains(['\\', ':'])
        || !backup_name(backup)
    {
        emit_string(err_cb, err_ctx, "invalid backup name");
        return false;
    }
    let root = host_data_root();
    let p = backups_root(&root).join(folder).join(backup);
    if !p.is_dir() {
        emit_string(err_cb, err_ctx, &format!("backup not found: {name_str}"));
        return false;
    }
    let mut err = String::new();
    let ok = import_locked(&root, &p, &mut err);
    if !ok {
        emit_string(err_cb, err_ctx, &err);
    }
    ok
}

/// Enumerates all local Switch users, invoking `cb` for each `AcnhUserInfo`.
#[no_mangle]
pub extern "C" fn acnh_users_all(
    cb: Option<extern "C" fn(*const AcnhUserInfo, *mut c_void)>,
    ctx: *mut c_void,
) {
    let users = users_all_vec();
    if let Some(cb) = cb {
        for u in &users {
            let ffi = u.to_ffi();
            cb(&ffi, ctx);
        }
    }
}

/// Returns the number of local Switch users.
#[no_mangle]
pub extern "C" fn acnh_users_count() -> i32 {
    users_all_vec().len() as i32
}

/// Finds a user by `(lo, hi)`. Writes to `out` if non-null and returns `true` when found.
///
/// # Safety
/// When non-null, `out` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_user_find(lo: u64, hi: u64, out: *mut AcnhUserInfo) -> bool {
    for u in users_all_vec() {
        if u.lo == lo && u.hi == hi {
            if !out.is_null() {
                // SAFETY: caller guarantees `out` is valid when non-null.
                unsafe { *out = u.to_ffi() };
            }
            return true;
        }
    }
    false
}

/// Creates a new user (`"Player 2"`, etc.) if fewer than 8 exist. Returns `true` on success.
///
/// # Safety
/// When non-null, `out` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_user_create(out: *mut AcnhUserInfo) -> bool {
    let data_root = host_data_root();
    let mut st = users_lock().lock().unwrap_or_else(|e| e.into_inner());
    load_users_locked(&data_root, &mut st);
    if st.users.len() >= ACNH_MAX_USERS {
        if !out.is_null() {
            // SAFETY: caller guarantees `out` is valid when non-null.
            unsafe { *out = AcnhUserInfo::default() };
        }
        return false;
    }
    let next_lo = st.users.iter().map(|u| u.lo).max().unwrap_or(0) + 1;
    let mut nickname = String::new();
    for k in (st.users.len() + 1).. {
        let candidate = format!("Player {k}");
        if !st.users.iter().any(|u| u.nickname == candidate) {
            nickname = candidate;
            break;
        }
    }
    let rec = UserRecord {
        lo: next_lo,
        hi: ACNH_FIRST_USER_HI,
        nickname,
    };
    st.users.push(rec.clone());
    save_users_locked(&data_root, &st.users);
    host_log(&format!(
        "users: created user {} {}",
        hex_uid(rec.lo, rec.hi),
        rec.nickname
    ));
    if !out.is_null() {
        // SAFETY: caller guarantees `out` is valid when non-null.
        unsafe { *out = rec.to_ffi() };
    }
    true
}

/// Returns the user the game boots into.
///
/// # Safety
/// When non-null, `out` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_user_boot(out: *mut AcnhUserInfo) {
    // SAFETY: caller guarantees `out` is valid when non-null.
    unsafe { acnh_user_last_opened(out) };
}

/// Remembers `(lo, hi)` as the last opened user if it exists.
#[no_mangle]
pub extern "C" fn acnh_user_opened(lo: u64, hi: u64) {
    let found = users_all_vec()
        .into_iter()
        .find(|u| u.lo == lo && u.hi == hi);
    if let Some(u) = found {
        let mut st = users_lock().lock().unwrap_or_else(|e| e.into_inner());
        st.last = Some(u);
    }
}

/// Returns the last opened user (or `NX_USER`, or the first user).
///
/// # Safety
/// When non-null, `out` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_user_last_opened(out: *mut AcnhUserInfo) {
    if out.is_null() {
        return;
    }
    let u = user_last_opened_rec();
    // SAFETY: caller guarantees `out` is valid.
    unsafe { *out = u.to_ffi() };
}

/// Returns `"save"` for the first user or `"save_<uid hex>"` for others.
pub fn user_save_dir_name(lo: u64, hi: u64) -> String {
    if lo == ACNH_FIRST_USER_LO && hi == ACNH_FIRST_USER_HI {
        String::from("save")
    } else {
        format!("save_{}", hex_uid(lo, hi))
    }
}

/// Returns `"save"` for the first user or `"save_<uid hex>"` for others, via `cb`.
#[no_mangle]
pub extern "C" fn acnh_user_save_dir(
    lo: u64,
    hi: u64,
    cb: Option<extern "C" fn(*const c_char, usize, *mut c_void)>,
    ctx: *mut c_void,
) {
    let s = user_save_dir_name(lo, hi);
    emit_string(cb, ctx, &s);
}

/// Writes the `0x88`-byte preselected user `IStorage` payload into `out` and returns `0x88`.
///
/// # Safety
/// When non-null, `out` must be valid for `cap` bytes of writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_account_preselected_user_blob(out: *mut u8, cap: usize) -> usize {
    const SIZE: usize = 0x88;
    let u = user_last_opened_rec();
    host_log(&format!("account: preselected user {}", u.nickname));
    if !out.is_null() && cap >= SIZE {
        // SAFETY: caller guarantees `out` is valid for `cap >= 0x88` bytes.
        let slice = unsafe { std::slice::from_raw_parts_mut(out, SIZE) };
        slice.fill(0);
        slice[0..4].copy_from_slice(&ACNH_PRESELECTED_USER_MAGIC.to_le_bytes());
        slice[4..8].copy_from_slice(&1u32.to_le_bytes());
        slice[8..16].copy_from_slice(&u.lo.to_le_bytes());
        slice[16..24].copy_from_slice(&u.hi.to_le_bytes());
    }
    SIZE
}
