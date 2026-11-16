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
            geometry: Default::default(),
            timing: Default::default(),
            sound: Default::default(),
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

/// A Devamp cue may target a Video cue (VID-07: its loops are an Audio cue's): the devamp goes
/// to the picture's voice, and once the binary says where it leaves the loop the cue after the
/// Devamp cue starts on that sample.
#[test]
fn a_devamp_cue_devamps_a_looping_video_cue_and_starts_the_next_cue_at_the_loop_end() {
    let devamp = cue(
        2,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: true,
            stop_target_when_slice_ends: false,
            devamp_mode: DevampMode::CurrentSlice,
        },
    );
    let mut rig = rig(vec![
        video_on(1, Some(stage_id(1))),
        devamp,
        memo(3),
        memo(4),
    ]);
    assert!(rig.problems(2).is_empty(), "{:?}", rig.problems(2));
    rig.start(0, 1);
    rig.set_playhead(0, 2);
    let go = rig.go(100);
    assert!(
        go.iter().any(|a| matches!(
            a,
            Action::DevampVoice {
                at: 100,
                stop: false,
                ..
            }
        )),
        "{go:?}"
    );
    let run = rig.run(1);
    rig.at(
        150,
        Input::DevampScheduled {
            cue: id(1),
            run,
            at: 2000,
        },
    );
    assert!(starts(&rig.tick(1999)).is_empty());
    assert_eq!(starts(&rig.tick(2000)), vec![(id(3), 2000)]);
    assert_eq!(rig.state(1), CueState::Running);
}

/// A Video cue whose trims and slices the binary cannot plan for its file shows broken with
/// why, as an Audio cue does, until the plan can be played again.
#[test]
fn a_video_cue_whose_slices_cannot_play_is_broken_until_fixed() {
    let mut rig = rig(vec![video_on(1, Some(stage_id(1)))]);
    rig.at(
        1,
        Input::SliceProblems {
            cues: vec![(id(1), Some("a looping slice is too short".into()))],
        },
    );
    assert_eq!(
        rig.problems(1),
        vec![CueProblem::SlicesUnplayable {
            reason: "a looping slice is too short".into()
        }]
    );
    // An edit of the trims makes the report stale: the binary checks the new plan.
    if let CueKind::Video { timing, .. } = &mut rig.find_mut(1).kind {
        timing.start_time = 0.5;
    }
    rig.engine.model_changed();
    rig.tick(2);
    assert!(rig.problems(1).is_empty());
}

fn sounding_on(n: u128, patch: Option<PatchId>) -> Cue {
    let mut c = video_on(n, Some(stage_id(1)));
    if let CueKind::Video { sound, .. } = &mut c.kind {
        sound.patch = patch;
    }
    c
}

fn sound(rig: &mut Rig, now: SampleTime, sounds: bool) {
    rig.at(
        now,
        Input::MediaSound {
            media: vec![(MediaRef::new("media/v.mov"), sounds)],
        },
    );
}

/// A Video cue needs its output patch only while its file is told to have sound: then no patch,
/// a patch the workspace lacks and a patch with no device break it as they break an Audio cue,
/// and it does not start; a file without sound is never broken by patches.
#[test]
fn a_video_cue_whose_file_has_sound_needs_its_patch() {
    let mut rig = rig(vec![
        sounding_on(1, None),
        sounding_on(2, Some(patch_id(9))),
        sounding_on(3, Some(patch_id(1))),
    ]);
    for n in 1..=3 {
        assert!(rig.problems(n).is_empty(), "no sound told: cue {n}");
    }
    sound(&mut rig, 1, true);
    assert_eq!(rig.problems(1), vec![CueProblem::NoPatch]);
    assert_eq!(
        rig.problems(2),
        vec![CueProblem::PatchMissing { patch: patch_id(9) }]
    );
    assert!(rig.problems(3).is_empty());
    assert_eq!(rig.state(1), CueState::Broken);
    let actions = rig.at(2, Input::Start { cue: id(1) });
    assert!(voice_starts(&actions).is_empty(), "{actions:?}");
    // The patch loses its device: the device problem, cleared when it is back.
    let status = |resolution| Input::PatchStatus {
        patch: patch_id(1),
        resolution,
    };
    rig.at(3, status(PatchResolution::Unmapped));
    assert_eq!(
        rig.problems(3),
        vec![CueProblem::DeviceUnavailable {
            patch: patch_id(1),
            resolution: PatchResolution::Unmapped
        }]
    );
    let mapped = PatchResolution::Mapped {
        device: "Out".into(),
        channels: vec![0, 1],
    };
    rig.at(4, status(mapped));
    assert!(rig.problems(3).is_empty());
    // A file without sound needs no patch.
    sound(&mut rig, 5, false);
    for n in 1..=3 {
        assert!(rig.problems(n).is_empty(), "no sound: cue {n}");
    }
}

/// A Fade cue may target a Video cue: it fades the levels of its sound as an Audio cue's.
#[test]
fn a_fade_cue_ramps_a_playing_video_cues_levels() {
    let fade = cue(
        2,
        CueKind::Fade {
            target: Some(id(1)),
            duration: 1.0,
            curve: FadeCurve::Linear,
            levels: FadeLevels {
                master_db: Some(-20.0),
                output_db: vec![],
                crosspoints: vec![],
            },
            stop_target_when_done: false,
            mode: FadeMode::default(),
        },
    );
    let mut rig = rig(vec![sounding_on(1, Some(patch_id(1))), fade]);
    assert!(rig.problems(2).is_empty(), "a Video cue is a Fade's target");
    rig.at(1, Input::Start { cue: id(1) });
    let actions = rig.at(100, Input::Start { cue: id(2) });
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::Ramp { cue, param: LevelParam::Master, .. } if *cue == id(1)
        )),
        "{actions:?}"
    );
}
