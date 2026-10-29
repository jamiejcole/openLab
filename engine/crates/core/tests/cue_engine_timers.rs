// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `CueEngine::timers` (M4.2e): the pre-wait, silent-action and post-wait countdowns a client
//! shows for cues without a voice (docs/cue-engine.md, "Timer progress"). Virtual sample clock
//! at 1 kHz: one second is 1000 samples.

mod common;

use common::*;
use openlab_core::*;

use ContinueMode::{AutoContinue as AC, AutoFollow as AF};

impl Rig {
    /// Ticks at `now`, then reads the timers there.
    fn timers(&mut self, now: SampleTime) -> Vec<(u128, Timer, SampleTime, Option<SampleTime>)> {
        self.at(now, Input::Tick);
        self.read(now)
    }

    /// Reads the timers at `t` without a tick (as the binary does between ticks).
    fn read(&self, t: SampleTime) -> Vec<(u128, Timer, SampleTime, Option<SampleTime>)> {
        self.engine
            .timers(t)
            .map(|p| (p.cue.0.as_u128(), p.timer, p.elapsed, p.remaining))
            .collect()
    }
}

#[test]
fn a_pre_wait_counts_up_and_down_to_its_action() {
    let mut rig = Rig::panicking(vec![timed(memo(1), 2.0, 0.0, AC)]);
    rig.start(100, 1);
    assert_eq!(rig.read(100), [(1, Timer::PreWait, 0, Some(2000))]);
    assert_eq!(rig.timers(600), [(1, Timer::PreWait, 500, Some(1500))]);
    // Due but not ticked yet: it shows as elapsed, never beyond.
    assert_eq!(rig.read(2500), [(1, Timer::PreWait, 2000, Some(0))]);
    // The memo's action is instant and its post-wait 0: nothing is left counting.
    assert_eq!(rig.timers(2100), []);
}

#[test]
fn a_wait_cue_shows_its_action_and_a_group_its_elapsed_time() {
    let mut rig = Rig::panicking(vec![
        wait(1, 3.0),
        cue(2, group(vec![timed(memo(3), 10.0, 0.0, AC)])),
    ]);
    rig.start(0, 1);
    rig.start(0, 2);
    assert_eq!(
        rig.timers(1000),
        [
            (1, Timer::Action, 1000, Some(2000)),
            // The group's end is not known while a child is busy.
            (2, Timer::Action, 1000, None),
            (3, Timer::PreWait, 1000, Some(9000)),
        ]
    );
    // Due but not ticked yet: the Wait cue stands at its end, never beyond.
    assert_eq!(rig.read(3500)[0], (1, Timer::Action, 3000, Some(0)));
    assert_eq!(rig.timers(3000)[0], (2, Timer::Action, 3000, None));
}

#[test]
fn a_post_wait_counts_from_the_action_start_alongside_the_action() {
    let mut rig = Rig::panicking(vec![timed(wait(1, 2.0), 0.0, 5.0, AC), memo(2)]);
    rig.start(0, 1);
    assert_eq!(
        rig.timers(1000),
        [
            (1, Timer::Action, 1000, Some(1000)),
            (1, Timer::PostWait, 1000, Some(4000)),
        ]
    );
    // The action is over; the post-wait still counts (post-waiting).
    assert_eq!(rig.timers(3000), [(1, Timer::PostWait, 3000, Some(2000))]);
    assert_eq!(rig.engine.state(id(1)), CueState::PostWaiting);
    assert_eq!(rig.timers(5000), []);
}

#[test]
fn an_audio_cue_shows_only_its_waits_its_action_comes_from_the_voice() {
    let mut rig = Rig::panicking(vec![timed(cue(1, audio()), 1.0, 4.0, AC), memo(2)]);
    rig.start(0, 1);
    assert_eq!(rig.timers(500), [(1, Timer::PreWait, 500, Some(500))]);
    assert_eq!(rig.timers(2000), [(1, Timer::PostWait, 1000, Some(3000))]);
}

#[test]
fn paused_timers_freeze_and_resume_where_they_stood() {
    let mut rig = Rig::panicking(vec![
        timed(memo(1), 2.0, 0.0, AC),
        timed(wait(2, 3.0), 0.0, 4.0, AC),
        memo(3),
    ]);
    rig.start(0, 1);
    rig.start(0, 2);
    rig.at(1000, Input::PauseAll);
    let frozen = vec![
        (1, Timer::PreWait, 1000, Some(1000)),
        (2, Timer::Action, 1000, Some(2000)),
        (2, Timer::PostWait, 1000, Some(3000)),
    ];
    assert_eq!(rig.read(1000), frozen);
    assert_eq!(rig.timers(9000), frozen);
    rig.at(10_000, Input::ResumeAll);
    assert_eq!(
        rig.timers(10_500),
        [
            (1, Timer::PreWait, 1500, Some(500)),
            (2, Timer::Action, 1500, Some(1500)),
            (2, Timer::PostWait, 1500, Some(2500)),
        ]
    );
}

#[test]
fn a_pause_committed_ahead_freezes_once_it_is_reached() {
    // Lookahead 10: a pause at `now` is committed on that sample, which is ahead of what has
    // been rendered.
    let mut rig = Rig::panicking_with(vec![wait(1, 3.0)], 10);
    rig.start(0, 1);
    rig.at(1000, Input::Pause { cue: id(1) });
    assert_eq!(rig.read(995), [(1, Timer::Action, 995, Some(2005))]);
    assert_eq!(rig.read(1000), [(1, Timer::Action, 1000, Some(2000))]);
    assert_eq!(rig.read(4000), [(1, Timer::Action, 1000, Some(2000))]);
}

#[test]
fn a_start_scheduled_ahead_shows_the_whole_timer_not_more() {
    let mut rig = Rig::panicking_with(vec![timed(wait(1, 3.0), 2.0, 0.0, AF)], 10);
    rig.start(100, 1);
    // Rendered sample 90: the start on 100 has not been reached.
    assert_eq!(rig.read(90), [(1, Timer::PreWait, 0, Some(2000))]);
    rig.at(2100, Input::Tick);
    assert_eq!(rig.read(2090), [(1, Timer::Action, 0, Some(3000))]);
}

#[test]
fn panic_stop_and_reset_clear_every_timer() {
    let cues = || {
        vec![
            timed(memo(1), 5.0, 0.0, AC),
            timed(wait(2, 5.0), 0.0, 8.0, AC),
            timed(memo(3), 0.0, 6.0, AC),
            memo(4),
        ]
    };
    let started = |rig: &mut Rig| {
        for n in 1..=3 {
            rig.start(0, n);
        }
        assert_eq!(rig.timers(1000).len(), 4);
    };

    let mut rig = Rig::panicking(cues());
    started(&mut rig);
    rig.at(1000, Input::Panic);
    assert_eq!(rig.read(1000), []);
    assert_eq!(rig.timers(3000), []);

    let mut rig = Rig::panicking(cues());
    started(&mut rig);
    for n in 1..=3 {
        let input = Input::Stop {
            cue: id(n),
            fade_out: None,
        };
        rig.at(1000, input);
    }
    assert_eq!(rig.read(1000), []);

    let mut rig = Rig::panicking(cues());
    started(&mut rig);
    for n in 1..=3 {
        rig.at(1000, Input::Reset { cue: id(n) });
    }
    assert_eq!(rig.read(1000), []);
}

#[test]
fn nothing_counts_when_idle_or_loaded() {
    let mut rig = Rig::panicking(vec![timed(wait(1, 3.0), 2.0, 1.0, AC), memo(2)]);
    assert_eq!(rig.read(0), []);
    rig.at(
        0,
        Input::Load {
            cue: id(1),
            time: 0.0,
        },
    );
    assert_eq!(rig.engine.state(id(1)), CueState::Loaded);
    assert_eq!(rig.timers(500), []);
}

#[test]
fn a_load_to_time_counts_from_where_it_was_loaded() {
    let mut rig = Rig::panicking(vec![
        timed(wait(1, 5.0), 2.0, 6.0, AC),
        timed(wait(2, 5.0), 2.0, 0.0, AF),
        memo(3),
    ]);
    // Half a second into the pre-wait.
    rig.at(
        0,
        Input::Load {
            cue: id(1),
            time: 0.5,
        },
    );
    // A second into the action, past the pre-wait: the post-wait counts from the action's top.
    rig.at(
        0,
        Input::Load {
            cue: id(2),
            time: 3.0,
        },
    );
    rig.start(100, 1);
    rig.start(100, 2);
    assert_eq!(
        rig.read(100),
        [
            (1, Timer::PreWait, 500, Some(1500)),
            (2, Timer::Action, 1000, Some(4000)),
        ]
    );
    // Cue 1 reaches its action at 1600; its post-wait has its whole 6 s from there.
    assert_eq!(
        rig.timers(2600),
        [
            (1, Timer::Action, 1000, Some(4000)),
            (1, Timer::PostWait, 1000, Some(5000)),
            (2, Timer::Action, 3500, Some(1500)),
        ]
    );
}

#[test]
fn a_cue_with_static_problems_keeps_its_waits_but_shows_no_action() {
    // A Fade cue whose target is missing: skipped like a disarmed cue, timing kept.
    let fade = cue(
        1,
        CueKind::Fade {
            target: Some(id(99)),
            duration: 3.0,
            curve: FadeCurve::Linear,
            levels: FadeLevels {
                master_db: Some(-10.0),
                output_db: vec![],
                crosspoints: vec![],
            },
            stop_target_when_done: false,
        },
    );
    let mut rig = Rig::panicking(vec![timed(fade, 1.0, 4.0, AC), memo(2)]);
    rig.start(0, 1);
    assert_eq!(rig.engine.state(id(1)), CueState::Broken);
    assert_eq!(rig.timers(500), [(1, Timer::PreWait, 500, Some(500))]);
    assert_eq!(rig.timers(2000), [(1, Timer::PostWait, 1000, Some(3000))]);
}

#[test]
fn a_groups_post_wait_counts_from_its_action_start() {
    let mut rig = Rig::panicking(vec![
        timed(cue(1, group(vec![wait(2, 1.0)])), 0.0, 3.0, AC),
        memo(3),
    ]);
    rig.start(0, 1);
    assert_eq!(
        rig.timers(500),
        [
            (1, Timer::Action, 500, None),
            (1, Timer::PostWait, 500, Some(2500)),
            (2, Timer::Action, 500, Some(500)),
        ]
    );
    assert_eq!(rig.timers(2000), [(1, Timer::PostWait, 2000, Some(1000))]);
}

#[test]
fn a_halted_playlists_follow_wait_counts_its_whole_rest_down() {
    // #192: an auto-follow playlist of two sounds (4 s and 3 s) halts when the first breaks
    // while playing. Its action ends at the broken child's nominal end (4 s); the follow then
    // comes 3 s later, as if the second child had played. The countdown shows that rest as
    // the whole post-wait: elapsed + remaining = 3000, not a total of 0.
    let sound = |n| cue(n, audio());
    let p = timed(
        cue(
            1,
            CueKind::Group {
                mode: GroupMode::Playlist {
                    loop_: false,
                    shuffle: false,
                },
                children: vec![sound(10), sound(11)],
            },
        ),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::panicking(vec![p, memo(2)]);
    let lengths = [(10, 4000), (11, 3000)]
        .into_iter()
        .map(|(n, f)| (id(n), Some(f)))
        .collect();
    rig.at(0, Input::CueLengths { lengths });
    rig.start(0, 1);
    let run = rig.engine.run_of(id(10)).unwrap();
    rig.at(
        1000,
        Input::ActionFailed {
            cue: id(10),
            run,
            reason: BrokenReason::StreamFailed,
        },
    );
    // Still in the broken child's nominal time: no follow wait yet.
    assert!(rig.timers(3000).iter().all(|t| t.1 != Timer::PostWait));
    assert_eq!(rig.engine.state(id(1)), CueState::Running);
    // The action is over; the group waits for its follow.
    assert_eq!(rig.timers(4000), [(1, Timer::PostWait, 0, Some(3000))]);
    assert_eq!(rig.engine.state(id(1)), CueState::PostWaiting);
    assert_eq!(rig.timers(5000), [(1, Timer::PostWait, 1000, Some(2000))]);
    assert_eq!(rig.read(6500), [(1, Timer::PostWait, 2500, Some(500))]);
    assert_eq!(rig.timers(7000), []);
}
