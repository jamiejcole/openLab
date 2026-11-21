// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wall-clock triggers (TRG-05; QLab 5 manual p. 129, pp. 146-147) through the control loop: the
//! engine's epoch clock is a `ManualClock` the test sets, the zone a fixed or scripted offset,
//! and each `run(BLOCK)` is one wake. Local time is UTC plus the zone's offset.

use crate::show::*;
use openlab_core::protocol::{
    ClientMessage, LogEntry, LogKind, LogLevel, Outcome, Request, ServerMessage, TransportAction,
    WireWarning,
};
use openlab_core::{
    Command, CueKind, CueProperty, CueState, Triggers, WallClockTrigger, Weekday, Weekdays,
    Workspace, WorkspaceId,
};
use openlab_engine::control::offline::OfflineShow;
use openlab_engine::control::wall_clock::LocalZone;
use openlab_engine::control::{DocSource, ManualClock};
use uuid::Uuid;

const DAY: i64 = 86_400;
/// 2026-01-05, a Monday: local midnight in local seconds.
const MONDAY: i64 = 20_458 * DAY;
/// The zone of most tests: eleven hours ahead of UTC (Sydney in summer).
const ZONE: i64 = 11 * 3600;

/// Epoch milliseconds at local time `local` (seconds) plus `ms`, in a zone `zone` ahead of UTC.
fn utc_ms(local: i64, zone: i64, ms: i64) -> i64 {
    (local - zone) * 1000 + ms
}

fn hms(day: i64, h: i64, m: i64, s: i64) -> i64 {
    day + h * 3600 + m * 60 + s
}

fn clocked(h: u8, m: u8, s: u8) -> WallClockTrigger {
    WallClockTrigger {
        enabled: true,
        hours: h,
        minutes: m,
        seconds: s,
        ..WallClockTrigger::default()
    }
}

/// A show of `cues` whose wall clock reads local `local` in [`ZONE`].
fn clock_show(cues: Vec<openlab_core::Cue>, local: i64) -> (OfflineShow, ManualClock) {
    zone_show(workspace(cues), local, LocalZone::fixed(ZONE), ZONE)
}

fn zone_show(
    ws: Workspace,
    local: i64,
    zone: LocalZone,
    offset_now: i64,
) -> (OfflineShow, ManualClock) {
    let clock = ManualClock::new(utc_ms(local, offset_now, 0));
    let mut c = config(&ws);
    c.clock = clock.clock();
    c.local_zone = zone;
    let mut s = show_with(ws, c);
    s.run(BLOCK);
    s.take_outbound();
    (s, clock)
}

/// Sets the clock to local `local` (in [`ZONE`]) and runs one wake.
fn at(s: &mut OfflineShow, clock: &ManualClock, local: i64, ms: i64) {
    clock.set(utc_ms(local, ZONE, ms));
    s.run(BLOCK);
}

fn set_triggers(
    s: &mut OfflineShow,
    n: u128,
    edit: impl FnOnce(&mut Triggers),
) -> Vec<ServerMessage> {
    let mut triggers = s
        .control()
        .workspace()
        .find_cue(id(n))
        .unwrap()
        .triggers
        .clone();
    edit(&mut triggers);
    let rid = s.request_id();
    s.send(ClientMessage::Command {
        id: rid,
        command: Box::new(Command::SetCueProperty {
            id: id(n),
            property: CueProperty::Triggers { value: triggers },
        }),
        coalesce: None,
    })
}

fn outcome_ok(replies: &[ServerMessage]) -> bool {
    replies.iter().any(|m| {
        matches!(
            m,
            ServerMessage::Result {
                outcome: Outcome::Ok(_),
                ..
            }
        )
    })
}

fn wall_clock_log(s: &OfflineShow) -> Vec<LogEntry> {
    s.control()
        .show_log()
        .tail(500)
        .into_iter()
        .filter(|e| e.kind == LogKind::Trigger && e.source.as_deref() == Some("wall clock"))
        .collect()
}

fn tone() -> std::path::PathBuf {
    wav(&dc(4000, 0.25))
}

/// Flow: enable the trigger for a time one minute ahead with the workspace open; at that second
/// the cue starts on the sample a Start would have started it on, the playhead stays, and no
/// second start comes in that second or on the next wake.
#[test]
fn enable_it_for_a_time_one_minute_ahead_the_cue_starts_at_that_second_once() {
    let path = tone();
    let now = hms(MONDAY, 10, 30, 0);
    let (mut s, clock) = clock_show(vec![audio(1, &path, 0), audio(2, &path, 1)], now);
    let replies = set_triggers(&mut s, 1, |t| t.wall_clock = clocked(10, 31, 0));
    assert!(outcome_ok(&replies));
    for second in 1..60 {
        at(&mut s, &clock, now + second, 0);
    }
    at(&mut s, &clock, now + 59, 999);
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert!(s.channel(0).iter().all(|&v| v == 0.0));
    let before = s.now() as usize;
    at(&mut s, &clock, now + 60, 0);
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    assert_eq!(s.control().engine().playhead(list_id()), Some(id(1)));
    // More wakes in the same second and the next: still one voice.
    at(&mut s, &clock, now + 60, 500);
    at(&mut s, &clock, now + 61, 0);
    at(&mut s, &clock, now + 61, 10);
    assert_eq!(s.control().live_voices(), 1);
    let started = first_nonzero(&s.channel(0)).expect("the cue played");

    // The same cue started with Start on the same wake of a twin show starts on the same sample.
    let (mut twin, tclock) = clock_show(vec![audio(1, &path, 0), audio(2, &path, 1)], now);
    for second in 1..60 {
        at(&mut twin, &tclock, now + second, 0);
    }
    at(&mut twin, &tclock, now + 59, 999);
    at(&mut twin, &tclock, now + 60, 0);
    assert_eq!(twin.now() as usize - BLOCK as usize, before);
    start(&mut twin, 1);
    twin.run(BLOCK * 4);
    s.run(BLOCK * 3);
    assert_eq!(first_nonzero(&twin.channel(0)), Some(started));
    let log = wall_clock_log(&s);
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(log[0].level, LogLevel::Info);
    assert_eq!(log[0].cue, Some(id(1)));
    assert!(
        log[0].message.starts_with("wall clock 10:31:00"),
        "{}",
        log[0].message
    );
}

/// Flow: days limited to other days do not start the cue; a chosen day does. Local time is the
/// zone's: at 01:00 on a Tuesday in a zone two hours ahead it is still Monday in UTC.
#[test]
fn day_of_week_limited_to_other_days_does_not_fire_and_a_chosen_day_does_in_local_time() {
    let path = tone();
    let mut tue = audio(1, &path, 0);
    tue.triggers.wall_clock = clocked(1, 0, 0);
    tue.triggers.wall_clock.days = Weekdays::NONE.with(Weekday::Tuesday, true);
    let mut mon = audio(2, &path, 1);
    mon.triggers.wall_clock = clocked(1, 0, 0);
    mon.triggers.wall_clock.days = Weekdays::NONE.with(Weekday::Monday, true);
    let zone = 2 * 3600;
    let local = hms(MONDAY + DAY, 0, 59, 59);
    let (mut s, clock) = zone_show(
        workspace(vec![tue, mon]),
        local,
        LocalZone::fixed(zone),
        zone,
    );
    clock.set(utc_ms(local + 1, zone, 0));
    s.run(BLOCK);
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    assert_eq!(s.control().engine().state(id(2)), CueState::Idle);
    s.run(BLOCK * 4);
    assert!(
        s.channel(1).iter().all(|&v| v == 0.0),
        "Monday's cue played on Tuesday"
    );
}

/// Flow: the trigger fires only while its workspace is open: a workspace opened after the time
/// does not catch up, a closed one fires nothing, and two open ones both fire.
#[test]
fn the_trigger_fires_only_while_its_workspace_is_open() {
    // Wait cues: no patch to map in the workspaces opened beside the first.
    let mut a = cue(1, CueKind::Wait { duration: 30.0 });
    a.triggers.wall_clock = clocked(20, 0, 0);
    let now = hms(MONDAY, 19, 59, 58);
    let (mut s, clock) = clock_show(vec![a.clone()], now);
    let second = |n: u128| {
        let mut ws = workspace(vec![a.clone()]);
        ws.id = WorkspaceId(Uuid::from_u128(500 + n));
        ws
    };
    let b = s
        .open_doc(DocSource::Workspace(Box::new(second(1))))
        .unwrap();
    let closed = s
        .open_doc(DocSource::Workspace(Box::new(second(2))))
        .unwrap();
    s.settle_media();
    at(&mut s, &clock, now + 1, 0);
    s.close_doc(closed, true).unwrap();
    at(&mut s, &clock, now + 2, 0);
    // Opened on its very second: it does not fire.
    let late = s
        .open_doc(DocSource::Workspace(Box::new(second(3))))
        .unwrap();
    s.settle_media();
    at(&mut s, &clock, now + 2, 400);
    at(&mut s, &clock, now + 3, 0);
    let state = |s: &OfflineShow, doc| s.control().doc(doc).unwrap().engine().state(id(1));
    let first = s.control().doc_ids()[0];
    assert_eq!(state(&s, first), CueState::Running);
    assert_eq!(state(&s, b), CueState::Running);
    assert_eq!(state(&s, late), CueState::Idle);
    let started: Vec<_> = wall_clock_log(&s)
        .into_iter()
        .filter(|e| e.cue == Some(id(1)))
        .collect();
    assert_eq!(started.len(), 2, "{started:?}");
}

/// Flow: Save, close, reopen: the reopened workspace has the trigger and it fires.
#[test]
fn save_close_reopen_the_trigger_remains_set_and_fires() {
    let path = tone();
    let mut a = audio(1, &path, 0);
    a.triggers.wall_clock = clocked(21, 0, 0);
    a.triggers.wall_clock.days = Weekdays::NONE.with(Weekday::Monday, true);
    let text = openlab_core::save_show(&workspace(vec![a])).unwrap();
    let ws = openlab_core::load_show(&text).unwrap().workspace;
    let now = hms(MONDAY, 20, 59, 59);
    let (mut s, clock) = zone_show(ws, now, LocalZone::fixed(ZONE), ZONE);
    let trigger = &s
        .control()
        .workspace()
        .find_cue(id(1))
        .unwrap()
        .triggers
        .wall_clock;
    assert!(trigger.enabled && trigger.days.contains(Weekday::Monday));
    at(&mut s, &clock, now + 1, 0);
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
}

/// Flow: the time passes while the cue already runs (no second voice), while it is paused (it
/// resumes, as any trigger resumes a paused cue), in Show Mode (it fires); a disarmed or broken
/// cue is logged as such.
#[test]
fn the_trigger_time_passes_while_running_paused_in_show_mode_disarmed_or_broken() {
    let path = tone();
    let mut running = audio(1, &path, 0);
    running.triggers.wall_clock = clocked(9, 0, 0);
    let mut paused = audio(2, &path, 1);
    paused.triggers.wall_clock = clocked(9, 0, 0);
    let mut disarmed = audio(3, &path, 0);
    disarmed.armed = false;
    disarmed.triggers.wall_clock = clocked(9, 0, 0);
    let mut broken = audio(4, &wav(&dc(10, 0.0)), 0);
    if let CueKind::Audio { patch, .. } = &mut broken.kind {
        *patch = openlab_core::PatchId(Uuid::from_u128(999));
    }
    broken.triggers.wall_clock = clocked(9, 0, 0);
    let now = hms(MONDAY, 8, 59, 59);
    let (mut s, clock) = clock_show(vec![running, paused, disarmed, broken], now);
    let rid = s.request_id();
    s.send(ClientMessage::Request {
        id: rid,
        request: Request::SetShowMode { on: true },
    });
    start(&mut s, 1);
    start(&mut s, 2);
    s.run(BLOCK);
    s.transport(TransportAction::Pause { cue: id(2) });
    s.run(BLOCK);
    assert_eq!(s.control().live_voices(), 2);
    at(&mut s, &clock, now + 1, 0);
    let engine = s.control().engine();
    assert_eq!(engine.state(id(1)), CueState::Running);
    assert_eq!(
        engine.state(id(2)),
        CueState::Running,
        "a trigger resumes a paused cue"
    );
    assert_eq!(s.control().live_voices(), 2, "no second voice");
    let log = wall_clock_log(&s);
    let of = |n: u128| log.iter().find(|e| e.cue == Some(id(n))).unwrap();
    assert_eq!(of(3).level, LogLevel::Warn);
    assert!(of(3).message.contains("disarmed"), "{}", of(3).message);
    assert_eq!(of(4).level, LogLevel::Warn);
    assert!(of(4).message.contains("broken"), "{}", of(4).message);
    assert_eq!(of(1).level, LogLevel::Info);
}

/// Flow: a trigger missed while the computer slept or the engine was held up does not fire
/// later; it is logged with its cue.
#[test]
fn a_trigger_missed_while_asleep_or_held_up_does_not_fire_later() {
    let path = tone();
    let mut a = audio(1, &path, 0);
    a.triggers.wall_clock = clocked(9, 0, 0);
    let mut b = audio(2, &path, 1);
    b.triggers.wall_clock = clocked(9, 0, 10);
    let now = hms(MONDAY, 8, 59, 59);
    let (mut s, clock) = clock_show(vec![a, b], now);
    // Asleep for an hour.
    at(&mut s, &clock, now + 3600, 0);
    at(&mut s, &clock, now + 3601, 0);
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    let log = wall_clock_log(&s);
    assert!(
        log.iter()
            .any(|e| e.cue == Some(id(1)) && e.level == LogLevel::Warn),
        "{log:?}"
    );
    // The operator sees it: a notice naming each cue the hour passed over.
    let missed = notices(&mut s, "wall_clock_missed");
    assert_eq!(missed, vec![Some(id(1)), Some(id(2))]);
    // Held up for three seconds over 09:00:10 the next day: missed too.
    let next = now + DAY + 10;
    at(&mut s, &clock, next - 1, 0);
    at(&mut s, &clock, next + 2, 0);
    s.run(BLOCK * 4);
    assert!(s.channel(1).iter().all(|&v| v == 0.0));
}

/// Flow: clock changes. Daylight saving ends at 03:00 local (the offset drops an hour): 02:30
/// happens twice and the cue starts once; the operator setting the clock back a few seconds
/// over the time does not start it again.
#[test]
fn clock_changes_fire_the_trigger_once_at_its_wall_time() {
    let path = tone();
    let mut a = audio(1, &path, 0);
    a.triggers.wall_clock = clocked(2, 30, 0);
    a.second_trigger = openlab_core::SecondTrigger::HardStopAndRestart;
    // Offset +11 h until the UTC instant that is 03:00 local, +10 h after.
    let change_utc = hms(MONDAY, 3, 0, 0) - 11 * 3600;
    let zone = LocalZone::new(move |utc| {
        Some(if utc < change_utc {
            11 * 3600
        } else {
            10 * 3600
        })
    });
    let start_local = hms(MONDAY, 2, 29, 59);
    let (mut s, clock) = zone_show(workspace(vec![a]), start_local, zone, 11 * 3600);
    let mut utc = utc_ms(start_local, 11 * 3600, 0);
    // Wakes every half second for two hours of UTC time.
    for _ in 0..(2 * 3600 * 2) {
        utc += 500;
        clock.set(utc);
        s.run(BLOCK);
    }
    let started: Vec<_> = wall_clock_log(&s)
        .into_iter()
        .filter(|e| e.cue == Some(id(1)))
        .collect();
    assert_eq!(started.len(), 1, "{started:?}");
    // The clock set back a few seconds over the time fires nothing again.
    let (mut s, clock) = clock_show(
        {
            let mut a = audio(1, &path, 0);
            a.triggers.wall_clock = clocked(2, 30, 0);
            vec![a]
        },
        start_local,
    );
    for local in [
        start_local + 1,
        start_local + 2,
        start_local - 2,
        start_local - 1,
        start_local + 1,
    ] {
        at(&mut s, &clock, local, 0);
    }
    let started: Vec<_> = wall_clock_log(&s)
        .into_iter()
        .filter(|e| e.cue == Some(id(1)))
        .collect();
    assert_eq!(started.len(), 1, "{started:?}");
    let back = wall_clock_log(&s)
        .into_iter()
        .find(|e| e.cue.is_none())
        .expect("the step back is logged");
    assert!(back.message.contains("went back"), "{}", back.message);
}

/// A cue added with the trigger off, then edited by command: a time out of range is refused
/// and nothing changes.
#[test]
fn a_wall_clock_time_out_of_range_is_refused_over_the_protocol() {
    let path = tone();
    let (mut s, _clock) = clock_show(vec![audio(1, &path, 0)], MONDAY);
    let replies = set_triggers(&mut s, 1, |t| t.wall_clock = clocked(24, 0, 0));
    assert!(!outcome_ok(&replies));
    let trigger = &s
        .control()
        .workspace()
        .find_cue(id(1))
        .unwrap()
        .triggers
        .wall_clock;
    assert_eq!(*trigger, WallClockTrigger::default());
}

/// The notices of `code` sent since the last call, by cue.
fn notices(s: &mut OfflineShow, code: &str) -> Vec<Option<openlab_core::CueId>> {
    warnings(&s.take_outbound())
        .into_iter()
        .filter_map(|w| match w {
            WireWarning::Notice { code: c, cue, .. } if c == code => Some(cue),
            _ => None,
        })
        .collect()
}

/// Daylight saving starts (Sydney: 01:59:59 to 03:00:00 in one real second): the cue at
/// 03:00:00 starts on that second, the one in the skipped hour is reported missed and does
/// not start later.
#[test]
fn clock_changes_daylight_saving_start_fires_03_00_00_and_misses_the_skipped_hour() {
    let path = tone();
    let mut skipped = audio(1, &path, 0);
    skipped.triggers.wall_clock = clocked(2, 30, 0);
    let mut first = audio(2, &path, 1);
    first.triggers.wall_clock = clocked(3, 0, 0);
    // +10 h until the UTC instant that is 02:00 local, +11 h after.
    let change_utc = hms(MONDAY, 2, 0, 0) - 10 * 3600;
    let zone = LocalZone::new(move |utc| {
        Some(if utc < change_utc {
            10 * 3600
        } else {
            11 * 3600
        })
    });
    let start_local = hms(MONDAY, 1, 59, 58);
    let (mut s, clock) = zone_show(
        workspace(vec![skipped, first]),
        start_local,
        zone,
        10 * 3600,
    );
    let mut utc = utc_ms(start_local, 10 * 3600, 0);
    for _ in 0..4 {
        utc += 500;
        clock.set(utc);
        s.run(BLOCK);
    }
    assert_eq!(s.control().engine().state(id(2)), CueState::Running);
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert_eq!(notices(&mut s, "wall_clock_missed"), vec![Some(id(1))]);
    for _ in 0..7200 {
        utc += 500;
        clock.set(utc);
        s.run(BLOCK);
    }
    s.run(BLOCK * 4);
    assert!(
        s.channel(0).iter().all(|&v| v == 0.0),
        "the skipped cue never played"
    );
}

/// The zone cannot be read (its thread did not start, or the first lookup has not answered):
/// nothing fires, and a workspace with a trigger on raises one notice once it has lasted two
/// seconds. The first time known after is a fresh start: a time crossed as it becomes known
/// does not fire, the next one does.
#[test]
fn an_unknown_local_time_fires_nothing_raises_one_notice_and_starts_afresh() {
    let path = tone();
    let mut a = audio(1, &path, 0);
    a.triggers.wall_clock = clocked(9, 0, 0);
    let mut b = audio(2, &path, 1);
    b.triggers.wall_clock = clocked(9, 0, 5);
    let known = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let k = std::sync::Arc::clone(&known);
    let zone = LocalZone::new(move |_| k.load(std::sync::atomic::Ordering::SeqCst).then_some(ZONE));
    let now = hms(MONDAY, 8, 59, 58);
    let (mut s, clock) = zone_show(workspace(vec![a, b]), now, zone, ZONE);
    at(&mut s, &clock, now + 1, 0);
    assert!(
        notices(&mut s, "wall_clock_unknown").is_empty(),
        "not after one second"
    );
    for t in 2..=6 {
        at(&mut s, &clock, now + t, 0);
    }
    assert_eq!(
        s.control().engine().state(id(1)),
        CueState::Idle,
        "09:00:00 passed unknown"
    );
    assert_eq!(
        notices(&mut s, "wall_clock_unknown"),
        vec![None],
        "one notice"
    );
    let log = wall_clock_log(&s);
    assert!(log.iter().any(|e| e.level == LogLevel::Error), "{log:?}");
    // Known again on 09:00:05: that second is the fresh start and does not fire; nothing
    // passed before it is caught up.
    known.store(true, std::sync::atomic::Ordering::SeqCst);
    at(&mut s, &clock, now + 7, 0);
    at(&mut s, &clock, now + 8, 0);
    assert_eq!(s.control().engine().state(id(2)), CueState::Idle);
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert!(notices(&mut s, "wall_clock_unknown").is_empty());
    // The next day both fire on time.
    let next = now + 86_400;
    for t in [
        next,
        next + 1,
        next + 2,
        next + 3,
        next + 4,
        next + 5,
        next + 6,
        next + 7,
    ] {
        at(&mut s, &clock, t, 0);
    }
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    assert_eq!(s.control().engine().state(id(2)), CueState::Running);
}

/// A zone that can never be read (`LocalZone::unknown`, what the host uses when the zone's
/// thread cannot start) and no trigger on: no notice, nothing logged.
#[test]
fn an_unknown_local_time_with_no_trigger_on_is_quiet() {
    let path = tone();
    let now = hms(MONDAY, 8, 0, 0);
    let (mut s, clock) = zone_show(
        workspace(vec![audio(1, &path, 0)]),
        now,
        LocalZone::unknown(),
        ZONE,
    );
    for t in 1..10 {
        at(&mut s, &clock, now + t, 0);
    }
    assert!(notices(&mut s, "wall_clock_unknown").is_empty());
    assert!(wall_clock_log(&s).is_empty());
}
