//! The recompile stage: the recompiled game code from the game executable.

use super::*;

/// The `NX_REGCACHE*` switches the emitter reads (`crates/recomp/src/emit/regcache.rs`): they
/// change the code it writes, so their values are inputs of the recompile.
const REGCACHE_SWITCHES: [&str; 4] = [
    "NX_REGCACHE",
    "NX_REGCACHE_REGS",
    "NX_REGCACHE_FLAGS",
    "NX_REGCACHE_V",
];

/// A build version that moves with the code: the engine's version (fixed at 0.0.0 in the
/// workspace) plus a fingerprint of this crate's source. It is the version term in the
/// `romfs.pak`, `spirv.pak` and shaders keys.
pub(super) fn build_version() -> String {
    format!(
        "{}+{}",
        env!("CARGO_PKG_VERSION"),
        env!("ACNH_CORE_FINGERPRINT")
    )
}

/// Deterministic SHA-256 hash of the recompiler inputs, so a change to the recompiler
/// invalidates `gen/`: the emitter's own source fingerprint (taken when it was built) and
/// the values of the `NX_REGCACHE*` switches. `source` is the repo root; nothing under it
/// is read now that the recompiler is the in-process `acnh_recomp` crate.
pub fn recomp_hash(_source: &Path) -> Result<String> {
    Ok(recomp_hash_with(acnh_recomp::EMITTER_FINGERPRINT, |name| {
        std::env::var(name).ok()
    }))
}

/// `recomp_hash` for a given emitter fingerprint and environment lookup.
pub fn recomp_hash_with(fingerprint: &str, env: impl Fn(&str) -> Option<String>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"rust-emitter:");
    hasher.update(fingerprint.as_bytes());
    hasher.update(b"\0");
    for name in REGCACHE_SWITCHES {
        hasher.update(name.as_bytes());
        hasher.update(b"=");
        match env(name) {
            Some(value) => {
                hasher.update(b"1");
                hasher.update(value.as_bytes());
            }
            None => hasher.update(b"0"),
        }
        hasher.update(b"\0");
    }
    format!("{:x}", hasher.finalize())
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

/// A `gen/<module>` already made in the game's source folder, taken in place of a recompile
/// when the hash Rust recorded beside it (`gen/.recomp-hash`, the file `prepare` writes) is
/// the current one and its profile is the wanted one.
pub(super) fn prepared_gen(
    out: &Path,
    module: &str,
    profile: &str,
    expected_hash: &str,
) -> Option<PathBuf> {
    let src = std::fs::read_to_string(out.join("game_source.txt")).ok()?;
    let pre = Path::new(src.trim()).join("gen");
    let same = std::fs::read_to_string(pre.join(".recomp-hash"))
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

#[cfg(test)]
mod recomp_staleness_tests {
    use super::{build_version, prepared_gen, recomp_hash, recomp_hash_with};
    use std::path::Path;

    fn none(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn the_hash_changes_with_the_emitter_fingerprint() {
        assert_ne!(
            recomp_hash_with("emitter-a", none),
            recomp_hash_with("emitter-b", none)
        );
        assert_eq!(
            recomp_hash_with("emitter-a", none),
            recomp_hash_with("emitter-a", none)
        );
    }

    #[test]
    fn the_hash_changes_with_each_regcache_switch() {
        let base = recomp_hash_with("e", none);
        for switch in [
            "NX_REGCACHE",
            "NX_REGCACHE_REGS",
            "NX_REGCACHE_FLAGS",
            "NX_REGCACHE_V",
        ] {
            let set =
                |value: &'static str| move |name: &str| (name == switch).then(|| value.to_owned());
            let zero = recomp_hash_with("e", set("0"));
            assert_ne!(base, zero, "{switch} set is not seen");
            assert_ne!(
                zero,
                recomp_hash_with("e", set("1")),
                "{switch} value is not seen"
            );
        }
    }

    #[test]
    fn the_real_hash_carries_the_built_fingerprint_and_ignores_the_source_folder() {
        let a = recomp_hash(Path::new("/nonexistent-a")).unwrap();
        let b = recomp_hash(Path::new("/nonexistent-b")).unwrap();
        assert_eq!(a, b);
        assert_eq!(acnh_recomp::EMITTER_FINGERPRINT.len(), 64);
    }

    #[test]
    fn the_build_version_is_not_the_bare_workspace_version() {
        assert_ne!(build_version(), env!("CARGO_PKG_VERSION"));
        assert!(build_version().starts_with(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn a_prepared_gen_is_taken_only_for_the_recorded_current_hash() {
        let root = std::env::temp_dir().join(format!("acnh-prepared-gen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (out, game) = (root.join("out"), root.join("game"));
        let module = game.join("gen/main");
        std::fs::create_dir_all(&module).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("game_source.txt"), format!("{}\n", game.display())).unwrap();
        std::fs::write(module.join("profile.txt"), "none\n").unwrap();
        std::fs::write(module.join("table.cpp"), "").unwrap();
        std::fs::write(game.join("gen/.recomp-hash"), "abc\n").unwrap();
        assert_eq!(
            prepared_gen(&out, "main", "none", "abc"),
            Some(module.clone())
        );
        assert_eq!(prepared_gen(&out, "main", "none", "other"), None);
        assert_eq!(prepared_gen(&out, "main", "--hooks x", "abc"), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
