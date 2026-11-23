// Tests for the prepare stage runner (issue 1.22), with a fake script runner.
use acnh_core::config::Config;
use acnh_core::error::Error;
use acnh_core::gamefolder::GameFolder;
use acnh_core::platform::{AppDirs, Os, Vars};
use acnh_core::prepare::{
    delivery_folder, delivery_subfolder, prepare_installed, prepare_with, step, toolchain_with,
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
    }
}

/// The stages that run the script: all but the shaders stage, which is Rust.
fn scripted() -> Vec<Stage> {
    STAGES
        .into_iter()
        .filter(|stage| *stage != Stage::Shaders)
        .collect()
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
}

impl Fake {
    /// A Mac with Xcode, cargo and the iOS Rust target.
    fn with_ios_tools() -> Fake {
        Fake {
            tools: vec![
                ("xcrun", "/Applications/Xcode.app/iPhoneOS.sdk\n"),
                ("cargo", "cargo 1.90.0\n"),
                ("rustup", "aarch64-apple-darwin\naarch64-apple-ios\n"),
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

    fn shaders(&mut self, _req: &Request) -> Result<(), Error> {
        Ok(())
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
fn step_is_rust_for_shaders_and_upstreams_script_for_the_rest() {
    let req = request("step", Os::Linux);
    for stage in STAGES {
        if stage == Stage::Shaders {
            assert_eq!(step(stage, &req), Step::Rust);
            continue;
        }
        assert_eq!(
            step(stage, &req),
            Step::Script(
                vec![
                    "python3".to_owned(),
                    "tools/setup_acnh.py".to_owned(),
                    "--game".to_owned(),
                    "/game".to_owned(),
                    "--out".to_owned(),
                    req.out.display().to_string(),
                ]
                .into_iter()
                .chain(
                    (stage != Stage::Fonts && stage != Stage::Build)
                        .then(|| "--stop-before-shaders".to_owned())
                )
                .collect()
            )
        );
    }
    let win = request("step-win", Os::Windows);
    assert_eq!(step(Stage::Shaders, &win), Step::Rust);
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
    assert_eq!(fake.ran, vec![Stage::Game, Stage::Recompile, Stage::Fonts]);
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
fn mac_skips_upstreams_native_executable() {
    let req = request("native-mac", Os::Mac);
    std::fs::create_dir_all(req.out.join("build")).unwrap();
    std::fs::write(req.out.join("build/acnh"), "synthetic executable").unwrap();
    let mut fake = Fake::default();
    prepare_with(&req, Hub::Mac, &mut fake, Path::new("/h"), &mut |_| {}).unwrap();
    assert!(!fake.ran.contains(&Stage::Build));
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

/// A runner that starts nothing: the Rust shaders stage must not need the script.
struct Idle;

impl ScriptRunner for Idle {
    fn run(&mut self, _stage: Stage, _args: &[String]) -> Result<(), Vec<String>> {
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

    fn finish(&mut self) -> Result<(), Vec<String>> {
        self.runner.finish()
    }

    fn shaders(&mut self, _req: &Request) -> Result<(), Error> {
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
            // The whole script has run through before game is reported as started: every
            // later output is there before its stage is looked at.
            if stage == Stage::Game {
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
    let req = request("fail-fonts", Os::Linux);
    let mut seen = vec![];
    let err = prepare_with(
        &req,
        Hub::StrixHalo,
        &mut stand_in(&req, 0, &["fail:fonts"]),
        Path::new("/home/me"),
        &mut report(&req, &mut seen),
    )
    .unwrap_err();
    match err {
        Error::Build(m) => {
            assert!(m.starts_with("stage fonts: == fonts"), "{m}");
            assert!(m.contains("boom at ~/x"), "{m}");
            assert!(m.contains("trace of fonts"), "{m}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(seen, STAGES[..6].to_vec());
    assert_eq!(starts(&req), 1);
    assert!(!req.out.join("build").exists());
}

#[test]
fn a_complete_build_folder_starts_no_script() {
    let req = request("complete", Os::Linux);
    std::fs::write(req.out.join(".prepare-tools.json"), "{}").unwrap();
    for stage in STAGES {
        make(&outputs(&req.out, stage));
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
    let req = request("fonts-only", Os::Linux);
    std::fs::write(req.out.join(".prepare-tools.json"), "{}").unwrap();
    for stage in STAGES {
        if stage != Stage::Fonts {
            make(&outputs(&req.out, stage));
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
    assert_eq!(seen, vec![Stage::Fonts]);
    assert_eq!(starts(&req), 1);
    assert!(outputs(&req.out, Stage::Fonts)[0].is_file());
    assert!(req.out.join("ended.txt").is_file());
}

#[test]
fn a_failure_in_a_stage_we_did_not_wait_on_is_named_by_the_scripts_line() {
    // Only the build is missing, and the script fails before it (its own tools check), then
    // after the last stage we waited on (the build that follows the fonts).
    for (missing, fail, named) in [
        (Stage::Build, "fail:tools", "stage tools: == tools"),
        (Stage::Fonts, "fail:build", "stage build: == build"),
    ] {
        let req = request(fail.trim_start_matches("fail:"), Os::Linux);
        std::fs::write(req.out.join(".prepare-tools.json"), "{}").unwrap();
        for stage in STAGES {
            if stage != missing {
                make(&outputs(&req.out, stage));
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
fn fan_mod_choices_are_snapshotted_in_each_script_pass() {
    let mut req = request("fan-mods", Os::Linux);
    let args = |req: &Request| match step(Stage::Game, req) {
        Step::Script(args) => args,
        _ => unreachable!(),
    };
    assert!(!args(&req).contains(&"--ptbr".to_owned()));
    assert!(!args(&req).contains(&"--speedfix".to_owned()));
    req.fan_mods.ptbr = true;
    assert!(args(&req).contains(&"--ptbr".to_owned()));
    assert!(!args(&req).contains(&"--speedfix".to_owned()));
    req.fan_mods.speedfix = true;
    req.fan_mods.ptbr = false;
    assert!(!args(&req).contains(&"--ptbr".to_owned()));
    assert!(args(&req).contains(&"--speedfix".to_owned()));
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
        fake.args[n - 2],
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
            assert!(
                message.contains("~/Dropbox/_transfers/ACNH-Universal"),
                "{message}"
            );
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

/// The macOS job's runner: the stages of upstream's script are faked, everything else runs.
struct StagesFaked(ProcessRunner);

impl ScriptRunner for StagesFaked {
    fn run(&mut self, stage: Stage, args: &[String]) -> Result<(), Vec<String>> {
        if args.iter().any(|arg| arg == "tools/setup_acnh.py") {
            return Ok(());
        }
        self.0.run(stage, args)
    }

    fn shaders(&mut self, _req: &Request) -> Result<(), Error> {
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
