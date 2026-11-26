//! Keep guest registers in locals, synchronizing around calls and function exits.
//! This pass follows the C++ syntax produced by the emitter, and rewrites it in place.

use super::{Error, Result};
use regex::{Captures, Regex};
use std::{collections::BTreeMap, sync::LazyLock};

macro_rules! pattern {
    ($name:ident, $pattern:literal) => {
        static $name: LazyLock<Regex> =
            LazyLock::new(|| Regex::new($pattern).expect("valid emitter regex"));
    };
}

pattern!(REG, r"\bc\.x\[(\d+)\]");
pattern!(WRITE, r"\bx(\d+)\s*([-+|&^]?=|\+\+|--)");
pattern!(GOTO, r"\bgoto L_([0-9A-F]+);");
pattern!(LABEL, r"^L_([0-9A-F]+):;$");
pattern!(
    CALL,
    r"((?:js_\[\w+\]\.)?[A-Za-z_][\w:]*)(<[^<>()]*>)?\(c([,)])"
);
pattern!(FLAG, r"\bc\.([nzcv]f)\b");
pattern!(
    FLAG_HELPER,
    r"\b(nx::(?:subs|adds|logic)(?:32|64)|nx::set_nzcv|nx::get_nzcv|nx::fcmpe?)(<[^<>()]*>)?\(c([,)])"
);
pattern!(
    FLAG_WRITE,
    r"\bnx::(?:subs|adds|logic)(?:32|64)\(fl_|\bnx::set_nzcv\(fl_|\bnx::fcmpe?(?:<[^<>()]*>)?\(fl_|\bfl_\.[nzcv]f\s*="
);
pattern!(VREG, r"\bc\.v\[(\d+)\]");
pattern!(
    VWRITE,
    r"\bvr(\d+)(?:\.\w+\[[^\]]*\])?\s*([-+*/|&^]?=|\+\+|--)|memcpy\(&vr(\d+)\b"
);
pattern!(RETURN, r"\breturn;");
const RESTORE: &str = "js_[jr_].restore(c);";
const FLAGS: u32 = 32;
const VECTOR_BASE: u32 = 33;

const PURE: &[&str] = &[
    "nx::subs32",
    "nx::subs64",
    "nx::adds32",
    "nx::adds64",
    "nx::logic32",
    "nx::logic64",
    "nx::set_nzcv",
    "nx::get_nzcv",
    "nx::clrex",
    "nx::ldxr",
    "nx::stxr",
    "nx::ldxp",
    "nx::stxp",
    "nx::fcmp",
    "nx::fcmpe",
    "nx::mrs",
    "nx::msr",
];

/// Register cache controls. Bit 31 of the general mask is the stack pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheOptions {
    pub enabled: bool,
    pub general: u32,
    pub flags: bool,
    pub vectors: bool,
}

impl Default for CacheOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            general: u32::MAX,
            flags: true,
            vectors: true,
        }
    }
}

impl CacheOptions {
    /// Read the same environment switches as the Python driver.
    pub fn from_env() -> Self {
        let mut options = Self::default();
        if let Ok(list) = std::env::var("NX_REGCACHE_REGS") {
            if list != "all" {
                options.general = list
                    .split(',')
                    .filter_map(|part| part.trim().parse::<u32>().ok())
                    .filter(|&r| r < 32)
                    .fold(0, |mask, r| mask | (1 << r));
            }
        }
        options.enabled = std::env::var("NX_REGCACHE").as_deref() != Ok("0");
        options.flags = std::env::var("NX_REGCACHE_FLAGS").as_deref() != Ok("0");
        options.vectors = std::env::var("NX_REGCACHE_V").as_deref() != Ok("0");
        options
    }
}

fn index(cap: &Captures<'_>, group: usize) -> Option<u32> {
    cap.get(group)?.as_str().parse().ok().filter(|&n| n < 32)
}

fn code(line: &str) -> &str {
    line.split_once(" // ").map_or(line, |(code, _)| code)
}

fn terminal(stmt: &str) -> bool {
    let stmt = stmt.trim();
    stmt.starts_with("goto ")
        || stmt == "return;"
        || stmt.starts_with("throw")
        || (stmt.starts_with('{') && stmt.ends_with("return; }") && !stmt.starts_with("{ if"))
        || (stmt.starts_with("switch (") && stmt.contains("default: nx::call"))
        || stmt.starts_with("nx::unimpl(")
}

/// End of the call starting at the opening parenthesis, including its semicolon.
fn call_end(line: &str, paren: usize) -> Option<usize> {
    let mut depth = 0;
    let mut quoted = false;
    for (index, byte) in line.bytes().enumerate().skip(paren) {
        match byte {
            b'"' => quoted = !quoted,
            b'(' if !quoted => depth += 1,
            b')' if !quoted => {
                depth -= 1;
                if depth == 0 {
                    return (line.as_bytes().get(index + 1) == Some(&b';')).then_some(index + 2);
                }
            }
            _ => {}
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Action {
    Sync(usize),
    Write(u32),
}

#[derive(Default)]
struct Line {
    actions: Vec<(usize, Action)>,
    conditional: bool,
    successors: Vec<usize>,
}

fn line_info(lines: &[String]) -> Result<Vec<Line>> {
    let labels: BTreeMap<_, _> = lines
        .iter()
        .enumerate()
        .filter_map(|(k, line)| {
            LABEL
                .captures(line.trim())
                .and_then(|m| Some((m.get(1)?.as_str(), k)))
        })
        .collect();
    lines
        .iter()
        .enumerate()
        .map(|(k, line)| {
            let next = (k + 1 < lines.len()).then_some(k + 1);
            if k == 0 || line.contains(RESTORE) {
                return Ok(Line {
                    successors: next.into_iter().collect(),
                    ..Line::default()
                });
            }
            let stmt = code(line);
            let mut actions = Vec::new();
            for m in WRITE.captures_iter(stmt) {
                let whole = m.get(0).expect("regex match");
                if !stmt[whole.end()..].starts_with('=') {
                    actions.extend(index(&m, 1).map(|n| (whole.start(), Action::Write(n))));
                }
            }
            for m in FLAG_WRITE.find_iter(stmt) {
                if !stmt[m.end()..].starts_with('=') {
                    actions.push((m.start(), Action::Write(FLAGS)));
                }
            }
            for m in VWRITE.captures_iter(stmt) {
                let whole = m.get(0).expect("regex match");
                if !stmt[whole.end()..].starts_with('=') {
                    actions.extend(
                        index(&m, 1)
                            .or_else(|| index(&m, 3))
                            .map(|n| (whole.start(), Action::Write(VECTOR_BASE + n))),
                    );
                }
            }
            let mut past = 0;
            for m in CALL.captures_iter(stmt) {
                let whole = m.get(0).expect("regex match");
                let before = stmt[..whole.start()].chars().next_back();
                if whole.start() < past
                    || PURE.contains(&&m[1])
                    || before.is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '>'))
                {
                    continue;
                }
                let paren = m
                    .get(2)
                    .map_or_else(|| m.get(1).expect("name").end(), |part| part.end());
                let end = call_end(stmt, paren).ok_or_else(|| {
                    Error::Cache(format!(
                        "regcache: call inside an expression: {}",
                        stmt.trim()
                    ))
                })?;
                actions.push((whole.start(), Action::Sync(end)));
                past = end;
            }
            actions.sort_unstable();
            let leading = stmt.trim_start();
            let conditional = leading.starts_with("if ")
                || leading.starts_with("if(")
                || leading.starts_with("switch");
            let mut successors: Vec<_> = GOTO
                .captures_iter(stmt)
                .filter_map(|m| labels.get(&m[1]).copied())
                .collect();
            if !terminal(stmt) {
                successors.extend(next);
            }
            Ok(Line {
                actions,
                conditional,
                successors,
            })
        })
        .collect()
}

fn writes_at_entry(info: &[Line]) -> Vec<u128> {
    let mut dirty = vec![0u128; info.len()];
    loop {
        let mut changed = false;
        for (k, line) in info.iter().enumerate() {
            let mut outgoing = dirty[k];
            for &(_, action) in &line.actions {
                match action {
                    Action::Write(reg) => outgoing |= 1 << reg,
                    Action::Sync(_) if !line.conditional => outgoing = 0,
                    Action::Sync(_) => {}
                }
            }
            for &successor in &line.successors {
                let after = dirty[successor] | outgoing;
                changed |= after != dirty[successor];
                dirty[successor] = after;
            }
        }
        if !changed {
            return dirty;
        }
    }
}

struct Locals {
    general: Vec<u32>,
    vectors: Vec<u32>,
    flags: bool,
}

impl Locals {
    fn new(lines: &[String], options: &CacheOptions) -> Self {
        let use_mask = |pattern: &Regex| {
            lines
                .iter()
                .flat_map(|line| pattern.captures_iter(line))
                .filter_map(|cap| index(&cap, 1))
                .fold(0u32, |mask, n| mask | (1 << n))
        };
        let general = use_mask(&REG) & options.general;
        let vectors = if options.vectors { use_mask(&VREG) } else { 0 };
        let used = |mask: u32| (0..32).filter(|&n| mask & (1 << n) != 0).collect();
        Self {
            general: used(general),
            vectors: used(vectors),
            flags: options.flags
                && lines
                    .iter()
                    .any(|line| FLAG.is_match(line) || FLAG_HELPER.is_match(line)),
        }
    }

    fn rewrite(&self, line: &str) -> String {
        if line.contains(RESTORE) {
            return line.to_owned();
        }
        let line = REG.replace_all(line, |cap: &Captures<'_>| {
            if index(cap, 1).is_some_and(|n| self.general.contains(&n)) {
                format!("x{}", &cap[1])
            } else {
                cap[0].to_owned()
            }
        });
        let line = if self.vectors.is_empty() {
            line
        } else {
            VREG.replace_all(&line, "vr$1").into_owned().into()
        };
        if !self.flags {
            return line.into_owned();
        }
        let line = FLAG.replace_all(&line, "fl_.$1");
        FLAG_HELPER
            .replace_all(&line, |cap: &Captures<'_>| {
                let templates = cap.get(2).map_or("", |m| m.as_str());
                format!("{}{templates}(fl_{}", &cap[1], &cap[3])
            })
            .into_owned()
    }

    fn declarations(&self, out: &mut Vec<String>) {
        if !self.general.is_empty() {
            let init: Vec<_> = self
                .general
                .iter()
                .map(|n| format!("x{n} = c.x[{n}]"))
                .collect();
            out.push(format!("    uint64_t {};", init.join(", ")));
        }
        if self.flags {
            out.push("    nx::Flags fl_{c.nf, c.zf, c.cf, c.vf};".to_owned());
        }
        if !self.vectors.is_empty() {
            let init: Vec<_> = self
                .vectors
                .iter()
                .map(|n| format!("vr{n} = c.v[{n}]"))
                .collect();
            out.push(format!("    nx::V128 {};", init.join(", ")));
        }
    }

    fn fill(&self) -> String {
        let mut out = String::new();
        for n in &self.general {
            out.push_str(&format!(" x{n} = c.x[{n}];"));
        }
        for n in &self.vectors {
            out.push_str(&format!(" vr{n} = c.v[{n}];"));
        }
        if self.flags {
            out.push_str(" fl_ = {c.nf, c.zf, c.cf, c.vf};");
        }
        out
    }

    fn spill(&self, dirty: u128) -> String {
        let mut out = String::new();
        for n in &self.general {
            if dirty & (1 << n) != 0 {
                out.push_str(&format!("c.x[{n}] = x{n}; "));
            }
        }
        for n in &self.vectors {
            if dirty & (1 << (VECTOR_BASE + n)) != 0 {
                out.push_str(&format!("c.v[{n}] = vr{n}; "));
            }
        }
        if self.flags && dirty & (1 << FLAGS) != 0 {
            out.push_str("c.nf = fl_.nf; c.zf = fl_.zf; c.cf = fl_.cf; c.vf = fl_.vf; ");
        }
        out
    }
}

pub(super) fn apply(lines: &[String], options: &CacheOptions) -> Result<Vec<String>> {
    if !options.enabled {
        return Ok(lines.to_vec());
    }
    let locals = Locals::new(lines, options);
    if locals.general.is_empty() && locals.vectors.is_empty() && !locals.flags {
        return Ok(lines.to_vec());
    }
    let lines: Vec<_> = lines.iter().map(|line| locals.rewrite(line)).collect();
    let info = line_info(&lines)?;
    let dirty = writes_at_entry(&info);
    let fill = locals.fill();
    let mut out = Vec::new();
    for (k, line) in lines.into_iter().enumerate() {
        if k == 0 {
            out.push(line);
            locals.declarations(&mut out);
        } else if line.contains(RESTORE) {
            out.push(line + &fill);
        } else {
            let stmt = code(&line);
            let comment = &line[stmt.len()..];
            let mut synced = String::new();
            let mut at = 0;
            let mut dirty = dirty[k];
            for &(start, action) in &info[k].actions {
                match action {
                    Action::Write(n) => dirty |= 1 << n,
                    Action::Sync(end) => {
                        synced.push_str(&stmt[at..start]);
                        synced.push_str(&format!(
                            "{{ {}{}{fill} }}",
                            locals.spill(dirty),
                            &stmt[start..end]
                        ));
                        at = end;
                        if !info[k].conditional {
                            dirty = 0;
                        }
                    }
                }
            }
            synced.push_str(&stmt[at..]);
            if synced.contains("return;") {
                let replacement = format!("{{ {}return; }}", locals.spill(dirty));
                synced = RETURN
                    .replace_all(&synced, replacement.as_str())
                    .into_owned();
            }
            synced.push_str(comment);
            out.push(synced);
        }
    }
    Ok(out)
}
