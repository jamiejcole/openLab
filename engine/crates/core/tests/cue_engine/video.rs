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
            cues: vec![(
                id(1),
                Some(SliceProblem::Unplayable {
                    reason: "a looping slice is too short".into(),
                }),
            )],
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
            geometry: Default::default(),
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

// Fade cues on a Video cue's picture (VID-05).

const OPACITY: LevelParam = LevelParam::Picture(PictureParam::Opacity);

/// A Fade of Video cue `target` over `seconds` with `curve`, moving the picture values `set`
/// ticks, in `mode`.
fn picture_fade(
    n: u128,
    target: u128,
    seconds: f64,
    curve: FadeCurve,
    mode: FadeMode,
    set: impl FnOnce(&mut FadeGeometry),
) -> Cue {
    let mut geometry = FadeGeometry::default();
    set(&mut geometry);
    cue(
        n,
        CueKind::Fade {
            target: Some(id(target)),
            duration: seconds,
            curve,
            levels: FadeLevels {
                master_db: None,
                output_db: vec![],
                crosspoints: vec![],
            },
            stop_target_when_done: false,
            mode,
            geometry,
        },
    )
}

/// A linear absolute Fade of cue 1's opacity to `to` over `seconds`.
fn opacity_fade(n: u128, seconds: f64, to: f64) -> Cue {
    picture_fade(n, 1, seconds, FadeCurve::Linear, FadeMode::Absolute, |g| {
        g.fade_opacity = true;
        g.opacity = to;
    })
}

/// Each picture ramp: (cue, param, at, to, frames, curve).
fn picture_ramps(
    actions: &[Action],
) -> Vec<(CueId, PictureParam, SampleTime, f32, SampleTime, FadeCurve)> {
    actions
        .iter()
        .filter_map(|a| match *a {
            Action::Ramp {
                cue,
                at,
                param: LevelParam::Picture(p),
                gain,
                frames,
                curve,
                ..
            } => Some((cue, p, at, gain, frames, curve)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_fade_moves_only_the_ticked_picture_values_along_its_curve() {
    let fade = picture_fade(2, 1, 3.0, FadeCurve::SCurve, FadeMode::Absolute, |g| {
        g.fade_opacity = true;
        g.opacity = 0.0;
        // Unticked: kept, never faded.
        g.translation = Pair { x: 500.0, y: 0.0 };
        g.scale = Pair { x: 4.0, y: 4.0 };
    });
    let mut rig = rig(vec![video_on(1, Some(stage_id(1))), fade]);
    assert!(
        rig.problems(2).is_empty(),
        "a ticked picture value is something to fade"
    );
    rig.start(0, 1);
    let go = rig.start(1000, 2);
    assert_eq!(
        picture_ramps(&go),
        vec![(
            id(1),
            PictureParam::Opacity,
            1000,
            0.0,
            3000,
            FadeCurve::SCurve
        )]
    );
    // No level of its sound moves: none was set.
    assert!(go.iter().all(|a| !matches!(
        a,
        Action::Ramp {
            param: LevelParam::Master | LevelParam::Output(_) | LevelParam::Crosspoint { .. },
            ..
        }
    )));
    assert_eq!(rig.state(1), CueState::Fading);
    let half = ramp_value(1.0, 0.0, 1500, 3000, FadeCurve::SCurve) as f32;
    assert_eq!(rig.level(1, OPACITY, 2500), Some(half));
    assert_eq!(
        rig.level(1, LevelParam::Picture(PictureParam::ScaleX), 2500),
        None
    );
    rig.tick(4000);
    assert_eq!(rig.state(1), CueState::Running);
    assert_eq!(rig.level(1, OPACITY, 4000), Some(0.0));
}

#[test]
fn a_picture_fade_with_stop_target_when_done_stops_the_video_on_its_last_sample() {
    let mut fade = opacity_fade(2, 2.0, 0.0);
    if let CueKind::Fade {
        stop_target_when_done,
        ..
    } = &mut fade.kind
    {
        *stop_target_when_done = true;
    }
    let mut rig = rig(vec![video_on(1, Some(stage_id(1))), fade]);
    rig.start(0, 1);
    rig.start(500, 2);
    assert!(voice_stops(&rig.tick(2499)).is_empty());
    assert_eq!(voice_stops(&rig.tick(2500)), vec![(id(1), 2500, None)]);
}

#[test]
fn a_relative_picture_fade_multiplies_opacity_and_scale_and_adds_to_position_and_rotation() {
    let mut target = video_on(1, Some(stage_id(1)));
    if let CueKind::Video { geometry, .. } = &mut target.kind {
        geometry.opacity = 0.8;
        geometry.translation = Pair { x: 50.0, y: -20.0 };
        geometry.scale = Pair { x: 3.0, y: 0.5 };
        geometry.rotation = 300.0;
    }
    let fade = picture_fade(2, 1, 1.0, FadeCurve::Linear, FadeMode::Relative, |g| {
        g.fade_opacity = true;
        g.opacity = 0.5;
        g.fade_translation = true;
        g.translation = Pair { x: 100.0, y: 10.0 };
        g.fade_scale = true;
        g.scale = Pair { x: 2.0, y: 2.0 };
        g.fade_rotation = true;
        g.rotation = 90.0;
    });
    let mut rig = rig(vec![target, fade]);
    rig.start(0, 1);
    let go = rig.start(0, 2);
    let to: Vec<(PictureParam, f32)> = picture_ramps(&go).iter().map(|r| (r.1, r.3)).collect();
    assert_eq!(
        to,
        vec![
            (PictureParam::Opacity, 0.4),
            (PictureParam::TranslationX, 150.0),
            (PictureParam::TranslationY, -10.0),
            (PictureParam::ScaleX, 6.0),
            (PictureParam::ScaleY, 1.0),
            // Within its range: 390 would be past a turn.
            (PictureParam::Rotation, 360.0),
        ]
    );
    // Run again from where the first left them: relative to the values now.
    rig.tick(1000);
    rig.stop(1000, 2);
    let again = rig.start(1500, 2);
    let to: Vec<(PictureParam, f32)> = picture_ramps(&again).iter().map(|r| (r.1, r.3)).collect();
    assert_eq!(to[0], (PictureParam::Opacity, 0.2));
    assert_eq!(to[1], (PictureParam::TranslationX, 250.0));
}

#[test]
fn a_relative_fade_leaves_a_transparent_picture_alone() {
    let mut target = video_on(1, Some(stage_id(1)));
    if let CueKind::Video { geometry, .. } = &mut target.kind {
        geometry.opacity = 0.0;
    }
    let fade = picture_fade(2, 1, 1.0, FadeCurve::Linear, FadeMode::Relative, |g| {
        g.fade_opacity = true;
        g.opacity = 0.5;
    });
    let mut rig = rig(vec![target, fade]);
    rig.start(0, 1);
    assert!(picture_ramps(&rig.start(0, 2)).is_empty());
}

#[test]
fn a_later_picture_fade_takes_the_value_over_from_where_it_is() {
    let mut rig = rig(vec![
        video_on(1, Some(stage_id(1))),
        opacity_fade(2, 4.0, 0.0),
        opacity_fade(3, 1.0, 1.0),
    ]);
    rig.start(0, 1);
    rig.start(0, 2);
    let second = rig.start(1000, 3);
    assert_eq!(
        picture_ramps(&second),
        vec![(
            id(1),
            PictureParam::Opacity,
            1000,
            1.0,
            1000,
            FadeCurve::Linear
        )]
    );
    // From where the first had it: no jump.
    assert_eq!(rig.level(1, OPACITY, 1000), Some(0.75));
    assert_eq!(rig.level(1, OPACITY, 1500), Some(0.875));
    // The first no longer owns it: pausing or stopping it sends nothing.
    assert!(picture_ramps(&rig.pause(1500, 2)).is_empty());
    assert!(picture_ramps(&rig.stop(1600, 2)).is_empty());
    rig.tick(2000);
    assert_eq!(rig.level(1, OPACITY, 2000), Some(1.0));
}

#[test]
fn pausing_a_picture_fade_holds_its_value_and_resuming_carries_on() {
    let mut rig = rig(vec![
        video_on(1, Some(stage_id(1))),
        opacity_fade(2, 4.0, 0.0),
    ]);
    rig.start(0, 1);
    rig.start(0, 2);
    // Held where it is, at once (a picture has no de-zipper).
    assert_eq!(
        picture_ramps(&rig.pause(1000, 2)),
        vec![(
            id(1),
            PictureParam::Opacity,
            1000,
            0.75,
            0,
            FadeCurve::Linear
        )]
    );
    assert_eq!(rig.level(1, OPACITY, 2500), Some(0.75));
    assert_eq!(
        picture_ramps(&rig.resume(3000, 2)),
        vec![(
            id(1),
            PictureParam::Opacity,
            3000,
            0.0,
            3000,
            FadeCurve::Linear
        )]
    );
    assert_eq!(rig.level(1, OPACITY, 4500), Some(0.375));
    rig.tick(6000);
    assert_eq!(rig.level(1, OPACITY, 6000), Some(0.0));
}

#[test]
fn a_panic_holds_a_picture_fade_where_it_is_while_the_picture_fades_out() {
    let mut rig = rig(vec![
        video_on(1, Some(stage_id(1))),
        opacity_fade(2, 4.0, 0.0),
    ]);
    rig.ws.settings.panic_duration = 2.0;
    rig.start(0, 1);
    rig.start(0, 2);
    let panic = rig.at(1000, Input::Panic);
    assert_eq!(
        picture_ramps(&panic),
        vec![(
            id(1),
            PictureParam::Opacity,
            1000,
            0.75,
            0,
            FadeCurve::Linear
        )]
    );
    let stops = voice_stops(&panic);
    assert_eq!(stops.len(), 1, "{panic:?}");
    assert_eq!((stops[0].0, stops[0].1), (id(1), 1000));
    assert_eq!(stops[0].2.map(|f| f.frames), Some(2000));
}

#[test]
fn a_picture_fade_on_a_video_that_is_not_running_moves_nothing() {
    let mut rig = rig(vec![
        video_on(1, Some(stage_id(1))),
        opacity_fade(2, 1.0, 0.0),
    ]);
    let go = rig.start(0, 2);
    assert!(picture_ramps(&go).is_empty());
    // Said to the binary only; the app lists no such warning.
    assert_eq!(
        warnings(&go),
        vec![EngineWarning::TargetNotRunning {
            cue: id(2),
            target: id(1)
        }]
    );
    assert_eq!(rig.state(2), CueState::Running);
    rig.tick(1000);
    assert_eq!(rig.state(2), CueState::Idle);
}

#[test]
fn an_edit_of_a_value_a_fade_moves_takes_it_over_and_ends_the_fade_without_stopping() {
    let mut fade = opacity_fade(2, 4.0, 0.0);
    if let CueKind::Fade {
        stop_target_when_done,
        ..
    } = &mut fade.kind
    {
        *stop_target_when_done = true;
    }
    let mut rig = rig(vec![video_on(1, Some(stage_id(1))), fade]);
    rig.start(0, 1);
    rig.start(0, 2);
    // An edit of another value changes nothing for the fade.
    if let CueKind::Video { geometry, .. } = &mut rig.find_mut(1).kind {
        geometry.layer = 3;
    }
    rig.engine.model_changed();
    assert!(picture_ramps(&rig.tick(500)).is_empty());
    assert_eq!(rig.state(2), CueState::Running);
    if let CueKind::Video { geometry, .. } = &mut rig.find_mut(1).kind {
        geometry.opacity = 0.5;
    }
    rig.engine.model_changed();
    let edit = rig.tick(1000);
    assert_eq!(
        picture_ramps(&edit),
        vec![(
            id(1),
            PictureParam::Opacity,
            1000,
            0.5,
            10,
            FadeCurve::Linear
        )]
    );
    // The fade is over at once and does not stop its target.
    assert_eq!(rig.state(2), CueState::Idle);
    assert!(voice_stops(&rig.tick(4000)).is_empty());
    assert_eq!(rig.state(1), CueState::Running);
    assert_eq!(rig.level(1, OPACITY, 4000), Some(0.5), "the model's value");
    // A later edit of it reaches the picture the same way.
    if let CueKind::Video { geometry, .. } = &mut rig.find_mut(1).kind {
        geometry.opacity = 0.9;
    }
    rig.engine.model_changed();
    assert_eq!(
        picture_ramps(&rig.tick(5000)),
        vec![(
            id(1),
            PictureParam::Opacity,
            5000,
            0.9,
            10,
            FadeCurve::Linear
        )]
    );
}

#[test]
fn a_zero_length_picture_fade_sets_the_value_at_once() {
    let mut rig = rig(vec![
        video_on(1, Some(stage_id(1))),
        opacity_fade(2, 0.0, 0.25),
    ]);
    rig.start(0, 1);
    let go = rig.start(700, 2);
    assert_eq!(
        picture_ramps(&go),
        vec![(
            id(1),
            PictureParam::Opacity,
            700,
            0.25,
            0,
            FadeCurve::Linear
        )]
    );
    assert_eq!(rig.level(1, OPACITY, 700), Some(0.25));
}

#[test]
fn volume_limits_never_touch_a_picture_value() {
    let fade = picture_fade(2, 1, 1.0, FadeCurve::Linear, FadeMode::Absolute, |g| {
        g.fade_scale = true;
        g.scale = Pair { x: 50.0, y: 50.0 };
    });
    let mut rig = rig(vec![video_on(1, Some(stage_id(1))), fade]);
    rig.start(0, 1);
    rig.start(0, 2);
    rig.tick(1000);
    rig.ws.settings.output_max_db = -30.0;
    rig.engine.model_changed();
    assert!(picture_ramps(&rig.tick(1500)).is_empty());
    assert_eq!(
        rig.level(1, LevelParam::Picture(PictureParam::ScaleX), 1500),
        Some(50.0)
    );
}

#[test]
fn a_group_fade_moves_no_picture() {
    let fade = picture_fade(3, 2, 1.0, FadeCurve::Linear, FadeMode::Absolute, |g| {
        g.fade_opacity = true;
        g.opacity = 0.0;
    });
    let mut rig = rig(vec![
        cue(2, group(vec![video_on(1, Some(stage_id(1)))])),
        fade,
    ]);
    rig.start(0, 2);
    assert!(picture_ramps(&rig.start(10, 3)).is_empty());
}

#[test]
fn picture_values_alone_are_nothing_to_fade_for_a_target_without_a_picture() {
    let fade = picture_fade(3, 1, 1.0, FadeCurve::Linear, FadeMode::Absolute, |g| {
        g.fade_opacity = true;
        g.opacity = 0.0;
    });
    let mut rig = rig(vec![video_on(1, Some(stage_id(1))), cue(2, audio()), fade]);
    assert!(rig.problems(3).is_empty());
    let retarget = |rig: &mut Rig, to: u128, now| {
        if let CueKind::Fade { target, .. } = &mut rig.find_mut(3).kind {
            *target = Some(id(to));
        }
        rig.engine.model_changed();
        rig.tick(now);
    };
    retarget(&mut rig, 2, 1);
    assert_eq!(rig.problems(3), vec![CueProblem::NothingToFade]);
    assert_eq!(rig.state(3), CueState::Broken);
    retarget(&mut rig, 1, 2);
    assert!(rig.problems(3).is_empty());
    // The target itself made into an Audio cue: the same.
    rig.find_mut(1).kind = audio();
    rig.engine.model_changed();
    rig.tick(3);
    assert_eq!(rig.problems(3), vec![CueProblem::NothingToFade]);
    // A Group target is moved by levels only.
    rig.find_mut(1).kind = group(vec![]);
    rig.engine.model_changed();
    rig.tick(4);
    assert_eq!(rig.problems(3), vec![CueProblem::NothingToFade]);
}

fn seek(rig: &mut Rig, now: SampleTime, n: u128, seconds: f64) -> Vec<Action> {
    rig.at(
        now,
        Input::Seek {
            cue: id(n),
            position: seconds,
        },
    )
}

#[test]
fn an_idle_video_cue_seeked_is_loaded_at_the_point_past_its_pre_wait_and_go_starts_there() {
    let mut cue = video_on(1, Some(stage_id(1)));
    cue.pre_wait = 2.0;
    let mut rig = rig(vec![cue]);
    let loaded = seek(&mut rig, 0, 1, 1.5);
    assert!(
        loaded
            .iter()
            .any(|a| matches!(a, Action::LoadVoice { offset: 1500, .. })),
        "{loaded:?}"
    );
    assert_eq!(rig.state(1), CueState::Loaded);
    // The point is where the picture starts: the pre-wait is over.
    let go = rig.go(100);
    assert!(
        go.iter().any(|a| matches!(
            a,
            Action::StartVoice {
                at: 100,
                offset: 1500,
                ..
            }
        )),
        "{go:?}"
    );
    // A second seek of the loaded cue loads it at the new point.
    rig.stop(150, 1);
    seek(&mut rig, 200, 1, 0.5);
    let again = seek(&mut rig, 300, 1, 0.75);
    assert!(
        again
            .iter()
            .any(|a| matches!(a, Action::LoadVoice { offset: 750, .. })),
        "{again:?}"
    );
    assert_eq!(rig.state(1), CueState::Loaded);
}

#[test]
fn a_playing_video_cue_seeked_moves_to_the_point_and_ends_where_its_length_says() {
    let mut rig = rig(vec![video_on(1, Some(stage_id(1)))]);
    rig.go(0);
    let old = rig.run(1);
    length(&mut rig, 0, 1, 4000);
    assert_eq!(rig.engine.next_deadline(), Some(4000));
    let moved = seek(&mut rig, 1000, 1, 3.0);
    let fresh = rig.run(1);
    assert_ne!(fresh, old);
    assert!(
        moved.iter().any(|a| matches!(
            a,
            Action::SeekVoice {
                from,
                run,
                at: 1000,
                position: 3000,
                ..
            } if *from == old && *run == fresh
        )),
        "{moved:?}"
    );
    assert_eq!(rig.state(1), CueState::Running, "no stop, no restart");
    // The picture says its whole length again (offset included): the end is 1 s from the move.
    length(&mut rig, 1010, 1, 4000);
    assert_eq!(rig.engine.next_deadline(), Some(2000));
    // The run it moved from is no one's.
    let stale = rig.at(
        1500,
        Input::ActionEnded {
            cue: id(1),
            run: old,
            at: 1400,
        },
    );
    assert!(changes(&stale, 1).is_empty(), "{stale:?}");
    // Paused halfway, then seeked: the cue stays paused and the resume plays from the new point.
    let paused = rig.pause(1500, 1);
    assert!(
        paused
            .iter()
            .any(|a| matches!(a, Action::PauseVoice { offset: 3500, .. }))
    );
    let before = rig.run(1);
    let moved = seek(&mut rig, 1600, 1, 1.0);
    assert!(
        moved.iter().any(|a| matches!(
            a,
            Action::SeekVoice { from, position: 1000, .. } if *from == before
        )),
        "{moved:?}"
    );
    assert_eq!(rig.state(1), CueState::Paused);
    let resumed = rig.resume(2000, 1);
    assert!(
        resumed
            .iter()
            .any(|a| matches!(a, Action::ResumeVoice { offset: 1000, .. })),
        "{resumed:?}"
    );
}

#[test]
fn a_video_cue_seeked_in_its_pre_wait_starts_its_action_at_the_point_now() {
    let mut cue = video_on(1, Some(stage_id(1)));
    cue.pre_wait = 5.0;
    let mut rig = rig(vec![cue]);
    rig.go(0);
    assert_eq!(rig.state(1), CueState::PreWaiting);
    let moved = seek(&mut rig, 1000, 1, 2.0);
    let started = rig.tick(1000);
    let all: Vec<&Action> = moved.iter().chain(started.iter()).collect();
    assert!(
        all.iter().any(|a| matches!(
            a,
            Action::StartVoice {
                at: 1000,
                offset: 2000,
                ..
            }
        )),
        "{all:?}"
    );
    assert_eq!(rig.state(1), CueState::Running);
}
