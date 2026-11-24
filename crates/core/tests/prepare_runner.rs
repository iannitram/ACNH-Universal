// Tests for the prepare stage runner (issue 1.22), with a fake script runner.
use acnh_core::config::Config;
use acnh_core::error::Error;
use acnh_core::gamefolder::GameFolder;
use acnh_core::platform::{AppDirs, Os, Vars};
use acnh_core::prepare::{
    delivery_folder, delivery_subfolder, install_mod_archive, install_mods, prepare_installed,
    prepare_with, recomp_hash, rust_version, speedfix_args, speedfix_patch, step, toolchain_with,
    Missing, ProcessRunner, Request, ScriptRunner, Step,
};
use acnh_core::recipe::{Hub, Stage, STAGES};
use acnh_core::version::Version;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn request(name: &str, target: Os) -> Request {
    let d = std::env::temp_dir().join(format!("acnh-prep-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(".fan-mod-settings"), "false false\n").unwrap();
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
        source: None,
        fan_mods: Default::default(),
        delivery: None,
        install: None,
    }
}

/// The stages that run a script: only recompile (`python3 -m recomp` until #49).
fn scripted() -> Vec<Stage> {
    vec![Stage::Recompile]
}

#[derive(Default)]
struct Fake {
    ran: Vec<Stage>,
    args: Vec<Vec<String>>,
    fail_at: Option<Stage>,
    root: Option<PathBuf>,
    /// The programs `probe` finds (and what they print); the others are not there.
    tools: Vec<(&'static str, &'static str)>,
    probed: Vec<String>,
    /// The `(build, install)` folders the Mac's app was assembled from and into.
    assembled: Vec<(PathBuf, PathBuf)>,
}

/// A script call without the `env PATH=...` a Mac that has Homebrew puts before it.
fn without_env(args: &[String]) -> &[String] {
    if args.first().is_some_and(|program| program == "env") {
        &args[2..]
    } else {
        args
    }
}

impl Fake {
    /// A Mac whose prepared folder is no git repository (the Mac target needs no other tool).
    fn with_mac_tools() -> Fake {
        Fake {
            tools: vec![(
                "git:stderr",
                "fatal: not a git repository (or any of the parent directories): .git\n",
            )],
            ..Fake::default()
        }
    }

    /// A Mac with Xcode, cargo and the iOS Rust target.
    fn with_ios_tools() -> Fake {
        Fake {
            tools: vec![
                ("xcrun", "/Applications/Xcode.app/iPhoneOS.sdk\n"),
                ("cargo", "cargo 1.90.0\n"),
                ("rustup", "aarch64-apple-darwin\naarch64-apple-ios\n"),
                // `git` fails (it is not found in the fake), saying the folder is no repository.
                (
                    "git:stderr",
                    "fatal: not a git repository (or any of the parent directories): .git\n",
                ),
            ],
            ..Fake::default()
        }
    }
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

    fn set_root(&mut self, root: &Path) {
        self.root = Some(root.to_owned());
    }

    fn tools(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn game(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn version(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn recompile(&mut self, _req: &Request) -> Result<(), Error> {
        self.ran.push(Stage::Recompile);
        if self.fail_at == Some(Stage::Recompile) {
            return Err(Error::Build("stage recompile: boom at ~/x".to_owned()));
        }
        Ok(())
    }

    fn shaders(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn fonts(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn build(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn assemble(&mut self, build: &Path, install: &Path) -> acnh_core::Result<PathBuf> {
        self.assembled.push((build.to_owned(), install.to_owned()));
        Ok(install.join("ACNH-Universal.app"))
    }

    fn probe_stderr(&mut self, program: &str, _args: &[&str]) -> Option<(bool, String)> {
        self.tools
            .iter()
            .find(|(name, _)| *name == format!("{program}:stderr"))
            .map(|(_, error)| (false, (*error).to_owned()))
    }

    fn probe(&mut self, program: &str, args: &[&str]) -> Option<String> {
        self.probed.push(format!("{program} {}", args.join(" ")));
        self.tools
            .iter()
            .find(|(name, _)| *name == program)
            .map(|(_, output)| (*output).to_owned())
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
    assert_eq!(fake.ran, scripted());
    assert_eq!(seen, STAGES.to_vec());
}

#[test]
fn a_stage_with_its_output_is_skipped() {
    let req = request("skip", Os::Linux);
    let spirv = req.out.join("spirv");
    std::fs::create_dir_all(&spirv).unwrap();
    std::fs::write(spirv.join("summary.txt"), "x").unwrap();
    let mut fake = Fake::default();
    let mut seen = Vec::new();
    prepare_with(&req, Hub::StrixHalo, &mut fake, Path::new("/h"), &mut |s| {
        seen.push(s)
    })
    .unwrap();
    assert!(!seen.contains(&Stage::Shaders));
    assert!(seen.contains(&Stage::Fonts));
    assert_eq!(fake.ran, vec![Stage::Recompile]);
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
fn step_is_rust_for_every_stage() {
    let req = request("step", Os::Linux);
    for stage in STAGES {
        assert_eq!(step(stage, &req), Step::Rust);
    }
    let win = request("step-win", Os::Windows);
    for stage in STAGES {
        assert_eq!(step(stage, &win), Step::Rust);
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
    assert_eq!(fake.ran, vec![Stage::Recompile]);
    assert_eq!(
        seen,
        vec![Stage::Game, Stage::Recompile, Stage::Shaders, Stage::Fonts]
    );
}

#[test]
fn incomplete_module_outputs_are_not_skipped() {
    let req = request("partial", Os::Linux);
    std::fs::create_dir_all(req.out.join("analysis")).unwrap();
    std::fs::write(req.out.join("analysis/main.bin"), "synthetic module").unwrap();
    std::fs::create_dir_all(req.out.join("gen/main")).unwrap();
    std::fs::write(req.out.join("gen/main/table.cpp"), "// synthetic table").unwrap();
    let mut fake = Fake::default();
    let mut seen = Vec::new();
    prepare_with(&req, Hub::StrixHalo, &mut fake, Path::new("/h"), &mut |s| {
        seen.push(s)
    })
    .unwrap();
    assert!(seen.contains(&Stage::Game));
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
    let mut seen2 = Vec::new();
    prepare_with(&req, Hub::StrixHalo, &mut fake, Path::new("/h"), &mut |s| {
        seen2.push(s)
    })
    .unwrap();
    assert!(!seen2.contains(&Stage::Game));
    assert!(!fake.ran.contains(&Stage::Recompile));
}

#[test]
fn mobile_requests_never_fall_back_to_a_desktop_build() {
    {
        let (target, hub) = (Os::Android, Hub::StrixHalo);
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
fn process_runner_keeps_stdout_and_stderr_lines_separate() {
    // The system's shell, so the test needs nothing a build machine may lack.
    let command = if cfg!(windows) {
        ["cmd", "/C", "echo stdout& echo stderr>&2& exit /b 1"]
    } else {
        ["sh", "-c", "printf stdout; printf stderr >&2; exit 1"]
    }
    .map(str::to_owned);
    let log = ProcessRunner::new(std::env::temp_dir(), 0)
        .run(Stage::Shaders, &command)
        .unwrap_err();
    assert_eq!(log, vec!["stdout", "stderr"]);
}

/// A runner that starts nothing: the Rust shaders stage must not need a script.
struct Idle;

impl ScriptRunner for Idle {
    fn run(&mut self, _stage: Stage, _args: &[String]) -> Result<(), Vec<String>> {
        Ok(())
    }

    fn tools(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn game(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn version(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn recompile(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn fonts(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn build(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }
}

fn prepare_error(req: &Request) -> String {
    prepare_with(
        req,
        Hub::StrixHalo,
        &mut Idle,
        Path::new("/home/me"),
        &mut |_| {},
    )
    .unwrap_err()
    .to_string()
}

#[test]
fn prepare_reaches_the_rust_sarc_fix_for_a_mod() {
    // The shaders stage runs `sarc::fix_archives` on each mod's romfs: an archive that is
    // not a SARC is refused with the sarc reader's own message.
    let req = request("sarc-live", Os::Linux);
    let mod_romfs = req.out.join("mods").join("tr").join("romfs");
    std::fs::create_dir_all(&mod_romfs).unwrap();
    std::fs::write(mod_romfs.join("bad.sarc.zs"), b"not an archive").unwrap();

    let message = prepare_error(&req);
    assert!(message.contains("stage shaders"), "{message}");
    assert!(
        message.contains("SARC archive")
            || message.contains("zstd")
            || message.contains("translator"),
        "{message}"
    );
}

#[test]
fn prepare_reaches_the_rust_shader_extraction() {
    // With a RomFS and no translator installed, the stage runs the Rust extraction (it makes
    // `shaders_extracted`) and then refuses plainly, without starting the script.
    let mut req = request("shaders-live", Os::Linux);
    let romfs = req.out.join("romfs");
    std::fs::create_dir_all(&romfs).unwrap();
    req.game.romfs = romfs;

    let message = prepare_error(&req);
    assert!(req.out.join("shaders_extracted").exists(), "{message}");
    assert!(message.contains("stage shaders"), "{message}");
}

// ---- The real runner (`ProcessRunner`) on a stand-in for upstream's script ----

/// What upstream's script prints after `== ` as each stage begins (`tools/setup_acnh.py`).
const SAYS: [&str; 7] = [
    "tools",
    "game (the game folder: ExeFS + RomFS, add-on content)",
    "version",
    "recompile (main, sdk, subsdk0)",
    "shaders (Maxwell -> SPIR-V)",
    "fonts (button icons)",
    "build (-j2; the first build compiles ~5 CPU-hours of code)",
];

/// The files a stage leaves in the build folder (tools: none there).
fn outputs(out: &Path, stage: Stage) -> Vec<PathBuf> {
    let modules = ["main", "sdk", "subsdk0"];
    match stage {
        Stage::Tools => vec![],
        Stage::Game => modules
            .iter()
            .map(|m| out.join("analysis").join(format!("{m}.bin")))
            .collect(),
        Stage::Version => vec![out.join("version.ini")],
        Stage::Recompile => modules
            .iter()
            .map(|m| out.join("gen").join(m).join("table.cpp"))
            .collect(),
        Stage::Shaders => vec![out.join("spirv").join("summary.txt")],
        Stage::Fonts => {
            vec![out.join("fonts/glyphs_xbox/console/romfs/Font/ScalableFont.sarc.zs")]
        }
        Stage::Build => vec![out.join("build").join("acnh")],
    }
}

fn make(paths: &[PathBuf]) {
    for path in paths {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "synthetic").unwrap();
    }
}

fn make_completed_stage(out: &Path, stage: Stage) {
    make(&outputs(out, stage));
    if stage == Stage::Recompile {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let hash = recomp_hash(&root).unwrap();
        std::fs::write(out.join("gen/.recomp-hash"), format!("{hash}\n")).unwrap();
    }
}

/// Wait (at most 20 s) for `path` to be there.
fn wait_for(path: &Path) -> bool {
    let start = Instant::now();
    while !path.is_file() {
        if start.elapsed() > Duration::from_secs(20) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

/// Not a test of its own: the stand-in for upstream's script. The tests below start this
/// test binary again with `setup:<out>` among its arguments, and then it does what
/// `tools/setup_acnh.py` does, with made-up files: one pass over every stage, `== <stage>`
/// as each begins, a stage's work skipped when its output is there, exit 1 where
/// `fail:<stage>` says. Before it works on a stage it waits for `<out>/go-<stage>`, which a
/// test's progress callback makes: work nobody reported never gets done (`eager`: no wait).
/// It counts its starts in `starts.txt` of the folder it runs in.
#[test]
fn stand_in_setup() {
    let args: Vec<String> = std::env::args().collect();
    let Some(out) = args.iter().find_map(|a| a.strip_prefix("setup:")) else {
        return;
    };
    let out = Path::new(out);
    let fail = args.iter().find_map(|a| a.strip_prefix("fail:"));
    let eager = args.iter().any(|a| a == "eager");
    let say = |line: &str| writeln!(std::io::stdout(), "{line}").unwrap();
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("starts.txt")
        .and_then(|mut f| f.write_all(b"start\n"))
        .unwrap();
    std::fs::write(
        out.join("jobs.txt"),
        std::env::var("JOBS").unwrap_or_default(),
    )
    .unwrap();
    for (stage, name) in STAGES.into_iter().zip(SAYS) {
        say(&format!("\n== {name}"));
        if fail == Some(stage.key()) {
            say("boom at /home/me/x");
            writeln!(std::io::stderr(), "trace of {}", stage.key()).unwrap();
            std::process::exit(1);
        }
        let missing: Vec<PathBuf> = outputs(out, stage)
            .into_iter()
            .filter(|path| !path.is_file())
            .collect();
        if missing.is_empty() {
            continue;
        }
        if !eager && !wait_for(&out.join(format!("go-{}", stage.key()))) {
            say(&format!("nobody reported {}", stage.key()));
            std::process::exit(3);
        }
        make(&missing);
    }
    say("\n== done: synthetic");
    std::fs::write(out.join("ended.txt"), "ended").unwrap();
    std::process::exit(0);
}

/// The real `ProcessRunner`, in the build folder, started on the stand-in instead of
/// upstream's command.
struct StandIn {
    runner: ProcessRunner,
    command: Vec<String>,
}

fn stand_in(req: &Request, jobs: u32, more: &[&str]) -> StandIn {
    let mut command = vec![
        std::env::current_exe().unwrap().display().to_string(),
        "--exact".to_owned(),
        "stand_in_setup".to_owned(),
        "--nocapture".to_owned(),
        format!("setup:{}", req.out.display()),
    ];
    command.extend(more.iter().map(|a| (*a).to_owned()));
    StandIn {
        runner: ProcessRunner::new(req.out.clone(), jobs),
        command,
    }
}

impl ScriptRunner for StandIn {
    fn run(&mut self, stage: Stage, _args: &[String]) -> Result<(), Vec<String>> {
        self.runner.run(stage, &self.command)
    }

    fn tools(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn game(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn version(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn recompile(&mut self, _req: &Request) -> Result<(), Error> {
        self.run(Stage::Recompile, &[]).map_err(|log| {
            let named = log
                .first()
                .and_then(|l| l.strip_prefix("== "))
                .and_then(|rest| rest.split_whitespace().next())
                .unwrap_or("recompile");
            let text = log.join("\n").replace("/home/me", "~");
            Error::Build(format!("stage {named}: {text}"))
        })
    }

    fn finish(&mut self) -> Result<(), Vec<String>> {
        self.runner.finish()
    }

    fn shaders(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn fonts(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn build(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }
}

/// A progress callback that records the stage and lets the stand-in work on it.
fn report<'a>(req: &'a Request, seen: &'a mut Vec<Stage>) -> impl FnMut(Stage) + 'a {
    move |stage| {
        seen.push(stage);
        std::fs::write(req.out.join(format!("go-{}", stage.key())), "").unwrap();
    }
}

fn starts(req: &Request) -> usize {
    std::fs::read_to_string(req.out.join("starts.txt"))
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

#[test]
fn one_run_of_the_script_is_followed_stage_by_stage() {
    let req = request("followed", Os::Linux);
    let mut seen = vec![];
    let out = prepare_with(
        &req,
        Hub::StrixHalo,
        &mut stand_in(&req, 2, &[]),
        Path::new("/home/me"),
        &mut report(&req, &mut seen),
    )
    .unwrap();
    assert_eq!(out, req.out);
    // The script works on a stage only once it was reported, so each was, in order, while
    // the one run went on; and the run was over when prepare came back.
    assert_eq!(seen, STAGES.to_vec());
    assert_eq!(starts(&req), 1);
    assert!(req.out.join("ended.txt").is_file());
    assert_eq!(
        std::fs::read_to_string(req.out.join("jobs.txt")).unwrap(),
        "2"
    );
}

#[test]
fn stages_the_script_is_already_past_are_still_reported_once() {
    let req = request("eager", Os::Linux);
    let mut seen = vec![];
    prepare_with(
        &req,
        Hub::StrixHalo,
        &mut stand_in(&req, 0, &["eager"]),
        Path::new("/h"),
        &mut |stage| {
            if stage == Stage::Shaders {
                assert!(wait_for(&req.out.join("ended.txt")));
            }
            seen.push(stage);
        },
    )
    .unwrap();
    assert_eq!(seen, STAGES.to_vec());
    assert_eq!(starts(&req), 1);
}

#[test]
fn a_failing_script_run_names_the_stage_it_failed_in() {
    let req = request("fail-recomp", Os::Linux);
    let mut seen = vec![];
    let err = prepare_with(
        &req,
        Hub::StrixHalo,
        &mut stand_in(&req, 0, &["fail:recompile"]),
        Path::new("/home/me"),
        &mut report(&req, &mut seen),
    )
    .unwrap_err();
    match err {
        Error::Build(m) => {
            assert!(m.starts_with("stage recompile: == recompile"), "{m}");
            assert!(m.contains("boom at ~/x"), "{m}");
            assert!(m.contains("trace of recompile"), "{m}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(seen, STAGES[..4].to_vec());
    assert_eq!(starts(&req), 1);
    assert!(!req.out.join("build").exists());
}

#[test]
fn a_complete_build_folder_starts_no_script() {
    let req = request("complete", Os::Linux);
    std::fs::write(req.out.join(".prepare-tools.json"), "{}").unwrap();
    for stage in STAGES {
        make_completed_stage(&req.out, stage);
    }
    let mut seen = vec![];
    prepare_with(
        &req,
        Hub::StrixHalo,
        &mut stand_in(&req, 0, &[]),
        Path::new("/h"),
        &mut report(&req, &mut seen),
    )
    .unwrap();
    assert!(seen.is_empty());
    assert_eq!(starts(&req), 0);
}

#[test]
fn one_missing_output_is_one_reported_stage_and_the_run_ends_first() {
    let req = request("recomp-only", Os::Linux);
    std::fs::write(req.out.join(".prepare-tools.json"), "{}").unwrap();
    for stage in STAGES {
        if stage != Stage::Recompile {
            make_completed_stage(&req.out, stage);
        }
    }
    let mut seen = vec![];
    prepare_with(
        &req,
        Hub::StrixHalo,
        &mut stand_in(&req, 0, &[]),
        Path::new("/h"),
        &mut report(&req, &mut seen),
    )
    .unwrap();
    assert_eq!(seen, vec![Stage::Recompile]);
    assert_eq!(starts(&req), 1);
    assert!(outputs(&req.out, Stage::Recompile)[0].is_file());
    assert!(req.out.join("ended.txt").is_file());
}

#[test]
fn a_failure_in_a_stage_we_did_not_wait_on_is_named_by_the_scripts_line() {
    for (missing, fail, named) in [
        (Stage::Recompile, "fail:tools", "stage tools: == tools"),
        (Stage::Recompile, "fail:build", "stage build: == build"),
    ] {
        let req = request(fail.trim_start_matches("fail:"), Os::Linux);
        std::fs::write(req.out.join(".prepare-tools.json"), "{}").unwrap();
        for stage in STAGES {
            if stage != missing {
                make_completed_stage(&req.out, stage);
            }
        }
        let mut seen = vec![];
        let err = prepare_with(
            &req,
            Hub::StrixHalo,
            &mut stand_in(&req, 0, &[fail]),
            Path::new("/h"),
            &mut report(&req, &mut seen),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::Build(m) if m.starts_with(named)),
            "{err:?}"
        );
        assert_eq!(seen, vec![missing]);
    }
}

// ---- The toolchain check and a packaged source (issue 1.22a) ----

fn toolchain_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("acnh-tool-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn file(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "synthetic").unwrap();
}

fn find<'a>(missing: &'a [Missing], what: &str) -> Option<&'a Missing> {
    missing.iter().find(|piece| piece.what == what)
}

#[test]
fn mac_finds_homebrew_off_path_and_names_a_missing_molten_vk() {
    let base = toolchain_dir("mac-brew");
    let bin = base.join("app/usr/bin");
    for tool in ["clang", "clang++", "curl", "python3", "xcode-select"] {
        file(&bin.join(tool));
    }
    // No brew on PATH: the fake prefix is found through toolchain_with, as the Finder's
    // missing PATH is handled by the real /opt/homebrew.
    let prefix = base.join("homebrew");
    file(&prefix.join("bin/brew"));
    std::fs::create_dir_all(prefix.join("opt/sdl3")).unwrap();
    std::fs::create_dir_all(prefix.join("opt/vulkan-loader")).unwrap();
    std::fs::create_dir_all(prefix.join("opt/libvpx")).unwrap();
    std::fs::create_dir_all(prefix.join("opt/libvorbis")).unwrap();

    let missing = toolchain_with(Os::Mac, &bin, std::slice::from_ref(&prefix));
    let molten = find(&missing, "molten-vk").expect("molten-vk is named");
    assert!(
        molten.message.contains("brew install molten-vk"),
        "{molten:?}"
    );
    assert!(!molten.warning);
    assert!(find(&missing, "Homebrew").is_none(), "{missing:?}");
}

#[test]
fn linux_names_a_missing_clang_and_warns_about_missing_libvpx() {
    let base = toolchain_dir("linux-clang");
    let bin = base.join("usr/bin");
    for tool in ["clang++", "curl", "python3"] {
        file(&bin.join(tool));
    }
    let prefix = base.join("usr");
    file(&prefix.join("include/SDL3/SDL.h"));

    let missing = toolchain_with(Os::Linux, &bin, &[]);
    let clang = find(&missing, "clang").expect("clang is named");
    assert!(!clang.warning);
    assert!(
        clang.install.contains("sudo apt install clang"),
        "{clang:?}"
    );
    let vpx = find(&missing, "libvpx").expect("libvpx is named");
    assert!(vpx.warning, "{vpx:?}");
}

#[test]
fn a_complete_toolchain_has_nothing_missing() {
    let base = toolchain_dir("linux-complete");
    let bin = base.join("usr/bin");
    for tool in ["clang", "clang++", "curl", "python3"] {
        file(&bin.join(tool));
    }
    let prefix = base.join("usr");
    file(&prefix.join("include/SDL3/SDL.h"));
    file(&prefix.join("lib/pkgconfig/vpx.pc"));
    file(&prefix.join("lib/pkgconfig/vorbis.pc"));

    assert_eq!(toolchain_with(Os::Linux, &bin, &[]), Vec::new());
}

/// A Homebrew-style `python3`: `<prefix>/bin/python3` links into the Cellar, and the
/// `<prefix>/lib/python3.N` beside the link holds only `site-packages`. The standard library
/// (and its `venv`) is under the link's target.
#[cfg(unix)]
fn linked_python(name: &str, with_venv: bool) -> Vec<Missing> {
    let base = toolchain_dir(name);
    let prefix = base.join("usr");
    let bin = prefix.join("bin");
    for tool in ["clang", "clang++", "curl"] {
        file(&bin.join(tool));
    }
    file(&prefix.join("include/SDL3/SDL.h"));
    file(&prefix.join("lib/pkgconfig/vpx.pc"));
    file(&prefix.join("lib/pkgconfig/vorbis.pc"));
    file(&prefix.join("lib/python3.15/site-packages/placeholder"));
    let framework = base.join("Cellar/python/3.15.2/Frameworks/Python.framework/Versions/3.15");
    file(&framework.join("bin/python3.15"));
    let stdlib = framework.join("lib/python3.15");
    if with_venv {
        file(&stdlib.join("venv/__init__.py"));
    } else {
        file(&stdlib.join("os.py"));
    }
    std::os::unix::fs::symlink(framework.join("bin/python3.15"), bin.join("python3")).unwrap();
    toolchain_with(Os::Linux, &bin, &[])
}

#[cfg(unix)]
#[test]
fn a_linked_python_finds_venv_under_the_link_target() {
    let missing = linked_python("linked-venv", true);
    assert!(
        find(&missing, "python3's venv module").is_none(),
        "{missing:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_linked_python_without_venv_is_still_named() {
    let missing = linked_python("linked-no-venv", false);
    assert!(
        find(&missing, "python3's venv module").is_some(),
        "{missing:?}"
    );
}

/// A fake installed layout: `<base>/app/usr/bin/acnh-app` and the source under
/// `<base>/app/usr/share/acnh-universal/source`.
fn installed(name: &str, commit: &str) -> (PathBuf, PathBuf, AppDirs) {
    let base = toolchain_dir(name);
    let exe = base.join("app/usr/bin/acnh-app");
    file(&exe);
    let source = base.join("app/usr/share/acnh-universal/source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("SOURCE"), format!("{commit}\n")).unwrap();
    std::fs::write(source.join("CMakeLists.txt"), "synthetic source").unwrap();
    let dirs = AppDirs::new(&Vars {
        home: base.join("home"),
        data: base.join("data"),
        config: base.join("config"),
    });
    (base, exe, dirs)
}

#[test]
fn a_packaged_source_is_copied_to_the_data_folder_and_the_runner_runs_there() {
    let req = request("installed-copy", Os::Linux);
    let (_base, exe, dirs) = installed("installed-copy", "abc123");
    let mut fake = Fake::default();
    let mut seen = Vec::new();
    let out = prepare_installed(
        &req,
        Hub::StrixHalo,
        &exe,
        &dirs,
        Some("abc123"),
        &[],
        &mut fake,
        Path::new("/home/me"),
        &mut |stage| seen.push(stage),
    )
    .unwrap();
    assert_eq!(out, req.out);
    let copy = dirs.data.join("source/abc123");
    assert!(copy.join("CMakeLists.txt").is_file());
    assert_eq!(fake.root.as_deref(), Some(copy.as_path()));
    assert_eq!(fake.ran, scripted());
    assert_eq!(seen, STAGES.to_vec());
}

#[test]
fn a_packaged_source_from_another_commit_is_refused() {
    let req = request("installed-other", Os::Linux);
    let (_base, exe, dirs) = installed("installed-other", "abc123");
    let mut fake = Fake::default();
    let error = prepare_installed(
        &req,
        Hub::StrixHalo,
        &exe,
        &dirs,
        Some("different"),
        &[],
        &mut fake,
        Path::new("/home/me"),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(
        matches!(&error, Error::Io(message) if message.contains("abc123") && message.contains("different")),
        "{error:?}"
    );
    assert!(!dirs.data.join("source/abc123").exists());
    assert!(fake.ran.is_empty());
}

/// A runner that keeps the default `shaders` step and a source folder given by `set_root`, as
/// an installed launcher's runner does.
#[derive(Default)]
struct Rooted {
    root: Option<PathBuf>,
}

impl ScriptRunner for Rooted {
    fn run(&mut self, _stage: Stage, _args: &[String]) -> Result<(), Vec<String>> {
        Ok(())
    }

    fn tools(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn game(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn version(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn recompile(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn fonts(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn build(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn set_root(&mut self, root: &Path) {
        self.root = Some(root.to_owned());
    }

    fn root(&self) -> PathBuf {
        self.root.clone().expect("root set")
    }
}

#[test]
fn shaders_step_looks_for_its_tools_under_the_runners_root() {
    let mut req = request("shaders-root", Os::Linux);
    let romfs = req.out.join("romfs");
    std::fs::create_dir_all(&romfs).unwrap();
    req.game.romfs = romfs;
    let root = req.out.join("installed-source");
    std::fs::create_dir_all(&root).unwrap();

    let mut runner = Rooted::default();
    runner.set_root(&root);
    let message = prepare_with(
        &req,
        Hub::StrixHalo,
        &mut runner,
        Path::new("/home/me"),
        &mut |_| {},
    )
    .unwrap_err()
    .to_string();
    // The refusal names the runner's root, not the checkout this test was built in.
    assert!(message.contains("installed-source"), "{message}");
}

#[test]
fn speedfix_patch_selection_preserves_sources_and_rejects_old_output_after_game_change() {
    let dir = std::env::temp_dir().join(format!("acnh-speedfix-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let game = dir.join("game");
    let out = dir.join("out");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::create_dir_all(&out).unwrap();
    let old = out.join("speedfix_main.pchtxt");
    std::fs::write(&old, "synthetic old patch").unwrap();
    assert_eq!(
        speedfix_patch(&game, &out, "3.0.3", false),
        Some(old.clone())
    );
    assert_eq!(speedfix_patch(&game, &out, "3.0.3", true), None);
    let patch302 = game.join("speedfix_main_302.pchtxt");
    std::fs::write(&patch302, "synthetic patch").unwrap();
    assert_eq!(
        speedfix_patch(&game, &out, "3.0.2", true),
        Some(patch302.clone())
    );
    assert!(old.is_file() && patch302.is_file());
    assert_eq!(
        speedfix_args(false, "main", "3.0.2", &game, &out, true).unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        speedfix_args(true, "main", "3.0.2", &game, &out, true).unwrap(),
        vec!["--variant".to_owned(), patch302.display().to_string()]
    );
    for version in ["1.0.0", "2.0.7"] {
        assert_eq!(
            speedfix_args(true, "main", version, &game, &out, true).unwrap(),
            Vec::<String>::new()
        );
    }
    assert_eq!(
        speedfix_args(true, "sdk", "3.0.2", &game, &out, true).unwrap(),
        Vec::<String>::new()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn incomplete_mod_archive_preserves_previous_and_opt_in_then_off_preserves_other_mods() {
    let dir = std::env::temp_dir().join(format!("acnh-mods-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let game = dir.join("game");
    let out = dir.join("out");
    let root = dir.join("repo");
    let installed = out.join("mods/ptbr/romfs/previous.txt");
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::fs::write(&installed, "previous translation").unwrap();
    let bad_zip = dir.join("bad.zip");
    // Empty zip with only romfs/ directory entry and no files.
    let status = std::process::Command::new("python3")
        .args([
            "-c",
            "import sys, zipfile; z=zipfile.ZipFile(sys.argv[1],'w'); z.writestr('romfs/','')",
            &bad_zip.display().to_string(),
        ])
        .status()
        .unwrap();
    assert!(status.success());
    assert!(install_mod_archive(&out.join("mods"), "ptbr", &bad_zip, &dir, "python3").is_err());
    assert_eq!(
        std::fs::read_to_string(&installed).unwrap(),
        "previous translation"
    );

    // Clean ptbr and test opt-in then off preserving other mods.
    std::fs::remove_dir_all(out.join("mods/ptbr")).unwrap();
    std::fs::create_dir_all(&game).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    let ptbr_zip = game.join("ptbr.zip");
    let status = std::process::Command::new("python3")
        .args([
            "-c",
            "import sys, zipfile; z=zipfile.ZipFile(sys.argv[1],'w'); z.writestr('romfs/synthetic.txt','translation')",
            &ptbr_zip.display().to_string(),
        ])
        .status()
        .unwrap();
    assert!(status.success());
    let other = out.join("mods/other/romfs/file");
    std::fs::create_dir_all(other.parent().unwrap()).unwrap();
    std::fs::write(&other, "other").unwrap();

    install_mods(&out, &dir, &game, &root, false, &[], "python3").unwrap();
    assert!(!out.join("mods/ptbr").exists());
    install_mods(&out, &dir, &game, &root, true, &[], "python3").unwrap();
    assert_eq!(
        std::fs::read_to_string(out.join("mods/ptbr/romfs/synthetic.txt")).unwrap(),
        "translation"
    );
    install_mods(&out, &dir, &game, &root, false, &[], "python3").unwrap();
    assert!(!out.join("mods/ptbr").exists());
    assert!(ptbr_zip.is_file());
    assert_eq!(std::fs::read_to_string(&other).unwrap(), "other");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rust_version_reads_build_id_and_writes_version_ini() {
    let mut req = request("version", Os::Linux);
    let exefs = req.out.join("exefs");
    std::fs::create_dir_all(&exefs).unwrap();
    let mut nso = vec![0u8; 0x60];
    nso[..4].copy_from_slice(b"NSO0");
    // 3.0.2 build id: FCD2BB238ABE99E925B4E452DF1F41F1
    let bid = [
        0xFC, 0xD2, 0xBB, 0x23, 0x8A, 0xBE, 0x99, 0xE9, 0x25, 0xB4, 0xE4, 0x52, 0xDF, 0x1F, 0x41,
        0xF1,
    ];
    nso[0x40..0x50].copy_from_slice(&bid);
    std::fs::write(exefs.join("main"), &nso).unwrap();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    rust_version(&req, &repo).unwrap();
    let ini = std::fs::read_to_string(req.out.join("version.ini")).unwrap();
    assert!(
        ini.contains("build_id = FCD2BB238ABE99E925B4E452DF1F41F1"),
        "{ini}"
    );
    assert!(ini.contains("version = 3.0.2"), "{ini}");
    assert!(ini.contains("hooks = games/acnh/hooks.txt"), "{ini}");
    req.game.root = PathBuf::from("/game");
}

/// A dropbox folder with `_transfers/ACNH-Universal/` in it, and the request that delivers to
/// its `ios` folder.
fn ios_request(name: &str) -> (Request, PathBuf) {
    let mut req = request(name, Os::Ios);
    let dropbox = req.out.join("dropbox");
    std::fs::create_dir_all(dropbox.join("_transfers/ACNH-Universal")).unwrap();
    let config = Config::parse(&format!("dropbox = {}\n", dropbox.display())).unwrap();
    let delivery = delivery_folder(&config, Path::new("/Users/user"), "ios");
    req.delivery = Some(delivery.clone());
    (req, delivery)
}

#[test]
fn the_delivery_folder_is_the_dropbox_keys_or_homes() {
    let home = Path::new("/Users/user");
    assert_eq!(
        delivery_folder(&Config::default(), home, "ios"),
        Path::new("/Users/user/Dropbox/_transfers/ACNH-Universal/ios")
    );
    assert_eq!(
        delivery_folder(&Config::default(), home, "mac"),
        Path::new("/Users/user/Dropbox/_transfers/ACNH-Universal/mac")
    );
    let keyed = Config::parse("dropbox = /Volumes/Cloud\n").unwrap();
    assert_eq!(
        delivery_folder(&keyed, home, "ios"),
        Path::new("/Volumes/Cloud/_transfers/ACNH-Universal/ios")
    );
    // A key that is empty is not set.
    let empty = Config::parse("dropbox =\n").unwrap();
    assert_eq!(
        delivery_folder(&empty, home, "ios"),
        Path::new("/Users/user/Dropbox/_transfers/ACNH-Universal/ios")
    );
    assert_eq!(delivery_subfolder(Os::Ios), Some("ios"));
    assert_eq!(delivery_subfolder(Os::Mac), Some("mac"));
    assert_eq!(delivery_subfolder(Os::Linux), None);
}

#[test]
fn ios_runs_the_stages_the_packs_and_the_two_scripts() {
    let (mut req, delivery) = ios_request("ios-order");
    // The game's RomFS and one translated shader, so the two packs can be made.
    let romfs = req.out.join("game-romfs");
    std::fs::create_dir_all(&romfs).unwrap();
    std::fs::write(romfs.join("a.bin"), b"a").unwrap();
    req.game.romfs = romfs;
    std::fs::create_dir_all(req.out.join("spirv")).unwrap();
    std::fs::write(req.out.join("spirv/a.spv"), b"spv").unwrap();
    let mut fake = Fake::with_ios_tools();
    let mut seen = vec![];
    let ipa = prepare_with(&req, Hub::Mac, &mut fake, Path::new("/h"), &mut |s| {
        seen.push(s)
    })
    .unwrap();
    assert_eq!(seen, STAGES.to_vec());
    assert!(req.out.join("romfs.pak").is_file());
    assert!(req.out.join("spirv.pak").is_file());
    let mut stages = scripted();
    stages.extend([Stage::Build, Stage::Build]);
    assert_eq!(fake.ran, stages);
    let n = fake.args.len();
    let build = req.out.join("build-ios");
    assert_eq!(
        without_env(&fake.args[n - 2]),
        [
            "tools/build_ios.sh".to_owned(),
            "iphoneos".to_owned(),
            req.out.join("gen").display().to_string(),
            build.display().to_string(),
        ]
    );
    assert_eq!(
        fake.args[n - 1],
        [
            "tools/package_ios.sh".to_owned(),
            env!("CARGO_PKG_VERSION").to_owned(),
            build.display().to_string(),
            "--game".to_owned(),
            req.out.display().to_string(),
            delivery.display().to_string(),
        ]
    );
    assert_eq!(
        ipa,
        delivery.join(format!("ACNH-Universal-{}.ipa", env!("CARGO_PKG_VERSION")))
    );
}

#[test]
fn ios_without_the_transfers_folder_stops_before_the_first_stage() {
    let (mut req, _) = ios_request("ios-no-transfers");
    let home = Path::new("/Users/user");
    req.delivery = Some(delivery_folder(&Config::default(), home, "ios"));
    let mut fake = Fake::with_ios_tools();
    let mut seen = vec![];
    let error = prepare_with(&req, Hub::Mac, &mut fake, home, &mut |s| seen.push(s)).unwrap_err();
    match error {
        Error::Build(message) => {
            assert!(message.contains("`dropbox`"), "{message}");
            // The home folder is `~` and the separators are the platform's.
            assert!(message.starts_with("the folder ~"), "{message}");
            assert!(message.contains("ACNH-Universal"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    assert!(fake.ran.is_empty() && seen.is_empty());
}

#[test]
fn ios_without_the_iphoneos_sdk_stops_before_the_first_stage() {
    let (req, _) = ios_request("ios-no-sdk");
    // The command-line tools alone: `xcrun` fails for the iphoneos SDK.
    let mut fake = Fake::default();
    let mut seen = vec![];
    let error = prepare_with(&req, Hub::Mac, &mut fake, Path::new("/h"), &mut |s| {
        seen.push(s)
    })
    .unwrap_err();
    assert!(matches!(error, Error::Build(message) if message.contains("Xcode")));
    assert!(fake.probed[0].starts_with("xcrun --sdk iphoneos --show-sdk-path"));
    assert!(fake.ran.is_empty() && seen.is_empty());
}

#[test]
fn ios_without_cargo_or_its_rust_target_stops_before_the_first_stage() {
    let (req, _) = ios_request("ios-no-rust");
    let mut no_cargo = Fake::with_ios_tools();
    no_cargo.tools.retain(|(name, _)| *name != "cargo");
    let error =
        prepare_with(&req, Hub::Mac, &mut no_cargo, Path::new("/h"), &mut |_| {}).unwrap_err();
    assert!(matches!(error, Error::Build(message) if message.contains("cargo")));
    assert!(no_cargo.ran.is_empty());

    let mut no_target = Fake::with_ios_tools();
    no_target.tools.retain(|(name, _)| *name != "rustup");
    no_target
        .tools
        .push(("rustup", "aarch64-apple-darwin\naarch64-apple-ios-sim\n"));
    let error =
        prepare_with(&req, Hub::Mac, &mut no_target, Path::new("/h"), &mut |_| {}).unwrap_err();
    assert!(matches!(
        error,
        Error::Build(message) if message.contains("rustup target add aarch64-apple-ios")
    ));
    assert!(no_target.ran.is_empty());
}

#[test]
fn ios_on_another_hub_is_unsupported_and_names_the_mac() {
    for hub in [Hub::StrixHalo, Hub::Desktop] {
        let (req, _) = ios_request("ios-wrong-hub");
        let mut fake = Fake::with_ios_tools();
        let error = prepare_with(&req, hub, &mut fake, Path::new("/h"), &mut |_| {}).unwrap_err();
        assert!(matches!(error, Error::Unsupported(message) if message.contains("the Mac")));
        assert!(fake.ran.is_empty() && fake.probed.is_empty());
    }
}

#[test]
fn ios_needs_a_delivery_folder() {
    let mut req = request("ios-no-delivery", Os::Ios);
    req.delivery = None;
    let mut fake = Fake::with_ios_tools();
    let error = prepare_with(&req, Hub::Mac, &mut fake, Path::new("/h"), &mut |_| {}).unwrap_err();
    assert!(matches!(error, Error::Env(_)));
    assert!(fake.ran.is_empty());
}

/// The macOS job's runner: the shared stages are faked, everything else runs.
struct StagesFaked(ProcessRunner);

impl ScriptRunner for StagesFaked {
    fn run(&mut self, stage: Stage, args: &[String]) -> Result<(), Vec<String>> {
        self.0.run(stage, args)
    }

    fn tools(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn game(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn version(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn recompile(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn shaders(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn fonts(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn build(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
    }

    fn root(&self) -> PathBuf {
        self.0.root()
    }

    fn finish(&mut self) -> Result<(), Vec<String>> {
        self.0.finish()
    }
}

/// macOS job only (`ACNH_IOS_PREPARE_DIR` names a scratch folder outside the checkout): the
/// stages are faked, the two scripts are real, on the stub gen folder, and the delivery
/// folder is under the scratch folder. An `.ipa` is made there.
#[test]
fn ios_real_scripts_make_an_ipa_on_the_stub_gen() {
    let Some(scratch) = std::env::var_os("ACNH_IOS_PREPARE_DIR").map(PathBuf::from) else {
        eprintln!("skipped: ACNH_IOS_PREPARE_DIR is not set");
        return;
    };
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let _ = std::fs::remove_dir_all(&scratch);
    let out = scratch.join("prepared");
    let delivery = scratch.join("Dropbox/_transfers/ACNH-Universal/ios");
    std::fs::create_dir_all(delivery.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join(".fan-mod-settings"), "false false\n").unwrap();
    // No game is prepared here: empty files stand in for what the packaging script reads
    // (it allows only empty files under CI).
    std::fs::create_dir_all(out.join("analysis")).unwrap();
    for file in ["analysis/main.bin", "romfs.pak", "spirv.pak"] {
        std::fs::write(out.join(file), b"").unwrap();
    }
    // The stub gen folder stands in for the recompiled code.
    let status = std::process::Command::new("cp")
        .arg("-R")
        .arg(repo.join("tests/ios_stub_gen"))
        .arg(out.join("gen"))
        .status()
        .unwrap();
    assert!(status.success());
    let mut req = request("ios-real", Os::Ios);
    req.out = out;
    req.delivery = Some(delivery.clone());
    let mut runner = StagesFaked(ProcessRunner::new(repo, 0));
    let ipa = prepare_with(&req, Hub::Mac, &mut runner, Path::new("/h"), &mut |_| {}).unwrap();
    assert!(ipa.is_file(), "{} was not made", ipa.display());
    assert!(ipa.starts_with(&delivery));
}

#[cfg(unix)]
#[test]
fn cargo_found_only_under_the_cargo_home_bin_is_put_first_on_the_build_path() {
    let (req, _) = ios_request("ios-cargo-home");
    let mut fake = Fake::with_ios_tools();
    fake.tools
        .retain(|(name, _)| !matches!(*name, "cargo" | "rustup"));
    fake.tools.push(("/h/.cargo/bin/cargo", "cargo 1.90.0\n"));
    fake.tools
        .push(("/h/.cargo/bin/rustup", "aarch64-apple-ios\n"));
    prepare_with(&req, Hub::Mac, &mut fake, Path::new("/h"), &mut |_| {}).unwrap();
    let build = &fake.args[fake.args.len() - 2];
    assert_eq!(build[0], "env");
    assert!(build[1].starts_with("PATH=/h/.cargo/bin:"), "{}", build[1]);
    assert_eq!(build[2], "tools/build_ios.sh");
    // With cargo on the PATH the script is called as it is.
    let mut plain = Fake::with_ios_tools();
    prepare_with(&req, Hub::Mac, &mut plain, Path::new("/h"), &mut |_| {}).unwrap();
    let plain_build = &plain.args[plain.args.len() - 2];
    assert!(!plain_build.iter().any(|arg| arg.contains("/h/.cargo/bin")));
    assert_eq!(without_env(plain_build)[0], "tools/build_ios.sh");
}

#[test]
fn ios_refuses_a_prepared_folder_inside_a_git_work_tree() {
    let (req, _) = ios_request("ios-in-git");
    let mut fake = Fake::with_ios_tools();
    fake.tools.push(("git", "true\n"));
    let mut seen = vec![];
    let error = prepare_with(&req, Hub::Mac, &mut fake, Path::new("/h"), &mut |s| {
        seen.push(s)
    })
    .unwrap_err();
    assert!(matches!(error, Error::Build(message) if message.contains("git work tree")));
    assert!(fake.ran.is_empty() && seen.is_empty());
    assert!(fake.probed.iter().any(|p| p.starts_with("git -C ")));
}

#[test]
fn a_relative_out_is_made_absolute_before_the_work_tree_check_and_the_steps() {
    let (mut req, _) = ios_request("ios-relative");
    req.out = PathBuf::from("relative-prepared");
    let mut fake = Fake::with_ios_tools();
    // The check is told the absolute folder, and says it is a work tree.
    fake.tools.push(("git", "true\n"));
    let error = prepare_with(&req, Hub::Mac, &mut fake, Path::new("/h"), &mut |_| {}).unwrap_err();
    assert!(matches!(error, Error::Build(message) if message.contains("git work tree")));
    let git = fake
        .probed
        .iter()
        .find(|p| p.starts_with("git -C "))
        .unwrap();
    let folder = git.split_whitespace().nth(2).unwrap();
    assert!(Path::new(folder).is_absolute(), "{git}");
    // Outside a work tree the steps name the absolute folder too.
    let mut fake = Fake::with_ios_tools();
    prepare_with(&req, Hub::Mac, &mut fake, Path::new("/h"), &mut |_| {}).unwrap();
    let build = without_env(&fake.args[fake.args.len() - 2]);
    assert!(Path::new(&build[2]).is_absolute(), "{build:?}");
    assert!(Path::new(&build[3]).is_absolute(), "{build:?}");
}

#[test]
fn the_work_tree_check_fails_closed_when_git_cannot_answer() {
    let (req, _) = ios_request("ios-git-fails");
    // Git is missing: neither answer nor a word that this is no repository.
    let mut missing = Fake::with_ios_tools();
    missing.tools.retain(|(name, _)| *name != "git:stderr");
    let error =
        prepare_with(&req, Hub::Mac, &mut missing, Path::new("/h"), &mut |_| {}).unwrap_err();
    assert!(matches!(error, Error::Build(message) if message.contains("cannot tell")));
    assert!(missing.ran.is_empty());
    // Git fails for another reason.
    let mut broken = Fake::with_ios_tools();
    broken.tools.retain(|(name, _)| *name != "git:stderr");
    broken
        .tools
        .push(("git:stderr", "fatal: detected dubious ownership\n"));
    let error =
        prepare_with(&req, Hub::Mac, &mut broken, Path::new("/h"), &mut |_| {}).unwrap_err();
    assert!(matches!(error, Error::Build(message) if message.contains("cannot tell")));
    assert!(broken.ran.is_empty());
}

/// A dropbox folder with `_transfers/ACNH-Universal/` in it, and the Mac request that delivers
/// to its `mac` folder and assembles in `<out>/install`.
fn mac_request(name: &str) -> (Request, PathBuf) {
    let mut req = request(name, Os::Mac);
    let dropbox = req.out.join("dropbox");
    std::fs::create_dir_all(dropbox.join("_transfers/ACNH-Universal")).unwrap();
    let config = Config::parse(&format!("dropbox = {}\n", dropbox.display())).unwrap();
    let delivery = delivery_folder(&config, Path::new("/Users/user"), "mac");
    req.delivery = Some(delivery.clone());
    req.install = Some(req.out.join("install"));
    (req, delivery)
}

#[test]
fn mac_skips_upstreams_native_executable() {
    let (req, _) = mac_request("native-mac");
    std::fs::create_dir_all(req.out.join("build")).unwrap();
    std::fs::write(req.out.join("build/acnh"), "synthetic executable").unwrap();
    let mut fake = Fake::with_mac_tools();
    prepare_with(&req, Hub::Mac, &mut fake, Path::new("/h"), &mut |_| {}).unwrap();
    assert!(!fake.ran[..fake.ran.len() - 1].contains(&Stage::Build));
}

#[test]
fn mac_runs_the_stages_the_packs_the_assembly_and_the_package_script() {
    let (req, delivery) = mac_request("mac-order");
    let romfs = req.out.join("game-romfs");
    let mut req = req;
    std::fs::create_dir_all(&romfs).unwrap();
    std::fs::write(romfs.join("a.bin"), b"a").unwrap();
    req.game.romfs = romfs;
    std::fs::create_dir_all(req.out.join("spirv")).unwrap();
    std::fs::write(req.out.join("spirv/a.spv"), b"spv").unwrap();
    let mut fake = Fake::with_mac_tools();
    let mut seen = vec![];
    let zip = prepare_with(&req, Hub::Mac, &mut fake, Path::new("/h"), &mut |s| {
        seen.push(s)
    })
    .unwrap();
    assert_eq!(seen, STAGES.to_vec());
    // The packs were missing, so they were made in the prepared folder.
    assert!(req.out.join("romfs.pak").is_file());
    assert!(req.out.join("spirv.pak").is_file());
    let mut stages = scripted();
    stages.push(Stage::Build);
    assert_eq!(fake.ran, stages);
    let install = req.out.join("install");
    assert_eq!(fake.assembled, [(req.out.join("build"), install.clone())]);
    assert_eq!(
        fake.args.last().unwrap(),
        &[
            "tools/package_mac.sh".to_owned(),
            env!("CARGO_PKG_VERSION").to_owned(),
            install.join("ACNH-Universal.app").display().to_string(),
            req.out.display().to_string(),
            delivery.display().to_string(),
        ]
    );
    assert_eq!(
        zip,
        delivery.join(format!("ACNH-Universal-{}.zip", env!("CARGO_PKG_VERSION")))
    );
}

#[test]
fn mac_keeps_packs_that_are_already_there() {
    let (req, _) = mac_request("mac-packs");
    std::fs::write(req.out.join("romfs.pak"), b"r").unwrap();
    std::fs::write(req.out.join("spirv.pak"), b"s").unwrap();
    let mut fake = Fake::with_mac_tools();
    prepare_with(&req, Hub::Mac, &mut fake, Path::new("/h"), &mut |_| {}).unwrap();
    assert_eq!(std::fs::read(req.out.join("romfs.pak")).unwrap(), b"r");
    assert_eq!(std::fs::read(req.out.join("spirv.pak")).unwrap(), b"s");
}

#[test]
fn the_mac_delivery_is_homes_dropbox_without_the_key_and_follows_it() {
    let home = Path::new("/Users/user");
    assert_eq!(
        delivery_folder(&Config::default(), home, "mac"),
        Path::new("/Users/user/Dropbox/_transfers/ACNH-Universal/mac/")
    );
    let keyed = Config::parse("dropbox = /Volumes/Cloud\n").unwrap();
    assert_eq!(
        delivery_folder(&keyed, home, "mac"),
        Path::new("/Volumes/Cloud/_transfers/ACNH-Universal/mac/")
    );
}

#[test]
fn mac_without_the_transfers_folder_stops_before_the_first_stage() {
    let (mut req, _) = mac_request("mac-no-transfers");
    let home = Path::new("/Users/user");
    req.delivery = Some(delivery_folder(&Config::default(), home, "mac"));
    let mut fake = Fake::with_mac_tools();
    let mut seen = vec![];
    let error = prepare_with(&req, Hub::Mac, &mut fake, home, &mut |s| seen.push(s)).unwrap_err();
    match error {
        Error::Build(message) => {
            assert!(message.contains("`dropbox`"), "{message}");
            assert!(message.contains("ACNH-Universal"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    assert!(fake.ran.is_empty() && seen.is_empty() && fake.assembled.is_empty());
}

#[test]
fn mac_needs_a_delivery_folder() {
    let (mut req, _) = mac_request("mac-no-delivery");
    req.delivery = None;
    let mut fake = Fake::with_mac_tools();
    let error = prepare_with(&req, Hub::Mac, &mut fake, Path::new("/h"), &mut |_| {}).unwrap_err();
    assert!(matches!(error, Error::Env(_)));
    assert!(fake.ran.is_empty());
}

#[test]
fn mac_refuses_a_prepared_folder_inside_a_git_work_tree() {
    let (req, _) = mac_request("mac-in-git");
    let mut fake = Fake::with_mac_tools();
    fake.tools.push(("git", "true\n"));
    let error = prepare_with(&req, Hub::Mac, &mut fake, Path::new("/h"), &mut |_| {}).unwrap_err();
    assert!(matches!(error, Error::Build(message) if message.contains("git work tree")));
    assert!(fake.ran.is_empty() && fake.assembled.is_empty());
}

#[test]
fn a_failed_package_script_is_a_build_error_after_the_assembly() {
    let (req, _) = mac_request("mac-package-fails");
    let mut fake = Fake::with_mac_tools();
    fake.fail_at = Some(Stage::Build);
    // The stages skip Build (the player is there); only the package script runs it.
    std::fs::create_dir_all(req.out.join("build")).unwrap();
    std::fs::write(req.out.join("build/acnh"), "x").unwrap();
    let error = prepare_with(&req, Hub::Mac, &mut fake, Path::new("/h"), &mut |_| {}).unwrap_err();
    assert!(matches!(error, Error::Build(_)));
    assert_eq!(fake.assembled.len(), 1);
}

fn seed_all_stages_without_recomp_hash(out: &Path) {
    std::fs::write(out.join(".prepare-tools.json"), "{}").unwrap();
    for stage in STAGES {
        make(&outputs(out, stage));
    }
}

#[test]
fn recompile_with_missing_hash_reruns_and_writes_hash() {
    let req = request("recomp-missing-hash", Os::Linux);
    seed_all_stages_without_recomp_hash(&req.out);
    assert!(!req.out.join("gen/.recomp-hash").exists());

    let mut fake = Fake::default();
    prepare_with(
        &req,
        Hub::StrixHalo,
        &mut fake,
        Path::new("/h"),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(fake.ran, vec![Stage::Recompile]);

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let expected = recomp_hash(&root).unwrap();
    let recorded = std::fs::read_to_string(req.out.join("gen/.recomp-hash")).unwrap();
    assert_eq!(recorded.trim(), expected);
}

#[test]
fn recompile_with_changed_hash_reruns_and_updates_hash() {
    let req = request("recomp-changed-hash", Os::Linux);
    seed_all_stages_without_recomp_hash(&req.out);
    std::fs::write(req.out.join("gen/.recomp-hash"), "stale-hash\n").unwrap();

    let mut fake = Fake::default();
    prepare_with(
        &req,
        Hub::StrixHalo,
        &mut fake,
        Path::new("/h"),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(fake.ran, vec![Stage::Recompile]);

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let expected = recomp_hash(&root).unwrap();
    let recorded = std::fs::read_to_string(req.out.join("gen/.recomp-hash")).unwrap();
    assert_eq!(recorded.trim(), expected);
}

#[test]
fn recompile_with_matching_hash_stays_cached() {
    let req = request("recomp-matching-hash", Os::Linux);
    seed_all_stages_without_recomp_hash(&req.out);
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let expected = recomp_hash(&root).unwrap();
    std::fs::write(req.out.join("gen/.recomp-hash"), format!("{expected}\n")).unwrap();

    let mut fake = Fake::default();
    prepare_with(
        &req,
        Hub::StrixHalo,
        &mut fake,
        Path::new("/h"),
        &mut |_| {},
    )
    .unwrap();
    assert!(!fake.ran.contains(&Stage::Recompile));
    assert!(fake.ran.is_empty());
}

#[test]
fn recompile_hash_changes_when_recomp_python_file_changes() {
    let source = std::env::temp_dir().join(format!("acnh-recomp-src-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&source);
    std::fs::create_dir_all(source.join("recomp")).unwrap();
    std::fs::write(source.join("recomp/emit.py"), "def emit(): return 1\n").unwrap();

    let h1 = recomp_hash(&source).unwrap();
    std::fs::write(source.join("recomp/emit.py"), "def emit(): return 2\n").unwrap();
    let h2 = recomp_hash(&source).unwrap();
    assert_ne!(h1, h2);
    let _ = std::fs::remove_dir_all(&source);
}
