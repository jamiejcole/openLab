// SPDX-License-Identifier: GPL-3.0-or-later

//! A Fade cue's curve as the mixer follows it: a rising and a falling shape and the domain the
//! gain moves in. The formulas are openlab-core's (`fade_curve.rs`, `RampShape`), copied because
//! neither crate depends on the other; `core/tests/level_maths.rs` holds the two equal to the bit.
//! Fixed-size and `Copy`, so a voice keeps a few without allocating.

/// Most knots one side holds: the 64 control points a shape may have and its two ends.
pub const MAX_SHAPE_KNOTS: usize = 66;

/// Ramp positions that are multiples of this are worked out exactly; between, the gain lies on
/// the straight line between the knots around it.
pub const SHAPE_KNOT: u64 = 64;

/// The kind of one side of a shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeKind {
    SCurve,
    /// A monotone cubic through the knots with the tangents given.
    Custom,
    /// `sin(πt/2)` raised to the parameter.
    Parametric,
    /// Straight lines through the knots.
    Linear,
}

/// The scale a gain moves on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeDomain {
    /// The gain is the fourth power of a position moved evenly.
    Slider,
    /// Evenly in dB, silent at and below the shape's floor.
    Decibel,
    /// Evenly in amplitude.
    Linear,
}

/// One side (rising or falling) of a shape: its knots as drawn, ends included.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeSide {
    kind: ShapeKind,
    parameter: f64,
    n: usize,
    ts: [f64; MAX_SHAPE_KNOTS],
    vs: [f64; MAX_SHAPE_KNOTS],
    ms: [f64; MAX_SHAPE_KNOTS],
}

impl ShapeSide {
    /// A side from its knots (times strictly increasing from 0 to 1, heights 0 to 1) and their
    /// tangents. `None` unless there are 2 to [`MAX_SHAPE_KNOTS`] of them, as many of each.
    pub fn new(
        kind: ShapeKind,
        parameter: f64,
        ts: &[f64],
        vs: &[f64],
        ms: &[f64],
    ) -> Option<Self> {
        let n = ts.len();
        if !(2..=MAX_SHAPE_KNOTS).contains(&n) || vs.len() != n || ms.len() != n {
            return None;
        }
        let mut side = Self {
            kind,
            parameter,
            n,
            ts: [0.0; MAX_SHAPE_KNOTS],
            vs: [0.0; MAX_SHAPE_KNOTS],
            ms: [0.0; MAX_SHAPE_KNOTS],
        };
        side.ts.get_mut(..n)?.copy_from_slice(ts);
        side.vs.get_mut(..n)?.copy_from_slice(vs);
        side.ms.get_mut(..n)?.copy_from_slice(ms);
        Some(side)
    }

    fn knots(&self) -> (&[f64], &[f64], &[f64]) {
        let n = self.n.min(MAX_SHAPE_KNOTS);
        (
            self.ts.get(..n).unwrap_or(&[]),
            self.vs.get(..n).unwrap_or(&[]),
            self.ms.get(..n).unwrap_or(&[]),
        )
    }

    fn drawn(&self, rising: bool, t: f64) -> f64 {
        let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
        let u = if rising { t } else { 1.0 - t };
        let (ts, vs, ms) = self.knots();
        let v = match self.kind {
            ShapeKind::SCurve => (1.0 - (u * std::f64::consts::PI).cos()) * 0.5,
            ShapeKind::Parametric => (u * std::f64::consts::FRAC_PI_2).sin().powf(self.parameter),
            ShapeKind::Linear => lines(ts, vs, t),
            ShapeKind::Custom => cubic(ts, vs, ms, t),
        };
        v.clamp(0.0, 1.0)
    }

    fn progress(&self, rising: bool, t: f64) -> f64 {
        if t.is_nan() || t <= 0.0 {
            return 0.0;
        }
        if t >= 1.0 {
            return 1.0;
        }
        let d = self.drawn(rising, t);
        if rising { d } else { 1.0 - d }
    }
}

fn interval(ts: &[f64], t: f64) -> usize {
    let i = ts.partition_point(|&x| x <= t);
    i.saturating_sub(1).min(ts.len().saturating_sub(2))
}

fn lines(ts: &[f64], vs: &[f64], t: f64) -> f64 {
    let i = interval(ts, t);
    match (ts.get(i), ts.get(i + 1), vs.get(i), vs.get(i + 1)) {
        (Some(t0), Some(t1), Some(v0), Some(v1)) if t1 > t0 => {
            v0 + (v1 - v0) * ((t - t0) / (t1 - t0))
        }
        _ => t,
    }
}

fn cubic(ts: &[f64], vs: &[f64], ms: &[f64], t: f64) -> f64 {
    let i = interval(ts, t);
    match (
        ts.get(i),
        ts.get(i + 1),
        vs.get(i),
        vs.get(i + 1),
        ms.get(i),
        ms.get(i + 1),
    ) {
        (Some(t0), Some(t1), Some(v0), Some(v1), Some(m0), Some(m1)) if t1 > t0 => {
            let h = t1 - t0;
            let s = (t - t0) / h;
            let (s2, s3) = (s * s, s * s * s);
            let h00 = 2.0 * s3 - 3.0 * s2 + 1.0;
            let h10 = s3 - 2.0 * s2 + s;
            let h01 = -2.0 * s3 + 3.0 * s2;
            let h11 = s3 - s2;
            h00 * v0 + h10 * h * m0 + h01 * v1 + h11 * h * m1
        }
        _ => t,
    }
}

/// A Fade cue's curve: the side a rising gain follows, the side a falling one follows, the
/// domain and, for the decibel domain, the floor in dB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FadeShape {
    domain: ShapeDomain,
    floor_db: f64,
    rising: ShapeSide,
    falling: ShapeSide,
}

impl FadeShape {
    pub fn new(domain: ShapeDomain, floor_db: f64, rising: ShapeSide, falling: ShapeSide) -> Self {
        Self {
            domain,
            floor_db,
            rising,
            falling,
        }
    }

    fn between(&self, a: f64, b: f64, s: f64) -> f64 {
        match self.domain {
            ShapeDomain::Linear => a + (b - a) * s,
            ShapeDomain::Slider => {
                let (xa, xb) = (a.max(0.0).sqrt().sqrt(), b.max(0.0).sqrt().sqrt());
                let x = xa + (xb - xa) * s;
                let x2 = x * x;
                x2 * x2
            }
            ShapeDomain::Decibel => {
                let floor = self.floor_db;
                let db = |g: f64| {
                    if g > 0.0 {
                        (20.0 * g.log10()).max(floor)
                    } else {
                        floor
                    }
                };
                let (da, dbb) = (db(a), db(b));
                let d = da + (dbb - da) * s;
                if d <= floor {
                    0.0
                } else {
                    10f64.powf(d / 20.0)
                }
            }
        }
    }

    /// The exact gain at knot `pos` of a ramp from `from` to `to` over `len` (or an end).
    pub(crate) fn at(&self, from: f64, to: f64, pos: u64, len: u64) -> f64 {
        if pos == 0 {
            return from;
        }
        if pos >= len {
            return to;
        }
        let rising = to >= from;
        let side = if rising { &self.rising } else { &self.falling };
        self.between(from, to, side.progress(rising, pos as f64 / len as f64))
    }

    /// The gain at `pos` of a ramp from `from` to `to` over `len`: exactly `from` at 0 and `to`
    /// from `len` on, between the exact knot values on a straight line.
    pub fn value(&self, from: f32, to: f32, pos: u64, len: u64) -> f32 {
        if pos == 0 {
            return from;
        }
        if pos >= len {
            return to;
        }
        let (a, b) = (f64::from(from), f64::from(to));
        let k0 = pos - pos % SHAPE_KNOT;
        let k1 = k0.saturating_add(SHAPE_KNOT).min(len);
        let (v0, v1) = (self.at(a, b, k0, len), self.at(a, b, k1, len));
        let inv = 1.0 / (k1 - k0) as f64;
        (v0 + (v1 - v0) * ((pos - k0) as f64 * inv)) as f32
    }

    /// [`Self::value`] at `pos, pos + 1, …` into `out`, positions before `len`: two exact
    /// evaluations per knot interval, the samples between by the same operations as `value`.
    pub(crate) fn fill(&self, from: f32, to: f32, pos: u64, len: u64, out: &mut [f32]) {
        let (a, b) = (f64::from(from), f64::from(to));
        let mut p = pos;
        let mut rest = out;
        while !rest.is_empty() {
            if p == 0 {
                if let Some((o, tail)) = std::mem::take(&mut rest).split_first_mut() {
                    *o = from;
                    rest = tail;
                }
                p = 1;
                continue;
            }
            if p >= len {
                rest.fill(to);
                return;
            }
            let k0 = p - p % SHAPE_KNOT;
            let k1 = k0.saturating_add(SHAPE_KNOT).min(len);
            let (v0, v1) = (self.at(a, b, k0, len), self.at(a, b, k1, len));
            let inv = 1.0 / (k1 - k0) as f64;
            let m = usize::try_from(k1 - p)
                .unwrap_or(usize::MAX)
                .min(rest.len());
            let (seg, tail) = std::mem::take(&mut rest).split_at_mut(m);
            for (j, o) in seg.iter_mut().enumerate() {
                let q = p + j as u64;
                *o = (v0 + (v1 - v0) * ((q - k0) as f64 * inv)) as f32;
            }
            p += m as u64;
            rest = tail;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn straight(domain: ShapeDomain) -> FadeShape {
        let up = ShapeSide::new(
            ShapeKind::Linear,
            1.0,
            &[0.0, 1.0],
            &[0.0, 1.0],
            &[1.0, 1.0],
        )
        .unwrap();
        let down = ShapeSide::new(
            ShapeKind::Linear,
            1.0,
            &[0.0, 1.0],
            &[1.0, 0.0],
            &[-1.0, -1.0],
        )
        .unwrap();
        FadeShape::new(domain, -60.0, up, down)
    }

    #[test]
    fn filling_matches_single_values_whatever_the_split() {
        let s = straight(ShapeDomain::Decibel);
        let want: Vec<f32> = (0..1000).map(|p| s.value(0.0, 2.0, p, 900)).collect();
        for cut in [1usize, 7, 64, 129, 500] {
            let mut got = vec![0.0; 1000];
            let (a, b) = got.split_at_mut(cut);
            s.fill(0.0, 2.0, 0, 900, a);
            s.fill(0.0, 2.0, cut as u64, 900, b);
            assert_eq!(
                got.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                want.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "cut at {cut}"
            );
        }
        assert_eq!(want[0], 0.0);
        assert_eq!(want[900], 2.0);
    }

    #[test]
    fn a_side_needs_two_to_sixty_six_knots() {
        assert!(ShapeSide::new(ShapeKind::Linear, 1.0, &[0.0], &[0.0], &[0.0]).is_none());
        let many = [0.5; MAX_SHAPE_KNOTS + 1];
        assert!(ShapeSide::new(ShapeKind::Linear, 1.0, &many, &many, &many).is_none());
        assert!(ShapeSide::new(ShapeKind::Linear, 1.0, &[0.0, 1.0], &[0.0], &[0.0, 0.0]).is_none());
    }
}
