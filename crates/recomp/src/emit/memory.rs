//! Plain loads and stores and single-register atomic operations.

use super::{hex_signed, unsupported, Emitter, Instruction, Reg, Result, B};
use capstone::arch::arm64::{
    Arm64Extender, Arm64OpMem, Arm64Operand, Arm64OperandType, Arm64Shift,
};

impl Emitter<'_> {
    pub(super) fn memory(&self, i: &Instruction<'_>) -> Result<Option<String>> {
        let (size, signed, store, pair) = match i.mnemonic {
            "ldr" | "ldur" => (None, false, false, false),
            "ldrb" | "ldurb" => (Some(1), false, false, false),
            "ldrh" | "ldurh" => (Some(2), false, false, false),
            "ldrsb" | "ldursb" => (Some(1), true, false, false),
            "ldrsh" | "ldursh" => (Some(2), true, false, false),
            "ldrsw" | "ldursw" => (Some(4), true, false, false),
            "str" | "stur" => (None, false, true, false),
            "strb" | "sturb" => (Some(1), false, true, false),
            "strh" | "sturh" => (Some(2), false, true, false),
            "ldp" | "ldnp" => (None, false, false, true),
            "ldpsw" => (Some(4), true, false, true),
            "stp" | "stnp" => (None, false, true, true),
            _ => return self.atomic(i),
        };
        let Some(at) = i
            .operands
            .iter()
            .position(|op| matches!(op.op_type, Arm64OperandType::Mem(_)))
        else {
            let d = self.reg_at(i, 0)?;
            let addr = hex_signed(i.immediate(i.operands.len() - 1)?);
            return Ok(Some(load(
                d,
                &format!("{B} + {addr}ull"),
                size.unwrap_or(d.bits / 8),
                signed,
            )?));
        };
        let regs: Vec<_> = i.operands[..at]
            .iter()
            .map(|op| self.reg(op))
            .collect::<Result<_>>()?;
        let first = *regs
            .first()
            .ok_or_else(|| unsupported("missing register"))?;
        let size = size.unwrap_or(first.bits / 8);
        let memo = i.op(at)?;
        let Arm64OperandType::Mem(mem) = memo.op_type else {
            unreachable!()
        };
        let base = self.reg_id(mem.base())?.index()?;
        let (address, writeback) = self.address(i, memo, mem, i.operands.get(at + 1))?;
        let mut parts = vec![format!("uint64_t a = {address};")];
        if store {
            for (k, r) in regs.iter().enumerate() {
                let addr = if k == 0 {
                    "a".to_owned()
                } else {
                    format!("a + {}", k as u32 * size)
                };
                parts.push(store_value(*r, &addr, size)?);
            }
            parts.extend(writeback);
        } else if pair {
            let t = memory_type(size, signed);
            for k in 0..regs.len() {
                parts.push(format!("auto v{k} = nx::ld<{t}>(a + {});", k as u32 * size));
            }
            parts.extend(writeback);
            for (k, r) in regs.iter().enumerate() {
                parts.push(load_result(*r, &format!("v{k}"), size, signed)?);
            }
        } else {
            if writeback.is_some() && first.gp && first.n == Some(base) {
                return Err(unsupported("load writeback to base"));
            }
            parts.extend(writeback);
            parts.push(load(first, "a", size, signed)?);
        }
        Ok(Some(format!("{{ {} }}", parts.join(" "))))
    }

    fn address(
        &self,
        i: &Instruction<'_>,
        operand: &Arm64Operand,
        mem: Arm64OpMem,
        after: Option<&Arm64Operand>,
    ) -> Result<(String, Option<String>)> {
        let base = self.reg_id(mem.base())?.index()?;
        let mut addr = format!("c.x[{base}]");
        if mem.index().0 != 0 {
            use Arm64Extender::*;
            let index = self.reg_id(mem.index())?.read()?;
            let mut index = match operand.ext {
                ARM64_EXT_SXTW => format!("uint64_t(int64_t(int32_t({index})))"),
                ARM64_EXT_UXTW => format!("uint64_t(uint32_t({index}))"),
                ARM64_EXT_SXTX => format!("uint64_t({index})"),
                _ => index,
            };
            if let Arm64Shift::Lsl(n) = operand.shift {
                if n != 0 {
                    index = format!("(uint64_t({index}) << {n})");
                }
            }
            addr.push_str(&format!(" + uint64_t({index})"));
        }
        if let Some(post) = after {
            let inc = match post.op_type {
                Arm64OperandType::Imm(v) => Some(format!("{:#x}ull", v as u64)),
                Arm64OperandType::Reg(id) => Some(self.reg_id(id)?.read()?),
                _ => None,
            };
            if let Some(inc) = inc {
                return Ok((addr, Some(format!("c.x[{base}] += {inc};"))));
            }
        }
        if mem.disp() != 0 {
            addr.push_str(&format!(" + {:#x}ull", i64::from(mem.disp()) as u64));
        }
        let writeback = i.writeback.then(|| format!("c.x[{base}] = a;"));
        Ok((addr, writeback))
    }

    fn atomic(&self, i: &Instruction<'_>) -> Result<Option<String>> {
        let op = match i.mnemonic {
            "ldar" | "ldarb" | "ldarh" => "ld_acq",
            "stlr" | "stlrb" | "stlrh" => "st_rel",
            "ldaxr" | "ldaxrb" | "ldaxrh" | "ldxr" | "ldxrb" | "ldxrh" => "ldxr",
            "stlxr" | "stlxrb" | "stlxrh" | "stxr" | "stxrb" | "stxrh" => "stxr",
            _ => return Ok(None),
        };
        let mem = i
            .operands
            .iter()
            .find_map(|op| match op.op_type {
                Arm64OperandType::Mem(mem) => Some(mem),
                _ => None,
            })
            .ok_or_else(|| unsupported("expected memory operand"))?;
        let base = self.reg_id(mem.base())?.index()?;
        let d = self.reg_at(i, 0)?;
        let value = if op == "stxr" { self.reg_at(i, 1)? } else { d };
        let bits = match i.mnemonic.as_bytes().last() {
            Some(b'b') => 8,
            Some(b'h') => 16,
            _ => value.bits,
        };
        let v = value.read()?;
        let statement = match op {
            "ld_acq" => d.write(format!("nx::ld_acq<uint{bits}_t>(c.x[{base}])"))?,
            "st_rel" => format!("nx::st_rel<uint{bits}_t>(c.x[{base}], uint{bits}_t({v}));"),
            "ldxr" => d.write(format!("nx::ldxr<uint{bits}_t>(c, c.x[{base}])"))?,
            _ => d.write(format!(
                "nx::stxr<uint{bits}_t>(c, c.x[{base}], uint{bits}_t({v}))"
            ))?,
        };
        Ok(Some(statement))
    }
}

fn memory_type(size: u32, signed: bool) -> String {
    match (size, signed) {
        (16, _) => "nx::V128".to_owned(),
        (_, true) => format!("int{}_t", size * 8),
        (_, false) => format!("uint{}_t", size * 8),
    }
}

fn load_result(r: Reg, value: &str, size: u32, signed: bool) -> Result<String> {
    if r.gp {
        if signed {
            let bits = r.bits;
            return r.write(format!("uint{bits}_t(int{bits}_t({value}))"));
        }
        return r.write(value);
    }
    let n = r.index()?;
    if size == 16 {
        Ok(format!("c.v[{n}] = {value};"))
    } else {
        let bits = size * 8;
        Ok(format!(
            "{{ nx::V128 t_ = nx::vzero(); t_.u{bits}[0] = {value}; c.v[{n}] = t_; }}"
        ))
    }
}

fn load(r: Reg, addr: &str, size: u32, signed: bool) -> Result<String> {
    load_result(
        r,
        &format!("nx::ld<{}>({addr})", memory_type(size, signed)),
        size,
        signed,
    )
}

fn store_value(r: Reg, addr: &str, size: u32) -> Result<String> {
    if r.gp {
        let v = r.read()?;
        let bits = size * 8;
        return Ok(format!("nx::st<uint{bits}_t>({addr}, uint{bits}_t({v}));"));
    }
    let n = r.index()?;
    if size == 16 {
        Ok(format!("nx::st<nx::V128>({addr}, c.v[{n}]);"))
    } else {
        let bits = size * 8;
        Ok(format!(
            "nx::st<uint{bits}_t>({addr}, c.v[{n}].u{bits}[0]);"
        ))
    }
}
