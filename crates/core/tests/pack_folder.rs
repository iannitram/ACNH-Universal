use acnh_core::bundle;
use acnh_core::config::Config;
use acnh_core::platform::Os;
use acnh_core::prepare::{app_out_dir, pack_folder_with, PackContext};
use acnh_core::Error;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn write_empty(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, b"").unwrap();
}

fn collect_rel_paths(root: &Path, dir: &Path, out: &mut BTreeSet<String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            collect_rel_paths(root, &path, out);
        } else {
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.insert(rel);
        }
    }
}

fn make_stub_prepared(prepared: &Path) {
    for rel in [
        "analysis/main.bin",
        "romfs.pak",
        "spirv.pak",
        "fonts/shared.ttf",
        "aoc/1/data",
        "gen/main/table.cpp",
        "build/acnh",
        "exefs/main",
        "extract/romfs/a",
        "spirv/a.spv",
        "shaders_extracted/a",
        "mods/m/a",
        "user/device_save/main.dat",
        "logs/nx.log",
        "config.ini",
    ] {
        write_empty(&prepared.join(rel));
    }
}

fn make_fake_appdir(appdir: &Path) {
    write_empty(&appdir.join("usr/bin/acnh-app"));
    write_empty(&appdir.join("usr/bin/acnh-universal"));
    write_empty(&appdir.join("usr/share/acnh-universal/acnhrecomp.png"));
    write_empty(&appdir.join("usr/share/acnh-universal/source/SOURCE"));
}

fn make_fake_assembled(assembled: &Path) {
    write_empty(&assembled.join("acnh"));
    write_empty(&assembled.join("libSDL3.so.0"));
    // Device-owned folders in install() must never be copied into the packed folder.
    write_empty(&assembled.join("root/user/device_save/main.dat"));
}

fn no_git(_: &Path) -> acnh_core::Result<bool> {
    Ok(false)
}

fn host_copy(from: &Path, to: &Path) -> acnh_core::Result<()> {
    fs::copy(from, to)?;
    Ok(())
}

#[test]
fn packs_appdir_assembled_and_read_only_game_only() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let data_root = root.path().join("data/root");
    let appdir = root.path().join("mount");
    let assembled = root.path().join("install");
    let prepared = root.path().join("prepared");
    let out = root.path().join("out");

    make_fake_appdir(&appdir);
    make_fake_assembled(&assembled);
    make_stub_prepared(&prepared);

    let mut git_probe = no_git;
    let mut copy = host_copy;
    let mut ctx = PackContext {
        appdir: Some(&appdir),
        home: &home,
        data_root: &data_root,
        game_folder: None,
        dropbox: None,
        ci: false,
        runner_temp: None,
        git_in_work_tree: &mut git_probe,
        copy_file: &mut copy,
    };

    let packed = pack_folder_with(
        &appdir.join("usr/bin/acnh-app"),
        &assembled,
        &prepared,
        &out,
        &mut ctx,
    )
    .unwrap();
    assert_eq!(packed, out.join("ACNH-Universal"));

    let mut files = BTreeSet::new();
    collect_rel_paths(&packed, &packed, &mut files);
    let expected: BTreeSet<String> = [
        "acnh",
        "acnh-app",
        "acnh-universal",
        "acnhrecomp.png",
        "game/analysis/main.bin",
        "game/aoc/1/data",
        "game/fonts/shared.ttf",
        "game/romfs.pak",
        "game/spirv.pak",
        "libSDL3.so.0",
        "source/SOURCE",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(files, expected);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for exe in ["acnh-app", "acnh-universal", "acnh"] {
            let mode = fs::metadata(packed.join(exe)).unwrap().permissions().mode();
            assert_ne!(mode & 0o111, 0, "{exe} must be executable");
        }
        fn assert_no_write(dir: &Path) {
            let mode = fs::metadata(dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o222, 0, "{} must be read-only", dir.display());
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    assert_no_write(&path);
                } else {
                    let mode = fs::metadata(&path).unwrap().permissions().mode();
                    assert_eq!(mode & 0o222, 0, "{} must be read-only", path.display());
                }
            }
        }
        assert_no_write(&packed.join("game"));
    }

    // No .AppImage and no .desktop file anywhere under the temp root.
    let mut all_temp = BTreeSet::new();
    collect_rel_paths(root.path(), root.path(), &mut all_temp);
    assert!(
        !all_temp
            .iter()
            .any(|p| p.ends_with(".AppImage") || p.ends_with(".desktop")),
        "{all_temp:?}"
    );

    // bundle::manifest must refuse the packed folder and name a file under game/.
    let err = bundle::manifest(&packed, Os::Linux, "1.0.0", "abc").unwrap_err();
    assert!(
        err.to_string().contains("game/"),
        "expected game/ in error: {err}"
    );

    // Cleanup read-only permissions before tempdir drops.
    fn restore_write(dir: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(dir).unwrap().permissions();
            perms.set_mode(perms.mode() | 0o700);
            fs::set_permissions(dir, perms).unwrap();
        }
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                restore_write(&path);
            }
        }
    }
    restore_write(&packed.join("game"));
}

#[test]
fn packs_from_beside_executable_when_appdir_unset() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let data_root = root.path().join("data/root");
    let bin_dir = root.path().join("bin");
    let assembled = root.path().join("install");
    let prepared = root.path().join("prepared");
    let out = root.path().join("out");

    write_empty(&bin_dir.join("acnh-app"));
    write_empty(&bin_dir.join("acnh-universal"));
    write_empty(&bin_dir.join("acnhrecomp.png"));
    make_fake_assembled(&assembled);
    make_stub_prepared(&prepared);

    let mut git_probe = no_git;
    let mut copy = host_copy;
    let mut ctx = PackContext {
        appdir: None,
        home: &home,
        data_root: &data_root,
        game_folder: None,
        dropbox: None,
        ci: false,
        runner_temp: None,
        git_in_work_tree: &mut git_probe,
        copy_file: &mut copy,
    };

    let packed = pack_folder_with(
        &bin_dir.join("acnh-app"),
        &assembled,
        &prepared,
        &out,
        &mut ctx,
    )
    .unwrap();
    assert!(packed.join("acnh-app").is_file());
    assert!(packed.join("acnh-universal").is_file());
    assert!(packed.join("acnhrecomp.png").is_file());
    assert!(packed.join("acnh").is_file());
    assert!(packed.join("game/romfs.pak").is_file());
}

#[test]
fn refuses_unsafe_out_folders_and_invalid_prepared() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let data_root = root.path().join("data/root");
    let game_folder = root.path().join("my-game");
    let appdir = root.path().join("mount");
    let assembled = root.path().join("install");
    let prepared = root.path().join("prepared");
    let runner_temp = root.path().join("runner_temp");

    make_fake_appdir(&appdir);
    make_fake_assembled(&assembled);
    make_stub_prepared(&prepared);
    fs::create_dir_all(&game_folder).unwrap();
    fs::create_dir_all(&data_root).unwrap();
    fs::create_dir_all(home.join("Dropbox")).unwrap();
    fs::create_dir_all(home.join("Nextcloud")).unwrap();
    fs::create_dir_all(&runner_temp).unwrap();

    // Real git repo refusal
    let git_repo = root.path().join("repo");
    fs::create_dir_all(&git_repo).unwrap();
    let status = Command::new("git")
        .arg("-C")
        .arg(&git_repo)
        .arg("init")
        .status()
        .unwrap();
    assert!(status.success());

    let check_refusal = |out: &Path, ci: bool, rt: Option<&Path>, prep: &Path, needle: &str| {
        let mut git_probe = |dir: &Path| {
            let output = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["rev-parse", "--is-inside-work-tree"])
                .output()
                .unwrap();
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).trim() == "true")
            } else {
                Ok(false)
            }
        };
        let mut copy_fn = host_copy;
        let mut ctx = PackContext {
            appdir: Some(&appdir),
            home: &home,
            data_root: &data_root,
            game_folder: Some(&game_folder),
            dropbox: None,
            ci,
            runner_temp: rt,
            git_in_work_tree: &mut git_probe,
            copy_file: &mut copy_fn,
        };
        let err = pack_folder_with(
            &appdir.join("usr/bin/acnh-app"),
            &assembled,
            prep,
            out,
            &mut ctx,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains(needle),
            "expected {needle:?} in {err}"
        );
        assert!(!out.join("ACNH-Universal").exists());
        assert!(!out.join(".ACNH-Universal.partial").exists());
    };

    check_refusal(
        &git_repo.join("sub"),
        false,
        None,
        &prepared,
        "git work tree",
    );
    check_refusal(
        &game_folder.join("sub"),
        false,
        None,
        &prepared,
        "game folder",
    );
    check_refusal(&data_root.join("sub"), false, None, &prepared, "data root");
    check_refusal(&home.join("Dropbox/sub"), false, None, &prepared, "Dropbox");
    check_refusal(
        &home.join("Nextcloud/sub"),
        false,
        None,
        &prepared,
        "Nextcloud",
    );
    check_refusal(
        &root.path().join("outside-rt"),
        true,
        Some(&runner_temp),
        &prepared,
        "RUNNER_TEMP",
    );

    // CI non-empty decoy file refusal
    let nonempty_prep = root.path().join("nonempty_prep");
    make_stub_prepared(&nonempty_prep);
    fs::write(nonempty_prep.join("romfs.pak"), b"not empty").unwrap();
    check_refusal(
        &runner_temp.join("out"),
        true,
        Some(&runner_temp),
        &nonempty_prep,
        "empty decoy",
    );

    // Missing romfs.pak refusal
    let missing_prep = root.path().join("missing_prep");
    make_stub_prepared(&missing_prep);
    fs::remove_file(missing_prep.join("romfs.pak")).unwrap();
    check_refusal(
        &root.path().join("valid_out"),
        false,
        None,
        &missing_prep,
        "romfs.pak",
    );
}

#[test]
fn failed_copy_cleans_partial_and_preserves_existing_pack() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let data_root = root.path().join("data/root");
    let appdir = root.path().join("mount");
    let assembled = root.path().join("install");
    let prepared = root.path().join("prepared");
    let out = root.path().join("out");

    make_fake_appdir(&appdir);
    make_fake_assembled(&assembled);
    make_stub_prepared(&prepared);

    // First, create an earlier pack.
    let mut git_probe = no_git;
    let mut copy = host_copy;
    let mut ctx = PackContext {
        appdir: Some(&appdir),
        home: &home,
        data_root: &data_root,
        game_folder: None,
        dropbox: None,
        ci: false,
        runner_temp: None,
        git_in_work_tree: &mut git_probe,
        copy_file: &mut copy,
    };
    let packed = pack_folder_with(
        &appdir.join("usr/bin/acnh-app"),
        &assembled,
        &prepared,
        &out,
        &mut ctx,
    )
    .unwrap();
    fs::write(packed.join("marker.txt"), b"previous").unwrap();

    // Now fail on the 3rd copied file.
    let mut count = 0usize;
    let mut failing_copy = |from: &Path, to: &Path| -> acnh_core::Result<()> {
        count += 1;
        if count == 3 {
            return Err(Error::Io("simulated copy failure".into()));
        }
        fs::copy(from, to)?;
        Ok(())
    };
    let mut ctx_fail = PackContext {
        appdir: Some(&appdir),
        home: &home,
        data_root: &data_root,
        game_folder: None,
        dropbox: None,
        ci: false,
        runner_temp: None,
        git_in_work_tree: &mut git_probe,
        copy_file: &mut failing_copy,
    };
    let err = pack_folder_with(
        &appdir.join("usr/bin/acnh-app"),
        &assembled,
        &prepared,
        &out,
        &mut ctx_fail,
    )
    .unwrap_err();
    assert!(err.to_string().contains("simulated copy failure"), "{err}");
    assert!(!out.join(".ACNH-Universal.partial").exists());
    assert_eq!(fs::read(packed.join("marker.txt")).unwrap(), b"previous");

    // Re-packing successfully replaces the earlier pack (even with read-only game/).
    let mut ctx_ok = PackContext {
        appdir: Some(&appdir),
        home: &home,
        data_root: &data_root,
        game_folder: None,
        dropbox: None,
        ci: false,
        runner_temp: None,
        git_in_work_tree: &mut git_probe,
        copy_file: &mut copy,
    };
    pack_folder_with(
        &appdir.join("usr/bin/acnh-app"),
        &assembled,
        &prepared,
        &out,
        &mut ctx_ok,
    )
    .unwrap();
    assert!(!packed.join("marker.txt").exists());
}

#[test]
fn app_out_dir_uses_app_dir_key_or_applications() {
    let home = Path::new("/home/user");
    assert_eq!(
        app_out_dir(&Config::default(), home),
        PathBuf::from("/home/user/Applications")
    );
    let cfg = Config::parse("app_dir = /opt/games\n").unwrap();
    assert_eq!(app_out_dir(&cfg, home), PathBuf::from("/opt/games"));
}
