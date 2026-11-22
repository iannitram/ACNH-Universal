//! Cubic-to-quadratic conversion matching fontTools' one-unit tolerance.
use super::truetype::Point;
use crate::{Error, Result};
use std::ops::{Add, Mul, Sub};
#[derive(Clone, Copy, Debug)]
pub struct Vec2(pub f64, pub f64);
impl Add for Vec2 {
    type Output = Self;
    fn add(self, r: Self) -> Self {
        Self(self.0 + r.0, self.1 + r.1)
    }
}
impl Sub for Vec2 {
    type Output = Self;
    fn sub(self, r: Self) -> Self {
        Self(self.0 - r.0, self.1 - r.1)
    }
}
impl Mul<f64> for Vec2 {
    type Output = Self;
    fn mul(self, r: f64) -> Self {
        Self(self.0 * r, self.1 * r)
    }
}
impl Vec2 {
    fn norm(self) -> f64 {
        self.0.hypot(self.1)
    }
    pub fn point(self, on_curve: bool) -> Point {
        Point {
            x: self.0,
            y: self.1,
            on_curve,
        }
    }
}
fn fits(p0: Vec2, p1: Vec2, p2: Vec2, p3: Vec2, t: f64) -> bool {
    if p1.norm() <= t && p2.norm() <= t {
        return true;
    }
    let mid = (p0 + (p1 + p2) * 3.0 + p3) * 0.125;
    if mid.norm() > t {
        return false;
    }
    let d = (p3 + p2 - p1 - p0) * 0.125;
    fits(p0, (p0 + p1) * 0.5, mid - d, mid, t) && fits(mid, mid + d, (p2 + p3) * 0.5, p3, t)
}
fn split(p: [Vec2; 4], n: usize, i: usize) -> [Vec2; 4] {
    let [p0, p1, p2, p3] = p;
    let c = (p1 - p0) * 3.0;
    let b = (p2 - p1) * 3.0 - c;
    let a = p3 - p0 - c - b;
    let dt = 1.0 / n as f64;
    let t = i as f64 * dt;
    let aa = a * (dt * dt * dt);
    let bb = (a * (3.0 * t) + b) * (dt * dt);
    let cc = (b * (2.0 * t) + c + a * (3.0 * t * t)) * dt;
    let dd = a * (t * t * t) + b * (t * t) + c * t + p0;
    [
        dd,
        dd + cc * (1.0 / 3.0),
        dd + (cc * 2.0 + bb) * (1.0 / 3.0),
        dd + cc + bb + aa,
    ]
}
fn control(p: [Vec2; 4], t: f64) -> Vec2 {
    let a = p[0] + (p[1] - p[0]) * 1.5;
    let b = p[3] + (p[2] - p[3]) * 1.5;
    a + (b - a) * t
}
/// Return consecutive off-curve points followed by the final on-curve point.
pub(super) fn cubic(p: [Vec2; 4]) -> Result<Vec<Point>> {
    let zero = Vec2(0.0, 0.0);
    let ab = p[1] - p[0];
    let cd = p[3] - p[2];
    let perp = Vec2(-ab.1, ab.0);
    let denom = perp.0 * cd.0 + perp.1 * cd.1;
    if denom != 0.0 {
        let v = p[0] - p[2];
        let h = (perp.0 * v.0 + perp.1 * v.1) / denom;
        let q = p[2] + cd * h;
        let c1 = p[0] + (q - p[0]) * (2.0 / 3.0);
        let c2 = p[3] + (q - p[3]) * (2.0 / 3.0);
        if fits(zero, c1 - p[1], c2 - p[2], zero, 1.0) {
            return Ok(vec![q.point(false), p[3].point(true)]);
        }
    }
    for n in 2..=100 {
        let mut q2 = p[0];
        let mut d1 = zero;
        let mut next = control(split(p, n, 0), 0.0);
        let mut result = vec![next.point(false)];
        let mut valid = true;
        for i in 1..=n {
            let c = split(p, n, i - 1);
            let q0 = q2;
            let q1 = next;
            if i < n {
                next = control(split(p, n, i), i as f64 / (n - 1) as f64);
                result.push(next.point(false));
                q2 = (q1 + next) * 0.5
            } else {
                q2 = c[3]
            }
            let d0 = d1;
            d1 = q2 - c[3];
            if d1.norm() > 1.0
                || !fits(
                    d0,
                    q0 + (q1 - q0) * (2.0 / 3.0) - c[1],
                    q2 + (q1 - q2) * (2.0 / 3.0) - c[2],
                    d1,
                    1.0,
                )
            {
                valid = false;
                break;
            }
        }
        if valid {
            result.push(p[3].point(true));
            return Ok(result);
        }
    }
    Err(Error::Io("could not approximate a button outline".into()))
}
