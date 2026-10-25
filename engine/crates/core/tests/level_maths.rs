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
