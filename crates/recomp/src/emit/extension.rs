//! Atomic, arithmetic and crypto extensions from recomp/emit_ext.py and emit_sdk20.py.
use super::{neon::substitute, unsupported, width_mask, Emitter, Instruction, Result};
use capstone::arch::arm64::{Arm64OperandType, Arm64Vas};
use regex::Regex;
use std::sync::LazyLock;

static LSE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(ldadd|ldclr|ldeor|ldset|ldsmax|ldsmin|ldumax|ldumin|stadd|stclr|steor|stset|stsmax|stsmin|stumax|stumin|swp|casp|cas)(al|a|l)?(b|h)?$")
        .expect("valid atomic mnemonic pattern")
});

impl Emitter<'_> {
    pub(super) fn extension(&self, i: &Instruction<'_>) -> Result<Option<String>> {
        let statement = match i.mnemonic {
            "ldaxp" | "ldxp" => {
                let lo = self.reg_at(i, 0)?;
                let hi = self.reg_at(i, 1)?;
                let t = format!("uint{}_t", lo.bits);
                let address = self.extension_address(i)?;
                let (lo, hi) = (lo.write("lo_")?, hi.write("hi_")?);
                format!("{{ {t} lo_, hi_; nx::ldxp<{t}>(c, {address}, lo_, hi_); {lo} {hi} }}")
            }
            "stlxp" | "stxp" => {
                let status = self.reg_at(i, 0)?;
                let lo = self.reg_at(i, 1)?;
                let t = format!("uint{}_t", lo.bits);
                let address = self.extension_address(i)?;
                let (lo, hi) = (lo.read()?, self.read(i, 2)?);
                status.write(format!("nx::stxp<{t}>(c, {address}, {t}({lo}), {t}({hi}))"))?
            }
            "stg" => {
                if i.operands.len() > 2 || i.writeback {
                    return Err(unsupported("stg with writeback"));
                }
                ";".to_owned()
            }
            "bfc" => {
                let d = self.reg_at(i, 0)?;
                let lsb = i.immediate(1)? as u32;
                let width = i.immediate(2)? as u32;
                let mask = !(width_mask(width) << lsb) & width_mask(d.bits);
                d.write(format!("{} & {mask:#x}ull", d.read()?))?
            }
            "umnegl" | "smnegl" => {
                let d = self.reg_at(i, 0)?;
                let (n, m) = (self.read(i, 1)?, self.read(i, 2)?);
                d.write(if i.mnemonic == "umnegl" {
                    format!("uint64_t(0) - uint64_t(uint32_t({n})) * uint64_t(uint32_t({m}))")
                } else {
                    format!("uint64_t(0 - int64_t(int32_t({n})) * int64_t(int32_t({m})))")
                })?
            }
            "sadalp" | "uadalp" => self.pairwise_long(i)?,
            "sminv" | "uminv" | "saddlv" => self.reduce_extension(i)?,
            "rshrn" | "rshrn2" => {
                self.narrow(i, "(({a} >> {n}) + (({a} >> ({n} - 1)) & 1))", "u", "u")?
            }
            "fmaxp" => self.pairwise(i, "nx::fmax_(a, b)", "f")?,
            "fminp" => self.pairwise(i, "nx::fmin_(a, b)", "f")?,
            "fmaxnmp" => self.pairwise(i, "nx::fmaxnm(a, b)", "f")?,
            "fminnmp" => self.pairwise(i, "nx::fminnm(a, b)", "f")?,
            "smaxp" => self.pairwise(i, "a > b ? a : b", "s")?,
            "sminp" => self.pairwise(i, "a < b ? a : b", "s")?,
            "umaxp" => self.pairwise(i, "a > b ? a : b", "u")?,
            "uminp" => self.pairwise(i, "a < b ? a : b", "u")?,
            "facge" | "facgt" => {
                let (bits, lanes) = self.vinfo(i.op(0)?)?;
                let (d, a, b) = self.vector_registers(i)?;
                let cmp = if i.mnemonic == "facge" { ">=" } else { ">" };
                format!("{{ nx::V128 S = c.v[{a}], T = c.v[{b}]; nx::V128 r = nx::vzero(); for (int k = 0; k < {lanes}; k++) r.u{bits}[k] = std::fabs(S.f{bits}[k]) {cmp} std::fabs(T.f{bits}[k]) ? uint{bits}_t(~0ull) : 0; c.v[{d}] = r; }}")
            }
            "sqrdmulh" => {
                let (bits, _) = self.vinfo(i.op(0)?)?;
                let expr = format!("(int64_t(a) * int64_t(b) == (int64_t(1) << {})) ? int{bits}_t(~0ull >> {}) : nx::sat<int{bits}_t>((int64_t(a) * int64_t(b) * 2 + (int64_t(1) << {})) >> {bits})", 2 * bits - 2, 65 - bits, bits - 1);
                self.v_bin(i, &expr, "s", false)?
            }
            "shsub" | "uhsub" => self.v_bin(
                i,
                "(int64_t(a) - int64_t(b)) >> 1",
                if i.mnemonic == "shsub" { "s" } else { "u" },
                false,
            )?,
            "srshl" => {
                let (bits, _) = self.vinfo(i.op(0)?)?;
                let sh = format!("int8_t(S1.u{bits}[k])");
                let expr = format!("{sh} >= 0 ? ({sh} >= {bits} ? 0 : int64_t(uint64_t(int64_t(a)) << {sh})) : (-{sh} >= {bits} ? 0 : (int64_t(a) >> -{sh}) + ((int64_t(a) >> (-{sh} - 1)) & 1))");
                self.v_bin(i, &expr, "s", false)?
            }
            "uabdl" | "uabdl2" | "sabdl" | "sabdl2" => self.widen(
                i,
                "{a} > {b} ? {a} - {b} : {b} - {a}",
                i.mnemonic.starts_with('s'),
                false,
            )?,
            "umlsl" | "umlsl2" | "smlsl" | "smlsl2" => {
                self.widen(i, "{o} - {a} * {b}", i.mnemonic.starts_with('s'), false)?
            }
            "sqabs" | "sqneg" => {
                let (bits, _) = self.vinfo(i.op(0)?)?;
                // 64-bit lanes saturate through a 128-bit intermediate: int64_t negation would overflow (UB) and
                // nx::sat<int64_t> is a no-op.
                let (wide, sat) = if bits == 64 {
                    ("__int128", "nx::sat128<int64_t>".to_owned())
                } else {
                    ("int64_t", format!("nx::sat<int{bits}_t>"))
                };
                let expr = if i.mnemonic == "sqabs" {
                    format!("a < 0 ? -{wide}(a) : {wide}(a)")
                } else {
                    format!("-{wide}(a)")
                };
                self.v_un(i, &format!("{sat}({expr})"), "s", "s", false)?
            }
            "sqshl" | "sqrshl" | "uqshl" => self.saturating_shift(i)?,
            "sli" | "sri" => {
                let (bits, _) = self.vinfo(i.op(0)?)?;
                let sh = i.immediate(2)? as u32;
                let (direction, keep) = if i.mnemonic == "sli" {
                    ("<<", width_mask(sh))
                } else if sh >= bits {
                    return Ok(Some(";".to_owned()));
                } else {
                    (">>", !(width_mask(bits) >> sh) & width_mask(bits))
                };
                self.v_shift_imm(
                    i,
                    &format!("(({{a}} {direction} {{n}}) | (uint64_t({{o}}) & {keep:#x}ull))"),
                    false,
                    true,
                )?
            }
            "pmull" | "pmull2" => {
                let (d, a, b) = self.vector_registers(i)?;
                let upper = i.mnemonic == "pmull2";
                if i.op(0)?.vas == Arm64Vas::ARM64_VAS_1Q {
                    let lane = u8::from(upper);
                    format!("c.v[{d}] = nx::clmul64(c.v[{a}].u64[{lane}], c.v[{b}].u64[{lane}]);")
                } else {
                    let base = if upper { 8 } else { 0 };
                    format!("{{ nx::V128 A_ = c.v[{a}], B_ = c.v[{b}]; nx::V128 r = nx::vzero(); for (int k = 0; k < 8; k++) {{ uint16_t x_ = A_.u8[{base} + k], p_ = 0; uint8_t y_ = B_.u8[{base} + k]; for (int j = 0; j < 8; j++) if (y_ >> j & 1) p_ ^= uint16_t(x_ << j); r.u16[k] = p_; }} c.v[{d}] = r; }}")
                }
            }
            "tbx" => self.table_extension(i)?,
            "aese" | "aesd" | "aesmc" | "aesimc" | "sha1su1" | "sha256su0" | "sha1su0"
            | "sha256h" | "sha256h2" | "sha256su1" | "sha1c" | "sha1p" | "sha1m" | "sha1h" => {
                self.crypto(i)?
            }
            _ => return Ok(None),
        };
        Ok(Some(statement))
    }

    fn extension_address(&self, i: &Instruction<'_>) -> Result<String> {
        let mem = i
            .operands
            .iter()
            .find_map(|operand| match operand.op_type {
                Arm64OperandType::Mem(mem) => Some(mem),
                _ => None,
            })
            .ok_or_else(|| unsupported("expected memory operand"))?;
        let base = self.reg_id(mem.base())?.index()?;
        Ok(format!("c.x[{base}]"))
    }

    pub(super) fn lse(&self, i: &Instruction<'_>) -> Result<Option<String>> {
        let Some(m) = LSE.captures(i.mnemonic) else {
            return Ok(None);
        };
        let base = &m[1];
        let first = self.reg_at(i, 0)?;
        let bits = match m.get(3).map(|size| size.as_str()) {
            Some("b") => 8,
            Some("h") => 16,
            _ => first.bits,
        };
        let t = format!("uint{bits}_t");
        let address = self.extension_address(i)?;
        let src = first.read()?;
        let statement = match base {
            "casp" => {
                let hi = self.reg_at(i, 1)?;
                let (second, target_lo, target_hi) =
                    (hi.read()?, self.read(i, 2)?, self.read(i, 3)?);
                if bits == 32 {
                    let lo_write = first.write("uint32_t(e_)")?;
                    let hi_write = hi.write("uint32_t(e_ >> 32)")?;
                    format!("{{ uint64_t e_ = uint64_t(uint32_t({src})) | uint64_t(uint32_t({second})) << 32; uint64_t n_ = uint64_t(uint32_t({target_lo})) | uint64_t(uint32_t({target_hi})) << 32; __atomic_compare_exchange_n(reinterpret_cast<uint64_t*>({address}), &e_, n_, false, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST); {lo_write} {hi_write} }}")
                } else {
                    let lo_write = first.write("uint64_t(e_)")?;
                    let hi_write = hi.write("uint64_t(e_ >> 64)")?;
                    format!("{{ unsigned __int128 e_ = (unsigned __int128){second} << 64 | {src}; unsigned __int128 n_ = (unsigned __int128){target_hi} << 64 | {target_lo}; __atomic_compare_exchange_n(reinterpret_cast<unsigned __int128*>({address}), &e_, n_, false, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST); {lo_write} {hi_write} }}")
                }
            }
            "cas" => {
                let dst = self.read(i, 1)?;
                let write = first.write("e_")?;
                format!("{{ {t} e_ = {t}({src}); __atomic_compare_exchange_n(reinterpret_cast<{t}*>({address}), &e_, {t}({dst}), false, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST); {write} }}")
            }
            "swp" => {
                let write = self.reg_at(i, 1)?.write("o_")?;
                format!("{{ {t} o_ = __atomic_exchange_n(reinterpret_cast<{t}*>({address}), {t}({src}), __ATOMIC_SEQ_CST); {write} }}")
            }
            base => {
                let compute = match &base[2..] {
                    "add" => "__atomic_fetch_add(p_, v_, __ATOMIC_SEQ_CST)",
                    "clr" => "__atomic_fetch_and(p_, T_(~v_), __ATOMIC_SEQ_CST)",
                    "eor" => "__atomic_fetch_xor(p_, v_, __ATOMIC_SEQ_CST)",
                    "set" => "__atomic_fetch_or(p_, v_, __ATOMIC_SEQ_CST)",
                    "smax" => "nx::amo_minmax<T_, true, true>(p_, v_)",
                    "smin" => "nx::amo_minmax<T_, true, false>(p_, v_)",
                    "umax" => "nx::amo_minmax<T_, false, true>(p_, v_)",
                    "umin" => "nx::amo_minmax<T_, false, false>(p_, v_)",
                    _ => return Err(unsupported("lse form")),
                };
                let write = if base.starts_with("ld") {
                    self.reg_at(i, 1)?.write("o_")?
                } else {
                    "(void)o_;".to_owned()
                };
                format!("{{ using T_ = {t}; T_* p_ = reinterpret_cast<T_*>({address}); T_ v_ = T_({src}); T_ o_ = {compute}; {write} }}")
            }
        };
        Ok(Some(statement))
    }

    fn vector_registers(&self, i: &Instruction<'_>) -> Result<(u8, u8, u8)> {
        Ok((
            self.reg_at(i, 0)?.index()?,
            self.reg_at(i, 1)?.index()?,
            self.reg_at(i, 2)?.index()?,
        ))
    }

    fn pairwise_long(&self, i: &Instruction<'_>) -> Result<String> {
        let (bits, lanes) = self.vinfo(i.op(0)?)?;
        let sbits = bits / 2;
        let signed = i.mnemonic == "sadalp";
        let kind = if signed { "s" } else { "u" };
        let ty = if signed { "int" } else { "uint" };
        let (d, a) = (self.reg_at(i, 0)?.index()?, self.reg_at(i, 1)?.index()?);
        let high = if bits * lanes == 64 {
            " r.u64[1] = 0;"
        } else {
            ""
        };
        Ok(format!("{{ nx::V128 S = c.v[{a}]; nx::V128 r = c.v[{d}]; for (int k = 0; k < {lanes}; k++) r.{kind}{bits}[k] = {ty}{bits}_t({ty}64_t(r.{kind}{bits}[k]) + {ty}64_t(S.{kind}{sbits}[2 * k]) + S.{kind}{sbits}[2 * k + 1]);{high} c.v[{d}] = r; }}"))
    }

    fn pairwise(&self, i: &Instruction<'_>, expression: &str, kind: &str) -> Result<String> {
        let (d, a) = (self.reg_at(i, 0)?.index()?, self.reg_at(i, 1)?.index()?);
        if i.op(0)?.vas == Arm64Vas::ARM64_VAS_INVALID {
            let (bits, _) = self.vinfo(i.op(1)?)?;
            let expr = substitute(
                expression,
                &[
                    ("a", format!("c.v[{a}].{kind}{bits}[0]")),
                    ("b", format!("c.v[{a}].{kind}{bits}[1]")),
                ],
            );
            return Ok(format!(
                "{{ nx::V128 r = nx::vzero(); r.{kind}{bits}[0] = {expr}; c.v[{d}] = r; }}"
            ));
        }
        let b = self.reg_at(i, 2)?.index()?;
        let (bits, lanes) = self.vinfo(i.op(0)?)?;
        let half = lanes / 2;
        let expr = |source| {
            substitute(
                expression,
                &[
                    ("a", format!("{source}_.{kind}{bits}[2 * k]")),
                    ("b", format!("{source}_.{kind}{bits}[2 * k + 1]")),
                ],
            )
        };
        Ok(format!("{{ nx::V128 A_ = c.v[{a}], B_ = c.v[{b}]; nx::V128 r = nx::vzero(); for (int k = 0; k < {half}; k++) {{ r.{kind}{bits}[k] = {}; r.{kind}{bits}[k + {half}] = {}; }} c.v[{d}] = r; }}", expr('A'), expr('B')))
    }

    fn reduce_extension(&self, i: &Instruction<'_>) -> Result<String> {
        let (d, a) = (self.reg_at(i, 0)?.index()?, self.reg_at(i, 1)?.index()?);
        let (bits, lanes) = self.vinfo(i.op(1)?)?;
        if i.mnemonic == "saddlv" {
            let wide = bits * 2;
            return Ok(format!("{{ int64_t s_ = 0; for (int k = 0; k < {lanes}; k++) s_ += c.v[{a}].s{bits}[k]; nx::V128 r = nx::vzero(); r.s{wide}[0] = int{wide}_t(s_); c.v[{d}] = r; }}"));
        }
        let (ty, kind) = if i.mnemonic == "sminv" {
            ("int", "s")
        } else {
            ("uint", "u")
        };
        let source = format!("c.v[{a}].{kind}{bits}");
        Ok(format!("{{ {ty}{bits}_t s_ = {source}[0]; for (int k = 1; k < {lanes}; k++) s_ = {source}[k] < s_ ? {source}[k] : s_; nx::V128 r = nx::vzero(); r.{kind}{bits}[0] = s_; c.v[{d}] = r; }}"))
    }

    fn saturating_shift(&self, i: &Instruction<'_>) -> Result<String> {
        let (bits, _) = self.vinfo(i.op(0)?)?;
        let signed = i.mnemonic != "uqshl";
        let rounding = i.mnemonic == "sqrshl";
        let max = format!("uint{bits}_t(~0ull)");
        if let Arm64OperandType::Imm(shift) = i.op(2)?.op_type {
            if rounding {
                return Err(unsupported("sqrshl immediate"));
            }
            let (kind, expr) = if signed {
                (
                    "s",
                    format!("nx::sat128<int{bits}_t>((__int128)int64_t(a) << {shift})"),
                )
            } else if shift == 0 {
                ("u", "a".to_owned())
            } else {
                ("u", format!("(uint64_t(a) >> ({bits} - {shift})) ? {max} : uint{bits}_t(uint64_t(a) << {shift})"))
            };
            return self.v_un(i, &expr, kind, kind, false);
        }
        let sh = format!("int(int8_t(S1.u{bits}[k]))");
        if signed {
            let left = format!(
                "nx::sat128<int{bits}_t>((__int128)int64_t(a) << ({sh} > {bits} ? {bits} : {sh}))"
            );
            let right = if rounding {
                format!("int{bits}_t(-{sh} > {bits} ? 0 : (((__int128)int64_t(a) + ((__int128)1 << (-{sh} - 1))) >> -{sh}))")
            } else {
                let last = bits - 1;
                format!("int{bits}_t(int64_t(a) >> (-{sh} > {last} ? {last} : -{sh}))")
            };
            self.v_bin(i, &format!("(({sh}) >= 0 ? {left} : {right})"), "s", false)
        } else {
            let expr = format!("{sh} >= 0 ? ({sh} >= {bits} ? (a ? {max} : 0) : ((unsigned __int128)a << {sh}) > {max} ? {max} : uint{bits}_t(uint64_t(a) << {sh})) : (-{sh} >= {bits} ? 0 : uint64_t(a) >> -{sh})");
            self.v_bin(i, &expr, "u", false)
        }
    }

    fn table_extension(&self, i: &Instruction<'_>) -> Result<String> {
        let (_, lanes) = self.vinfo(i.op(0)?)?;
        let d = self.reg_at(i, 0)?.index()?;
        let last = i
            .operands
            .len()
            .checked_sub(1)
            .ok_or_else(|| unsupported("missing table index"))?;
        let index = self.reg_at(i, last)?.index()?;
        let copied = i.operands[1..last]
            .iter()
            .enumerate()
            .map(|(slot, operand)| {
                let register = self.reg(operand)?.index()?;
                Ok(format!(
                    "std::memcpy(t_ + {}, &c.v[{register}], 16);",
                    16 * slot
                ))
            })
            .collect::<Result<Vec<_>>>()?
            .join(" ");
        let clear = if lanes == 16 { "" } else { " r.u64[1] = 0;" };
        Ok(format!("{{ uint8_t t_[64]; {copied} nx::V128 I_ = c.v[{index}]; nx::V128 r = c.v[{d}]; for (int k = 0; k < {lanes}; k++) if (I_.u8[k] < {}) r.u8[k] = t_[I_.u8[k]];{clear} c.v[{d}] = r; }}", 16 * (last - 1)))
    }

    fn crypto(&self, i: &Instruction<'_>) -> Result<String> {
        let (d, a) = (self.reg_at(i, 0)?.index()?, self.reg_at(i, 1)?.index()?);
        let name = i.mnemonic;
        let expression = match name {
            "sha1h" => return Ok(format!("{{ nx::V128 r = nx::vzero(); r.u32[0] = nx::sha1h(c.v[{a}].u32[0]); c.v[{d}] = r; }}")),
            "aesmc" | "aesimc" => format!("c.v[{a}]"),
            "aese" | "aesd" | "sha1su1" | "sha256su0" => format!("c.v[{d}], c.v[{a}]"),
            "sha1c" | "sha1p" | "sha1m" => format!("c.v[{d}], c.v[{a}].u32[0], c.v[{}]", self.reg_at(i, 2)?.index()?),
            _ => format!("c.v[{d}], c.v[{a}], c.v[{}]", self.reg_at(i, 2)?.index()?),
        };
        Ok(format!("c.v[{d}] = nx::{name}({expression});"))
    }
}
