// SPDX-License-Identifier: GPL-3.0-or-later

//! A Fade cue's curve (CT-10; QLab 5 manual pp. 198-199): a shape for levels that rise and one
//! for levels that fall, each a curve type with its parameter and control points, and the audio
//! domain the level moves in. Also the maths a ramp on such a curve follows: the mixer
//! (`openlab-audio::fade_shape`) carries a copy of it, which `tests/level_maths.rs` holds equal
//! to the bit.

use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::cue::{FadeCurve, SILENCE_DB};

/// Most control points one shape holds.
pub const MAX_CURVE_POINTS: usize = 64;
/// The range of a shape's parameter (the parametric curve's intensity).
pub const CURVE_PARAMETER_RANGE: (f64, f64) = (0.01, 100.0);
/// A shape's parameter when nothing sets it.
pub const DEFAULT_CURVE_PARAMETER: f64 = 1.0;

/// The scale a Fade cue moves audio levels on. It changes nothing else a Fade moves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioDomain {
    /// As a console fader's travel: the level's amplitude is the fourth power of the position.
    #[default]
    Slider,
    /// Evenly in dB, from the workspace's minimum level up.
    Decibel,
    /// Evenly in amplitude.
    Linear,
}

/// The kind of a shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurveType {
    /// Eases in and out: `(1 − cos πt) / 2`.
    #[default]
    SCurve,
    /// A smooth curve drawn near the control points, as QLab's (a quadratic B-spline).
    Custom,
    /// `sin(πt/2)` raised to the parameter: equal power at 1, the S-curve at 2.
    Parametric,
    /// Straight lines through the control points.
    Linear,
}

/// A control point, as drawn in its graph: `t` the time from the fade's start (0) to its end
/// (1), `v` the height from the bottom (0) to the top (1).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub t: f64,
    pub v: f64,
}

/// One of a Fade cue's two shapes. A rising shape is drawn from the bottom left `(0, 0)` to the
/// top right `(1, 1)`, a falling one from the top left `(0, 1)` to the bottom right `(1, 0)`;
/// those ends are fixed, and `points` are the control points between them, `t` strictly
/// increasing inside `(0, 1)`. Only custom and linear shapes read them; only a parametric one
/// reads `parameter`. Both are kept whatever the type, so a type change and back loses nothing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CurveShape {
    #[serde(rename = "type")]
    pub kind: CurveType,
    pub parameter: f64,
    pub points: Vec<CurvePoint>,
}

impl Default for CurveShape {
    fn default() -> Self {
        Self {
            kind: CurveType::SCurve,
            parameter: DEFAULT_CURVE_PARAMETER,
            points: Vec::new(),
        }
    }
}

impl CurveShape {
    /// Whether every value is one this shape may hold.
    pub fn is_valid(&self) -> bool {
        let (lo, hi) = CURVE_PARAMETER_RANGE;
        (lo..=hi).contains(&self.parameter)
            && self.points.len() <= MAX_CURVE_POINTS
            && self
                .points
                .iter()
                .all(|p| p.t > 0.0 && p.t < 1.0 && (0.0..=1.0).contains(&p.v))
            && self.points.windows(2).all(|w| match w {
                [a, b] => a.t < b.t,
                _ => true,
            })
    }
}

/// A Fade cue's curve: the shape a rising level follows, the one a falling level follows, and
/// the domain an audio level moves in. QLab gives both shapes one type; they are stored apart
/// as QLab stores them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FadeCueCurve {
    pub audio_domain: AudioDomain,
    pub rising: CurveShape,
    pub falling: CurveShape,
}

impl FadeCueCurve {
    pub fn is_valid(&self) -> bool {
        self.rising.is_valid() && self.falling.is_valid()
    }

    /// The curve that moves a level as the standard curve `c` does (evenly in amplitude, the
    /// falling shape the rising one mirrored); the default curve for one it has no match for.
    pub fn like(c: FadeCurve) -> Self {
        let kind = match c {
            FadeCurve::Linear => CurveType::Linear,
            FadeCurve::EqualPower => CurveType::Parametric,
            FadeCurve::SCurve => CurveType::SCurve,
            FadeCurve::Logarithmic | FadeCurve::Exponential => return Self::default(),
        };
        let shape = CurveShape {
            kind,
            ..Default::default()
        };
        Self {
            audio_domain: AudioDomain::Linear,
            rising: shape.clone(),
            falling: shape,
        }
    }

    /// The standard curve this one moves a value as in `domain`, if there is one: both shapes the
    /// same standard shape, in the linear domain.
    fn standard(&self, domain: AudioDomain) -> Option<FadeCurve> {
        if domain != AudioDomain::Linear {
            return None;
        }
        let one = |s: &CurveShape| match s.kind {
            CurveType::SCurve => Some(FadeCurve::SCurve),
            CurveType::Parametric if s.parameter == 1.0 => Some(FadeCurve::EqualPower),
            CurveType::Linear | CurveType::Custom if s.points.is_empty() => Some(FadeCurve::Linear),
            _ => None,
        };
        match (one(&self.rising), one(&self.falling)) {
            (Some(a), Some(b)) if a == b => Some(a),
            _ => None,
        }
    }

    /// A curve with a value it may not hold gets the default shapes (its domain is kept).
    /// Returns whether it did.
    pub fn repair(&mut self) -> bool {
        if self.is_valid() {
            return false;
        }
        self.rising = CurveShape::default();
        self.falling = CurveShape::default();
        true
    }
}

/// Fade positions that are multiples of this are evaluated exactly; the samples between lie on a
/// straight line between the two around them (the mixer's `KNOT`).
pub const SHAPE_KNOT: u64 = 64;

/// One shape ready to evaluate: its knots including the fixed ends, as drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeEval {
    pub kind: CurveType,
    pub parameter: f64,
    pub ts: Vec<f64>,
    pub vs: Vec<f64>,
}

impl ShapeEval {
    /// `shape`, which must be valid ([`CurveShape::is_valid`]); `rising` says which ends it has.
    fn new(shape: &CurveShape, rising: bool) -> Self {
        let (v0, v1) = if rising { (0.0, 1.0) } else { (1.0, 0.0) };
        let ts: Vec<f64> = std::iter::once(0.0)
            .chain(shape.points.iter().map(|p| p.t))
            .chain(std::iter::once(1.0))
            .collect();
        let vs: Vec<f64> = std::iter::once(v0)
            .chain(shape.points.iter().map(|p| p.v))
            .chain(std::iter::once(v1))
            .collect();
        Self {
            kind: shape.kind,
            parameter: shape.parameter,
            ts,
            vs,
        }
    }

    fn shape(&self) -> CurveShape {
        let inner = self.ts.len().saturating_sub(1);
        CurveShape {
            kind: self.kind,
            parameter: self.parameter,
            points: self
                .ts
                .iter()
                .zip(&self.vs)
                .take(inner)
                .skip(1)
                .map(|(&t, &v)| CurvePoint { t, v })
                .collect(),
        }
    }

    /// The height of the drawn curve at `t` (0 to 1), for the rising or the falling graph.
    pub fn drawn(&self, rising: bool, t: f64) -> f64 {
        let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
        let u = if rising { t } else { 1.0 - t };
        let v = match self.kind {
            CurveType::SCurve => (1.0 - (u * std::f64::consts::PI).cos()) * 0.5,
            CurveType::Parametric => (u * std::f64::consts::FRAC_PI_2).sin().powf(self.parameter),
            CurveType::Linear => lines(&self.ts, &self.vs, t),
            CurveType::Custom => spline(&self.ts, &self.vs, t),
        };
        v.clamp(0.0, 1.0)
    }

    /// How far a level following this shape is from its start (0) to its end (1) at `t`. Both
    /// ends are exact.
    pub fn progress(&self, rising: bool, t: f64) -> f64 {
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

/// The knot interval `t` lies in: `i` with `ts[i] <= t < ts[i + 1]` (the last for `t = 1`).
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

/// The custom curve at `t`, as QLab 5 draws it: a quadratic B-spline on the knots, ends kept.
/// Each knot between the ends shapes one quadratic Bezier piece, from the midpoint with the knot
/// before it (the start, for the first) to the midpoint with the knot after it (the end, for the
/// last), so the curve passes near its points, not through them, and turns smoothly where two
/// pieces meet. With knot times strictly increasing a piece's time only grows, so its height at
/// `t` is found by solving the piece's time for its parameter.
fn spline(ts: &[f64], vs: &[f64], t: f64) -> f64 {
    let n = ts.len();
    if n < 3 {
        return lines(ts, vs, t);
    }
    let knot = |k: usize| match (ts.get(k), vs.get(k)) {
        (Some(&x), Some(&y)) => (x, y),
        _ => (0.0, 0.0),
    };
    let mid = |k: usize| {
        let ((x0, y0), (x1, y1)) = (knot(k), knot(k + 1));
        ((x0 + x1) * 0.5, (y0 + y1) * 0.5)
    };
    // The piece `t` lies in: shaped by knot `i`, from 1 to n − 2.
    let j = interval(ts, t);
    let i = (if t >= mid(j).0 { j + 1 } else { j }).clamp(1, n - 2);
    let (ax, ay) = if i == 1 { knot(0) } else { mid(i - 1) };
    let (bx, by) = knot(i);
    let (cx, cy) = if i == n - 2 { knot(n - 1) } else { mid(i) };
    // Time on the piece is ax + bb·s + aa·s², rising from ax to cx; s by the root that keeps
    // its precision as aa nears 0.
    let dt = (t - ax).min(cx - ax).max(0.0);
    let (aa, bb) = (ax - 2.0 * bx + cx, 2.0 * (bx - ax));
    let den = bb + (bb * bb + 4.0 * aa * dt).max(0.0).sqrt();
    let s = if den > 0.0 {
        (2.0 * dt / den).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let r = 1.0 - s;
    r * r * ay + 2.0 * s * r * by + s * s * cy
}

/// A Fade cue's curve as its ramps follow it: both shapes ready to evaluate, the domain the value
/// moves in and, for the decibel domain, the level at and below which it is silent.
#[derive(Clone, Debug, PartialEq)]
pub struct RampShape {
    pub domain: AudioDomain,
    /// dB, at least [`SILENCE_DB`]: the workspace's minimum level.
    pub floor_db: f64,
    pub rising: ShapeEval,
    pub falling: ShapeEval,
}

impl RampShape {
    /// `curve` for ramps of audio levels with silence at `floor_db`; for a picture's values, the
    /// linear domain. An invalid curve (a model never holds one) gives the default shapes.
    pub fn new(curve: &FadeCueCurve, domain: AudioDomain, floor_db: f64) -> Self {
        let mut curve = curve.clone();
        curve.repair();
        let floor_db = if floor_db.is_nan() {
            SILENCE_DB
        } else {
            floor_db.max(SILENCE_DB)
        };
        Self {
            domain,
            floor_db,
            rising: ShapeEval::new(&curve.rising, true),
            falling: ShapeEval::new(&curve.falling, false),
        }
    }

    fn curve(&self) -> FadeCueCurve {
        FadeCueCurve {
            audio_domain: self.domain,
            rising: self.rising.shape(),
            falling: self.falling.shape(),
        }
    }

    /// The value on a move from `a` to `b` once its shape has made `s` (0 to 1) of the way.
    pub fn between(&self, a: f64, b: f64, s: f64) -> f64 {
        match self.domain {
            AudioDomain::Linear => a + (b - a) * s,
            AudioDomain::Slider => {
                let (xa, xb) = (a.max(0.0).sqrt().sqrt(), b.max(0.0).sqrt().sqrt());
                let x = xa + (xb - xa) * s;
                let x2 = x * x;
                x2 * x2
            }
            AudioDomain::Decibel => {
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

    /// The value at position `pos` of `len`: exactly `from` at 0 and `to` from `len` on; at a
    /// multiple of [`SHAPE_KNOT`], the shape's own value; between, on the straight line between
    /// the knots around it. A rising move (`to >= from`) follows the rising shape.
    pub fn value(&self, from: f64, to: f64, pos: u64, len: u64) -> f64 {
        if pos == 0 {
            return from;
        }
        if pos >= len {
            return to;
        }
        let k0 = pos - pos % SHAPE_KNOT;
        let k1 = k0.saturating_add(SHAPE_KNOT).min(len);
        let (v0, v1) = (self.at(from, to, k0, len), self.at(from, to, k1, len));
        let inv = 1.0 / (k1 - k0) as f64;
        v0 + (v1 - v0) * ((pos - k0) as f64 * inv)
    }

    /// The exact value at knot `pos` (or an end).
    pub fn at(&self, from: f64, to: f64, pos: u64, len: u64) -> f64 {
        if pos == 0 {
            return from;
        }
        if pos >= len {
            return to;
        }
        let rising = to >= from;
        let shape = if rising { &self.rising } else { &self.falling };
        self.between(from, to, shape.progress(rising, pos as f64 / len as f64))
    }

    /// [`Self::value`] of a gain, as the mixer works it out.
    pub fn gain(&self, from: f32, to: f32, pos: u64, len: u64) -> f32 {
        if pos == 0 {
            return from;
        }
        if pos >= len {
            return to;
        }
        self.value(f64::from(from), f64::from(to), pos, len) as f32
    }
}

/// The curve a ramp follows: one of the standard curves (built-in fades, edits, panic) or a Fade
/// cue's. Serialised as the standard curve's name, or the Fade cue's curve with its floor.
#[derive(Clone, Debug, PartialEq)]
pub enum RampCurve {
    Standard(FadeCurve),
    Shaped(Arc<RampShape>),
}

impl RampCurve {
    pub const LINEAR: RampCurve = RampCurve::Standard(FadeCurve::Linear);

    /// What a Fade cue's ramps follow for its `curve` in `domain` (silence at `floor_db`): the
    /// standard curve it is the same as, where there is one, so the mixer shares the work.
    pub fn of_fade(curve: &FadeCueCurve, domain: AudioDomain, floor_db: f64) -> Self {
        let mut curve = curve.clone();
        curve.repair();
        match curve.standard(domain) {
            Some(c) => Self::Standard(c),
            None => Self::Shaped(Arc::new(RampShape::new(&curve, domain, floor_db))),
        }
    }

    /// The gain on step `pos` of a ramp from `from` to `to` over `len` steps.
    pub fn gain(&self, from: f32, to: f32, pos: u64, len: u64) -> f32 {
        match self {
            Self::Standard(c) => crate::cue_engine::ramp_gain(from, to, pos, len, *c),
            Self::Shaped(s) => s.gain(from, to, pos, len),
        }
    }

    /// [`Self::gain`] for any value (a picture's), in f64.
    pub fn value(&self, from: f64, to: f64, pos: u64, len: u64) -> f64 {
        match self {
            Self::Standard(c) => crate::cue_engine::ramp_value(from, to, pos, len, *c),
            Self::Shaped(s) => s.value(from, to, pos, len),
        }
    }
}

impl From<FadeCurve> for RampCurve {
    fn from(c: FadeCurve) -> Self {
        Self::Standard(c)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum RampCurveForm {
    Standard(FadeCurve),
    Shaped {
        #[serde(flatten)]
        curve: FadeCueCurve,
        floor_db: f64,
    },
}

impl Serialize for RampCurve {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Standard(c) => RampCurveForm::Standard(*c),
            Self::Shaped(r) => RampCurveForm::Shaped {
                curve: r.curve(),
                floor_db: r.floor_db,
            },
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for RampCurve {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match RampCurveForm::deserialize(d)? {
            RampCurveForm::Standard(c) => Ok(Self::Standard(c)),
            RampCurveForm::Shaped { curve, floor_db } => {
                if !curve.is_valid() || !floor_db.is_finite() {
                    return Err(serde::de::Error::custom(
                        "a curve with a value it may not hold",
                    ));
                }
                Ok(Self::Shaped(Arc::new(RampShape::new(
                    &curve,
                    curve.audio_domain,
                    floor_db,
                ))))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn shape(kind: CurveType, points: &[(f64, f64)]) -> CurveShape {
        CurveShape {
            kind,
            parameter: 1.0,
            points: points.iter().map(|&(t, v)| CurvePoint { t, v }).collect(),
        }
    }

    #[test]
    fn linear_shapes_pass_through_their_points_and_custom_ones_are_qlabs_spline() {
        let s = ShapeEval::new(&shape(CurveType::Linear, &[(0.25, 0.7), (0.6, 0.8)]), true);
        assert!((s.drawn(true, 0.25) - 0.7).abs() < 1e-12);
        assert!((s.drawn(true, 0.6) - 0.8).abs() < 1e-12);
        let f = ShapeEval::new(&shape(CurveType::Linear, &[(0.5, 0.2)]), false);
        assert!((f.progress(false, 0.5) - 0.8).abs() < 1e-12);
        // One point: the quadratic Bezier from the start to the end with the point its control.
        let one = ShapeEval::new(&shape(CurveType::Custom, &[(0.5, 0.8)]), true);
        assert!((one.drawn(true, 0.5) - 0.65).abs() < 1e-12);
        assert_eq!(one.progress(true, 0.0), 0.0);
        assert_eq!(one.progress(true, 1.0), 1.0);
        // Two points: through the midpoint between them, not through either.
        let two = ShapeEval::new(&shape(CurveType::Custom, &[(0.25, 0.3), (0.6, 0.1)]), false);
        assert!((two.drawn(false, 0.425) - 0.2).abs() < 1e-12);
        assert!((two.drawn(false, 0.25) - 0.3).abs() > 0.01);
        // Read off QLab 5.6.5's Curve tab (846 by 583 pixels) for these points: within a pixel.
        let qlab = ShapeEval::new(
            &shape(CurveType::Custom, &[(0.2, 0.5), (0.5, 0.3), (0.8, 0.9)]),
            true,
        );
        for (t, v) in [
            (0.1005, 0.2196),
            (0.3499, 0.3998),
            (0.5, 0.3998),
            (0.6501, 0.6002),
            (0.8995, 0.935),
        ] {
            let got = qlab.drawn(true, t);
            assert!((got - v).abs() < 0.002, "at {t}: {got}, QLab {v}");
        }
    }

    #[test]
    fn standard_shapes_match_their_formulas() {
        let s = ShapeEval::new(&CurveShape::default(), true);
        assert!(
            (s.drawn(true, 0.25) - (1.0 - (0.25 * std::f64::consts::PI).cos()) / 2.0).abs() < 1e-15
        );
        let f = ShapeEval::new(&CurveShape::default(), false);
        // The falling S-curve is the rising one mirrored.
        assert!((f.drawn(false, 0.25) - s.drawn(true, 0.75)).abs() < 1e-15);
        // Parametric at 1 is equal power; at 2, the S-curve.
        let mut p = shape(CurveType::Parametric, &[]);
        let e = ShapeEval::new(&p, true);
        assert!((e.drawn(true, 0.3) - (0.3 * std::f64::consts::FRAC_PI_2).sin()).abs() < 1e-15);
        p.parameter = 2.0;
        let e = ShapeEval::new(&p, true);
        assert!((e.drawn(true, 0.3) - s.drawn(true, 0.3)).abs() < 1e-12);
    }

    #[test]
    fn domains_meet_their_ends_and_the_slider_and_db_ones_differ_from_linear() {
        let curve = FadeCueCurve {
            rising: shape(CurveType::Linear, &[]),
            falling: shape(CurveType::Linear, &[]),
            ..Default::default()
        };
        for domain in [
            AudioDomain::Slider,
            AudioDomain::Decibel,
            AudioDomain::Linear,
        ] {
            let r = RampShape::new(&curve, domain, -60.0);
            assert_eq!(r.gain(0.0, 1.0, 0, 640), 0.0);
            assert_eq!(r.gain(0.0, 1.0, 640, 640), 1.0);
        }
        let mid = |d| RampShape::new(&curve, d, -60.0).gain(0.0, 1.0, 320, 640);
        assert!((mid(AudioDomain::Linear) - 0.5).abs() < 1e-6);
        assert!((mid(AudioDomain::Slider) - 0.0625).abs() < 1e-6);
        // Half way from −60 dB to 0 dB: −30 dB.
        assert!((mid(AudioDomain::Decibel) - 10f32.powf(-1.5)).abs() < 1e-6);
    }

    #[test]
    fn invalid_curves_are_repaired_to_the_default_shapes() {
        let bad = [
            shape(CurveType::Custom, &[(0.5, 1.5)]),
            shape(CurveType::Custom, &[(0.0, 0.5)]),
            shape(CurveType::Custom, &[(0.6, 0.5), (0.4, 0.5)]),
            shape(CurveType::Custom, &[(f64::NAN, 0.5)]),
            CurveShape {
                parameter: 0.0,
                ..Default::default()
            },
            shape(CurveType::Linear, &[(0.5, 0.5); MAX_CURVE_POINTS + 1]),
        ];
        for b in bad {
            let mut c = FadeCueCurve {
                audio_domain: AudioDomain::Decibel,
                rising: b,
                falling: CurveShape::default(),
            };
            assert!(c.repair());
            assert_eq!(
                c,
                FadeCueCurve {
                    audio_domain: AudioDomain::Decibel,
                    ..Default::default()
                }
            );
        }
    }

    #[test]
    fn a_ramp_curve_goes_through_json_and_refuses_a_bad_one() {
        let curve = FadeCueCurve {
            audio_domain: AudioDomain::Decibel,
            rising: shape(CurveType::Custom, &[(0.5, 0.9)]),
            falling: CurveShape::default(),
        };
        let r = RampCurve::Shaped(Arc::new(RampShape::new(&curve, curve.audio_domain, -60.0)));
        let text = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<RampCurve>(&text).unwrap(), r);
        assert_eq!(
            serde_json::to_string(&RampCurve::LINEAR).unwrap(),
            r#""linear""#
        );
        let bad = text.replace("0.9", "9.0");
        assert!(serde_json::from_str::<RampCurve>(&bad).is_err());
    }
}
