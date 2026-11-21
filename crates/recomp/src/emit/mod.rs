//! Emit C++ for AArch64 instructions (roadmap 3.03 and 3.04).
//!
//! The Prepare driver remains in Python until its Rust replacement lands in 3.05.
//! Capstone provides its original disassembly and operand details, including aliases.

mod extension;
mod fp;
mod integer;
mod memory;
mod neon;
mod neon_wide;
mod regcache;
mod structured;

pub use regcache::CacheOptions;

use crate::{
    analyse::{Analysis, Function},
    Module,
};
use capstone::arch::arm64::{
    self, Arm64CC, Arm64Extender, Arm64Operand, Arm64OperandType, Arm64Shift,
};
use capstone::{prelude::*, Insn};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

const B: &str = "NXB";

/// Optional instrumentation and register caching for generated functions.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub hooks: BTreeSet<u64>,
    pub trace_sp: BTreeSet<u64>,
    pub cache: CacheOptions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Decoder(capstone::Error),
    Undecodable,
    Unsupported(String),
    FunctionRange { start: u64, end: u64 },
    Cache(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decoder(e) => write!(f, "{e}"),
            Self::Undecodable => f.write_str("undecodable instruction"),
            Self::Unsupported(reason) | Self::Cache(reason) => f.write_str(reason),
            Self::FunctionRange { start, end } => {
                write!(f, "invalid function range {start:#x}..{end:#x}")
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<capstone::Error> for Error {
    fn from(error: capstone::Error) -> Self {
        Self::Decoder(error)
    }
}

fn unsupported(reason: &str) -> Error {
    Error::Unsupported(reason.to_owned())
}

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Coverage {
    pub handled: usize,
    pub unhandled: BTreeMap<String, usize>,
}

struct Instruction<'a> {
    mnemonic: &'a str,
    op_str: &'a str,
    address: u64,
    word: u32,
    operands: Vec<Arm64Operand>,
    cc: Arm64CC,
    writeback: bool,
}

impl Instruction<'_> {
    fn op(&self, index: usize) -> Result<&Arm64Operand> {
        self.operands
            .get(index)
            .ok_or_else(|| unsupported("missing operand"))
    }

    fn immediate(&self, index: usize) -> Result<i64> {
        match self.op(index)?.op_type {
            Arm64OperandType::Imm(value) => Ok(value),
            _ => Err(unsupported("expected immediate")),
        }
    }

    fn cond(&self) -> Result<&'static str> {
        use Arm64CC::*;
        Ok(match self.cc {
            ARM64_CC_EQ => "c.zf",
            ARM64_CC_NE => "!c.zf",
            ARM64_CC_HS => "c.cf",
            ARM64_CC_LO => "!c.cf",
            ARM64_CC_MI => "c.nf",
            ARM64_CC_PL => "!c.nf",
            ARM64_CC_VS => "c.vf",
            ARM64_CC_VC => "!c.vf",
            ARM64_CC_HI => "(c.cf && !c.zf)",
            ARM64_CC_LS => "(!c.cf || c.zf)",
            ARM64_CC_GE => "(c.nf == c.vf)",
            ARM64_CC_LT => "(c.nf != c.vf)",
            ARM64_CC_GT => "(!c.zf && c.nf == c.vf)",
            ARM64_CC_LE => "(c.zf || c.nf != c.vf)",
            ARM64_CC_AL | ARM64_CC_NV => "true",
            ARM64_CC_INVALID => return Err(unsupported("condition")),
        })
    }
}

#[derive(Clone, Copy)]
struct Reg {
    n: Option<u8>,
    bits: u32,
    gp: bool,
}

impl Reg {
    fn parse(name: &str) -> Result<Self> {
        let (n, bits, gp) = match name {
            "sp" => (Some(31), 64, true),
            "wsp" => (Some(31), 32, true),
            "xzr" => (None, 64, true),
            "wzr" => (None, 32, true),
            "fp" => (Some(29), 64, true),
            "lr" => (Some(30), 64, true),
            _ => {
                let Some((kind, index)) = name.split_at_checked(1) else {
                    return Err(unsupported("register without a name"));
                };
                let (bits, gp) = match kind {
                    "x" => (64, true),
                    "w" => (32, true),
                    "b" => (8, false),
                    "h" => (16, false),
                    "s" => (32, false),
                    "d" => (64, false),
                    "q" | "v" => (128, false),
                    _ => return Err(unsupported(&format!("register {name}"))),
                };
                let index: u8 = index
                    .parse()
                    .map_err(|_| unsupported(&format!("register {name}")))?;
                (Some(index), bits, gp)
            }
        };
        Ok(Self { n, bits, gp })
    }

    fn index(self) -> Result<u8> {
        self.n
            .ok_or_else(|| unsupported("expected a numbered register"))
    }

    fn read(self) -> Result<String> {
        if !self.gp {
            return Err(unsupported("no handler"));
        }
        Ok(match (self.n, self.bits) {
            (None, 64) => "0ull".to_owned(),
            (None, _) => "0u".to_owned(),
            (Some(n), 64) => format!("c.x[{n}]"),
            (Some(n), _) => format!("uint32_t(c.x[{n}])"),
        })
    }

    fn write(self, expr: impl fmt::Display) -> Result<String> {
        if !self.gp {
            return Err(unsupported("no handler"));
        }
        let bits = self.bits;
        Ok(match self.n {
            Some(n) => format!("c.x[{n}] = uint{bits}_t({expr});"),
            None => format!("(void)({expr});"),
        })
    }
}

fn width_mask(bits: u32) -> u64 {
    if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

fn hex_signed(value: i64) -> String {
    if value < 0 {
        format!("-{:#x}", value.unsigned_abs())
    } else {
        format!("{value:#x}")
    }
}

#[derive(Default)]
struct FunctionState {
    labels: BTreeSet<u64>,
    setjmp_sites: Vec<u64>,
}

/// A recompiler instance with a shared ARM64 operand decoder and coverage counts.
pub struct Emitter<'a> {
    module: &'a Module,
    analysis: &'a Analysis,
    prefix: &'a str,
    decoder: Capstone,
    entries: BTreeSet<u64>,
    pub options: Options,
    pub coverage: Coverage,
}

impl<'a> Emitter<'a> {
    pub fn new(module: &'a Module, analysis: &'a Analysis, prefix: &'a str) -> Result<Self> {
        let decoder = Capstone::new()
            .arm64()
            .mode(arm64::ArchMode::Arm)
            .detail(true)
            .build()?;
        let entries = analysis.functions.iter().map(|f| f.start).collect();
        Ok(Self {
            module,
            analysis,
            prefix,
            decoder,
            entries,
            options: Options::default(),
            coverage: Coverage::default(),
        })
    }

    pub fn function_name(&self, address: u64) -> String {
        format!("{}_{address:08X}", self.prefix)
    }

    fn reg_id(&self, id: RegId) -> Result<Reg> {
        Reg::parse(
            &self
                .decoder
                .reg_name(id)
                .ok_or_else(|| unsupported("register without a name"))?,
        )
    }

    fn reg(&self, op: &Arm64Operand) -> Result<Reg> {
        match op.op_type {
            Arm64OperandType::Reg(id) => self.reg_id(id),
            _ => Err(unsupported("expected register")),
        }
    }

    fn reg_at(&self, insn: &Instruction<'_>, index: usize) -> Result<Reg> {
        self.reg(insn.op(index)?)
    }

    fn read(&self, insn: &Instruction<'_>, index: usize) -> Result<String> {
        self.reg_at(insn, index)?.read()
    }

    fn read_shifted(&self, op: &Arm64Operand, bits: u32) -> Result<String> {
        if let Arm64OperandType::Imm(value) = op.op_type {
            let value = match op.shift {
                Arm64Shift::Lsl(n) => (value as u64).wrapping_shl(n),
                _ => value as u64,
            } & width_mask(bits);
            return Ok(format!("uint{bits}_t({value:#x}ull)"));
        }
        let value = self.reg(op)?.read()?;
        use Arm64Extender::*;
        let extended = match op.ext {
            ARM64_EXT_UXTB => Some(("uint8_t", false)),
            ARM64_EXT_UXTH => Some(("uint16_t", false)),
            ARM64_EXT_UXTW => Some(("uint32_t", false)),
            ARM64_EXT_UXTX => Some(("uint64_t", false)),
            ARM64_EXT_SXTB => Some(("int8_t", true)),
            ARM64_EXT_SXTH => Some(("int16_t", true)),
            ARM64_EXT_SXTW => Some(("int32_t", true)),
            ARM64_EXT_SXTX => Some(("int64_t", true)),
            ARM64_EXT_INVALID => None,
        };
        if let Some((src, signed)) = extended {
            let mid = if signed { "int" } else { "uint" };
            let extended = format!("uint{bits}_t({mid}{bits}_t({src}({value})))");
            return Ok(match op.shift {
                Arm64Shift::Lsl(n) if n != 0 => format!("uint{bits}_t({extended} << {n})"),
                _ => extended,
            });
        }
        use Arm64Shift::*;
        Ok(match op.shift {
            Lsl(n) if n != 0 => format!("uint{bits}_t(uint{bits}_t({value}) << {n})"),
            Lsr(n) if n != 0 => format!("uint{bits}_t(uint{bits}_t({value}) >> {n})"),
            Asr(n) if n != 0 => format!("uint{bits}_t(int{bits}_t({value}) >> {n})"),
            Ror(n) if n != 0 => format!("nx::ror{bits}({value}, {n})"),
            Msl(n) if n != 0 => return Err(unsupported("shift")),
            _ => format!("uint{bits}_t({value})"),
        })
    }

    fn instruction_details<'i>(&self, decoded: &'i Insn<'_>, word: u32) -> Result<Instruction<'i>> {
        let mnemonic = decoded.mnemonic().unwrap_or("");
        let op_str = decoded.op_str().unwrap_or("");
        let address = decoded.address();
        // MRS/MSR can encode implementation-defined system registers. As in the
        // Python emitter, read the register and system-register fields from the word.
        if matches!(mnemonic, "mrs" | "msr") {
            return Ok(Instruction {
                mnemonic,
                op_str,
                address,
                word,
                operands: Vec::new(),
                cc: Arm64CC::ARM64_CC_INVALID,
                writeback: false,
            });
        }
        let detail = self.decoder.insn_detail(decoded)?;
        let arch = detail.arch_detail();
        let arm64 = arch.arm64().ok_or(Error::Undecodable)?;
        Ok(Instruction {
            mnemonic,
            op_str,
            address,
            word,
            operands: arm64.operands().collect(),
            cc: arm64.cc(),
            writeback: arm64.writeback(),
        })
    }

    /// Emit one statement, without comments, register caching or instrumentation.
    /// This is the shared entry point with the native/Unicorn differential harness.
    pub fn instruction(&self, f: &Function, address: u64, word: u32) -> Result<String> {
        let bytes = word.to_le_bytes();
        let decoded = self.decoder.disasm_count(&bytes, address, 1)?;
        let decoded = decoded.iter().next().ok_or(Error::Undecodable)?;
        let info = self.instruction_details(decoded, word)?;
        self.statement(f, &info, &mut FunctionState::default())
    }

    /// Emit the C++ function and update coverage. Out-of-scope instructions trap.
    pub fn function(&mut self, f: &Function) -> Result<String> {
        let start = usize::try_from(f.start).ok();
        let end = usize::try_from(f.end).ok();
        let bytes = match start.zip(end) {
            Some((start, end)) if start % 4 == 0 && end % 4 == 0 => {
                self.module.image.get(start..end)
            }
            _ => None,
        }
        .ok_or(Error::FunctionRange {
            start: f.start,
            end: f.end,
        })?;
        let mut state = FunctionState::default();
        let mut body = Vec::new();
        let mut last_terminal = false;
        for (offset, data) in bytes.as_chunks::<4>().0.iter().enumerate() {
            let address = f.start + offset as u64 * 4;
            let word = u32::from_le_bytes(*data);
            let decoded = self.decoder.disasm_count(data, address, 1)?;
            let Some(decoded) = decoded.iter().next() else {
                body.push((
                    address,
                    format!("nx::unimpl(c, {B} + {address:#x}ull, \"undecodable {word:#010x}\");"),
                ));
                last_terminal = true;
                continue;
            };
            let insn = self.instruction_details(decoded, word)?;
            let mut stmt = match self.statement(f, &insn, &mut state) {
                Ok(stmt) => {
                    self.coverage.handled += 1;
                    stmt
                }
                Err(Error::Unsupported(error)) => {
                    *self
                        .coverage
                        .unhandled
                        .entry(insn.mnemonic.to_owned())
                        .or_default() += 1;
                    let text = format!("{} {}", insn.mnemonic, insn.op_str).replace('"', "'");
                    format!("nx::unimpl(c, {B} + {address:#x}ull, \"{text}\"); // {error}")
                }
                Err(error) => return Err(error),
            };
            if self.options.hooks.contains(&address) {
                stmt = format!("nx_hook(c, {B} + {address:#x}ull); {stmt}");
            }
            if self.options.trace_sp.contains(&f.start)
                && (insn
                    .op_str
                    .split(',')
                    .next()
                    .is_some_and(|first| first.contains("sp"))
                    || insn.op_str.contains("[sp") && insn.writeback)
            {
                stmt.push_str(&format!(" nx::trace_sp(c, {B} + {address:#x}ull);"));
            }
            last_terminal = matches!(insn.mnemonic, "b" | "br" | "ret" | "udf" | "brk")
                || stmt.ends_with("return; }");
            body.push((
                address,
                format!("{stmt} // {} {}", insn.mnemonic, insn.op_str),
            ));
        }
        let mut lines = vec![format!(
            "void {}(nx::CPU& NX_RESTRICT c) {{",
            self.function_name(f.start)
        )];
        if self.options.hooks.range(f.start..f.end).next().is_some() {
            lines.push("    void nx_hook(nx::CPU&, uint64_t);".to_owned());
        }
        let setjmps = state.setjmp_sites.len();
        if setjmps != 0 {
            lines.push(format!("    nx::JmpState js_[{setjmps}]; int jr_ = -1;"));
            lines.push("    for (;;) { try {".to_owned());
            let cases: Vec<String> = state
                .setjmp_sites
                .iter()
                .enumerate()
                .map(|(k, address)| format!("case {k}: goto L_{address:X};"))
                .collect();
            lines.push(format!(
                "    switch (jr_) {{ {} default: break; }}",
                cases.join(" ")
            ));
        }
        for (address, stmt) in body {
            if state.labels.contains(&address) {
                lines.push(format!("L_{address:X}:;"));
            }
            lines.push(format!("    {stmt}"));
        }
        if !last_terminal {
            if self.entries.contains(&f.end) {
                lines.push(format!("    {}(c);", self.function_name(f.end)));
            } else {
                lines.push(format!("    nx::call(c, {B} + {:#x}ull);", f.end));
            }
        }
        if setjmps != 0 {
            lines.extend([
                "    return;".to_owned(),
                "    } catch (const nx::LongJmp& e_) {".to_owned(),
                "        jr_ = -1;".to_owned(),
                format!("        for (int k = 0; k < {setjmps}; k++) if (js_[k].buf == e_.buf) {{ jr_ = k; break; }}"),
                "        if (jr_ < 0) throw;".to_owned(),
                "        js_[jr_].restore(c); c.x[0] = e_.val ? e_.val : 1;".to_owned(),
                "    } }".to_owned(),
            ]);
        }
        lines.push("}".to_owned());
        Ok(regcache::apply(&lines, &self.options.cache)?.join("\n"))
    }

    fn goto_or_tail(&self, f: &Function, target: i64, state: &mut FunctionState) -> String {
        if let Ok(address) = u64::try_from(target) {
            if f.start <= address && address < f.end {
                state.labels.insert(address);
                return format!("goto L_{address:X};");
            }
            if self.entries.contains(&address) {
                return format!("{{ {}(c); return; }}", self.function_name(address));
            }
        }
        format!(
            "{{ nx::call(c, {B} + {}ull); return; }}",
            hex_signed(target)
        )
    }

    fn branch(
        &self,
        f: &Function,
        i: &Instruction<'_>,
        state: &mut FunctionState,
    ) -> Result<Option<String>> {
        let result = match i.mnemonic {
            "b" => self.goto_or_tail(f, i.immediate(0)?, state),
            "bl" => {
                let target = i.immediate(0)?;
                let next = i.address + 4;
                let direct = u64::try_from(target).ok();
                if direct.is_some_and(|target| {
                    [
                        &self.analysis.plt,
                        &self.analysis.plt_local,
                        &self.analysis.names,
                    ]
                    .into_iter()
                    .any(|map| {
                        matches!(
                            map.get(&target).map(String::as_str),
                            Some("setjmp" | "_setjmp" | "__setjmp")
                        )
                    })
                }) {
                    let site = state.setjmp_sites.len();
                    state.setjmp_sites.push(next);
                    state.labels.insert(next);
                    format!("js_[{site}].save(c); c.x[0] = 0;")
                } else {
                    let call = match direct.filter(|t| self.entries.contains(t)) {
                        Some(t) => format!("{}(c);", self.function_name(t)),
                        None => format!("nx::call(c, {B} + {}ull);", hex_signed(target)),
                    };
                    format!("c.x[30] = {B} + {next:#x}ull; {call}")
                }
            }
            "blr" => format!(
                "{{ uint64_t t = {}; c.x[30] = {B} + {:#x}ull; nx::call(c, t); }}",
                self.read(i, 0)?,
                i.address + 4
            ),
            "br" => {
                let r = self.read(i, 0)?;
                if let Some(targets) = self.analysis.jump_tables.get(&i.address) {
                    let cases: Vec<_> = targets
                        .iter()
                        .map(|&target| {
                            format!(
                                "case {B} + {target:#x}ull: {}",
                                self.goto_or_tail(f, target as i64, state)
                            )
                        })
                        .collect();
                    format!(
                        "switch ({r}) {{ {} default: nx::call(c, {r}); return; }}",
                        cases.join(" ")
                    )
                } else {
                    format!("{{ nx::call(c, {r}); return; }}")
                }
            }
            "ret" if !i.operands.is_empty() && self.reg_at(i, 0)?.n != Some(30) => {
                format!("{{ nx::call(c, {}); return; }}", self.read(i, 0)?)
            }
            "ret" => "return;".to_owned(),
            "cbz" | "cbnz" => {
                let op = if i.mnemonic == "cbz" { "==" } else { "!=" };
                format!(
                    "if ({} {op} 0) {}",
                    self.read(i, 0)?,
                    self.goto_or_tail(f, i.immediate(1)?, state)
                )
            }
            "tbz" | "tbnz" => {
                let op = if i.mnemonic == "tbz" { "==" } else { "!=" };
                format!(
                    "if ((({} >> {}) & 1) {op} 0) {}",
                    self.read(i, 0)?,
                    i.immediate(1)?,
                    self.goto_or_tail(f, i.immediate(2)?, state)
                )
            }
            mnemonic if mnemonic.starts_with("b.") => format!(
                "if ({}) {}",
                i.cond()?,
                self.goto_or_tail(f, i.immediate(0)?, state)
            ),
            _ => return Ok(None),
        };
        Ok(Some(result))
    }

    fn statement(
        &self,
        f: &Function,
        i: &Instruction<'_>,
        state: &mut FunctionState,
    ) -> Result<String> {
        if let Some(statement) = self.branch(f, i, state)? {
            return Ok(statement);
        }
        if let Some(statement) = self.misc(i)? {
            return Ok(statement);
        }
        if let Some(statement) = self.structured(i)? {
            return Ok(statement);
        }
        if let Some(statement) = self.extension(i)? {
            return Ok(statement);
        }
        if let Some(statement) = self.neon(i)? {
            return Ok(statement);
        }
        if let Some(statement) = self.neon_wide(i)? {
            return Ok(statement);
        }
        if let Some(statement) = self.fp(i)? {
            return Ok(statement);
        }
        if let Some(statement) = self.integer(i)? {
            return Ok(statement);
        }
        if let Some(statement) = self.memory(i)? {
            return Ok(statement);
        }
        self.lse(i)?.ok_or_else(|| unsupported("no handler"))
    }

    fn misc(&self, i: &Instruction<'_>) -> Result<Option<String>> {
        let address = i.address;
        let result = match i.mnemonic {
            "nop" | "hint" | "prfm" | "prfum" | "yield" | "wfe" | "wfi" | "sev" | "sevl" | "ic"
            | "autia1716" | "autib1716" | "pacia1716" | "pacib1716" | "isb" | "xpaclri"
            | "csdb" => ";".to_owned(),
            "dc" if i.op_str.contains("zva") => {
                let register = i
                    .op_str
                    .split(',')
                    .nth(1)
                    .ok_or_else(|| unsupported("missing operand"))?;
                let n = Reg::parse(register.trim())?.index()?;
                format!("std::memset(reinterpret_cast<void*>(c.x[{n}] & ~63ull), 0, 64);")
            }
            "dc" => ";".to_owned(),
            "crc32b" | "crc32h" | "crc32w" | "crc32x" | "crc32cb" | "crc32ch" | "crc32cw"
            | "crc32cx" => {
                let size = match i.mnemonic.as_bytes().last() {
                    Some(b'b') => 1,
                    Some(b'h') => 2,
                    Some(b'w') => 4,
                    _ => 8,
                };
                let poly = if i.mnemonic.starts_with("crc32c") {
                    0x82F63B78u32
                } else {
                    0xEDB88320u32
                };
                self.reg_at(i, 0)?.write(format!(
                    "nx::crc32<{size}>({}, {}, {poly:#x}u)",
                    self.read(i, 1)?,
                    self.read(i, 2)?
                ))?
            }
            "dmb" | "dsb" => "nx::barrier();".to_owned(),
            "clrex" => "nx::clrex(c);".to_owned(),
            "udf" | "brk" => format!("nx::unimpl(c, {B} + {address:#x}ull, \"{}\");", i.mnemonic),
            "svc" => format!("nx::svc(c, {}, {B} + {address:#x}ull);", i.immediate(0)?),
            "mrs" | "msr" => self.system(i)?,
            _ => return Ok(None),
        };
        Ok(Some(result))
    }

    fn system(&self, i: &Instruction<'_>) -> Result<String> {
        let index = (i.word & 31) as u8;
        let reg = Reg {
            n: (index != 31).then_some(index),
            bits: 64,
            gp: true,
        };
        let sr = (i.word >> 5) & 0xffff;
        if i.mnemonic == "mrs" {
            let value = match sr {
                0xDE82 => "c.tpidr".to_owned(),
                0xDE83 => "c.tpidrro".to_owned(),
                0xDA20 => "c.fpcr".to_owned(),
                0xDA21 => "c.fpsr".to_owned(),
                0xDA10 => "nx::get_nzcv(c)".to_owned(),
                _ => format!("nx::mrs(c, {sr:#x})"),
            };
            return reg.write(value);
        }
        if i.word >> 20 != 0xD51 {
            return Err(unsupported("msr pstate"));
        }
        let value = reg.read()?;
        Ok(match sr {
            0xDE82 => format!("c.tpidr = {value};"),
            0xDA20 => format!("c.fpcr = {value};"),
            0xDA21 => format!("c.fpsr = {value};"),
            0xDA10 => format!("nx::set_nzcv(c, uint32_t({value} >> 28));"),
            _ => format!("nx::msr(c, {sr:#x}, {value});"),
        })
    }
}
