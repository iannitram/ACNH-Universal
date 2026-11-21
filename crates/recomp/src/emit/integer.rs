//! Integer data processing. Format the operations exactly as recomp/emit_int.py.

use super::{hex_signed, unsupported, width_mask, Emitter, Instruction, Reg, Result, B};
use capstone::arch::arm64::{Arm64OperandType, Arm64Shift};

impl Emitter<'_> {
    pub(super) fn integer(&self, i: &Instruction<'_>) -> Result<Option<String>> {
        let statement = match i.mnemonic {
            "mov" | "movz" | "movn" | "movk" => self.move_value(i)?,
            "adrp" | "adr" => self
                .reg_at(i, 0)?
                .write(format!("{B} + {}ull", hex_signed(i.immediate(1)?)))?,
            "add" | "sub" | "adds" | "subs" | "cmp" | "cmn" | "neg" | "negs" => {
                self.arithmetic(i)?
            }
            "adc" | "adcs" | "sbc" | "sbcs" | "ngc" | "ngcs" => self.carry(i)?,
            "and" | "ands" | "orr" | "eor" | "bic" | "bics" | "orn" | "eon" | "tst" | "mvn" => {
                self.logic(i)?
            }
            "lsl" | "lsr" | "asr" | "ror" | "extr" => self.shift(i)?,
            "ubfx" | "sbfx" | "ubfiz" | "sbfiz" | "bfi" | "bfxil" => self.bitfield(i)?,
            "ubfm" => return Err(unsupported("raw ubfm")),
            "sxtb" | "sxth" | "sxtw" | "uxtb" | "uxth" => self.extend(i)?,
            "csel" | "csinc" | "csinv" | "csneg" | "cset" | "csetm" | "cinc" | "cinv" | "cneg"
            | "ccmp" | "ccmn" => self.conditional(i)?,
            "madd" | "msub" | "mul" | "mneg" | "smull" | "umull" | "smaddl" | "umaddl"
            | "smsubl" | "umsubl" | "smulh" | "umulh" | "udiv" | "sdiv" => {
                self.multiply_divide(i)?
            }
            "clz" | "cls" | "rbit" | "rev" | "rev16" | "rev32" => self.bits(i)?,
            _ => return Ok(None),
        };
        Ok(Some(statement))
    }

    fn move_value(&self, i: &Instruction<'_>) -> Result<String> {
        let d = self.reg_at(i, 0)?;
        let operand = i.op(1)?;
        let mask = width_mask(d.bits.min(64));
        let value = match operand.op_type {
            Arm64OperandType::Imm(value) => value as u64,
            _ if i.mnemonic == "mov" => return d.write(self.read(i, 1)?),
            _ => return Err(unsupported("expected immediate")),
        };
        let shift = match operand.shift {
            Arm64Shift::Lsl(n) => n,
            _ => 0,
        };
        let expr = match i.mnemonic {
            "mov" => format!("{:#x}ull", value & mask),
            "movz" => format!("{:#x}ull", value.wrapping_shl(shift) & mask),
            "movn" => format!("{:#x}ull", !value.wrapping_shl(shift) & mask),
            "movk" => {
                let keep = !(0xffffu64 << shift) & mask;
                let put = (value & 0xffff) << shift;
                format!("({} & {keep:#x}ull) | {put:#x}ull", d.read()?)
            }
            _ => unreachable!(),
        };
        d.write(expr)
    }

    fn arithmetic(&self, i: &Instruction<'_>) -> Result<String> {
        let d = self.reg_at(i, 0)?;
        let bits = d.bits;
        let plus = matches!(i.mnemonic, "add" | "adds" | "cmn");
        let op = if plus { '+' } else { '-' };
        let helper = if plus { "adds" } else { "subs" };
        let carry = if plus { ", 0" } else { "" };
        let result = match i.mnemonic {
            "cmp" | "cmn" => {
                let n = d.read()?;
                let v = self.read_shifted(i.op(1)?, bits)?;
                return Ok(format!("nx::{helper}{bits}(c, {n}, {v}{carry});"));
            }
            "neg" | "negs" => {
                let v = self.read_shifted(i.op(1)?, bits)?;
                if i.mnemonic == "neg" {
                    format!("uint{bits}_t(0) - {v}")
                } else {
                    format!("nx::subs{bits}(c, 0, {v})")
                }
            }
            _ => {
                let a = self.read(i, 1)?;
                let b = self.read_shifted(i.op(2)?, bits)?;
                if matches!(i.mnemonic, "adds" | "subs") {
                    format!("nx::{helper}{bits}(c, {a}, {b}{carry})")
                } else {
                    format!("uint{bits}_t({a}) {op} {b}")
                }
            }
        };
        d.write(result)
    }

    fn carry(&self, i: &Instruction<'_>) -> Result<String> {
        let d = self.reg_at(i, 0)?;
        let bits = d.bits;
        let n = self.read(i, 1)?;
        let result = match i.mnemonic {
            "ngc" => format!("uint{bits}_t(uint{bits}_t(~{n}) + c.cf)"),
            "ngcs" => format!("nx::adds{bits}(c, 0, uint{bits}_t(~{n}), c.cf)"),
            mnemonic => {
                let m = self.read(i, 2)?;
                let m = if mnemonic.starts_with("sbc") {
                    format!("uint{bits}_t(~{m})")
                } else {
                    m
                };
                if mnemonic.ends_with('s') {
                    format!("nx::adds{bits}(c, {n}, {m}, c.cf)")
                } else {
                    format!("uint{bits}_t({n} + {m} + c.cf)")
                }
            }
        };
        d.write(result)
    }

    fn logic(&self, i: &Instruction<'_>) -> Result<String> {
        let d = self.reg_at(i, 0)?;
        let bits = d.bits;
        if !d.gp {
            return Err(if matches!(i.mnemonic, "ands" | "bics" | "eon") {
                unsupported("vector logic here")
            } else {
                unsupported("no handler")
            });
        }
        if i.mnemonic == "mvn" {
            let value = self.read_shifted(i.op(1)?, bits)?;
            return d.write(format!("uint{bits}_t(~{value})"));
        }
        if i.mnemonic == "tst" {
            let n = d.read()?;
            let v = self.read_shifted(i.op(1)?, bits)?;
            return Ok(format!("nx::logic{bits}(c, uint{bits}_t({n} & {v}));"));
        }
        let op = match i.mnemonic {
            "orr" | "orn" => '|',
            "eor" | "eon" => '^',
            _ => '&',
        };
        let a = self.read(i, 1)?;
        let b = self.read_shifted(i.op(2)?, bits)?;
        let b = if matches!(i.mnemonic, "bic" | "bics" | "orn" | "eon") {
            format!("uint{bits}_t(~{b})")
        } else {
            b
        };
        let value = format!("uint{bits}_t({a} {op} {b})");
        if matches!(i.mnemonic, "ands" | "bics") {
            Ok(format!(
                "{{ uint{bits}_t r = {value}; nx::logic{bits}(c, r); {} }}",
                d.write("r")?
            ))
        } else {
            d.write(value)
        }
    }

    fn shift(&self, i: &Instruction<'_>) -> Result<String> {
        let d = self.reg_at(i, 0)?;
        let bits = d.bits;
        let a = self.read(i, 1)?;
        if i.mnemonic == "extr" {
            let lo = self.read(i, 2)?;
            let n = i.immediate(3)? as u32;
            if n == 0 {
                return d.write(lo);
            }
            return d.write(format!(
                "(uint{bits}_t({lo}) >> {n}) | (uint{bits}_t({a}) << {})",
                bits - n
            ));
        }
        let n = match i.op(2)?.op_type {
            Arm64OperandType::Imm(n) => n.to_string(),
            _ => format!("({} & {})", self.read(i, 2)?, bits - 1),
        };
        d.write(match i.mnemonic {
            "lsl" => format!("uint{bits}_t({a}) << {n}"),
            "lsr" => format!("uint{bits}_t({a}) >> {n}"),
            kind => format!("nx::{kind}{bits}({a}, unsigned({n}))"),
        })
    }

    fn bitfield(&self, i: &Instruction<'_>) -> Result<String> {
        let d = self.reg_at(i, 0)?;
        let bits = d.bits;
        let value = self.read(i, 1)?;
        let offset = i.immediate(2)? as u32;
        let width = i.immediate(3)? as u32;
        let (lsb, pos) = match i.mnemonic {
            "ubfiz" | "sbfiz" | "bfi" => (0, offset),
            _ => (offset, 0),
        };
        let shifted = bits
            .checked_sub(width)
            .and_then(|left| left.checked_sub(offset))
            .ok_or_else(|| unsupported("bitfield width"))?;
        if i.mnemonic == "sbfiz" {
            return d.write(format!(
                "uint{bits}_t(int{bits}_t(uint{bits}_t({value}) << {}) >> {shifted})",
                bits - width
            ));
        }
        let field_mask = width_mask(width);
        let mut result = if i.mnemonic == "sbfx" {
            format!(
                "uint{bits}_t(int{bits}_t(uint{bits}_t({value}) << {shifted}) >> {})",
                bits - width
            )
        } else {
            format!("((uint{bits}_t({value}) >> {lsb}) & {field_mask:#x}ull)")
        };
        if pos != 0 {
            result = format!("uint{bits}_t({result} << {pos})");
        }
        if matches!(i.mnemonic, "bfi" | "bfxil") {
            let put = (field_mask << pos) & width_mask(bits);
            let keep = !put & width_mask(bits);
            result = format!("({} & {keep:#x}ull) | ({result} & {put:#x}ull)", d.read()?);
        }
        d.write(result)
    }

    fn extend(&self, i: &Instruction<'_>) -> Result<String> {
        let d = self.reg_at(i, 0)?;
        let bits = d.bits;
        let src = self.read(i, 1)?;
        let src_bits = match i.mnemonic.as_bytes()[3] {
            b'b' => 8,
            b'h' => 16,
            _ => 32,
        };
        let result = if i.mnemonic.starts_with('s') {
            format!("uint{bits}_t(int{bits}_t(int{src_bits}_t({src})))")
        } else {
            format!("uint{bits}_t(uint{src_bits}_t({src}))")
        };
        d.write(result)
    }

    fn conditional(&self, i: &Instruction<'_>) -> Result<String> {
        let cond = i.cond()?;
        let d = self.reg_at(i, 0)?;
        let bits = d.bits;
        let result = match i.mnemonic {
            "cset" => format!("({cond}) ? 1 : 0"),
            "csetm" => format!("({cond}) ? uint{bits}_t(~uint{bits}_t(0)) : uint{bits}_t(0)"),
            "ccmp" | "ccmn" => {
                let n = d.read()?;
                let b = self.read_shifted(i.op(1)?, bits)?;
                let (helper, carry) = if i.mnemonic == "ccmp" {
                    ("subs", "")
                } else {
                    ("adds", ", 0")
                };
                return Ok(format!(
                    "if ({cond}) nx::{helper}{bits}(c, {n}, {b}{carry}); else nx::set_nzcv(c, {});",
                    i.immediate(2)?
                ));
            }
            "csel" => format!("{cond} ? {} : {}", self.read(i, 1)?, self.read(i, 2)?),
            "csinc" | "csinv" | "csneg" => {
                let n = self.read(i, 1)?;
                let m = self.read(i, 2)?;
                let inverted = match i.mnemonic {
                    "csinc" => format!("{m} + 1"),
                    "csinv" => format!("~{m}"),
                    _ => format!("0 - {m}"),
                };
                format!("{cond} ? uint{bits}_t({n}) : uint{bits}_t({inverted})")
            }
            _ => {
                let n = self.read(i, 1)?;
                let inverted = match i.mnemonic {
                    "cinc" => format!("{n} + 1"),
                    "cinv" => format!("~{n}"),
                    _ => format!("0 - {n}"),
                };
                format!("({cond}) ? uint{bits}_t({inverted}) : uint{bits}_t({n})")
            }
        };
        d.write(result)
    }

    fn multiply_divide(&self, i: &Instruction<'_>) -> Result<String> {
        let d = self.reg_at(i, 0)?;
        let bits = d.bits;
        let n = self.read(i, 1)?;
        let m = self.read(i, 2)?;
        d.write(match i.mnemonic {
            "madd" | "msub" => {
                let a = self.read(i, 3)?;
                let op = if i.mnemonic == "madd" { '+' } else { '-' };
                format!("uint{bits}_t({a} {op} uint{bits}_t({n}) * uint{bits}_t({m}))")
            }
            "mul" => format!("uint{bits}_t(uint{bits}_t({n}) * uint{bits}_t({m}))"),
            "mneg" => format!("uint{bits}_t(0 - uint{bits}_t({n}) * uint{bits}_t({m}))"),
            "smulh" => format!("uint64_t(nx::smulh(int64_t({n}), int64_t({m})))"),
            "umulh" => format!("nx::umulh({n}, {m})"),
            "udiv" => format!("nx::udiv{bits}({n}, {m})"),
            "sdiv" => format!("uint{bits}_t(nx::sdiv{bits}(int{bits}_t({n}), int{bits}_t({m})))"),
            _ => self.long_mul(i, d, &n, &m)?,
        })
    }

    fn long_mul(&self, i: &Instruction<'_>, d: Reg, n: &str, m: &str) -> Result<String> {
        if !d.gp {
            return Err(unsupported("no handler"));
        }
        let t = if i.mnemonic.starts_with('s') {
            "int"
        } else {
            "uint"
        };
        let prod = format!("uint64_t({t}64_t({t}32_t({n})) * {t}64_t({t}32_t({m})))");
        if matches!(i.mnemonic, "smull" | "umull") {
            return Ok(prod);
        }
        let a = self.read(i, 3)?;
        let op = if i.mnemonic.ends_with("addl") {
            '+'
        } else {
            '-'
        };
        Ok(format!("{a} {op} {prod}"))
    }

    fn bits(&self, i: &Instruction<'_>) -> Result<String> {
        let d = self.reg_at(i, 0)?;
        let bits = d.bits;
        let n = self.read(i, 1)?;
        d.write(match i.mnemonic {
            "clz" => format!("std::countl_zero(uint{bits}_t({n}))"),
            "cls" => format!("std::countl_zero(uint{bits}_t((uint{bits}_t({n}) ^ (uint{bits}_t({n}) << 1)) | 1)) "),
            "rbit" => format!("nx::rbit{bits}({n})"),
            "rev" => format!("nx::bswap(uint{bits}_t({n}))"),
            "rev16" => format!("nx::rev16_{bits}({n})"),
            _ => format!("nx::rev32_64({n})"),
        })
    }
}
