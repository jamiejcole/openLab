// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The cue engine computes where an interrupted fade stands with its own copy of the audio
//! crate's gain maths (core does not depend on the audio crate). Core matches
//! `openlab_audio::ramp_value` and `Curve::shape` bit for bit; the mixer's own per-sample curve
//! evaluation is within `openlab_audio::RAMP_TOLERANCE` of `ramp_value`, so a held or resumed
//! fade continues from core's level to within that tolerance.

use openlab_core::{
    AudioDomain, CurvePoint, CurveShape, CurveType, FadeCueCurve, FadeCurve, RampShape, ShapeEval,
    curve_shape, db_to_gain, ramp_gain,
};
use proptest::prelude::*;

const CURVES: [(FadeCurve, openlab_audio::Curve); 5] = [
    (FadeCurve::Linear, openlab_audio::Curve::Linear),
    (FadeCurve::EqualPower, openlab_audio::Curve::EqualPower),
    (FadeCurve::SCurve, openlab_audio::Curve::SCurve),
    (FadeCurve::Logarithmic, openlab_audio::Curve::Logarithmic),
    (FadeCurve::Exponential, openlab_audio::Curve::Exponential),
];

#[test]
fn every_step_of_a_ramp_matches_the_mixer() {
    for (core, audio) in CURVES {
        for (from, to) in [(1.0f32, 0.0f32), (0.0, 1.0), (0.3, 0.7), (2.0, 0.5)] {
            for pos in 0..=480u64 {
                assert_eq!(
                    ramp_gain(from, to, pos, 480, core).to_bits(),
                    openlab_audio::ramp_value(from, to, pos, 480, audio).to_bits(),
                    "{core:?} {from}→{to} at {pos}"
                );
            }
        }
    }
}

proptest! {
    #[test]
    fn gains_and_shapes_match_the_mixer(
        db in -130.0f64..13.0,
        t in -0.5f64..1.5,
        from in 0.0f32..4.0,
        to in 0.0f32..4.0,
        len in 1u64..1_000_000,
        pos in 0u64..1_100_000,
        curve in 0usize..5,
    ) {
        let (core, audio) = CURVES[curve];
        prop_assert_eq!(db_to_gain(db).to_bits(), openlab_audio::db_to_linear(db).to_bits());
        prop_assert_eq!(curve_shape(core, t).to_bits(), audio.shape(t).to_bits());
        prop_assert_eq!(
            ramp_gain(from, to, pos, len, core).to_bits(),
            openlab_audio::ramp_value(from, to, pos, len, audio).to_bits()
        );
    }
}

#[test]
fn edge_values_match_the_mixer() {
    for db in [
        -120.0,
        -120.000_000_1,
        -119.999_999_9,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        0.0,
        12.0,
        24.0,
        30.0,
        f64::MAX,
    ] {
        assert_eq!(
            db_to_gain(db).to_bits(),
            openlab_audio::db_to_linear(db).to_bits(),
            "{db}"
        );
    }
    let gains = [
        0.0f32,
        1.0,
        0.5,
        // Beyond the mixer's maximum gain; the mixer clamps when it applies, not here.
        openlab_audio::MAX_GAIN * 2.0,
        f32::NAN,
        f32::INFINITY,
    ];
    for (core, audio) in CURVES {
        for t in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -0.0,
            0.0,
            1.0,
            2.0,
        ] {
            assert_eq!(
                curve_shape(core, t).to_bits(),
                audio.shape(t).to_bits(),
                "{core:?} {t}"
            );
        }
        for &from in &gains {
            for &to in &gains {
                for (pos, len) in [
                    (0, 0),
                    (1, 0),
                    (0, 1),
                    (1, 1),
                    (5, 10),
                    (10, 10),
                    (u64::MAX, u64::MAX),
                    (1, u64::MAX),
                ] {
                    assert_eq!(
                        ramp_gain(from, to, pos, len, core).to_bits(),
                        openlab_audio::ramp_value(from, to, pos, len, audio).to_bits(),
                        "{core:?} {from}→{to} {pos}/{len}"
                    );
                }
            }
        }
    }
}

/// A Fade cue's curve as the engine hands it to the mixer: every knot and tangent the same.
fn mixer_shape(r: &RampShape) -> openlab_audio::FadeShape {
    let side = |e: &ShapeEval| {
        let kind = match e.kind {
            CurveType::SCurve => openlab_audio::ShapeKind::SCurve,
            CurveType::Custom => openlab_audio::ShapeKind::Custom,
            CurveType::Parametric => openlab_audio::ShapeKind::Parametric,
            CurveType::Linear => openlab_audio::ShapeKind::Linear,
        };
        openlab_audio::ShapeSide::new(kind, e.parameter, &e.ts, &e.vs).unwrap()
    };
    let domain = match r.domain {
        AudioDomain::Slider => openlab_audio::ShapeDomain::Slider,
        AudioDomain::Decibel => openlab_audio::ShapeDomain::Decibel,
        AudioDomain::Linear => openlab_audio::ShapeDomain::Linear,
    };
    openlab_audio::FadeShape::new(domain, r.floor_db, side(&r.rising), side(&r.falling))
}

proptest! {
    #[test]
    fn fade_cue_curves_match_the_mixer(
        kind in 0usize..4,
        domain in 0usize..3,
        parameter in 0.01f64..100.0,
        points in proptest::collection::vec((1u32..999, 0u32..=1000), 0..8),
        from in 0.0f32..4.0,
        to in 0.0f32..4.0,
        len in 1u64..1_000_000,
        pos in 0u64..1_100_000,
        floor in -180.0f64..-40.0,
    ) {
        let kind = [CurveType::SCurve, CurveType::Custom, CurveType::Parametric, CurveType::Linear][kind];
        let domain = [AudioDomain::Slider, AudioDomain::Decibel, AudioDomain::Linear][domain];
        let mut points = points;
        points.sort_by_key(|p| p.0);
        points.dedup_by_key(|p| p.0);
        let shape = CurveShape {
            kind,
            parameter,
            points: points
                .into_iter()
                .map(|(t, v)| CurvePoint { t: f64::from(t) / 1000.0, v: f64::from(v) / 1000.0 })
                .collect(),
        };
        let curve = FadeCueCurve { audio_domain: domain, rising: shape.clone(), falling: shape };
        let core = RampShape::new(&curve, domain, floor);
        let mixer = mixer_shape(&core);
        prop_assert_eq!(
            core.gain(from, to, pos, len).to_bits(),
            mixer.value(from, to, pos, len).to_bits()
        );
    }
}
