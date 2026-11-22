use acnh_core::error::{Error, Result};
use acnh_core::shaders::{self, Runner};
use std::path::{Path, PathBuf};

struct Fake {
    root: PathBuf,
    validator: bool,
    sdk: bool,
    calls: Vec<Vec<String>>,
    summary: &'static str,
}

impl Runner for Fake {
    fn extract(
        &mut self,
        _root: &Path,
        _romfs: &Path,
        _main: &Path,
        _out: &Path,
        _jobs: u32,
    ) -> Result<()> {
        self.calls.push(vec!["extract".into()]);
        Ok(())
    }
    fn find_program(&self, name: &str) -> Option<PathBuf> {
        match name {
            "dotnet" => Some(self.root.join("dotnet")),
            "spirv-val" if self.validator => Some(self.root.join("spirv-val")),
            _ => None,
        }
    }
    fn has_dotnet_10(&mut self, _dotnet: &Path) -> bool {
        self.sdk
    }
    fn run(&mut self, args: &[String], out: &Path) -> Result<()> {
        self.calls.push(args.to_vec());
        std::fs::create_dir_all(out.join("spirv"))?;
        std::fs::write(out.join("spirv/summary.txt"), self.summary)?;
        Ok(())
    }
}

fn setup(name: &str) -> Fake {
    let root = std::env::temp_dir().join(format!("acnh-harness-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dll = root.join("private/build/spirv_harness/bin/Release/net10.0/spirv_harness.dll");
    std::fs::create_dir_all(dll.parent().unwrap()).unwrap();
    std::fs::write(dll, "synthetic").unwrap();
    Fake {
        root,
        validator: false,
        sdk: true,
        calls: vec![],
        summary: "blocks 1 translated\n",
    }
}

#[test]
fn extracts_then_runs_harness_with_optional_validator() {
    for validator in [false, true] {
        let mut fake = setup(if validator { "val" } else { "no-val" });
        fake.validator = validator;
        let root = fake.root.clone();
        let out = root.join("output");
        shaders::stage_with(
            &mut fake,
            &root,
            &root.join("romfs"),
            &root.join("main"),
            &out,
            4,
        )
        .unwrap();
        let mut expected = vec![
            root.join("dotnet").display().to_string(),
            root.join("private/build/spirv_harness/bin/Release/net10.0/spirv_harness.dll")
                .display()
                .to_string(),
            "--batch".into(),
            out.join("shaders_extracted").display().to_string(),
            "--out".into(),
            out.join("spirv").display().to_string(),
            "--jobs".into(),
            "4".into(),
        ];
        if validator {
            expected.extend([
                "--spirv-val".into(),
                root.join("spirv-val").display().to_string(),
            ]);
        } else {
            expected.push("--no-val".into());
        }
        assert_eq!(fake.calls, vec![vec!["extract".to_owned()], expected]);
    }
}

#[test]
fn empty_summary_is_removed_and_reported() {
    let mut fake = setup("empty");
    fake.summary = "blocks 0 translated\n";
    let root = fake.root.clone();
    let out = root.join("output");
    let error = shaders::stage_with(
        &mut fake,
        &root,
        &root.join("romfs"),
        &root.join("main"),
        &out,
        1,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("no shader was extracted"), "{error}");
    assert!(!out.join("spirv/summary.txt").exists());
}

#[test]
fn missing_sdk_or_harness_names_tools_stage() {
    for missing_sdk in [true, false] {
        let mut fake = setup(if missing_sdk { "sdk" } else { "dll" });
        let root = fake.root.clone();
        if missing_sdk {
            fake.sdk = false;
        } else {
            std::fs::remove_file(
                root.join("private/build/spirv_harness/bin/Release/net10.0/spirv_harness.dll"),
            )
            .unwrap();
        }
        let error = shaders::stage_with(
            &mut fake,
            &root,
            &root.join("romfs"),
            &root.join("main"),
            &root.join("output"),
            1,
        )
        .unwrap_err();
        assert!(
            matches!(error, Error::Build(ref message) if message.contains("tools stage")),
            "{error}"
        );
    }
}
