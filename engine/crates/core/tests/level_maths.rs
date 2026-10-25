// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The cue engine computes where an interrupted fade stands with its own copy of the mixer's
//! gain maths (core does not depend on the audio crate). They must agree bit for bit, or a held
//! or resumed fade would jump.

use openlab_core::{FadeCurve, curve_shape, db_to_gain, ramp_gain};
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
