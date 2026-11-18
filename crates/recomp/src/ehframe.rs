//! Function bounds from `.eh_frame_hdr` / `.eh_frame` (port of `recomp/ehframe.py`).
//! Every C++ function with unwind info has an FDE; the binary search table in the
//! header lists them all.

use crate::load::{Error, Module};

const OMIT: u8 = 0xFF;
const ABSPTR: u8 = 0x00;
const ULEB128: u8 = 0x01;
const UDATA2: u8 = 0x02;
const UDATA4: u8 = 0x03;
const UDATA8: u8 = 0x04;
const SLEB128: u8 = 0x09;
const SDATA2: u8 = 0x0A;
const SDATA4: u8 = 0x0B;
const SDATA8: u8 = 0x0C;
const PCREL: u8 = 0x10;
const DATAREL: u8 = 0x30;

/// One frame description entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fde {
    pub start: u64,
    pub size: u64,
    /// 0 when absent.
    pub lsda: u64,
    /// From the CIE, 0 when absent.
    pub personality: u64,
}

fn bad(what: &str, at: usize) -> Error {
    Error::Malformed(format!("eh_frame: {what} at {at:#x}"))
}

fn byte(b: &[u8], p: usize) -> Result<u8, Error> {
    b.get(p).copied().ok_or_else(|| bad("read past image", p))
}

fn bytes<const N: usize>(b: &[u8], p: usize) -> Result<[u8; N], Error> {
    let end = p.checked_add(N).ok_or_else(|| bad("overflow", p))?;
    let s = b.get(p..end).ok_or_else(|| bad("read past image", p))?;
    let mut out = [0u8; N];
    out.copy_from_slice(s);
    Ok(out)
}

fn uleb(b: &[u8], mut p: usize) -> Result<(u64, usize), Error> {
    let (mut r, mut s) = (0u64, 0u32);
    loop {
        let x = byte(b, p)?;
        p += 1;
        if s < 64 {
            r |= u64::from(x & 0x7F) << s;
        }
        s += 7;
        if x & 0x80 == 0 {
            return Ok((r, p));
        }
    }
}

fn sleb(b: &[u8], mut p: usize) -> Result<(i64, usize), Error> {
    let (mut r, mut s) = (0i64, 0u32);
    loop {
        let x = byte(b, p)?;
        p += 1;
        if s < 64 {
            r |= i64::from(x & 0x7F) << s;
        }
        s += 7;
        if x & 0x80 == 0 {
            if x & 0x40 != 0 && s < 64 {
                r -= 1i64 << s;
            }
            return Ok((r, p));
        }
    }
}

/// Read an encoded pointer at `p`; returns (value, new p).
fn read(b: &[u8], p: usize, enc: u8, datarel: u64) -> Result<(u64, usize), Error> {
    if enc == OMIT {
        return Ok((0, p));
    }
    let at = p as u64;
    let (v, p): (u64, usize) = match enc & 0x0F {
        ABSPTR | UDATA8 => (u64::from_le_bytes(bytes(b, p)?), p + 8),
        SDATA8 => (i64::from_le_bytes(bytes(b, p)?) as u64, p + 8),
        UDATA4 => (u64::from(u32::from_le_bytes(bytes(b, p)?)), p + 4),
        SDATA4 => (i64::from(i32::from_le_bytes(bytes(b, p)?)) as u64, p + 4),
        UDATA2 => (u64::from(u16::from_le_bytes(bytes(b, p)?)), p + 2),
        SDATA2 => (i64::from(i16::from_le_bytes(bytes(b, p)?)) as u64, p + 2),
        ULEB128 => uleb(b, p)?,
        SLEB128 => {
            let (v, q) = sleb(b, p)?;
            (v as u64, q)
        }
        _ => return Err(bad(&format!("pointer encoding {enc:#x}"), p)),
    };
    let v = match enc & 0x70 {
        0 => v,
        PCREL => v.wrapping_add(at),
        DATAREL => v.wrapping_add(datarel),
        _ => return Err(bad(&format!("pointer application {enc:#x}"), p)),
    };
    Ok((v, p))
}

/// Parse the CIE at `p`: (fde pointer encoding, lsda encoding, personality).
fn cie(b: &[u8], p: usize) -> Result<(u8, u8, u64), Error> {
    let mut q = p + 8; // length + CIE id
    let version = byte(b, q)?;
    q += 1;
    let end = b[q.min(b.len())..]
        .iter()
        .position(|&c| c == 0)
        .map(|i| q + i)
        .ok_or_else(|| bad("unterminated augmentation", q))?;
    let aug = b[q..end].to_vec();
    q = end + 1;
    q = uleb(b, q)?.1; // code alignment
    q = sleb(b, q)?.1; // data alignment
    if version == 1 {
        q += 1;
    } else {
        q = uleb(b, q)?.1;
    }
    let (mut fde_enc, mut lsda_enc, mut pers) = (ABSPTR, OMIT, 0u64);
    if aug.first() == Some(&b'z') {
        q = uleb(b, q)?.1;
        for &c in &aug[1..] {
            match c {
                b'R' => {
                    fde_enc = byte(b, q)?;
                    q += 1;
                }
                b'L' => {
                    lsda_enc = byte(b, q)?;
                    q += 1;
                }
                b'P' => {
                    let penc = byte(b, q)?;
                    let (v, nq) = read(b, q + 1, penc, 0)?;
                    pers = v;
                    q = nq;
                }
                b'S' | b'B' => {}
                _ => break,
            }
        }
    }
    Ok((fde_enc, lsda_enc, pers))
}

/// All FDEs of the module, sorted by start address (stable, like Python's sort).
pub fn parse(m: &Module) -> Result<Vec<Fde>, Error> {
    let b = &m.image[..];
    let hdr = m.eh_frame_hdr.0;
    if hdr == 0 {
        return Ok(Vec::new());
    }
    let h = hdr as usize;
    let [version, frame_enc, count_enc, table_enc] = bytes::<4>(b, h)?;
    if version != 1 {
        return Err(bad(&format!("eh_frame_hdr version {version}"), h));
    }
    let p = h + 4;
    let (_, p) = read(b, p, frame_enc, 0)?;
    let (count, mut p) = read(b, p, count_enc, 0)?;
    let mut cies: std::collections::HashMap<u64, (u8, u8, u64)> = Default::default();
    let mut fdes = Vec::new();
    for _ in 0..count {
        let (_, np) = read(b, p, table_enc, hdr)?;
        let (fde, np) = read(b, np, table_enc, hdr)?;
        p = np;
        let f = fde as usize;
        let back = u32::from_le_bytes(bytes(b, f + 4)?);
        let cie_ptr = fde.wrapping_add(4).wrapping_sub(u64::from(back));
        let (fde_enc, lsda_enc, personality) = match cies.get(&cie_ptr) {
            Some(c) => *c,
            None => {
                let c = cie(b, cie_ptr as usize)?;
                cies.insert(cie_ptr, c);
                c
            }
        };
        let q = f + 8;
        let (start, q) = read(b, q, fde_enc, 0)?;
        let (size, mut q) = read(b, q, fde_enc & 0x0F, 0)?;
        let mut lsda = 0;
        if lsda_enc != OMIT {
            q = uleb(b, q)?.1;
            lsda = read(b, q, lsda_enc, 0)?.0;
        }
        fdes.push(Fde {
            start,
            size,
            lsda,
            personality,
        });
    }
    fdes.sort_by_key(|f| f.start);
    Ok(fdes)
}
