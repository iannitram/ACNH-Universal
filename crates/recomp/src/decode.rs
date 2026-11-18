//! A small AArch64 decoder: only what the Analyse pass reads (branches, address
//! materialisation, jump-table shapes, and which general register an instruction
//! writes). It stands in for Capstone's lite disassembly in `recomp/analysis.py`.
//! Words in reserved encoding space decode to `None`, like Capstone's skipdata.

/// A decoded instruction. Register number 31 (sp/zr) never matches a search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Insn {
    B(u64),
    Bl(u64),
    /// `b.cond` with its condition code (2 = hs, 8 = hi).
    BCond(u64, u8),
    /// cbz / cbnz / tbz / tbnz, with the target.
    CmpBranch(u64),
    Br(u32),
    Adrp {
        rd: u32,
        page: u64,
    },
    Adr {
        rd: u32,
        target: u64,
    },
    /// `add rd, rn, #imm` (the immediate is the unshifted field, as Capstone prints it).
    AddImm {
        rd: u32,
        rn: u32,
        imm: u64,
    },
    /// `add rd, rn, rm{, lsl|uxtw|sxtw #shift}`; shift is 0 for other forms.
    AddReg {
        rd: u32,
        rn: u32,
        rm: u32,
        shift: u32,
    },
    Mov {
        rd: u32,
        rm: u32,
    },
    MovImm {
        rd: u32,
        imm: i64,
    },
    /// uxtw / sxtw / `and rd, rn, #0xffffffff`.
    Copy {
        rd: u32,
        rn: u32,
    },
    CmpImm {
        rn: u32,
        imm: u64,
    },
    /// ldr family; `ldr_name` is false for ldur/ldtr, which `startswith("ldr")` rejects.
    Ldr {
        rt: u32,
        rn: u32,
        /// Register-offset index.
        rm: Option<u32>,
        /// Byte offset of the unsigned-immediate form.
        uoff: Option<u64>,
        wide: bool,
        mnemonic: &'static str,
        ldr_name: bool,
    },
    /// Anything else, with the (up to two) general registers it writes.
    Other(Option<u32>, Option<u32>),
}

impl Insn {
    /// The registers written (first, second), as Python's operand-0 / ldp-operand-1 rule.
    pub fn dests(&self) -> (Option<u32>, Option<u32>) {
        let r = |x: u32| if x < 31 { Some(x) } else { None };
        match *self {
            Insn::B(_)
            | Insn::Bl(_)
            | Insn::BCond(..)
            | Insn::CmpBranch(_)
            | Insn::Br(_)
            | Insn::CmpImm { .. } => (None, None),
            Insn::Adrp { rd, .. }
            | Insn::Adr { rd, .. }
            | Insn::AddImm { rd, .. }
            | Insn::AddReg { rd, .. }
            | Insn::Mov { rd, .. }
            | Insn::MovImm { rd, .. }
            | Insn::Copy { rd, .. } => (r(rd), None),
            Insn::Ldr { rt, .. } => (r(rt), None),
            Insn::Other(a, b) => (a, b),
        }
    }
}

fn sext(v: u32, bits: u32) -> i64 {
    let s = 64 - bits;
    (i64::from(v) << s) >> s
}

fn reg(r: u32) -> Option<u32> {
    if r < 31 {
        Some(r)
    } else {
        None
    }
}

fn other(rd: u32) -> Option<Insn> {
    Some(Insn::Other(reg(rd), None))
}

/// DecodeBitMasks for logical immediates; None for reserved encodings.
fn bitmask(sf: bool, n: u32, imms: u32, immr: u32) -> Option<u64> {
    let x = (n << 6) | ((!imms) & 0x3F);
    if x == 0 {
        return None;
    }
    let len = 31 - x.leading_zeros();
    if len < 1 || (!sf && n == 1) {
        return None;
    }
    let esize = 1u32 << len;
    let levels = esize - 1;
    let (s, r) = (imms & levels, immr & levels);
    if s == levels {
        return None;
    }
    let welem = (1u64 << (s + 1)) - 1;
    let emask = if esize == 64 {
        u64::MAX
    } else {
        (1u64 << esize) - 1
    };
    let rot = if r == 0 {
        welem
    } else {
        ((welem >> r) | (welem << (esize - r))) & emask
    };
    let mut out = 0u64;
    let mut i = 0;
    while i < 64 {
        out |= rot << i;
        i += esize;
    }
    Some(if sf { out } else { out & 0xFFFF_FFFF })
}

fn ldr_name(size: u32, opc: u32) -> &'static str {
    match (opc, size) {
        (1, 0) => "ldrb",
        (1, 1) => "ldrh",
        (2, 0) | (3, 0) => "ldrsb",
        (2, 1) | (3, 1) => "ldrsh",
        (2, 2) => "ldrsw",
        _ => "ldr",
    }
}

fn decode_loadstore(w: u32) -> Option<Insn> {
    let (rt, rn, rm) = (w & 31, (w >> 5) & 31, (w >> 16) & 31);
    let v = (w >> 26) & 1;
    let top = (w >> 27) & 7; // bits 29:27
    match top {
        0b001 => {
            // exclusive / ordered
            if v == 1 || (w >> 24) & 3 != 0 {
                return Some(Insn::Other(None, None));
            }
            if (w >> 22) & 1 == 1 {
                let t2 = if (w >> 21) & 1 == 1 {
                    reg((w >> 10) & 31)
                } else {
                    None
                };
                Some(Insn::Other(reg(rt), t2))
            } else {
                Some(Insn::Other(reg(rm), None))
            }
        }
        0b011 if (w >> 24) & 3 == 0 => {
            // literal
            if v == 1 || ((w >> 30) & 3 == 3) {
                Some(Insn::Other(None, None))
            } else {
                other(rt)
            }
        }
        0b101 => {
            // pair
            if v == 1 || (w >> 22) & 1 == 0 {
                Some(Insn::Other(None, None))
            } else {
                Some(Insn::Other(reg(rt), reg((w >> 10) & 31)))
            }
        }
        0b111 => {
            if v == 1 {
                return Some(Insn::Other(None, None));
            }
            let size = (w >> 30) & 3;
            let opc = (w >> 22) & 3;
            let unsigned = (w >> 24) & 1 == 1;
            let b21 = (w >> 21) & 1;
            let b1110 = (w >> 10) & 3;
            if !unsigned && b21 == 1 && b1110 == 0 {
                return other(rt); // atomics
            }
            if opc == 0 || (size == 3 && opc == 2) {
                return Some(Insn::Other(None, None));
            }
            let mnemonic = ldr_name(size, opc);
            let wide = size == 3 && opc == 1;
            let (rmo, uoff, name) = if unsigned {
                (None, Some(u64::from((w >> 10) & 0xFFF) << size), true)
            } else if b21 == 1 && b1110 == 2 {
                (Some(rm), None, true)
            } else if b21 == 0 && b1110 == 0 {
                (None, None, false) // ldur / ldtr
            } else {
                (None, None, true)
            };
            Some(Insn::Ldr {
                rt,
                rn,
                rm: rmo,
                uoff,
                wide,
                mnemonic,
                ldr_name: name,
            })
        }
        _ => Some(Insn::Other(None, None)),
    }
}

fn decode_dp_imm(w: u32, pc: u64) -> Option<Insn> {
    let (rd, rn) = (w & 31, (w >> 5) & 31);
    let sf = (w >> 31) & 1 == 1;
    match (w >> 23) & 7 {
        0 | 1 => {
            let imm = sext(((w >> 5) & 0x7FFFF) << 2 | ((w >> 29) & 3), 21);
            if sf {
                let page = (pc & !0xFFF).wrapping_add((imm << 12) as u64);
                Some(Insn::Adrp { rd, page })
            } else {
                Some(Insn::Adr {
                    rd,
                    target: pc.wrapping_add(imm as u64),
                })
            }
        }
        2 => {
            let (op, s, sh) = ((w >> 30) & 1, (w >> 29) & 1, (w >> 22) & 1);
            let imm12 = u64::from((w >> 10) & 0xFFF);
            if op == 0 && s == 0 {
                Some(Insn::AddImm { rd, rn, imm: imm12 })
            } else if op == 1 && s == 1 && rd == 31 && sh == 0 {
                Some(Insn::CmpImm { rn, imm: imm12 })
            } else {
                other(rd)
            }
        }
        3 => other(rd),
        4 => {
            let (opc, n) = ((w >> 29) & 3, (w >> 22) & 1);
            let mask = bitmask(sf, n, (w >> 10) & 0x3F, (w >> 16) & 0x3F)?;
            if opc == 1 && rn == 31 && rd != 31 {
                let imm = if sf {
                    mask as i64
                } else {
                    i64::from(mask as u32 as i32)
                };
                Some(Insn::MovImm { rd, imm })
            } else if opc == 0 && sf && mask == 0xFFFF_FFFF && rn != 31 && rd != 31 {
                Some(Insn::Copy { rd, rn })
            } else {
                other(rd)
            }
        }
        5 => {
            let (opc, hw) = ((w >> 29) & 3, (w >> 21) & 3);
            if opc == 1 || (!sf && hw >= 2) {
                return None;
            }
            let imm = u64::from((w >> 5) & 0xFFFF) << (16 * hw);
            let val = |v: u64| {
                if sf {
                    v as i64
                } else {
                    i64::from(v as u32 as i32)
                }
            };
            if opc == 2 && rd != 31 && !(imm == 0 && hw != 0) {
                Some(Insn::MovImm { rd, imm: val(imm) })
            } else if opc == 0 && rd != 31 && !(imm == 0xFFFF && hw != 0) {
                Some(Insn::MovImm { rd, imm: val(!imm) })
            } else {
                other(rd)
            }
        }
        6 => {
            let (opc, n) = ((w >> 29) & 3, (w >> 22) & 1);
            if opc == 3 || n != u32::from(sf) {
                return None;
            }
            let (immr, imms) = ((w >> 16) & 0x3F, (w >> 10) & 0x3F);
            if opc == 0 && sf && immr == 0 && imms == 31 && rn != 31 && rd != 31 {
                Some(Insn::Copy { rd, rn })
            } else {
                other(rd)
            }
        }
        _ => other(rd),
    }
}

fn decode_dp_reg(w: u32) -> Option<Insn> {
    let (rd, rn, rm) = (w & 31, (w >> 5) & 31, (w >> 16) & 31);
    if (w >> 21) & 0xFF == 0xD2 {
        return Some(Insn::Other(None, None)); // ccmp / ccmn
    }
    if (w >> 28) & 1 == 0 {
        if (w >> 24) & 1 == 0 {
            // logical, shifted register
            let (opc, n, imm6) = ((w >> 29) & 3, (w >> 21) & 1, (w >> 10) & 0x3F);
            if opc == 1 && n == 0 && rn == 31 && imm6 == 0 && ((w >> 22) & 3) == 0 && rd != 31 {
                return Some(Insn::Mov { rd, rm });
            }
            return other(rd);
        }
        let (op, s) = ((w >> 30) & 1, (w >> 29) & 1);
        if op == 0 && s == 0 && rd != 31 {
            let shift = if (w >> 21) & 1 == 0 {
                if (w >> 22) & 3 == 0 {
                    (w >> 10) & 0x3F
                } else {
                    0
                }
            } else {
                let (opt, imm3) = ((w >> 13) & 7, (w >> 10) & 7);
                match opt {
                    2 | 6 => imm3,
                    3 if rd == 31 || rn == 31 => imm3,
                    _ => 0,
                }
            };
            return Some(Insn::AddReg { rd, rn, rm, shift });
        }
    }
    other(rd)
}

fn decode_simd(w: u32) -> Option<Insn> {
    let rd = w & 31;
    if w & 0x7F20_FC00 == 0x1E20_0000 {
        let opcode = (w >> 16) & 7;
        if matches!(opcode, 0 | 1 | 4 | 5 | 6) {
            return other(rd);
        }
    } else if w & 0xBFE0_8400 == 0x0E00_0400 && matches!((w >> 11) & 15, 5 | 7) {
        return other(rd);
    }
    Some(Insn::Other(None, None))
}

/// Decode the word at `pc`; None when it is not an instruction.
pub fn decode(w: u32, pc: u64) -> Option<Insn> {
    let rd = w & 31;
    match (w >> 25) & 0xF {
        0b0000 => (w >> 16 == 0).then_some(Insn::Other(None, None)),
        0b0001 | 0b0011 => None,
        0b0010 => Some(Insn::Other(None, None)),
        0b1000 | 0b1001 => decode_dp_imm(w, pc),
        0b1010 | 0b1011 => {
            let imm26 = sext(w & 0x03FF_FFFF, 26) << 2;
            let imm19 = sext((w >> 5) & 0x7FFFF, 19) << 2;
            let imm14 = sext((w >> 5) & 0x3FFF, 14) << 2;
            Some(if w & 0x7C00_0000 == 0x1400_0000 {
                let t = pc.wrapping_add(imm26 as u64);
                if w >> 31 == 1 {
                    Insn::Bl(t)
                } else {
                    Insn::B(t)
                }
            } else if w & 0xFF00_0010 == 0x5400_0000 {
                Insn::BCond(pc.wrapping_add(imm19 as u64), (w & 15) as u8)
            } else if w & 0x7E00_0000 == 0x3400_0000 {
                Insn::CmpBranch(pc.wrapping_add(imm19 as u64))
            } else if w & 0x7E00_0000 == 0x3600_0000 {
                Insn::CmpBranch(pc.wrapping_add(imm14 as u64))
            } else if w & 0xFFFF_FC1F == 0xD61F_0000 {
                Insn::Br((w >> 5) & 31)
            } else if w & 0xFFF0_0000 == 0xD530_0000 {
                Insn::Other(reg(rd), None)
            } else {
                Insn::Other(None, None)
            })
        }
        0b0100 | 0b0110 | 0b1100 | 0b1110 => decode_loadstore(w),
        0b0101 | 0b1101 => decode_dp_reg(w),
        _ => decode_simd(w),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branches_and_pc_relative() {
        assert_eq!(decode(0x9400_0004, 0x1000), Some(Insn::Bl(0x1010)));
        assert_eq!(decode(0x17FF_FFFF, 0x1000), Some(Insn::B(0x0FFC)));
        assert_eq!(decode(0x5400_0088, 0x1000), Some(Insn::BCond(0x1010, 8)));
        assert_eq!(
            decode(0xB000_0010, 0x1000),
            Some(Insn::Adrp {
                rd: 16,
                page: 0x1000 + 0x1000
            })
        );
        assert_eq!(decode(0xD61F_0220, 0), Some(Insn::Br(17)));
        assert_eq!(decode(0xD65F_03C0, 0), Some(Insn::Other(None, None)));
    }

    #[test]
    fn logical_immediates() {
        assert_eq!(bitmask(true, 1, 0x1F, 0), Some(0xFFFF_FFFF));
        assert_eq!(bitmask(true, 1, 0x3F, 0), None);
    }
}
