//! The shaders and fonts stages, and the packs they make.

use super::*;

/// The Rust part of the shaders stage: pack the RomFS and the stage's `.spv` files for the
/// runtime (`pack::romfs`, `pack::spirv`). A source that is not there yet leaves its pack
/// alone, as upstream keeps the loose folder until something packs it.
pub(super) fn pack_stage(req: &Request) -> Result<()> {
    std::fs::create_dir_all(&req.out)?;
    if req.game.romfs.is_dir() {
        let pak = req.out.join("romfs.pak");
        let inputs = format!(
            "romfs.pak v1 {}\n{}",
            env!("CARGO_PKG_VERSION"),
            tree_key(&req.game.romfs)?
        );
        redo_unless_current(&req.out.join(".romfs.pak.key"), &inputs, &pak, || {
            crate::pack::romfs(&req.game.romfs, &pak)
        })?;
    }
    let spirv = req.out.join("spirv");
    if spirv.is_dir() {
        let pak = req.out.join("spirv.pak");
        let inputs = format!(
            "spirv.pak v1 {}\n{}",
            env!("CARGO_PKG_VERSION"),
            tree_key(&spirv)?
        );
        redo_unless_current(&req.out.join(".spirv.pak.key"), &inputs, &pak, || {
            crate::pack::spirv(&spirv, &pak)
        })?;
    }
    Ok(())
}

/// A file's size and change time, or `missing`: enough to tell a rewritten file from the one
/// recorded, without reading it.
pub(super) fn file_key(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(stat) if stat.is_file() => {
            let modified = stat
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |time| time.as_nanos());
            format!("{} {modified}", stat.len())
        }
        _ => "missing".to_owned(),
    }
}

/// A SHA-256 over every file under `root` (links to files followed): its path, size and change time, in
/// name order. A file added, removed or rewritten changes it; reading the 10 GB RomFS does not.
pub(super) fn tree_key(root: &Path) -> Result<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            // Only real folders, as `pack::romfs` walks: a link to a folder is not followed,
            // so a link loop cannot recurse forever.
            if entry.file_type()?.is_dir() {
                walk(root, &path, out)?;
            } else {
                let rel = path.strip_prefix(root).unwrap_or(&path);
                out.push((rel.to_string_lossy().into_owned(), file_key(&path)));
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for (rel, key) in files {
        hasher.update(rel.as_bytes());
        hasher.update(b"\0");
        hasher.update(key.as_bytes());
        hasher.update(b"\0");
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Runs `work`, which writes `output`, unless `record` shows a finished run from these same
/// `inputs` whose `output` is still the file it wrote. Re-running Prepare with fan mods on
/// otherwise translated every shader and rewrote the 10 GB `romfs.pak` each time.
pub(super) fn redo_unless_current(
    record: &Path,
    inputs: &str,
    output: &Path,
    work: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let line = |output: &Path| format!("{inputs}\noutput {}\n", file_key(output));
    if std::fs::read_to_string(record).ok().as_deref() == Some(line(output).as_str()) {
        return Ok(());
    }
    match std::fs::remove_file(record) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    work()?;
    crate::platform::atomic_write(record, line(output).as_bytes())?;
    Ok(())
}

/// The shaders stage, in Rust: extract the shaders, translate them with the tools' .NET
/// harness (built by the tools stage), fix the mods' archives and pack. The script has ended
/// before this stage and starts again after it.
pub(super) fn rust_shaders(req: &Request, root: &Path) -> Result<()> {
    let stage = Stage::Shaders;
    let invalidate = |error| {
        let _ = std::fs::remove_file(req.out.join("spirv/summary.txt"));
        Error::Build(format!("stage {}: {error}", stage.key()))
    };
    fix_mod_archives(req).map_err(invalidate)?;
    // The shaders come from the game's RomFS and main.bin, translated by the tools' harness:
    // with those unchanged, the last translation stands (fan mods do not touch them).
    let main = req.out.join("analysis/main.bin");
    let harness = root.join("private/build/spirv_harness");
    let inputs = format!(
        "shaders v1 {}\nspirv-val {}\nromfs {}\nmain {}\nharness {} {}\n",
        env!("CARGO_PKG_VERSION"),
        find_on_path("spirv-val").is_some(),
        tree_key(&req.game.romfs).map_err(invalidate)?,
        file_key(&main),
        file_key(&harness.join("bin/Release/net10.0/spirv_harness.dll")),
        file_key(&harness.join("out/spirv_harness.dll")),
    );
    redo_unless_current(
        &req.out.join(".shaders.key"),
        &inputs,
        &req.out.join("spirv/summary.txt"),
        || {
            crate::shaders::stage_with(
                &mut crate::shaders::ProcessRunner,
                root,
                &req.game.romfs,
                &main,
                &req.out,
                req.jobs,
            )
        },
    )
    // Any failure here, writing the record included, clears the summary: a summary left
    // beside packs that were never rewritten would mark the stage complete next run.
    .map_err(invalidate)?;
    // A summary only marks the stage complete once its archives and packs are ready.
    pack_stage(req).map_err(invalidate)
}

/// The Rust fonts stage for the Prepare runner.
pub fn rust_fonts(req: &Request) -> Result<()> {
    let host = [
        r"C:\Windows\Fonts\arial.ttf",
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
    ]
    .into_iter()
    .map(Path::new)
    .find(|path| path.is_file());
    crate::glyphs::stage(&req.out.join("extract/romfs"), &req.out.join("fonts"), host)
        .map_err(|error| Error::Build(format!("stage {}: {error}", Stage::Fonts.key())))
}

#[cfg(test)]
mod redo_unless_current_tests {
    use super::{redo_unless_current, tree_key};
    use std::cell::Cell;
    use std::path::Path;

    fn dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("acnh-redo-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/sub")).unwrap();
        std::fs::write(dir.join("src/a"), b"one").unwrap();
        std::fs::write(dir.join("src/sub/b"), b"two").unwrap();
        dir
    }

    fn pack(dir: &Path, runs: &Cell<u32>) {
        let inputs = tree_key(&dir.join("src")).unwrap();
        let out = dir.join("out.pak");
        redo_unless_current(&dir.join(".key"), &inputs, &out, || {
            runs.set(runs.get() + 1);
            std::fs::write(&out, format!("pack {}", runs.get()))?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn the_same_inputs_and_output_skip_the_work() {
        let dir = dir("same");
        let runs = Cell::new(0);
        pack(&dir, &runs);
        pack(&dir, &runs);
        assert_eq!(runs.get(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_changed_added_or_removed_source_file_redoes_the_work() {
        let dir = dir("source");
        let runs = Cell::new(0);
        pack(&dir, &runs);
        std::fs::write(dir.join("src/sub/b"), b"longer").unwrap();
        pack(&dir, &runs);
        std::fs::write(dir.join("src/c"), b"new").unwrap();
        pack(&dir, &runs);
        std::fs::remove_file(dir.join("src/a")).unwrap();
        pack(&dir, &runs);
        assert_eq!(runs.get(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_rewritten_output_redoes_the_work() {
        let dir = dir("output");
        let runs = Cell::new(0);
        pack(&dir, &runs);
        std::fs::remove_file(dir.join("out.pak")).unwrap();
        pack(&dir, &runs);
        std::fs::write(dir.join("out.pak"), b"something else entirely").unwrap();
        pack(&dir, &runs);
        assert_eq!(runs.get(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_loop_under_the_source_is_not_followed() {
        let dir = dir("loop");
        std::os::unix::fs::symlink("..", dir.join("src/sub/up")).unwrap();
        let runs = Cell::new(0);
        pack(&dir, &runs);
        pack(&dir, &runs);
        assert_eq!(runs.get(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_work_leaves_no_record() {
        let dir = dir("failed");
        let record = dir.join(".key");
        let out = dir.join("out.pak");
        let failed = redo_unless_current(&record, "in", &out, || {
            std::fs::write(&out, b"half")?;
            Err(std::io::Error::other("stopped").into())
        });
        assert!(failed.is_err());
        assert!(!record.exists());
        let runs = Cell::new(0);
        redo_unless_current(&record, "in", &out, || {
            runs.set(1);
            Ok(())
        })
        .unwrap();
        assert_eq!(runs.get(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
