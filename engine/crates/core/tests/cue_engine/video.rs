// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Video cues (CT-03, VID-03) in the cue engine: their problems, and their picture played as a
//! voice (the binary hands a Video cue's voice to the stage host).
//! Virtual sample clock at 1 kHz: one second is 1000 samples.

use crate::common::*;
use openlab_core::*;

fn rig(cues: Vec<Cue>) -> Rig {
    let mut ws = workspace(cues);
    ws.stages = vec![Stage::new(stage_id(1), "Stage 1", 1920, 1080)];
    let mut rig = Rig::on(ws, EngineConfig::new(RATE, 0, 1).unwrap());
    rig.check_cache = true;
    rig
}

fn video_on(n: u128, stage: Option<StageId>) -> Cue {
    cue(
        n,
        CueKind::Video {
            media: Some(MediaRef::new("media/v.mov")),
            stage,
            duration: None,
        },
    )
}

fn length(rig: &mut Rig, now: SampleTime, n: u128, frames: SampleTime) -> Vec<Action> {
    let run = rig.run(n);
    rig.at(
        now,
        Input::ActionLength {
            cue: id(n),
            run,
            frames,
        },
    )
}

#[test]
fn a_video_cue_with_stage_none_is_broken_with_that_reason_and_does_not_play() {
    let mut rig = rig(vec![video_on(1, None), memo(2)]);
    assert_eq!(rig.problems(1), vec![CueProblem::NoStage]);
    assert_eq!(rig.state(1), CueState::Broken);
    let actions = rig.go(5);
    assert!(voice_starts(&actions).is_empty(), "{actions:?}");
    // Given a stage, it plays.
    if let CueKind::Video { stage, .. } = &mut rig.find_mut(1).kind {
        *stage = Some(stage_id(1));
    }
    rig.engine.model_changed();
    rig.tick(6);
    assert!(rig.problems(1).is_empty());
    rig.set_playhead(7, 1);
    assert_eq!(voice_starts(&rig.go(8)), vec![(id(1), 8)]);
}

#[test]
fn a_video_cue_with_no_file_or_a_stage_that_is_gone_is_broken() {
    let mut no_file = video_on(1, Some(stage_id(1)));
    if let CueKind::Video { media, .. } = &mut no_file.kind {
        *media = None;
    }
    let mut rig = rig(vec![no_file, video_on(2, Some(stage_id(1)))]);
    assert_eq!(rig.problems(1), vec![CueProblem::NoMedia]);
    assert!(rig.problems(2).is_empty());
    // The stage is removed: every cue on it breaks, and comes back with it.
    let stages = std::mem::take(&mut rig.ws.stages);
    rig.engine.model_changed();
    rig.tick(1);
    assert_eq!(
        rig.problems(2),
        vec![CueProblem::StageMissing { stage: stage_id(1) }]
    );
    rig.ws.stages = stages;
    rig.engine.model_changed();
    rig.tick(2);
    assert!(rig.problems(2).is_empty());
}

#[test]
fn a_missing_or_unplayable_video_file_breaks_the_cue_until_it_is_back() {
    let mut rig = rig(vec![video_on(1, Some(stage_id(1)))]);
    let media = MediaRef::new("media/v.mov");
    rig.at(
        1,
        Input::MediaStatus {
            media: media.clone(),
            status: MediaStatus::Missing,
        },
    );
    assert_eq!(
        rig.problems(1),
        vec![CueProblem::MediaMissing {
            media: media.clone()
        }]
    );
    rig.at(
        2,
        Input::MediaStatus {
            media: media.clone(),
            status: MediaStatus::Unsupported {
                reason: "damaged".into(),
            },
        },
    );
    assert_eq!(
        rig.problems(1),
        vec![CueProblem::MediaUnsupported {
            media: media.clone(),
            reason: "damaged".into()
        }]
    );
    rig.at(
        3,
        Input::MediaStatus {
            media,
            status: MediaStatus::Available,
        },
    );
    assert!(rig.problems(1).is_empty());
    assert_eq!(rig.state(1), CueState::Idle);
}

#[test]
fn go_plays_a_video_cue_as_a_voice_that_ends_on_its_last_frame() {
    let mut rig = rig(vec![video_on(1, Some(stage_id(1))), memo(2)]);
    let actions = rig.go(10);
    assert_eq!(voice_starts(&actions), vec![(id(1), 10)]);
    assert_eq!(rig.state(1), CueState::Running);
    // The binary knows the file's length (2 s): the cue ends on that sample, not later.
    length(&mut rig, 10, 1, 2000);
    rig.tick(2009);
    assert_eq!(rig.state(1), CueState::Running);
    let actions = rig.tick(2010);
    assert_eq!(changes(&actions, 1), vec![(CueState::Idle, 2010)]);
}

#[test]
fn a_still_with_no_duration_runs_until_it_is_stopped() {
    let mut rig = rig(vec![video_on(1, Some(stage_id(1)))]);
    rig.go(0);
    rig.tick(3_600_000);
    assert_eq!(rig.state(1), CueState::Running);
    let actions = rig.stop(3_600_001, 1);
    assert_eq!(voice_stops(&actions), vec![(id(1), 3_600_001, None)]);
    assert_eq!(rig.state(1), CueState::Idle);
}

#[test]
fn pause_resume_load_to_time_and_panic_act_on_a_video_cue_as_on_an_audio_cue() {
    let mut rig = rig(vec![video_on(1, Some(stage_id(1)))]);
    rig.ws.settings.panic_duration = 1.0;
    // Load to 1.5 s: the picture is prepared there.
    let actions = rig.at(
        0,
        Input::Load {
            cue: id(1),
            time: 1.5,
        },
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::LoadVoice { offset: 1500, .. })),
        "{actions:?}"
    );
    let actions = rig.go(10);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::StartVoice {
                at: 10,
                offset: 1500,
                ..
            }
        )),
        "{actions:?}"
    );
    let actions = rig.pause(510, 1);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::PauseVoice {
                at: 510,
                offset: 2000,
                ..
            }
        )),
        "{actions:?}"
    );
    assert_eq!(rig.state(1), CueState::Paused);
    let actions = rig.resume(900, 1);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::ResumeVoice {
                at: 900,
                offset: 2000,
                ..
            }
        )),
        "{actions:?}"
    );
    // Panic fades the picture over the panic time, then stops it.
    let actions = rig.at(1000, Input::Panic);
    let stops = voice_stops(&actions);
    assert_eq!(stops.len(), 1, "{actions:?}");
    assert_eq!(stops[0].2.map(|f| f.frames), Some(1000));
}

#[test]
fn a_decode_failure_breaks_only_that_video_cue() {
    let mut rig = rig(vec![
        video_on(1, Some(stage_id(1))),
        video_on(2, Some(stage_id(1))),
    ]);
    rig.start(0, 1);
    rig.start(0, 2);
    let run = rig.run(1);
    rig.at(
        5,
        Input::ActionFailed {
            cue: id(1),
            run,
            reason: BrokenReason::DecodeFailed,
        },
    );
    assert_eq!(rig.state(1), CueState::Broken);
    assert_eq!(rig.state(2), CueState::Running);
}
