//! Split from the old `files.rs` (issue #356).

#[allow(unused_imports)]
use super::*;

// ---- FFI exports: add-on content (DLC) ---------------------------------------------------------

#[no_mangle]
pub extern "C" fn acnh_files_count_aoc() -> u64 {
    let v = aoc_indexes_vec();
    if AOC_LOGGED
        .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        let mut list = String::new();
        for idx in &v {
            list.push_str(&format!(" {idx}"));
        }
        host_log(&format!("aoc: {} add-on content(s):{list}", v.len()));
    }
    v.len() as u64
}

/// Writes up to `count` AOC indexes starting at `offset` into `out`.
///
/// # Safety
/// `out` must be valid for `count` `i32` writes when `count > 0`.
#[no_mangle]
pub unsafe extern "C" fn acnh_files_list_aoc(out: *mut i32, offset: i32, count: i32) -> i32 {
    if out.is_null() || count <= 0 {
        return 0;
    }
    let v = aoc_indexes_vec();
    let start = offset.max(0) as usize;
    let mut k = 0i32;
    for &val in v.iter().skip(start).take(count as usize) {
        // SAFETY: caller guarantees `out` has capacity for `count` i32s.
        unsafe { *out.add(k as usize) = val };
        k += 1;
    }
    k
}

#[no_mangle]
pub extern "C" fn acnh_files_mount_aoc(name: *const c_char, index: i32) -> u32 {
    let n = cstr_lossy(name);
    let p = game_root().join("aoc").join(index.to_string());
    if !p.is_dir() {
        host_log(&format!("aoc: MountAddOnContent({index}): not imported"));
        return FS_TARGET_NOT_FOUND;
    }
    let mut st = fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    host_log(&format!(
        "fs: mounted {n}: -> {} (add-on {index})",
        p.display()
    ));
    st.mounts.insert(n, p);
    FS_OK
}

// ---- FFI exports: mod_fetch --------------------------------------------------------------------

pub(crate) fn bundle_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let canon = fs::canonicalize(&exe).unwrap_or(exe);
    let dir = canon.parent()?;
    if dir.file_name().and_then(|n| n.to_str()) == Some("MacOS")
        && dir
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            == Some("Contents")
    {
        return Some(dir.parent()?.join("Resources"));
    }
    Some(dir.to_path_buf())
}

pub(crate) fn read_bundled_mod_file(rel: &str) -> Option<Vec<u8>> {
    if let Some(bytes) = host_read_asset(&format!("mods/{rel}")) {
        return Some(bytes);
    }
    let dir = bundle_dir()?;
    fs::read(dir.join("mods").join(rel)).ok()
}

pub(crate) fn first_line(s: &str) -> String {
    s.lines()
        .next()
        .unwrap_or("")
        .trim_end_matches('\r')
        .to_string()
}

/// Copies bundled SpeedFix parameters into `<data_root>/mods` when opted in (`mod_fetch.cpp`).
#[no_mangle]
pub extern "C" fn acnh_files_mods_fetch() {
    if let Ok(e) = std::env::var("NX_MOD_FETCH") {
        if e.starts_with('0') {
            return;
        }
    }
    if config::config_get("game.mod_fetch", "on") == "off" {
        return;
    }
    let Some(list_bytes) = read_bundled_mod_file("bundle.txt") else {
        return;
    };
    let list = String::from_utf8_lossy(&list_bytes);
    let version = first_line(&list);
    if version.is_empty() {
        return;
    }
    let mods = data_root().join("mods");
    let marker = mods.join(".bundle-speedfix");
    if let Ok(existing) = fs::read_to_string(&marker) {
        if first_line(&existing) == version {
            return;
        }
    }
    let files: Vec<&str> = list
        .lines()
        .skip(1)
        .map(|l| l.trim_end_matches('\r'))
        .filter(|rel| rel.starts_with("speedfix60/") && !rel.contains(".."))
        .collect();
    if files.is_empty() {
        return;
    }
    let mut top_dirs = BTreeSet::new();
    for rel in &files {
        if let Some((top, _)) = rel.split_once('/') {
            top_dirs.insert(top);
        }
    }
    for top in top_dirs {
        let _ = fs::remove_dir_all(mods.join(top));
    }

    let mut copied = 0usize;
    let mut bytes = 0usize;
    for rel in &files {
        let Some(data) = read_bundled_mod_file(rel) else {
            host_log(&format!(
                "mods: {rel} could not be copied into {}",
                mods.display()
            ));
            continue;
        };
        let dst = mods.join(rel);
        let wrote = dst.parent().is_some_and(|p| fs::create_dir_all(p).is_ok())
            && fs::write(&dst, &data).is_ok();
        if !wrote {
            host_log(&format!(
                "mods: {rel} could not be copied into {}",
                mods.display()
            ));
            continue;
        }
        copied += 1;
        bytes += data.len();
    }
    let ok = copied == files.len();
    if ok {
        let _ = fs::create_dir_all(&mods);
        let _ = fs::write(&marker, format!("{version}\n"));
    }
    host_log(&format!(
        "mods: the package's mods ({version}) copied into {}: {copied} of {} files, {} KB{}",
        mods.display(),
        files.len(),
        bytes >> 10,
        if ok {
            ""
        } else {
            " (the rest: at the next start)"
        }
    ));
}

// ---- FFI exports: game_folder helpers ----------------------------------------------------------

/// Builds the platform-specific game shared library path under `folder`.
pub fn game_library_path_for_os(folder: &str, game_id: &str, os: &str) -> String {
    match os {
        "windows" => format!("{folder}/game/windows-x64/{game_id}_game.dll"),
        "macos" => format!("{folder}/game/macos-arm64/lib{game_id}_game.dylib"),
        _ => format!("{folder}/game/linux-x64/lib{game_id}_game.so"),
    }
}

/// Formats the user-facing explanation shown when `load_game` fails.
pub fn format_load_game_error(why: &str) -> String {
    if why.starts_with("abi") {
        format!(
            "This game folder was prepared for another version of ACNHRecomp. Open the ACNHRecomp \
             launcher and choose Prepare again on its home page: the game's code is built again \
             for this version, the rest is kept.\n\n{why}"
        )
    } else {
        format!(
            "This is not a game folder prepared for this computer. Open the ACNHRecomp launcher \
             and prepare your game there, from the ExeFS and RomFS you extracted from your own \
             copy with your emulator (a folder prepared before: Prepare again, with this \
             computer ticked).\n\n{why}"
        )
    }
}

#[no_mangle]
pub extern "C" fn acnh_files_game_library_path(
    folder: *const c_char,
    game_id: *const c_char,
    cb: Option<extern "C" fn(*const c_char, usize, *mut c_void)>,
    ctx: *mut c_void,
) {
    let f = cstr_lossy(folder);
    let id = cstr_lossy(game_id);
    let os = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let path = game_library_path_for_os(&f, &id, os);
    emit_string(cb, ctx, &path);
}

#[no_mangle]
pub extern "C" fn acnh_files_format_load_error(
    why: *const c_char,
    cb: Option<extern "C" fn(*const c_char, usize, *mut c_void)>,
    ctx: *mut c_void,
) {
    let w = cstr_lossy(why);
    let msg = format_load_game_error(&w);
    emit_string(cb, ctx, &msg);
}
