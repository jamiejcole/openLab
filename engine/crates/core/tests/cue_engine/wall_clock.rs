// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wall-clock triggers (TRG-05; QLab 5 manual p. 129): which cues the local clock reaches, with
//! local time passed in as whole seconds since 1970-01-01 local (day 0 was a Thursday), and
//! what the starts do to the cues (the hotkey path: `Input::Start`).

use crate::common::*;
use openlab_core::*;

const DAY: i64 = 86_400;
/// 2026-01-05, a Monday, local midnight.
const MONDAY: i64 = 20_458 * DAY;

fn at(day: i64, h: i64, m: i64, s: i64) -> i64 {
    day + h * 3600 + m * 60 + s
}

fn clocked(mut c: Cue, h: u8, m: u8, s: u8) -> Cue {
    c.triggers.wall_clock = WallClockTrigger {
        enabled: true,
        hours: h,
        minutes: m,
        seconds: s,
        ..WallClockTrigger::default()
    };
    c
}

fn only_on(mut c: Cue, days: &[Weekday]) -> Cue {
    c.triggers.wall_clock.days = Weekdays::from(days.to_vec());
    c
}

/// The zone of these tests: eleven hours ahead of UTC.
const ZONE: i64 = 11 * 3600;

/// Local second `local` in [`ZONE`]: real and local time move together (a clock set by hand or
/// a sleep moves both).
fn wt(local: i64) -> WallTime {
    WallTime {
        utc: local - ZONE,
        local,
    }
}

/// Real second `utc` read as local second `local` (the zone's offset changed).
fn real(utc: i64, local: i64) -> WallTime {
    WallTime { utc, local }
}

/// Looks at each local time in turn (in [`ZONE`]); the cues started at each.
fn looks(watch: &mut WallClockWatch, ws: &Workspace, times: &[i64]) -> Vec<Vec<CueId>> {
    times
        .iter()
        .map(|t| watch.advance(ws, wt(*t)).start)
        .collect()
}

#[test]
fn monday_is_day_20458() {
    assert_eq!(Weekday::of_day(MONDAY / DAY), Weekday::Monday);
}

#[test]
fn a_new_cue_has_the_wall_clock_trigger_off_at_midnight_every_day() {
    let t = WallClockTrigger::default();
    assert!(!t.enabled);
    assert_eq!((t.hours, t.minutes, t.seconds), (0, 0, 0));
    assert_eq!(t.days, Weekdays::EVERY_DAY);
    // A cue left off never fires, also at its time.
    let ws = workspace(vec![memo(1)]);
    let mut w = WallClockWatch::new();
    let got = looks(&mut w, &ws, &[MONDAY - 1, MONDAY, MONDAY + 1]);
    assert!(got.iter().all(Vec::is_empty), "{got:?}");
}

#[test]
fn enable_it_for_a_time_one_minute_ahead_the_cue_starts_at_that_second_once() {
    let ws = workspace(vec![clocked(memo(1), 10, 31, 0), memo(2)]);
    let mut w = WallClockWatch::new();
    let t = at(MONDAY, 10, 30, 0);
    let got = looks(
        &mut w,
        &ws,
        &[t, t + 30, t + 59, t + 60, t + 60, t + 61, t + 62],
    );
    assert_eq!(
        got,
        vec![vec![], vec![], vec![], vec![id(1)], vec![], vec![], vec![]]
    );
}

#[test]
fn the_trigger_starts_the_cue_as_a_hotkey_does_and_a_running_cue_is_not_restarted() {
    let mut rig = Rig::new(vec![clocked(sound(1), 10, 31, 0), memo(2)]);
    let mut w = WallClockWatch::new();
    let t = at(MONDAY, 10, 30, 59);
    let mut fire = |rig: &mut Rig, wall: i64, now: SampleTime| {
        let cues = w.advance(&rig.ws.clone(), wt(wall)).start;
        cues.into_iter()
            .flat_map(|c| rig.at(now, Input::Start { cue: c }))
            .collect::<Vec<_>>()
    };
    assert!(fire(&mut rig, t, 0).is_empty());
    let first = fire(&mut rig, t + 1, 10);
    assert_eq!(voice_starts(&first), vec![(id(1), 10)]);
    assert_eq!(rig.engine.state(id(1)), CueState::Running);
    // Playhead not moved (manual pp. 146-147): it still stands on cue 1.
    assert_eq!(rig.engine.playhead(list()), Some(id(1)));
    // Its time moved a second on, reached while cue 1 still runs: the second trigger "does
    // nothing", no second voice.
    rig.ws.cue_lists[0].cues[0].triggers.wall_clock.seconds = 1;
    let again = fire(&mut rig, t + 2, 20);
    assert!(voice_starts(&again).is_empty());
    assert_eq!(rig.engine.state(id(1)), CueState::Running);
}

#[test]
fn a_time_already_passed_when_the_workspace_opens_does_not_fire() {
    let ws = workspace(vec![clocked(memo(1), 10, 30, 0)]);
    let mut w = WallClockWatch::new();
    let t = at(MONDAY, 10, 30, 1);
    let got = looks(&mut w, &ws, &[t, t + 1, t + 2]);
    assert!(got.iter().all(Vec::is_empty), "{got:?}");
    // It fires the next day.
    let next = at(MONDAY + DAY, 10, 29, 59);
    let got = looks(&mut w, &ws, &[next - 1, next, next + 1]);
    // The first look is a jump (most of a day): nothing; then the second is crossed.
    assert_eq!(got, vec![vec![], vec![], vec![id(1)]]);
}

#[test]
fn the_first_look_on_the_very_second_does_not_fire() {
    let ws = workspace(vec![clocked(memo(1), 10, 30, 0)]);
    let mut w = WallClockWatch::new();
    let t = at(MONDAY, 10, 30, 0);
    assert!(w.advance(&ws, wt(t)).start.is_empty());
    assert!(w.advance(&ws, wt(t + 1)).start.is_empty());
}

#[test]
fn day_of_week_limited_to_other_days_does_not_fire_and_a_chosen_day_does() {
    let ws = workspace(vec![
        only_on(clocked(memo(1), 9, 0, 0), &[Weekday::Tuesday]),
        only_on(
            clocked(memo(2), 9, 0, 0),
            &[Weekday::Monday, Weekday::Friday],
        ),
    ]);
    let mut w = WallClockWatch::new();
    let mon = at(MONDAY, 9, 0, 0);
    assert_eq!(
        looks(&mut w, &ws, &[mon - 1, mon]),
        vec![vec![], vec![id(2)]]
    );
    let tue = at(MONDAY + DAY, 9, 0, 0);
    assert_eq!(
        looks(&mut w, &ws, &[tue - 1, tue]),
        vec![vec![], vec![id(1)]]
    );
    let wed = at(MONDAY + 2 * DAY, 9, 0, 0);
    assert_eq!(looks(&mut w, &ws, &[wed - 1, wed]), vec![vec![], vec![]]);
}

#[test]
fn midnight_counts_on_the_new_day() {
    // 00:00:00 only on Tuesdays: reached going from Monday 23:59:59 into Tuesday.
    let ws = workspace(vec![
        only_on(clocked(memo(1), 0, 0, 0), &[Weekday::Tuesday]),
        only_on(clocked(memo(2), 0, 0, 0), &[Weekday::Monday]),
        clocked(memo(3), 23, 59, 59),
    ]);
    let mut w = WallClockWatch::new();
    let tue = MONDAY + DAY;
    let got = looks(&mut w, &ws, &[tue - 2, tue - 1, tue, tue + 1]);
    assert_eq!(got, vec![vec![], vec![id(3)], vec![id(1)], vec![]]);
    // A two-second step over midnight still fires both sides of it.
    let mut w = WallClockWatch::new();
    let got = looks(&mut w, &ws, &[tue - 2, tue]);
    assert_eq!(got, vec![vec![], vec![id(1), id(3)]]);
}

#[test]
fn a_step_of_up_to_two_seconds_fires_what_it_crossed_and_a_longer_one_fires_nothing() {
    let ws = workspace(vec![clocked(memo(1), 10, 30, 0)]);
    let t = at(MONDAY, 10, 30, 0);
    let mut w = WallClockWatch::new();
    assert_eq!(looks(&mut w, &ws, &[t - 2, t]), vec![vec![], vec![id(1)]]);
    let mut w = WallClockWatch::new();
    assert_eq!(
        looks(&mut w, &ws, &[t - 1, t + 1]),
        vec![vec![], vec![id(1)]]
    );
    // Three seconds: the control thread was held up, or the clock was set: missed, reported.
    let mut w = WallClockWatch::new();
    w.advance(&ws, wt(t - 1));
    let step = w.advance(&ws, wt(t + 2));
    assert!(step.start.is_empty());
    assert_eq!(step.missed, vec![id(1)]);
    assert_eq!(
        step.jump,
        Some(WallClockJump {
            from: t - 1,
            to: t + 2,
            real: 3,
        })
    );
    assert!(w.advance(&ws, wt(t + 3)).start.is_empty());
}

#[test]
fn a_trigger_missed_while_asleep_does_not_fire_later() {
    let ws = workspace(vec![
        clocked(memo(1), 10, 30, 0),
        clocked(memo(2), 12, 0, 0),
    ]);
    let mut w = WallClockWatch::new();
    let before = at(MONDAY, 10, 0, 0);
    w.advance(&ws, wt(before));
    // Asleep from 10:00 to 11:00; the look on waking reports cue 1 missed, cue 2 not yet.
    let woke = at(MONDAY, 11, 0, 0);
    let step = w.advance(&ws, wt(woke));
    assert!(step.start.is_empty());
    assert_eq!(step.missed, vec![id(1)]);
    let got = looks(&mut w, &ws, &[woke + 1, woke + 2]);
    assert!(got.iter().all(Vec::is_empty));
    // Cue 2 still fires at its time.
    let noon = at(MONDAY, 12, 0, 0);
    assert_eq!(
        looks(&mut w, &ws, &[noon - 1, noon]),
        vec![vec![], vec![id(2)]]
    );
}

#[test]
fn a_sleep_of_days_reports_each_missed_cue_once() {
    let ws = workspace(vec![clocked(memo(1), 10, 30, 0), memo(2)]);
    let mut w = WallClockWatch::new();
    w.advance(&ws, wt(MONDAY));
    let step = w.advance(&ws, wt(MONDAY + 30 * DAY));
    assert!(step.start.is_empty());
    assert_eq!(step.missed, vec![id(1)]);
}

#[test]
fn daylight_saving_end_repeats_the_hour_but_the_cue_fires_once() {
    // Real time goes on a second at a time; the local clock goes from 02:59:59 back to 02:00:00
    // (the hour is lived twice).
    let ws = workspace(vec![clocked(memo(1), 2, 30, 0), clocked(memo(2), 2, 0, 0)]);
    let mut w = WallClockWatch::new();
    let start = at(MONDAY, 1, 59, 59);
    let mut got = Vec::new();
    let mut utc = start - ZONE;
    // First pass: 01:59:59 to 02:59:59 at +11 h.
    for local in start..=at(MONDAY, 2, 59, 59) {
        got.push((local, w.advance(&ws, real(utc, local))));
        utc += 1;
    }
    // Then the offset drops an hour: 02:00:00 again, to 03:00:00.
    for local in at(MONDAY, 2, 0, 0)..=at(MONDAY, 3, 0, 0) {
        got.push((local, w.advance(&ws, real(utc, local))));
        utc += 1;
    }
    let fired: Vec<(i64, Vec<CueId>)> = got
        .iter()
        .filter(|(_, s)| !s.start.is_empty())
        .map(|(t, s)| (*t, s.start.clone()))
        .collect();
    assert_eq!(
        fired,
        vec![
            (at(MONDAY, 2, 0, 0), vec![id(2)]),
            (at(MONDAY, 2, 30, 0), vec![id(1)])
        ],
        "each fires on its first pass only"
    );
    let back: Vec<_> = got.iter().filter_map(|(_, s)| s.jump).collect();
    assert_eq!(
        back,
        vec![WallClockJump {
            from: at(MONDAY, 2, 59, 59),
            to: at(MONDAY, 2, 0, 0),
            real: 1,
        }]
    );
    assert!(got.iter().all(|(_, s)| s.missed.is_empty()));
    // The next day both fire again.
    let next = at(MONDAY + DAY, 2, 0, 0);
    let mut w2 = w.clone();
    w2.advance(&ws, real(utc + DAY - 3600, next - 1));
    assert_eq!(
        w2.advance(&ws, real(utc + DAY - 3599, next)).start,
        vec![id(2)]
    );
}

/// Daylight saving starts at 02:00 (Sydney: the clock goes from 01:59:59 to 03:00:00 in one
/// real second): a cue in the skipped hour is missed and reported; one at 03:00:00, the first
/// real second after, fires on that second.
#[test]
fn daylight_saving_start_fires_the_first_second_after_the_skip_and_misses_the_skipped_hour() {
    let ws = workspace(vec![
        clocked(memo(1), 2, 30, 0),
        clocked(memo(2), 3, 0, 0),
        clocked(memo(3), 3, 0, 1),
        clocked(memo(4), 2, 59, 59),
    ]);
    let mut w = WallClockWatch::new();
    let utc = at(MONDAY, 1, 59, 59) - 10 * 3600;
    w.advance(&ws, real(utc, at(MONDAY, 1, 59, 59)));
    let step = w.advance(&ws, real(utc + 1, at(MONDAY, 3, 0, 0)));
    assert_eq!(step.start, vec![id(2)]);
    assert_eq!(step.missed, vec![id(1), id(4)]);
    assert_eq!(step.jump.map(|j| j.real), Some(1));
    let next = w.advance(&ws, real(utc + 2, at(MONDAY, 3, 0, 1)));
    assert_eq!(next.start, vec![id(3)]);
    assert!(next.jump.is_none() && next.missed.is_empty());
}

/// Zones that change at midnight: 23:59:59 to 01:00:00. A cue at 01:00:00 fires; one at 00:30
/// is missed; one at midnight (never lived that day) is missed too.
#[test]
fn a_change_at_midnight_fires_01_00_00_and_misses_the_skipped_hour() {
    let ws = workspace(vec![
        clocked(memo(1), 0, 30, 0),
        clocked(memo(2), 1, 0, 0),
        clocked(memo(3), 0, 0, 0),
    ]);
    let mut w = WallClockWatch::new();
    let utc = 1_000_000;
    w.advance(&ws, real(utc, MONDAY + DAY - 1));
    let step = w.advance(&ws, real(utc + 1, at(MONDAY + DAY, 1, 0, 0)));
    assert_eq!(step.start, vec![id(2)]);
    assert_eq!(step.missed, vec![id(1), id(3)]);
}

/// The operator changes the computer's zone while the show runs: five hours on fires only the
/// new local second (15:00:00 here); the hours skipped are missed. Five hours back fires nothing on the
/// way back, nothing already fired again, and a time not yet reached that day fires when the
/// clock reaches it again.
#[test]
fn a_zone_changed_while_running_fires_only_the_seconds_lived() {
    let ws = workspace(vec![
        clocked(memo(1), 12, 0, 0),
        clocked(memo(2), 15, 0, 0),
        clocked(memo(3), 10, 0, 1),
    ]);
    let mut w = WallClockWatch::new();
    let utc = 5_000_000;
    let ten = at(MONDAY, 10, 0, 0);
    w.advance(&ws, real(utc, ten - 1));
    w.advance(&ws, real(utc + 1, ten));
    let on = w.advance(&ws, real(utc + 2, ten + 5 * 3600));
    assert_eq!(on.start, vec![id(2)], "15:00:00 is the second lived");
    assert_eq!(on.missed, vec![id(1), id(3)]);
    let back = w.advance(&ws, real(utc + 3, ten + 1));
    assert!(back.start.is_empty() && back.missed.is_empty());
    assert!(back.jump.is_some());
    // 10:00:01 again: cue 3 was passed over, never fired, so it fires now as the clock lives it.
    let again = w.advance(&ws, real(utc + 4, ten + 2));
    assert!(
        again.start.is_empty(),
        "10:00:01 was the second before: already behind"
    );
    let mut w = WallClockWatch::new();
    w.advance(&ws, real(utc, ten - 1));
    w.advance(&ws, real(utc + 1, ten + 5 * 3600));
    assert_eq!(
        w.advance(&ws, real(utc + 2, ten)).start,
        Vec::<CueId>::new()
    );
    assert_eq!(w.advance(&ws, real(utc + 3, ten + 1)).start, vec![id(3)]);
}

/// The control thread held up for three real seconds while the local clock moved three: the
/// step does not count, even at a daylight-saving start (real time decides).
#[test]
fn a_held_up_look_over_a_daylight_saving_start_fires_nothing() {
    let ws = workspace(vec![clocked(memo(1), 3, 0, 1)]);
    let mut w = WallClockWatch::new();
    let utc = 7_000_000;
    w.advance(&ws, real(utc, at(MONDAY, 1, 59, 59)));
    let step = w.advance(&ws, real(utc + 3, at(MONDAY, 3, 0, 2)));
    assert!(step.start.is_empty());
    assert_eq!(step.missed, vec![id(1)]);
}

/// The clock set forward by hand an hour (real time and local time jump together): nothing
/// fires, the cue passed over is reported.
#[test]
fn the_clock_set_forward_by_hand_fires_nothing_and_reports_the_cue() {
    let ws = workspace(vec![clocked(memo(1), 10, 30, 0)]);
    let mut w = WallClockWatch::new();
    let t = at(MONDAY, 10, 0, 0);
    w.advance(&ws, wt(t));
    let step = w.advance(&ws, wt(t + 3600));
    assert!(step.start.is_empty());
    assert_eq!(step.missed, vec![id(1)]);
}

#[test]
fn the_clock_set_back_a_few_seconds_does_not_fire_the_same_second_twice() {
    let ws = workspace(vec![clocked(memo(1), 10, 30, 0)]);
    let mut w = WallClockWatch::new();
    let t = at(MONDAY, 10, 30, 0);
    let got = looks(
        &mut w,
        &ws,
        &[t - 1, t, t + 1, t - 3, t - 2, t - 1, t, t + 1],
    );
    assert_eq!(got.iter().filter(|s| !s.is_empty()).count(), 1, "{got:?}");
}

#[test]
fn the_clock_set_back_a_day_fires_at_the_time_on_that_day() {
    // Another date's second: the operator put the clock back to yesterday and the show runs
    // that day again.
    let ws = workspace(vec![clocked(memo(1), 10, 30, 0)]);
    let mut w = WallClockWatch::new();
    let t = at(MONDAY + DAY, 10, 30, 0);
    assert_eq!(looks(&mut w, &ws, &[t - 1, t]), vec![vec![], vec![id(1)]]);
    let y = t - DAY;
    assert_eq!(looks(&mut w, &ws, &[y - 1, y]), vec![vec![], vec![id(1)]]);
}

#[test]
fn a_disabled_trigger_or_a_time_out_of_range_never_fires() {
    let mut off = clocked(memo(1), 10, 30, 0);
    off.triggers.wall_clock.enabled = false;
    let bad = clocked(memo(2), 24, 30, 0);
    let no_days = only_on(clocked(memo(3), 10, 30, 0), &[]);
    let ws = workspace(vec![off, bad, no_days]);
    let mut w = WallClockWatch::new();
    let t = at(MONDAY, 10, 30, 0);
    let got = looks(&mut w, &ws, &[t - 1, t, t + DAY / 24]);
    assert!(got.iter().all(Vec::is_empty), "{got:?}");
}

#[test]
fn a_time_moved_later_after_it_fired_fires_again_at_the_new_time() {
    let mut ws = workspace(vec![clocked(memo(1), 10, 30, 0)]);
    let mut w = WallClockWatch::new();
    let t = at(MONDAY, 10, 30, 0);
    assert_eq!(looks(&mut w, &ws, &[t - 1, t]), vec![vec![], vec![id(1)]]);
    ws.cue_lists[0].cues[0].triggers.wall_clock.seconds = 2;
    assert_eq!(
        looks(&mut w, &ws, &[t + 1, t + 2, t + 3]),
        vec![vec![], vec![id(1)], vec![]]
    );
}

#[test]
fn cues_sharing_a_time_all_start_in_workspace_order_children_included() {
    let ws = workspace(vec![
        clocked(memo(3), 8, 0, 0),
        cue(10, group(vec![memo(11), clocked(memo(12), 8, 0, 0)])),
        clocked(memo(1), 8, 0, 0),
    ]);
    let mut w = WallClockWatch::new();
    let t = at(MONDAY, 8, 0, 0);
    assert_eq!(
        looks(&mut w, &ws, &[t - 1, t]),
        vec![vec![], vec![id(3), id(12), id(1)]]
    );
}

#[test]
fn extreme_clock_values_are_total() {
    let ws = workspace(vec![clocked(memo(1), 23, 59, 59)]);
    let mut w = WallClockWatch::new();
    for t in [
        i64::MIN,
        i64::MIN + 1,
        -1,
        0,
        i64::MAX - 1,
        i64::MAX,
        i64::MIN,
        0,
    ] {
        for utc in [i64::MIN, 0, i64::MAX] {
            let _ = w.advance(&ws, real(utc, t));
        }
    }
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(prop_cases(256)))]

    /// Any run of looks (real steps from back to a long sleep, local steps from an hour back to
    /// a zone change on): a cue starts only on a counting step whose local clock moved on, at
    /// a time in the last real seconds of it, and never twice for one local instant.
    #[test]
    fn any_clock_never_fires_a_cue_twice_or_outside_a_counting_step(
        steps in proptest::collection::vec((-3i64..6, -4000i64..4000), 1..200),
    ) {
        let ws = workspace(vec![
            clocked(memo(1), 10, 0, 0),
            clocked(memo(2), 10, 0, 1),
            only_on(clocked(memo(3), 10, 30, 0), &[Weekday::Monday]),
        ]);
        let sod = |n: u128| match n {
            1 => 36_000,
            2 => 36_001,
            _ => 37_800,
        };
        let mut w = WallClockWatch::new();
        let mut now = real(1_000_000, at(MONDAY, 9, 59, 50));
        w.advance(&ws, now);
        let mut seen = std::collections::HashSet::new();
        for (real_step, extra) in steps {
            // Mostly the local clock moves with real time; sometimes the offset changes.
            let local_step = if extra.abs() < 3000 { real_step } else { real_step + extra };
            let next = real(now.utc + real_step, now.local + local_step);
            let step = w.advance(&ws, next);
            let moved = next.local - now.local;
            if !step.start.is_empty() {
                proptest::prop_assert!((0..=MAX_GAP_SECS).contains(&real_step) && moved > 0);
            }
            for cue in &step.start {
                let n = cue.0.as_u128();
                let at = next.local - (next.local - sod(n)).rem_euclid(DAY);
                proptest::prop_assert!(next.local - at < real_step.max(1), "fired late: {n}");
                proptest::prop_assert!(seen.insert((n, at)), "fired twice: {n} at {at}");
            }
            now = next;
        }
    }
}
