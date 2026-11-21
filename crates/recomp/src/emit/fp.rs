//! Scalar floating-point operations.
use super::{neon::substitute, unsupported, Emitter, Instruction, Reg, Result};
use capstone::arch::arm64::{Arm64OperandType, Arm64Vas};

fn read(r: Reg) -> Result<String> {
    let n = r.index()?;
    match r.bits {
        16 => Ok(format!("nx::f16_to_f32(c.v[{n}].u16[0])")),
        32 | 64 => Ok(format!("c.v[{n}].f{}[0]", r.bits)),
        _ => Err(unsupported("fp read")),
    }
}
pub(super) fn write(r: Reg, expr: &str) -> Result<String> {
    let n = r.index()?;
    if r.bits == 16 {
        return Ok(format!("{{ uint16_t h_ = nx::f32_to_f16(float({expr})); nx::V128 t_ = nx::vzero(); t_.u16[0] = h_; c.v[{n}] = t_; }}"));
    }
    let ty = match r.bits {
        32 => "float",
        64 => "double",
        _ => return Err(unsupported("fp write")),
    };
    Ok(format!(
        "{{ {ty} r_ = {ty}({expr}); nx::V128 t_ = nx::vzero(); t_.f{}[0] = r_; c.v[{n}] = t_; }}",
        r.bits
    ))
}
impl Emitter<'_> {
    pub(super) fn fp(&self, i: &Instruction<'_>) -> Result<Option<String>> {
        let binary = match i.mnemonic {
            "fadd" => Some("a + b"),
            "fsub" => Some("a - b"),
            "fmul" => Some("a * b"),
            "fdiv" => Some("a / b"),
            "fnmul" => Some("-(a * b)"),
            "fabd" => Some("std::fabs(a - b)"),
            "fmaxnm" => Some("nx::fmaxnm(a, b)"),
            "fminnm" => Some("nx::fminnm(a, b)"),
            "frsqrts" => Some("nx::frsqrts(a, b)"),
            "frecps" => Some("nx::frecps(a, b)"),
            "fmax" => Some("nx::fmax_(a, b)"),
            "fmin" => Some("nx::fmin_(a, b)"),
            _ => None,
        };
        let unary = match i.mnemonic {
            "frecpe" => Some("1.0f / a"),
            "frsqrte" => Some("1.0f / std::sqrt(a)"),
            "fneg" => Some("-a"),
            "fabs" => Some("std::fabs(a)"),
            "fsqrt" => Some("std::sqrt(a)"),
            "frintm" => Some("std::floor(a)"),
            "frintp" => Some("std::ceil(a)"),
            "frintz" => Some("std::trunc(a)"),
            "frinta" => Some("nx::round_away(a)"),
            "frintn" | "frintx" | "frinti" => Some("nx::round_even(a)"),
            _ => None,
        };
        if binary.is_some() || unary.is_some() {
            if i.op(0)?.vas != Arm64Vas::ARM64_VAS_INVALID {
                return Ok(None);
            }
            let d = self.reg_at(i, 0)?;
            let a = format!("({})", read(self.reg_at(i, 1)?)?);
            let expr = if let Some(binary) = binary {
                let op = i.op(2)?;
                let m = self.reg_at(i, 2)?;
                let b = if let Some(lane) = op.vector_index {
                    format!("c.v[{}].f{}[{lane}]", m.index()?, d.bits)
                } else {
                    read(m)?
                };
                substitute(binary, &[("a", a), ("b", format!("({b})"))])
            } else {
                substitute(unary.expect("unary FP operation"), &[("a", a)])
            };
            return Ok(Some(write(d, &expr)?));
        }
        if matches!(
            i.mnemonic,
            "fcvtzs"
                | "fcvtzu"
                | "fcvtms"
                | "fcvtmu"
                | "fcvtps"
                | "fcvtpu"
                | "fcvtas"
                | "fcvtau"
                | "fcvtns"
                | "fcvtnu"
        ) {
            if i.op(0)?.vas != Arm64Vas::ARM64_VAS_INVALID {
                return Ok(None);
            }
            let d = self.reg_at(i, 0)?;
            let mut v = read(self.reg_at(i, 1)?)?;
            if i.operands.len() > 2 {
                v = format!("({v}) * {}", fixed_point_scale(i.immediate(2)?));
            }
            let round = match i.mnemonic.as_bytes()[4] {
                b'z' => "std::trunc",
                b'm' => "std::floor",
                b'p' => "std::ceil",
                b'a' => "nx::round_away",
                _ => "nx::round_even",
            };
            let signed = i.mnemonic.ends_with('s');
            let t = if signed { "int" } else { "uint" };
            let bits = d.bits;
            let expr = format!("uint{bits}_t(nx::fcvt_sat<{t}{bits}_t>({round}({v})))");
            if d.gp {
                return Ok(Some(d.write(expr)?));
            }
            return Ok(Some(format!(
                "{{ nx::V128 t_ = nx::vzero(); t_.u{bits}[0] = {expr}; c.v[{}] = t_; }}",
                d.index()?
            )));
        }
        if matches!(i.mnemonic, "scvtf" | "ucvtf") {
            if i.op(0)?.vas != Arm64Vas::ARM64_VAS_INVALID {
                return Ok(None);
            }
            let d = self.reg_at(i, 0)?;
            let n = self.reg_at(i, 1)?;
            let signed = i.mnemonic == "scvtf";
            let kind = if signed { "s" } else { "u" };
            let v = if n.gp {
                format!(
                    "{}{}_t({})",
                    if signed { "int" } else { "uint" },
                    n.bits,
                    n.read()?
                )
            } else {
                format!("c.v[{}].{kind}{}[0]", n.index()?, n.bits)
            };
            let ty = if d.bits == 64 { "double" } else { "float" };
            let mut v = format!("{ty}({v})");
            if i.operands.len() > 2 {
                v = format!("{v} / {ty}({})", fixed_point_scale(i.immediate(2)?));
            }
            return Ok(Some(write(d, &v)?));
        }
        let result = match i.mnemonic {
            "fmla" | "fmls" => {
                let d = self.reg_at(i, 0)?;
                let n = read(self.reg_at(i, 1)?)?;
                let m = self.reg_at(i, 2)?;
                let lane = i
                    .op(2)?
                    .vector_index
                    .ok_or_else(|| unsupported("missing lane"))?;
                let bits = if i.op(2)?.vas == Arm64Vas::ARM64_VAS_INVALID {
                    m.bits
                } else {
                    self.vinfo(i.op(2)?)?.0
                };
                let m = format!("c.v[{}].f{bits}[{lane}]", m.index()?);
                let n = if i.mnemonic == "fmls" {
                    format!("-({n})")
                } else {
                    n
                };
                write(d, &format!("std::fma({n}, {m}, {})", read(d)?))?
            }
            "fmadd" | "fmsub" | "fnmadd" | "fnmsub" => {
                let d = self.reg_at(i, 0)?;
                let n = read(self.reg_at(i, 1)?)?;
                let m = read(self.reg_at(i, 2)?)?;
                let a = read(self.reg_at(i, 3)?)?;
                let n = if matches!(i.mnemonic, "fmsub" | "fnmadd") {
                    format!("-({n})")
                } else {
                    n
                };
                let a = if matches!(i.mnemonic, "fnmadd" | "fnmsub") {
                    format!("-({a})")
                } else {
                    a
                };
                write(d, &format!("std::fma({n}, {m}, {a})"))?
            }
            "fcmp" | "fcmpe" => {
                let n = self.reg_at(i, 0)?;
                let ty = if n.bits == 64 { "double" } else { "float" };
                let b = match i.op(1)?.op_type {
                    Arm64OperandType::Fp(_) | Arm64OperandType::Imm(_) => "0.0".to_owned(),
                    _ => read(self.reg_at(i, 1)?)?,
                };
                format!("nx::fcmp<{ty}>(c, {}, {ty}({b}));", read(n)?)
            }
            "fccmp" | "fccmpe" => {
                let n = self.reg_at(i, 0)?;
                let ty = if n.bits == 64 { "double" } else { "float" };
                format!(
                    "if ({}) nx::fcmp<{ty}>(c, {}, {}); else nx::set_nzcv(c, {});",
                    i.cond()?,
                    read(n)?,
                    read(self.reg_at(i, 1)?)?,
                    i.immediate(2)?
                )
            }
            "fcsel" => write(
                self.reg_at(i, 0)?,
                &format!(
                    "({}) ? {} : {}",
                    i.cond()?,
                    read(self.reg_at(i, 1)?)?,
                    read(self.reg_at(i, 2)?)?
                ),
            )?,
            "fcvt" => write(self.reg_at(i, 0)?, &read(self.reg_at(i, 1)?)?)?,
            "fmov" => {
                let d = self.reg_at(i, 0)?;
                let op = i.op(1)?;
                match op.op_type {
                    Arm64OperandType::Fp(v) => {
                        if i.op(0)?.vas != Arm64Vas::ARM64_VAS_INVALID {
                            let (bits, lanes) = self.vinfo(i.op(0)?)?;
                            let field = if bits == 32 { "f32" } else { "f64" };
                            format!("{{ nx::V128 r = nx::vzero(); for (int k = 0; k < {lanes}; k++) r.{field}[k] = {v:?}; c.v[{}] = r; }}",d.index()?)
                        } else {
                            write(d, &format!("{v:?}"))?
                        }
                    }
                    Arm64OperandType::Imm(_) => write(d, "0.0")?,
                    Arm64OperandType::Reg(_) => {
                        let n = self.reg_at(i, 1)?;
                        if let Some(lane) = i.op(0)?.vector_index {
                            format!("c.v[{}].u64[{lane}] = {};", d.index()?, n.read()?)
                        } else if let Some(lane) = op.vector_index {
                            d.write(format!("c.v[{}].u64[{lane}]", n.index()?))?
                        } else if d.gp {
                            d.write(format!("c.v[{}].u{}[0]", n.index()?, d.bits))?
                        } else if n.gp {
                            format!("{{ nx::V128 t_ = nx::vzero(); t_.u{}[0] = uint{}_t({}); c.v[{}] = t_; }}", d.bits, d.bits, n.read()?, d.index()?)
                        } else {
                            format!("{{ nx::V128 t_ = nx::vzero(); t_.u{}[0] = c.v[{}].u{}[0]; c.v[{}] = t_; }}", d.bits, n.index()?, d.bits, d.index()?)
                        }
                    }
                    _ => return Err(unsupported("fmov form")),
                }
            }
            _ => return Ok(None),
        };
        Ok(Some(result))
    }
}
/// Format a fixed-point scale like Python's float repr on the AArch64 range of 1–64.
pub(super) fn fixed_point_scale(bits: i64) -> String {
    let value = 2f64.powi(bits as i32);
    if value >= 1e16 {
        format!("{value:e}").replace('e', "e+")
    } else {
        format!("{value:.1}")
    }
}
