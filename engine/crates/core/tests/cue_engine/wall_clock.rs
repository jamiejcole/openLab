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

/// Looks at each time in turn; the cues started at each.
fn looks(watch: &mut WallClockWatch, ws: &Workspace, times: &[i64]) -> Vec<Vec<CueId>> {
    times.iter().map(|t| watch.advance(ws, *t).start).collect()
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
    let got = looks(&mut w, &ws, &[t, t + 30, t + 59, t + 60, t + 60, t + 61, t + 62]);
    assert_eq!(
        got,
        vec![
            vec![],
            vec![],
            vec![],
            vec![id(1)],
            vec![],
            vec![],
            vec![]
        ]
    );
}

#[test]
fn the_trigger_starts_the_cue_as_a_hotkey_does_and_a_running_cue_is_not_restarted() {
    let mut rig = Rig::new(vec![clocked(sound(1), 10, 31, 0), memo(2)]);
    let mut w = WallClockWatch::new();
    let t = at(MONDAY, 10, 30, 59);
    let mut fire = |rig: &mut Rig, wall: i64, now: SampleTime| {
        let cues = w.advance(&rig.ws.clone(), wall).start;
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
    assert!(w.advance(&ws, t).start.is_empty());
    assert!(w.advance(&ws, t + 1).start.is_empty());
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
    assert_eq!(looks(&mut w, &ws, &[mon - 1, mon]), vec![vec![], vec![id(2)]]);
    let tue = at(MONDAY + DAY, 9, 0, 0);
    assert_eq!(looks(&mut w, &ws, &[tue - 1, tue]), vec![vec![], vec![id(1)]]);
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
    assert_eq!(looks(&mut w, &ws, &[t - 1, t + 1]), vec![vec![], vec![id(1)]]);
    // Three seconds: the control thread was held up, or the clock was set: missed, reported.
    let mut w = WallClockWatch::new();
    w.advance(&ws, t - 1);
    let step = w.advance(&ws, t + 2);
    assert!(step.start.is_empty());
    assert_eq!(
        step.jump,
        Some(WallClockJump {
            from: t - 1,
            to: t + 2,
            missed: vec![id(1)],
        })
    );
    assert!(w.advance(&ws, t + 3).start.is_empty());
}

#[test]
fn a_trigger_missed_while_asleep_does_not_fire_later() {
    let ws = workspace(vec![clocked(memo(1), 10, 30, 0), clocked(memo(2), 12, 0, 0)]);
    let mut w = WallClockWatch::new();
    let before = at(MONDAY, 10, 0, 0);
    w.advance(&ws, before);
    // Asleep from 10:00 to 11:00; the look on waking reports cue 1 missed, cue 2 not yet.
    let woke = at(MONDAY, 11, 0, 0);
    let step = w.advance(&ws, woke);
    assert!(step.start.is_empty());
    assert_eq!(step.jump.unwrap().missed, vec![id(1)]);
    let got = looks(&mut w, &ws, &[woke + 1, woke + 2]);
    assert!(got.iter().all(Vec::is_empty));
    // Cue 2 still fires at its time.
    let noon = at(MONDAY, 12, 0, 0);
    assert_eq!(looks(&mut w, &ws, &[noon - 1, noon]), vec![vec![], vec![id(2)]]);
}

#[test]
fn a_sleep_of_days_reports_each_missed_cue_once() {
    let ws = workspace(vec![clocked(memo(1), 10, 30, 0), memo(2)]);
    let mut w = WallClockWatch::new();
    w.advance(&ws, MONDAY);
    let step = w.advance(&ws, MONDAY + 30 * DAY);
    assert!(step.start.is_empty());
    assert_eq!(step.jump.unwrap().missed, vec![id(1)]);
}

#[test]
fn daylight_saving_end_repeats_the_hour_but_the_cue_fires_once() {
    // Local clock: 02:59:59 then back to 02:00:00 (the hour is lived twice).
    let ws = workspace(vec![clocked(memo(1), 2, 30, 0)]);
    let mut w = WallClockWatch::new();
    let t = at(MONDAY, 2, 30, 0);
    assert_eq!(looks(&mut w, &ws, &[t - 1, t]), vec![vec![], vec![id(1)]]);
    let late = at(MONDAY, 2, 59, 59);
    w.advance(&ws, late - 1);
    w.advance(&ws, late);
    let back = w.advance(&ws, at(MONDAY, 2, 0, 0));
    assert!(back.start.is_empty());
    assert_eq!(back.jump.unwrap().to, at(MONDAY, 2, 0, 0));
    // The repeated 02:30:00 does not fire again.
    let got = looks(&mut w, &ws, &[t - 1, t, t + 1]);
    assert!(got.iter().all(Vec::is_empty), "{got:?}");
    // The next day it does.
    let next = t + DAY;
    let mut w2 = w.clone();
    w2.advance(&ws, next - 1);
    assert_eq!(w2.advance(&ws, next).start, vec![id(1)]);
}

#[test]
fn daylight_saving_start_skips_the_hour_and_its_cue_is_missed_not_fired_late() {
    let ws = workspace(vec![clocked(memo(1), 2, 30, 0), clocked(memo(2), 3, 0, 1)]);
    let mut w = WallClockWatch::new();
    w.advance(&ws, at(MONDAY, 1, 59, 59));
    let step = w.advance(&ws, at(MONDAY, 3, 0, 0));
    assert!(step.start.is_empty());
    assert_eq!(step.jump.unwrap().missed, vec![id(1)]);
    assert_eq!(w.advance(&ws, at(MONDAY, 3, 0, 1)).start, vec![id(2)]);
}

#[test]
fn the_clock_set_back_a_few_seconds_does_not_fire_the_same_second_twice() {
    let ws = workspace(vec![clocked(memo(1), 10, 30, 0)]);
    let mut w = WallClockWatch::new();
    let t = at(MONDAY, 10, 30, 0);
    let got = looks(&mut w, &ws, &[t - 1, t, t + 1, t - 3, t - 2, t - 1, t, t + 1]);
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
    assert_eq!(
        looks(&mut w, &ws, &[y - 1, y]),
        vec![vec![], vec![id(1)]]
    );
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
    for t in [i64::MIN, i64::MIN + 1, -1, 0, i64::MAX - 1, i64::MAX, i64::MIN, 0] {
        let _ = w.advance(&ws, t);
    }
}
