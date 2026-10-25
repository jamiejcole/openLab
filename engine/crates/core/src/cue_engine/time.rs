// SPDX-License-Identifier: GPL-3.0-or-later

//! Sample time: the only clock the cue engine knows (ARCHITECTURE §3.2).

use std::num::NonZeroU32;

/// Frames at the device rate since the audio device started. The audio crate uses the same count.
pub type SampleTime = u64;

/// Converts seconds (the unit of the show file) to samples at `rate`, rounding to the nearest
/// sample (halves away from zero). Every show time goes through this one function.
///
/// Validation keeps times finite and within 0 to [`MAX_TIME`](crate::MAX_TIME), but this never
/// panics on other input: negative values and NaN give 0, and values beyond `SampleTime::MAX`
/// (including +∞) saturate to it.
pub fn seconds_to_samples(seconds: f64, rate: NonZeroU32) -> SampleTime {
    if seconds.is_nan() || seconds <= 0.0 {
        return 0;
    }
    // A float-to-int `as` cast saturates, so +∞ and huge products become `SampleTime::MAX`.
    (seconds * f64::from(rate.get())).round() as SampleTime
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(hz: u32) -> NonZeroU32 {
        NonZeroU32::new(hz).unwrap()
    }

    #[test]
    fn exact_seconds_convert_exactly() {
        assert_eq!(seconds_to_samples(1.0, rate(48_000)), 48_000);
        assert_eq!(seconds_to_samples(2.5, rate(44_100)), 110_250);
        assert_eq!(seconds_to_samples(0.0, rate(48_000)), 0);
    }

    #[test]
    fn rounds_to_nearest_sample_with_halves_away_from_zero() {
        // 1 Hz makes the fractional part exact.
        assert_eq!(seconds_to_samples(0.4, rate(1)), 0);
        assert_eq!(seconds_to_samples(0.5, rate(1)), 1);
        assert_eq!(seconds_to_samples(1.49, rate(1)), 1);
        assert_eq!(seconds_to_samples(2.5, rate(1)), 3);
        // 1/3 s at 48 kHz is 16000 samples, whatever the float error.
        assert_eq!(seconds_to_samples(1.0 / 3.0, rate(48_000)), 16_000);
    }

    #[test]
    fn invalid_input_never_panics() {
        assert_eq!(seconds_to_samples(-1.0, rate(48_000)), 0);
        assert_eq!(seconds_to_samples(-0.0, rate(48_000)), 0);
        assert_eq!(seconds_to_samples(f64::NAN, rate(48_000)), 0);
        assert_eq!(seconds_to_samples(f64::NEG_INFINITY, rate(48_000)), 0);
        assert_eq!(
            seconds_to_samples(f64::INFINITY, rate(48_000)),
            SampleTime::MAX
        );
        assert_eq!(seconds_to_samples(f64::MAX, rate(384_000)), SampleTime::MAX);
    }

    #[test]
    fn longest_valid_time_fits() {
        assert_eq!(
            seconds_to_samples(crate::MAX_TIME, rate(384_000)),
            86_400 * 384_000
        );
    }
}
