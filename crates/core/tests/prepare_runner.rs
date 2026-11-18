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

#[test]
fn completed_tools_version_and_build_do_not_run_again() {
    let req = request("completed", Os::Linux);
    std::fs::write(req.out.join(".prepare-tools.json"), "{}").unwrap();
    std::fs::write(req.out.join("version.ini"), "synthetic version").unwrap();
    std::fs::create_dir_all(req.out.join("build")).unwrap();
    std::fs::write(req.out.join("build/acnh"), "synthetic executable").unwrap();
    let mut fake = Fake::default();
    let mut seen = Vec::new();
    prepare_with(&req, Hub::StrixHalo, &mut fake, Path::new("/h"), &mut |s| {
        seen.push(s)
    })
    .unwrap();
    assert_eq!(
        fake.ran,
        vec![Stage::Game, Stage::Recompile, Stage::Shaders, Stage::Fonts]
    );
    assert_eq!(seen, fake.ran);
}

#[test]
fn incomplete_module_outputs_are_not_skipped() {
    let req = request("partial", Os::Linux);
    std::fs::create_dir_all(req.out.join("analysis")).unwrap();
    std::fs::write(req.out.join("analysis/main.bin"), "synthetic module").unwrap();
    std::fs::create_dir_all(req.out.join("gen/main")).unwrap();
    std::fs::write(req.out.join("gen/main/table.cpp"), "// synthetic table").unwrap();
    let mut fake = Fake::default();
    prepare_with(
        &req,
        Hub::StrixHalo,
        &mut fake,
        Path::new("/h"),
        &mut |_| {},
    )
    .unwrap();
    assert!(fake.ran.contains(&Stage::Game));
    assert!(fake.ran.contains(&Stage::Recompile));
    for module in ["sdk", "subsdk0"] {
        std::fs::write(
            req.out.join("analysis").join(format!("{module}.bin")),
            "synthetic module",
        )
        .unwrap();
        std::fs::create_dir_all(req.out.join("gen").join(module)).unwrap();
        std::fs::write(
            req.out.join("gen").join(module).join("table.cpp"),
            "// synthetic table",
        )
        .unwrap();
    }
    let mut fake = Fake::default();
    prepare_with(
        &req,
        Hub::StrixHalo,
        &mut fake,
        Path::new("/h"),
        &mut |_| {},
    )
    .unwrap();
    assert!(!fake.ran.contains(&Stage::Game));
    assert!(!fake.ran.contains(&Stage::Recompile));
}

#[test]
fn mobile_requests_never_fall_back_to_a_desktop_build() {
    for (target, hub) in [(Os::Android, Hub::StrixHalo), (Os::Ios, Hub::Mac)] {
        let req = request(target.key(), target);
        let mut fake = Fake::default();
        let mut seen = Vec::new();
        let error =
            prepare_with(&req, hub, &mut fake, Path::new("/h"), &mut |s| seen.push(s)).unwrap_err();
        assert!(matches!(error, Error::Unsupported(message) if message.contains(target.key())));
        assert!(fake.ran.is_empty());
        assert!(seen.is_empty());
    }
}

#[test]
fn mac_skips_upstreams_native_executable() {
    let req = request("native-mac", Os::Mac);
    std::fs::create_dir_all(req.out.join("build")).unwrap();
    std::fs::write(req.out.join("build/acnh"), "synthetic executable").unwrap();
    let mut fake = Fake::default();
    prepare_with(&req, Hub::Mac, &mut fake, Path::new("/h"), &mut |_| {}).unwrap();
    assert!(!fake.ran.contains(&Stage::Build));
}

#[test]
fn production_adapter_isolated_stages_match_upstreams_commands() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new(if cfg!(windows) { "python" } else { "python3" })
        .args([
            "-B",
            "-m",
            "unittest",
            "discover",
            "-s",
            "tools/tests",
            "-p",
            "test_prepare_stage.py",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn stage_commands_select_one_stage_and_forward_requested_jobs() {
    let mut req = request("selected-stage", Os::Linux);
    req.jobs = 2;
    for stage in STAGES {
        assert_eq!(
            step(stage, &req),
            Step::Script(vec![
                "python3".to_owned(),
                "tools/prepare_stage.py".to_owned(),
                "--stage".to_owned(),
                stage.key().to_owned(),
                "--target".to_owned(),
                "linux".to_owned(),
                "--game".to_owned(),
                req.game.root.display().to_string(),
                "--out".to_owned(),
                req.out.display().to_string(),
                "--jobs".to_owned(),
                "2".to_owned(),
            ])
        );
    }
}

#[test]
fn process_runner_keeps_stdout_and_stderr_lines_separate() {
    use acnh_core::prepare::ProcessRunner;
    let command = [
        if cfg!(windows) { "python" } else { "python3" }.to_owned(),
        "-c".to_owned(),
        "import sys; sys.stdout.write('stdout'); sys.stderr.write('stderr'); sys.exit(1)"
            .to_owned(),
    ];
    let log = ProcessRunner.run(Stage::Shaders, &command).unwrap_err();
    assert_eq!(log, vec!["stdout", "stderr"]);
}
