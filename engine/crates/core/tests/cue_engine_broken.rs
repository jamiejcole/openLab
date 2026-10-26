// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Static broken-cue detection (M2.8: CUE-06, NFR-07), per docs/cue-engine.md "Broken cues".
//! Virtual sample clock at 1 kHz: one second is 1000 samples.

mod common;

use common::*;
use openlab_core::*;

const RATE: u32 = 1000;

use ContinueMode::{AutoContinue as AC, AutoFollow as AF};

fn id(n: u128) -> CueId {
    cue_id(n)
}

fn memo(n: u128) -> Cue {
    cue(n, CueKind::Memo)
}

fn sound(n: u128) -> Cue {
    cue(n, audio())
}

fn sound_of(n: u128, file: &str, patch: u128) -> Cue {
    let mut c = sound(n);
    if let CueKind::Audio {
        media, patch: p, ..
    } = &mut c.kind
    {
        *media = Some(MediaRef::new(file));
        *p = patch_id(patch);
    }
    c
}

fn timed(mut c: Cue, pre: f64, post: f64, mode: ContinueMode) -> Cue {
    c.pre_wait = pre;
    c.post_wait = post;
    c.continue_mode = mode;
    c
}

fn start_cue(n: u128, target: Option<u128>) -> Cue {
    cue(
        n,
        CueKind::Start {
            target: target.map(id),
        },
    )
}

fn media() -> MediaRef {
    MediaRef::new("media/a.wav")
}

struct Rig {
    ws: Workspace,
    engine: CueEngine,
    /// The workspace as the engine last saw it (see `handle_as_binary`).
    last: Option<Workspace>,
}

impl Rig {
    fn new(cues: Vec<Cue>) -> Self {
        let mut rig = Self {
            ws: workspace(cues),
            engine: CueEngine::new(EngineConfig::new(RATE, 0, 1).unwrap()),
            last: None,
        };
        rig.tick(0);
        rig
    }

    fn at(&mut self, now: SampleTime, input: Input) -> Vec<Action> {
        handle_as_binary(&mut self.engine, &self.ws, &mut self.last, input, now)
    }

    fn tick(&mut self, now: SampleTime) -> Vec<Action> {
        self.at(now, Input::Tick)
    }

    fn go(&mut self, now: SampleTime) -> Vec<Action> {
        self.at(now, Input::Go { list: list_id(1) })
    }

    fn start(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::Start { cue: id(n) })
    }

    fn media(&mut self, now: SampleTime, file: &str, status: MediaStatus) -> Vec<Action> {
        self.at(
            now,
            Input::MediaStatus {
                media: MediaRef::new(file),
                status,
            },
        )
    }

    fn device(&mut self, now: SampleTime, patch: u128, resolution: PatchResolution) -> Vec<Action> {
        self.at(
            now,
            Input::PatchStatus {
                patch: patch_id(patch),
                resolution,
            },
        )
    }

    fn state(&self, n: u128) -> CueState {
        self.engine.state(id(n))
    }

    fn problems(&self, n: u128) -> Vec<CueProblem> {
        self.engine.problems(id(n)).to_vec()
    }

    fn find_mut(&mut self, n: u128) -> &mut Cue {
        fn find(cues: &mut [Cue], id: CueId) -> Option<&mut Cue> {
            for c in cues {
                if c.id == id {
                    return Some(c);
                }
                if let CueKind::Group { children, .. } = &mut c.kind
                    && let Some(found) = find(children, id)
                {
                    return Some(found);
                }
            }
            None
        }
        find(&mut self.ws.cue_lists[0].cues, id(n)).unwrap()
    }
}

fn changes(actions: &[Action], n: u128) -> Vec<(CueState, SampleTime)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::StateChanged { cue, state, at } if *cue == id(n) => Some((*state, *at)),
            _ => None,
        })
        .collect()
}

fn problem_reports(actions: &[Action]) -> Vec<(CueId, Vec<CueProblem>)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::ProblemsChanged { cue, problems } => Some((*cue, problems.clone())),
            _ => None,
        })
        .collect()
}

fn starts(actions: &[Action]) -> Vec<(CueId, SampleTime)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::StateChanged {
                cue,
                state: CueState::PreWaiting,
                at,
            } => Some((*cue, *at)),
            _ => None,
        })
        .collect()
}

fn voice_starts(actions: &[Action]) -> Vec<(CueId, SampleTime)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::StartVoice { cue, at, .. } => Some((*cue, *at)),
            _ => None,
        })
        .collect()
}

fn warnings(actions: &[Action]) -> Vec<EngineWarning> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Warning(w) => Some(w.clone()),
            _ => None,
        })
        .collect()
}

// ----- media -----

#[test]
fn missing_media_breaks_every_cue_using_it_until_it_is_available() {
    let mut rig = Rig::new(vec![sound(1), sound(2), sound_of(3, "media/b.wav", 1)]);
    let missing = rig.media(100, "media/a.wav", MediaStatus::Missing);
    let problem = vec![CueProblem::MediaMissing { media: media() }];
    assert_eq!(
        problem_reports(&missing),
        vec![(id(1), problem.clone()), (id(2), problem.clone())]
    );
    assert_eq!(changes(&missing, 1), vec![(CueState::Broken, 100)]);
    assert_eq!(changes(&missing, 2), vec![(CueState::Broken, 100)]);
    assert_eq!(rig.state(3), CueState::Idle);
    assert_eq!(rig.problems(1), problem);
    assert_eq!(problem[0].to_string(), "media file media/a.wav not found");

    let back = rig.media(200, "media/a.wav", MediaStatus::Available);
    assert_eq!(
        problem_reports(&back),
        vec![(id(1), vec![]), (id(2), vec![])]
    );
    assert_eq!(changes(&back, 1), vec![(CueState::Idle, 200)]);
    assert_eq!(rig.state(2), CueState::Idle);
    assert_eq!(voice_starts(&rig.start(300, 1)), vec![(id(1), 300)]);
}

#[test]
fn unsupported_media_is_broken_with_its_reason() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.media(
        0,
        "media/a.wav",
        MediaStatus::Unsupported {
            reason: "unknown codec".into(),
        },
    );
    assert_eq!(
        rig.problems(1),
        vec![CueProblem::MediaUnsupported {
            media: media(),
            reason: "unknown codec".into()
        }]
    );
    assert_eq!(rig.state(1), CueState::Broken);
    // Unknown forgets what was reported: the engine assumes the file plays.
    let forget = rig.media(10, "media/a.wav", MediaStatus::Unknown);
    assert_eq!(changes(&forget, 1), vec![(CueState::Idle, 10)]);
}

#[test]
fn damaged_but_playable_media_is_not_broken() {
    // The binary reports a file that probed with damage warnings as available (M3-B decision).
    let mut rig = Rig::new(vec![sound(1)]);
    let report = rig.media(0, "media/a.wav", MediaStatus::Available);
    assert!(report.is_empty());
    assert_eq!(rig.state(1), CueState::Idle);
    assert_eq!(rig.engine.broken_cues().len(), 0);
}

#[test]
fn an_audio_cue_without_media_or_with_a_missing_patch_is_broken() {
    let mut none = sound(1);
    if let CueKind::Audio { media, .. } = &mut none.kind {
        *media = None;
    }
    let rig = Rig::new(vec![none, sound_of(2, "media/a.wav", 9)]);
    assert_eq!(rig.problems(1), vec![CueProblem::NoMedia]);
    assert_eq!(
        rig.problems(2),
        vec![CueProblem::PatchMissing { patch: patch_id(9) }]
    );
    assert_eq!(rig.state(1), CueState::Broken);
}

// ----- devices -----

#[test]
fn a_patch_without_its_device_breaks_the_cues_on_it() {
    let mut ws_cues = vec![sound(1), sound_of(2, "media/a.wav", 2), memo(3)];
    ws_cues.push(sound(4));
    let mut rig = Rig::new(ws_cues);
    rig.ws.patches.push(patch(2, 2));
    rig.tick(0);
    let missing = PatchResolution::DeviceMissing {
        device: "Interface".into(),
    };
    let lost = rig.device(100, 1, missing.clone());
    let problem = vec![CueProblem::DeviceUnavailable {
        patch: patch_id(1),
        resolution: missing,
    }];
    assert_eq!(
        problem_reports(&lost),
        vec![(id(1), problem.clone()), (id(4), problem)]
    );
    assert_eq!(rig.state(2), CueState::Idle);
    assert_eq!(rig.state(3), CueState::Idle);
    // Every resolution but Mapped breaks them.
    for resolution in [
        PatchResolution::Unmapped,
        PatchResolution::ChannelsMissing {
            device: "Interface".into(),
            needed: 4,
            available: 2,
        },
        PatchResolution::OutputCountMismatch {
            mapped: 1,
            outputs: 2,
        },
    ] {
        rig.device(200, 1, resolution);
        assert_eq!(rig.state(1), CueState::Broken);
    }
    let back = rig.device(
        300,
        1,
        PatchResolution::Mapped {
            device: "Interface".into(),
            channels: vec![0, 1],
        },
    );
    assert_eq!(changes(&back, 1), vec![(CueState::Idle, 300)]);
    assert_eq!(changes(&back, 4), vec![(CueState::Idle, 300)]);
}

#[test]
fn a_cue_can_have_several_problems() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.media(0, "media/a.wav", MediaStatus::Missing);
    let missing = PatchResolution::DeviceMissing {
        device: "Interface".into(),
    };
    let report = rig.device(10, 1, missing.clone());
    let both = vec![
        CueProblem::MediaMissing { media: media() },
        CueProblem::DeviceUnavailable {
            patch: patch_id(1),
            resolution: missing,
        },
    ];
    assert_eq!(problem_reports(&report), vec![(id(1), both.clone())]);
    // Already broken: no new state report.
    assert!(changes(&report, 1).is_empty());
    assert_eq!(rig.problems(1), both);
    assert_eq!(
        both[1].to_string(),
        format!(
            "audio device \"Interface\" of patch {} is not present",
            patch_id(1)
        )
    );
    // One fixed: still broken with the other.
    let fixed = rig.media(20, "media/a.wav", MediaStatus::Available);
    assert_eq!(
        problem_reports(&fixed),
        vec![(id(1), vec![both[1].clone()])]
    );
    assert_eq!(rig.state(1), CueState::Broken);
}

// ----- targets -----

#[test]
fn invalid_self_and_wrong_kind_targets_are_broken() {
    let rig = Rig::new(vec![
        memo(1),
        start_cue(2, None),
        start_cue(3, Some(99)),
        cue(
            4,
            CueKind::Stop {
                target: Some(id(4)),
                fade_out: no_fade(),
            },
        ),
        cue(5, fade(1)),
        cue(
            6,
            CueKind::Devamp {
                target: Some(id(1)),
                start_next_cue: false,
                stop_target_when_slice_ends: false,
            },
        ),
        cue(7, group(vec![sound(8)])),
        cue(9, fade(7)),
        start_cue(10, Some(8)),
        cue(
            11,
            CueKind::Network {
                destination: net_id(5),
                address: "/go".into(),
                args: vec![],
            },
        ),
    ]);
    assert_eq!(rig.problems(2), vec![CueProblem::NoTarget]);
    assert_eq!(
        rig.problems(3),
        vec![CueProblem::TargetMissing { target: id(99) }]
    );
    assert_eq!(rig.problems(4), vec![CueProblem::TargetSelf]);
    assert_eq!(
        rig.problems(5),
        vec![CueProblem::TargetWrongKind { target: id(1) }]
    );
    assert_eq!(
        rig.problems(6),
        vec![CueProblem::TargetWrongKind { target: id(1) }]
    );
    assert_eq!(
        rig.problems(11),
        vec![CueProblem::NetworkPatchMissing { patch: net_id(5) }]
    );
    // A Fade of a group and a Start of a cue inside a group are fine.
    assert!(rig.problems(9).is_empty());
    assert!(rig.problems(10).is_empty());
    let listed: Vec<CueId> = rig
        .engine
        .broken_cues()
        .into_iter()
        .map(|(c, _)| c)
        .collect();
    assert_eq!(listed, vec![id(2), id(3), id(4), id(5), id(6), id(11)]);
}

#[test]
fn model_edits_introduce_and_fix_breakage() {
    let mut rig = Rig::new(vec![memo(1), start_cue(2, Some(1)), memo(3)]);
    assert_eq!(rig.state(2), CueState::Idle);
    // Deleting the target breaks the Start cue at the next call.
    rig.ws.cue_lists[0].cues.remove(0);
    let edit = rig.tick(100);
    assert_eq!(
        problem_reports(&edit),
        vec![(id(2), vec![CueProblem::TargetMissing { target: id(1) }])]
    );
    assert_eq!(changes(&edit, 2), vec![(CueState::Broken, 100)]);
    // Retargeting fixes it.
    rig.find_mut(2).kind.set_target(Some(id(3)));
    let fix = rig.tick(200);
    assert_eq!(problem_reports(&fix), vec![(id(2), vec![])]);
    assert_eq!(changes(&fix, 2), vec![(CueState::Idle, 200)]);
    // Deleting a broken cue: it reports idle once, and no longer has problems.
    rig.find_mut(2).kind.set_target(None);
    rig.tick(300);
    rig.ws.cue_lists[0].cues.remove(0);
    let gone = rig.tick(400);
    assert_eq!(changes(&gone, 2), vec![(CueState::Idle, 400)]);
    assert!(problem_reports(&gone).is_empty());
    assert_eq!(rig.engine.broken_cues().len(), 0);
}

// ----- triggering a broken cue -----

#[test]
fn go_on_a_broken_cue_does_not_start_it_but_keeps_its_auto_continue() {
    let mut rig = Rig::new(vec![
        timed(start_cue(1, None), 1.0, 0.5, AC),
        sound(2),
        memo(3),
    ]);
    let go = rig.go(0);
    assert_eq!(warnings(&go), vec![EngineWarning::CueBroken { cue: id(1) }]);
    assert!(starts(&go).is_empty());
    // The playhead moves past its continue chain, as for any cue.
    assert_eq!(rig.engine.playhead(list_id(1)), Some(id(3)));
    assert_eq!(rig.state(1), CueState::Broken);
    // Pre-wait 1 s, then post-wait 0.5 s from the (skipped) action start.
    assert!(starts(&rig.tick(1499)).is_empty());
    let tick = rig.tick(1500);
    assert_eq!(starts(&tick), vec![(id(2), 1500)]);
    assert_eq!(voice_starts(&tick), vec![(id(2), 1500)]);
    assert!(changes(&tick, 1).is_empty(), "still broken");
    assert_eq!(
        rig.engine.cue_states().collect::<Vec<_>>(),
        vec![(id(1), CueState::Broken), (id(2), CueState::Running)]
    );
}

#[test]
fn a_broken_cue_never_auto_follows() {
    let mut rig = Rig::new(vec![timed(start_cue(1, None), 0.0, 0.0, AF), memo(2)]);
    let go = rig.go(0);
    assert!(starts(&go).is_empty());
    assert_eq!(rig.state(2), CueState::Idle);
    assert!(rig.engine.next_deadline().is_none());
}

#[test]
fn stop_of_a_broken_cue_cancels_its_pending_continue() {
    let mut rig = Rig::new(vec![timed(start_cue(1, None), 0.0, 1.0, AC), memo(2)]);
    rig.go(0);
    let stop = rig.at(
        100,
        Input::Stop {
            cue: id(1),
            fade_out: None,
        },
    );
    assert!(changes(&stop, 1).is_empty());
    assert!(starts(&rig.tick(1000)).is_empty());
    assert!(rig.engine.next_deadline().is_none());
}

#[test]
fn load_and_reset_of_a_broken_cue() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.media(0, "media/a.wav", MediaStatus::Missing);
    let load = rig.at(
        10,
        Input::Load {
            cue: id(1),
            time: 0.0,
        },
    );
    assert_eq!(
        warnings(&load),
        vec![EngineWarning::CueBroken { cue: id(1) }]
    );
    assert!(load.iter().all(|a| !matches!(a, Action::LoadVoice { .. })));
    // Reset does not clear a static problem.
    assert!(rig.at(20, Input::Reset { cue: id(1) }).is_empty());
    assert_eq!(rig.state(1), CueState::Broken);
}

#[test]
fn a_loaded_cue_that_breaks_is_unloaded() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.at(
        0,
        Input::Load {
            cue: id(1),
            time: 0.0,
        },
    );
    let run = rig.engine.run_of(id(1)).unwrap();
    let missing = rig.media(10, "media/a.wav", MediaStatus::Missing);
    assert!(missing.contains(&Action::UnloadVoice { cue: id(1), run }));
    assert_eq!(changes(&missing, 1), vec![(CueState::Broken, 10)]);
    assert_eq!(rig.engine.run_of(id(1)), None);
}

#[test]
fn a_running_cue_that_breaks_plays_on_and_is_broken_when_it_ends() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.start(0, 1);
    let missing = rig.media(10, "media/a.wav", MediaStatus::Missing);
    assert!(changes(&missing, 1).is_empty());
    assert_eq!(rig.state(1), CueState::Running);
    let run = rig.engine.run_of(id(1)).unwrap();
    let end = rig.at(
        500,
        Input::ActionEnded {
            cue: id(1),
            run,
            at: 490,
        },
    );
    assert_eq!(changes(&end, 1), vec![(CueState::Broken, 490)]);
}

#[test]
fn a_cue_that_breaks_during_its_pre_wait_skips_its_action() {
    let mut rig = Rig::new(vec![timed(sound(1), 1.0, 0.5, AC), memo(2)]);
    rig.go(0);
    rig.media(100, "media/a.wav", MediaStatus::Missing);
    assert_eq!(rig.state(1), CueState::PreWaiting);
    let tick = rig.tick(1000);
    assert!(voice_starts(&tick).is_empty());
    assert_eq!(changes(&tick, 1), vec![(CueState::Broken, 1000)]);
    // Fixed before its continue: it stays a skipped run, and the continue still fires. While
    // it reports broken it has a reason to show (review L3).
    let fixed = rig.media(1200, "media/a.wav", MediaStatus::Available);
    assert_eq!(rig.state(1), CueState::Broken);
    assert_eq!(rig.problems(1), vec![CueProblem::StartedBroken]);
    assert_eq!(
        problem_reports(&fixed),
        vec![(id(1), vec![CueProblem::StartedBroken])]
    );
    assert_eq!(rig.engine.broken_cues().len(), 1);
    let next = rig.tick(1500);
    assert_eq!(starts(&next), vec![(id(2), 1500)]);
    assert_eq!(changes(&next, 1), vec![(CueState::Idle, 1500)]);
    assert_eq!(problem_reports(&next), vec![(id(1), vec![])]);
    assert!(rig.problems(1).is_empty());
    // Broken again while that run is pending: its real problems are shown.
    rig.media(2000, "media/a.wav", MediaStatus::Missing);
    rig.start(2010, 1);
    rig.media(2100, "media/a.wav", MediaStatus::Available);
    assert_eq!(rig.problems(1), vec![CueProblem::StartedBroken]);
    let again = rig.media(2200, "media/a.wav", MediaStatus::Missing);
    assert_eq!(
        problem_reports(&again),
        vec![(id(1), vec![CueProblem::MediaMissing { media: media() }])]
    );
}

#[test]
fn a_group_with_a_broken_child_is_not_broken_but_does_not_follow() {
    let mut rig = Rig::new(vec![
        timed(
            cue(1, group(vec![sound(2), start_cue(3, None)])),
            0.0,
            0.0,
            AF,
        ),
        memo(4),
    ]);
    assert!(rig.problems(1).is_empty());
    assert_eq!(rig.state(1), CueState::Idle);
    // Not broken, but it shows a warning (QLab 5).
    assert!(rig.engine.has_broken_descendant(id(1)));
    let go = rig.go(0);
    assert_eq!(warnings(&go), vec![EngineWarning::CueBroken { cue: id(3) }]);
    assert_eq!(voice_starts(&go), vec![(id(2), 0)]);
    let run = rig.engine.run_of(id(2)).unwrap();
    let end = rig.at(
        1000,
        Input::ActionEnded {
            cue: id(2),
            run,
            at: 1000,
        },
    );
    // The group completes, but a child of its run was broken: no auto-follow (as for a child
    // that broke while it ran).
    assert_eq!(changes(&end, 1), vec![(CueState::Idle, 1000)]);
    assert!(starts(&end).is_empty());
}

fn group_warnings(actions: &[Action]) -> Vec<(CueId, bool)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::GroupWarningChanged { group, warning } => Some((*group, *warning)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_group_shows_a_warning_while_a_descendant_is_broken() {
    // 1 { 2 { 3 (audio a.wav), 4 (audio b.wav) }, 5 }, 6 { 7 }
    let inner = cue(2, group(vec![sound(3), sound_of(4, "media/b.wav", 1)]));
    let mut rig = Rig::new(vec![
        cue(1, group(vec![inner, memo(5)])),
        cue(6, group(vec![memo(7)])),
    ]);
    let warned = |rig: &Rig| [1, 2, 6].map(|g| rig.engine.has_broken_descendant(id(g)));
    assert_eq!(warned(&rig), [false; 3]);
    // The first broken descendant puts a warning on every group around it, each reported once.
    let missing = rig.media(10, "media/a.wav", MediaStatus::Missing);
    assert_eq!(group_warnings(&missing), vec![(id(2), true), (id(1), true)]);
    assert_eq!(warned(&rig), [true, true, false]);
    // The groups are not broken.
    assert!(rig.problems(1).is_empty() && rig.problems(2).is_empty());
    assert_eq!(rig.state(1), CueState::Idle);
    assert_eq!(
        rig.engine.broken_cues(),
        vec![(id(3), &[CueProblem::MediaMissing { media: media() }][..])]
    );
    // A second one changes nothing shown; fixing one of two leaves the warning.
    let second = rig.media(20, "media/b.wav", MediaStatus::Missing);
    assert!(group_warnings(&second).is_empty());
    let one_fixed = rig.media(30, "media/a.wav", MediaStatus::Available);
    assert!(group_warnings(&one_fixed).is_empty());
    assert_eq!(warned(&rig), [true, true, false]);
    let all_fixed = rig.media(40, "media/b.wav", MediaStatus::Available);
    assert_eq!(
        group_warnings(&all_fixed),
        vec![(id(2), false), (id(1), false)]
    );
    assert_eq!(warned(&rig), [false; 3]);
    // Not a group, or not a cue: no warning.
    assert!(!rig.engine.has_broken_descendant(id(3)));
    assert!(!rig.engine.has_broken_descendant(id(99)));
}

#[test]
fn a_group_warning_follows_model_edits() {
    let mut rig = Rig::new(vec![
        cue(1, group(vec![start_cue(2, None), memo(3)])),
        cue(4, group(vec![memo(5)])),
    ]);
    assert!(rig.engine.has_broken_descendant(id(1)));
    // The broken cue moves to the other group.
    let moved = {
        let CueKind::Group { children, .. } = &mut rig.find_mut(1).kind else {
            panic!("not a group")
        };
        children.remove(0)
    };
    let CueKind::Group { children, .. } = &mut rig.find_mut(4).kind else {
        panic!("not a group")
    };
    children.push(moved);
    let edit = rig.tick(10);
    assert_eq!(group_warnings(&edit), vec![(id(1), false), (id(4), true)]);
    // Fixed by an edit (a target set).
    if let CueKind::Start { target } = &mut rig.find_mut(2).kind {
        *target = Some(id(3));
    }
    let fixed = rig.tick(20);
    assert_eq!(group_warnings(&fixed), vec![(id(4), false)]);
    // Broken again, then its group deleted: the warning is reported gone once.
    if let CueKind::Start { target } = &mut rig.find_mut(2).kind {
        *target = None;
    }
    assert_eq!(group_warnings(&rig.tick(30)), vec![(id(4), true)]);
    rig.ws.cue_lists[0].cues.retain(|c| c.id != id(4));
    assert_eq!(group_warnings(&rig.tick(40)), vec![(id(4), false)]);
    assert!(!rig.engine.has_broken_descendant(id(4)));
    assert!(group_warnings(&rig.tick(50)).is_empty());
}

#[test]
fn a_group_warning_lasts_while_a_child_shows_started_broken() {
    let mut rig = Rig::new(vec![cue(
        1,
        group(vec![timed(sound(2), 1.0, 0.5, AC), memo(3)]),
    )]);
    rig.media(0, "media/a.wav", MediaStatus::Missing);
    rig.start(10, 2);
    // Fixed while its skipped run is pending: it still shows started_broken, so the warning stays.
    let fixed = rig.media(100, "media/a.wav", MediaStatus::Available);
    assert!(group_warnings(&fixed).is_empty());
    assert!(rig.engine.has_broken_descendant(id(1)));
    // The run ends (pre-wait 1 s, post-wait 0.5 s): nothing left to show.
    let end = rig.tick(1510);
    assert_eq!(problem_reports(&end), vec![(id(2), vec![])]);
    assert_eq!(group_warnings(&end), vec![(id(1), false)]);
    assert!(!rig.engine.has_broken_descendant(id(1)));
}

#[test]
fn a_status_input_updates_group_warnings_without_a_rebuild() {
    // A deep tree: a status input still costs only the cues on that file.
    let mut nested = sound(1000);
    for g in (1..=50).rev() {
        nested = cue(g, group(vec![nested, memo(500 + g)]));
    }
    let mut rig = Rig::new(vec![nested]);
    let before = rig.engine.problem_checks();
    let missing = rig.media(10, "media/a.wav", MediaStatus::Missing);
    assert_eq!(rig.engine.problem_checks(), before + 1);
    assert_eq!(group_warnings(&missing).len(), 50);
    assert!((1..=50).all(|g| rig.engine.has_broken_descendant(id(g))));
    rig.media(20, "media/a.wav", MediaStatus::Available);
    assert!((1..=50).all(|g| !rig.engine.has_broken_descendant(id(g))));
}

#[test]
fn a_disarmed_broken_cue_is_still_broken() {
    let mut c = start_cue(1, None);
    c.armed = false;
    let rig = Rig::new(vec![c]);
    assert_eq!(rig.state(1), CueState::Broken);
}

#[test]
fn the_first_call_reports_every_broken_cue() {
    let mut engine = CueEngine::new(EngineConfig::new(RATE, 0, 1).unwrap());
    let ws = workspace(vec![memo(1), start_cue(2, None)]);
    let first = engine.handle(&ws, Input::Tick, 0);
    assert_eq!(
        problem_reports(&first),
        vec![(id(2), vec![CueProblem::NoTarget])]
    );
    assert_eq!(changes(&first, 2), vec![(CueState::Broken, 0)]);
    // Nothing changed: nothing reported again.
    assert!(problem_reports(&engine.handle(&ws, Input::Tick, 10)).is_empty());
}

#[test]
fn the_pre_show_query_lists_every_broken_cue_with_its_reasons() {
    let mut rig = Rig::new(vec![
        sound(1),
        start_cue(2, None),
        cue(3, group(vec![sound_of(4, "media/b.wav", 1)])),
    ]);
    rig.media(0, "media/b.wav", MediaStatus::Missing);
    let listed: Vec<(CueId, Vec<CueProblem>)> = rig
        .engine
        .broken_cues()
        .into_iter()
        .map(|(c, p)| (c, p.to_vec()))
        .collect();
    assert_eq!(
        listed,
        vec![
            (id(2), vec![CueProblem::NoTarget]),
            (
                id(4),
                vec![CueProblem::MediaMissing {
                    media: MediaRef::new("media/b.wav")
                }]
            ),
        ]
    );
    // A runtime break is not static: it is in `broken_reason`, not the pre-show list.
    rig.start(10, 1);
    let run = rig.engine.run_of(id(1)).unwrap();
    rig.at(
        20,
        Input::ActionFailed {
            cue: id(1),
            run,
            reason: BrokenReason::DeviceLost,
        },
    );
    assert_eq!(
        rig.engine.broken_reason(id(1)),
        Some(BrokenReason::DeviceLost)
    );
    assert_eq!(rig.engine.broken_cues().len(), 2);
}

#[test]
fn a_broken_devamp_cue_uses_its_own_continue_like_a_disarmed_one() {
    // Its "start next cue" can never fire (it sends no devamp), so GO must not skip the next
    // cue for it, and its own auto-continue applies.
    let mut devamp = cue(
        1,
        CueKind::Devamp {
            target: Some(id(3)),
            start_next_cue: true,
            stop_target_when_slice_ends: false,
        },
    );
    devamp.continue_mode = AC;
    let mut rig = Rig::new(vec![devamp, memo(2), memo(3), memo(4)]);
    assert_eq!(
        rig.problems(1),
        vec![CueProblem::TargetWrongKind { target: id(3) }]
    );
    let go = rig.go(0);
    assert_eq!(starts(&go), vec![(id(2), 0)]);
    assert_eq!(rig.engine.playhead(list_id(1)), Some(id(3)));
    // Without its own continue, GO stops right after it.
    let mut devamp = cue(
        1,
        CueKind::Devamp {
            target: Some(id(3)),
            start_next_cue: true,
            stop_target_when_slice_ends: false,
        },
    );
    devamp.continue_mode = ContinueMode::DoNotContinue;
    let mut rig = Rig::new(vec![devamp, memo(2), memo(3)]);
    assert!(starts(&rig.go(0)).is_empty());
    assert_eq!(rig.engine.playhead(list_id(1)), Some(id(2)));
}

// ----- cost (review H1) -----

fn many(n: u128) -> Workspace {
    workspace(
        (0..n)
            .map(|i| sound_of(10 + i, &format!("media/f{i}.wav"), 1))
            .collect(),
    )
}

#[test]
fn problem_work_is_proportional_to_what_changed() {
    // Regression (review H1): every call, and every status input, recomputed every cue.
    let ws = many(2000);
    let mut engine = CueEngine::new(EngineConfig::new(RATE, 5, 1).unwrap());
    engine.handle(&ws, Input::Tick, 0);
    assert_eq!(
        engine.problem_checks(),
        2000,
        "the first call builds them once"
    );
    for t in 1..=100 {
        engine.handle(&ws, Input::Tick, t);
    }
    assert_eq!(engine.problem_checks(), 2000, "idle ticks cost nothing");
    engine.handle(
        &ws,
        Input::MediaStatus {
            media: MediaRef::new("media/f7.wav"),
            status: MediaStatus::Missing,
        },
        200,
    );
    assert_eq!(engine.problem_checks(), 2001, "one status: its cue only");
    assert_eq!(engine.state(id(17)), CueState::Broken);
    // A drive goes away: one batch, each cue once.
    let gone = (0..2000)
        .map(|i| {
            (
                MediaRef::new(format!("media/f{i}.wav")),
                MediaStatus::Missing,
            )
        })
        .collect();
    let batch = engine.handle(
        &ws,
        Input::Availability {
            media: gone,
            patches: vec![],
        },
        300,
    );
    assert_eq!(engine.problem_checks(), 4000, "cue 17 was missing already");
    assert_eq!(problem_reports(&batch).len(), 1999);
    assert_eq!(engine.broken_cues().len(), 2000);
    for t in 301..=400 {
        engine.handle(&ws, Input::Tick, t);
    }
    assert_eq!(
        engine.problem_checks(),
        4000,
        "idle ticks with every cue broken"
    );
    // The device of patch 1 goes away: the cues on patch 1.
    engine.handle(
        &ws,
        Input::PatchStatus {
            patch: patch_id(1),
            resolution: PatchResolution::Unmapped,
        },
        500,
    );
    assert_eq!(engine.problem_checks(), 6000);
    // A repeated, unchanged report costs nothing.
    engine.handle(
        &ws,
        Input::PatchStatus {
            patch: patch_id(1),
            resolution: PatchResolution::Unmapped,
        },
        510,
    );
    assert_eq!(engine.problem_checks(), 6000);
    // A model change: one rebuild on the next call.
    engine.model_changed();
    engine.handle(&ws, Input::Tick, 600);
    engine.handle(&ws, Input::Tick, 601);
    assert_eq!(engine.problem_checks(), 8000);
    assert_eq!(engine.unsignalled_model_changes(), 0);
}

#[test]
fn a_batch_reports_media_and_patches_together() {
    let mut rig = Rig::new(vec![sound(1), sound_of(2, "media/b.wav", 1)]);
    let missing = PatchResolution::DeviceMissing { device: "X".into() };
    let batch = rig.at(
        10,
        Input::Availability {
            media: vec![
                (media(), MediaStatus::Missing),
                (MediaRef::new("media/b.wav"), MediaStatus::Available),
            ],
            patches: vec![(patch_id(1), missing.clone())],
        },
    );
    assert_eq!(
        problem_reports(&batch),
        vec![
            (
                id(1),
                vec![
                    CueProblem::MediaMissing { media: media() },
                    CueProblem::DeviceUnavailable {
                        patch: patch_id(1),
                        resolution: missing.clone()
                    }
                ]
            ),
            (
                id(2),
                vec![CueProblem::DeviceUnavailable {
                    patch: patch_id(1),
                    resolution: missing
                }]
            ),
        ]
    );
}

#[test]
fn model_changes_count_from_the_signal() {
    let mut ws = workspace(vec![memo(1), start_cue(2, Some(1))]);
    let mut engine = CueEngine::new(EngineConfig::new(RATE, 0, 1).unwrap());
    engine.handle(&ws, Input::Tick, 0);
    ws.cue_lists[0].cues.remove(0);
    engine.model_changed();
    let edit = engine.handle(&ws, Input::Tick, 10);
    assert_eq!(changes(&edit, 2), vec![(CueState::Broken, 10)]);
    assert_eq!(engine.unsignalled_model_changes(), 0);
}

/// Debug builds catch (and correct) an edit the binary did not signal, so tests find a broken
/// contract; release builds trust the signal.
#[cfg(debug_assertions)]
#[test]
fn an_unsignalled_model_change_is_caught_in_debug_builds() {
    let mut ws = workspace(vec![memo(1), start_cue(2, Some(1))]);
    let mut engine = CueEngine::new(EngineConfig::new(RATE, 0, 1).unwrap());
    engine.handle(&ws, Input::Tick, 0);
    ws.cue_lists[0].cues.remove(0);
    let edit = engine.handle(&ws, Input::Tick, 10);
    assert_eq!(engine.unsignalled_model_changes(), 1);
    assert_eq!(changes(&edit, 2), vec![(CueState::Broken, 10)]);
}

#[test]
fn the_pre_show_list_is_in_workspace_order() {
    // Review L4: lists in order, cues depth-first, not id order.
    let mut rig = Rig::new(vec![
        start_cue(9, None),
        cue(5, group(vec![start_cue(7, None), memo(1)])),
        start_cue(3, None),
    ]);
    rig.ws.cue_lists.push(CueList {
        id: list_id(2),
        name: "Two".into(),
        cues: vec![start_cue(2, None)],
    });
    rig.tick(10);
    let listed: Vec<CueId> = rig
        .engine
        .broken_cues()
        .into_iter()
        .map(|(c, _)| c)
        .collect();
    assert_eq!(listed, vec![id(9), id(7), id(3), id(2)]);
}

#[test]
fn go_on_a_broken_auto_follow_cue_leaves_the_playhead_on_the_cue_it_would_follow_into() {
    // Regression (review M2): the playhead skipped cue 2, which the broken cue's auto-follow
    // can never start, so cue 2 was lost for the show.
    let mut rig = Rig::new(vec![
        timed(sound(1), 0.0, 0.0, AF),
        sound_of(2, "media/b.wav", 1),
        memo(3),
    ]);
    rig.media(0, "media/a.wav", MediaStatus::Missing);
    let go = rig.go(10);
    assert_eq!(warnings(&go), vec![EngineWarning::CueBroken { cue: id(1) }]);
    assert_eq!(rig.engine.playhead(list_id(1)), Some(id(2)));
    assert_eq!(voice_starts(&rig.go(20)), vec![(id(2), 20)]);
    // Inside a chain: an auto-continue into the broken auto-follow cue stops there too.
    let mut rig = Rig::new(vec![
        timed(memo(1), 0.0, 0.0, AC),
        timed(sound(2), 0.0, 0.0, AF),
        memo(3),
        memo(4),
    ]);
    rig.media(0, "media/a.wav", MediaStatus::Missing);
    rig.go(10);
    assert_eq!(rig.engine.playhead(list_id(1)), Some(id(3)));
    // A broken auto-continue cue's continue fires, so it is still skipped over.
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 0.0, AC), memo(2), memo(3)]);
    rig.media(0, "media/a.wav", MediaStatus::Missing);
    let go = rig.go(10);
    assert_eq!(starts(&go), vec![(id(2), 10)]);
    assert_eq!(rig.engine.playhead(list_id(1)), Some(id(3)));
}
