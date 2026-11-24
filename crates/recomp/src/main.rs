//! Command-line driver for `acnh_recomp` (replaces `python3 -m recomp` and `recomp.stats`).

use acnh_recomp::driver::{parse_hex, recompile, DriverError, RecompileOptions};
use acnh_recomp::stats::stats;
use std::collections::BTreeSet;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;

fn run() -> Result<(), DriverError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("stats") {
        let nso = args.get(1).ok_or_else(|| {
            DriverError::Message("usage: acnh_recomp stats <nso> [out.txt]".into())
        })?;
        if let Some(out_path) = args.get(2) {
            let file = std::fs::File::create(out_path)?;
            let mut w = BufWriter::new(file);
            stats(std::path::Path::new(nso), &mut w)?;
            w.flush()?;
        } else {
            let mut w = BufWriter::new(io::stdout().lock());
            stats(std::path::Path::new(nso), &mut w)?;
            w.flush()?;
        }
        return Ok(());
    }

    let mut opts = RecompileOptions::default();
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--name" => {
                i += 1;
                opts.name = args
                    .get(i)
                    .ok_or_else(|| DriverError::Message("--name requires a value".into()))?
                    .clone();
            }
            "--base" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| DriverError::Message("--base requires a value".into()))?;
                opts.base = parse_hex(val)?;
            }
            "--jobs" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| DriverError::Message("--jobs requires a value".into()))?;
                opts.jobs = val
                    .parse()
                    .map_err(|e| DriverError::Message(format!("invalid --jobs '{val}': {e}")))?;
            }
            "--hooks" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| DriverError::Message("--hooks requires a value".into()))?;
                opts.hooks = Some(PathBuf::from(val));
            }
            "--variant" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| DriverError::Message("--variant requires a value".into()))?;
                opts.variant = Some(PathBuf::from(val));
            }
            "--only" => {
                let mut set = opts.only.take().unwrap_or_default();
                while i + 1 < args.len() && !args[i + 1].starts_with("--") {
                    i += 1;
                    set.insert(parse_hex(&args[i])?);
                }
                opts.only = Some(set);
            }
            _ if arg.starts_with("--") => {
                return Err(DriverError::Message(format!("unknown option: {arg}")));
            }
            _ => positional.push(arg.clone()),
        }
        i += 1;
    }

    if positional.len() != 2 {
        return Err(DriverError::Message(
            "usage: acnh_recomp <module.nso> <out dir> [--name NAME] [--base ADDR] [--jobs N] [--hooks FILE] [--variant FILE] [--only ADDR...]".into(),
        ));
    }
    opts.nso = PathBuf::from(&positional[0]);
    opts.out = PathBuf::from(&positional[1]);
    if let Some(only) = &opts.only {
        if only.is_empty() {
            opts.only = Some(BTreeSet::new());
        }
    }

    recompile(&opts, &mut |line| println!("{line}"))?;
    Ok(())
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}
