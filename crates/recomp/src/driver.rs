//! Recompile an NSO module to C++ (port of `recomp/__main__.py`).

use crate::{
    analyse::{analyse, Analysis, Function},
    emit::{self, CacheOptions, Emitter},
    load::{self, load, Module},
};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// Reference base address where `--base` addresses are measured from (`NX_BASE` of the reference layout).
pub const REF_BASE: u64 = 0x40_0000_0000;

/// Maximum instructions per generated `code_NNN.cpp` translation unit.
pub const INSNS_PER_FILE: usize = 12_000;

#[derive(Debug)]
pub enum DriverError {
    Load(load::Error),
    Emit(emit::Error),
    Io(std::io::Error),
    Message(String),
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Load(e) => write!(f, "{e}"),
            Self::Emit(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "{e}"),
            Self::Message(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for DriverError {}

impl From<load::Error> for DriverError {
    fn from(e: load::Error) -> Self {
        Self::Load(e)
    }
}

impl From<emit::Error> for DriverError {
    fn from(e: emit::Error) -> Self {
        Self::Emit(e)
    }
}

impl From<std::io::Error> for DriverError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Options for [`recompile`], matching `python3 -m recomp` CLI arguments.
#[derive(Debug, Clone)]
pub struct RecompileOptions {
    pub nso: PathBuf,
    pub out: PathBuf,
    pub name: String,
    pub base: u64,
    pub jobs: usize,
    pub only: Option<BTreeSet<u64>>,
    pub hooks: Option<PathBuf>,
    pub variant: Option<PathBuf>,
}

impl Default for RecompileOptions {
    fn default() -> Self {
        Self {
            nso: PathBuf::new(),
            out: PathBuf::new(),
            name: "main".to_owned(),
            base: REF_BASE,
            jobs: 0,
            only: None,
            hooks: None,
            variant: None,
        }
    }
}

/// Summary of a completed [`recompile`] run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub functions: usize,
    pub files: usize,
    pub handled: usize,
    pub unhandled_total: usize,
    pub unhandled: BTreeMap<String, usize>,
    pub jump_tables: usize,
    pub unresolved_br: usize,
}

struct VariantData {
    module: Module,
    analysis: Analysis,
    changed: BTreeMap<u64, Function>,
    new: Vec<Function>,
    words: Vec<(u64, u32)>,
}

/// Write `text` to `path` only if the existing contents differ (preserving mtime on unchanged files).
fn write_if_changed(path: &Path, text: &str) -> std::io::Result<()> {
    if let Ok(existing) = std::fs::read_to_string(path) {
        if existing == text {
            return Ok(());
        }
    }
    std::fs::write(path, text)
}

fn cstr(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

pub fn parse_hex(text: &str) -> Result<u64, DriverError> {
    let text = text.trim();
    let stripped = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    u64::from_str_radix(stripped, 16)
        .map_err(|e| DriverError::Message(format!("invalid hex '{text}': {e}")))
}

fn read_hooks(path: &Path) -> Result<BTreeSet<u64>, DriverError> {
    let text = std::fs::read_to_string(path)?;
    let mut hooks = BTreeSet::new();
    for line in text.lines() {
        let code = line.split('#').next().unwrap_or("").trim();
        if !code.is_empty() {
            hooks.insert(parse_hex(code)?);
        }
    }
    Ok(hooks)
}

fn read_trace_sp_env() -> Result<BTreeSet<u64>, DriverError> {
    let val = std::env::var("NX_TRACE_SP").unwrap_or_default();
    let mut out = BTreeSet::new();
    for part in val.split(',') {
        let part = part.trim();
        if !part.is_empty() {
            out.insert(parse_hex(part)?);
        }
    }
    Ok(out)
}

/// `(module offset, 32-bit word)` pairs of the enabled sections of a `.pchtxt` (IPS text) patch.
pub fn read_pchtxt(path: &Path) -> Result<Vec<(u64, u32)>, DriverError> {
    let text = std::fs::read_to_string(path)?;
    parse_pchtxt(&text, path)
}

pub fn parse_pchtxt(text: &str, path: &Path) -> Result<Vec<(u64, u32)>, DriverError> {
    let mut enabled = false;
    let mut words = Vec::new();
    let line_re = Regex::new(r"^([0-9A-Fa-f]{8})\s+([0-9A-Fa-f]+)$").expect("valid regex");
    for line in text.lines() {
        let line = line.split("//").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line == "@enabled" {
            enabled = true;
        } else if line == "@disabled" {
            enabled = false;
        } else if line == "@stop" {
            break;
        } else if enabled {
            if let Some(caps) = line_re.captures(line) {
                let off = u64::from_str_radix(&caps[1], 16)
                    .map_err(|e| DriverError::Message(format!("{}: {e}", path.display())))?;
                let hex = &caps[2];
                if hex.len() % 8 != 0 {
                    return Err(DriverError::Message(format!(
                        "{}: {line}: not whole words",
                        path.display()
                    )));
                }
                for k in (0..hex.len()).step_by(8) {
                    let b0 = u8::from_str_radix(&hex[k..k + 2], 16).unwrap();
                    let b1 = u8::from_str_radix(&hex[k + 2..k + 4], 16).unwrap();
                    let b2 = u8::from_str_radix(&hex[k + 4..k + 6], 16).unwrap();
                    let b3 = u8::from_str_radix(&hex[k + 6..k + 8], 16).unwrap();
                    let w = u32::from_le_bytes([b0, b1, b2, b3]);
                    words.push((off + (k / 2) as u64, w));
                }
            }
        }
    }
    Ok(words)
}

fn build_variant(
    m: &Module,
    an: &Analysis,
    words: Vec<(u64, u32)>,
    log: &mut dyn FnMut(&str),
) -> Result<VariantData, DriverError> {
    let t = Instant::now();
    let mut m2 = m.clone();
    for &(off, w) in &words {
        let idx = off as usize;
        if idx + 4 > m2.image.len() {
            return Err(DriverError::Message(format!(
                "variant patch offset {off:#x} out of module image bounds"
            )));
        }
        m2.image[idx..idx + 4].copy_from_slice(&w.to_le_bytes());
    }
    let mut text: Vec<u64> = words
        .iter()
        .filter(|&&(off, _)| m.in_text(off))
        .map(|&(off, _)| off)
        .collect();
    let an2 = analyse(&m2, log, &text)?;
    text.sort_unstable();
    let touched = |f: &Function| {
        let k = text.partition_point(|&a| a < f.start);
        k < text.len() && text[k] < f.end
    };
    let base: BTreeSet<u64> = an.functions.iter().map(|f| f.start).collect();
    let by_start: BTreeMap<u64, &Function> = an2.functions.iter().map(|f| (f.start, f)).collect();
    let mut changed = BTreeMap::new();
    for f in &an.functions {
        if let Some(&f2) = by_start.get(&f.start) {
            if touched(f) || touched(f2) {
                changed.insert(f.start, f2.clone());
            }
        }
    }
    let new: Vec<Function> = an2
        .functions
        .iter()
        .filter(|f| !base.contains(&f.start))
        .cloned()
        .collect();
    log(&format!(
        "  variant analysis: {} functions ({:.0}s)",
        an2.functions.len(),
        t.elapsed().as_secs_f64()
    ));
    Ok(VariantData {
        module: m2,
        analysis: an2,
        changed,
        new,
        words,
    })
}

fn variant_function(
    e: &mut Emitter<'_>,
    f: &Function,
    variant: Option<&VariantData>,
    v_emitter: Option<&mut Emitter<'_>>,
) -> Result<String, DriverError> {
    let code = e.function(f)?;
    let (Some(v), Some(ve)) = (variant, v_emitter) else {
        return Ok(code);
    };
    let Some(vf) = v.changed.get(&f.start) else {
        return Ok(code);
    };
    let sym = e.function_name(f.start);
    let head = format!("void {sym}(nx::CPU& NX_RESTRICT c) {{");
    let v1 = ve.function(vf)?;
    if !code.contains(&head) || !v1.contains(&head) {
        return Err(DriverError::Message(format!(
            "--variant: no header for {sym}"
        )));
    }
    let v0_head = format!("static void {sym}__v0(nx::CPU& NX_RESTRICT c) {{");
    let v1_head = format!("static void {sym}__v1(nx::CPU& NX_RESTRICT c) {{");
    let body0 = code.replacen(&head, &v0_head, 1);
    let body1 = v1.replacen(&head, &v1_head, 1);
    Ok(format!(
        "{body0}\n\n{body1}\n\n{head}\n    if (nx_variant_on && nx_variant_ok({:#x})) {sym}__v1(c); else {sym}__v0(c);\n}}",
        f.start
    ))
}

/// One job per core, as many as free memory allows (a worker grows to about 300 MB).
pub fn default_jobs() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    #[cfg(target_os = "linux")]
    {
        if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
            if let Some(kb) = meminfo
                .lines()
                .find(|l| l.starts_with("MemAvailable:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<usize>().ok())
            {
                let avail_mb = kb / 1024;
                return cores.min(avail_mb.saturating_sub(1000) / 300).max(1);
            }
        }
    }
    cores.max(1)
}

fn write_funcs_header(
    out_dir: &Path,
    an: &Analysis,
    prefix: &str,
    base: u64,
    variant: Option<&VariantData>,
) -> Result<(), DriverError> {
    use std::fmt::Write;
    let mut h = String::new();
    let offset = base.wrapping_sub(REF_BASE);
    writeln!(
        h,
        "#pragma once\n#include <nx/cpu.h>\n\n#undef NXB\n#define NXB (NX_BASE + {offset:#x}ull)\n"
    )
    .unwrap();
    for f in &an.functions {
        writeln!(h, "void {prefix}_{:08X}(nx::CPU& c);", f.start).unwrap();
    }
    if let Some(v) = variant {
        writeln!(
            h,
            "\nextern \"C\" bool nx_variant_on;  // the runtime's NX_SPEEDFIX: the --variant bodies"
        )
        .unwrap();
        writeln!(
            h,
            "extern \"C\" uint64_t nx_variant_lo, nx_variant_hi;  // NX_VARIANT_RANGE: only these (bisecting)"
        )
        .unwrap();
        writeln!(
            h,
            "static inline bool nx_variant_ok(uint64_t a) {{ return a >= nx_variant_lo && a < nx_variant_hi; }}"
        )
        .unwrap();
        for f in &v.new {
            writeln!(h, "void {prefix}_{:08X}(nx::CPU& c);", f.start).unwrap();
        }
    }
    write_if_changed(&out_dir.join("funcs.h"), &h)?;
    Ok(())
}

fn write_table(
    path: &Path,
    m: &Module,
    an: &Analysis,
    name: &str,
    base: u64,
    variant: Option<&VariantData>,
) -> Result<(), DriverError> {
    use std::fmt::Write;
    let mut out = String::new();
    writeln!(out, "#include \"funcs.h\"\n#include <nx/module.h>\n").unwrap();
    writeln!(out, "static const nx::FuncEntry funcs[] = {{").unwrap();
    let mut entries: Vec<u64> = an.functions.iter().map(|f| f.start).collect();
    if let Some(v) = variant {
        entries.extend(v.new.iter().map(|f| f.start));
    }
    entries.sort_unstable();
    for &a in &entries {
        writeln!(out, "    {{{a:#x}, {name}_{a:08X}}},").unwrap();
    }
    writeln!(out, "}};\n").unwrap();

    writeln!(out, "static const nx::Reloc relocs[] = {{").unwrap();
    for r in &m.relocs {
        writeln!(
            out,
            "    {{{:#x}, {}, {}, {}ll}},",
            r.offset, r.r#type, r.sym, r.addend
        )
        .unwrap();
    }
    writeln!(out, "}};\n").unwrap();

    writeln!(out, "static const nx::Symbol symbols[] = {{").unwrap();
    for s in &m.symbols {
        writeln!(
            out,
            "    {{{}, {:#x}, {}, {}, {}, {}}},",
            cstr(&s.name),
            s.value,
            s.size,
            s.r#type,
            s.bind,
            u8::from(s.imported())
        )
        .unwrap();
    }
    writeln!(out, "}};\n").unwrap();

    let ia = m
        .init_array
        .iter()
        .chain(std::iter::once(&0u64))
        .map(|a| format!("{a:#x}"))
        .collect::<Vec<_>>()
        .join(", ");
    let fa = m
        .fini_array
        .iter()
        .chain(std::iter::once(&0u64))
        .map(|a| format!("{a:#x}"))
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(out, "static const uint64_t init_array[] = {{{ia}}};").unwrap();
    writeln!(out, "static const uint64_t fini_array[] = {{{fa}}};\n").unwrap();

    let offset = base.wrapping_sub(REF_BASE);
    writeln!(out, "extern const nx::ModuleInfo nx_module_{name} = {{").unwrap();
    writeln!(out, "    {}, NX_BASE + {offset:#x}ull,", cstr(name)).unwrap();
    writeln!(
        out,
        "    {:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x},",
        m.text.0, m.text.1, m.ro.0, m.ro.1, m.data.0, m.data.1, m.bss.0, m.bss.1
    )
    .unwrap();
    writeln!(out, "    funcs, {},", entries.len()).unwrap();
    writeln!(out, "    relocs, {},", m.relocs.len()).unwrap();
    writeln!(out, "    symbols, {},", m.symbols.len()).unwrap();
    writeln!(
        out,
        "    init_array, {}, fini_array, {},",
        m.init_array.len(),
        m.fini_array.len()
    )
    .unwrap();
    writeln!(out, "}};").unwrap();

    if let Some(v) = variant {
        writeln!(out, "\n#include <nx/runtime.h>").unwrap();
        let w = v
            .words
            .iter()
            .map(|(o, x)| format!("{o:#x}, {x:#010x}"))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(out, "\nstatic const uint32_t variant_words[] = {{{w}}};").unwrap();
        writeln!(
            out,
            "static nx::VariantRegistrar variant_registrar({}, variant_words, {});",
            cstr(name),
            v.words.len()
        )
        .unwrap();
    }

    write_if_changed(path, &out)?;
    Ok(())
}

/// Recompile an NSO module to C++ into `opts.out`.
pub fn recompile(
    opts: &RecompileOptions,
    log: &mut dyn FnMut(&str),
) -> Result<Report, DriverError> {
    let t = Instant::now();
    let m = load(&opts.nso, Some(&opts.name))?;
    let an = analyse(&m, log, &[])?;
    log(&format!(
        "analysis: {} functions, {} jump tables ({:.1}s)",
        an.functions.len(),
        an.jump_tables.len(),
        t.elapsed().as_secs_f64()
    ));

    std::fs::create_dir_all(&opts.out)?;
    let hooks = match &opts.hooks {
        Some(path) => {
            let h = read_hooks(path)?;
            log(&format!("  {} hooks from {}", h.len(), path.display()));
            h
        }
        None => BTreeSet::new(),
    };
    let trace_sp = read_trace_sp_env()?;
    let cache = CacheOptions::from_env();

    let variant = match &opts.variant {
        Some(path) => {
            let words = read_pchtxt(path)?;
            Some(build_variant(&m, &an, words, log)?)
        }
        None => None,
    };

    let funcs: Vec<Function> = match &opts.only {
        Some(want) => an
            .functions
            .iter()
            .filter(|f| want.contains(&f.start))
            .cloned()
            .collect(),
        None => an.functions.clone(),
    };

    write_funcs_header(&opts.out, &an, &opts.name, opts.base, variant.as_ref())?;

    let mut chunks: Vec<(usize, usize, usize)> = Vec::new();
    let mut first = 0usize;
    let mut n = 0usize;
    for (k, f) in funcs.iter().enumerate() {
        n += ((f.end - f.start) / 4) as usize;
        if n >= INSNS_PER_FILE || k == funcs.len() - 1 {
            chunks.push((chunks.len(), first, k + 1));
            first = k + 1;
            n = 0;
        }
    }

    let jobs = if opts.jobs == 0 {
        default_jobs()
    } else {
        opts.jobs
    };
    let jobs = jobs.min(chunks.len()).max(1);
    log(&format!(
        "  emitting {} functions into {} files with {} jobs",
        funcs.len(),
        chunks.len(),
        jobs
    ));

    type ChunkResult = Result<(usize, BTreeMap<String, usize>, usize), DriverError>;
    let next_chunk = AtomicUsize::new(0);
    let chunk_results: Vec<ChunkResult> = std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(jobs);
        for _ in 0..jobs {
            handles.push(s.spawn(|| -> ChunkResult {
                let mut emitter = Emitter::new(&m, &an, &opts.name)?;
                emitter.options.hooks = hooks.clone();
                emitter.options.trace_sp = trace_sp.clone();
                emitter.options.cache = cache;

                let mut v_emitter = match variant.as_ref() {
                    Some(v) => {
                        let mut ve = Emitter::new(&v.module, &v.analysis, &opts.name)?;
                        ve.options.hooks = hooks.clone();
                        ve.options.trace_sp = trace_sp.clone();
                        ve.options.cache = cache;
                        Some(ve)
                    }
                    None => None,
                };

                let mut total_count = 0usize;
                loop {
                    let idx = next_chunk.fetch_add(1, Ordering::Relaxed);
                    if idx >= chunks.len() {
                        break;
                    }
                    let (file_idx, lo, hi) = chunks[idx];
                    let mut bodies = Vec::with_capacity(hi - lo);
                    for f in &funcs[lo..hi] {
                        bodies.push(variant_function(
                            &mut emitter,
                            f,
                            variant.as_ref(),
                            v_emitter.as_mut(),
                        )?);
                    }
                    let mut content = String::from("#include \"funcs.h\"\n\n");
                    content.push_str(&bodies.join("\n\n"));
                    content.push('\n');
                    write_if_changed(&opts.out.join(format!("code_{file_idx:03}.cpp")), &content)?;
                    total_count += hi - lo;
                }
                Ok((
                    emitter.coverage.handled,
                    emitter.coverage.unhandled,
                    total_count,
                ))
            }));
        }
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(DriverError::Message("worker thread panicked".into())))
            })
            .collect()
    });

    let mut handled = 0usize;
    let mut unhandled: BTreeMap<String, usize> = BTreeMap::new();
    let mut done = 0usize;
    for res in chunk_results {
        let (h, u, count) = res?;
        handled += h;
        for (mn, c) in u {
            *unhandled.entry(mn).or_default() += c;
        }
        let prev = done;
        done += count;
        if done / 8000 != prev / 8000 {
            log(&format!(
                "  emitted {done}/{} functions ({:.0}s)",
                funcs.len(),
                t.elapsed().as_secs_f64()
            ));
        }
    }

    let variant_path = opts.out.join("code_variant.cpp");
    if let Some(v) = variant.as_ref() {
        let mut ve = Emitter::new(&v.module, &v.analysis, &opts.name)?;
        ve.options.hooks = hooks;
        ve.options.trace_sp = trace_sp;
        ve.options.cache = cache;
        let mut bodies = Vec::with_capacity(v.new.len());
        for f in &v.new {
            bodies.push(ve.function(f)?);
        }
        let mut content = String::from("#include \"funcs.h\"\n\n");
        content.push_str(&bodies.join("\n\n"));
        content.push('\n');
        write_if_changed(&variant_path, &content)?;
        log(&format!(
            "  variant: {} functions with two bodies, {} only in the patched code, {} words",
            v.changed.len(),
            v.new.len(),
            v.words.len()
        ));
    } else if variant_path.exists() {
        std::fs::remove_file(&variant_path)?;
    }

    write_table(
        &opts.out.join("table.cpp"),
        &m,
        &an,
        &opts.name,
        opts.base,
        variant.as_ref(),
    )?;

    let unhandled_total: usize = unhandled.values().sum();
    let mut unhandled_sorted: Vec<(&String, &usize)> = unhandled.iter().collect();
    unhandled_sorted.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));

    {
        use std::fmt::Write;
        let mut rpt = String::new();
        writeln!(
            rpt,
            "functions {}, handled {}, unhandled {}",
            funcs.len(),
            handled,
            unhandled_total
        )
        .unwrap();
        writeln!(
            rpt,
            "jump tables {}, unresolved br (indirect tail calls) {}\n",
            an.jump_tables.len(),
            an.unresolved_br.len()
        )
        .unwrap();
        for (mn, n) in &unhandled_sorted {
            writeln!(rpt, "{n:8} {mn}").unwrap();
        }
        std::fs::write(opts.out.join("report.txt"), rpt)?;
    }

    log(&format!(
        "done: {} files, handled {}, unhandled {} ({:.0}s)",
        chunks.len(),
        handled,
        unhandled_total,
        t.elapsed().as_secs_f64()
    ));
    for (mn, n) in unhandled_sorted.iter().take(30) {
        log(&format!("  {n:7} {mn}"));
    }

    Ok(Report {
        functions: funcs.len(),
        files: chunks.len(),
        handled,
        unhandled_total,
        unhandled,
        jump_tables: an.jump_tables.len(),
        unresolved_br: an.unresolved_br.len(),
    })
}
