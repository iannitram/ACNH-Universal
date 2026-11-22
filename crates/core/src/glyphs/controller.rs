//! ParkExt drawing rules, ported from tools/make_button_glyphs.py.

pub(super) const FACE: [[u32; 2]; 4] = [
    [0xe0e0, 0xe0a0],
    [0xe0e1, 0xe0a1],
    [0xe0e2, 0xe0a2],
    [0xe0e3, 0xe0a3],
];
const SHOULDER: [[u32; 2]; 4] = [
    [0xe0e4, 0xe0a4],
    [0xe0e5, 0xe0a5],
    [0xe0e6, 0xe0a6],
    [0xe0e7, 0xe0a7],
];
const SYSTEM: [[u32; 4]; 2] = [
    [0xe0ef, 0xe0b3, 0xe0f1, 0xe0b5],
    [0xe0f0, 0xe0b4, 0xe0f2, 0xe0b6],
];
const DPAD: &[(u32, bool, &str, &str)] = &[
    (0xe0aa, false, "", ""),
    (0xe0ab, false, "r", ""),
    (0xe0ac, false, "d", ""),
    (0xe0ad, false, "u", ""),
    (0xe0ae, false, "l", ""),
    (0xe0ea, true, "", ""),
    (0xe0ca, false, "", "rd"),
    (0xe0cb, false, "", "lr"),
    (0xe0cc, false, "", "ur"),
    (0xe0cd, false, "", "ld"),
    (0xe0ce, false, "", "ud"),
    (0xe0cf, false, "", "ul"),
    (0xe23f, true, "", "udlr"),
    (0xe240, false, "r", "r"),
    (0xe241, false, "d", "d"),
    (0xe242, false, "u", "u"),
    (0xe243, false, "l", "l"),
    (0xe0eb, true, "", "u"),
    (0xe0ec, true, "", "d"),
    (0xe0ed, true, "", "l"),
    (0xe0ee, true, "", "r"),
    (0xe0af, false, "", "u"),
    (0xe0b0, false, "", "d"),
    (0xe0b1, false, "", "l"),
    (0xe0b2, false, "", "r"),
];

type XY = (f64, f64);

use super::truetype::Point;

pub(super) type Outline = Vec<Vec<Point>>;

#[derive(Clone, Copy)]
pub(super) struct Bounds(pub f64, pub f64, pub f64, pub f64);

impl Bounds {
    fn width(self) -> f64 {
        self.2 - self.0
    }
    fn height(self) -> f64 {
        self.3 - self.1
    }
    fn center(self) -> XY {
        ((self.0 + self.2) / 2.0, (self.1 + self.3) / 2.0)
    }
    fn radius(self) -> f64 {
        self.width().min(self.height()) / 2.0
    }
    fn inset(self, d: f64) -> Self {
        Self(self.0 + d, self.1 + d, self.2 - d, self.3 - d)
    }

    pub(super) fn from_outline(outline: &Outline) -> Option<Self> {
        let first = outline.iter().flatten().next()?;
        Some(
            outline
                .iter()
                .flatten()
                .fold(Self(first.x, first.y, first.x, first.y), |b, p| {
                    Self(b.0.min(p.x), b.1.min(p.y), b.2.max(p.x), b.3.max(p.y))
                }),
        )
    }

    /// Keep the original glyph's box even when the new outline is circular, or
    /// when Switch A and B come from contours with different proportions.
    pub(super) fn fit(self, outline: &mut Outline) -> bool {
        let Some(source) = Self::from_outline(outline) else {
            return false;
        };
        if source.width() <= 0.0
            || source.height() <= 0.0
            || self.width() <= 0.0
            || self.height() <= 0.0
        {
            return false;
        }
        for p in outline.iter_mut().flatten() {
            p.x = self.0 + (p.x - source.0) / source.width() * self.width();
            p.y = self.1 + (p.y - source.1) / source.height() * self.height();
        }
        true
    }
}

#[derive(Clone, Copy)]
enum Shape {
    Face(&'static str, bool),
    Shoulder(&'static str, bool, bool),
    System(&'static str, Frame),
    Dpad(bool, &'static str, &'static str),
}

#[derive(Clone, Copy, PartialEq)]
enum Frame {
    Disc,
    Ring,
    Bare,
}

pub(super) enum Replacement {
    Swap(u32),
    Draw(ControllerGlyph),
}

pub(super) struct ControllerGlyph(Shape);

pub(super) fn plan(playstation: bool, xbox_layout: bool) -> Vec<(u32, Replacement)> {
    let mut plan = Vec::new();
    let mut labels = if playstation {
        ["circle", "cross", "triangle", "square"]
    } else {
        ["B", "A", "Y", "X"]
    };
    if xbox_layout {
        labels.swap(0, 1);
    }
    for (pair, label) in FACE.into_iter().zip(labels) {
        for (cp, solid) in pair.into_iter().zip([true, false]) {
            plan.push((
                cp,
                Replacement::Draw(ControllerGlyph(Shape::Face(label, solid))),
            ));
        }
    }
    let shoulders = if playstation {
        ["L1", "R1", "L2", "R2"]
    } else {
        ["LB", "RB", "LT", "RT"]
    };
    for (index, (pair, label)) in SHOULDER.into_iter().zip(shoulders).enumerate() {
        for (cp, solid) in pair.into_iter().zip([true, false]) {
            plan.push((
                cp,
                Replacement::Draw(ControllerGlyph(Shape::Shoulder(label, index >= 2, solid))),
            ));
        }
    }
    let system = if playstation {
        ["options", "create"]
    } else {
        ["menu", "view"]
    };
    for (points, name) in SYSTEM.into_iter().zip(system) {
        for (cp, frame) in
            points
                .into_iter()
                .zip([Frame::Disc, Frame::Ring, Frame::Bare, Frame::Bare])
        {
            plan.push((
                cp,
                Replacement::Draw(ControllerGlyph(Shape::System(name, frame))),
            ));
        }
    }
    plan.extend(DPAD.iter().map(|&(cp, solid, lit, arrows)| {
        (
            cp,
            Replacement::Draw(ControllerGlyph(Shape::Dpad(solid, lit, arrows))),
        )
    }));
    plan
}

pub(super) fn switch_plan() -> Vec<(u32, Replacement)> {
    FACE[0]
        .into_iter()
        .zip(FACE[1])
        .flat_map(|(a, b)| [(a, Replacement::Swap(b)), (b, Replacement::Swap(a))])
        .collect()
}

pub(super) struct Label {
    pub advance: f64,
    pub outline: Outline,
}

pub(super) struct Labels(pub f64, pub std::collections::BTreeMap<char, Label>);

impl Labels {
    fn width(&self, text: &str, height: f64) -> f64 {
        let gap = 0.04 * self.0;
        let width = text.chars().map(|c| self.1[&c].advance - gap).sum::<f64>() + gap;
        width * height / self.0
    }

    fn draw(&self, pen: &mut Pen, text: &str, (cx, cy): XY, height: f64, hole: bool) {
        let scale = height / self.0;
        let mut left = cx - self.width(text, height) / 2.0;
        for c in text.chars() {
            let label = &self.1[&c];
            for contour in &label.outline {
                let mut contour: Vec<Point> = contour
                    .iter()
                    .map(|p| Point {
                        x: left + p.x * scale,
                        y: cy - height / 2.0 + p.y * scale,
                        on_curve: p.on_curve,
                    })
                    .collect();
                if hole {
                    contour.reverse();
                }
                pen.0.push(contour);
            }
            left += (label.advance - 0.04 * self.0) * scale;
        }
    }
}

#[derive(Default)]
struct Pen(Outline);

fn point((x, y): XY, on_curve: bool) -> Point {
    Point { x, y, on_curve }
}

impl Pen {
    fn push(&mut self, mut points: Vec<Point>, hole: bool) {
        points.dedup_by(|a, b| a.x == b.x && a.y == b.y && a.on_curve == b.on_curve);
        if points.len() > 1 {
            let first = points[0];
            let last = points[points.len() - 1];
            if first.x == last.x && first.y == last.y && first.on_curve == last.on_curve {
                points.pop();
            }
        }
        if hole {
            points.reverse();
        }
        self.0.push(points);
    }

    fn polygon(&mut self, points: impl IntoIterator<Item = XY>, hole: bool) {
        self.push(points.into_iter().map(|p| point(p, true)).collect(), hole);
    }

    fn circle(&mut self, (x, y): XY, r: f64, hole: bool) {
        self.rounded(Bounds(x - r, y - r, x + r, y + r), [r; 4], hole);
    }

    fn rounded(&mut self, Bounds(x0, y0, x1, y1): Bounds, radii: [f64; 4], hole: bool) {
        let mut path = Vec::new();
        let limit = ((x1 - x0).min(y1 - y0) / 2.0).max(0.0);
        let [tl, tr, br, bl] = radii.map(|r| r.clamp(0.0, limit));
        corner(&mut path, (x0, y1 - tl), (x0, y1), (x0 + tl, y1), tl);
        corner(&mut path, (x1 - tr, y1), (x1, y1), (x1, y1 - tr), tr);
        corner(&mut path, (x1, y0 + br), (x1, y0), (x1 - br, y0), br);
        corner(&mut path, (x0 + bl, y0), (x0, y0), (x0, y0 + bl), bl);
        self.push(path, hole);
    }
}

// Two quadratics approximate each octant pair. Intersect the endpoint tangent
// with the diagonal tangent so a circular corner stays smooth at the join.
fn corner(path: &mut Vec<Point>, start: XY, vertex: XY, end: XY, radius: f64) {
    if radius == 0.0 {
        path.push(point(vertex, true));
        return;
    }
    let midpoint = 1.0 - std::f64::consts::FRAC_1_SQRT_2;
    let tangent = 2.0_f64.sqrt() - 1.0;
    let mix = |a: XY, b: XY, t: f64| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
    let middle = (
        vertex.0 + (start.0 + end.0 - 2.0 * vertex.0) * midpoint,
        vertex.1 + (start.1 + end.1 - 2.0 * vertex.1) * midpoint,
    );
    path.extend([
        point(start, true),
        point(mix(start, vertex, tangent), false),
        point(middle, true),
        point(mix(end, vertex, tangent), false),
        point(end, true),
    ]);
}

fn rotate((x, y): XY, degrees: f64, (cx, cy): XY) -> XY {
    let (s, c) = degrees.to_radians().sin_cos();
    (cx + x * c - y * s, cy + x * s + y * c)
}

fn plus(a: f64, t: f64) -> [XY; 12] {
    [
        (-t, a),
        (t, a),
        (t, t),
        (a, t),
        (a, -t),
        (t, -t),
        (t, -a),
        (-t, -a),
        (-t, -t),
        (-a, -t),
        (-a, t),
        (-t, t),
    ]
}

fn angle(direction: char) -> f64 {
    match direction {
        'r' => 0.0,
        'u' => 90.0,
        'l' => 180.0,
        _ => 270.0,
    }
}

impl ControllerGlyph {
    pub(super) fn draw(self, b: Bounds, labels: &Labels) -> Outline {
        let mut pen = Pen::default();
        let center @ (cx, cy) = b.center();
        let r = b.radius();
        match self.0 {
            Shape::Face(label, solid) => {
                pen.circle(center, r, false);
                if !solid {
                    pen.circle(center, 0.83 * r, true);
                }
                match label {
                    "cross" => pen.polygon(
                        plus(0.56 * r, 0.11 * r).map(|p| rotate(p, 45.0, center)),
                        solid,
                    ),
                    "circle" => {
                        pen.circle(center, 0.48 * r, solid);
                        pen.circle(center, 0.28 * r, !solid);
                    }
                    "square" => {
                        let a = 0.44 * r;
                        let rect = Bounds(cx - a, cy - a, cx + a, cy + a);
                        pen.rounded(rect, [0.06 * r; 4], solid);
                        pen.rounded(rect.inset(0.19 * r), [0.0; 4], !solid);
                    }
                    "triangle" => {
                        let triangle = |radius| {
                            [90.0, -30.0, 210.0]
                                .map(|a| rotate((radius, 0.0), a, (cx, cy - 0.04 * r)))
                        };
                        pen.polygon(triangle(0.62 * r), solid);
                        pen.polygon(triangle(0.24 * r), !solid);
                    }
                    _ => labels.draw(&mut pen, label, center, 1.05 * r, solid),
                }
            }
            Shape::Shoulder(label, trigger, solid) => {
                let h = b.height();
                let radii = if trigger {
                    [0.42 * h, 0.42 * h, 0.08 * h, 0.08 * h]
                } else {
                    [0.22 * h; 4]
                };
                let stroke = 0.085 * h;
                pen.rounded(b, radii, false);
                if !solid {
                    pen.rounded(b.inset(stroke), radii.map(|r| (r - stroke).max(0.0)), true);
                }
                let cap = (0.52 * h)
                    .min(0.72 * (b.width() - 2.0 * stroke) / labels.width(label, 1.0).max(1e-6));
                let cy = if trigger { cy - 0.03 * h } else { cy };
                labels.draw(&mut pen, label, (cx, cy), cap, solid);
            }
            Shape::System(name, frame) => system(&mut pen, center, r, name, frame),
            Shape::Dpad(solid, lit, arrows) => {
                let a = r;
                let (w, t, g) = (0.40 * a, 0.13 * a, 0.06 * a);
                pen.polygon(plus(a, w).map(|p| rotate(p, 0.0, center)), false);
                if !solid {
                    pen.polygon(plus(a - t, w - t).map(|p| rotate(p, 0.0, center)), true);
                }
                let lw = w - t - g;
                for d in lit.chars() {
                    let arm = [(lw, lw), (a - t - g, lw), (a - t - g, -lw), (lw, -lw)];
                    pen.polygon(arm.map(|p| rotate(p, angle(d), center)), false);
                }
                for d in arrows.chars() {
                    let hw = if lit.contains(d) {
                        0.62 * lw
                    } else {
                        0.75 * (w - t)
                    };
                    let m = 0.66 * a;
                    let tri = [
                        (m + 0.55 * hw, 0.0),
                        (m - 0.55 * hw, -hw),
                        (m - 0.55 * hw, hw),
                    ];
                    pen.polygon(
                        tri.map(|p| rotate(p, angle(d), center)),
                        solid || lit.contains(d),
                    );
                }
            }
        }
        pen.0
    }
}

fn system(pen: &mut Pen, center @ (cx, cy): XY, mut r: f64, name: &str, frame: Frame) {
    match frame {
        Frame::Disc => pen.circle(center, r, false),
        Frame::Ring => {
            pen.circle(center, r, false);
            pen.circle(center, 0.83 * r, true);
        }
        Frame::Bare => r *= 1.25,
    }
    let mut cut = frame == Frame::Disc;
    let s = 0.5 * r;
    if matches!(name, "options" | "create") {
        let pill = Bounds(cx - 0.82 * s, cy - 1.25 * s, cx + 0.82 * s, cy + 1.25 * s);
        pen.rounded(pill, [0.82 * s; 4], cut);
        cut = !cut;
    }
    match name {
        "menu" | "options" => {
            let half = if name == "menu" { s } else { 0.62 * s };
            let t = 0.13 * r;
            for dy in [-0.62 * s, 0.0, 0.62 * s] {
                pen.rounded(
                    Bounds(cx - half, cy + dy - t / 2.0, cx + half, cy + dy + t / 2.0),
                    [t / 2.0; 4],
                    cut,
                );
            }
        }
        "view" => {
            let (q, d, t, g) = (0.68 * s, 0.5 * s, 0.13 * r, 0.1 * r);
            let (bx, by) = (cx - q - d / 2.0, cy - q + d / 2.0);
            let (fx, fy) = (bx + d, by - d);
            let side = 2.0 * q;
            pen.rounded(Bounds(fx, fy, fx + side, fy + side), [0.05 * r; 4], cut);
            for rect in [
                Bounds(bx, by + side - t, bx + side, by + side),
                Bounds(bx, by, bx + t, by + side - t),
                Bounds(bx + t, by, fx - g, by + t),
                Bounds(bx + side - t, fy + side + g, bx + side, by + side - t),
            ] {
                if rect.width() > 0.0 && rect.height() > 0.0 {
                    pen.rounded(rect, [0.0; 4], cut);
                }
            }
        }
        _ => {
            let t = 0.12 * r;
            let arrow = [
                (cx, cy + 0.85 * s),
                (cx + 0.5 * s, cy + 0.3 * s),
                (cx + t / 2.0, cy + 0.3 * s),
                (cx + t / 2.0, cy - 0.45 * s),
                (cx - t / 2.0, cy - 0.45 * s),
                (cx - t / 2.0, cy + 0.3 * s),
                (cx - 0.5 * s, cy + 0.3 * s),
            ];
            pen.polygon(arrow, cut);
            pen.rounded(
                Bounds(cx - 0.5 * s, cy - 0.85 * s, cx + 0.5 * s, cy - 0.85 * s + t),
                [t / 2.0; 4],
                cut,
            );
        }
    }
}
