// Tests for the prepare stage runner (issue 1.22), with a fake script runner.
use acnh_core::error::Error;
use acnh_core::gamefolder::GameFolder;
use acnh_core::platform::Os;
use acnh_core::prepare::{prepare_with, step, Request, ScriptRunner, Step};
use acnh_core::recipe::{Hub, Stage, STAGES};
use acnh_core::version::Version;
use std::path::{Path, PathBuf};

fn request(name: &str, target: Os) -> Request {
    let d = std::env::temp_dir().join(format!("acnh-prep-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
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
        target,
        jobs: 0,
    }
}

#[derive(Default)]
struct Fake {
    ran: Vec<Stage>,
    args: Vec<Vec<String>>,
    fail_at: Option<Stage>,
}

impl ScriptRunner for Fake {
    fn run(&mut self, stage: Stage, args: &[String]) -> Result<(), Vec<String>> {
        self.ran.push(stage);
        self.args.push(args.to_vec());
        if self.fail_at == Some(stage) {
            return Err(vec!["boom at /home/me/x".to_owned()]);
        }
        Ok(())
    }
}

#[test]
fn stages_run_in_order_and_progress_sees_each_once() {
    let req = request("order", Os::Linux);
    let mut fake = Fake::default();
    let mut seen = vec![];
    let out = prepare_with(
        &req,
        Hub::StrixHalo,
        &mut fake,
        Path::new("/home/me"),
        &mut |s| seen.push(s),
    )
    .unwrap();
    assert_eq!(out, req.out);
    assert_eq!(fake.ran, STAGES.to_vec());
    assert_eq!(seen, STAGES.to_vec());
}

#[test]
fn a_stage_with_its_output_is_skipped() {
    let req = request("skip", Os::Linux);
    let spirv = req.out.join("spirv");
    std::fs::create_dir_all(&spirv).unwrap();
    std::fs::write(spirv.join("summary.txt"), "x").unwrap();
    let mut fake = Fake::default();
    prepare_with(
        &req,
        Hub::StrixHalo,
        &mut fake,
        Path::new("/h"),
        &mut |_| {},
    )
    .unwrap();
    assert!(!fake.ran.contains(&Stage::Shaders));
    assert!(fake.ran.contains(&Stage::Fonts));
}

#[test]
fn a_failing_script_stops_with_build_naming_the_stage() {
    let req = request("fail", Os::Linux);
    let mut fake = Fake {
        fail_at: Some(Stage::Recompile),
        ..Fake::default()
    };
    let err = prepare_with(
        &req,
        Hub::StrixHalo,
        &mut fake,
        Path::new("/home/me"),
        &mut |_| {},
    )
    .unwrap_err();
    match err {
        Error::Build(m) => {
            assert!(m.contains("recompile"), "{m}");
            assert!(m.contains("boom at ~/x"), "{m}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(fake.ran.last(), Some(&Stage::Recompile));
}

#[test]
fn a_target_the_hub_does_not_serve_is_unsupported() {
    let req = request("unsup", Os::Windows);
    let err = prepare_with(
        &req,
        Hub::StrixHalo,
        &mut Fake::default(),
        Path::new("/h"),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
}

#[test]
fn step_is_upstreams_script_for_every_stage() {
    let req = request("step", Os::Linux);
    for stage in STAGES {
        assert_eq!(
            step(stage, &req),
            Step::Script(vec![
                "python3".to_owned(),
                "tools/setup_acnh.py".to_owned(),
                "--game".to_owned(),
                "/game".to_owned(),
                "--out".to_owned(),
                req.out.display().to_string(),
            ])
        );
    }
    let win = request("step-win", Os::Windows);
    match step(Stage::Tools, &win) {
        Step::Script(a) => assert_eq!(a[0], "python"),
        Step::Rust => panic!("not ported"),
    }
}
