//! TrueType outline editing for locally prepared fonts.
use crate::{Error, Result};
use std::collections::BTreeMap;
use ttf_parser::{Face, GlyphId, OutlineBuilder};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
    pub on_curve: bool,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outline {
    pub contours: Vec<Vec<Point>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
}

fn invalid() -> Error {
    Error::Io("invalid or unsupported TrueType font".to_owned())
}
fn u16at(b: &[u8], n: usize) -> Result<u16> {
    Ok(u16::from_be_bytes(
        b.get(n..n + 2).ok_or_else(invalid)?.try_into().unwrap(),
    ))
}
fn u32at(b: &[u8], n: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(
        b.get(n..n + 4).ok_or_else(invalid)?.try_into().unwrap(),
    ))
}
fn put16(b: &mut [u8], n: usize, v: u16) {
    b[n..n + 2].copy_from_slice(&v.to_be_bytes());
}
fn put32(b: &mut [u8], n: usize, v: u32) {
    b[n..n + 4].copy_from_slice(&v.to_be_bytes());
}
fn push16(b: &mut Vec<u8>, v: u16) {
    b.extend_from_slice(&v.to_be_bytes());
}
fn push32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_be_bytes());
}

#[derive(Default)]
struct Collector(Outline, bool);
impl OutlineBuilder for Collector {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.contours.push(vec![Point {
            x: x.into(),
            y: y.into(),
            on_curve: true,
        }]);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.contours.last_mut().unwrap().push(Point {
            x: x.into(),
            y: y.into(),
            on_curve: true,
        });
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.contours.last_mut().unwrap().push(Point {
            x: x1.into(),
            y: y1.into(),
            on_curve: false,
        });
        self.line_to(x, y);
    }
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {
        self.1 = true;
    }
    fn close(&mut self) {
        let c = self.0.contours.last_mut().unwrap();
        if c.len() > 1 && c.first() == c.last() {
            c.pop();
        }
    }
}

/// An owned font with edits accumulated until serialization.
pub struct Font {
    data: Vec<u8>,
    tables: BTreeMap<[u8; 4], Vec<u8>>,
    edits: BTreeMap<u16, (Outline, u16)>,
    added: BTreeMap<char, u16>,
    count: u16,
}
impl Font {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let face = Face::parse(data, 0).map_err(|_| invalid())?;
        let count = face.number_of_glyphs();
        if data.get(..4) != Some(&[0, 1, 0, 0]) {
            return Err(invalid());
        }
        let n = u16at(data, 4)? as usize;
        let mut tables = BTreeMap::new();
        for i in 0..n {
            let p = 12 + i * 16;
            let tag: [u8; 4] = data.get(p..p + 4).ok_or_else(invalid)?.try_into().unwrap();
            let offset = u32at(data, p + 8)? as usize;
            let len = u32at(data, p + 12)? as usize;
            let end = offset.checked_add(len).ok_or_else(invalid)?;
            tables.insert(tag, data.get(offset..end).ok_or_else(invalid)?.to_vec());
        }
        for tag in [
            b"head", b"hhea", b"hmtx", b"maxp", b"loca", b"glyf", b"cmap",
        ] {
            if !tables.contains_key(tag) {
                return Err(invalid());
            }
        }
        Ok(Self {
            data: data.to_vec(),
            tables,
            edits: BTreeMap::new(),
            added: BTreeMap::new(),
            count,
        })
    }
    fn face(&self) -> Result<Face<'_>> {
        Face::parse(&self.data, 0).map_err(|_| invalid())
    }
    pub fn units_per_em(&self) -> Result<u16> {
        Ok(self.face()?.units_per_em())
    }
    pub fn glyph_id(&self, c: char) -> Option<u16> {
        self.added
            .get(&c)
            .copied()
            .or_else(|| self.face().ok()?.glyph_index(c).map(|g| g.0))
    }
    pub fn advance(&self, g: u16) -> Result<u16> {
        if let Some((_, a)) = self.edits.get(&g) {
            return Ok(*a);
        }
        self.face()?
            .glyph_hor_advance(GlyphId(g))
            .ok_or_else(invalid)
    }
    pub fn side_bearing(&self, g: u16) -> Result<i16> {
        if let Some((o, _)) = self.edits.get(&g) {
            return Ok(outline_bounds(o)?.map_or(0, |b| b.x_min));
        }
        self.face()?
            .glyph_hor_side_bearing(GlyphId(g))
            .ok_or_else(invalid)
    }
    pub fn tight_bounds(&self, g: u16) -> Result<Option<Bounds>> {
        Ok(self.face()?.glyph_bounding_box(GlyphId(g)).map(|b| Bounds {
            x_min: b.x_min,
            y_min: b.y_min,
            x_max: b.x_max,
            y_max: b.y_max,
        }))
    }
    pub fn bounds(&self, g: u16) -> Result<Option<Bounds>> {
        if let Some((o, _)) = self.edits.get(&g) {
            return outline_bounds(o);
        }
        let b = self.glyph_bytes(g)?;
        if b.is_empty() {
            return Ok(None);
        }
        Ok(Some(Bounds {
            x_min: u16at(b, 2)? as i16,
            y_min: u16at(b, 4)? as i16,
            x_max: u16at(b, 6)? as i16,
            y_max: u16at(b, 8)? as i16,
        }))
    }
    fn glyph_bytes(&self, g: u16) -> Result<&[u8]> {
        if g >= self.face()?.number_of_glyphs() {
            return Err(invalid());
        }
        let loca = self.tables.get(b"loca").ok_or_else(invalid)?;
        let glyf = self.tables.get(b"glyf").ok_or_else(invalid)?;
        let long = u16at(self.tables.get(b"head").ok_or_else(invalid)?, 50)? != 0;
        let offset = |i: usize| -> Result<usize> {
            if long {
                Ok(u32at(loca, i * 4)? as usize)
            } else {
                Ok(u16at(loca, i * 2)? as usize * 2)
            }
        };
        glyf.get(offset(g as usize)?..offset(g as usize + 1)?)
            .ok_or_else(invalid)
    }
    pub fn outline(&self, g: u16) -> Result<Outline> {
        if let Some((o, _)) = self.edits.get(&g) {
            return Ok(o.clone());
        }
        if let Some(outline) = self.simple_outline(g)? {
            return Ok(outline);
        }
        let mut c = Collector::default();
        self.face()?.outline_glyph(GlyphId(g), &mut c);
        if c.1 {
            return Err(invalid());
        }
        Ok(c.0)
    }
    fn simple_outline(&self, g: u16) -> Result<Option<Outline>> {
        let b = self.glyph_bytes(g)?;
        if b.is_empty() {
            return Ok(Some(Outline::default()));
        }
        let n = u16at(b, 0)? as i16;
        if n < 0 {
            return Ok(None);
        }
        let n = n as usize;
        if n == 0 {
            return Ok(Some(Outline::default()));
        }
        let mut ends = Vec::with_capacity(n);
        for i in 0..n {
            ends.push(u16at(b, 10 + i * 2)? as usize)
        }
        let count = ends.last().copied().ok_or_else(invalid)? + 1;
        let mut pos = 10 + n * 2;
        let instructions = u16at(b, pos)? as usize;
        pos = pos.checked_add(2 + instructions).ok_or_else(invalid)?;
        let mut flags = Vec::with_capacity(count);
        while flags.len() < count {
            let f = *b.get(pos).ok_or_else(invalid)?;
            pos += 1;
            flags.push(f);
            if f & 8 != 0 {
                let repeat = *b.get(pos).ok_or_else(invalid)? as usize;
                pos += 1;
                if flags.len() + repeat > count {
                    return Err(invalid());
                }
                flags.extend(std::iter::repeat_n(f, repeat));
            }
        }
        let mut coords = vec![(0i32, 0i32); count];
        for axis in 0..2 {
            let mut value = 0i32;
            for (i, &f) in flags.iter().enumerate() {
                let short = if axis == 0 { 2 } else { 4 };
                let same = if axis == 0 { 16 } else { 32 };
                let delta = if f & short != 0 {
                    let v = *b.get(pos).ok_or_else(invalid)? as i32;
                    pos += 1;
                    if f & same != 0 {
                        v
                    } else {
                        -v
                    }
                } else if f & same != 0 {
                    0
                } else {
                    let v = u16at(b, pos)? as i16 as i32;
                    pos += 2;
                    v
                };
                value = value.checked_add(delta).ok_or_else(invalid)?;
                if axis == 0 {
                    coords[i].0 = value
                } else {
                    coords[i].1 = value
                }
            }
        }
        let mut contours = Vec::with_capacity(n);
        let mut from = 0;
        for end in ends {
            if end < from || end >= count {
                return Err(invalid());
            }
            let mut c = Vec::new();
            for i in from..=end {
                c.push(Point {
                    x: coords[i].0 as f64,
                    y: coords[i].1 as f64,
                    on_curve: flags[i] & 1 != 0,
                })
            }
            contours.push(c);
            from = end + 1;
        }
        Ok(Some(Outline { contours }))
    }
    pub fn replace(&mut self, g: u16, o: Outline, advance: u16) -> Result<()> {
        if g >= self.count {
            return Err(invalid());
        }
        outline_bounds(&o)?;
        self.edits.insert(g, (o, advance));
        Ok(())
    }
    pub fn add(&mut self, c: char, o: Outline, advance: u16) -> Result<u16> {
        let g = self.count;
        self.count = self.count.checked_add(1).ok_or_else(invalid)?;
        self.replace(g, o, advance)?;
        self.added.insert(c, g);
        Ok(g)
    }
    pub fn finish(mut self) -> Result<Vec<u8>> {
        let old_count = self.face()?.number_of_glyphs() as usize;
        let head = self.tables.get(b"head").ok_or_else(invalid)?;
        let long = u16at(head, 50)? != 0;
        let loca = self.tables.get(b"loca").ok_or_else(invalid)?;
        let glyf = self.tables.get(b"glyf").ok_or_else(invalid)?;
        let offset = |i: usize| -> Result<usize> {
            if long {
                Ok(u32at(loca, i * 4)? as usize)
            } else {
                Ok(u16at(loca, i * 2)? as usize * 2)
            }
        };
        let mut new_glyf = Vec::new();
        let mut new_loca = Vec::new();
        let mut hmtx = Vec::new();
        let mut max_points = 0u16;
        let mut max_contours = 0u16;
        let mut global: Option<Bounds> = None;
        let mut advance_max = 0u16;
        let mut min_lsb = i32::MAX;
        let mut min_rsb = i32::MAX;
        let mut max_extent = i32::MIN;
        for i in 0..self.count as usize {
            push32(
                &mut new_loca,
                u32::try_from(new_glyf.len()).map_err(|_| invalid())?,
            );
            if let Some((outline, _)) = self.edits.get(&(i as u16)) {
                let (g, p, c) = encode_outline(outline)?;
                max_points = max_points.max(p);
                max_contours = max_contours.max(c);
                new_glyf.extend(g);
            } else if i < old_count {
                let a = offset(i)?;
                let b = offset(i + 1)?;
                new_glyf.extend_from_slice(glyf.get(a..b).ok_or_else(invalid)?);
            }
            while new_glyf.len() % 4 != 0 {
                new_glyf.push(0)
            }
            let adv = self.advance(i as u16)?;
            let lsb = if let Some((o, _)) = self.edits.get(&(i as u16)) {
                outline_bounds(o)?.map_or(0, |b| b.x_min)
            } else {
                self.face()?
                    .glyph_hor_side_bearing(GlyphId(i as u16))
                    .ok_or_else(invalid)?
            };
            if let Some(b) = self.bounds(i as u16)? {
                if let Some(ref mut all) = global {
                    all.x_min = all.x_min.min(b.x_min);
                    all.y_min = all.y_min.min(b.y_min);
                    all.x_max = all.x_max.max(b.x_max);
                    all.y_max = all.y_max.max(b.y_max);
                } else {
                    global = Some(b);
                }
                let extent = lsb as i32 + b.x_max as i32 - b.x_min as i32;
                min_lsb = min_lsb.min(lsb as i32);
                min_rsb = min_rsb.min(adv as i32 - extent);
                max_extent = max_extent.max(extent);
            }
            advance_max = advance_max.max(adv);
            push16(&mut hmtx, adv);
            push16(&mut hmtx, lsb as u16);
        }
        push32(
            &mut new_loca,
            u32::try_from(new_glyf.len()).map_err(|_| invalid())?,
        );
        if !self.added.is_empty() {
            let cmap = build_cmap(&self)?;
            self.tables.insert(*b"cmap", cmap);
        }
        self.tables.insert(*b"glyf", new_glyf);
        self.tables.insert(*b"loca", new_loca);
        self.tables.insert(*b"hmtx", hmtx);
        let head = self.tables.get_mut(b"head").unwrap();
        if head.len() < 54 {
            return Err(invalid());
        }
        if let Some(b) = global {
            for (at, v) in [(36, b.x_min), (38, b.y_min), (40, b.x_max), (42, b.y_max)] {
                put16(head, at, v as u16)
            }
        }
        put16(head, 50, 1);
        put32(head, 8, 0);
        let hhea = self.tables.get_mut(b"hhea").unwrap();
        if hhea.len() < 36 {
            return Err(invalid());
        }
        put16(hhea, 10, advance_max);
        if global.is_some() {
            for (at, v) in [(12, min_lsb), (14, min_rsb), (16, max_extent)] {
                put16(hhea, at, i16::try_from(v).map_err(|_| invalid())? as u16)
            }
        }
        put16(hhea, 34, self.count);
        let maxp = self.tables.get_mut(b"maxp").unwrap();
        if maxp.len() < 10 {
            return Err(invalid());
        }
        put16(maxp, 4, self.count);
        let p = u16at(maxp, 6)?.max(max_points);
        let c = u16at(maxp, 8)?.max(max_contours);
        put16(maxp, 6, p);
        put16(maxp, 8, c);
        for tag in [b"DSIG", b"hdmx", b"LTSH", b"VDMX"] {
            self.tables.remove(tag);
        }
        // Format 3 has no glyph-name array to become inconsistent when adding glyphs.
        if !self.added.is_empty() {
            if let Some(post) = self.tables.get_mut(b"post") {
                if post.len() >= 32 {
                    post.truncate(32);
                    put32(post, 0, 0x0003_0000);
                }
            }
        }
        serialize(&self.tables)
    }
}
fn coord(v: f64) -> Result<i16> {
    if !v.is_finite() || (v + 0.5).floor() < i16::MIN as f64 || (v + 0.5).floor() > i16::MAX as f64
    {
        return Err(invalid());
    }
    Ok((v + 0.5).floor() as i16)
}
fn outline_bounds(o: &Outline) -> Result<Option<Bounds>> {
    let mut bounds: Option<Bounds> = None;
    for p in o.contours.iter().flatten() {
        let x = coord(p.x)?;
        let y = coord(p.y)?;
        if let Some(b) = &mut bounds {
            b.x_min = b.x_min.min(x);
            b.x_max = b.x_max.max(x);
            b.y_min = b.y_min.min(y);
            b.y_max = b.y_max.max(y)
        } else {
            bounds = Some(Bounds {
                x_min: x,
                y_min: y,
                x_max: x,
                y_max: y,
            })
        }
    }
    Ok(bounds)
}
fn encode_outline(o: &Outline) -> Result<(Vec<u8>, u16, u16)> {
    let count = u16::try_from(o.contours.len()).map_err(|_| invalid())?;
    if count > i16::MAX as u16 {
        return Err(invalid());
    }
    let mut out = Vec::new();
    let Some(b) = outline_bounds(o)? else {
        return Ok((out, 0, 0));
    };
    push16(&mut out, count);
    for v in [b.x_min, b.y_min, b.x_max, b.y_max] {
        push16(&mut out, v as u16)
    }
    let mut points = 0usize;
    for c in &o.contours {
        if c.is_empty() {
            return Err(invalid());
        }
        points = points.checked_add(c.len()).ok_or_else(invalid)?;
        push16(&mut out, u16::try_from(points - 1).map_err(|_| invalid())?)
    }
    push16(&mut out, 0);
    for p in o.contours.iter().flatten() {
        out.push(if p.on_curve { 1 } else { 0 })
    }
    let mut prev = 0i32;
    for p in o.contours.iter().flatten() {
        let v = coord(p.x)? as i32;
        let delta = i16::try_from(v - prev).map_err(|_| invalid())?;
        push16(&mut out, delta as u16);
        prev = v
    }
    prev = 0;
    for p in o.contours.iter().flatten() {
        let v = coord(p.y)? as i32;
        let delta = i16::try_from(v - prev).map_err(|_| invalid())?;
        push16(&mut out, delta as u16);
        prev = v
    }
    Ok((out, u16::try_from(points).map_err(|_| invalid())?, count))
}
fn build_cmap(font: &Font) -> Result<Vec<u8>> {
    let face = font.face()?;
    let mut mapping = BTreeMap::new();
    if let Some(cmap) = face.tables().cmap {
        for sub in cmap.subtables {
            if sub.is_unicode() {
                sub.codepoints(|c| {
                    if let Some(ch) = char::from_u32(c) {
                        if let Some(g) = face.glyph_index(ch) {
                            mapping.insert(c, g.0);
                        }
                    }
                });
            }
        }
    }
    for (&c, &g) in &font.added {
        mapping.insert(c as u32, g);
    }
    // Format 4 uses delta-only segments, preserving every BMP mapping.
    let mut segments: Vec<(u16, u16, u16)> = Vec::new();
    for (&c, &g) in mapping.range(..0xffff) {
        let cp = c as u16;
        let delta = g.wrapping_sub(cp);
        if let Some(last) = segments.last_mut() {
            if last.1.checked_add(1) == Some(cp) && last.2 == delta {
                last.1 = cp;
                continue;
            }
        }
        segments.push((cp, cp, delta));
    }
    segments.push((0xffff, 0xffff, 1));
    let n = u16::try_from(segments.len()).map_err(|_| invalid())?;
    let length = 16usize
        .checked_add(segments.len() * 8)
        .ok_or_else(invalid)?;
    let len = u16::try_from(length).map_err(|_| invalid())?;
    let pow = 1u16 << (15 - n.leading_zeros() as u16);
    let mut f4 = Vec::new();
    for v in [
        4,
        len,
        0,
        n * 2,
        pow * 2,
        pow.trailing_zeros() as u16,
        n * 2 - pow * 2,
    ] {
        push16(&mut f4, v)
    }
    for s in &segments {
        push16(&mut f4, s.1)
    }
    push16(&mut f4, 0);
    for s in &segments {
        push16(&mut f4, s.0)
    }
    for s in &segments {
        push16(&mut f4, s.2)
    }
    for _ in &segments {
        push16(&mut f4, 0)
    }
    let mut groups: Vec<(u32, u32, u32)> = Vec::new();
    for (&c, &g) in &mapping {
        let g = g as u32;
        if let Some(last) = groups.last_mut() {
            if last.1.checked_add(1) == Some(c) && last.2 + (c - last.0) == g {
                last.1 = c;
                continue;
            }
        }
        groups.push((c, c, g));
    }
    let mut f12 = Vec::new();
    push16(&mut f12, 12);
    push16(&mut f12, 0);
    push32(
        &mut f12,
        u32::try_from(16 + groups.len() * 12).map_err(|_| invalid())?,
    );
    push32(&mut f12, 0);
    push32(&mut f12, groups.len() as u32);
    for (a, b, g) in groups {
        push32(&mut f12, a);
        push32(&mut f12, b);
        push32(&mut f12, g)
    }
    let original = font.tables.get(b"cmap").ok_or_else(invalid)?;
    let count = usize::from(u16at(original, 2)?);
    let old_records_end = 4 + 8 * count;
    let old_data = original.get(old_records_end..).ok_or_else(invalid)?;
    // Keep platform-specific encodings and variation selector records.
    // Update Unicode format 4 and 12 records to point to the expanded mappings.
    let mut records = BTreeMap::new();
    records.insert((3, 1), 0);
    records.insert((3, 10), f4.len());
    for i in 0..count {
        let at = 4 + i * 8;
        let platform = u16at(original, at)?;
        let encoding = u16at(original, at + 2)?;
        let offset = u32at(original, at + 4)? as usize;
        let format = u16at(original, offset)?;
        let unicode = platform == 0 || (platform == 3 && matches!(encoding, 1 | 10));
        let destination = match (unicode, format) {
            (true, 4) => 0,
            (true, 12) => f4.len(),
            _ => f4.len() + f12.len() + offset.checked_sub(old_records_end).ok_or_else(invalid)?,
        };
        records.entry((platform, encoding)).or_insert(destination);
    }
    let mut out = Vec::new();
    push16(&mut out, 0);
    push16(
        &mut out,
        u16::try_from(records.len()).map_err(|_| invalid())?,
    );
    let data_start = 4 + records.len() * 8;
    for ((platform, encoding), offset) in records {
        push16(&mut out, platform);
        push16(&mut out, encoding);
        push32(
            &mut out,
            u32::try_from(data_start + offset).map_err(|_| invalid())?,
        );
    }
    out.extend(f4);
    out.extend(f12);
    out.extend(old_data);
    Ok(out)
}

fn checksum(data: &[u8]) -> u32 {
    let mut sum = 0u32;
    for chunk in data.chunks(4) {
        let mut b = [0; 4];
        b[..chunk.len()].copy_from_slice(chunk);
        sum = sum.wrapping_add(u32::from_be_bytes(b))
    }
    sum
}
fn serialize(tables: &BTreeMap<[u8; 4], Vec<u8>>) -> Result<Vec<u8>> {
    let n = u16::try_from(tables.len()).map_err(|_| invalid())?;
    if n == 0 || n > u16::MAX / 16 {
        return Err(invalid());
    }
    let pow = 1u16 << (15 - n.leading_zeros() as u16);
    let mut out = Vec::new();
    push32(&mut out, 0x10000);
    for v in [n, pow * 16, pow.trailing_zeros() as u16, n * 16 - pow * 16] {
        push16(&mut out, v)
    }
    let mut offset = 12 + tables.len() * 16;
    let mut head_offset = None;
    for (tag, data) in tables {
        out.extend(tag);
        push32(&mut out, checksum(data));
        push32(&mut out, u32::try_from(offset).map_err(|_| invalid())?);
        push32(&mut out, u32::try_from(data.len()).map_err(|_| invalid())?);
        if tag == b"head" {
            head_offset = Some(offset)
        }
        offset = offset
            .checked_add((data.len() + 3) & !3)
            .ok_or_else(invalid)?;
    }
    for data in tables.values() {
        out.extend(data);
        while out.len() % 4 != 0 {
            out.push(0)
        }
    }
    let adjustment = 0xb1b0_afbau32.wrapping_sub(checksum(&out));
    put32(&mut out, head_offset.ok_or_else(invalid)? + 8, adjustment);
    Ok(out)
}
