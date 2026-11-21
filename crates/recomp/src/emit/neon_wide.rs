//! AdvSIMD widening, narrowing, permutation, and reduction operations.
use super::{unsupported, Emitter, Instruction, Result};
use capstone::arch::arm64::{Arm64OperandType, Arm64Vas};
impl Emitter<'_> {
    pub(super) fn widen(
        &self,
        i: &Instruction<'_>,
        expr: &str,
        signed: bool,
        second_wide: bool,
    ) -> Result<String> {
        let (dbits, n) = self.vinfo(i.op(0)?)?;
        let sbits = dbits / 2;
        let hi = i.mnemonic.ends_with('2');
        let kind = if signed { "s" } else { "u" };
        let ty = if signed { "int" } else { "uint" };
        let d = self.reg_at(i, 0)?.index()?;
        let count = if i
            .operands
            .get(2)
            .is_some_and(|o| matches!(o.op_type, Arm64OperandType::Reg(_)))
        {
            2
        } else {
            1
        };
        let mut pre = Vec::new();
        let mut src = Vec::new();
        for k in 0..count {
            let op = i.op(k + 1)?;
            let r = self.reg(op)?.index()?;
            pre.push(format!("nx::V128 S{k} = c.v[{r}];"));
            let (bits, idx) = if let Some(lane) = op.vector_index {
                (sbits, lane.to_string())
            } else if second_wide && k == 0 {
                (dbits, "k".to_owned())
            } else {
                (
                    sbits,
                    if hi {
                        format!("k + {n}")
                    } else {
                        "k".to_owned()
                    },
                )
            };
            src.push(format!("{ty}64_t(S{k}.{kind}{bits}[{idx}])"));
        }
        let imm = i
            .operands
            .get(2)
            .and_then(|o| {
                if let Arm64OperandType::Imm(v) = o.op_type {
                    Some(v)
                } else {
                    None
                }
            })
            .unwrap_or(0);
        let e = expr
            .replace("{a}", &src[0])
            .replace("{b}", src.get(1).map_or("0", String::as_str))
            .replace("{n}", &imm.to_string())
            .replace("{o}", &format!("O.{kind}{dbits}[k]"));
        Ok(format!("{{ {} nx::V128 O = c.v[{d}]; nx::V128 r = nx::vzero(); for (int k = 0; k < {n}; k++) r.{kind}{dbits}[k] = {ty}{dbits}_t({e}); c.v[{d}] = r; }}",pre.join(" ")))
    }
    pub(super) fn narrow(
        &self,
        i: &Instruction<'_>,
        expr: &str,
        src_kind: &str,
        dst_kind: &str,
    ) -> Result<String> {
        let (dbits, n) = self.vinfo(i.op(0)?)?;
        let hi = i.mnemonic.ends_with('2');
        let sbits = dbits * 2;
        let d = self.reg_at(i, 0)?.index()?;
        let a = self.reg_at(i, 1)?.index()?;
        let cnt = if hi { n / 2 } else { n };
        let base = if hi { n / 2 } else { 0 };
        let imm = i
            .operands
            .get(2)
            .and_then(|o| {
                if let Arm64OperandType::Imm(v) = o.op_type {
                    Some(v)
                } else {
                    None
                }
            })
            .unwrap_or(0);
        let sty = if src_kind == "s" {
            "int64_t"
        } else {
            "uint64_t"
        };
        let ty = if dst_kind == "s" { "int" } else { "uint" };
        let init = if hi {
            format!("c.v[{d}]")
        } else {
            "nx::vzero()".to_owned()
        };
        let e = expr
            .replace("{a}", &format!("{sty}(S.{src_kind}{sbits}[k])"))
            .replace("{n}", &imm.to_string())
            .replace("{dbits}", &dbits.to_string());
        Ok(format!("{{ nx::V128 S = c.v[{a}]; nx::V128 r = {init}; for (int k = 0; k < {cnt}; k++) r.{dst_kind}{dbits}[{base} + k] = {ty}{dbits}_t({e}); c.v[{d}] = r; }}"))
    }
    pub(super) fn neon_wide(&self, i: &Instruction<'_>) -> Result<Option<String>> {
        let base = i.mnemonic.strip_suffix('2').unwrap_or(i.mnemonic);
        let wide = match base {
            "ushll" | "sshll" => Some(("{a} << {n}", false)),
            "uxtl" | "sxtl" => Some(("{a}", false)),
            "uaddw" | "saddw" => Some(("{a} + {b}", true)),
            "usubw" | "ssubw" => Some(("{a} - {b}", true)),
            "uaddl" | "saddl" => Some(("{a} + {b}", false)),
            "usubl" | "ssubl" => Some(("{a} - {b}", false)),
            "umull" | "smull" => Some(("{a} * {b}", false)),
            "umlal" | "smlal" => Some(("{o} + {a} * {b}", false)),
            "umlsl" | "smlsl" => Some(("{o} - {a} * {b}", false)),
            "uabdl" | "sabdl" => Some(("{a} > {b} ? {a} - {b} : {b} - {a}", false)),
            _ => None,
        };
        if let Some((expr, second)) = wide {
            if self.reg_at(i, 0)?.gp {
                return Ok(None);
            }
            return self.widen(i, expr, base.starts_with('s'), second).map(Some);
        }
        if base == "shll" {
            let bits = self.vinfo(i.op(0)?)?.0 / 2;
            return self
                .widen(i, &format!("{{a}} << {bits}"), false, false)
                .map(Some);
        }
        let narrow = match base {
            "xtn" => Some(("{a}", "u", "u")),
            "shrn" => Some(("{a} >> {n}", "u", "u")),
            "rshrn" => Some(("(({a} >> {n}) + (({a} >> ({n} - 1)) & 1))", "u", "u")),
            "sqshrn" => Some(("nx::sat<int{dbits}_t>({a} >> {n})", "s", "s")),
            "sqrshrn" => Some((
                "nx::sat<int{dbits}_t>(({a} + (int64_t(1) << ({n} - 1))) >> {n})",
                "s",
                "s",
            )),
            "sqxtun" => Some(("nx::sat<uint{dbits}_t>({a})", "s", "u")),
            "sqxtn" => Some(("nx::sat<int{dbits}_t>({a})", "s", "s")),
            "uqxtn" => Some((
                "{a} > uint{dbits}_t(~0ull) ? uint{dbits}_t(~0ull) : {a}",
                "u",
                "u",
            )),
            "sqshrun" => Some(("nx::sat<uint{dbits}_t>({a} >> {n})", "s", "u")),
            "sqrshrun" => Some((
                "nx::sat<uint{dbits}_t>(({a} + (int64_t(1) << ({n} - 1))) >> {n})",
                "s",
                "u",
            )),
            _ => None,
        };
        if let Some((expr, src, dst)) = narrow {
            return self.narrow(i, expr, src, dst).map(Some);
        }
        if !matches!(
            i.mnemonic,
            "saddlp"
                | "uaddlp"
                | "fcvtn"
                | "fcvtn2"
                | "fcvtl"
                | "fcvtl2"
                | "ext"
                | "zip1"
                | "zip2"
                | "uzp1"
                | "uzp2"
                | "trn1"
                | "trn2"
                | "rev16"
                | "rev32"
                | "rev64"
                | "tbl"
                | "addv"
                | "uaddlv"
                | "umaxv"
                | "smaxv"
                | "fmaxnmv"
                | "fminnmv"
                | "addp"
                | "faddp"
        ) {
            return Ok(None);
        }
        let d = self.reg_at(i, 0)?.index()?;
        let a = self.reg_at(i, 1)?.index()?;
        let stmt = match i.mnemonic {
            "addp" | "faddp" => {
                let fp = i.mnemonic == "faddp";
                let kind = if fp { "f" } else { "u" };
                if i.op(0)?.vas == Arm64Vas::ARM64_VAS_INVALID {
                    if fp {
                        let bits = self.vinfo(i.op(1)?)?.0;
                        let ty = if bits == 32 { "float" } else { "double" };
                        format!("{{ {ty} r_ = {ty}(c.v[{a}].f{bits}[0] + c.v[{a}].f{bits}[1]); nx::V128 t_ = nx::vzero(); t_.f{bits}[0] = r_; c.v[{d}] = t_; }}")
                    } else {
                        format!("{{ nx::V128 r = nx::vzero(); r.u64[0] = c.v[{a}].u64[0] + c.v[{a}].u64[1]; c.v[{d}] = r; }}")
                    }
                } else {
                    let (bits, n) = self.vinfo(i.op(0)?)?;
                    let b = self.reg_at(i, 2)?.index()?;
                    let h = n / 2;
                    format!("{{ nx::V128 A_ = c.v[{a}], B_ = c.v[{b}]; nx::V128 r = nx::vzero(); for (int k = 0; k < {h}; k++) {{ r.{kind}{bits}[k] = A_.{kind}{bits}[2 * k] + A_.{kind}{bits}[2 * k + 1]; r.{kind}{bits}[k + {h}] = B_.{kind}{bits}[2 * k] + B_.{kind}{bits}[2 * k + 1]; }} c.v[{d}] = r; }}")
                }
            }
            "saddlp" | "uaddlp" => {
                let (dbits, n) = self.vinfo(i.op(0)?)?;
                let sbits = dbits / 2;
                let signed = i.mnemonic.starts_with('s');
                let kind = if signed { "s" } else { "u" };
                let ty = if signed { "int" } else { "uint" };
                format!("{{ nx::V128 S = c.v[{a}]; nx::V128 r = nx::vzero(); for (int k = 0; k < {n}; k++) r.{kind}{dbits}[k] = {ty}{dbits}_t({ty}64_t(S.{kind}{sbits}[2 * k]) + S.{kind}{sbits}[2 * k + 1]); c.v[{d}] = r; }}")
            }
            "fcvtn" | "fcvtn2" => {
                let (bits, n) = self.vinfo(i.op(0)?)?;
                let hi = i.mnemonic.ends_with('2');
                let cnt = if hi { n / 2 } else { n };
                let base = if hi { n / 2 } else { 0 };
                let init = if hi {
                    format!("c.v[{d}]")
                } else {
                    "nx::vzero()".to_owned()
                };
                let body = if bits == 32 {
                    format!("r.f32[{base} + k] = float(S.f64[k]);")
                } else {
                    format!("r.u16[{base} + k] = nx::f32_to_f16(S.f32[k]);")
                };
                format!("{{ nx::V128 S = c.v[{a}]; nx::V128 r = {init}; for (int k = 0; k < {cnt}; k++) {body} c.v[{d}] = r; }}")
            }
            "fcvtl" | "fcvtl2" => {
                let (bits, n) = self.vinfo(i.op(0)?)?;
                let off = if i.mnemonic.ends_with('2') {
                    format!("k + {n}")
                } else {
                    "k".to_owned()
                };
                let body = if bits == 64 {
                    format!("r.f64[k] = double(S.f32[{off}]);")
                } else {
                    format!("r.f32[k] = nx::f16_to_f32(S.u16[{off}]);")
                };
                format!("{{ nx::V128 S = c.v[{a}]; nx::V128 r = nx::vzero(); for (int k = 0; k < {n}; k++) {body} c.v[{d}] = r; }}")
            }
            "ext" => {
                let (bits, n) = self.vinfo(i.op(0)?)?;
                let count = bits * n / 8;
                let b = self.reg_at(i, 2)?.index()?;
                let imm = i.immediate(3)?;
                format!("{{ uint8_t t_[32]; std::memcpy(t_, &c.v[{a}], {count}); std::memcpy(t_ + {count}, &c.v[{b}], {count}); nx::V128 r = nx::vzero(); std::memcpy(&r, t_ + {imm}, {count}); c.v[{d}] = r; }}")
            }
            "zip1" | "zip2" | "uzp1" | "uzp2" | "trn1" | "trn2" => {
                let (bits, n) = self.vinfo(i.op(0)?)?;
                let h = n / 2;
                let b = self.reg_at(i, 2)?.index()?;
                let t = format!("u{bits}");
                let body = match i.mnemonic {
                    "zip1" => format!("r.{t}[2 * k] = A_.{t}[k]; r.{t}[2 * k + 1] = B_.{t}[k];"),
                    "zip2" => format!(
                        "r.{t}[2 * k] = A_.{t}[k + {h}]; r.{t}[2 * k + 1] = B_.{t}[k + {h}];"
                    ),
                    "uzp1" => format!("r.{t}[k] = A_.{t}[2 * k]; r.{t}[k + {h}] = B_.{t}[2 * k];"),
                    "uzp2" => {
                        format!("r.{t}[k] = A_.{t}[2 * k + 1]; r.{t}[k + {h}] = B_.{t}[2 * k + 1];")
                    }
                    "trn1" => {
                        format!("r.{t}[2 * k] = A_.{t}[2 * k]; r.{t}[2 * k + 1] = B_.{t}[2 * k];")
                    }
                    _ => format!(
                        "r.{t}[2 * k] = A_.{t}[2 * k + 1]; r.{t}[2 * k + 1] = B_.{t}[2 * k + 1];"
                    ),
                };
                format!("{{ nx::V128 A_ = c.v[{a}], B_ = c.v[{b}]; nx::V128 r = nx::vzero(); for (int k = 0; k < {h}; k++) {{ {body} }} c.v[{d}] = r; }}")
            }
            "rev16" | "rev32" | "rev64" => {
                if i.op(0)?.vas == Arm64Vas::ARM64_VAS_INVALID {
                    return Ok(None);
                }
                let (bits, n) = self.vinfo(i.op(0)?)?;
                let container = i.mnemonic[3..]
                    .parse::<u32>()
                    .map_err(|_| unsupported("reverse"))?;
                let per = container / bits;
                format!("{{ nx::V128 S = c.v[{a}]; nx::V128 r = nx::vzero(); for (int k = 0; k < {n}; k++) r.u{bits}[k] = S.u{bits}[(k / {per}) * {per} + ({per} - 1 - k % {per})]; c.v[{d}] = r; }}")
            }
            "tbl" => {
                let (_, n) = self.vinfo(i.op(0)?)?;
                let tables = &i.operands[1..i.operands.len() - 1];
                let idx = self.reg_at(i, i.operands.len() - 1)?.index()?;
                let cp = tables
                    .iter()
                    .enumerate()
                    .map(|(k, o)| {
                        Ok(format!(
                            "std::memcpy(t_ + {}, &c.v[{}], 16);",
                            16 * k,
                            self.reg(o)?.index()?
                        ))
                    })
                    .collect::<Result<Vec<_>>>()?
                    .join(" ");
                format!("{{ uint8_t t_[64]; {cp} nx::V128 I_ = c.v[{idx}]; nx::V128 r = nx::vzero(); for (int k = 0; k < {n}; k++) r.u8[k] = I_.u8[k] < {} ? t_[I_.u8[k]] : 0; c.v[{d}] = r; }}",16*tables.len())
            }
            "addv" | "uaddlv" => {
                let (bits, n) = self.vinfo(i.op(1)?)?;
                let out = if i.mnemonic == "uaddlv" {
                    bits * 2
                } else {
                    bits
                };
                format!("{{ uint64_t s_ = 0; for (int k = 0; k < {n}; k++) s_ += c.v[{a}].u{bits}[k]; nx::V128 r = nx::vzero(); r.u{out}[0] = uint{out}_t(s_); c.v[{d}] = r; }}")
            }
            "umaxv" => {
                let (bits, n) = self.vinfo(i.op(1)?)?;
                format!("{{ uint{bits}_t s_ = 0; for (int k = 0; k < {n}; k++) s_ = c.v[{a}].u{bits}[k] > s_ ? c.v[{a}].u{bits}[k] : s_; nx::V128 r = nx::vzero(); r.u{bits}[0] = s_; c.v[{d}] = r; }}")
            }
            "smaxv" => {
                let (bits, n) = self.vinfo(i.op(1)?)?;
                format!("{{ int{bits}_t s_ = c.v[{a}].s{bits}[0]; for (int k = 1; k < {n}; k++) s_ = c.v[{a}].s{bits}[k] > s_ ? c.v[{a}].s{bits}[k] : s_; nx::V128 r = nx::vzero(); r.s{bits}[0] = s_; c.v[{d}] = r; }}")
            }
            "fmaxnmv" | "fminnmv" => {
                let op = if i.mnemonic == "fmaxnmv" {
                    "fmaxnm"
                } else {
                    "fminnm"
                };
                format!("{{ nx::V128 S = c.v[{a}]; float s_ = nx::{op}(nx::{op}(S.f32[0], S.f32[1]), nx::{op}(S.f32[2], S.f32[3])); nx::V128 r = nx::vzero(); r.f32[0] = s_; c.v[{d}] = r; }}")
            }
            _ => return Ok(None),
        };
        Ok(Some(stmt))
    }
}
