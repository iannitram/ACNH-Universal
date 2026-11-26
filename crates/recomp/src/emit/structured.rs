//! AdvSIMD structured transfers.
use super::{unsupported, Emitter, Instruction, Result};
use capstone::arch::arm64::Arm64OperandType;

impl Emitter<'_> {
    pub(super) fn structured(&self, i: &Instruction<'_>) -> Result<Option<String>> {
        let (store, structures, replicate) = match i.mnemonic.as_bytes() {
            [b'l', b'd', n @ b'1'..=b'4'] => (false, n - b'0', false),
            [b's', b't', n @ b'1'..=b'4'] => (true, n - b'0', false),
            [b'l', b'd', n @ b'1'..=b'4', b'r'] => (false, n - b'0', true),
            _ => return Ok(None),
        };
        let (at, operand, mem) = i
            .operands
            .iter()
            .enumerate()
            .find_map(|(index, operand)| match operand.op_type {
                Arm64OperandType::Mem(mem) => Some((index, operand, mem)),
                _ => None,
            })
            .ok_or_else(|| unsupported("expected memory operand"))?;
        let registers = i.operands[..at]
            .iter()
            .map(|operand| self.reg(operand)?.index())
            .collect::<Result<Vec<_>>>()?;
        let first = i.op(0)?;
        let (bits, lanes) = self.vinfo(first)?;
        let after = i.operands.get(at + 1);
        let (mut address, mut writeback) = self.address(i, operand, mem, after)?;
        let element = bits / 8;
        let size = element * lanes;
        if i.writeback && after.is_none() {
            // Capstone omits immediate post-index on some structured forms.
            let per_register = if first.vector_index.is_some() || replicate {
                element
            } else {
                size
            };
            let bytes = per_register as usize * registers.len();
            let base = self.reg_id(mem.base())?.index()?;
            address = format!("c.x[{base}]");
            writeback = Some(format!("c.x[{base}] += {bytes};"));
        }
        let lane = first.vector_index.or_else(|| {
            let (_, suffix) = i.op_str.split_once("}[")?;
            suffix.split_once(']')?.0.parse().ok()
        });
        let mut parts = vec![format!("uint64_t a = {address};")];
        let mut commits = Vec::new();
        for (slot, register) in registers.into_iter().enumerate() {
            let offset = slot as u32 * element;
            let lane_address = format!("a + {offset}");
            let statement = if let Some(lane) = lane {
                let value = format!("c.v[{register}].u{bits}[{lane}]");
                if store {
                    format!("nx::st<uint{bits}_t>({lane_address}, {value});")
                } else {
                    format!("{value} = nx::ld<uint{bits}_t>({lane_address});")
                }
            } else if replicate {
                format!("{{ uint{bits}_t e_ = nx::ld<uint{bits}_t>({lane_address}); nx::V128 r_ = nx::vzero(); for (int j = 0; j < {lanes}; j++) r_.u{bits}[j] = e_; c.v[{register}] = r_; }}")
            } else if structures == 1 {
                let offset = slot as u32 * size;
                if store {
                    format!("std::memcpy(reinterpret_cast<void*>(a + {offset}), &c.v[{register}], {size});")
                } else {
                    format!("{{ nx::V128 r_ = nx::vzero(); std::memcpy(&r_, reinterpret_cast<const void*>(a + {offset}), {size}); c.v[{register}] = r_; }}")
                }
            } else {
                let address = format!("a + (j * {structures} + {slot}) * {element}");
                if store {
                    format!("for (int j = 0; j < {lanes}; j++) nx::st<uint{bits}_t>({address}, c.v[{register}].u{bits}[j]);")
                } else {
                    commits.push(format!("c.v[{register}] = t{slot};"));
                    format!("nx::V128 t{slot} = nx::vzero(); for (int j = 0; j < {lanes}; j++) t{slot}.u{bits}[j] = nx::ld<uint{bits}_t>({address});")
                }
            };
            parts.push(statement);
        }
        parts.extend(commits);
        parts.extend(writeback);
        Ok(Some(format!("{{ {} }}", parts.join(" "))))
    }
}
