//! The recompile stage: the recompiled game code from the game executable.

use super::*;

/// Deterministic SHA-256 hash of the recompiler inputs (`recomp/*.py` in sorted name order,
/// plus the Rust emitter/version marker) so changes to the recompiler invalidate `gen/`.
pub fn recomp_hash(source: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    let recomp_dir = source.join("recomp");
    if recomp_dir.is_dir() {
        let mut entries: Vec<(String, PathBuf)> = Vec::new();
        for entry in std::fs::read_dir(&recomp_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name.ends_with(".py") {
                entries.push((name, path));
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, path) in entries {
            let bytes = std::fs::read(&path)?;
            hasher.update(name.as_bytes());
            hasher.update(b"\0");
            hasher.update(&bytes);
            hasher.update(b"\0");
        }
    }
    hasher.update(b"rust-emitter:");
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    hasher.update(b"\0");
    Ok(format!("{:x}", hasher.finalize()))
}

pub(super) fn default_jobs(requested: u32) -> u32 {
    if requested > 0 {
        return requested;
    }
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1)
        .max(1)
}

pub(super) fn prepared_gen(
    out: &Path,
    module: &str,
    profile: &str,
    expected_hash: &str,
) -> Option<PathBuf> {
    let src = std::fs::read_to_string(out.join("game_source.txt")).ok()?;
    let pre = Path::new(src.trim()).join("gen");
    let same = std::fs::read_to_string(pre.join("recomp_source.txt"))
        .ok()
        .is_some_and(|h| h.trim() == expected_hash);
    let prof = std::fs::read_to_string(pre.join(module).join("profile.txt")).ok()?;
    let table = pre.join(module).join("table.cpp");
    if same && prof.trim() == profile && table.is_file() {
        Some(pre.join(module))
    } else {
        None
    }
}

pub(super) fn rust_recompile(req: &Request, root: &Path, jobs: u32) -> Result<()> {
    let ver_ini = std::fs::read_to_string(req.out.join("version.ini"))
        .ok()
        .and_then(|s| Config::parse(&s).ok())
        .unwrap_or_default();
    let hooks = ver_ini.get("hooks").unwrap_or("-").to_owned();
    let version = ver_ini.get("version").unwrap_or("unknown").to_owned();
    let game_src = std::fs::read_to_string(req.out.join("game_source.txt"))
        .ok()
        .map(|s| PathBuf::from(s.trim()))
        .unwrap_or_else(|| req.game.root.clone());

    let jobs = default_jobs(jobs);
    let expected_hash = recomp_hash(root)?;
    let recorded_hash = std::fs::read_to_string(recomp_hash_file(req)).ok();
    let hash_matches = recorded_hash
        .as_deref()
        .is_some_and(|h| h.trim() == expected_hash);

    let logs = req.out.join("logs");
    std::fs::create_dir_all(&logs)?;

    for (module, base) in [
        ("sdk", 0x40_1000_0000u64),
        ("subsdk0", 0x40_1800_0000u64),
        ("main", 0x40_0000_0000u64),
    ] {
        let mut h = Vec::new();
        let hooks_path = if module == "main" && hooks != "-" {
            h.push("--hooks".to_owned());
            h.push(hooks.clone());
            Some(root.join(&hooks))
        } else {
            None
        };
        let variant = speedfix_args(
            req.fan_mods.speedfix,
            module,
            &version,
            &game_src,
            &req.out,
            false,
        )?;
        let pch = variant.get(1).cloned();
        h.extend(variant);
        let profile = if h.is_empty() {
            "none".to_owned()
        } else {
            h.join(" ")
        };

        let gen = req.out.join("gen").join(module);
        let table = gen.join("table.cpp");
        let prof_file = gen.join("profile.txt");
        let prof_matches = std::fs::read_to_string(&prof_file)
            .ok()
            .is_some_and(|p| p.trim() == profile);
        let stale = !table.is_file()
            || !hash_matches
            || !prof_matches
            || (module == "main"
                && req.fan_mods.speedfix
                && pch
                    .as_deref()
                    .is_some_and(|p| mtime_newer(Path::new(p), &table)))
            || (module == "main" && hooks != "-" && mtime_newer(&root.join(&hooks), &table));

        if !stale {
            continue;
        }
        if let Some(pre) = prepared_gen(&req.out, module, &profile, &expected_hash) {
            copy_tree(&pre, &gen)?;
            crate::platform::atomic_write(&prof_file, format!("{profile}\n").as_bytes())?;
            continue;
        }

        let log_path = logs.join(format!("recomp_{module}.log"));
        let mut log_file = std::io::BufWriter::new(std::fs::File::create(&log_path)?);
        let opts = acnh_recomp::RecompileOptions {
            nso: req.out.join("exefs").join(module),
            out: gen.clone(),
            name: module.to_owned(),
            base,
            jobs: jobs as usize,
            only: None,
            hooks: hooks_path,
            variant: pch.map(PathBuf::from),
        };
        let result = acnh_recomp::recompile(&opts, &mut |line| {
            let _ = writeln!(log_file, "{line}");
        });
        let _ = log_file.flush();
        if let Err(err) = result {
            return Err(Error::Build(format!("stage recompile ({module}): {err}")));
        }
        crate::platform::atomic_write(&prof_file, format!("{profile}\n").as_bytes())?;
    }
    Ok(())
}
