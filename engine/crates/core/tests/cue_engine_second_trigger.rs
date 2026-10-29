// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Per-cue second-trigger options (M2.14, QLab 5), per docs/cue-engine.md "Second trigger":
//! every option on a running, paused, pre-waiting and stopping cue; playlist next/previous;
//! the trigger sources (Start, GO, Start cue, GoTo-and-GO, continue, Devamp "start next cue");
//! the release input; cycles staying within the chain budget; editing with undo and validation.
//! Virtual sample clock at 1 kHz: one second is 1000 samples.

mod common;

use common::*;
use openlab_core::*;

use SecondTrigger as ST;

fn with(mut c: Cue, option: SecondTrigger) -> Cue {
    c.second_trigger = option;
    c
}

fn on_release(mut c: Cue) -> Cue {
    c.second_trigger_on_release = true;
    c
}

fn pre(mut c: Cue, seconds: f64) -> Cue {
    c.pre_wait = seconds;
    c
}

fn continuing(mut c: Cue, mode: ContinueMode, post: f64) -> Cue {
    c.continue_mode = mode;
    c.post_wait = post;
    c
}

fn grp(n: u128, mode: GroupMode, children: Vec<Cue>) -> Cue {
    cue(n, CueKind::Group { mode, children })
}

fn playlist(loop_: bool) -> GroupMode {
    GroupMode::Playlist {
        loop_,
        shuffle: false,
    }
}

fn start_cue(n: u128, target: u128) -> Cue {
    cue(
        n,
        CueKind::Start {
            target: Some(id(target)),
        },
    )
}

impl Rig {
    fn release(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::TriggerReleased { cue: id(n) })
    }
}

fn starts(actions: &[Action]) -> Vec<CueId> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::StateChanged {
                cue,
                state: CueState::PreWaiting,
                ..
            } => Some(*cue),
            _ => None,
        })
        .collect()
}

fn voice_starts(actions: &[Action]) -> Vec<(CueId, RunId, SampleTime)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::StartVoice { cue, run, at, .. } => Some((*cue, *run, *at)),
            _ => None,
        })
        .collect()
}

fn voice_stops(actions: &[Action]) -> Vec<(CueId, RunId, SampleTime, Option<StopFade>)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::StopVoice { cue, run, at, fade } => Some((*cue, *run, *at, *fade)),
            _ => None,
        })
        .collect()
}

fn has_chain_limit(actions: &[Action]) -> bool {
    warnings(actions)
        .iter()
        .any(|w| matches!(w, EngineWarning::ChainLimit { .. }))
}

fn panic_fade(frames: SampleTime) -> Option<StopFade> {
    Some(StopFade {
        frames,
        curve: PANIC_CURVE,
    })
}

// ----- a running cue -----

#[test]
fn nothing_ignores_a_trigger_on_a_running_cue() {
    let mut rig = Rig::panicking(vec![sound(1)]);
    rig.start(0, 1);
    let run = rig.run(1);
    let out = rig.start(100, 1);
    assert_eq!(
        out,
        vec![Action::Warning(EngineWarning::TriggerIgnored {
            cue: id(1),
            state: CueState::Running
        })]
    );
    assert_eq!(rig.run(1), run);
}

#[test]
fn panic_fades_a_running_cue_out_over_the_panic_duration() {
    let mut rig = Rig::panicking(vec![with(sound(1), ST::Panic), sound(2)]);
    rig.start(0, 1);
    rig.start(0, 2);
    let run = rig.run(1);
    let out = rig.start(100, 1);
    assert_eq!(voice_stops(&out), vec![(id(1), run, 100, panic_fade(1000))]);
    assert_eq!(changes(&out, 1), vec![(CueState::Stopping, 100)]);
    // Only this cue: the workspace is not in a panic and other cues play on.
    assert_eq!(rig.engine.panic_end(), None);
    assert_eq!(rig.state(2), CueState::Running);
    assert_eq!(changes(&rig.tick(1100), 1), vec![(CueState::Idle, 1100)]);
    assert_eq!(rig.state(2), CueState::Running);
}

#[test]
fn a_cue_fading_out_in_its_own_panic_restarts_on_the_next_trigger() {
    let mut rig = Rig::panicking(vec![with(sound(1), ST::Panic)]);
    rig.start(0, 1);
    let first = rig.run(1);
    rig.start(100, 1);
    // Fading out in a panic: a trigger hard-stops it and starts it afresh (as after a Panic).
    let out = rig.start(500, 1);
    assert_eq!(voice_stops(&out), vec![(id(1), first, 500, None)]);
    let fresh = rig.run(1);
    assert_ne!(fresh, first);
    assert_eq!(voice_starts(&out), vec![(id(1), fresh, 500)]);
    assert_eq!(rig.state(1), CueState::Running);
}

#[test]
fn a_zero_panic_duration_stops_the_cue_at_once() {
    let mut rig = Rig::panicking(vec![with(sound(1), ST::Panic)]);
    rig.ws.settings.panic_duration = 0.0;
    rig.start(0, 1);
    let run = rig.run(1);
    let out = rig.start(100, 1);
    assert_eq!(voice_stops(&out), vec![(id(1), run, 100, None)]);
    assert_eq!(rig.state(1), CueState::Idle);
}

#[test]
fn stop_and_hard_stop_stop_a_running_cue_at_once() {
    for option in [ST::Stop, ST::HardStop] {
        let mut rig = Rig::panicking(vec![continuing(
            with(sound(1), option),
            ContinueMode::AutoContinue,
            5.0,
        )]);
        rig.start(0, 1);
        let run = rig.run(1);
        let out = rig.start(100, 1);
        assert_eq!(
            voice_stops(&out),
            vec![(id(1), run, 100, None)],
            "{option:?}"
        );
        assert_eq!(changes(&out, 1), vec![(CueState::Idle, 100)]);
        // Its pending auto-continue went with it.
        assert_eq!(rig.engine.next_deadline(), None);
    }
}

#[test]
fn hard_stop_and_restart_starts_a_running_cue_again_from_the_top() {
    let mut rig = Rig::panicking(vec![pre(with(sound(1), ST::HardStopAndRestart), 0.5)]);
    rig.start(0, 1);
    rig.tick(500);
    let run = rig.run(1);
    let out = rig.start(700, 1);
    assert_eq!(voice_stops(&out), vec![(id(1), run, 700, None)]);
    assert_eq!(
        changes(&out, 1),
        vec![(CueState::Idle, 700), (CueState::PreWaiting, 700)]
    );
    // Its pre-wait runs again in full.
    let fresh = rig.run(1);
    assert_ne!(fresh, run);
    assert_eq!(voice_starts(&rig.tick(1200)), vec![(id(1), fresh, 1200)]);
}

#[test]
fn devamp_sends_a_devamp_to_a_running_cue_without_starting_anything() {
    let mut rig = Rig::panicking(vec![with(sound(1), ST::Devamp), memo(2)]);
    rig.start(0, 1);
    let run = rig.run(1);
    let out = rig.start(100, 1);
    assert_eq!(
        out,
        vec![Action::DevampVoice {
            cue: id(1),
            run,
            at: 100,
            stop: false
        }]
    );
    rig.at(
        100,
        Input::DevampScheduled {
            cue: id(1),
            run,
            at: 400,
        },
    );
    // A second one while it is pending is ignored, as for Devamp cues.
    assert_eq!(
        rig.start(150, 1),
        vec![Action::Warning(EngineWarning::DevampIgnored { cue: id(1) })]
    );
    // No "start next cue": the cue after it does not start when the loop is left.
    assert!(starts(&rig.tick(400)).is_empty());
    assert_eq!(rig.state(1), CueState::Running);
}

#[test]
fn devamp_on_a_cue_that_cannot_devamp_does_nothing_but_say_so() {
    // A non-looping voice: the audio side answers that there is no loop to leave.
    let mut rig = Rig::panicking(vec![with(sound(1), ST::Devamp), with(memo(2), ST::Devamp)]);
    rig.start(0, 1);
    let run = rig.run(1);
    rig.start(100, 1);
    let out = rig.at(100, Input::DevampIgnored { cue: id(1), run });
    assert_eq!(
        warnings(&out),
        vec![EngineWarning::DevampIgnored { cue: id(1) }]
    );
    assert_eq!(rig.state(1), CueState::Running);
    // A Wait cue (any kind but audio and playlists) has nothing to devamp.
    let mut rig = Rig::panicking(vec![with(
        cue(3, CueKind::Wait { duration: 5.0 }),
        ST::Devamp,
    )]);
    rig.start(0, 3);
    assert_eq!(
        rig.start(100, 3),
        vec![Action::Warning(EngineWarning::DevampIgnored { cue: id(3) })]
    );
    assert_eq!(rig.state(3), CueState::Running);
}

// ----- a paused cue -----

#[test]
fn nothing_resumes_a_paused_cue() {
    let mut rig = Rig::panicking(vec![sound(1)]);
    rig.start(0, 1);
    rig.pause(50, 1);
    let out = rig.start(100, 1);
    assert!(
        out.iter()
            .any(|a| matches!(a, Action::ResumeVoice { at: 100, .. }))
    );
    assert_eq!(rig.state(1), CueState::Running);
}

/// QLab check S1 (M2.28): a trigger on a paused cue resumes it, whatever its option.
#[test]
fn a_trigger_on_a_paused_cue_resumes_it_whatever_its_option() {
    for option in [
        ST::Nothing,
        ST::Stop,
        ST::HardStop,
        ST::Panic,
        ST::HardStopAndRestart,
        ST::Devamp,
    ] {
        let mut rig = Rig::panicking(vec![with(sound(1), option)]);
        rig.start(0, 1);
        rig.pause(50, 1);
        let run = rig.run(1);
        let out = rig.start(100, 1);
        assert!(voice_stops(&out).is_empty(), "{option:?}: {out:?}");
        assert!(voice_starts(&out).is_empty(), "{option:?}: {out:?}");
        assert!(
            out.iter().any(|a| matches!(
                a,
                Action::ResumeVoice { paused, at: 100, .. } if *paused == run
            )),
            "{option:?}: {out:?}"
        );
        assert!(
            !out.iter().any(|a| matches!(a, Action::DevampVoice { .. })),
            "{option:?}: {out:?}"
        );
        assert!(warnings(&out).is_empty(), "{option:?}: {out:?}");
        assert_eq!(rig.state(1), CueState::Running, "{option:?}");
    }
}

/// The release of a cue that asks for a second trigger on release still applies its option to
/// a paused cue (only a trigger resumes it, QLab check S5).
#[test]
fn stop_panic_and_hard_stop_on_release_stop_a_paused_cue_without_resuming_it() {
    for option in [ST::Stop, ST::HardStop, ST::Panic] {
        let mut rig = Rig::panicking(vec![on_release(with(sound(1), option))]);
        rig.start(0, 1);
        rig.pause(50, 1);
        let run = rig.run(1);
        let out = rig.release(100, 1);
        // A paused voice is silent: no fade, and it is not resumed first.
        assert_eq!(
            voice_stops(&out),
            vec![(id(1), run, 100, None)],
            "{option:?}"
        );
        assert!(
            !out.iter().any(|a| matches!(a, Action::ResumeVoice { .. })),
            "{option:?}"
        );
        assert_eq!(rig.state(1), CueState::Idle, "{option:?}");
    }
}

#[test]
fn hard_stop_and_restart_on_release_restarts_a_paused_cue() {
    let mut rig = Rig::panicking(vec![on_release(with(sound(1), ST::HardStopAndRestart))]);
    rig.start(0, 1);
    rig.pause(50, 1);
    let run = rig.run(1);
    let out = rig.release(100, 1);
    assert_eq!(voice_stops(&out), vec![(id(1), run, 100, None)]);
    let fresh = rig.run(1);
    assert_eq!(voice_starts(&out), vec![(id(1), fresh, 100)]);
    assert!(!out.iter().any(|a| matches!(a, Action::ResumeVoice { .. })));
    assert_eq!(rig.state(1), CueState::Running);
}

#[test]
fn devamp_on_release_on_a_paused_cue_is_held_for_the_resume() {
    let mut rig = Rig::panicking(vec![on_release(with(sound(1), ST::Devamp))]);
    rig.start(0, 1);
    rig.pause(50, 1);
    // Nothing is sent to the paused voice, and the release does not resume it.
    assert!(rig.release(100, 1).is_empty());
    assert_eq!(rig.state(1), CueState::Paused);
    let out = rig.at(200, Input::Resume { cue: id(1) });
    let fresh = rig.run(1);
    let voice: Vec<&Action> = out
        .iter()
        .filter(|a| matches!(a, Action::ResumeVoice { .. } | Action::DevampVoice { .. }))
        .collect();
    assert!(matches!(voice[0], Action::ResumeVoice { at: 200, .. }));
    assert_eq!(
        voice[1],
        &Action::DevampVoice {
            cue: id(1),
            run: fresh,
            at: 200,
            stop: false
        }
    );
}

// ----- a pre-waiting cue -----

#[test]
fn options_on_a_pre_waiting_cue() {
    // Nothing: ignored, the pre-wait keeps counting.
    let mut rig = Rig::panicking(vec![pre(sound(1), 1.0)]);
    rig.start(0, 1);
    assert_eq!(
        warnings(&rig.start(100, 1)),
        vec![EngineWarning::TriggerIgnored {
            cue: id(1),
            state: CueState::PreWaiting
        }]
    );
    assert_eq!(voice_starts(&rig.tick(1000)).len(), 1);

    // Stop, hard stop, panic: stopped at once; the action never starts.
    for option in [ST::Stop, ST::HardStop, ST::Panic] {
        let mut rig = Rig::panicking(vec![pre(with(sound(1), option), 1.0)]);
        rig.start(0, 1);
        let out = rig.start(100, 1);
        assert_eq!(changes(&out, 1), vec![(CueState::Idle, 100)], "{option:?}");
        assert!(voice_stops(&out).is_empty());
        assert!(rig.tick(1000).is_empty(), "{option:?}");
    }

    // Restart: the pre-wait starts again in full.
    let mut rig = Rig::panicking(vec![pre(with(sound(1), ST::HardStopAndRestart), 1.0)]);
    rig.start(0, 1);
    let out = rig.start(100, 1);
    assert_eq!(
        changes(&out, 1),
        vec![(CueState::Idle, 100), (CueState::PreWaiting, 100)]
    );
    assert!(rig.tick(1000).is_empty());
    assert_eq!(voice_starts(&rig.tick(1100)).len(), 1);

    // Devamp: nothing to devamp before the action.
    let mut rig = Rig::panicking(vec![pre(with(sound(1), ST::Devamp), 1.0)]);
    rig.start(0, 1);
    assert_eq!(
        rig.start(100, 1),
        vec![Action::Warning(EngineWarning::DevampIgnored { cue: id(1) })]
    );
    assert_eq!(rig.state(1), CueState::PreWaiting);
}

// ----- a stopping cue (stop with a fade) -----

fn stopping_rig(option: SecondTrigger) -> (Rig, RunId) {
    let mut rig = Rig::panicking(vec![with(sound(1), option)]);
    rig.start(0, 1);
    let run = rig.run(1);
    rig.at(
        50,
        Input::Stop {
            cue: id(1),
            fade_out: Some(FadeSpec {
                duration: 2.0,
                curve: FadeCurve::Linear,
            }),
        },
    );
    assert_eq!(rig.state(1), CueState::Stopping);
    (rig, run)
}

#[test]
fn options_on_a_stopping_cue() {
    let (mut rig, _) = stopping_rig(ST::Nothing);
    assert_eq!(
        rig.start(100, 1),
        vec![Action::Warning(EngineWarning::TriggerIgnored {
            cue: id(1),
            state: CueState::Stopping
        })]
    );

    for option in [ST::Stop, ST::HardStop] {
        let (mut rig, run) = stopping_rig(option);
        let out = rig.start(100, 1);
        assert_eq!(
            voice_stops(&out),
            vec![(id(1), run, 100, None)],
            "{option:?}"
        );
        assert_eq!(rig.state(1), CueState::Idle);
    }

    // Panic: its stop would end at 2050, after the panic's 1100, so the panic fades it again.
    let (mut rig, run) = stopping_rig(ST::Panic);
    let out = rig.start(100, 1);
    assert_eq!(voice_stops(&out), vec![(id(1), run, 100, panic_fade(1000))]);
    assert_eq!(changes(&rig.tick(1100), 1), vec![(CueState::Idle, 1100)]);

    let (mut rig, run) = stopping_rig(ST::HardStopAndRestart);
    let out = rig.start(100, 1);
    assert_eq!(voice_stops(&out), vec![(id(1), run, 100, None)]);
    assert_eq!(voice_starts(&out).len(), 1);
    assert_eq!(rig.state(1), CueState::Running);

    let (mut rig, _) = stopping_rig(ST::Devamp);
    assert_eq!(
        rig.start(100, 1),
        vec![Action::Warning(EngineWarning::DevampIgnored { cue: id(1) })]
    );
    assert_eq!(rig.state(1), CueState::Stopping);
}

#[test]
fn a_panic_keeps_a_stop_that_ends_sooner() {
    let mut rig = Rig::panicking(vec![with(sound(1), ST::Panic)]);
    rig.start(0, 1);
    rig.at(
        50,
        Input::Stop {
            cue: id(1),
            fade_out: Some(FadeSpec {
                duration: 0.5,
                curve: FadeCurve::Linear,
            }),
        },
    );
    assert!(voice_stops(&rig.start(100, 1)).is_empty());
    assert_eq!(changes(&rig.tick(550), 1), vec![(CueState::Idle, 550)]);
}

#[test]
fn stop_on_a_post_waiting_cue_cancels_its_continue() {
    let mut rig = Rig::panicking(vec![
        continuing(with(memo(1), ST::Stop), ContinueMode::AutoContinue, 2.0),
        memo(2),
    ]);
    rig.start(0, 1);
    assert_eq!(rig.state(1), CueState::PostWaiting);
    assert_eq!(changes(&rig.start(100, 1), 1), vec![(CueState::Idle, 100)]);
    assert!(starts(&rig.tick(2000)).is_empty());
}

// ----- groups -----

#[test]
fn a_groups_option_applies_to_the_group_as_a_whole() {
    let mut rig = Rig::panicking(vec![with(
        grp(1, GroupMode::Timeline, vec![sound(2), sound(3)]),
        ST::HardStopAndRestart,
    )]);
    rig.start(0, 1);
    let (two, three) = (rig.run(2), rig.run(3));
    let out = rig.start(100, 1);
    let mut stopped: Vec<RunId> = voice_stops(&out).iter().map(|s| s.1).collect();
    stopped.sort();
    assert_eq!(stopped, vec![two, three]);
    let started: Vec<CueId> = voice_starts(&out).iter().map(|s| s.0).collect();
    assert_eq!(started, vec![id(2), id(3)]);
    assert_eq!(rig.state(1), CueState::Running);
}

#[test]
fn a_childs_option_applies_when_the_child_itself_is_triggered() {
    let mut rig = Rig::panicking(vec![grp(
        1,
        GroupMode::Timeline,
        vec![with(sound(2), ST::Stop)],
    )]);
    rig.start(0, 1);
    // The group's own option (nothing) applies to the group.
    assert_eq!(
        warnings(&rig.start(100, 1)),
        vec![EngineWarning::TriggerIgnored {
            cue: id(1),
            state: CueState::Running
        }]
    );
    assert_eq!(rig.state(2), CueState::Running);
    // The child's applies to the child; its group then has no busy child and stops too.
    let out = rig.start(200, 2);
    assert_eq!(changes(&out, 2), vec![(CueState::Idle, 200)]);
    assert_eq!(rig.state(1), CueState::Idle);
}

// ----- playlists: play next / play previous -----

fn playlist_rig(loop_: bool, option: SecondTrigger) -> Rig {
    let group = continuing(
        with(
            grp(1, playlist(loop_), vec![sound(11), sound(12), sound(13)]),
            option,
        ),
        ContinueMode::AutoFollow,
        0.0,
    );
    let mut rig = Rig::panicking(vec![group, memo(2)]);
    let out = rig.start(0, 1);
    assert_eq!(voice_starts(&out)[0].0, id(11));
    rig
}

fn playing(out: &[Action]) -> Vec<CueId> {
    voice_starts(out).iter().map(|s| s.0).collect()
}

#[test]
fn play_next_moves_a_playlist_on_and_completes_it_after_the_last_child() {
    let mut rig = playlist_rig(false, ST::PlayNext);
    let eleven = rig.run(11);
    let out = rig.start(100, 1);
    assert_eq!(voice_stops(&out), vec![(id(11), eleven, 100, None)]);
    assert_eq!(playing(&out), vec![id(12)]);
    assert_eq!(playing(&rig.start(200, 1)), vec![id(13)]);
    // Past the last child the playlist completes, as if that child had ended: auto-follow fires.
    let out = rig.start(300, 1);
    assert!(playing(&out).is_empty());
    assert_eq!(changes(&out, 1), vec![(CueState::Idle, 300)]);
    assert_eq!(starts(&out), vec![id(2)]);
}

#[test]
fn play_next_wraps_a_looping_playlist() {
    let mut rig = playlist_rig(true, ST::PlayNext);
    rig.start(100, 1);
    rig.start(200, 1);
    assert_eq!(playing(&rig.start(300, 1)), vec![id(11)]);
    assert_eq!(rig.state(1), CueState::Running);
}

#[test]
fn play_previous_goes_back_wrapping_when_the_playlist_loops() {
    let mut rig = playlist_rig(true, ST::PlayPrevious);
    let out = rig.start(100, 1);
    assert_eq!(playing(&out), vec![id(13)]);
    assert_eq!(playing(&rig.start(200, 1)), vec![id(12)]);
    assert_eq!(playing(&rig.start(300, 1)), vec![id(11)]);

    // Without loop, "previous" on the first child stops the playlist (QLab check S4, M2.28):
    // nothing starts and its auto-follow does not fire.
    let mut rig = playlist_rig(false, ST::PlayPrevious);
    let first = rig.run(11);
    let out = rig.start(100, 1);
    assert_eq!(voice_stops(&out), vec![(id(11), first, 100, None)]);
    assert!(playing(&out).is_empty(), "{out:?}");
    assert_eq!(rig.state(1), CueState::Idle);
    assert_eq!(rig.state(11), CueState::Idle);
    assert_eq!(rig.state(2), CueState::Idle);
    assert_eq!(rig.engine.next_deadline(), None);
}

#[test]
fn play_previous_after_moving_on_returns_to_the_child_before() {
    let mut rig = playlist_rig(false, ST::PlayPrevious);
    let run = rig.run(11);
    // 11 ends by itself: 12 plays.
    let out = rig.at(
        500,
        Input::ActionEnded {
            cue: id(11),
            run,
            at: 500,
        },
    );
    assert_eq!(playing(&out), vec![id(12)]);
    assert_eq!(playing(&rig.start(600, 1)), vec![id(11)]);
    // And the playlist goes on from there when that child ends.
    let run = rig.run(11);
    let out = rig.at(
        900,
        Input::ActionEnded {
            cue: id(11),
            run,
            at: 900,
        },
    );
    assert_eq!(playing(&out), vec![id(12)]);
}

#[test]
fn play_next_on_release_on_a_paused_playlist_resumes_it_on_the_next_child() {
    let mut rig = playlist_rig(false, ST::PlayNext);
    rig.ws.cue_lists[0].cues[0].second_trigger_on_release = true;
    rig.pause(50, 1);
    let eleven = rig.run(11);
    let out = rig.release(100, 1);
    assert_eq!(voice_stops(&out), vec![(id(11), eleven, 100, None)]);
    assert!(!out.iter().any(|a| matches!(a, Action::ResumeVoice { .. })));
    assert_eq!(playing(&out), vec![id(12)]);
    assert_eq!(rig.state(1), CueState::Running);
}

#[test]
fn play_next_follows_a_shuffled_pass() {
    let group = with(
        grp(
            1,
            GroupMode::Playlist {
                loop_: true,
                shuffle: true,
            },
            vec![sound(11), sound(12), sound(13)],
        ),
        ST::PlayNext,
    );
    let mut rig = Rig::panicking(vec![group]);
    let mut pass = playing(&rig.start(0, 1));
    for t in 1..3 {
        pass.extend(playing(&rig.start(t * 100, 1)));
    }
    let mut sorted = pass.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        vec![id(11), id(12), id(13)],
        "one pass plays each child once"
    );
    // The next pass never opens with the child that closed this one.
    let next = playing(&rig.start(300, 1));
    assert_eq!(next.len(), 1);
    assert_ne!(next[0], pass[2]);
}

#[test]
fn play_next_on_a_playlist_not_in_its_action_is_ignored() {
    let group = pre(
        with(grp(1, playlist(false), vec![sound(11)]), ST::PlayNext),
        1.0,
    );
    let mut rig = Rig::panicking(vec![group]);
    rig.start(0, 1);
    assert_eq!(
        rig.start(100, 1),
        vec![Action::Warning(EngineWarning::TriggerIgnored {
            cue: id(1),
            state: CueState::PreWaiting
        })]
    );
}

#[test]
fn a_restarted_current_child_keeps_its_place_in_the_playlist() {
    let group = grp(
        1,
        playlist(false),
        vec![with(sound(11), ST::HardStopAndRestart), sound(12)],
    );
    let mut rig = Rig::panicking(vec![group]);
    rig.start(0, 1);
    let old = rig.run(11);
    rig.start(100, 11);
    let fresh = rig.run(11);
    assert_ne!(old, fresh);
    // The old run's end is stale; the new run's end moves the playlist on.
    assert!(
        playing(&rig.at(
            150,
            Input::ActionEnded {
                cue: id(11),
                run: old,
                at: 150
            }
        ))
        .is_empty()
    );
    let out = rig.at(
        500,
        Input::ActionEnded {
            cue: id(11),
            run: fresh,
            at: 500,
        },
    );
    assert_eq!(playing(&out), vec![id(12)]);
    assert_eq!(rig.state(1), CueState::Running);
}

#[test]
fn play_next_and_previous_on_another_cue_act_as_nothing_and_validation_warns() {
    let mut rig = Rig::panicking(vec![
        with(sound(1), ST::PlayNext),
        with(
            grp(2, GroupMode::Timeline, vec![sound(3)]),
            ST::PlayPrevious,
        ),
        with(grp(4, playlist(true), vec![sound(5)]), ST::PlayNext),
    ]);
    let issues = validate(&rig.ws);
    let flagged: Vec<Option<CueId>> = issues
        .iter()
        .filter(|i| i.kind == IssueKind::SecondTriggerNeedsPlaylist)
        .map(|i| i.cue_id)
        .collect();
    assert_eq!(flagged, vec![Some(id(1)), Some(id(2))]);
    assert!(
        issues
            .iter()
            .all(|i| i.severity == Severity::Warning && !i.to_string().is_empty())
    );
    // Kept, not repaired: the file loads with the warning and the value.
    let loaded = load_show(&show_text(&rig.ws)).unwrap();
    assert_eq!(loaded.workspace, rig.ws);
    assert!(
        loaded
            .warnings
            .iter()
            .any(|i| i.kind == IssueKind::SecondTriggerNeedsPlaylist)
    );

    rig.start(0, 1);
    rig.start(0, 2);
    assert_eq!(
        warnings(&rig.start(100, 1)),
        vec![EngineWarning::TriggerIgnored {
            cue: id(1),
            state: CueState::Running
        }]
    );
    assert_eq!(
        warnings(&rig.start(100, 2)),
        vec![EngineWarning::TriggerIgnored {
            cue: id(2),
            state: CueState::Running
        }]
    );
    // As `nothing`, a paused one resumes.
    rig.pause(200, 1);
    rig.start(300, 1);
    assert_eq!(rig.state(1), CueState::Running);
}

// ----- trigger sources -----

#[test]
fn go_applies_the_option_and_still_moves_the_playhead() {
    let mut rig = Rig::panicking(vec![with(sound(1), ST::Stop), memo(2)]);
    rig.go(0);
    rig.at(
        50,
        Input::SetPlayhead {
            list: list_id(1),
            cue: Some(id(1)),
        },
    );
    let out = rig.go(100);
    assert_eq!(changes(&out, 1), vec![(CueState::Idle, 100)]);
    assert_eq!(rig.engine.playhead(list_id(1)), Some(id(2)));
}

#[test]
fn double_go_protection_still_filters_before_the_option() {
    let mut rig = Rig::panicking(vec![with(sound(1), ST::Stop), memo(2)]);
    rig.ws.settings.min_go_interval = 1.0;
    rig.go(0);
    rig.at(
        50,
        Input::SetPlayhead {
            list: list_id(1),
            cue: Some(id(1)),
        },
    );
    let out = rig.go(100);
    assert_eq!(
        out,
        vec![Action::GoIgnored {
            list: list_id(1),
            reason: GoIgnoredReason::DoubleGo
        }]
    );
    assert_eq!(rig.state(1), CueState::Running);
    // Once the interval has passed, GO reaches the cue and its option applies.
    assert_eq!(changes(&rig.go(1000), 1), vec![(CueState::Idle, 1000)]);
}

#[test]
fn a_start_cue_is_a_second_trigger() {
    let mut rig = Rig::panicking(vec![with(sound(1), ST::Stop), start_cue(2, 1)]);
    rig.start(0, 1);
    let run = rig.run(1);
    let out = rig.start(100, 2);
    assert_eq!(voice_stops(&out), vec![(id(1), run, 100, None)]);
    assert_eq!(rig.state(1), CueState::Idle);
}

#[test]
fn go_to_and_go_is_a_second_trigger() {
    let goto = cue(
        2,
        CueKind::GoTo {
            target: Some(id(1)),
            then_go: true,
        },
    );
    let mut rig = Rig::panicking(vec![with(sound(1), ST::HardStopAndRestart), goto, memo(3)]);
    rig.start(0, 1);
    let run = rig.run(1);
    let out = rig.start(100, 2);
    assert_eq!(voice_stops(&out), vec![(id(1), run, 100, None)]);
    assert_eq!(voice_starts(&out).len(), 1);
    assert_ne!(rig.run(1), run);
    assert_eq!(rig.engine.playhead(list_id(1)), Some(id(2)));
}

#[test]
fn a_continue_is_a_second_trigger() {
    let mut rig = Rig::panicking(vec![
        continuing(memo(1), ContinueMode::AutoContinue, 0.0),
        with(sound(2), ST::Stop),
    ]);
    rig.start(0, 2);
    let out = rig.start(100, 1);
    assert_eq!(changes(&out, 2), vec![(CueState::Idle, 100)]);
}

#[test]
fn a_devamp_cues_start_next_cue_is_a_second_trigger() {
    let devamp = cue(
        2,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: true,
            stop_target_when_slice_ends: false,
            devamp_mode: DevampMode::CurrentSlice,
        },
    );
    let mut rig = Rig::panicking(vec![sound(1), devamp, with(sound(3), ST::Stop)]);
    rig.start(0, 1);
    rig.start(0, 3);
    let run = rig.run(1);
    rig.start(100, 2);
    rig.at(
        100,
        Input::DevampScheduled {
            cue: id(1),
            run,
            at: 300,
        },
    );
    let out = rig.tick(300);
    assert_eq!(changes(&out, 3), vec![(CueState::Idle, 300)]);
}

#[test]
fn a_broken_cue_is_not_affected_by_its_option() {
    let mut rig = Rig::panicking(vec![with(sound(1), ST::HardStopAndRestart)]);
    rig.start(0, 1);
    let run = rig.run(1);
    rig.at(
        50,
        Input::ActionFailed {
            cue: id(1),
            run,
            reason: BrokenReason::StreamFailed,
        },
    );
    assert_eq!(
        rig.start(100, 1),
        vec![Action::Warning(EngineWarning::CueBroken { cue: id(1) })]
    );
}

#[test]
fn a_cue_fading_out_in_a_workspace_panic_restarts_whatever_its_option() {
    for option in [ST::Nothing, ST::Stop, ST::Devamp] {
        let mut rig = Rig::panicking(vec![with(sound(1), option)]);
        rig.start(0, 1);
        rig.at(100, Input::Panic);
        let out = rig.start(200, 1);
        assert_eq!(voice_starts(&out).len(), 1, "{option:?}");
        assert_eq!(rig.state(1), CueState::Running);
    }
}

// ----- on release -----

#[test]
fn release_is_a_second_trigger_when_the_cue_asks_for_it() {
    let mut rig = Rig::panicking(vec![on_release(with(sound(1), ST::Stop))]);
    rig.start(0, 1);
    let run = rig.run(1);
    let out = rig.release(100, 1);
    assert_eq!(voice_stops(&out), vec![(id(1), run, 100, None)]);
    assert_eq!(rig.state(1), CueState::Idle);
    // Released on an idle cue: nothing (it never starts a cue).
    assert!(rig.release(200, 1).is_empty());
    assert_eq!(rig.state(1), CueState::Idle);
}

#[test]
fn release_is_ignored_silently_without_the_flag() {
    let mut rig = Rig::panicking(vec![with(sound(1), ST::Stop)]);
    rig.start(0, 1);
    assert!(rig.release(100, 1).is_empty());
    assert_eq!(rig.state(1), CueState::Running);
}

#[test]
fn release_with_nothing_never_resumes_or_warns() {
    let mut rig = Rig::panicking(vec![on_release(sound(1))]);
    rig.start(0, 1);
    assert!(rig.release(50, 1).is_empty());
    rig.pause(60, 1);
    assert!(rig.release(100, 1).is_empty());
    assert_eq!(rig.state(1), CueState::Paused);
}

#[test]
fn release_applies_to_a_paused_cue_and_restarts_like_a_trigger() {
    let mut rig = Rig::panicking(vec![
        on_release(with(sound(1), ST::Stop)),
        on_release(with(sound(2), ST::HardStopAndRestart)),
    ]);
    rig.start(0, 1);
    rig.start(0, 2);
    rig.pause(50, 1);
    rig.release(100, 1);
    assert_eq!(rig.state(1), CueState::Idle);
    let run = rig.run(2);
    let out = rig.release(100, 2);
    assert_eq!(voice_starts(&out).len(), 1);
    assert_ne!(rig.run(2), run);
}

#[test]
fn release_leaves_a_cue_fading_out_in_a_panic_alone() {
    let mut rig = Rig::panicking(vec![on_release(with(sound(1), ST::Panic))]);
    rig.start(0, 1);
    // Sampler use: press to start, release to fade out.
    let out = rig.release(100, 1);
    assert_eq!(voice_stops(&out).len(), 1);
    assert_eq!(rig.state(1), CueState::Stopping);
    // A second release during that fade does not restart it (only a trigger does).
    assert!(rig.release(200, 1).is_empty());
    assert_eq!(rig.state(1), CueState::Stopping);
}

#[test]
fn release_of_an_unknown_cue_warns() {
    let mut rig = Rig::panicking(vec![]);
    assert_eq!(
        rig.release(0, 9),
        vec![Action::Warning(EngineWarning::UnknownCue { cue: id(9) })]
    );
}

// ----- cycles stay within the chain budget -----

/// A sound that auto-continues into a Start cue targeting it (review M3): with "hard stop and
/// restart", the Start cue's trigger reaches the sound on the sample the sound was triggered
/// on, so it is ignored. A restart happens at most once per sample, so a GO plays the sound
/// once instead of restarting it until the chain limit.
#[test]
fn a_restart_reached_again_on_the_same_sample_is_ignored() {
    let mut rig = Rig::panicking(vec![
        continuing(
            with(sound(1), ST::HardStopAndRestart),
            ContinueMode::AutoContinue,
            0.0,
        ),
        start_cue(2, 1),
    ]);
    let out = rig.go(0);
    assert_eq!(starts(&out), vec![id(1), id(2)]);
    assert_eq!(voice_starts(&out).len(), 1);
    assert!(voice_stops(&out).is_empty());
    assert!(!has_chain_limit(&out));
    assert!(warnings(&out).contains(&EngineWarning::TriggerIgnored {
        cue: id(1),
        state: CueState::Running
    }));
    // A trigger on a later sample restarts it, once: the Start cue it continues into again
    // reaches the new run on its own sample.
    let run = rig.run(1);
    let out = rig.start(1000, 1);
    assert_eq!(voice_stops(&out), vec![(id(1), run, 1000, None)]);
    assert_eq!(voice_starts(&out).len(), 1);
    assert_eq!(starts(&out), vec![id(1), id(2)]);
    assert!(!has_chain_limit(&out));
    // So does a second call on the same sample.
    let run = rig.run(1);
    assert!(voice_stops(&rig.start(1000, 1)).is_empty());
    assert_eq!(rig.run(1), run);
    assert_eq!(rig.engine.next_deadline(), None);
}

/// A group that restarts on a second trigger, holding a Start cue that targets it: the Start
/// cue reaches the group on the sample it was triggered on, so it is not restarted again.
#[test]
fn a_group_restart_cycle_restarts_at_most_once_per_sample() {
    let mut rig = Rig::panicking(vec![with(
        grp(1, GroupMode::Timeline, vec![start_cue(2, 1), sound(3)]),
        ST::HardStopAndRestart,
    )]);
    let out = rig.start(0, 1);
    assert_eq!(starts(&out), vec![id(1), id(2), id(3)]);
    assert!(!has_chain_limit(&out));
    let out = rig.start(500, 1);
    assert_eq!(starts(&out), vec![id(1), id(2), id(3)]);
    assert_eq!(voice_starts(&out).len(), voice_stops(&out).len());
    assert!(starts(&rig.tick(500)).is_empty());
    assert!(starts(&rig.tick(501)).is_empty());
}

/// A restart cycle that takes time (a pre-wait) is a timed loop: one restart per lap.
#[test]
fn a_timed_restart_cycle_restarts_once_per_lap() {
    let mut rig = Rig::panicking(vec![
        continuing(
            with(sound(1), ST::HardStopAndRestart),
            ContinueMode::AutoContinue,
            0.0,
        ),
        pre(start_cue(2, 1), 0.1),
    ]);
    rig.start(0, 1);
    for lap in 1..=3u64 {
        let out = rig.tick(lap * 100);
        assert_eq!(voice_starts(&out).len(), 1, "lap {lap}");
        assert_eq!(voice_stops(&out).len(), 1, "lap {lap}");
        assert!(!has_chain_limit(&out));
    }
}

/// A looping playlist whose children are Start cues targeting it with "play next": the playlist
/// moves on once per sample (the next child's trigger on the same sample is ignored); its
/// zero-length children then loop until the chain budget's reserve, as in any such playlist, and
/// the playlist completes.
#[test]
fn a_play_next_cycle_ends_at_the_chain_limit() {
    let mut rig = Rig::panicking(vec![
        with(
            grp(1, playlist(true), vec![start_cue(2, 1), start_cue(3, 1)]),
            ST::PlayNext,
        ),
        memo(4),
    ]);
    let limit = CueEngine::chain_limit(&rig.ws);
    let out = rig.start(0, 1);
    assert!(starts(&out).len() <= limit);
    assert!(has_chain_limit(&out));
    assert_eq!(rig.state(1), CueState::Idle);
    assert_eq!(rig.engine.next_deadline(), None);
}

/// A Stop option cannot make a cycle: it starts nothing.
#[test]
fn a_stop_option_ends_a_start_cycle() {
    let mut rig = Rig::panicking(vec![
        continuing(with(sound(1), ST::Stop), ContinueMode::AutoContinue, 0.0),
        start_cue(2, 1),
    ]);
    let out = rig.start(0, 1);
    assert_eq!(starts(&out), vec![id(1), id(2)]);
    assert!(!has_chain_limit(&out));
    assert_eq!(rig.state(1), CueState::Idle);
}

// ----- editing -----

#[test]
fn the_options_are_edited_with_undo_and_redo() {
    let mut ws = workspace(vec![memo(1), memo(2), grp(3, playlist(false), vec![])]);
    let original = ws.clone();
    let mut history = History::new();
    history
        .execute(
            &mut ws,
            Command::SetCueProperty {
                id: id(1),
                property: CueProperty::SecondTrigger {
                    value: ST::HardStopAndRestart,
                },
            },
        )
        .unwrap();
    history
        .execute(
            &mut ws,
            Command::BatchSet {
                ids: vec![id(1), id(2)],
                property: CueProperty::SecondTriggerOnRelease { value: true },
            },
        )
        .unwrap();
    // Play next on a non-playlist is accepted (kept, acts as nothing, validation warns), so
    // undoing a group-mode change can always put it back.
    history
        .execute(
            &mut ws,
            Command::SetCueProperty {
                id: id(2),
                property: CueProperty::SecondTrigger {
                    value: ST::PlayNext,
                },
            },
        )
        .unwrap();
    let edited = ws.clone();
    let find = |ws: &Workspace, n| ws.find_cue(id(n)).unwrap().clone();
    assert_eq!(find(&ws, 1).second_trigger, ST::HardStopAndRestart);
    assert!(find(&ws, 1).second_trigger_on_release);
    assert!(find(&ws, 2).second_trigger_on_release);
    assert_eq!(find(&ws, 2).second_trigger, ST::PlayNext);
    assert!(
        validate(&ws)
            .iter()
            .any(|i| i.kind == IssueKind::SecondTriggerNeedsPlaylist && i.cue_id == Some(id(2)))
    );
    // It saves and loads back unchanged.
    let text = save_show(&ws).unwrap();
    assert_eq!(load_show(&text).unwrap().workspace, ws);

    for _ in 0..3 {
        assert!(history.undo(&mut ws).unwrap());
    }
    assert_eq!(ws, original);
    for _ in 0..3 {
        assert!(history.redo(&mut ws).unwrap());
    }
    assert_eq!(ws, edited);
}

#[test]
fn a_group_mode_change_keeps_a_playlist_option_and_undo_restores_both() {
    let mut ws = workspace(vec![with(grp(1, playlist(true), vec![]), ST::PlayPrevious)]);
    assert!(validate(&ws).is_empty());
    let original = ws.clone();
    let inverse = ws
        .apply(Command::SetCueProperty {
            id: id(1),
            property: CueProperty::GroupMode {
                value: GroupMode::Timeline,
            },
        })
        .unwrap();
    assert_eq!(ws.find_cue(id(1)).unwrap().second_trigger, ST::PlayPrevious);
    assert!(
        validate(&ws)
            .iter()
            .any(|i| i.kind == IssueKind::SecondTriggerNeedsPlaylist)
    );
    ws.apply(inverse).unwrap();
    assert_eq!(ws, original);
}

// ----- fix round 1 (review M1–M3) -----

/// Review M1: a paused cue whose option has nothing to act on (a playlist paused in its
/// pre-wait, a sound paused in its pre-wait with "devamp") resumes, as with `nothing`.
#[test]
fn a_paused_cue_whose_option_cannot_act_resumes() {
    let group = pre(
        with(grp(1, playlist(false), vec![sound(11)]), ST::PlayNext),
        5.0,
    );
    let mut rig = Rig::panicking(vec![group, pre(with(sound(2), ST::Devamp), 5.0)]);
    rig.start(0, 1);
    rig.start(0, 2);
    rig.at(100, Input::PauseAll);
    for n in [1, 2] {
        assert_eq!(rig.state(n), CueState::Paused);
        let out = rig.start(200, n);
        assert!(warnings(&out).is_empty(), "{out:?}");
        assert_eq!(rig.state(n), CueState::PreWaiting, "cue {n}");
    }
    // The pre-waits kept the time they had left: both actions start at 5100.
    let out = rig.tick(5100);
    let mut started: Vec<CueId> = voice_starts(&out).iter().map(|s| s.0).collect();
    started.sort();
    assert_eq!(started, vec![id(2), id(11)]);
}

/// Review M1: on release, a paused cue whose option can act gets it; a trigger resumes it
/// (QLab check S1) and sends the devamp held for the resume.
#[test]
fn a_paused_cue_whose_option_can_act_gets_it_on_release() {
    let mut rig = Rig::panicking(vec![on_release(with(sound(1), ST::Devamp))]);
    rig.start(0, 1);
    rig.pause(50, 1);
    assert!(rig.release(100, 1).is_empty());
    assert_eq!(rig.state(1), CueState::Paused);
    let out = rig.start(200, 1);
    assert_eq!(rig.state(1), CueState::Running);
    assert!(
        out.iter()
            .any(|a| matches!(a, Action::DevampVoice { at: 200, .. }))
    );
}

/// Review M2: "play previous" passes over children that would refuse to start, as moving on
/// does, instead of coming back round to the child playing.
#[test]
fn play_previous_passes_over_broken_and_disarmed_children() {
    let mut broken = sound(12);
    if let CueKind::Audio { media, .. } = &mut broken.kind {
        *media = None;
    }
    let mut disarmed = sound(13);
    disarmed.armed = false;
    let group = with(
        grp(
            1,
            playlist(false),
            vec![sound(11), broken, disarmed, sound(14)],
        ),
        ST::PlayPrevious,
    );
    let mut rig = Rig::panicking(vec![group]);
    rig.start(0, 1);
    // Get 14 playing: 11 ends, 12 and 13 are passed at once.
    let run = rig.run(11);
    let out = rig.at(
        100,
        Input::ActionEnded {
            cue: id(11),
            run,
            at: 100,
        },
    );
    assert_eq!(playing(&out), vec![id(14)]);
    let out = rig.start(200, 1);
    assert_eq!(playing(&out), vec![id(11)]);
    assert_eq!(rig.state(14), CueState::Idle);
    // Looping: from the first child back to the last playable one.
    let group = with(
        grp(
            1,
            playlist(true),
            vec![sound(11), sound(12), {
                let mut c = sound(13);
                c.armed = false;
                c
            }],
        ),
        ST::PlayPrevious,
    );
    let mut rig = Rig::panicking(vec![group]);
    rig.start(0, 1);
    assert_eq!(playing(&rig.start(100, 1)), vec![id(12)]);
}

/// Review M3: a playlist moves on at most once per sample.
#[test]
fn play_next_moves_a_playlist_on_at_most_once_per_sample() {
    let mut rig = playlist_rig(false, ST::PlayNext);
    assert_eq!(playing(&rig.start(100, 1)), vec![id(12)]);
    let out = rig.start(100, 1);
    assert!(playing(&out).is_empty());
    assert_eq!(
        warnings(&out),
        vec![EngineWarning::TriggerIgnored {
            cue: id(1),
            state: CueState::Running
        }]
    );
    assert_eq!(playing(&rig.start(101, 1)), vec![id(13)]);
}
