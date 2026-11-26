//! Split from the old `files.rs` (issue #356).

#[allow(unused_imports)]
use super::*;

// ---- FFI exports: lifecycle & host -------------------------------------------------------------

/// Installs the host callback table from `runtime/src/files_wrapper.cpp`.
///
/// # Safety
/// `host` must be null or point to a valid `AcnhFilesHost`.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_set_host(host: *const AcnhFilesHost) {
    if host.is_null() {
        return;
    }
    // SAFETY: caller guarantees `host` is valid for reads.
    let h = unsafe { *host };
    *host_lock().lock().unwrap_or_else(|e| e.into_inner()) = h;
}

/// Logs the recent file/directory opens ring buffer (safe from crash handlers via `try_lock`).
#[no_mangle]
pub extern "C" fn acnh_files_log_recent() {
    let Ok(st) = recent_lock().try_lock() else {
        return;
    };
    host_log("  last file opens (newest last; thread = host thread id):");
    for k in 0..RECENT_OPS {
        let idx = (st.seq as usize + k) % RECENT_OPS;
        let r = &st.ring[idx];
        if r.seq != 0 {
            host_log(&format!("    #{} thread {}  {}", r.seq, r.thread, r.text));
        }
    }
}

/// Registers an in-memory virtual file served under any mount matching `path`.
///
/// # Safety
/// When `len > 0` and `data` is non-null, `data` must be valid for `len` bytes of reads.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_add_virtual_file(
    path: *const c_char,
    data: *const u8,
    len: usize,
) {
    let Some(p) = cstr_opt(path) else {
        return;
    };
    let bytes = if data.is_null() || len == 0 {
        Vec::new()
    } else {
        // SAFETY: caller guarantees `data` is valid for `len` bytes.
        unsafe { std::slice::from_raw_parts(data, len) }.to_vec()
    };
    let key = virtual_key(p);
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    st.virtual_files.insert(key, Arc::new(bytes));
}

/// Reads a file relative to the RomFS root into `cb` (empty if missing).
#[no_mangle]
pub extern "C" fn acnh_files_read_game_file(
    relative_to_romfs: *const c_char,
    cb: Option<extern "C" fn(*const u8, usize, *mut c_void)>,
    ctx: *mut c_void,
) {
    let Some(cb) = cb else {
        return;
    };
    let rel = cstr_lossy(relative_to_romfs);
    let rel = rel.trim_start_matches(['/', '\\']);
    let full = romfs_root().join(rel);
    if let Ok(bytes) = fs::read(&full) {
        cb(bytes.as_ptr(), bytes.len(), ctx);
    } else {
        cb(std::ptr::null(), 0, ctx);
    }
}

// ---- FFI exports: mounts -----------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn acnh_files_mount_rom(name: *const c_char) -> u32 {
    let n = cstr_lossy(name);
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    mount(&mut st, &n, romfs_root());
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_ensure_save_data(uid_lo: u64, uid_hi: u64) -> u32 {
    saves::acnh_user_opened(uid_lo, uid_hi);
    let dir = data_root()
        .join("user")
        .join(saves::user_save_dir_name(uid_lo, uid_hi));
    let _ = fs::create_dir_all(&dir);
    host_log(&format!("fs: EnsureSaveData -> {}", dir.display()));
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_mount_save_data(name: *const c_char, uid_lo: u64, uid_hi: u64) -> u32 {
    let n = cstr_lossy(name);
    saves::acnh_user_opened(uid_lo, uid_hi);
    let dir = data_root()
        .join("user")
        .join(saves::user_save_dir_name(uid_lo, uid_hi));
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    mount(&mut st, &n, dir);
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_unmount(name: *const c_char) -> u32 {
    let n = cstr_lossy(name);
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    st.mounts.remove(&n);
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_commit(names: *const c_char) -> u32 {
    let n = cstr_lossy(names);
    host_log(&format!("fs: save: Commit({n})"));
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_mount_device_save_data(name: *const c_char) -> u32 {
    let n = cstr_lossy(name);
    let dir = data_root().join("user").join("device_save");
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    mount(&mut st, &n, dir);
    FS_OK
}

/// Writes device save data size and journal size.
///
/// # Safety
/// Non-null `out_size` and `out_journal` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_get_device_save_data_size(
    out_size: *mut i64,
    out_journal: *mut i64,
) -> u32 {
    let mut size = DEFAULT_DEVICE_SAVE_SIZE;
    let mut journal = DEFAULT_DEVICE_SAVE_SIZE;
    let size_file = data_root().join("user").join("device_save.size");
    if let Ok(text) = fs::read_to_string(&size_file) {
        let mut nums = text.split_whitespace();
        if let (Some(a), Some(b)) = (nums.next(), nums.next()) {
            if let (Ok(sa), Ok(sb)) = (a.parse::<i64>(), b.parse::<i64>()) {
                size = sa;
                journal = sb;
            }
        }
    }
    if !out_size.is_null() {
        // SAFETY: caller guarantees `out_size` is valid for writes.
        unsafe { *out_size = size };
    }
    if !out_journal.is_null() {
        // SAFETY: caller guarantees `out_journal` is valid for writes.
        unsafe { *out_journal = journal };
    }
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_extend_device_save_data(size: i64, journal: i64) -> u32 {
    let user_dir = data_root().join("user");
    let _ = fs::create_dir_all(&user_dir);
    let _ = fs::write(
        user_dir.join("device_save.size"),
        format!("{size} {journal}\n"),
    );
    host_log(&format!(
        "fs: ExtendDeviceSaveData({size:#x}, {journal:#x})"
    ));
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_mount_cache_storage(name: *const c_char, index: i32) -> u32 {
    let n = cstr_lossy(name);
    let dir = data_root().join("user").join(format!("cache{index}"));
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    mount(&mut st, &n, dir);
    FS_OK
}

// ---- FFI exports: files & directories ----------------------------------------------------------

#[no_mangle]
pub extern "C" fn acnh_files_create_file(path: *const c_char, size: i64) -> u32 {
    let guest = cstr_lossy(path);
    let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = resolve(&st, &guest) else {
        host_log(&format!(
            "fs: save: CreateFile({guest}, {size}): no parent directory"
        ));
        return FS_PATH_NOT_FOUND;
    };
    let parent_ok = p.parent().is_some_and(|parent| parent.exists());
    if !parent_ok {
        host_log(&format!(
            "fs: save: CreateFile({guest}, {size}): no parent directory"
        ));
        return FS_PATH_NOT_FOUND;
    }
    if p.exists() {
        return FS_PATH_ALREADY_EXISTS;
    }
    let ok = match File::create(&p) {
        Ok(mut f) => {
            if size > 0 {
                f.seek(SeekFrom::Start((size - 1) as u64)).is_ok() && f.write_all(&[0u8]).is_ok()
            } else {
                true
            }
        }
        Err(_) => false,
    };
    host_log(&format!(
        "fs: save: CreateFile({guest}, {size}){}",
        if ok { "" } else { " failed on the host" }
    ));
    if ok {
        FS_OK
    } else {
        FS_PATH_NOT_FOUND
    }
}

#[no_mangle]
pub extern "C" fn acnh_files_delete_file(path: *const c_char) -> u32 {
    let guest = cstr_lossy(path);
    let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let ok = resolve(&st, &guest).is_some_and(|p| fs::remove_file(p).is_ok());
    host_log(&format!(
        "fs: save: DeleteFile({guest}){}",
        if ok { "" } else { ": not found" }
    ));
    if ok {
        FS_OK
    } else {
        FS_PATH_NOT_FOUND
    }
}

#[no_mangle]
pub extern "C" fn acnh_files_create_directory(path: *const c_char) -> u32 {
    let guest = cstr_lossy(path);
    let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = resolve(&st, &guest) else {
        return FS_PATH_NOT_FOUND;
    };
    if p.exists() {
        return FS_PATH_ALREADY_EXISTS;
    }
    let ok = fs::create_dir(&p).is_ok();
    host_log(&format!(
        "fs: save: CreateDirectory({guest}){}",
        if ok { "" } else { ": failed" }
    ));
    if ok {
        FS_OK
    } else {
        FS_PATH_NOT_FOUND
    }
}

/// Resolves `path` and writes its `DirectoryEntryType` (`0` dir, `1` file) to `out_type`.
///
/// # Safety
/// When non-null, `out_type` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_get_entry_type(path: *const c_char, out_type: *mut u32) -> u32 {
    let guest = cstr_lossy(path);
    let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = resolve(&st, &guest) else {
        return FS_PATH_NOT_FOUND;
    };
    if find_virtual(&st, &p).is_some() {
        if !out_type.is_null() {
            // SAFETY: caller guarantees `out_type` is valid.
            unsafe { *out_type = ENTRY_TYPE_FILE };
        }
        return FS_OK;
    }
    if let Some((k, rel)) = in_pak(&p) {
        let is_file = k.files.contains_key(&rel);
        let is_dir = rel.is_empty() || k.dirs.contains_key(&rel);
        if is_file || is_dir {
            if !out_type.is_null() {
                // SAFETY: caller guarantees `out_type` is valid.
                unsafe {
                    *out_type = if is_dir {
                        ENTRY_TYPE_DIR
                    } else {
                        ENTRY_TYPE_FILE
                    }
                };
            }
            return FS_OK;
        }
        return FS_PATH_NOT_FOUND;
    }
    if !p.exists() {
        return FS_PATH_NOT_FOUND;
    }
    if !out_type.is_null() {
        // SAFETY: caller guarantees `out_type` is valid.
        unsafe {
            *out_type = if p.is_dir() {
                ENTRY_TYPE_DIR
            } else {
                ENTRY_TYPE_FILE
            }
        };
    }
    FS_OK
}

/// Opens a file at `path` with `mode` (`1` read, `2` write, `4` append) and writes handle to `out_handle`.
///
/// # Safety
/// When non-null, `out_handle` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_open_file(
    path: *const c_char,
    mode: i32,
    out_handle: *mut u64,
) -> u32 {
    let guest = cstr_lossy(path);
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let resolved = resolve(&st, &guest);
    let Some(p) = resolved else {
        note_open("OpenFile", &guest, "unknown mount");
        if trace_fs() || (mode & 2) != 0 {
            host_log(&format!("fs: OpenFile({guest}, {mode}) not found"));
        }
        return FS_PATH_NOT_FOUND;
    };

    if let Some(v) = find_virtual(&st, &p) {
        let h = st.next_handle;
        st.next_handle += 1;
        let len = v.len();
        st.files.insert(
            h,
            Arc::new(Mutex::new(OpenFileEntry {
                path: guest.clone(),
                backing: FileBacking::Virtual { data: v },
            })),
        );
        if !out_handle.is_null() {
            // SAFETY: caller guarantees `out_handle` is valid.
            unsafe { *out_handle = h };
        }
        note_open("OpenFile", &guest, "virtual");
        if trace_fs() {
            host_log(&format!("fs: OpenFile({guest}) -> virtual, {len} bytes"));
        }
        return FS_OK;
    }

    if let Some((k, rel)) = in_pak(&p) {
        let Some(&(off, size)) = k.files.get(&rel) else {
            note_open("OpenFile", &guest, "not found");
            if trace_fs() {
                host_log(&format!("fs: OpenFile({guest}) not in romfs.pak"));
            }
            return FS_PATH_NOT_FOUND;
        };
        if (mode & 2) != 0 {
            note_open("OpenFile", &guest, "not found");
            if trace_fs() {
                host_log(&format!("fs: OpenFile({guest}) not in romfs.pak"));
            }
            return FS_PATH_NOT_FOUND;
        }
        let h = st.next_handle;
        st.next_handle += 1;
        st.files.insert(
            h,
            Arc::new(Mutex::new(OpenFileEntry {
                path: guest.clone(),
                backing: FileBacking::Pak {
                    file: Arc::clone(&k.file),
                    base: off,
                    size,
                },
            })),
        );
        if !out_handle.is_null() {
            // SAFETY: caller guarantees `out_handle` is valid.
            unsafe { *out_handle = h };
        }
        note_open("OpenFile", &guest, "ok");
        if trace_fs() {
            host_log(&format!("fs: OpenFile({guest}, {mode}) -> {h} (packed)"));
        }
        return FS_OK;
    }

    if !p.is_file() {
        note_open("OpenFile", &guest, "not found");
        if trace_fs() || (mode & 2) != 0 {
            host_log(&format!("fs: OpenFile({guest}, {mode}) not found"));
        }
        note_lost_file(&mut st, &p);
        return FS_PATH_NOT_FOUND;
    }

    let open_res = if (mode & 2) != 0 {
        OpenOptions::new().read(true).write(true).open(&p)
    } else {
        File::open(&p)
    };
    let file = match open_res {
        Ok(f) => f,
        Err(err) => {
            note_open("OpenFile", &guest, "the host could not open it");
            if trace_fs() {
                host_log(&format!(
                    "fs: OpenFile({guest}) failed to open on the host ({err})"
                ));
            }
            return FS_PATH_NOT_FOUND;
        }
    };
    let h = st.next_handle;
    st.next_handle += 1;
    st.files.insert(
        h,
        Arc::new(Mutex::new(OpenFileEntry {
            path: guest.clone(),
            backing: FileBacking::Host { file, written: 0 },
        })),
    );
    if !out_handle.is_null() {
        // SAFETY: caller guarantees `out_handle` is valid.
        unsafe { *out_handle = h };
    }
    note_open("OpenFile", &guest, "ok");
    if trace_fs() {
        host_log(&format!("fs: OpenFile({guest}, {mode}) -> {h}"));
    }
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_close_file(handle: u64) {
    let removed = {
        let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
        st.files.remove(&handle)
    };
    if let Some(entry) = removed {
        let guard = entry.lock().unwrap_or_else(|e| e.into_inner());
        if let FileBacking::Host { written, .. } = &guard.backing {
            if *written > 0 {
                host_log(&format!(
                    "fs: save: wrote {written} bytes to {}",
                    guard.path
                ));
            }
        }
    }
}

/// Writes the file size to `out_size`.
///
/// # Safety
/// When non-null, `out_size` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_get_file_size(handle: u64, out_size: *mut i64) -> u32 {
    let entry = {
        let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
        st.files.get(&handle).cloned()
    };
    let Some(entry) = entry else {
        return FS_TARGET_NOT_FOUND;
    };
    let guard = entry.lock().unwrap_or_else(|e| e.into_inner());
    let size = match &guard.backing {
        FileBacking::Pak { size, .. } => *size,
        FileBacking::Virtual { data } => data.len() as i64,
        FileBacking::Host { file, .. } => {
            let sz = file.metadata().map(|m| m.len() as i64).unwrap_or(0);
            if host_trace_global() {
                host_log(&format!("fs: size {} = {sz}", guard.path));
            }
            sz
        }
    };
    if !out_size.is_null() {
        // SAFETY: caller guarantees `out_size` is valid.
        unsafe { *out_size = size };
    }
    FS_OK
}

/// Reads `size` bytes at `off` from `handle` into `buf`, optionally writing bytes read to `out_read`.
///
/// # Safety
/// `buf` must be valid for `size` bytes of writes when `size > 0`, and `out_read` when non-null.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_read_file(
    handle: u64,
    off: i64,
    buf: *mut u8,
    size: u64,
    out_read: *mut u64,
) -> u32 {
    let entry = {
        let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
        st.files.get(&handle).cloned()
    };
    let Some(entry) = entry else {
        return FS_TARGET_NOT_FOUND;
    };

    // Snapshot the underlying handle/slice so positional reads run without holding any lock.
    enum ReadSource {
        Host(File, String),
        Pak(Arc<File>, i64, i64, String),
        Virtual(Arc<Vec<u8>>, String),
    }

    let src = {
        let guard = entry.lock().unwrap_or_else(|e| e.into_inner());
        match &guard.backing {
            FileBacking::Host { file, .. } => match file.try_clone() {
                Ok(cloned) => ReadSource::Host(cloned, guard.path.clone()),
                Err(_) => return FS_PATH_NOT_FOUND,
            },
            FileBacking::Pak { file, base, size } => {
                ReadSource::Pak(Arc::clone(file), *base, *size, guard.path.clone())
            }
            FileBacking::Virtual { data } => {
                ReadSource::Virtual(Arc::clone(data), guard.path.clone())
            }
        }
    };

    let t0 = host_monotonic_ns();
    let (n, path, effective_off) = if size == 0 || buf.is_null() || off < 0 {
        let p = match &src {
            ReadSource::Host(_, p) | ReadSource::Pak(_, _, _, p) | ReadSource::Virtual(_, p) => {
                p.clone()
            }
        };
        (0u64, p, off)
    } else {
        // SAFETY: caller guarantees `buf` is valid for `size` bytes of writes.
        let dst = unsafe { std::slice::from_raw_parts_mut(buf, size as usize) };
        match src {
            ReadSource::Virtual(data, p) => {
                let start = off as usize;
                let n = if start >= data.len() {
                    0
                } else {
                    let avail = (data.len() - start).min(dst.len());
                    dst[..avail].copy_from_slice(&data[start..start + avail]);
                    avail as u64
                };
                (n, p, off)
            }
            ReadSource::Pak(file, base, file_size, p) => {
                if off >= file_size {
                    (0, p, off)
                } else {
                    let max_read = ((file_size - off) as u64).min(size) as usize;
                    let eff = base + off;
                    let mut total = 0usize;
                    while total < max_read {
                        match read_at_file(
                            &file,
                            (eff as u64) + total as u64,
                            &mut dst[total..max_read],
                        ) {
                            Ok(0) | Err(_) => break,
                            Ok(k) => total += k,
                        }
                    }
                    (total as u64, p, eff)
                }
            }
            ReadSource::Host(file, p) => {
                let mut total = 0usize;
                while total < dst.len() {
                    match read_at_file(&file, (off as u64) + total as u64, &mut dst[total..]) {
                        Ok(0) | Err(_) => break,
                        Ok(k) => total += k,
                    }
                }
                (total as u64, p, off)
            }
        }
    };

    let ms = host_monotonic_ns().saturating_sub(t0) / 1_000_000;
    if host_trace_global() {
        host_log(&format!(
            "fs: read {path} at {effective_off} size {size} -> {n}"
        ));
    }
    if trace_fs() && path.contains(".bwav") {
        let mut map = stream_reads_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let k = map.entry(path.clone()).or_insert(0);
        *k += 1;
        let count = *k;
        if count <= 3 || count.is_multiple_of(50) {
            host_log(&format!(
                "fs: stream read #{count} {path} at {effective_off}"
            ));
        }
    }
    if ms >= 30 {
        host_log(&format!(
            "fs: slow read {path}: {size} bytes at {effective_off} took {ms} ms"
        ));
    }
    if !out_read.is_null() {
        // SAFETY: caller guarantees `out_read` is valid.
        unsafe { *out_read = n };
    }
    FS_OK
}

/// Writes `size` bytes from `buf` at `off` into `handle`.
///
/// # Safety
/// `buf` must be valid for `size` bytes of reads when `size > 0`.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_write_file(
    handle: u64,
    off: i64,
    buf: *const u8,
    size: u64,
) -> u32 {
    let entry = {
        let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
        st.files.get(&handle).cloned()
    };
    let Some(entry) = entry else {
        return FS_TARGET_NOT_FOUND;
    };
    let mut guard = entry.lock().unwrap_or_else(|e| e.into_inner());
    let path = guard.path.clone();
    let FileBacking::Host { file, written } = &mut guard.backing else {
        return FS_PATH_NOT_FOUND;
    };
    if off < 0 {
        return FS_PATH_NOT_FOUND;
    }
    if file.seek(SeekFrom::Start(off as u64)).is_err() {
        return FS_PATH_NOT_FOUND;
    }
    if size > 0 && !buf.is_null() {
        // SAFETY: caller guarantees `buf` is valid for `size` bytes of reads.
        let src = unsafe { std::slice::from_raw_parts(buf, size as usize) };
        match file.write(src) {
            Ok(n) => {
                let _ = file.flush();
                *written += n as u64;
                if (n as u64) != size {
                    host_log(&format!(
                        "fs: save: WriteFile({path}) wrote {n} of {size} bytes"
                    ));
                }
            }
            Err(e) => {
                host_log(&format!(
                    "fs: save: WriteFile({path}) wrote 0 of {size} bytes ({e})"
                ));
            }
        }
    }
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_flush_file(handle: u64) -> u32 {
    let entry = {
        let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
        st.files.get(&handle).cloned()
    };
    if let Some(entry) = entry {
        let mut guard = entry.lock().unwrap_or_else(|e| e.into_inner());
        if let FileBacking::Host { file, .. } = &mut guard.backing {
            let _ = file.flush();
        }
    }
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_set_file_size(handle: u64, size: i64) -> u32 {
    let entry = {
        let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
        st.files.get(&handle).cloned()
    };
    let Some(entry) = entry else {
        return FS_TARGET_NOT_FOUND;
    };
    let mut guard = entry.lock().unwrap_or_else(|e| e.into_inner());
    let FileBacking::Host { file, .. } = &mut guard.backing else {
        return FS_PATH_NOT_FOUND;
    };
    if size < 0 {
        return FS_PATH_NOT_FOUND;
    }
    let _ = file.flush();
    if file.set_len(size as u64).is_ok() {
        FS_OK
    } else {
        FS_PATH_NOT_FOUND
    }
}

/// Opens a directory at `path` with `mode` (`1` dirs, `2` files) and writes handle to `out_handle`.
///
/// # Safety
/// When non-null, `out_handle` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_open_directory(
    path: *const c_char,
    mode: i32,
    out_handle: *mut u64,
) -> u32 {
    let guest = cstr_lossy(path);
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = resolve(&st, &guest) else {
        note_open("OpenDirectory", &guest, "not found");
        if trace_fs() {
            host_log(&format!("fs: OpenDirectory({guest}, {mode}) not found"));
        }
        return FS_PATH_NOT_FOUND;
    };

    if let Some((k, rel)) = in_pak(&p) {
        let Some(entries) = k.dirs.get(&rel) else {
            note_open("OpenDirectory", &guest, "not found");
            return FS_PATH_NOT_FOUND;
        };
        note_open("OpenDirectory", &guest, "ok");
        let filtered: Vec<DirEntry> = entries
            .iter()
            .filter(|e| (e.dir && (mode & 1) != 0) || (!e.dir && (mode & 2) != 0))
            .cloned()
            .collect();
        let h = st.next_handle;
        st.next_handle += 1;
        st.dirs.insert(
            h,
            DirHandle {
                entries: filtered,
                pos: 0,
            },
        );
        if !out_handle.is_null() {
            // SAFETY: caller guarantees `out_handle` is valid.
            unsafe { *out_handle = h };
        }
        return FS_OK;
    }

    let found = p.is_dir();
    note_open(
        "OpenDirectory",
        &guest,
        if found { "ok" } else { "not found" },
    );
    if trace_fs() {
        host_log(&format!(
            "fs: OpenDirectory({guest}, {mode}){}",
            if found { "" } else { " not found" }
        ));
    }
    if !found {
        return FS_PATH_NOT_FOUND;
    }

    let mut entries = Vec::new();
    if let Ok(rd) = fs::read_dir(&p) {
        for item in rd.flatten() {
            let meta = match item.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let is_dir = meta.is_dir();
            if (is_dir && (mode & 1) != 0) || (!is_dir && (mode & 2) != 0) {
                entries.push(DirEntry {
                    name: item.file_name().to_string_lossy().into_owned(),
                    dir: is_dir,
                    size: if is_dir { 0 } else { meta.len() as i64 },
                });
            }
        }
    }

    let h = st.next_handle;
    st.next_handle += 1;
    st.dirs.insert(h, DirHandle { entries, pos: 0 });
    if !out_handle.is_null() {
        // SAFETY: caller guarantees `out_handle` is valid.
        unsafe { *out_handle = h };
    }
    FS_OK
}

/// Reads up to `max_entries` from directory `handle` via `cb`, writing actual count to `out_count`.
///
/// # Safety
/// When non-null, `out_count` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_read_directory(
    handle: u64,
    max_entries: i64,
    out_count: *mut i64,
    cb: Option<extern "C" fn(*const c_char, usize, bool, i64, *mut c_void)>,
    ctx: *mut c_void,
) -> u32 {
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(dir) = st.dirs.get_mut(&handle) else {
        return FS_TARGET_NOT_FOUND;
    };
    let mut n = 0i64;
    while n < max_entries && dir.pos < dir.entries.len() {
        let e = &dir.entries[dir.pos];
        if let Some(cb) = cb {
            let sanitized = e.name.replace('\0', "");
            if let Ok(c) = CString::new(sanitized.as_str()) {
                cb(c.as_ptr(), sanitized.len(), e.dir, e.size, ctx);
            }
        }
        dir.pos += 1;
        n += 1;
    }
    if !out_count.is_null() {
        // SAFETY: caller guarantees `out_count` is valid.
        unsafe { *out_count = n };
    }
    FS_OK
}

/// Writes total entry count for directory `handle` to `out_count`.
///
/// # Safety
/// When non-null, `out_count` must be valid for writes.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_get_directory_entry_count(
    handle: u64,
    out_count: *mut i64,
) -> u32 {
    let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let count = st
        .dirs
        .get(&handle)
        .map(|d| d.entries.len() as i64)
        .unwrap_or(0);
    if !out_count.is_null() {
        // SAFETY: caller guarantees `out_count` is valid.
        unsafe { *out_count = count };
    }
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_close_directory(handle: u64) {
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    st.dirs.remove(&handle);
}

#[no_mangle]
pub extern "C" fn acnh_files_clean_directory_recursively(path: *const c_char) -> u32 {
    let guest = cstr_lossy(path);
    let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = resolve(&st, &guest) else {
        return FS_PATH_NOT_FOUND;
    };
    if !p.is_dir() {
        return FS_PATH_NOT_FOUND;
    }
    let Ok(rd) = fs::read_dir(&p) else {
        return FS_PATH_NOT_FOUND;
    };
    for entry in rd.flatten() {
        let ep = entry.path();
        let res = if ep.is_dir() {
            fs::remove_dir_all(&ep)
        } else {
            fs::remove_file(&ep)
        };
        if res.is_err() {
            return FS_PATH_NOT_FOUND;
        }
    }
    FS_OK
}

#[no_mangle]
pub extern "C" fn acnh_files_rename_directory(from: *const c_char, to: *const c_char) -> u32 {
    let guest_from = cstr_lossy(from);
    let guest_to = cstr_lossy(to);
    let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let (Some(a), Some(b)) = (resolve(&st, &guest_from), resolve(&st, &guest_to)) else {
        return FS_PATH_NOT_FOUND;
    };
    if !a.is_dir() {
        return FS_PATH_NOT_FOUND;
    }
    if b.exists() {
        return FS_PATH_ALREADY_EXISTS;
    }
    let ok = fs::rename(&a, &b).is_ok();
    host_log(&format!(
        "fs: save: RenameDirectory({guest_from}, {guest_to}){}",
        if ok { "" } else { ": failed" }
    ));
    if ok {
        FS_OK
    } else {
        FS_PATH_NOT_FOUND
    }
}

#[no_mangle]
pub extern "C" fn acnh_files_delete_directory_recursively(path: *const c_char) -> u32 {
    let guest = cstr_lossy(path);
    let st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = resolve(&st, &guest) else {
        return FS_PATH_NOT_FOUND;
    };
    if !p.is_dir() {
        return FS_PATH_NOT_FOUND;
    }
    if fs::remove_dir_all(&p).is_ok() {
        FS_OK
    } else {
        FS_PATH_NOT_FOUND
    }
}
