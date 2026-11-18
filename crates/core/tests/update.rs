// Tests for update (issue 1.25: back the island up, rebuild, check it is still there;
// SPEC.md M7).
use acnh_core::backup::{list, KEEP};
use acnh_core::error::Error;
use acnh_core::gamefolder::GameFolder;
use acnh_core::platform::Os;
use acnh_core::prepare::{prepare_with, Request, ScriptRunner};
use acnh_core::recipe::{Hub, Stage};
use acnh_core::update::update;
use acnh_core::version::Version;
use std::fs;
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("acnh-update-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn put(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn island(d: &Path) -> PathBuf {
    let save_dir = d.join("device_save");
    put(&save_dir, "main.dat", "m");
    put(&save_dir, "mainHeader.dat", "h");
    put(&save_dir, "Villager0/personal.dat", "v");
    save_dir
}

#[test]
fn the_backup_exists_before_the_rebuild_starts() {
    let d = scratch("before");
    let save_dir = island(&d);
    let backups = d.join("backups");
    let stamp = "20261118-090000";
    let mut seen = None;
    update(&save_dir, &backups, stamp, &mut || {
        seen = Some(backups.join(stamp).is_dir());
        Ok(())
    })
    .unwrap();
    assert_eq!(seen, Some(true));
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn eleven_backups_prune_to_ten() {
    let d = scratch("prune");
    let save_dir = island(&d);
    let backups = d.join("backups");
    for day in 1..=10 {
        put(&backups, &format!("202610{day:02}-000000/main.dat"), "old");
    }
    let updated = update(&save_dir, &backups, "20261118-090000", &mut || Ok(())).unwrap();
    let names = list(&backups).unwrap();
    assert_eq!(names.len(), KEEP);
    assert!(!names.contains(&"20261001-000000".to_owned()));
    assert!(names.contains(&"20261118-090000".to_owned()));
    assert_eq!(updated.backup, backups.join("20261118-090000"));
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_failed_rebuild_leaves_the_island_untouched_and_names_the_backup() {
    let d = scratch("rebuild-fails");
    let save_dir = island(&d);
    let backups = d.join("backups");
    let backup = backups.join("20261118-090000");
    let error = update(&save_dir, &backups, "20261118-090000", &mut || {
        Err(Error::Build("stage build: boom".into()))
    })
    .unwrap_err();
    assert!(error.to_string().contains("stage build: boom"));
    assert!(error.to_string().contains(&backup.display().to_string()));
    assert_eq!(fs::read_to_string(save_dir.join("main.dat")).unwrap(), "m");
    assert_eq!(
        fs::read_to_string(save_dir.join("Villager0/personal.dat")).unwrap(),
        "v"
    );
    assert!(backup.join("main.dat").is_file());
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_missing_island_after_the_rebuild_is_an_error() {
    let d = scratch("missing-after");
    let save_dir = island(&d);
    let backups = d.join("backups");
    let backup = backups.join("20261118-090000");
    let error = update(&save_dir, &backups, "20261118-090000", &mut || {
        fs::remove_dir_all(&save_dir)?;
        Ok(())
    })
    .unwrap_err();
    match &error {
        Error::Saves(m) => {
            assert!(m.contains("no island at"), "{m}");
            assert!(m.contains(&backup.display().to_string()), "{m}");
        }
        other => panic!("{other:?}"),
    }
    assert!(backup.join("main.dat").is_file());
    fs::remove_dir_all(&d).unwrap();
}

#[derive(Default)]
struct Fake {
    ran: Vec<Stage>,
}

impl ScriptRunner for Fake {
    fn run(&mut self, stage: Stage, _args: &[String]) -> Result<(), Vec<String>> {
        self.ran.push(stage);
        Ok(())
    }
}

fn request(name: &str) -> Request {
    let d = std::env::temp_dir().join(format!("acnh-update-build-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    Request {
        game: GameFolder {
            root: PathBuf::from("/game"),
            exefs: PathBuf::from("/game/exefs"),
            romfs: PathBuf::from("/game/romfs"),
            aoc: vec![],
            from_importer: false,
            version: Version::V3_0_2,
        },
        out: d,
        target: Os::Mac,
        jobs: 0,
    }
}

#[test]
fn a_fake_build_through_the_prepare_runner_keeps_the_island() {
    let d = scratch("fake-build");
    let save_dir = island(&d);
    let backups = d.join("backups");
    let req = request("fake-build");
    let mut fake = Fake::default();
    let mut rebuild = || {
        prepare_with(
            &req,
            Hub::Mac,
            &mut fake,
            Path::new("/home/user"),
            &mut |_| {},
        )
        .map(|_| ())
    };
    let updated = update(&save_dir, &backups, "20261118-090000", &mut rebuild).unwrap();
    assert!(!fake.ran.is_empty());
    assert_eq!(updated.files_before, 3);
    assert_eq!(updated.files_after, 3);
    fs::remove_dir_all(&d).unwrap();
}
