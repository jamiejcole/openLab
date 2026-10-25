// SPDX-License-Identifier: GPL-3.0-or-later

//! Gain helpers: dB conversion, fade curves and per-sample gain ramps. Formulas are documented in
//! `docs/audio.md`; keep the two in sync.

/// Levels at or below this are silence (ARCHITECTURE §4).
pub const SILENCE_DB: f64 = -120.0;

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
    /// Rising shape `f(t)` for `t` in `[0, 1]` (clamped).
    pub fn shape(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
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

#[derive(Clone, Copy, Debug)]
struct Ramp {
    from: f32,
    to: f32,
    pos: u64,
    len: u64,
    curve: Curve,
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
            Some(r) => ramp_value(r.from, r.to, r.pos, r.len, r.curve),
            None => self.value,
        }
    }

    /// Constant zero: contributes nothing and needs no per-sample work.
    pub(crate) fn is_silent(&self) -> bool {
        self.ramp.is_none() && self.value == 0.0
    }

    pub(crate) fn is_ramping(&self) -> bool {
        self.ramp.is_some()
    }

    pub(crate) fn set(&mut self, value: f32) {
        self.value = value;
        self.ramp = None;
    }

    /// Ramps from the current value to `to`, starting at the next sample.
    pub(crate) fn ramp_to(&mut self, to: f32, frames: u64, curve: Curve) {
        if frames == 0 {
            self.set(to);
        } else {
            let from = self.current();
            self.ramp = Some(Ramp {
                from,
                to,
                pos: 0,
                len: frames,
                curve,
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
            for o in head {
                *o = ramp_value(r.from, r.to, r.pos, r.len, r.curve);
                r.pos += 1;
            }
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
}
