//! The host-font button icons, following tools/fonts/make_button_font.py.
use super::geometry::{cubic, Vec2};
use super::truetype::{Font, Outline, Point};
use crate::{Error, Result};
const K: f64 = 0.5522847498;
fn v(x: f64, y: f64) -> Vec2 {
    Vec2(x, y)
}
fn fail() -> Error {
    Error::Io("the host font is missing a required letter".to_owned())
}
#[derive(Default)]
struct Pen {
    outline: Outline,
}
impl Pen {
    fn poly(&mut self, pts: &[(f64, f64)], hole: bool) {
        let mut c: Vec<_> = pts
            .iter()
            .map(|&(x, y)| Point {
                x,
                y,
                on_curve: true,
            })
            .collect();
        if hole {
            c[1..].reverse()
        }
        self.outline.contours.push(c)
    }
    fn shape(&mut self, start: Vec2, segments: &[[Vec2; 3]], hole: bool) -> Result<()> {
        let mut c = vec![start.point(true)];
        let mut p = start;
        for s in segments {
            c.extend(cubic([p, s[0], s[1], s[2]])?);
            p = s[2]
        }
        if c.last() == c.first() {
            c.pop();
        }
        if hole {
            c[1..].reverse();
        }
        self.outline.contours.push(c);
        Ok(())
    }
    fn circle(&mut self, cx: f64, cy: f64, r: f64, hole: bool) -> Result<()> {
        let k = K * r;
        self.shape(
            v(cx + r, cy),
            &[
                [v(cx + r, cy - k), v(cx + k, cy - r), v(cx, cy - r)],
                [v(cx - k, cy - r), v(cx - r, cy - k), v(cx - r, cy)],
                [v(cx - r, cy + k), v(cx - k, cy + r), v(cx, cy + r)],
                [v(cx + k, cy + r), v(cx + r, cy + k), v(cx + r, cy)],
            ],
            hole,
        )
    }
    fn rrect(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, r: f64) -> Result<()> {
        let r = r.min((x1 - x0) / 2.).min((y1 - y0) / 2.);
        let k = K * r;
        let mut c = vec![v(x0, y0 + r).point(true), v(x0, y1 - r).point(true)];
        let append = |c: &mut Vec<Point>, p: Vec2, a: Vec2, b: Vec2, e: Vec2| {
            c.extend(cubic([p, a, b, e])?);
            Ok::<_, Error>(())
        };
        append(
            &mut c,
            v(x0, y1 - r),
            v(x0, y1 - r + k),
            v(x0 + r - k, y1),
            v(x0 + r, y1),
        )?;
        c.push(v(x1 - r, y1).point(true));
        append(
            &mut c,
            v(x1 - r, y1),
            v(x1 - r + k, y1),
            v(x1, y1 - r + k),
            v(x1, y1 - r),
        )?;
        c.push(v(x1, y0 + r).point(true));
        append(
            &mut c,
            v(x1, y0 + r),
            v(x1, y0 + r - k),
            v(x1 - r + k, y0),
            v(x1 - r, y0),
        )?;
        c.push(v(x0 + r, y0).point(true));
        append(
            &mut c,
            v(x0 + r, y0),
            v(x0 + r - k, y0),
            v(x0, y0 + r - k),
            v(x0, y0 + r),
        )?;
        if c.last() == c.first() {
            c.pop();
        }
        self.outline.contours.push(c);
        Ok(())
    }
}
struct Builder<'a> {
    font: &'a Font,
    upm: f64,
    cap: f64,
    size: f64,
    cy: f64,
    side: f64,
}
impl<'a> Builder<'a> {
    fn text_width(&self, s: &str, h: f64) -> Result<f64> {
        let mut sum = 0.;
        for c in s.chars() {
            sum += self.font.advance(self.font.glyph_id(c).ok_or_else(fail)?)? as f64
        }
        Ok(h / self.cap * (sum - 0.02 * self.upm * (s.len() - 1) as f64))
    }
    fn text(&self, pen: &mut Pen, s: &str, cx: f64, cy: f64, h: f64) -> Result<()> {
        let scale = h / self.cap;
        let mut x = -self.text_width(s, h)? / (2. * scale);
        for ch in s.chars() {
            let g = self.font.glyph_id(ch).ok_or_else(fail)?;
            let outline = self.font.outline(g)?;
            let x_offset = self.font.side_bearing(g)? as f64
                - self.font.bounds(g)?.map_or(0., |b| b.x_min as f64);
            for mut c in outline.contours {
                for p in &mut c {
                    p.x = cx + x * scale + (p.x + x_offset) * scale;
                    p.y = cy - self.cap * scale / 2. + p.y * scale;
                }
                if c.len() > 1 {
                    c[1..].reverse()
                }
                pen.outline.contours.push(c)
            }
            x += self.font.advance(g)? as f64 - 0.02 * self.upm;
        }
        Ok(())
    }
    fn build(&self, kind: &str, arg: &str) -> Result<(Outline, u16)> {
        let mut p = Pen::default();
        let s = self.size;
        let cy = self.cy;
        let side = self.side;
        let adv = match kind {
            "circle_letter" => {
                let r = s / 2.;
                let cx = side + r;
                p.circle(cx, cy, r, false)?;
                self.text(&mut p, arg, cx, cy, 0.58 * s)?;
                s + 2. * side
            }
            "circle_sign" => {
                let cx = side + s / 2.;
                p.circle(cx, cy, s / 2., false)?;
                let a = 0.30 * s;
                let t = 0.075 * s;
                let pts = if arg == "+" {
                    cross(cx, cy, a, t)
                } else {
                    vec![
                        (cx - a, cy + t),
                        (cx + a, cy + t),
                        (cx + a, cy - t),
                        (cx - a, cy - t),
                    ]
                };
                p.poly(&pts, true);
                s + 2. * side
            }
            "shoulder" | "trigger" | "rail" => {
                let h = if kind == "rail" { 0.62 * s } else { 0.78 * s };
                let cap = if kind == "rail" { 0.40 * s } else { 0.50 * s };
                let min = if kind == "trigger" { 1.45 * s } else { 1.2 * s };
                let w = (self.text_width(arg, cap)? + 0.42 * s).max(min);
                let x0 = side;
                let y0 = cy - h / 2.;
                let x1 = side + w;
                let y1 = cy + h / 2.;
                let r = match kind {
                    "shoulder" => 0.34 * h,
                    "trigger" => 0.18 * h,
                    _ => 0.5 * h,
                };
                p.rrect(x0, y0, x1, y1, r)?;
                self.text(&mut p, arg, (x0 + x1) / 2., cy, cap)?;
                w + 2. * side
            }
            "dpad" => {
                let cx = side + s / 2.;
                let a = s / 2.;
                let t = 0.18 * s;
                let ring = 0.07 * s;
                p.poly(&cross(cx, cy, a, t), false);
                if arg == "all" {
                    p.circle(cx, cy, 0.09 * s, true)?;
                } else {
                    p.poly(&cross(cx, cy, a - ring, t - ring), true);
                    let arm = [
                        (t * 0.3, t - ring),
                        (a - ring, t - ring),
                        (a - ring, -(t - ring)),
                        (t * 0.3, -(t - ring)),
                    ];
                    let deg = match arg {
                        "right" => 0.,
                        "up" => 90.,
                        "left" => 180.,
                        _ => 270.,
                    };
                    p.poly(&rot(&arm, deg, cx, cy), false)
                }
                s + 2. * side
            }
            "stick" => {
                let r = s / 2.;
                let cx = side + r;
                p.circle(cx, cy, r, false)?;
                p.circle(cx, cy, r - 0.08 * s, true)?;
                p.circle(cx, cy, 0.33 * s, false)?;
                if !arg.is_empty() {
                    self.text(&mut p, arg, cx, cy, 0.36 * s)?
                }
                s + 2. * side
            }
            "arrow" => {
                let l = 0.8 * s;
                let cx = side + l / 2.;
                let hl = 0.45 * l;
                let hw = 0.42 * l;
                let sw = 0.13 * l;
                let pts = [
                    (-l / 2., sw),
                    (l / 2. - hl, sw),
                    (l / 2. - hl, hw),
                    (l / 2., 0.),
                    (l / 2. - hl, -hw),
                    (l / 2. - hl, -sw),
                    (-l / 2., -sw),
                ];
                p.poly(&rot(&pts, arg.parse().map_err(|_| fail())?, cx, cy), false);
                l + 2. * side
            }
            "triangle" => {
                let l = 0.45 * s;
                let cx = side + l / 2.;
                let pts = [(-l / 2., 0.55 * l), (l / 2., 0.), (-l / 2., -0.55 * l)];
                p.poly(&rot(&pts, arg.parse().map_err(|_| fail())?, cx, cy), false);
                l + 2. * side
            }
            _ => return Err(fail()),
        };
        Ok((p.outline, adv.round_ties_even() as u16))
    }
}
fn cross(cx: f64, cy: f64, a: f64, t: f64) -> Vec<(f64, f64)> {
    vec![
        (cx - t, cy + a),
        (cx + t, cy + a),
        (cx + t, cy + t),
        (cx + a, cy + t),
        (cx + a, cy - t),
        (cx + t, cy - t),
        (cx + t, cy - a),
        (cx - t, cy - a),
        (cx - t, cy - t),
        (cx - a, cy - t),
        (cx - a, cy + t),
        (cx - t, cy + t),
    ]
}
fn rot(pts: &[(f64, f64)], deg: f64, cx: f64, cy: f64) -> Vec<(f64, f64)> {
    let (s, c) = deg.to_radians().sin_cos();
    pts.iter()
        .map(|&(x, y)| (cx + x * c - y * s, cy + x * s + y * c))
        .collect()
}
/// Add the extension glyphs to a local TrueType font.
pub fn build(base: &[u8]) -> Result<Vec<u8>> {
    let mut font = Font::parse(base)?;
    let upm = font.units_per_em()? as f64;
    let h = font.glyph_id('H').ok_or_else(fail)?;
    let cap = font
        .tight_bounds(h)?
        .map_or(0.716 * upm, |b| b.y_max as f64);
    if cap <= 0. {
        return Err(fail());
    }
    let size = cap + 0.12 * upm;
    let cy = cap / 2.;
    let side = 0.07 * upm;
    let mut generated = Vec::new();
    {
        let b = Builder {
            font: &font,
            upm,
            cap,
            size,
            cy,
            side,
        };
        for (cp, kind, arg) in icons() {
            let (outline, advance) = b.build(kind, arg)?;
            generated.push((char::from_u32(cp).unwrap(), outline, advance));
        }
    }
    for (cp, o, a) in generated {
        font.add(cp, o, a)?;
    }
    font.finish()
}
fn icons() -> Vec<(u32, &'static str, &'static str)> {
    let mut x = Vec::new();
    for base in [0xe0e0, 0xe0a0] {
        for (i, (kind, arg)) in [
            ("circle_letter", "A"),
            ("circle_letter", "B"),
            ("circle_letter", "X"),
            ("circle_letter", "Y"),
            ("shoulder", "L"),
            ("shoulder", "R"),
            ("trigger", "ZL"),
            ("trigger", "ZR"),
            ("rail", "SL"),
            ("rail", "SR"),
        ]
        .into_iter()
        .enumerate()
        {
            x.push((base + i as u32, kind, arg));
        }
    }
    x.extend([
        (0xe0ef, "circle_sign", "+"),
        (0xe0f0, "circle_sign", "-"),
        (0xe0ab, "dpad", "right"),
        (0xe0ac, "dpad", "down"),
        (0xe0ad, "dpad", "up"),
        (0xe0ae, "dpad", "left"),
        (0xe0ea, "dpad", "all"),
        (0xe100, "stick", ""),
        (0xe101, "stick", "L"),
        (0xe090, "arrow", "0"),
        (0xe091, "arrow", "180"),
        (0xe092, "arrow", "90"),
        (0xe093, "arrow", "270"),
        (0xe094, "arrow", "45"),
        (0xe095, "arrow", "315"),
        (0xe096, "arrow", "225"),
        (0xe097, "arrow", "135"),
        (0xe145, "triangle", "180"),
        (0xe146, "triangle", "0"),
    ]);
    x
}
