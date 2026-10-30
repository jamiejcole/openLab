// SPDX-License-Identifier: GPL-3.0-or-later

//! Gain helpers: dB conversion, fade curves and per-sample gain ramps. Formulas are documented in
//! `docs/audio.md`; keep the two in sync.

/// Levels at or below this are silence (ARCHITECTURE §4).
pub const SILENCE_DB: f64 = -120.0;

/// Safety ceiling for every gain the mixer applies (the show model itself stops at +12 dB).
pub const MAX_GAIN_DB: f64 = 24.0;

/// [`MAX_GAIN_DB`] as linear amplitude.
pub const MAX_GAIN: f32 = 15.848_932;

/// Makes a gain safe to apply: NaN is silence, everything else is clamped to ±[`MAX_GAIN`].
pub fn sanitize_gain(g: f32) -> f32 {
    if g.is_nan() {
        0.0
    } else {
        g.clamp(-MAX_GAIN, MAX_GAIN)
    }
}

/// Converts dB to linear amplitude. At or below [`SILENCE_DB`] (and NaN) is exactly 0.0.
pub fn db_to_linear(db: f64) -> f32 {
    if db.is_nan() || db <= SILENCE_DB {
        0.0
    } else {
        10f64.powf(db / 20.0) as f32
    }
}

/// Converts linear amplitude (sign ignored) to dB, clamped below at [`SILENCE_DB`].
pub fn linear_to_db(gain: f32) -> f64 {
    let g = f64::from(gain).abs();
    if g.is_nan() {
        return SILENCE_DB;
    }
    (20.0 * g.log10()).max(SILENCE_DB)
}

/// The exponential curve spans this many dB (a ratio of 1000), so it is close to dB-linear.
const EXP_K: f64 = 6.907_755_278_982_137; // ln(1000)

/// Fade curve shape. Each defines `f: [0, 1] → [0, 1]` with `f(0) = 0`, `f(1) = 1`, monotonic and
/// continuous; this is the shape of a rising ramp. A falling ramp traces the same gain path in
/// reverse (`1 − f(1 − t)`), so a fade-out is the time-reverse of the matching fade-in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Curve {
    /// `f(t) = t`
    #[default]
    Linear,
    /// `f(t) = sin(πt/2)`: from/to silence the in and out curves are power-complementary.
    EqualPower,
    /// `f(t) = (1 − cos πt) / 2`
    SCurve,
    /// `f(t) = 1 − e(1 − t)`, the point reflection of [`Curve::Exponential`].
    Logarithmic,
    /// `f(t) = e(t) = (exp(kt) − 1) / (exp(k) − 1)`, `k = ln 1000`: about dB-linear over 60 dB.
    Exponential,
}

impl Curve {
    /// Rising shape `f(t)` for `t` in `[0, 1]` (clamped; NaN counts as 0).
    pub fn shape(self, t: f64) -> f64 {
        let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
        // Same exact-ends guard as openlab-core's `curve_shape` (tests/level_maths.rs).
        if t <= 0.0 {
            return 0.0;
        }
        if t >= 1.0 {
            return 1.0;
        }
        let v = match self {
            Curve::Linear => t,
            Curve::EqualPower => (t * std::f64::consts::FRAC_PI_2).sin(),
            Curve::SCurve => (1.0 - (t * std::f64::consts::PI).cos()) * 0.5,
            Curve::Logarithmic => 1.0 - exp_shape(1.0 - t),
            Curve::Exponential => exp_shape(t),
        };
        v.clamp(0.0, 1.0)
    }
}

fn exp_shape(t: f64) -> f64 {
    // Same guard as openlab-core's `exp_shape` (tests/level_maths.rs holds them bit-equal).
    if t >= 1.0 {
        return 1.0;
    }
    (EXP_K * t).exp_m1() / EXP_K.exp_m1()
}

/// A duration in frames plus the curve to follow. Zero frames means "immediately".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fade {
    pub frames: u64,
    pub curve: Curve,
}

impl Fade {
    pub const NONE: Fade = Fade {
        frames: 0,
        curve: Curve::Linear,
    };

    pub fn new(frames: u64, curve: Curve) -> Self {
        Fade { frames, curve }
    }
}

/// Gain at step `pos` of a ramp from `from` to `to` over `len` steps. Step 0 is exactly `from`,
/// step `len` (and beyond) exactly `to`.
pub fn ramp_value(from: f32, to: f32, pos: u64, len: u64, curve: Curve) -> f32 {
    if pos == 0 {
        return from;
    }
    if pos >= len {
        return to;
    }
    let t = pos as f64 / len as f64;
    let s = if to >= from {
        curve.shape(t)
    } else {
        1.0 - curve.shape(1.0 - t)
    };
    let (a, b) = (f64::from(from), f64::from(to));
    (a + (b - a) * s) as f32
}

/// The mixer's gain along a ramp or fade from `from` to `to` differs from [`ramp_value`] by at
/// most this times `max(|from|, |to|)`: −120 dB, the silence floor. One `f32` step of the
/// result is up to 1.2e-7 of it and the mixer's recurrence adds about 1e-13 (docs/audio.md);
/// the first frame and the end are exact.
pub const RAMP_TOLERANCE: f64 = 1e-6;

/// Ramp positions that are multiples of this are evaluated exactly; the frames between follow
/// a recurrence from the last one. Fixed and position-aligned, so a value depends only on the
/// ramp and its position, never on how the timeline is cut into blocks.
const KNOT: u64 = 64;

/// `1 / (exp(k) − 1)` of the exponential curve.
const EXP_INV: f64 = 1.0 / 999.0;

/// How [`CurveRamp`] gets the curve's progress `s` from its state `(x, y)`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    /// Delegates to [`ramp_value`].
    Exact,
    /// `(x, y) = (cos φ, sin φ)` with `φ = θ·pos`, advanced by a rotation through `θ`.
    /// Equal-power rising: `s = sin φ`.
    Sin,
    /// Equal-power falling: `s = 1 − cos φ`.
    OneMinusCos,
    /// S-curve, either way: `s = (1 − cos φ) / 2`.
    HalfOneMinusCos,
    /// `y = exp(k·t)`, advanced by multiplying with `exp(k/len)`: `s = (y − 1) / (e^k − 1)`
    /// (exponential rising, logarithmic falling).
    ExpUp,
    /// `y = exp(k·(1 − t))`, multiplied by `exp(−k/len)`: `s = 1 − (y − 1) / (e^k − 1)`
    /// (exponential falling, logarithmic rising).
    ExpDown,
}

/// The mixer's evaluator of [`ramp_value`]: one exact evaluation per [`KNOT`] frames of ramp
/// position instead of a `sin`/`cos`/`exp` per frame. A value is a pure function of the ramp
/// and the position, whatever order positions are asked in; consecutive positions cost a few
/// multiplications each. Position 0 is exactly `from` and `len` (and beyond) exactly `to`; every
/// other value is within [`RAMP_TOLERANCE`] of [`ramp_value`] (docs/audio.md).
#[derive(Clone, Copy, Debug)]
pub(crate) struct CurveRamp {
    from: f32,
    to: f32,
    len: u64,
    curve: Curve,
    kind: Kind,
    a: f64,
    d: f64,
    /// Rotation step `(cos θ, sin θ)`, or the multiplicative step in `cr`.
    cr: f64,
    sr: f64,
    /// Position the state `(x, y)` belongs to; `None` until the first evaluation.
    at: Option<u64>,
    x: f64,
    y: f64,
}

impl CurveRamp {
    pub(crate) fn new(from: f32, to: f32, len: u64, curve: Curve) -> Self {
        let rising = to >= from;
        let kind = match (curve, rising) {
            (Curve::Linear, _) => Kind::Exact,
            (Curve::EqualPower, true) => Kind::Sin,
            (Curve::EqualPower, false) => Kind::OneMinusCos,
            (Curve::SCurve, _) => Kind::HalfOneMinusCos,
            (Curve::Exponential, true) | (Curve::Logarithmic, false) => Kind::ExpUp,
            (Curve::Exponential, false) | (Curve::Logarithmic, true) => Kind::ExpDown,
        };
        let n = len.max(1) as f64;
        let (cr, sr) = match kind {
            Kind::Exact => (1.0, 0.0),
            Kind::Sin | Kind::OneMinusCos => {
                let (s, c) = (std::f64::consts::FRAC_PI_2 / n).sin_cos();
                (c, s)
            }
            Kind::HalfOneMinusCos => {
                let (s, c) = (std::f64::consts::PI / n).sin_cos();
                (c, s)
            }
            Kind::ExpUp => ((EXP_K / n).exp(), 0.0),
            Kind::ExpDown => ((-EXP_K / n).exp(), 0.0),
        };
        let (a, b) = (f64::from(from), f64::from(to));
        CurveRamp {
            from,
            to,
            len,
            curve,
            kind,
            a,
            d: b - a,
            cr,
            sr,
            at: None,
            x: 0.0,
            y: 0.0,
        }
    }

    /// Whether this evaluates the ramp `from → to` over `len` with `curve`.
    pub(crate) fn is(&self, from: f32, to: f32, len: u64, curve: Curve) -> bool {
        self.from.to_bits() == from.to_bits()
            && self.to.to_bits() == to.to_bits()
            && self.len == len
            && self.curve == curve
    }

    /// Value at `pos` of the ramp `from → to` over `len` with `curve`, first turning this into
    /// that ramp's evaluator if it is another's.
    pub(crate) fn value_of(&mut self, from: f32, to: f32, len: u64, curve: Curve, pos: u64) -> f32 {
        if !self.is(from, to, len, curve) {
            *self = CurveRamp::new(from, to, len, curve);
        }
        self.value(pos)
    }

    /// Value at `pos`; leaves the state ready for `pos + 1`.
    pub(crate) fn value(&mut self, pos: u64) -> f32 {
        if pos == 0 || pos >= self.len || self.kind == Kind::Exact {
            return ramp_value(self.from, self.to, pos, self.len, self.curve);
        }
        if self.at != Some(pos) {
            self.seek(pos);
        }
        let v = self.output();
        let next = pos + 1;
        if next < self.len {
            if next.is_multiple_of(KNOT) {
                self.anchor(next);
            } else {
                self.step();
            }
            self.at = Some(next);
        }
        v
    }

    /// `f(slot, value)` for each slot of `out` and the values at positions `pos, pos + 1, …`:
    /// bit for bit what [`Self::value`] gives at each position, by the same operations, but with
    /// the curve's branches taken once per stretch between knots instead of once per frame.
    /// Leaves the state ready for the position after the last.
    #[inline]
    pub(crate) fn apply(&mut self, pos: u64, out: &mut [f32], mut f: impl FnMut(&mut f32, f32)) {
        if self.kind == Kind::Exact {
            for (o, p) in out.iter_mut().zip(pos..) {
                f(o, ramp_value(self.from, self.to, p, self.len, self.curve));
            }
            return;
        }
        let mut rest = out;
        let mut p = pos;
        if p == 0
            && let Some((o, tail)) = std::mem::take(&mut rest).split_first_mut()
        {
            f(o, self.from);
            rest = tail;
            p = 1;
        }
        let mid = usize::try_from(self.len.saturating_sub(p))
            .unwrap_or(usize::MAX)
            .min(rest.len());
        let (mut body, end) = rest.split_at_mut(mid);
        if !body.is_empty() && self.at != Some(p) {
            self.seek(p);
        }
        while !body.is_empty() {
            let k = usize::try_from(KNOT - p % KNOT)
                .unwrap_or(usize::MAX)
                .min(body.len());
            let (seg, more) = body.split_at_mut(k);
            self.run(seg, &mut f);
            body = more;
            p += k as u64;
            // As in `value`: the state moves to the next position only inside the ramp.
            if p < self.len {
                if p.is_multiple_of(KNOT) {
                    self.anchor(p);
                }
                self.at = Some(p);
            } else {
                self.at = None;
            }
        }
        for o in end {
            f(o, self.to);
        }
    }

    /// `f` of the outputs over `seg`, stepping after each; [`Self::output`] and [`Self::step`]
    /// written out per kind so the loop has no branch on it.
    #[inline]
    fn run(&mut self, seg: &mut [f32], f: &mut impl FnMut(&mut f32, f32)) {
        let (a, d) = (self.a, self.d);
        let out = |s: f64| (a + d * s.clamp(0.0, 1.0)) as f32;
        match self.kind {
            Kind::Exact => {}
            Kind::Sin => self.rotate(seg, f, |_, y| out(y)),
            Kind::OneMinusCos => self.rotate(seg, f, |x, _| out(1.0 - x)),
            Kind::HalfOneMinusCos => self.rotate(seg, f, |x, _| out((1.0 - x) * 0.5)),
            Kind::ExpUp => self.scale(seg, f, |y| out((y - 1.0) * EXP_INV)),
            Kind::ExpDown => self.scale(seg, f, |y| out(1.0 - (y - 1.0) * EXP_INV)),
        }
    }

    #[inline]
    fn rotate(
        &mut self,
        seg: &mut [f32],
        f: &mut impl FnMut(&mut f32, f32),
        out: impl Fn(f64, f64) -> f32,
    ) {
        let (cr, sr) = (self.cr, self.sr);
        let (mut x, mut y) = (self.x, self.y);
        for o in seg {
            f(o, out(x, y));
            (x, y) = (x * cr - y * sr, y * cr + x * sr);
        }
        (self.x, self.y) = (x, y);
    }

    #[inline]
    fn scale(
        &mut self,
        seg: &mut [f32],
        f: &mut impl FnMut(&mut f32, f32),
        out: impl Fn(f64) -> f32,
    ) {
        let cr = self.cr;
        let mut y = self.y;
        for o in seg {
            f(o, out(y));
            y *= cr;
        }
        self.y = y;
    }

    /// [`Self::apply`] for the ramp `from → to` over `len` with `curve`, first turning this into
    /// that ramp's evaluator if it is another's (as [`Self::value_of`]).
    #[inline]
    pub(crate) fn apply_of(
        &mut self,
        ramp: (f32, f32, u64, Curve),
        pos: u64,
        out: &mut [f32],
        f: impl FnMut(&mut f32, f32),
    ) {
        let (from, to, len, curve) = ramp;
        if !self.is(from, to, len, curve) {
            *self = CurveRamp::new(from, to, len, curve);
        }
        self.apply(pos, out, f);
    }

    /// Value at `pos` without moving the state.
    pub(crate) fn peek(&self, pos: u64) -> f32 {
        let mut c = *self;
        c.value(pos)
    }

    /// The state at `pos`: exact at the knot at or before it, then stepped. [`Self::value`]
    /// reaches every position by the same operations, so both agree to the bit.
    fn seek(&mut self, pos: u64) {
        let knot = pos - pos % KNOT;
        self.anchor(knot);
        for _ in knot..pos {
            self.step();
        }
        self.at = Some(pos);
    }

    fn anchor(&mut self, pos: u64) {
        let t = pos as f64 / self.len as f64;
        match self.kind {
            Kind::Exact => {}
            Kind::Sin | Kind::OneMinusCos => {
                (self.y, self.x) = (t * std::f64::consts::FRAC_PI_2).sin_cos();
            }
            Kind::HalfOneMinusCos => (self.y, self.x) = (t * std::f64::consts::PI).sin_cos(),
            Kind::ExpUp => self.y = (EXP_K * t).exp(),
            Kind::ExpDown => self.y = (EXP_K * (1.0 - t)).exp(),
        }
    }

    fn step(&mut self) {
        match self.kind {
            Kind::Exact => {}
            Kind::Sin | Kind::OneMinusCos | Kind::HalfOneMinusCos => {
                let (x, y) = (self.x, self.y);
                self.x = x * self.cr - y * self.sr;
                self.y = y * self.cr + x * self.sr;
            }
            Kind::ExpUp | Kind::ExpDown => self.y *= self.cr,
        }
    }

    fn output(&self) -> f32 {
        let s = match self.kind {
            Kind::Exact => 0.0,
            Kind::Sin => self.y,
            Kind::OneMinusCos => 1.0 - self.x,
            Kind::HalfOneMinusCos => (1.0 - self.x) * 0.5,
            Kind::ExpUp => (self.y - 1.0) * EXP_INV,
            Kind::ExpDown => 1.0 - (self.y - 1.0) * EXP_INV,
        };
        (self.a + self.d * s.clamp(0.0, 1.0)) as f32
    }
}

#[derive(Clone, Copy, Debug)]
struct Ramp {
    pos: u64,
    len: u64,
    to: f32,
    eval: CurveRamp,
}

/// A gain that can ramp, advanced one sample at a time on the audio thread.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Param {
    value: f32,
    ramp: Option<Ramp>,
}

impl Param {
    pub(crate) fn new(value: f32) -> Self {
        Param { value, ramp: None }
    }

    /// Value at the next sample to be rendered.
    pub(crate) fn current(&self) -> f32 {
        match &self.ramp {
            Some(r) => r.eval.peek(r.pos),
            None => self.value,
        }
    }

    /// Constant zero: contributes nothing and needs no per-sample work.
    pub(crate) fn is_silent(&self) -> bool {
        self.ramp.is_none() && self.value == 0.0
    }

    /// The value while not ramping: every sample of the next block has it.
    pub(crate) fn steady(&self) -> Option<f32> {
        match self.ramp {
            Some(_) => None,
            None => Some(self.value),
        }
    }

    pub(crate) fn is_ramping(&self) -> bool {
        self.ramp.is_some()
    }

    pub(crate) fn set(&mut self, value: f32) {
        self.value = value;
        self.ramp = None;
    }

    /// Ends a ramp at once on its target (nothing has been heard through it yet).
    pub(crate) fn finish(&mut self) {
        if let Some(r) = self.ramp.take() {
            self.value = r.to;
        }
    }

    /// Ramps from the current value to `to`, starting at the next sample.
    pub(crate) fn ramp_to(&mut self, to: f32, frames: u64, curve: Curve) {
        if frames == 0 {
            self.set(to);
        } else {
            let from = self.current();
            self.ramp = Some(Ramp {
                pos: 0,
                len: frames,
                to,
                eval: CurveRamp::new(from, to, frames, curve),
            });
        }
    }

    /// Writes the next `out.len()` values.
    pub(crate) fn fill(&mut self, out: &mut [f32]) {
        let mut rest = out;
        if let Some(r) = &mut self.ramp {
            let n = usize::try_from(r.len - r.pos)
                .unwrap_or(usize::MAX)
                .min(rest.len());
            let (head, tail) = rest.split_at_mut(n);
            r.eval.apply(r.pos, head, |o, v| *o = v);
            r.pos += n as u64;
            if r.pos >= r.len {
                self.value = r.to;
                self.ramp = None;
            }
            rest = tail;
        }
        rest.fill(self.value);
    }

    /// Skips `n` samples without producing values.
    pub(crate) fn advance(&mut self, n: usize) {
        if let Some(r) = &mut self.ramp {
            r.pos = r.pos.saturating_add(n as u64);
            if r.pos >= r.len {
                self.value = r.to;
                self.ramp = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Curve; 5] = [
        Curve::Linear,
        Curve::EqualPower,
        Curve::SCurve,
        Curve::Logarithmic,
        Curve::Exponential,
    ];

    #[test]
    fn db_conversion() {
        assert_eq!(db_to_linear(0.0), 1.0);
        assert_eq!(db_to_linear(-120.0), 0.0);
        assert_eq!(db_to_linear(-200.0), 0.0);
        assert_eq!(db_to_linear(f64::NAN), 0.0);
        assert!((db_to_linear(-6.0) - 0.501_187).abs() < 1e-6);
        assert!((db_to_linear(-119.9) - 1.0116e-6).abs() < 1e-9);
        assert_eq!(linear_to_db(0.0), SILENCE_DB);
        assert_eq!(linear_to_db(1.0), 0.0);
        assert!((linear_to_db(0.5) + 6.0206).abs() < 1e-4);
        for db in [-100.0, -40.0, -3.0, 0.0, 6.0] {
            assert!((linear_to_db(db_to_linear(db)) - db).abs() < 1e-4, "{db}");
        }
    }

    #[test]
    fn gain_ceiling_and_sanitize() {
        assert!((db_to_linear(MAX_GAIN_DB) - MAX_GAIN).abs() < 1e-5);
        assert_eq!(sanitize_gain(f32::NAN), 0.0);
        assert_eq!(sanitize_gain(1e10), MAX_GAIN);
        assert_eq!(sanitize_gain(f32::INFINITY), MAX_GAIN);
        assert_eq!(sanitize_gain(-1e10), -MAX_GAIN);
        assert_eq!(sanitize_gain(0.5), 0.5);
        assert_eq!(sanitize_gain(0.0), 0.0);
    }

    #[test]
    fn curve_shape_of_nan_is_zero() {
        for c in ALL {
            assert_eq!(c.shape(f64::NAN), 0.0, "{c:?}");
        }
    }

    #[test]
    fn curve_endpoints_exact_and_monotonic() {
        for c in ALL {
            assert_eq!(c.shape(0.0), 0.0, "{c:?}");
            assert_eq!(c.shape(1.0), 1.0, "{c:?}");
            let mut prev = 0.0;
            for i in 0..=100_000 {
                let v = c.shape(f64::from(i) / 100_000.0);
                assert!(v >= prev, "{c:?} not monotonic at {i}");
                prev = v;
            }
            // Continuity: no jump larger than the steepest slope (k·e^k/(e^k−1) ≈ 6.9) allows.
            for i in 0..1000 {
                let (a, b) = (
                    c.shape(f64::from(i) / 1000.0),
                    c.shape(f64::from(i + 1) / 1000.0),
                );
                assert!(b - a < 0.0075, "{c:?} jumps at {i}");
            }
        }
    }

    #[test]
    fn curve_midpoints_match_formulas() {
        let mid = |c: Curve| c.shape(0.5);
        assert_eq!(mid(Curve::Linear), 0.5);
        assert!((mid(Curve::EqualPower) - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-12);
        assert!((mid(Curve::SCurve) - 0.5).abs() < 1e-12);
        let e = (1000f64.sqrt() - 1.0) / 999.0;
        assert!((mid(Curve::Exponential) - e).abs() < 1e-12);
        assert!((mid(Curve::Logarithmic) - (1.0 - e)).abs() < 1e-12);
    }

    #[test]
    fn falling_ramp_is_time_reverse_of_rising() {
        for c in ALL {
            for pos in 0..=100 {
                let up = ramp_value(0.0, 1.0, 100 - pos, 100, c);
                let down = ramp_value(1.0, 0.0, pos, 100, c);
                assert!((up - down).abs() < 1e-6, "{c:?} {pos}");
            }
            assert_eq!(ramp_value(1.0, 0.0, 100, 100, c), 0.0);
            assert_eq!(ramp_value(0.0, 1.0, 0, 100, c), 0.0);
        }
        // Equal power: in² + out² = 1 throughout a crossfade.
        for pos in 0..=64 {
            let i = ramp_value(0.0, 1.0, pos, 64, Curve::EqualPower);
            let o = ramp_value(1.0, 0.0, pos, 64, Curve::EqualPower);
            assert!((i * i + o * o - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn param_fill_matches_ramp_value_and_lands_on_target() {
        let mut p = Param::new(1.0);
        p.ramp_to(0.0, 10, Curve::SCurve);
        let mut buf = [9.0; 16];
        p.fill(&mut buf[..4]);
        p.advance(2);
        p.fill(&mut buf[4..]);
        for (i, v) in buf[..4].iter().enumerate() {
            assert_eq!(*v, ramp_value(1.0, 0.0, i as u64, 10, Curve::SCurve));
        }
        assert_eq!(buf[4], ramp_value(1.0, 0.0, 6, 10, Curve::SCurve));
        assert!(buf[8..].iter().all(|v| *v == 0.0));
        assert!(p.is_silent());
    }

    #[test]
    fn ramp_from_mid_ramp_starts_at_current_value() {
        let mut p = Param::new(0.0);
        p.ramp_to(1.0, 100, Curve::Linear);
        p.advance(50);
        p.ramp_to(0.0, 10, Curve::Linear);
        let mut buf = [0.0; 1];
        p.fill(&mut buf);
        assert_eq!(buf[0], 0.5);
    }

    /// Four hours at 192 kHz: the longest ramp the tests stand in for.
    const HOURS: u64 = 4 * 3600 * 192_000;

    fn within_bound(from: f32, to: f32, len: u64, curve: Curve, pos: u64, got: f32) -> f64 {
        let want = ramp_value(from, to, pos, len, curve);
        let err = (f64::from(got) - f64::from(want)).abs();
        let scale = f64::from(from.abs().max(to.abs()));
        assert!(
            err <= RAMP_TOLERANCE * scale,
            "{curve:?} {from}->{to} len {len} pos {pos}: {got} vs {want} (err {err:e})"
        );
        if scale > 0.0 { err / scale } else { 0.0 }
    }

    fn gain() -> impl proptest::strategy::Strategy<Value = f32> {
        use proptest::prelude::*;
        prop_oneof![
            Just(0.0f32),
            Just(1.0f32),
            Just(MAX_GAIN),
            -MAX_GAIN..MAX_GAIN,
            0.0f32..1e-3,
        ]
    }

    fn length() -> impl proptest::strategy::Strategy<Value = u64> {
        use proptest::prelude::*;
        prop_oneof![1u64..=200, 1u64..=100_000, 1u64..=HOURS]
    }

    proptest::proptest! {
        /// Any position, however it is reached, is within the bound; the ends are exact.
        #[test]
        fn curve_ramp_is_within_bound_of_ramp_value(
            c in 0usize..5,
            from in gain(),
            to in gain(),
            len in length(),
            frac in 0.0f64..=1.0,
        ) {
            let curve = ALL[c];
            let r = CurveRamp::new(from, to, len, curve);
            let pos = ((len as f64 * frac) as u64).min(len);
            within_bound(from, to, len, curve, pos, r.peek(pos));
            // The worst drift is on the last frame before a knot.
            let late = (pos - pos % KNOT + KNOT - 1).min(len);
            within_bound(from, to, len, curve, late, r.peek(late));
            proptest::prop_assert_eq!(r.peek(0).to_bits(), from.to_bits());
            proptest::prop_assert_eq!(r.peek(len).to_bits(), to.to_bits());
            proptest::prop_assert_eq!(r.peek(len + 1).to_bits(), to.to_bits());
        }

        /// Values are a pure function of the position: rendering in any blocks, with skips,
        /// gives the bits a fresh evaluator gives for that position.
        #[test]
        fn curve_ramp_does_not_depend_on_how_positions_are_reached(
            c in 0usize..5,
            from in gain(),
            to in gain(),
            len in length(),
            frac in 0.0f64..1.0,
            cuts in proptest::collection::vec((1u64..300, 0u64..200), 1..12),
        ) {
            let curve = ALL[c];
            let mut r = CurveRamp::new(from, to, len, curve);
            let mut pos = (len as f64 * frac) as u64;
            for (run, skip) in cuts {
                for p in pos..pos + run {
                    let fresh = CurveRamp::new(from, to, len, curve).peek(p);
                    proptest::prop_assert_eq!(r.value(p).to_bits(), fresh.to_bits(), "pos {}", p);
                }
                pos += run + skip;
            }
        }

        /// The block evaluator gives the bits `value` gives at every position, from any start,
        /// over any span (across knots, the end and beyond), and leaves a state that agrees.
        #[test]
        fn curve_ramp_apply_matches_value_bit_for_bit(
            c in 0usize..5,
            from in gain(),
            to in gain(),
            len in proptest::prop_oneof![1u64..=200, 1u64..=100_000],
            frac in 0.0f64..=1.2,
            spans in proptest::collection::vec((0usize..300, 0u64..100), 1..8),
        ) {
            use proptest::prelude::*;
            let curve = ALL[c];
            let mut r = CurveRamp::new(from, to, len, curve);
            let mut pos = (len as f64 * frac) as u64;
            let mut buf = [0.0f32; 300];
            for (n, skip) in spans {
                let out = &mut buf[..n];
                r.apply(pos, out, |o, v| *o = v);
                for (k, v) in out.iter().enumerate() {
                    let want = CurveRamp::new(from, to, len, curve).peek(pos + k as u64);
                    prop_assert_eq!(v.to_bits(), want.to_bits(), "pos {}", pos + k as u64);
                }
                pos += n as u64;
                let want = CurveRamp::new(from, to, len, curve).peek(pos);
                prop_assert_eq!(r.value(pos).to_bits(), want.to_bits());
                pos += 1 + skip;
            }
        }

        /// `Param` cut into any blocks, with skipped stretches, follows the same values.
        #[test]
        fn param_fill_is_block_split_independent(
            c in 0usize..5,
            from in gain(),
            to in gain(),
            len in 1u64..5000,
            cuts in proptest::collection::vec((0usize..700, proptest::bool::ANY), 1..20),
        ) {
            let curve = ALL[c];
            let eval = CurveRamp::new(from, to, len, curve);
            let mut p = Param::new(from);
            p.ramp_to(to, len, curve);
            let mut pos = 0u64;
            let mut buf = [0.0f32; 700];
            for (n, skip) in cuts {
                if skip {
                    p.advance(n);
                } else {
                    let out = &mut buf[..n];
                    p.fill(out);
                    for (k, v) in out.iter().enumerate() {
                        let want = eval.peek(pos + k as u64);
                        proptest::prop_assert_eq!(v.to_bits(), want.to_bits());
                    }
                }
                pos += n as u64;
                proptest::prop_assert_eq!(p.current().to_bits(), eval.peek(pos).to_bits());
            }
        }
    }

    /// Every frame of long ramps, rendered in sequence: the drift never builds up past a knot.
    #[test]
    fn curve_ramp_streamed_over_a_long_ramp_stays_within_bound() {
        let len = 1 << 20;
        for c in ALL {
            for (from, to) in [(0.0, 1.0), (1.0, 0.0), (MAX_GAIN, 0.001), (0.25, 4.0)] {
                let mut r = CurveRamp::new(from, to, len, c);
                let mut worst: f64 = 0.0;
                for pos in 0..=len {
                    worst = worst.max(within_bound(from, to, len, c, pos, r.value(pos)));
                }
                assert!(worst <= 2.5e-7, "{c:?} {from}->{to}: worst {worst:e}");
                assert_eq!(r.value(len), to);
            }
        }
    }

    /// The last frame before every knot of a four-hour ramp, where the recurrence has run
    /// longest, and the frames around the ends.
    #[test]
    fn curve_ramp_worst_case_drift_over_hours() {
        for c in ALL {
            for (from, to) in [(0.0, 1.0), (1.0, 0.0), (MAX_GAIN, 0.0)] {
                let r = CurveRamp::new(from, to, HOURS, c);
                let mut worst: f64 = 0.0;
                let knots = HOURS / KNOT;
                for i in (0..knots).step_by((knots / 4000) as usize) {
                    let pos = i * KNOT + KNOT - 1;
                    worst = worst.max(within_bound(from, to, HOURS, c, pos, r.peek(pos)));
                }
                for pos in (1..200).chain(HOURS - 200..=HOURS) {
                    worst = worst.max(within_bound(from, to, HOURS, c, pos, r.peek(pos)));
                }
                assert!(worst <= 2.5e-7, "{c:?} {from}->{to}: worst {worst:e}");
                assert_eq!(r.peek(HOURS), to);
            }
        }
    }

    #[test]
    fn curve_ramp_of_a_new_ramp_restarts() {
        let mut r = CurveRamp::new(1.0, 0.0, 1000, Curve::SCurve);
        r.value(10);
        let v = r.value_of(0.5, 0.0, 300, Curve::EqualPower, 10);
        assert_eq!(
            v.to_bits(),
            CurveRamp::new(0.5, 0.0, 300, Curve::EqualPower)
                .peek(10)
                .to_bits()
        );
        assert!(r.is(0.5, 0.0, 300, Curve::EqualPower));
        // Linear is evaluated exactly.
        let mut l = CurveRamp::new(0.3, 0.9, 777, Curve::Linear);
        for pos in 0..=777 {
            assert_eq!(l.value(pos), ramp_value(0.3, 0.9, pos, 777, Curve::Linear));
        }
    }
}
