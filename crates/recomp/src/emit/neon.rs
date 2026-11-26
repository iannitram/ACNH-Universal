//! AdvSIMD lane operations and common helpers.
use super::{unsupported, Emitter, Instruction, Result};
use capstone::arch::arm64::{Arm64Operand, Arm64OperandType, Arm64Shift, Arm64Vas};
use regex::Regex;
use std::sync::LazyLock;

static LANE_VARIABLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[abo]\b").expect("valid lane variable pattern"));

pub(super) fn substitute(expr: &str, pairs: &[(&str, String)]) -> String {
    LANE_VARIABLE
        .replace_all(expr, |c: &regex::Captures<'_>| {
            pairs
                .iter()
                .find(|(key, _)| *key == &c[0])
                .map_or_else(|| c[0].to_owned(), |(_, v)| v.clone())
        })
        .into_owned()
}
impl Emitter<'_> {
    pub(super) fn vinfo(&self, op: &Arm64Operand) -> Result<(u32, u32)> {
        use Arm64Vas::*;
        Ok(match op.vas {
            ARM64_VAS_16B => (8, 16),
            ARM64_VAS_8B => (8, 8),
            ARM64_VAS_4B => (8, 4),
            ARM64_VAS_1B => (8, 1),
            ARM64_VAS_8H => (16, 8),
            ARM64_VAS_4H => (16, 4),
            ARM64_VAS_2H => (16, 2),
            ARM64_VAS_1H => (16, 1),
            ARM64_VAS_4S => (32, 4),
            ARM64_VAS_2S => (32, 2),
            ARM64_VAS_1S => (32, 1),
            ARM64_VAS_2D => (64, 2),
            ARM64_VAS_1D => (64, 1),
            ARM64_VAS_1Q => (128, 1),
            ARM64_VAS_INVALID => {
                let r = self.reg(op)?;
                if r.gp || r.bits == 128 || op.vector_index.is_some() {
                    return Err(unsupported("vector without arrangement"));
                }
                (r.bits, 1)
            }
        })
    }
    pub(super) fn v_bin(
        &self,
        i: &Instruction<'_>,
        expr: &str,
        kind: &str,
        bytes: bool,
    ) -> Result<String> {
        let (mut bits, mut n) = self.vinfo(i.op(0)?)?;
        if bytes {
            n = n * bits / 8;
            bits = 8;
        }
        let d = self.reg_at(i, 0)?.index()?;
        let a = self.reg_at(i, 1)?.index()?;
        let b = self.reg_at(i, 2)?.index()?;
        let idx = i
            .op(2)?
            .vector_index
            .map_or("k".to_owned(), |v| v.to_string());
        let e = substitute(
            expr,
            &[
                ("a", format!("S0.{kind}{bits}[k]")),
                ("b", format!("S1.{kind}{bits}[{idx}]")),
                ("o", format!("O.{kind}{bits}[k]")),
            ],
        );
        let ty = match (kind, bits) {
            ("f", 32) => "float".to_owned(),
            ("f", 64) => "double".to_owned(),
            ("s", _) => format!("int{bits}_t"),
            _ => format!("uint{bits}_t"),
        };
        Ok(format!("{{ nx::V128 S0 = c.v[{a}]; nx::V128 S1 = c.v[{b}]; nx::V128 O = c.v[{d}]; nx::V128 r = nx::vzero(); for (int k = 0; k < {n}; k++) r.{kind}{bits}[k] = {ty}({e}); c.v[{d}] = r; }}"))
    }
    pub(super) fn v_un(
        &self,
        i: &Instruction<'_>,
        expr: &str,
        kind: &str,
        source_kind: &str,
        bytes: bool,
    ) -> Result<String> {
        let (mut bits, mut n) = self.vinfo(i.op(0)?)?;
        if bytes {
            n = n * bits / 8;
            bits = 8;
        }
        let d = self.reg_at(i, 0)?.index()?;
        let a = self.reg_at(i, 1)?.index()?;
        let idx = i
            .op(1)?
            .vector_index
            .map_or("k".to_owned(), |v| v.to_string());
        let e = substitute(expr, &[("a", format!("S.{source_kind}{bits}[{idx}]"))]);
        let ty = match (kind, bits) {
            ("f", 32) => "float".to_owned(),
            ("f", 64) => "double".to_owned(),
            ("s", _) => format!("int{bits}_t"),
            _ => format!("uint{bits}_t"),
        };
        Ok(format!("{{ nx::V128 S = c.v[{a}]; nx::V128 r = nx::vzero(); for (int k = 0; k < {n}; k++) r.{kind}{bits}[k] = {ty}({e}); c.v[{d}] = r; }}"))
    }
    pub(super) fn v_shift_imm(
        &self,
        i: &Instruction<'_>,
        expr: &str,
        signed: bool,
        acc: bool,
    ) -> Result<String> {
        let (bits, n) = self.vinfo(i.op(0)?)?;
        let d = self.reg_at(i, 0)?.index()?;
        let a = self.reg_at(i, 1)?.index()?;
        let sh = i.immediate(2)?;
        let kind = if signed { "s" } else { "u" };
        let ty = if signed { "int" } else { "uint" };
        let e = expr
            .replace("{a}", &format!("{ty}64_t(S.{kind}{bits}[k])"))
            .replace("{n}", &sh.to_string())
            .replace("{bits}", &bits.to_string())
            .replace("{o}", &format!("r.{kind}{bits}[k]"));
        let init = if acc {
            format!("c.v[{d}]")
        } else {
            "nx::vzero()".to_owned()
        };
        let clear = if bits * n == 128 {
            ""
        } else {
            "r.u64[1] = 0; "
        };
        Ok(format!("{{ nx::V128 S = c.v[{a}]; nx::V128 r = {init}; for (int k = 0; k < {n}; k++) r.{kind}{bits}[k] = {ty}{bits}_t({e}); {clear}c.v[{d}] = r; }}"))
    }
    pub(super) fn neon(&self, i: &Instruction<'_>) -> Result<Option<String>> {
        let d_op = match i.operands.first() {
            Some(x) => x,
            None => return Ok(None),
        };
        let vector = d_op.vas != Arm64Vas::ARM64_VAS_INVALID
            || d_op.vector_index.is_some()
            || self.reg(d_op).is_ok_and(|r| !r.gp);
        if !vector
            && !matches!(i.mnemonic, "umov" | "smov")
            && !(i.mnemonic == "mov" && i.operands.get(1).is_some_and(|o| o.vector_index.is_some()))
        {
            return Ok(None);
        }
        let bin = match i.mnemonic {
            "add" => Some(("a + b", "u", false)),
            "sub" => Some(("a - b", "u", false)),
            "mul" => Some(("a * b", "u", false)),
            "and" => Some(("a & b", "u", true)),
            "orr" => Some(("a | b", "u", true)),
            "eor" => Some(("a ^ b", "u", true)),
            "bic" => Some(("a & ~b", "u", true)),
            "orn" => Some(("a | ~b", "u", true)),
            "mla" => Some(("o + a * b", "u", false)),
            "mls" => Some(("o - a * b", "u", false)),
            "bsl" => Some(("(o & a) | (~o & b)", "u", true)),
            "bit" => Some(("(o & ~b) | (a & b)", "u", true)),
            "bif" => Some(("(o & b) | (a & ~b)", "u", true)),
            "smax" => Some(("a > b ? a : b", "s", false)),
            "smin" => Some(("a < b ? a : b", "s", false)),
            "umax" => Some(("a > b ? a : b", "u", false)),
            "umin" => Some(("a < b ? a : b", "u", false)),
            "urhadd" => Some(("(uint64_t(a) + uint64_t(b) + 1) >> 1", "u", false)),
            "srhadd" => Some(("(int64_t(a) + int64_t(b) + 1) >> 1", "s", false)),
            "uhadd" => Some(("(uint64_t(a) + uint64_t(b)) >> 1", "u", false)),
            "shadd" => Some(("(int64_t(a) + int64_t(b)) >> 1", "s", false)),
            "sabd" => Some(("a > b ? a - b : b - a", "s", false)),
            "uabd" => Some(("a > b ? a - b : b - a", "u", false)),
            "cmtst" => Some(("(a & b) ? ~0ull : 0", "u", false)),
            "fadd" => Some(("a + b", "f", false)),
            "fsub" => Some(("a - b", "f", false)),
            "fmul" => Some(("a * b", "f", false)),
            "fdiv" => Some(("a / b", "f", false)),
            "fabd" => Some(("std::fabs(a - b)", "f", false)),
            "fmax" => Some(("nx::fmax_(a, b)", "f", false)),
            "fmin" => Some(("nx::fmin_(a, b)", "f", false)),
            "fmaxnm" => Some(("nx::fmaxnm(a, b)", "f", false)),
            "fminnm" => Some(("nx::fminnm(a, b)", "f", false)),
            "fmla" => Some(("std::fma(a, b, o)", "f", false)),
            "fmls" => Some(("std::fma(-a, b, o)", "f", false)),
            "frsqrts" => Some(("nx::frsqrts(a, b)", "f", false)),
            "frecps" => Some(("nx::frecps(a, b)", "f", false)),
            _ => None,
        };
        if let Some((expr, kind, bytes)) = bin {
            if kind == "f" && d_op.vas == Arm64Vas::ARM64_VAS_INVALID {
                return Ok(None);
            }
            if i.operands.len() > 2
                && matches!(i.op(1)?.op_type, Arm64OperandType::Reg(_))
                && matches!(i.op(2)?.op_type, Arm64OperandType::Reg(_))
            {
                return self.v_bin(i, expr, kind, bytes).map(Some);
            }
        }
        // neg/abs compute in the unsigned type so INT_MIN does not overflow (signed overflow is UB in C++).
        let ubits = self.vinfo(d_op).map_or(0, |v| v.0);
        let neg_expr = format!("uint{ubits}_t(0) - uint{ubits}_t(a)");
        let abs_expr = format!("a < 0 ? uint{ubits}_t(0) - uint{ubits}_t(a) : uint{ubits}_t(a)");
        let un = match i.mnemonic {
            "neg" => Some((neg_expr.as_str(), "s", false)),
            "cnt" => Some(("std::popcount(a)", "u", false)),
            "not" | "mvn" => Some(("~a", "u", true)),
            "abs" => Some((abs_expr.as_str(), "s", false)),
            "fneg" => Some(("-a", "f", false)),
            "fabs" => Some(("std::fabs(a)", "f", false)),
            "fsqrt" => Some(("std::sqrt(a)", "f", false)),
            "frintm" => Some(("std::floor(a)", "f", false)),
            "frintp" => Some(("std::ceil(a)", "f", false)),
            "frintz" => Some(("std::trunc(a)", "f", false)),
            "frinta" => Some(("nx::round_away(a)", "f", false)),
            "frintn" | "frinti" | "frintx" => Some(("nx::round_even(a)", "f", false)),
            "frecpe" => Some(("1.0f / a", "f", false)),
            "frsqrte" => Some(("1.0f / std::sqrt(a)", "f", false)),
            _ => None,
        };
        if let Some((expr, kind, bytes)) = un {
            if kind == "f" && d_op.vas == Arm64Vas::ARM64_VAS_INVALID {
                return Ok(None);
            }
            return self.v_un(i, expr, kind, kind, bytes).map(Some);
        }
        if d_op.vas != Arm64Vas::ARM64_VAS_INVALID
            && matches!(
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
            )
        {
            let (bits, n) = self.vinfo(d_op)?;
            let d = self.reg_at(i, 0)?.index()?;
            let a = self.reg_at(i, 1)?.index()?;
            let signed = i.mnemonic.ends_with('s');
            let kind = if signed { "s" } else { "u" };
            let ty = if signed { "int" } else { "uint" };
            let mut v = format!("S.f{bits}[k]");
            if i.operands.len() > 2 {
                v = format!("({v}) * {}", super::fp::fixed_point_scale(i.immediate(2)?));
            }
            let round = match i.mnemonic.as_bytes()[4] {
                b'z' => "std::trunc",
                b'm' => "std::floor",
                b'p' => "std::ceil",
                b'a' => "nx::round_away",
                _ => "nx::round_even",
            };
            return Ok(Some(format!("{{ nx::V128 S = c.v[{a}]; nx::V128 r = nx::vzero(); for (int k = 0; k < {n}; k++) r.{kind}{bits}[k] = nx::fcvt_sat<{ty}{bits}_t>({round}({v})); c.v[{d}] = r; }}")));
        }
        if d_op.vas != Arm64Vas::ARM64_VAS_INVALID && matches!(i.mnemonic, "scvtf" | "ucvtf") {
            let bits = self.vinfo(d_op)?.0;
            let ty = if bits == 32 { "float" } else { "double" };
            let expr = if i.operands.len() > 2 {
                format!(
                    "{ty}(a) / {ty}({})",
                    super::fp::fixed_point_scale(i.immediate(2)?)
                )
            } else {
                "a".to_owned()
            };
            return self
                .v_un(
                    i,
                    &expr,
                    "f",
                    if i.mnemonic == "scvtf" { "s" } else { "u" },
                    false,
                )
                .map(Some);
        }
        let bits = self.vinfo(d_op).map_or(0, |v| v.0);
        if matches!(i.mnemonic, "clz" | "cls") {
            let expression = if i.mnemonic == "clz" {
                format!("std::countl_zero(uint{bits}_t(a))")
            } else {
                format!("std::countl_zero(uint{bits}_t((uint{bits}_t(a) ^ (uint{bits}_t(a) << 1)) | 1))")
            };
            return self.v_un(i, &expression, "u", "u", false).map(Some);
        }
        // 64-bit lanes have no wider native type: saturate through 128-bit intermediates (and detect the unsigned
        // carry by wrap-around), because int64_t arithmetic would overflow (UB) and nx::sat<int64_t> is a no-op.
        let wide = bits == 64;
        let dynamic = match i.mnemonic {
            "sqadd" if wide => Some(("nx::sat128<int64_t>(__int128(a) + __int128(b))".to_owned(), "s")),
            "sqsub" if wide => Some(("nx::sat128<int64_t>(__int128(a) - __int128(b))".to_owned(), "s")),
            "uqadd" if wide => Some(("a + b < a ? ~0ull : a + b".to_owned(), "u")),
            "sqadd" => Some((format!("nx::sat<int{bits}_t>(int64_t(a) + int64_t(b))"), "s")),
            "sqsub" => Some((format!("nx::sat<int{bits}_t>(int64_t(a) - int64_t(b))"), "s")),
            "uqadd" => Some((format!("(uint64_t(a) + uint64_t(b)) > uint{bits}_t(~0ull) ? uint{bits}_t(~0ull) : a + b"), "u")),
            "uqsub" => Some(("a > b ? a - b : 0".to_owned(), "u")),
            "sqdmulh" => Some((format!("nx::sat<int{bits}_t>((int64_t(a) * int64_t(b) * 2) >> {bits})"), "s")),
            "sshl" => Some((format!("int8_t(S1.u{bits}[k]) >= 0 ? (int8_t(S1.u{bits}[k]) >= {bits} ? 0 : int64_t(a) << int8_t(S1.u{bits}[k])) : (int8_t(S1.u{bits}[k]) <= -{bits} ? (a < 0 ? -1 : 0) : int64_t(a) >> -int8_t(S1.u{bits}[k]))"), "s")),
            "ushl" => Some((format!("int8_t(S1.u{bits}[k]) >= 0 ? (int8_t(S1.u{bits}[k]) >= {bits} ? 0 : uint64_t(a) << int8_t(S1.u{bits}[k])) : (int8_t(S1.u{bits}[k]) <= -{bits} ? 0 : uint64_t(a) >> -int8_t(S1.u{bits}[k]))"), "u")),
            _ => None,
        };
        if let Some((expr, kind)) = dynamic {
            return self.v_bin(i, &expr, kind, false).map(Some);
        }
        // Rounding shifts of 64-bit lanes add the rounding constant in 128 bits so the carry out of bit 63 is kept
        // and a shift by #64 is legal; accumulation wraps in the unsigned type (signed overflow is UB).
        let mut shift = match i.mnemonic {
            "shl" => Some(("{a} << {n}", false, false)),
            "ushr" => Some(("({n} >= {bits}) ? 0 : ({a} >> {n})", false, false)),
            "sshr" => Some(("{a} >> ({n} >= {bits} ? {bits} - 1 : {n})", true, false)),
            "srshr" => Some(("({a} + (int64_t(1) << ({n} - 1))) >> {n}", true, false)),
            "urshr" => Some(("({a} + (uint64_t(1) << ({n} - 1))) >> {n}", false, false)),
            "ssra" => Some((
                "{o} + ({a} >> ({n} >= {bits} ? {bits} - 1 : {n}))",
                true,
                true,
            )),
            "usra" => Some(("{o} + (({n} >= {bits}) ? 0 : ({a} >> {n}))", false, true)),
            "srsra" => Some((
                "{o} + (({a} + (int64_t(1) << ({n} - 1))) >> {n})",
                true,
                true,
            )),
            "ursra" => Some((
                "{o} + (({a} + (uint64_t(1) << ({n} - 1))) >> {n})",
                false,
                true,
            )),
            _ => None,
        };
        if bits == 64 {
            shift = match i.mnemonic {
                "srshr" => Some(("({a} + (__int128(1) << ({n} - 1))) >> {n}", true, false)),
                "urshr" => Some((
                    "({a} + ((unsigned __int128)(1) << ({n} - 1))) >> {n}",
                    false,
                    false,
                )),
                "srsra" => Some((
                    "uint64_t({o}) + uint64_t(({a} + (__int128(1) << ({n} - 1))) >> {n})",
                    true,
                    true,
                )),
                "ursra" => Some((
                    "{o} + uint64_t(({a} + ((unsigned __int128)(1) << ({n} - 1))) >> {n})",
                    false,
                    true,
                )),
                "ssra" => Some((
                    "uint64_t({o}) + uint64_t({a} >> ({n} >= {bits} ? {bits} - 1 : {n}))",
                    true,
                    true,
                )),
                _ => shift,
            };
        }
        if let Some((expr, signed, acc)) = shift {
            return self.v_shift_imm(i, expr, signed, acc).map(Some);
        }
        let cmp = match i.mnemonic {
            "cmeq" => Some(("==", "u")),
            "cmgt" => Some((">", "s")),
            "cmge" => Some((">=", "s")),
            "cmhi" => Some((">", "u")),
            "cmhs" => Some((">=", "u")),
            "cmlt" => Some(("<", "s")),
            "cmle" => Some(("<=", "s")),
            "fcmeq" => Some(("==", "f")),
            "fcmgt" => Some((">", "f")),
            "fcmge" => Some((">=", "f")),
            "fcmlt" => Some(("<", "f")),
            "fcmle" => Some(("<=", "f")),
            _ => None,
        };
        if let Some((op, kind)) = cmp {
            let d = self.reg_at(i, 0)?.index()?;
            let a = self.reg_at(i, 1)?.index()?;
            let o = i.op(2)?;
            let b = if matches!(o.op_type, Arm64OperandType::Reg(_)) {
                Some(self.reg(o)?.index()?)
            } else {
                None
            };
            if i.mnemonic == "cmeq" && d_op.vas == Arm64Vas::ARM64_VAS_INVALID {
                let b = b.map_or("0".to_owned(), |v| format!("c.v[{v}].u64[0]"));
                return Ok(Some(format!("{{ uint64_t r_ = c.v[{a}].u64[0] == {b} ? ~0ull : 0; nx::V128 t_ = nx::vzero(); t_.u64[0] = r_; c.v[{d}] = t_; }}")));
            }
            let (bits, n) = self.vinfo(d_op)?;
            let expr = if b.is_some() {
                format!("T.{kind}{bits}[k]")
            } else {
                "0".to_owned()
            };
            let b = b.unwrap_or(a);
            return Ok(Some(format!("{{ nx::V128 S = c.v[{a}]; nx::V128 T = c.v[{b}]; nx::V128 r = nx::vzero(); for (int k = 0; k < {n}; k++) r.u{bits}[k] = (S.{kind}{bits}[k] {op} {expr}) ? uint{bits}_t(~0ull) : 0; c.v[{d}] = r; }}")));
        }
        self.neon_misc(i)
    }
    fn neon_misc(&self, i: &Instruction<'_>) -> Result<Option<String>> {
        let d = self.reg_at(i, 0)?;
        let dn = d.n.unwrap_or(0);
        let stmt = match i.mnemonic {
            "mov" | "umov" | "ins" => {
                let n = self.reg_at(i, 1)?;
                let dst = i.op(0)?;
                let src = i.op(1)?;
                if let Some(lane) = dst.vector_index {
                    let bits = if dst.vas == Arm64Vas::ARM64_VAS_INVALID {
                        32
                    } else {
                        self.vinfo(dst)?.0
                    };
                    if n.gp {
                        format!("c.v[{dn}].u{bits}[{lane}] = uint{bits}_t({});", n.read()?)
                    } else if let Some(s) = src.vector_index {
                        format!(
                            "c.v[{dn}].u{bits}[{lane}] = c.v[{}].u{bits}[{s}];",
                            n.index()?
                        )
                    } else {
                        return Err(unsupported("mov vector form"));
                    }
                } else if let Some(lane) = src.vector_index {
                    let bits = if src.vas == Arm64Vas::ARM64_VAS_INVALID {
                        d.bits
                    } else {
                        self.vinfo(src)?.0
                    };
                    if d.gp {
                        d.write(format!("c.v[{}].u{bits}[{lane}]", n.index()?))?
                    } else {
                        format!("{{ nx::V128 t_ = nx::vzero(); t_.u{bits}[0] = c.v[{}].u{bits}[{lane}]; c.v[{dn}] = t_; }}",n.index()?)
                    }
                } else if dst.vas != Arm64Vas::ARM64_VAS_INVALID
                    && src.vas != Arm64Vas::ARM64_VAS_INVALID
                {
                    let (bits, lanes) = self.vinfo(dst)?;
                    if bits * lanes == 64 {
                        format!("{{ nx::V128 t_ = nx::vzero(); t_.u64[0] = c.v[{}].u64[0]; c.v[{dn}] = t_; }}",n.index()?)
                    } else {
                        format!("c.v[{dn}] = c.v[{}];", n.index()?)
                    }
                } else {
                    return Ok(None);
                }
            }
            "smov" => {
                let n = self.reg_at(i, 1)?;
                let src = i.op(1)?;
                let bits = self.vinfo(src)?.0;
                let lane = src
                    .vector_index
                    .ok_or_else(|| unsupported("missing lane"))?;
                d.write(format!(
                    "uint{}_t(int{}_t(c.v[{}].s{bits}[{lane}]))",
                    d.bits,
                    d.bits,
                    n.index()?
                ))?
            }
            "dup" => {
                let (bits, lanes) = self.vinfo(i.op(0)?)?;
                let n = self.reg_at(i, 1)?;
                let v = if n.gp {
                    format!("uint{bits}_t({})", n.read()?)
                } else {
                    format!(
                        "c.v[{}].u{bits}[{}]",
                        n.index()?,
                        i.op(1)?
                            .vector_index
                            .ok_or_else(|| unsupported("missing lane"))?
                    )
                };
                format!("{{ uint{bits}_t e_ = {v}; nx::V128 r = nx::vzero(); for (int k = 0; k < {lanes}; k++) r.u{bits}[k] = e_; c.v[{dn}] = r; }}")
            }
            "orr" | "bic" if matches!(i.op(1)?.op_type, Arm64OperandType::Imm(_)) => {
                let (bits, lanes) = self.vinfo(i.op(0)?)?;
                let op = i.op(1)?;
                let shift = match op.shift {
                    Arm64Shift::Lsl(v)
                    | Arm64Shift::Msl(v)
                    | Arm64Shift::Lsr(v)
                    | Arm64Shift::Asr(v)
                    | Arm64Shift::Ror(v) => v,
                    _ => 0,
                };
                let imm = (i.immediate(1)? as u64) << shift;
                let expr = if i.mnemonic == "orr" {
                    format!("r.u{bits}[k] | uint{bits}_t({imm:#x}ull)")
                } else {
                    format!("r.u{bits}[k] & uint{bits}_t(~{imm:#x}ull)")
                };
                let clear = if bits * lanes == 128 {
                    ""
                } else {
                    " r.u64[1] = 0;"
                };
                format!("{{ nx::V128 r = c.v[{dn}]; for (int k = 0; k < {lanes}; k++) r.u{bits}[k] = {expr};{clear} c.v[{dn}] = r; }}")
            }
            "movi" | "mvni" => {
                let op = i.op(1)?;
                let mut imm = i.immediate(1)? as u64;
                match op.shift {
                    Arm64Shift::Lsl(n) => imm = imm.wrapping_shl(n),
                    Arm64Shift::Msl(n) => imm = imm.wrapping_shl(n) | ((1u64 << n) - 1),
                    _ => (),
                };
                if i.op(0)?.vas == Arm64Vas::ARM64_VAS_INVALID {
                    format!(
                        "{{ nx::V128 r = nx::vzero(); r.u64[0] = {imm:#x}ull; c.v[{dn}] = r; }}"
                    )
                } else {
                    let (bits, lanes) = self.vinfo(i.op(0)?)?;
                    let imm = if i.mnemonic == "mvni" {
                        format!("~{imm:#x}")
                    } else {
                        format!("{:#x}", imm & super::width_mask(bits))
                    };
                    format!("{{ nx::V128 r = nx::vzero(); for (int k = 0; k < {lanes}; k++) r.u{bits}[k] = uint{bits}_t({imm}ull); c.v[{dn}] = r; }}")
                }
            }
            _ => return Ok(None),
        };
        Ok(Some(stmt))
    }
}
