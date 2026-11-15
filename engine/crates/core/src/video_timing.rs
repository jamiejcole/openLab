// SPDX-License-Identifier: GPL-3.0-or-later

//! A Video cue's time and loops (VID-07; QLab 5 manual p. 214: as an Audio cue's, plus hold at
//! end): start and end trims, slice markers with play counts (0 loops until devamped), the
//! integrated fade in and out, and whether the last frame stays up when the picture ends.
//!
//! Stored in the Video cue's kind (file format §5, the same field names as an Audio cue's).
//! The engine plans a run from them as it plans an Audio cue's and sends the stage host the
//! plan as [`PlanPass`]es (docs/protocol.md, "Stage host link"); [`plan_point`] and
//! [`fade_factor`] are how the stage host and the engine's progress both read a plan, so the
//! two always agree on where a picture is and how opaque.

use serde::{Deserialize, Serialize};

use crate::cue::{FadeCurve, FadeSpec, MAX_TIME, SliceMarker};
use crate::cue_engine::curve_shape;

/// A Video cue's time and loops, stored as fields of its kind; a field missing from a file is
/// its default (the whole file, played once, no fades, not held).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoTiming {
    /// Trim in point, seconds from the start of the file.
    pub start_time: f64,
    /// Trim out point; `None` = end of file.
    pub end_time: Option<f64>,
    /// Opacity rises over this from the start of the cue's action.
    pub fade_in: FadeSpec,
    /// Opacity falls over this to the end of the cue's action (none while it loops for ever).
    pub fade_out: FadeSpec,
    /// Slice markers, as an Audio cue's.
    pub slices: Vec<SliceMarker>,
    /// Play count of the slice after the last marker; 0 = loop until devamped.
    pub last_slice_play_count: u32,
    /// At its last frame the picture stays up and the cue stays active until stopped; off, the
    /// cue ends there and its picture leaves the stage.
    pub hold_at_end: bool,
}

impl Default for VideoTiming {
    fn default() -> Self {
        let none = FadeSpec {
            duration: 0.0,
            curve: FadeCurve::Linear,
        };
        Self {
            start_time: 0.0,
            end_time: None,
            fade_in: none,
            fade_out: none,
            slices: Vec::new(),
            last_slice_play_count: 1,
            hold_at_end: false,
        }
    }
}

/// Most passes one picture plan carries: as many as an audio slice table.
pub const MAX_PLAN_PASSES: usize = 256;

/// One entry of a picture's plan: file seconds `from..from + length` played `plays` times in a
/// row (0: until devamped).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanPass {
    pub from: f64,
    pub length: f64,
    pub plays: u32,
}

impl PlanPass {
    fn ok(&self) -> bool {
        time_ok(self.from) && time_ok(self.length) && self.length > 0.0
    }

    fn end(&self) -> f64 {
        self.from + self.length
    }
}

/// A position or length in seconds: finite, 0 to 24 h.
fn time_ok(t: f64) -> bool {
    t.is_finite() && (0.0..=MAX_TIME).contains(&t)
}

/// How far before the end of the last pass a plan that has ended stands: the frame shown is the
/// last whose time is before the out point, never the one at it.
pub const HOLD_BEFORE_END: f64 = 1e-6;

/// Where a plan is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlanPoint {
    /// Seconds into the file.
    pub position: f64,
    /// Seconds until the pass under way ends (0 once the plan has ended).
    pub left: f64,
    /// Where the picture goes when this pass ends, when that is not straight on (the start of
    /// the loop again, or the slice after a loop): the jump a decoder can get ready for.
    pub next: Option<f64>,
    /// Past the plan's last pass: held just before its end.
    pub ended: bool,
}

/// Where `plan` is `elapsed` seconds after it starts playing. An empty plan has no position.
pub fn plan_point(plan: &[PlanPass], elapsed: f64) -> Option<PlanPoint> {
    let last = plan.last()?;
    let mut left = if elapsed.is_finite() {
        elapsed.max(0.0)
    } else {
        0.0
    };
    for (i, pass) in plan.iter().enumerate() {
        let after = plan.get(i + 1).map(|p| p.from);
        let whole = match pass.plays {
            0 => f64::INFINITY,
            n => pass.length * f64::from(n),
        };
        if left < whole {
            let pass_no = (left / pass.length).floor();
            let within = (left - pass_no * pass.length).clamp(0.0, pass.length);
            let more = pass.plays == 0 || pass_no + 1.0 < f64::from(pass.plays);
            let next = if more {
                Some(pass.from)
            } else {
                after.filter(|&a| (a - pass.end()).abs() > HOLD_BEFORE_END)
            };
            return Some(PlanPoint {
                position: pass.from + within,
                left: pass.length - within,
                next,
                ended: false,
            });
        }
        left -= whole;
    }
    Some(PlanPoint {
        position: (last.end() - HOLD_BEFORE_END).max(last.from),
        left: 0.0,
        next: None,
        ended: true,
    })
}

/// Seconds `plan` plays; `None` if it loops for ever.
pub fn plan_length(plan: &[PlanPass]) -> Option<f64> {
    plan.iter().try_fold(0.0, |acc, p| match p.plays {
        0 => None,
        n => Some(acc + p.length * f64::from(n)),
    })
}

/// A picture's integrated fades, in seconds of its cue's action.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PictureFades {
    pub fade_in: FadeSpec,
    pub fade_out: FadeSpec,
    /// Where the action ends (the fade out ends there); `None` while it loops for ever.
    pub end: Option<f64>,
}

impl PictureFades {
    /// No fades.
    pub const NONE: Self = Self {
        fade_in: FadeSpec {
            duration: 0.0,
            curve: FadeCurve::Linear,
        },
        fade_out: FadeSpec {
            duration: 0.0,
            curve: FadeCurve::Linear,
        },
        end: None,
    };

    fn ok(&self) -> bool {
        time_ok(self.fade_in.duration)
            && time_ok(self.fade_out.duration)
            && self.end.is_none_or(time_ok)
    }
}

/// How much of its opacity a picture has `action` seconds into its cue's action: rising on the
/// fade-in's curve from the action's start, falling on the fade-out's to its end (the time
/// reverse of the rise), the two multiplied where they overlap. 0 to 1.
pub fn fade_factor(fades: &PictureFades, action: f64) -> f64 {
    let action = if action.is_finite() { action } else { 0.0 };
    let rise = match fades.fade_in.duration {
        d if d > 0.0 => curve_shape(fades.fade_in.curve, action / d),
        _ => 1.0,
    };
    let fall = match (fades.end, fades.fade_out.duration) {
        (Some(end), d) if d > 0.0 => curve_shape(fades.fade_out.curve, (end - action) / d),
        _ => 1.0,
    };
    (rise * fall).clamp(0.0, 1.0)
}

/// Whether a plan and its fades are within the link's limits: at most [`MAX_PLAN_PASSES`]
/// passes, each in the file's 24 h and of some length, the fades and action time likewise.
pub fn picture_run_ok(plan: &[PlanPass], action: f64, fades: &PictureFades) -> bool {
    plan.len() <= MAX_PLAN_PASSES && plan.iter().all(PlanPass::ok) && time_ok(action) && fades.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass(from: f64, length: f64, plays: u32) -> PlanPass {
        PlanPass {
            from,
            length,
            plays,
        }
    }

    fn at(plan: &[PlanPass], t: f64) -> PlanPoint {
        plan_point(plan, t).unwrap()
    }

    #[test]
    fn a_trimmed_range_played_once_maps_straight_and_holds_before_its_end() {
        let plan = [pass(1.0, 2.0, 1)];
        let p = at(&plan, 0.5);
        assert_eq!((p.position, p.left, p.next, p.ended), (1.5, 1.5, None, false));
        let end = at(&plan, 2.0);
        assert!(end.ended);
        assert!(end.position < 3.0 && end.position > 2.999);
        assert!(at(&plan, 1e9).ended);
        assert_eq!(plan_length(&plan), Some(2.0));
        assert!(plan_point(&[], 1.0).is_none());
    }

    #[test]
    fn a_play_count_repeats_the_range_and_says_where_each_pass_goes_next() {
        let plan = [pass(1.0, 2.0, 3)];
        // In the first pass the next pass starts at the in point again.
        let p = at(&plan, 1.9);
        assert!((p.position - 2.9).abs() < 1e-12);
        assert_eq!(p.next, Some(1.0));
        // On the seam: the next pass's first frame.
        assert_eq!(at(&plan, 2.0).position, 1.0);
        assert_eq!(at(&plan, 4.5).position, 1.5);
        // The last pass goes nowhere: the plan ends.
        assert_eq!(at(&plan, 5.0).next, None);
        assert!(at(&plan, 6.0).ended);
        assert_eq!(plan_length(&plan), Some(6.0));
    }

    #[test]
    fn an_infinite_loop_never_ends_and_a_slice_after_a_loop_is_a_jump() {
        let plan = [pass(0.0, 1.0, 1), pass(1.0, 0.5, 0)];
        // Straight on from the first slice into the loop: no jump.
        assert_eq!(at(&plan, 0.9).next, None);
        let p = at(&plan, 1000.25);
        assert!((p.position - 1.25).abs() < 1e-9);
        assert_eq!(p.next, Some(1.0));
        assert!(!p.ended);
        assert_eq!(plan_length(&plan), None);
        // A slice that continues elsewhere after a loop (a devamp's continuation) is a jump.
        let after = [pass(1.0, 0.5, 2), pass(1.5, 1.0, 1)];
        assert_eq!(at(&after, 0.6).next, None, "straight on after its last pass");
        let gap = [pass(2.0, 0.5, 1), pass(0.0, 1.0, 1)];
        assert_eq!(at(&gap, 0.1).next, Some(0.0));
    }

    #[test]
    fn fades_rise_from_the_start_and_fall_to_the_end_on_their_curves() {
        let spec = |d| FadeSpec {
            duration: d,
            curve: FadeCurve::Linear,
        };
        let fades = PictureFades {
            fade_in: spec(1.0),
            fade_out: spec(2.0),
            end: Some(10.0),
        };
        assert_eq!(fade_factor(&fades, 0.0), 0.0);
        assert_eq!(fade_factor(&fades, 0.5), 0.5);
        assert_eq!(fade_factor(&fades, 5.0), 1.0);
        assert_eq!(fade_factor(&fades, 9.0), 0.5);
        assert_eq!(fade_factor(&fades, 10.0), 0.0);
        assert_eq!(fade_factor(&fades, 12.0), 0.0, "held at the end, faded out");
        // Looping for ever: no fade out.
        let looping = PictureFades { end: None, ..fades };
        assert_eq!(fade_factor(&looping, 1e6), 1.0);
        assert_eq!(fade_factor(&PictureFades::NONE, 0.0), 1.0);
        assert_eq!(fade_factor(&fades, f64::NAN), 0.0);
    }

    #[test]
    fn runs_outside_the_limits_are_refused() {
        let ok = [pass(0.0, 1.0, 0)];
        assert!(picture_run_ok(&ok, 0.0, &PictureFades::NONE));
        for bad in [
            pass(f64::NAN, 1.0, 1),
            pass(0.0, 0.0, 1),
            pass(-1.0, 1.0, 1),
            pass(0.0, MAX_TIME * 2.0, 1),
        ] {
            assert!(!picture_run_ok(&[bad], 0.0, &PictureFades::NONE), "{bad:?}");
        }
        assert!(!picture_run_ok(&ok, f64::INFINITY, &PictureFades::NONE));
        let many = vec![pass(0.0, 1.0, 1); MAX_PLAN_PASSES + 1];
        assert!(!picture_run_ok(&many, 0.0, &PictureFades::NONE));
        let fades = PictureFades {
            end: Some(-1.0),
            ..PictureFades::NONE
        };
        assert!(!picture_run_ok(&ok, 0.0, &fades));
    }
}
