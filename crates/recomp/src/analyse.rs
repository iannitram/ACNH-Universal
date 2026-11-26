//! Whole-module analysis: functions, PLT stubs, jump tables.
//! Instructions are decoded on demand from the image (`decode.rs`) instead of held in a table.

use crate::decode::{decode, Insn};
use crate::ehframe::{self, Fde};
use crate::load::{
    self, Error, Module, R_AARCH64_GLOB_DAT, R_AARCH64_JUMP_SLOT, R_AARCH64_RELATIVE,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The module's text as an address -> instruction map.
pub struct Insns<'a> {
    m: &'a Module,
    t0: u64,
    n: u64,
}

impl<'a> Insns<'a> {
    pub fn new(m: &'a Module) -> Self {
        Insns {
            m,
            t0: m.text.0,
            n: m.text.1 / 4,
        }
    }

    pub fn get(&self, a: u64) -> Option<Insn> {
        if a < self.t0 || a & 3 != 0 || (a - self.t0) >> 2 >= self.n {
            return None;
        }
        let at = a as usize;
        if at.checked_add(4)? > self.m.image.len() {
            return None;
        }
        decode(self.m.u32(a), a)
    }

    pub fn contains(&self, a: u64) -> bool {
        self.get(a).is_some()
    }

    /// Every instruction in address order.
    pub fn items(&self) -> impl Iterator<Item = (u64, Insn)> + '_ {
        (0..self.n).filter_map(move |i| {
            let a = self.t0 + 4 * i;
            self.get(a).map(|x| (a, x))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub start: u64,
    pub end: u64,
    pub fde: bool,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Analysis {
    /// Sorted by start.
    pub functions: Vec<Function>,
    /// Stub address -> import name.
    pub plt: BTreeMap<u64, String>,
    pub got_imports: BTreeMap<u64, String>,
    pub got_local: BTreeMap<u64, String>,
    pub plt_local: BTreeMap<u64, String>,
    /// `br` address -> sorted targets.
    pub jump_tables: BTreeMap<u64, Vec<u64>>,
    pub names: BTreeMap<u64, String>,
    pub unresolved_br: Vec<u64>,
}

impl Analysis {
    pub fn function_at(&self, addr: u64) -> Option<&Function> {
        let i = self.functions.partition_point(|f| f.start <= addr);
        let f = self.functions.get(i.checked_sub(1)?)?;
        (addr < f.end).then_some(f)
    }
}

/// `patched`: addresses a variant patch wrote; a function whose patched code runs on past the end its
/// FDE gives keeps those words.
pub fn analyse(m: &Module, log: &mut dyn FnMut(&str), patched: &[u64]) -> Result<Analysis, Error> {
    let insns = Insns::new(m);
    let (t0, tn) = m.text;
    let text_end = t0.saturating_add(tn);
    let mut an = Analysis {
        got_imports: load::plt_imports(m),
        ..Analysis::default()
    };
    for r in &m.relocs {
        if matches!(r.r#type, R_AARCH64_JUMP_SLOT | R_AARCH64_GLOB_DAT) && r.sym != 0 {
            if let Some(s) = m.symbols.get(r.sym as usize) {
                if !s.imported() {
                    an.got_local.insert(r.offset, s.name.clone());
                }
            }
        }
    }
    for s in &m.symbols {
        if !s.name.is_empty() && !s.imported() && m.in_text(s.value) {
            an.names.insert(s.value, s.name.clone());
        }
    }

    // PLT stubs: adrp x16, page / ldr x17, [x16, #off] / add x16, x16, #off / br x17
    for (a, ins) in insns.items() {
        if let Insn::Adrp { rd: 16, page } = ins {
            if let (
                Some(Insn::Ldr {
                    rt: 17,
                    rn: 16,
                    uoff,
                    wide: true,
                    ..
                }),
                Some(Insn::Br(17)),
            ) = (insns.get(a + 4), insns.get(a + 12))
            {
                let got = page.wrapping_add(uoff.unwrap_or(0));
                let name = an
                    .got_imports
                    .get(&got)
                    .cloned()
                    .unwrap_or_else(|| format!("got_{got:x}"));
                an.plt.insert(a, name);
                if let Some(l) = an.got_local.get(&got) {
                    an.plt_local.insert(a, l.clone());
                }
            }
        }
    }

    // Seeds.
    let fdes: Vec<Fde> = ehframe::parse(m)?
        .into_iter()
        .filter(|f| t0 <= f.start && f.start < text_end)
        .collect();
    let mut entries: BTreeSet<u64> = fdes.iter().map(|f| f.start).collect();
    entries.extend(an.plt.keys().copied());
    entries.extend(an.names.keys().copied());
    entries.extend(
        m.init_array
            .iter()
            .chain(&m.fini_array)
            .copied()
            .filter(|&a| t0 <= a && a < text_end),
    );
    for r in &m.relocs {
        if r.r#type == R_AARCH64_RELATIVE && r.addend >= 0 && insns.contains(r.addend as u64) {
            entries.insert(r.addend as u64);
        }
    }
    for (a, ins) in insns.items() {
        match ins {
            Insn::Bl(t) => {
                entries.insert(t);
            }
            Insn::Adr { target, .. }
                if t0 <= target
                    && target < text_end
                    && insns.contains(target)
                    && looks_like_code_pointer(&insns, a) =>
            {
                entries.insert(target);
            }
            _ => {}
        }
    }
    if insns.contains(t0) {
        entries.insert(t0);
    }
    let mut fde_bounds: BTreeMap<u64, u64> = BTreeMap::new();
    for f in &fdes {
        fde_bounds.insert(f.start, f.start.saturating_add(f.size).min(text_end));
    }
    if !patched.is_empty() {
        let grown: BTreeSet<u64> = patched.iter().copied().collect();
        for end in fde_bounds.values_mut() {
            while grown.contains(end) && !entries.contains(end) {
                *end += 4;
            }
        }
    }
    log(&format!(
        "  seeds: {} FDE, {} PLT stubs, {} entries",
        fdes.len(),
        an.plt.len(),
        entries.len()
    ));

    // Fixpoint: build ranges, resolve jump tables, promote out-of-range branch targets to entries.
    let mut converged = false;
    // A table depends only on the function's bounds and the (fixed) image, so a function that
    // keeps its bounds across passes keeps its tables instead of resolving them again.
    let mut resolved: HashMap<(u64, u64, u64), Option<Vec<u64>>> = HashMap::new();
    for it in 0..400 {
        let sorted: Vec<u64> = entries.iter().copied().collect();
        an.functions = ranges(m, &an, &sorted, &fdes, &fde_bounds);
        an.jump_tables.clear();
        let mut new: BTreeSet<u64> = BTreeSet::new();
        for f in &an.functions {
            if an.plt.contains_key(&f.start) {
                continue;
            }
            let mut a = f.start;
            while a < f.end {
                match insns.get(a) {
                    Some(Insn::B(t)) | Some(Insn::BCond(t, _)) | Some(Insn::CmpBranch(t)) => {
                        if !(f.start <= t && t < f.end)
                            && insns.contains(t)
                            && !entries.contains(&t)
                        {
                            new.insert(t);
                        }
                    }
                    Some(Insn::Br(_)) => {
                        let found = resolved
                            .entry((f.start, f.end, a))
                            .or_insert_with(|| jump_table(m, &insns, f, a));
                        if let Some(tg) = found.as_deref() {
                            let e = an.jump_tables.entry(a).or_default();
                            e.extend_from_slice(tg);
                            e.sort_unstable();
                            e.dedup();
                        }
                    }
                    _ => {}
                }
                a += 4;
            }
        }
        for f in &an.functions {
            let mut a = f.start;
            while a < f.end {
                if let Some(ts) = an.jump_tables.get(&a) {
                    for &t in ts {
                        if !(f.start <= t && t < f.end)
                            && insns.contains(t)
                            && !entries.contains(&t)
                        {
                            new.insert(t);
                        }
                    }
                }
                a += 4;
            }
        }
        log(&format!(
            "  pass {it}: {} functions, {} jump tables, +{} entries",
            an.functions.len(),
            an.jump_tables.len(),
            new.len()
        ));
        if new.is_empty() {
            converged = true;
            break;
        }
        entries.extend(new);
    }
    if !converged {
        return Err(Error::Malformed(
            "analysis: branch targets still growing after 400 passes".into(),
        ));
    }

    an.unresolved_br = insns
        .items()
        .filter(|(a, i)| {
            matches!(i, Insn::Br(_)) && !an.jump_tables.contains_key(a) && !in_plt(&an, *a)
        })
        .map(|(a, _)| a)
        .collect();
    Ok(an)
}

fn in_plt(an: &Analysis, a: u64) -> bool {
    [a.wrapping_sub(12), a.wrapping_sub(8), a.wrapping_sub(4), a]
        .iter()
        .any(|s| an.plt.contains_key(s))
}

/// An `adr` used for a jump-table anchor is followed by an `add ..., lsl #2`; those are not functions.
fn looks_like_code_pointer(insns: &Insns, a: u64) -> bool {
    !(1..4).any(|k| matches!(insns.get(a + 4 * k), Some(Insn::AddReg { shift: 2, .. })))
}

/// Function ranges: FDE bounds when known, otherwise up to the next entry. Entries inside an FDE
/// function become separate functions that run to that FDE's end (shared tails).
fn ranges(
    m: &Module,
    an: &Analysis,
    entries: &[u64],
    fdes: &[Fde],
    fde_bounds: &BTreeMap<u64, u64>,
) -> Vec<Function> {
    let text_end = m.text.0.saturating_add(m.text.1);
    let mut funcs = Vec::with_capacity(entries.len());
    for (i, &e) in entries.iter().enumerate() {
        let (end, fde) = if an.plt.contains_key(&e) {
            (e.saturating_add(16).min(text_end), false)
        } else if let Some(&end) = fde_bounds.get(&e) {
            (end, true)
        } else {
            let k = fdes.partition_point(|f| f.start <= e);
            match k.checked_sub(1).map(|j| &fdes[j]) {
                Some(f) if e < f.start.saturating_add(f.size) => {
                    (f.start.saturating_add(f.size).min(text_end), false)
                }
                _ => {
                    let mut nxt = entries.get(i + 1).copied().unwrap_or(text_end);
                    if let Some(f) = fdes.get(k) {
                        nxt = nxt.min(f.start);
                    }
                    (nxt, false)
                }
            }
        };
        funcs.push(Function {
            start: e,
            end,
            fde,
            name: an.names.get(&e).cloned().unwrap_or_default(),
        });
    }
    funcs
}

/// Last instruction in `f` before `before` (linear, backwards) that writes register `reg`.
fn find_def(insns: &Insns, f: &Function, before: u64, reg: u32) -> Option<(u64, Insn)> {
    if reg >= 31 {
        return None;
    }
    let mut a = before.checked_sub(4)?;
    while a >= f.start {
        if let Some(ins) = insns.get(a) {
            if ins.writes(reg) {
                return Some((a, ins));
            }
        }
        a = a.checked_sub(4)?;
    }
    None
}

/// Constant value of `reg` at `before` if it comes from adrp[+add] / adr / mov.
fn constant(insns: &Insns, f: &Function, before: u64, reg: u32, depth: u32) -> Option<i64> {
    let (a, ins) = find_def(insns, f, before, reg)?;
    if depth > 4 {
        return None;
    }
    match ins {
        Insn::Adr { target, .. } => Some(target as i64),
        Insn::Adrp { page, .. } => Some(page as i64),
        Insn::AddImm { rn, imm, .. } if rn != 31 => {
            let base = constant(insns, f, a, rn, depth + 1)?;
            Some(base.wrapping_add(imm as i64))
        }
        Insn::MovImm { imm, .. } => Some(imm),
        Insn::Mov { rm, .. } => constant(insns, f, a, rm, depth + 1),
        _ => None,
    }
}

/// Targets of a compressed clang jump table ending at `br`, or None.
fn jump_table(m: &Module, insns: &Insns, f: &Function, br: u64) -> Option<Vec<u64>> {
    let Some(Insn::Br(reg)) = insns.get(br) else {
        return None;
    };
    let (a_add, add) = find_def(insns, f, br, reg)?;
    let Insn::AddReg { rn, rm, shift, .. } = add else {
        return None;
    };
    // Either operand can be the loaded entry; the other one is the anchor (anchor + entry << shift).
    let mut found = None;
    for (ent, anc, anc_is_rm) in [(rm, rn, false), (rn, rm, true)] {
        if let Some((a, ld @ Insn::Ldr { ldr_name: true, .. })) = find_def(insns, f, a_add, ent) {
            if anc_is_rm && shift != 0 {
                return None;
            }
            found = Some((a, ld, constant(insns, f, a_add, anc, 0)));
            break;
        }
    }
    let (a_ld, ld, anchor) = found?;
    let anchor = anchor?;
    let Insn::Ldr {
        rn: base_reg,
        rm: Some(idx),
        mnemonic,
        ..
    } = ld
    else {
        return None;
    };
    let (size, signed) = match mnemonic {
        "ldrb" => (1usize, false),
        "ldrh" => (2, false),
        "ldrsb" => (1, true),
        "ldrsh" => (2, true),
        "ldrsw" => (4, true),
        "ldr" => (4, false),
        _ => return None,
    };
    let count = bound(insns, f, a_ld, idx);

    let entries = |table: i64| -> (Vec<u64>, u32) {
        // Without a bound, take every entry that lands on an instruction of this function (up to 256).
        let limit = count.unwrap_or(256);
        let (mut out, mut misses) = (Vec::new(), 0u32);
        for k in 0..limit as i64 {
            let p = table + k * size as i64;
            if p < 0 || p as usize + size > m.image.len() {
                break;
            }
            let raw = &m.image[p as usize..p as usize + size];
            let mut buf = [0u8; 8];
            buf[..size].copy_from_slice(raw);
            let mut v = u64::from_le_bytes(buf) as i64;
            if signed {
                let s = 64 - 8 * size as u32;
                v = (v << s) >> s;
            }
            let t = anchor.wrapping_add(v.wrapping_shl(shift));
            if !(f.start as i64 <= t && t < f.end as i64) || !insns.contains(t as u64) {
                misses += 1;
                if count.is_none() && misses > 4 {
                    break;
                }
                continue;
            }
            out.push(t as u64);
        }
        (out, misses)
    };

    let table = constant(insns, f, a_ld, base_reg, 0);
    if let Some(t) = table.filter(|&t| t >= 0 && m.in_image(t as u64)) {
        let (mut out, _) = entries(t);
        out.sort_unstable();
        out.dedup();
        return (!out.is_empty()).then_some(out);
    }
    // The base register's last definition in address order is not a constant: try every constant the
    // function puts in that register and keep the entries of those whose entries all land in this function.
    count?;
    let mut candidates: Vec<i64> = Vec::new();
    let mut a = f.start;
    while a < f.end {
        let writes_base = matches!(
            insns.get(a),
            Some(Insn::Adrp { rd, .. } | Insn::Adr { rd, .. } | Insn::AddImm { rd, .. } | Insn::AddReg { rd, .. })
                if rd == base_reg
        );
        if writes_base {
            if let Some(c) = constant(insns, f, a + 4, base_reg, 0) {
                if c >= 0 && m.in_image(c as u64) && !candidates.contains(&c) {
                    candidates.push(c);
                }
            }
        }
        a += 4;
    }
    let mut found = BTreeSet::new();
    for c in candidates {
        let (out, misses) = entries(c);
        if !out.is_empty() && misses == 0 {
            found.extend(out);
        }
    }
    (!found.is_empty()).then(|| found.into_iter().collect())
}

/// Number of table entries from `cmp idx, #N` + b.hi/b.hs before the load. The index may be a
/// zero/sign extended copy of the compared register.
fn bound(insns: &Insns, f: &Function, before: u64, idx: u32) -> Option<u32> {
    let mut regs: BTreeSet<u32> = BTreeSet::new();
    regs.insert(idx);
    let lo = f.start.max(before.saturating_sub(4 * 40));
    let mut a = before.checked_sub(4)?;
    while a >= lo {
        if let Some(ins) = insns.get(a) {
            let copy = match ins {
                Insn::Mov { rd, rm } => Some((rd, rm)),
                Insn::Copy { rd, rn } => Some((rd, rn)),
                _ => None,
            };
            if let Some((rd, src)) = copy {
                if regs.contains(&rd) {
                    regs.remove(&rd);
                    regs.insert(src);
                    a = a.checked_sub(4)?;
                    continue;
                }
            }
            if let Insn::CmpImm { rn, imm } = ins {
                if regs.contains(&rn) {
                    for k in 1..6 {
                        match insns.get(a + 4 * k) {
                            Some(Insn::BCond(_, 8)) => return Some((imm as u32).saturating_add(1)),
                            Some(Insn::BCond(_, 2)) => return Some(imm as u32),
                            _ => {}
                        }
                    }
                    return None;
                }
            }
            if regs.iter().any(|&r| ins.writes(r)) {
                return None;
            }
        }
        a = a.checked_sub(4)?;
    }
    None
}
