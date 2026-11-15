// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Seek (a click in a cue's waveform moves where it plays, CUE-05 load to time). Virtual sample
//! clock at 1 kHz: one second is 1000 samples, and a file position of 1.5 s is frame 1500.

use crate::common::*;
use openlab_core::*;

use ContinueMode::{AutoContinue as AC, AutoFollow as AF, DoNotContinue as DNC};

impl Rig {
    fn seek(&mut self, now: SampleTime, n: u128, seconds: f64) -> Vec<Action> {
        self.at(
            now,
            Input::Seek {
                cue: id(n),
                position: seconds,
            },
        )
    }
}

/// The `SeekVoice` actions: `(run, from, at, position)`.
fn seeks(actions: &[Action]) -> Vec<(RunId, RunId, SampleTime, SampleTime)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::SeekVoice {
                run,
                from,
                at,
                position,
                ..
            } => Some((*run, *from, *at, *position)),
            _ => None,
        })
        .collect()
}

fn stops_or_starts(actions: &[Action]) -> usize {
    actions
        .iter()
        .filter(|a| {
            matches!(
                a,
                Action::StopVoice { .. }
                    | Action::StartVoice { .. }
                    | Action::PauseVoice { .. }
                    | Action::ResumeVoice { .. }
                    | Action::UnloadVoice { .. }
            )
        })
        .count()
}

#[test]
fn idle_cue_click_loads_at_the_point_and_go_plays_from_there() {
    let mut rig = Rig::new(vec![timed(sound(1), 1.0, 0.0, DNC)]);
    let load = rig.seek(0, 1, 1.5);
    let run = rig.run(1);
    assert_eq!(
        load,
        vec![
            Action::LoadVoiceAt {
                cue: id(1),
                run,
                position: 1500
            },
            Action::StateChanged {
                cue: id(1),
                state: CueState::Loaded,
                at: 0
            }
        ]
    );
    // GO plays the stream loaded there at once: the point clicked is where the sound starts,
    // so the pre-wait is past.
    let start = rig.start(2000, 1);
    assert!(start.contains(&Action::StartVoice {
        cue: id(1),
        run,
        at: 2000,
        offset: 0
    }));
    assert!(
        !start
            .iter()
            .any(|a| matches!(a, Action::LoadVoiceAt { .. } | Action::LoadVoice { .. })),
        "{start:?}"
    );
    // A second click on the loaded cue loads it at the new point instead.
    rig.stop(2100, 1);
    rig.seek(2200, 1, 0.5);
    let again = rig.seek(2300, 1, 0.75);
    assert!(
        again
            .iter()
            .any(|a| matches!(a, Action::UnloadVoice { .. }))
    );
    assert!(
        again
            .iter()
            .any(|a| matches!(a, Action::LoadVoiceAt { position: 750, .. }))
    );
    assert_eq!(rig.state(1), CueState::Loaded);
}

#[test]
fn a_cue_loaded_by_a_click_while_disarmed_plays_from_the_point_once_armed() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.find_mut(1).armed = false;
    let load = rig.seek(0, 1, 0.5);
    assert!(!load.iter().any(|a| matches!(a, Action::LoadVoiceAt { .. })));
    rig.find_mut(1).armed = true;
    let start = rig.start(100, 1);
    let run = rig.run(1);
    let at = start
        .iter()
        .position(|a| {
            *a == Action::LoadVoiceAt {
                cue: id(1),
                run,
                position: 500,
            }
        })
        .expect("loaded at the point");
    let go = start
        .iter()
        .position(|a| matches!(a, Action::StartVoice { offset: 0, .. }))
        .expect("started");
    assert!(at < go, "{start:?}");
}

#[test]
fn playing_cue_click_jumps_there_without_a_stop_or_a_restart() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.start(0, 1);
    let old = rig.run(1);
    let moved = rig.seek(400, 1, 2.0);
    let fresh = rig.run(1);
    assert_ne!(fresh, old);
    assert_eq!(seeks(&moved), vec![(fresh, old, 400, 2000)]);
    assert_eq!(stops_or_starts(&moved), 0, "{moved:?}");
    assert!(
        !moved
            .iter()
            .any(|a| matches!(a, Action::StateChanged { .. })),
        "{moved:?}"
    );
    assert_eq!(rig.state(1), CueState::Running);
    // The voice it moved from is no one's: its end report changes nothing.
    let late = rig.at(
        500,
        Input::ActionEnded {
            cue: id(1),
            run: old,
            at: 450,
        },
    );
    assert!(changes(&late, 1).is_empty(), "{late:?}");
    assert_eq!(rig.state(1), CueState::Running);
}

#[test]
fn auto_follow_after_a_click_fires_at_the_new_end_once() {
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 0.0, AF), sound(2)]);
    rig.start(0, 1);
    let old = rig.run(1);
    rig.at(
        0,
        Input::ActionLength {
            cue: id(1),
            run: old,
            frames: 1000,
        },
    );
    assert_eq!(rig.engine.next_deadline(), Some(1000));
    rig.seek(200, 1, 5.0);
    // Until the moved voice's length is known the old end is gone.
    assert_eq!(rig.engine.next_deadline(), None);
    let fresh = rig.run(1);
    rig.at(
        210,
        Input::ActionLength {
            cue: id(1),
            run: fresh,
            frames: 300,
        },
    );
    // Counted from the move: 200 + 300.
    assert_eq!(rig.engine.next_deadline(), Some(500));
    let follow = rig.tick(500);
    assert_eq!(starts(&follow), vec![(id(2), 500)]);
    // The old voice's end (it was moved from before 1000) starts nothing again.
    let stale = rig.at(
        1001,
        Input::ActionEnded {
            cue: id(1),
            run: old,
            at: 1000,
        },
    );
    assert!(starts(&stale).is_empty(), "{stale:?}");
    assert!(starts(&rig.tick(1100)).is_empty());
}

#[test]
fn paused_cue_click_moves_and_stays_paused_and_resume_plays_from_there() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.start(0, 1);
    rig.pause(300, 1);
    let old = rig.run(1);
    let moved = rig.seek(500, 1, 1.0);
    let fresh = rig.run(1);
    assert_eq!(seeks(&moved), vec![(fresh, old, 500, 1000)]);
    assert_eq!(rig.state(1), CueState::Paused);
    assert!(changes(&moved, 1).is_empty(), "{moved:?}");
    let resumed = rig.resume(800, 1);
    let resume = resumed
        .iter()
        .find_map(|a| match a {
            Action::ResumeVoice {
                paused, at, offset, ..
            } => Some((*paused, *at, *offset)),
            _ => None,
        })
        .unwrap();
    // The moved voice resumes from where it was moved to (offset 0 of the moved voice).
    assert_eq!(resume, (fresh, 800, 0));
    assert_eq!(rig.state(1), CueState::Running);
}

#[test]
fn pre_wait_click_ends_the_pre_wait_and_starts_from_the_point_now() {
    let mut rig = Rig::new(vec![timed(sound(1), 2.0, 0.0, DNC)]);
    rig.start(0, 1);
    assert_eq!(rig.state(1), CueState::PreWaiting);
    let run = rig.run(1);
    let now = rig.seek(300, 1, 1.25);
    let load = now
        .iter()
        .position(|a| {
            *a == Action::LoadVoiceAt {
                cue: id(1),
                run,
                position: 1250,
            }
        })
        .expect("loaded at the point");
    let start = now
        .iter()
        .position(|a| {
            *a == Action::StartVoice {
                cue: id(1),
                run,
                at: 300,
                offset: 0,
            }
        })
        .expect("started now");
    assert!(load < start);
    assert_eq!(rig.state(1), CueState::Running);
    // The pre-wait's old end starts nothing more.
    assert!(voice_starts(&rig.tick(2000)).is_empty());
}

#[test]
fn a_click_in_a_paused_pre_wait_starts_the_point_on_the_resume() {
    let mut rig = Rig::new(vec![timed(sound(1), 2.0, 0.0, DNC)]);
    rig.start(0, 1);
    rig.pause(100, 1);
    let moved = rig.seek(200, 1, 0.5);
    assert!(voice_starts(&moved).is_empty(), "{moved:?}");
    assert_eq!(rig.state(1), CueState::Paused);
    let resumed = rig.resume(900, 1);
    assert_eq!(voice_starts(&resumed), vec![(id(1), 900)]);
    assert!(
        resumed
            .iter()
            .any(|a| matches!(a, Action::LoadVoiceAt { position: 500, .. }))
    );
}

#[test]
fn post_wait_click_moves_only_the_sound_and_the_continue_fires_once_on_time() {
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 2.0, AC), sound(2)]);
    rig.start(0, 1);
    let moved = rig.seek(500, 1, 3.0);
    assert_eq!(seeks(&moved).len(), 1);
    assert_eq!(rig.engine.next_deadline(), Some(2000));
    let cont = rig.tick(2000);
    assert_eq!(starts(&cont), vec![(id(2), 2000)]);
    assert!(starts(&rig.tick(4000)).is_empty());
}

#[test]
fn a_click_after_the_sound_ended_in_the_post_wait_is_ignored() {
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 2.0, AC), sound(2)]);
    rig.start(0, 1);
    rig.ended(600, 1, 500);
    assert_eq!(rig.state(1), CueState::PostWaiting);
    let click = rig.seek(700, 1, 0.2);
    assert_eq!(
        warnings(&click),
        vec![EngineWarning::LoadIgnored { cue: id(1) }]
    );
    assert!(seeks(&click).is_empty());
    assert_eq!(starts(&rig.tick(2000)), vec![(id(2), 2000)]);
}

#[test]
fn a_click_on_a_stopping_cue_or_one_that_is_not_audio_is_ignored() {
    let mut rig = Rig::new(vec![sound(1), memo(2)]);
    rig.start(0, 1);
    rig.at(
        100,
        Input::Stop {
            cue: id(1),
            fade_out: Some(FadeSpec {
                duration: 1.0,
                curve: FadeCurve::Linear,
            }),
        },
    );
    let click = rig.seek(200, 1, 0.5);
    assert_eq!(
        warnings(&click),
        vec![EngineWarning::LoadIgnored { cue: id(1) }]
    );
    assert!(seeks(&click).is_empty());
    let memo = rig.seek(200, 2, 0.5);
    assert_eq!(
        warnings(&memo),
        vec![EngineWarning::LoadIgnored { cue: id(2) }]
    );
    let unknown = rig.seek(200, 9, 0.5);
    assert_eq!(
        warnings(&unknown),
        vec![EngineWarning::UnknownCue { cue: id(9) }]
    );
}

#[test]
fn broken_cue_click_loads_nothing_and_stays_broken() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.at(
        0,
        Input::MediaStatus {
            media: MediaRef::new("media/a.wav"),
            status: MediaStatus::Missing,
        },
    );
    let click = rig.seek(10, 1, 0.5);
    assert_eq!(
        warnings(&click),
        vec![EngineWarning::CueBroken { cue: id(1) }]
    );
    assert!(
        !click
            .iter()
            .any(|a| matches!(a, Action::LoadVoiceAt { .. } | Action::SeekVoice { .. }))
    );
    assert_eq!(rig.state(1), CueState::Broken);
    // Broken at run time too: a failed voice is not moved.
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 1.0, AC), sound(2)]);
    rig.start(0, 1);
    rig.failed(100, 1);
    let click = rig.seek(200, 1, 0.5);
    assert_eq!(
        warnings(&click),
        vec![EngineWarning::CueBroken { cue: id(1) }]
    );
    assert!(seeks(&click).is_empty());
}

#[test]
fn a_cue_its_lost_device_left_paused_loads_nothing_while_the_device_is_away() {
    let mut rig = Rig::new(vec![sound(1)]);
    let mapped = PatchResolution::Mapped {
        device: "Interface".into(),
        channels: vec![0, 1],
    };
    let missing = PatchResolution::DeviceMissing {
        device: "Interface".into(),
    };
    rig.at(
        0,
        Input::PatchStatus {
            patch: patch_id(1),
            resolution: mapped.clone(),
        },
    );
    rig.start(10, 1);
    rig.at(
        20,
        Input::PatchStatus {
            patch: patch_id(1),
            resolution: missing,
        },
    );
    let run = rig.run(1);
    rig.at(
        30,
        Input::ActionFailed {
            cue: id(1),
            run,
            reason: BrokenReason::DeviceLost,
        },
    );
    rig.at(30, Input::DeviceLost);
    assert!(rig.engine.is_lost(id(1)));
    let click = rig.seek(40, 1, 0.5);
    assert_eq!(
        warnings(&click),
        vec![EngineWarning::CueBroken { cue: id(1) }]
    );
    assert!(
        !click
            .iter()
            .any(|a| matches!(a, Action::LoadVoiceAt { .. } | Action::SeekVoice { .. })),
        "{click:?}"
    );
    assert!(rig.engine.is_lost(id(1)));
    // Back: the mark clears and the click loads it there.
    rig.at(
        50,
        Input::PatchStatus {
            patch: patch_id(1),
            resolution: mapped,
        },
    );
    let back = rig.seek(60, 1, 0.5);
    assert!(
        back.iter()
            .any(|a| matches!(a, Action::LoadVoiceAt { position: 500, .. }))
    );
    assert_eq!(rig.state(1), CueState::Loaded);
}

#[test]
fn a_fade_on_the_cue_carries_on_on_the_moved_voice() {
    let mut rig = Rig::new(vec![sound(1), cue(2, fade(1))]);
    rig.start(0, 1);
    rig.start(0, 2);
    let half = rig.level(1, LevelParam::Master, 1000).unwrap();
    let moved = rig.seek(1000, 1, 3.0);
    let fresh = rig.run(1);
    let ramps: Vec<(SampleTime, f32, SampleTime)> = moved
        .iter()
        .filter_map(|a| match a {
            Action::Ramp {
                run,
                param: LevelParam::Master,
                at,
                gain,
                frames,
                ..
            } if *run == fresh => Some((*at, *gain, *frames)),
            _ => None,
        })
        .collect();
    // On its first sample it has the fade's level there, then goes on to the fade's end.
    assert_eq!(ramps.len(), 2, "{moved:?}");
    assert_eq!((ramps[0].0, ramps[0].2), (1000, 0));
    assert!((ramps[0].1 - half).abs() < 1e-6);
    assert_eq!((ramps[1].0, ramps[1].1, ramps[1].2), (1000, 0.0, 4000));
    // The fade still stops its target when it completes, the moved voice.
    let done = rig.tick(5000);
    assert!(done.iter().any(|a| matches!(
        a,
        Action::StopVoice { run, .. } if *run == fresh
    )));
}

#[test]
fn a_ducked_cue_stays_ducked_after_a_click() {
    let mut ducker = timed(sound(2), 0.0, 0.0, DNC);
    ducker.duck_others = DuckOthers {
        enabled: true,
        db: -12.0,
        duration: 1.0,
    };
    let mut rig = Rig::new(vec![sound(1), ducker]);
    rig.start(0, 1);
    rig.start(0, 2);
    let moved = rig.seek(2000, 1, 3.0);
    let fresh = rig.run(1);
    let ducks: Vec<(f32, SampleTime)> = moved
        .iter()
        .filter_map(|a| match a {
            Action::DuckVoice {
                run, gain, frames, ..
            } if *run == fresh => Some((*gain, *frames)),
            _ => None,
        })
        .collect();
    let want = level_db(-12.0);
    assert_eq!(ducks.len(), 1, "{moved:?}");
    assert!((ducks[0].0 - want).abs() < 1e-4, "{ducks:?}");
    assert_eq!(ducks[0].1, 0);
}

fn level_db(db: f64) -> f32 {
    10f64.powf(db / 20.0) as f32
}

#[test]
fn a_pending_devamp_goes_to_the_moved_voice() {
    let mut loop_cue = sound(1);
    if let CueKind::Audio {
        last_slice_play_count,
        ..
    } = &mut loop_cue.kind
    {
        *last_slice_play_count = 0;
    }
    let devamp = cue(
        2,
        CueKind::Devamp {
            target: Some(cue_id(1)),
            start_next_cue: false,
            stop_target_when_slice_ends: false,
            devamp_mode: Default::default(),
        },
    );
    let mut rig = Rig::new(vec![loop_cue, devamp]);
    rig.start(0, 1);
    rig.start(100, 2);
    let moved = rig.seek(200, 1, 0.5);
    let fresh = rig.run(1);
    assert!(moved.contains(&Action::DevampVoice {
        cue: id(1),
        run: fresh,
        at: 200,
        stop: false
    }));
}

#[test]
fn a_group_child_moved_while_its_group_runs_leaves_its_siblings_alone() {
    let mut rig = Rig::new(vec![cue(10, group(vec![sound(1), sound(2)]))]);
    rig.start(0, 10);
    let sibling = rig.run(2);
    let moved = rig.seek(300, 1, 1.0);
    assert!(
        moved.iter().all(|a| match a {
            Action::SeekVoice { cue, .. } => *cue == id(1),
            Action::StateChanged { .. } => false,
            Action::Ramp { cue, .. } | Action::DuckVoice { cue, .. } => *cue == id(1),
            _ => true,
        }),
        "{moved:?}"
    );
    assert_eq!(rig.run(2), sibling);
    assert_eq!(rig.state(10), CueState::Running);
    assert_eq!(rig.state(2), CueState::Running);
}

#[test]
fn previewing_cue_click_jumps_the_preview_and_does_not_end_it() {
    let mut rig = Rig::new(vec![sound(1)]);
    let started = rig.preview(0, 1);
    let old = started
        .iter()
        .find_map(|a| match a {
            Action::PreviewStart { run, .. } => Some(*run),
            _ => None,
        })
        .unwrap();
    let moved = rig.seek(300, 1, 2.0);
    let fresh = rig.engine.preview_run(id(1)).unwrap();
    assert_ne!(fresh, old);
    assert!(fresh.0 >= PREVIEW_RUN_BASE);
    assert!(seeks(&moved).contains(&(fresh, old, 300, 2000)));
    assert!(
        !moved.iter().any(|a| matches!(
            a,
            Action::PreviewChanged { .. } | Action::PreviewStop { .. }
        )),
        "{moved:?}"
    );
    assert!(rig.engine.is_previewing(id(1)));
    // The idle show cue is loaded at the point too.
    assert_eq!(rig.state(1), CueState::Loaded);
    // Stopping the preview stops the moved voice.
    let stop = rig.stop_preview(400, 1);
    assert!(stop.iter().any(|a| matches!(
        a,
        Action::PreviewStop { run, .. } if *run == fresh
    )));
}

#[test]
fn a_burst_of_clicks_moves_one_voice_and_costs_a_go_nothing() {
    let mut rig = Rig::new(vec![sound(1), sound(2)]);
    rig.start(0, 1);
    let mut all = Vec::new();
    for k in 0..200u32 {
        all.extend(rig.seek(100, 1, f64::from(k) / 100.0));
    }
    let moves = seeks(&all);
    assert_eq!(moves.len(), 200);
    // Each move is from the one before: one voice, never several.
    for pair in moves.windows(2) {
        assert_eq!(pair[1].1, pair[0].0);
    }
    assert_eq!(stops_or_starts(&all), 0);
    assert_eq!(rig.state(1), CueState::Running);
    // The budget a GO needs is untouched.
    assert_eq!(starts(&rig.start(100, 2)), vec![(id(2), 100)]);
}

#[test]
fn hostile_positions_are_taken_as_the_file_start_or_its_far_end() {
    let mut rig = Rig::new(vec![sound(1)]);
    for (secs, frame) in [
        (f64::NAN, 0),
        (-3.0, 0),
        (f64::NEG_INFINITY, 0),
        (f64::INFINITY, SampleTime::MAX),
        (1e300, SampleTime::MAX),
    ] {
        let load = rig.seek(0, 1, secs);
        assert!(
            load.iter().any(|a| matches!(
                a,
                Action::LoadVoiceAt { position, .. } if *position == frame
            )),
            "{secs}: {load:?}"
        );
    }
}

// ----- a playing cue follows its patch to another device (AUD-07) -----

impl Rig {
    fn device_moved(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        let run = self.run(n);
        self.at(now, Input::DeviceMoved { cue: id(n), run })
    }
}

/// The `RelocateVoice` actions: `(run, from, at, position)`.
fn relocations(actions: &[Action]) -> Vec<(RunId, RunId, SampleTime, SampleTime)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::RelocateVoice {
                run,
                from,
                at,
                position,
                ..
            } => Some((*run, *from, *at, *position)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_playing_cue_moved_to_another_device_plays_on_in_step_and_follows_once() {
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 0.0, AF), sound(2)]);
    rig.start(0, 1);
    let old = rig.run(1);
    rig.at(
        0,
        Input::ActionLength {
            cue: id(1),
            run: old,
            frames: 1000,
        },
    );
    let moved = rig.device_moved(400, 1);
    let fresh = rig.run(1);
    // Where it is now, no restart, no stop and no state change.
    assert_eq!(relocations(&moved), vec![(fresh, old, 400, 400)]);
    assert_eq!(stops_or_starts(&moved), 0, "{moved:?}");
    assert!(changes(&moved, 1).is_empty(), "{moved:?}");
    assert_eq!(rig.state(1), CueState::Running);
    // The moved voice tells its length counted from the action's start, as before.
    rig.at(
        410,
        Input::ActionLength {
            cue: id(1),
            run: fresh,
            frames: 1000,
        },
    );
    assert_eq!(rig.engine.next_deadline(), Some(1000));
    assert_eq!(starts(&rig.tick(1000)), vec![(id(2), 1000)]);
    // The old voice's end on the old device starts nothing again.
    let stale = rig.at(
        1001,
        Input::ActionEnded {
            cue: id(1),
            run: old,
            at: 1000,
        },
    );
    assert!(starts(&stale).is_empty(), "{stale:?}");
    assert!(starts(&rig.tick(1500)).is_empty());
}

#[test]
fn a_paused_cue_moved_to_another_device_stays_paused_and_resumes_from_where_it_froze() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.start(0, 1);
    rig.pause(300, 1);
    let old = rig.run(1);
    let moved = rig.device_moved(500, 1);
    let fresh = rig.run(1);
    assert_eq!(relocations(&moved), vec![(fresh, old, 500, 300)]);
    assert_eq!(rig.state(1), CueState::Paused);
    let resumed = rig.resume(800, 1);
    let resume = resumed
        .iter()
        .find_map(|a| match a {
            Action::ResumeVoice {
                paused, at, offset, ..
            } => Some((*paused, *at, *offset)),
            _ => None,
        })
        .unwrap();
    assert_eq!(resume, (fresh, 800, 300));
    assert_eq!(rig.state(1), CueState::Running);
}

#[test]
fn a_fade_in_progress_and_a_pending_devamp_carry_over_to_the_moved_voice() {
    let mut loop_cue = sound(1);
    if let CueKind::Audio {
        last_slice_play_count,
        ..
    } = &mut loop_cue.kind
    {
        *last_slice_play_count = 0;
    }
    let devamp = cue(
        3,
        CueKind::Devamp {
            target: Some(cue_id(1)),
            start_next_cue: false,
            stop_target_when_slice_ends: false,
            devamp_mode: Default::default(),
        },
    );
    let mut rig = Rig::new(vec![loop_cue, cue(2, fade(1)), devamp]);
    rig.start(0, 1);
    rig.start(0, 2);
    rig.start(100, 3);
    let half = rig.level(1, LevelParam::Master, 1000).unwrap();
    let moved = rig.device_moved(1000, 1);
    let fresh = rig.run(1);
    let ramps: Vec<(SampleTime, f32, SampleTime)> = moved
        .iter()
        .filter_map(|a| match a {
            Action::Ramp {
                run,
                param: LevelParam::Master,
                at,
                gain,
                frames,
                ..
            } if *run == fresh => Some((*at, *gain, *frames)),
            _ => None,
        })
        .collect();
    assert_eq!(ramps.len(), 2, "{moved:?}");
    assert!((ramps[0].1 - half).abs() < 1e-6);
    assert_eq!((ramps[1].0, ramps[1].1, ramps[1].2), (1000, 0.0, 4000));
    assert!(moved.contains(&Action::DevampVoice {
        cue: id(1),
        run: fresh,
        at: 1000,
        stop: false
    }));
}

#[test]
fn a_ducked_cue_moved_to_another_device_stays_ducked() {
    let mut ducker = timed(sound(2), 0.0, 0.0, DNC);
    ducker.duck_others = DuckOthers {
        enabled: true,
        db: -12.0,
        duration: 1.0,
    };
    let mut rig = Rig::new(vec![sound(1), ducker]);
    rig.start(0, 1);
    rig.start(0, 2);
    let moved = rig.device_moved(2000, 1);
    let fresh = rig.run(1);
    assert!(
        moved.iter().any(|a| matches!(
            a,
            Action::DuckVoice { run, gain, frames: 0, .. }
                if *run == fresh && (*gain - level_db(-12.0)).abs() < 1e-4
        )),
        "{moved:?}"
    );
}

#[test]
fn a_device_move_of_a_stale_stopping_or_waiting_run_does_nothing() {
    let mut rig = Rig::new(vec![timed(sound(1), 2.0, 0.0, DNC), sound(2)]);
    // In its pre-wait: nothing plays yet (its start plays on the patch's device then).
    rig.start(0, 1);
    assert!(relocations(&rig.device_moved(100, 1)).is_empty());
    // A stale report names a run the cue no longer has.
    rig.start(0, 2);
    let stale = rig.at(
        200,
        Input::DeviceMoved {
            cue: id(2),
            run: RunId(9999),
        },
    );
    assert!(relocations(&stale).is_empty(), "{stale:?}");
    // Stopping with a fade: it ends where it is.
    rig.at(
        300,
        Input::Stop {
            cue: id(2),
            fade_out: Some(FadeSpec {
                duration: 1.0,
                curve: FadeCurve::Linear,
            }),
        },
    );
    assert!(relocations(&rig.device_moved(400, 2)).is_empty());
}

#[test]
fn a_group_child_moved_to_another_device_leaves_its_siblings_alone() {
    let mut rig = Rig::new(vec![cue(10, group(vec![sound(1), sound(2)]))]);
    rig.start(0, 10);
    let sibling = rig.run(2);
    let moved = rig.device_moved(300, 1);
    assert!(
        moved.iter().all(|a| match a {
            Action::RelocateVoice { cue, .. }
            | Action::Ramp { cue, .. }
            | Action::DuckVoice { cue, .. } => *cue == id(1),
            Action::StateChanged { .. } => false,
            _ => true,
        }),
        "{moved:?}"
    );
    assert_eq!(rig.run(2), sibling);
    assert_eq!(rig.state(10), CueState::Running);
}
