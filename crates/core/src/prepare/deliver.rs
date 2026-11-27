//! The delivery of a build: the iOS, Mac and Android targets and the game folder they stage.

use super::*;

/// The folders of a delivery under the Dropbox folder: `_transfers/ACNH-Universal/<target>/`.
pub const DELIVERY_ROOT: [&str; 2] = ["_transfers", "ACNH-Universal"];

/// The delivery subfolder of `target`: "ios" for the iPhone and iPad, "mac" for the Mac,
/// "android" for Android, `None` for the others (nothing of theirs goes to Dropbox).
pub fn delivery_subfolder(target: Os) -> Option<&'static str> {
    match target {
        Os::Ios => Some("ios"),
        Os::Mac => Some("mac"),
        Os::Android => Some("android"),
        _ => None,
    }
}

/// The folders the app's in-app download reads, under save-relay's Dropbox app folder:
/// `acnh-universal-game/android/` (`GameDownload.java`).
pub const ANDROID_GAME_FOLDER: [&str; 2] = ["acnh-universal-game", "android"];

/// The local copy of save-relay's Dropbox app folder, where the Android game is staged:
/// `launcher.ini`'s `relay_app_folder` key when it is set and not empty, else
/// `<dropbox>/Apps/save-relay`. The Dropbox client uploads what lands there, and the app
/// signs in to that same app and reads only that folder.
pub fn relay_app_folder(config: &Config, home: &Path) -> PathBuf {
    match config.get("relay_app_folder") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => dropbox_folder(config, home).join("Apps").join("save-relay"),
    }
}

fn dropbox_folder(config: &Config, home: &Path) -> PathBuf {
    match config.get("dropbox") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => home.join("Dropbox"),
    }
}

/// The delivery folder for `subfolder` ("ios", "mac" or "android"). For "ios" and "mac":
/// `<dropbox>/_transfers/ACNH-Universal/<subfolder>/`, where `<dropbox>` is `launcher.ini`'s
/// `dropbox` key when it is set and not empty, else `<home>/Dropbox`. For "android": the
/// folder the app downloads from, `<relay app folder>/acnh-universal-game/android/`
/// (`relay_app_folder`).
pub fn delivery_folder(config: &Config, home: &Path, subfolder: &str) -> PathBuf {
    if subfolder == "android" {
        return ANDROID_GAME_FOLDER
            .iter()
            .fold(relay_app_folder(config, home), |path, part| path.join(part));
    }
    let dropbox = dropbox_folder(config, home);
    DELIVERY_ROOT
        .iter()
        .fold(dropbox, |path, part| path.join(part))
        .join(subfolder)
}

/// Stop with `Error::Build` unless the `_transfers/ACNH-Universal/` folder above `delivery`
/// is there. The message names the `dropbox` key of `launcher.ini` (paths through `redact`).
pub fn check_delivery(delivery: &Path, home: &Path) -> Result<()> {
    let root = delivery.parent().unwrap_or(delivery);
    if root.is_dir() {
        return Ok(());
    }
    Err(Error::Build(format!(
        "the folder {} is not there: make it in your Dropbox, or set the `dropbox` key in \
         launcher.ini to the folder that holds _transfers/ACNH-Universal/",
        redact(&root.display().to_string(), home)
    )))
}

/// Stop with `Error::Build` unless save-relay's Dropbox app folder, two above the Android
/// `delivery` (`<app folder>/acnh-universal-game/android`), is there. Android does not make
/// it: only the Dropbox client does, once the app is made there. The message names the
/// `relay_app_folder` key of `launcher.ini` (paths through `redact`).
pub fn check_android_delivery(delivery: &Path, home: &Path) -> Result<()> {
    let game = delivery.parent().unwrap_or(delivery);
    let app = game.parent().unwrap_or(game);
    if app.is_dir() {
        return Ok(());
    }
    Err(Error::Build(format!(
        "the folder {} is not there: it is the local copy of save-relay's Dropbox app folder          (Apps/save-relay in your Dropbox, which the Dropbox client makes). Set the \
         `relay_app_folder` key in launcher.ini to it",
        redact(&app.display().to_string(), home)
    )))
}

/// The arguments of `tools/build_ios.sh`: the `iphoneos` SDK, the recompiled code in `gen`,
/// and the folder the build is made in.
pub(super) fn build_ios_args(gen: &Path, build: &Path) -> Vec<String> {
    vec![
        "tools/build_ios.sh".to_owned(),
        "iphoneos".to_owned(),
        gen.display().to_string(),
        build.display().to_string(),
    ]
}

/// The arguments of `tools/package_ios.sh`: the game inside the `.ipa` comes from the
/// prepared folder `game`.
/// The bundled mods (`bundle_mods_args`) go beside the player in the app: `--mods <folder>`,
/// when they could be made (they are optional: only the 60 fps setting needs them).
pub(super) fn package_ios_args(
    version: &str,
    build: &Path,
    game: &Path,
    delivery: &Path,
    mods: Option<&Path>,
) -> Vec<String> {
    let mut args = vec![
        "tools/package_ios.sh".to_owned(),
        version.to_owned(),
        build.display().to_string(),
        "--game".to_owned(),
        game.display().to_string(),
        delivery.display().to_string(),
    ];
    if let Some(mods) = mods {
        args.extend(["--mods".to_owned(), mods.display().to_string()]);
    }
    args
}

/// The arguments of `tools/package_ios.sh --small` (issue #306): the small `.ipa`, with no game
/// inside, in `out`; the game folder and its manifest are written by `stage_ios_delivery`
/// (`--no-folder`). The bundled mods go beside the player as in the big one.
pub(super) fn package_ios_small_args(
    version: &str,
    build: &Path,
    game: &Path,
    build_id: &str,
    abi: &str,
    out: &Path,
    mods: Option<&Path>,
) -> Vec<String> {
    let mut args = vec![
        "tools/package_ios.sh".to_owned(),
        version.to_owned(),
        build.display().to_string(),
        "--small".to_owned(),
        game.display().to_string(),
        "--build-id".to_owned(),
        build_id.to_owned(),
        "--abi".to_owned(),
        abi.to_owned(),
        "--no-folder".to_owned(),
        out.display().to_string(),
    ];
    if let Some(mods) = mods {
        args.extend(["--mods".to_owned(), mods.display().to_string()]);
    }
    args
}

/// The arguments of `tools/bundle_mods.sh`: the pt-BR translation and the SpeedFix's files
/// (the 60 fps `video.frame_rate` needs them) are put in `<dest>/mods/`, which the Apple
/// packages carry beside the player, as the APK does.
///
/// The two zips are cached in `cache` (`ACNH_MODS_CACHE`), a folder of the prepared folder:
/// the installed app's source copy is replaced on every Prepare, so a cache in it would
/// download them again each time.
pub(super) fn bundle_mods_args(dest: &Path, cache: &Path) -> Vec<String> {
    vec![
        "env".to_owned(),
        format!("ACNH_MODS_CACHE={}", cache.display()),
        "tools/bundle_mods.sh".to_owned(),
        dest.display().to_string(),
    ]
}

/// Where a prepare leaves the bundled mods it made for the package: the folder
/// `bundle_mods.sh` is given, the `mods/` inside it, and the folder of its cached zips.
pub(super) fn bundled_mods_folder(out: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let dest = out.join("bundle-mods");
    let mods = dest.join("mods");
    (dest, mods, out.join("mods-cache"))
}

/// The arguments of `tools/package_mac.sh`: the contained app is the assembled `app`, with
/// the prepared folder `game` inside it.
pub(super) fn package_mac_args(
    version: &str,
    app: &Path,
    game: &Path,
    delivery: &Path,
    mods: Option<&Path>,
) -> Vec<String> {
    let mut args = vec![
        "tools/package_mac.sh".to_owned(),
        version.to_owned(),
        app.display().to_string(),
        game.display().to_string(),
        delivery.display().to_string(),
    ];
    if let Some(mods) = mods {
        args.extend(["--mods".to_owned(), mods.display().to_string()]);
    }
    args
}

/// The short build version the Apple packages are named with: `<BASE_VER>-<VCODE>` from
/// `tools/build_version.sh` (for example `0.11.0-1100891`), which the package scripts take
/// (they refuse the `+` of the long form). `base` has any `+build` suffix cut off.
pub(super) fn short_build_version(base: &str, vcode: &str) -> Option<String> {
    let base = base.split('+').next().unwrap_or(base).trim();
    let vcode = vcode.trim();
    let version = format!("{base}-{vcode}");
    let plain = |c: char| c.is_ascii_alphanumeric() || c == '.' || c == '-';
    (!base.is_empty()
        && !vcode.is_empty()
        && vcode.chars().all(|c| c.is_ascii_digit())
        && version.chars().all(plain))
    .then_some(version)
}

/// The version the Apple packages are named with. In a git checkout (`root` has a `.git`)
/// it is `tools/build_version.sh`'s, run there. A packaged source has no `.git` (the release
/// makes it with `git archive`), so the script would print 0.0.0: the release writes the
/// script's output to `BUILD_VERSION` in the source instead, and that is read. When neither
/// answers, the engine's own version.
pub(super) fn package_version(root: &Path) -> String {
    let fallback = || env!("CARGO_PKG_VERSION").to_owned();
    let text = if root.join(".git").exists() {
        let Ok(output) = std::process::Command::new("bash")
            .arg("tools/build_version.sh")
            .current_dir(root)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
        else {
            return fallback();
        };
        if !output.status.success() {
            return fallback();
        }
        String::from_utf8_lossy(&output.stdout).into_owned()
    } else {
        match std::fs::read_to_string(root.join("BUILD_VERSION")) {
            Ok(text) => text,
            Err(_) => return fallback(),
        }
    };
    let value = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .unwrap_or("")
            .to_owned()
    };
    short_build_version(&value("BASE_VER"), &value("VCODE")).unwrap_or_else(fallback)
}

/// The `.zip` a Mac prepare makes in `delivery` (`package_mac.sh` names it).
pub(super) fn zip_path(delivery: &Path, version: &str) -> PathBuf {
    delivery.join(format!("ACNH-Universal-{version}.zip"))
}

/// The prepared folder of `os`'s own target in `dirs`: on the Mac `<data root>/prepared/`
/// (one folder for both Apple targets), elsewhere `<data>/prepare/`.
pub fn prepared_folder(os: Os, dirs: &AppDirs) -> PathBuf {
    if os == Os::Mac {
        dirs.data_root().join("prepared")
    } else {
        dirs.data.join("prepare")
    }
}

/// The `.ipa` an iOS prepare makes in `delivery` (`package_ios.sh` names it).
pub(super) fn ipa_path(delivery: &Path, version: &str) -> PathBuf {
    delivery.join(format!("ACNH-Universal-{version}.ipa"))
}

/// The folder in the `ios` delivery folder that holds the small `.ipa` and, beside it, the game
/// folder `acnh-universal-game/ios/` (issue #306).
pub const IOS_SMALL_FOLDER: &str = "small";

/// One script of the Build stage, to its end; a failure is the Build error.
pub(super) fn run_script(
    runner: &mut dyn ScriptRunner,
    args: &[String],
    home: &Path,
) -> Result<()> {
    runner
        .run(Stage::Build, args)
        .map_err(|log| build_error(Stage::Build, &log, home))?;
    runner
        .finish()
        .map_err(|log| build_error(Stage::Build, &log, home))
}

/// Makes the bundled mods for the package. They are optional (only `video.frame_rate = 60`
/// needs them, and they come from the network on a first run): when the script fails (offline,
/// the release asset gone or changed) the package goes without them, and the mods are fetched
/// from the hub or the first run with a network. Returns the folder to pass as `--mods`.
pub(super) fn bundle_mods_optional(
    runner: &mut dyn ScriptRunner,
    dest: &Path,
    cache: &Path,
    mods: &Path,
) -> Option<PathBuf> {
    // A leftover from an earlier run must not be packaged when this one fails.
    let _ = std::fs::remove_dir_all(dest);
    let args = bundle_mods_args(dest, cache);
    let made = runner
        .run(Stage::Build, &args)
        .and_then(|()| runner.finish())
        .is_ok();
    made.then(|| mods.to_path_buf())
}

/// What the `ios` target does after the shared stages (which made the Mac player there too,
/// as upstream's script cannot stop before it; the iOS build does not use it): the two packs
/// when they are missing, the `iphoneos` build under the prepared folder, and the `.ipa` in
/// the delivery folder, whose path it returns, then the small `.ipa` and the game folder with
/// its manifest in `<delivery>/small/` (#306).
pub(super) fn ios_deliver(
    req: &Request,
    runner: &mut dyn ScriptRunner,
    home: &Path,
) -> Result<PathBuf> {
    let delivery = req
        .delivery
        .as_deref()
        .ok_or_else(|| Error::Env("the delivery folder is not set".into()))?;
    // Always: a no-op while the packs' key records match, a rebuild of a pack that has no
    // current record (one a killed run left truncated).
    pack_stage(req)?;
    let build = req.out.join("build-ios");
    let mut build_args = build_ios_args(&req.out.join("gen"), &build);
    // A Finder-launched app's PATH has only launchd's folders: the build script finds cargo
    // and Homebrew's tools (cmake, ninja) first in these.
    let mut dirs: Vec<PathBuf> = cargo_dir(runner, home).into_iter().collect();
    dirs.extend(
        ["/opt/homebrew/bin", "/usr/local/bin"]
            .map(PathBuf::from)
            .into_iter()
            .filter(|dir| dir.is_dir()),
    );
    let mut env = vec!["env".to_owned()];
    if !dirs.is_empty() {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(dirs.into_iter().chain(std::env::split_paths(&path)))
            .map_err(|e| Error::Build(format!("cannot make the PATH: {e}")))?;
        env.push(format!("PATH={}", path.to_string_lossy()));
    }
    // libvpx's clone and build outlast the source copy, which is made anew each prepare.
    env.push(format!("NX_VPX_DIR={}", req.out.join("vpx-ios").display()));
    build_args.splice(0..0, env);
    let root = runner.root();
    let version = package_version(&root);
    // The small .ipa's game folder names its build and ABI: refuse before any script runs
    // when the prepared folder has no build id (the player refuses a game of another build).
    let (build_id, abi) = game_build_and_abi(&req.out, &root, "the small .ipa")?;
    let small = delivery.join(IOS_SMALL_FOLDER);
    let (mods_dest, mods, mods_cache) = bundled_mods_folder(&req.out);
    run_script(runner, &build_args, home)?;
    let mods = bundle_mods_optional(runner, &mods_dest, &mods_cache, &mods);
    let args = package_ios_args(&version, &build, &req.out, delivery, mods.as_deref());
    run_script(runner, &args, home)?;
    let args = package_ios_small_args(
        &version,
        &build,
        &req.out,
        &build_id,
        &abi,
        &small,
        mods.as_deref(),
    );
    run_script(runner, &args, home)?;
    // The game's files and manifest.json for Dropbox, beside the small .ipa; copy
    // `acnh-universal-game/` into `Apps/save-relay/` (docs/IOS.md).
    stage_ios_delivery(
        &req.out,
        &small.join("acnh-universal-game").join("ios"),
        &build_id,
        &abi,
    )?;
    Ok(ipa_path(delivery, &version))
}

/// What the `mac` target does after the shared stages: the two packs when they are missing,
/// 1.22b's assembly of the contained app in the install folder, and `package_mac.sh`, which
/// puts the prepared game inside it and zips it into the delivery folder, whose zip's path it
/// returns.
pub(super) fn mac_deliver(
    req: &Request,
    runner: &mut dyn ScriptRunner,
    home: &Path,
) -> Result<PathBuf> {
    let delivery = req
        .delivery
        .as_deref()
        .ok_or_else(|| Error::Env("the delivery folder is not set".into()))?;
    let install = req
        .install
        .as_deref()
        .ok_or_else(|| Error::Env("the install folder is not set".into()))?;
    // Always: a no-op while the packs' key records match, a rebuild of a pack that has no
    // current record (one a killed run left truncated).
    pack_stage(req)?;
    let app = runner
        .assemble(&req.out.join("build"), install)
        .map_err(|e| build_error(Stage::Build, &[e.to_string()], home))?;
    let version = package_version(&runner.root());
    let (mods_dest, mods, mods_cache) = bundled_mods_folder(&req.out);
    let mods = bundle_mods_optional(runner, &mods_dest, &mods_cache, &mods);
    let args = package_mac_args(&version, &app, &req.out, delivery, mods.as_deref());
    run_script(runner, &args, home)?;
    Ok(zip_path(delivery, &version))
}

/// The prepared folder's build id (32 hex digits, upper case) and the runtime's ABI, which a
/// game folder's manifest names (the player refuses a game of another build or ABI):
/// `Error::Build` when either is missing, naming `what` the manifest is for.
pub(super) fn game_build_and_abi(out: &Path, root: &Path, what: &str) -> Result<(String, String)> {
    let build_id = read_prepared_build_id(out).to_ascii_uppercase();
    if build_id.len() != 32 || !build_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Build(format!(
            "the prepared folder has no 32 hex digit build id (version.ini) for {what}"
        )));
    }
    let abi = nx_game_abi(root);
    if abi == "unknown" {
        return Err(Error::Build(format!(
            "the runtime headers (runtime/include/nx) are missing, so the ABI of {what} is unknown"
        )));
    }
    Ok((build_id, abi))
}

/// The folder `tools/build_android.sh` makes its output in, under the prepared folder: the
/// only place `android_deliver` takes the game library from.
pub(super) fn android_apk_folder(out: &Path) -> PathBuf {
    out.join("apk-android")
}

/// The arguments of `tools/build_android.sh`: the recompiled code in `gen`, the CMake build
/// folder and the output folder, all under the prepared folder; no on-phone Prepare tools
/// (the game comes prepared from this computer, not from the phone).
pub(super) fn build_android_args(gen: &Path, build: &Path, apk: &Path) -> Vec<String> {
    vec![
        "tools/build_android.sh".to_owned(),
        "--no-phone-prepare".to_owned(),
        "--gen".to_owned(),
        gen.display().to_string(),
        "--build".to_owned(),
        build.display().to_string(),
        "--out".to_owned(),
        apk.display().to_string(),
    ]
}

/// What the `android` target does after the shared stages: ensures `romfs.pak` and `spirv.pak`,
/// refuses a prepared folder with no build id, builds `libacnh_game.so` from the code just
/// recompiled (`<out>/gen`; the script always runs, as its build is incremental), stages that
/// library and the prepared game files into the folder the app downloads from
/// (`<relay app folder>/acnh-universal-game/android/`), and writes `manifest.json` last.
pub(super) fn android_deliver(
    req: &Request,
    runner: &mut dyn ScriptRunner,
    home: &Path,
) -> Result<PathBuf> {
    let delivery = req
        .delivery
        .as_deref()
        .ok_or_else(|| Error::Env("the delivery folder is not set".into()))?;
    // Always: a no-op while the packs' key records match, a rebuild of a pack that has no
    // current record (one a killed run left truncated).
    pack_stage(req)?;

    let root = runner.root();
    let (build_id, abi) = game_build_and_abi(&req.out, &root, "the Android game folder")?;
    let apk = android_apk_folder(&req.out);
    let lib_path = apk.join("libacnh_game.so");
    // A library an earlier run left must not stand for this one's.
    match std::fs::remove_file(&lib_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let args = build_android_args(&req.out.join("gen"), &req.out.join("build-android"), &apk);
    run_script(runner, &args, home)?;
    if !lib_path.is_file() {
        return Err(Error::Build(
            "libacnh_game.so was not built for Android".into(),
        ));
    }
    stage_android_delivery(&req.out, &lib_path, delivery, &build_id, &abi)?;
    Ok(delivery.to_path_buf())
}

pub(super) fn read_prepared_build_id(out: &Path) -> String {
    if let Ok(ini) = std::fs::read_to_string(out.join("version.ini")) {
        if let Ok(cfg) = Config::parse(&ini) {
            if let Some(bid) = cfg.get("build_id") {
                if !bid.is_empty() {
                    return bid.to_owned();
                }
            }
        }
    }
    let main = out.join("exefs/main");
    if main.is_file() {
        if let Ok(bid) = crate::nso::read_build_id(&main) {
            return bid;
        }
    }
    String::new()
}

/// Compute `NX_GAME_ABI` matching `CMakeLists.txt` (first 12 hex digits of SHA-1 over
/// `runtime/include/nx/{cpu.h,module.h,runtime.h}`).
pub fn nx_game_abi(root: &Path) -> String {
    let mut text = Vec::new();
    for header in ["cpu.h", "module.h", "runtime.h"] {
        let path = root.join("runtime/include/nx").join(header);
        let Ok(bytes) = std::fs::read(&path) else {
            return "unknown".to_owned();
        };
        text.extend_from_slice(&bytes);
    }
    sha1_hex_12(&text)
}

pub(super) fn sha1_hex_12(data: &[u8]) -> String {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (i, word) in w[..16].iter_mut().enumerate() {
            let b = &chunk[i * 4..i * 4 + 4];
            *word = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let full = format!(
        "{:08x}{:08x}{:08x}{:08x}{:08x}",
        h[0], h[1], h[2], h[3], h[4]
    );
    full[..12].to_owned()
}

/// Stage the prepared Android game files and `manifest.json` atomically into `delivery`.
pub fn stage_android_delivery(
    prepared_out: &Path,
    game_lib: &Path,
    delivery: &Path,
    build_id: &str,
    nx_game_abi: &str,
) -> Result<()> {
    stage_game_delivery(
        prepared_out,
        Some(game_lib),
        delivery,
        build_id,
        nx_game_abi,
    )
}

/// Stage the prepared iOS game files (the same set, without a game library: the player carries
/// it) and `manifest.json` atomically into `delivery` (`.../acnh-universal-game/ios`).
pub fn stage_ios_delivery(
    prepared_out: &Path,
    delivery: &Path,
    build_id: &str,
    nx_game_abi: &str,
) -> Result<()> {
    stage_game_delivery(prepared_out, None, delivery, build_id, nx_game_abi)
}

pub(super) fn stage_game_delivery(
    prepared_out: &Path,
    game_lib: Option<&Path>,
    delivery: &Path,
    build_id: &str,
    nx_game_abi: &str,
) -> Result<()> {
    let parent = delivery.parent().unwrap_or(delivery);
    std::fs::create_dir_all(parent)?;
    let name = delivery
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("android");
    let staging = parent.join(format!(".{name}.new"));
    let backup = parent.join(format!(".{name}.old"));

    // A leftover of an interrupted run would land in the manifest.
    remove_dir_retrying(&staging)?;
    std::fs::create_dir_all(&staging)?;

    if let Some(game_lib) = game_lib {
        std::fs::copy(game_lib, staging.join("libacnh_game.so"))?;
    }
    for pak in ["romfs.pak", "spirv.pak"] {
        let src = prepared_out.join(pak);
        if !src.is_file() {
            return Err(Error::Build(format!("missing {}", src.display())));
        }
        std::fs::copy(&src, staging.join(pak))?;
    }
    for dir in ["analysis", "fonts", "aoc"] {
        let src_dir = prepared_out.join(dir);
        if src_dir.is_dir() {
            copy_dir_follow_symlinks(&src_dir, &staging.join(dir))?;
        }
    }

    let manifest = crate::appgame::Manifest::scan(&staging, build_id, nx_game_abi)?;
    // Write manifest.json last so an incomplete folder is never mistaken for a finished one.
    manifest.write_to(&staging)?;

    // A leftover backup must go first: the rename below needs its name free.
    remove_dir_retrying(&backup)?;
    if delivery.exists() {
        std::fs::rename(delivery, &backup)?;
    }
    std::fs::rename(&staging, delivery)?;
    // The delivery is in place: an old copy that will not go is left for the next run.
    let _ = remove_dir_retrying(&backup);
    Ok(())
}

/// Removes a folder in the Dropbox delivery folder, trying again while Finder or the Dropbox
/// client writes into it (a `.DS_Store` written during the walk makes the removal fail with
/// "Directory not empty"). A folder that is not there is already removed.
pub(super) fn remove_dir_retrying(dir: &Path) -> std::io::Result<()> {
    remove_dir_retrying_with(dir, 5, std::time::Duration::from_secs(1), &mut |d| {
        std::fs::remove_dir_all(d)
    })
}

pub(super) fn remove_dir_retrying_with(
    dir: &Path,
    tries: u32,
    wait: std::time::Duration,
    remove: &mut dyn FnMut(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut attempt = 1;
    loop {
        if std::fs::symlink_metadata(dir).is_err() {
            return Ok(());
        }
        match remove(dir) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) if attempt >= tries => return Err(e),
            Err(_) => {
                attempt += 1;
                std::thread::sleep(wait);
            }
        }
    }
}

pub(super) fn copy_dir_follow_symlinks(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".DS_Store" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        let meta = std::fs::metadata(&src_path)?;
        if meta.is_dir() {
            copy_dir_follow_symlinks(&src_path, &dst_path)?;
        } else if meta.is_file() {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod apple_version_tests {
    use super::{package_version, short_build_version};

    #[test]
    fn a_packaged_source_without_git_is_named_by_its_build_version_file() {
        let root = std::env::temp_dir().join(format!("acnh-pkg-version-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // No .git, no file: the engine's own version, never git's 0.0.0 of a bare folder.
        assert_eq!(package_version(&root), env!("CARGO_PKG_VERSION"));
        std::fs::write(
            root.join("BUILD_VERSION"),
            "BASE_VER=0.11.0\nVNAME=0.11.0\nVCODE=1100891\n",
        )
        .unwrap();
        assert_eq!(package_version(&root), "0.11.0-1100891");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_short_build_is_the_base_and_the_code_without_a_plus() {
        assert_eq!(
            short_build_version("0.11.0", "1100891").as_deref(),
            Some("0.11.0-1100891")
        );
        assert_eq!(
            short_build_version("0.11.0+abc1234", "1100891").as_deref(),
            Some("0.11.0-1100891")
        );
        assert_eq!(
            short_build_version("0.0.0", "10042\n").as_deref(),
            Some("0.0.0-10042")
        );
        // What the package scripts would refuse is never made.
        assert_eq!(short_build_version("", "1100891"), None);
        assert_eq!(short_build_version("0.11.0", ""), None);
        assert_eq!(short_build_version("0.11.0", "11x"), None);
        assert_eq!(short_build_version("0.11 0", "1100"), None);
        assert_eq!(short_build_version("../0", "1100"), None);
    }
}

#[cfg(test)]
mod remove_dir_retrying_tests {
    use super::remove_dir_retrying_with;
    use std::io::{Error, ErrorKind};
    use std::time::Duration;

    fn folder(tag: &str) -> std::path::PathBuf {
        let dir = tempfile::Builder::new()
            .prefix(&format!("acnh-remove-{tag}-"))
            .tempdir()
            .unwrap()
            .keep();
        std::fs::write(dir.join("file"), b"x").unwrap();
        dir
    }

    #[test]
    fn a_write_during_the_removal_is_tried_again_until_the_folder_goes() {
        let dir = folder("race");
        let mut calls = 0;
        // Finder or Dropbox writes a .DS_Store into the folder twice while it is being removed.
        let result = remove_dir_retrying_with(&dir, 5, Duration::ZERO, &mut |d| {
            calls += 1;
            if calls < 3 {
                std::fs::write(d.join(".DS_Store"), b"").unwrap();
                return Err(Error::other("Directory not empty"));
            }
            std::fs::remove_dir_all(d)
        });
        assert!(result.is_ok());
        assert_eq!(calls, 3);
        assert!(!dir.exists());
    }

    #[test]
    fn a_folder_that_never_goes_is_an_error_after_the_last_try() {
        let dir = folder("stuck");
        let mut calls = 0;
        let result = remove_dir_retrying_with(&dir, 4, Duration::ZERO, &mut |_| {
            calls += 1;
            Err(Error::other("Directory not empty"))
        });
        assert!(result.is_err());
        assert_eq!(calls, 4);
        assert!(dir.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_folder_that_is_not_there_or_goes_first_is_already_removed() {
        let missing =
            std::env::temp_dir().join(format!("acnh-remove-missing-{}", std::process::id()));
        let mut calls = 0;
        let counting = &mut |_: &std::path::Path| {
            calls += 1;
            Ok(())
        };
        assert!(remove_dir_retrying_with(&missing, 5, Duration::ZERO, counting).is_ok());
        assert_eq!(calls, 0);
        let dir = folder("gone");
        let result = remove_dir_retrying_with(&dir, 5, Duration::ZERO, &mut |d| {
            std::fs::remove_dir_all(d).unwrap();
            Err(Error::from(ErrorKind::NotFound))
        });
        assert!(result.is_ok());
    }
}
