// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! M2.28: the second batch of QLab 5 checks (answers of 2026-10-27, measured by the user). Where
//! QLab behaved differently, openLab now does what QLab does. One or more regression tests per
//! point, named after the check's id. Virtual sample clock at 1 kHz: one second is 1000 samples.

mod common;

use common::*;
use openlab_core::*;

const RATE: u32 = 1000;

fn id(n: u128) -> CueId {
    cue_id(n)
}

fn list() -> CueListId {
    list_id(1)
}

fn sound(n: u128) -> Cue {
    cue(n, audio())
}

fn memo(n: u128) -> Cue {
    cue(n, CueKind::Memo)
}

fn with(mut c: Cue, option: SecondTrigger) -> Cue {
    c.second_trigger = option;
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

fn hotkey(key: &str) -> Hotkey {
    Hotkey {
        key: key.into(),
        modifiers: Modifiers::default(),
    }
}

fn keyed(mut c: Cue, key: &str) -> Cue {
    c.triggers.hotkey = Some(hotkey(key));
    c
}

/// An audio cue at `master_db`, cue outputs at `out_db`.
fn sound_at(n: u128, master_db: f64, out_db: f64) -> Cue {
    let mut c = sound(n);
    if let CueKind::Audio { levels, .. } = &mut c.kind {
        levels.master_db = master_db;
        levels.outputs = vec![out(out_db), out(out_db)];
    }
    c
}

/// An audio cue whose file is cut in two slices at 1 s, the first looping until devamped, the
/// second playing `last` times (0: looping too).
fn sliced_sound(n: u128, last: u32) -> Cue {
    let mut c = sound(n);
    if let CueKind::Audio {
        slices,
        last_slice_play_count,
        ..
    } = &mut c.kind
    {
        *slices = vec![SliceMarker {
            time: 1.0,
            play_count: 0,
        }];
        *last_slice_play_count = last;
    }
    c
}

/// An audio cue with no slice markers that loops as a whole until devamped.
fn looping_sound(n: u128) -> Cue {
    let mut c = sound(n);
    if let CueKind::Audio {
        last_slice_play_count,
        ..
    } = &mut c.kind
    {
        *last_slice_play_count = 0;
    }
    c
}

fn devamp_cue(n: u128, target: u128, mode: DevampMode, start_next: bool) -> Cue {
    cue(
        n,
        CueKind::Devamp {
            target: Some(id(target)),
            start_next_cue: start_next,
            stop_target_when_slice_ends: false,
            devamp_mode: mode,
        },
    )
}

struct Rig {
    ws: Workspace,
    engine: CueEngine,
    last: Option<Workspace>,
}

impl Rig {
    /// One list; the panic lasts one second.
    fn new(cues: Vec<Cue>) -> Self {
        let mut ws = workspace(cues);
        ws.settings.panic_duration = 1.0;
        let mut rig = Self {
            ws,
            engine: CueEngine::new(EngineConfig::new(RATE, 0, 1).unwrap()),
            last: None,
        };
        rig.at(0, Input::Tick);
        rig
    }

    fn at(&mut self, now: SampleTime, input: Input) -> Vec<Action> {
        handle_as_binary(&mut self.engine, &self.ws, &mut self.last, input, now)
    }

    fn start(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::Start { cue: id(n) })
    }

    fn pause(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::Pause { cue: id(n) })
    }

    fn release(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::TriggerReleased { cue: id(n) })
    }

    fn preview(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::Preview { cue: id(n) })
    }

    fn state(&self, n: u128) -> CueState {
        self.engine.state(id(n))
    }

    fn run(&self, n: u128) -> RunId {
        self.engine.run_of(id(n)).unwrap()
    }

    /// Presses each hotkey cue for `key` as the binary does (M4.2 contract).
    fn press(&mut self, now: SampleTime, key: &str) -> Vec<Action> {
        let mut out = Vec::new();
        for cue in self.ws.cues_with_hotkey(&hotkey(key)) {
            out.extend(self.at(now, Input::Start { cue }));
        }
        out
    }
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

fn voice_starts(actions: &[Action]) -> Vec<CueId> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::StartVoice { cue, .. } => Some(*cue),
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

fn resumed(actions: &[Action]) -> Vec<CueId> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::ResumeVoice { cue, .. } => Some(*cue),
            _ => None,
        })
        .collect()
}

fn devamps(actions: &[Action]) -> Vec<CueId> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::DevampVoice { cue, .. } => Some(*cue),
            _ => None,
        })
        .collect()
}

fn ramps(actions: &[Action], n: u128) -> Vec<(LevelParam, f32)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Ramp {
                cue, param, gain, ..
            } if *cue == id(n) => Some((*param, *gain)),
            _ => None,
        })
        .collect()
}

fn preview_starts(actions: &[Action]) -> Vec<(CueId, RunId)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::PreviewStart { cue, run, .. } => Some((*cue, *run)),
            _ => None,
        })
        .collect()
}

fn preview_stops(actions: &[Action]) -> Vec<(CueId, RunId, SampleTime, Option<StopFade>)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::PreviewStop { cue, run, at, fade } => Some((*cue, *run, *at, *fade)),
            _ => None,
        })
        .collect()
}

fn preview_changes(actions: &[Action]) -> Vec<(CueId, bool)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::PreviewChanged { cue, active, .. } => Some((*cue, *active)),
            _ => None,
        })
        .collect()
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-6 * b.abs().max(1e-6)
}

// ----- D7: the two Devamp modes -----

/// "Devamp currently looping slice" leaves the slice loop playing (what openLab always did).
#[test]
fn d7_devamp_current_slice_leaves_the_looping_slice() {
    let mut rig = Rig::new(vec![
        sliced_sound(1, 1),
        devamp_cue(2, 1, DevampMode::CurrentSlice, false),
    ]);
    rig.start(0, 1);
    let out = rig.start(500, 2);
    assert_eq!(devamps(&out), vec![id(1)]);
    assert!(warnings(&out).is_empty(), "{out:?}");
}

/// "Devamp looping cue" fired while a slice loops does nothing (QLab: slice 1 keeps looping),
/// and so its "start next cue" never fires.
#[test]
fn d7_devamp_looping_cue_does_nothing_while_a_slice_loops() {
    let mut rig = Rig::new(vec![
        sliced_sound(1, 1),
        devamp_cue(2, 1, DevampMode::LoopingCue, true),
        memo(3),
    ]);
    rig.start(0, 1);
    let run = rig.run(1);
    let out = rig.start(500, 2);
    assert!(devamps(&out).is_empty(), "{out:?}");
    assert_eq!(
        warnings(&out),
        vec![EngineWarning::DevampIgnored { cue: id(1) }]
    );
    assert_eq!(rig.state(1), CueState::Running);
    // Nothing is pending: the target's end starts nothing after the Devamp cue.
    let out = rig.at(
        5000,
        Input::ActionEnded {
            cue: id(1),
            run,
            at: 5000,
        },
    );
    assert_eq!(rig.state(3), CueState::Idle, "{out:?}");
    // A later devamp of the slice is still free to act (none was left pending).
    rig.ws.cue_lists[0]
        .cues
        .push(devamp_cue(4, 1, DevampMode::CurrentSlice, false));
    rig.start(5100, 1);
    let out = rig.start(5200, 4);
    assert_eq!(devamps(&out), vec![id(1)]);
}

/// Also when the slice after the marker loops: with markers every loop is a slice's.
#[test]
fn d7_devamp_looping_cue_does_nothing_on_any_sliced_cue() {
    let mut rig = Rig::new(vec![
        sliced_sound(1, 0),
        devamp_cue(2, 1, DevampMode::LoopingCue, false),
    ]);
    rig.start(0, 1);
    let out = rig.start(500, 2);
    assert!(devamps(&out).is_empty(), "{out:?}");
}

/// A cue without markers loops as a whole: "devamp looping cue" leaves that loop, and so does
/// "devamp currently looping slice" (its one slice is the whole cue).
#[test]
fn d7_both_modes_leave_the_loop_of_a_cue_without_slices() {
    for mode in [DevampMode::LoopingCue, DevampMode::CurrentSlice] {
        let mut rig = Rig::new(vec![looping_sound(1), devamp_cue(2, 1, mode, false)]);
        rig.start(0, 1);
        let out = rig.start(500, 2);
        assert_eq!(devamps(&out), vec![id(1)], "{mode:?}");
        assert!(warnings(&out).is_empty(), "{mode:?}: {out:?}");
    }
}

/// The mode concerns an audio target's loops; a looping playlist is devamped in both modes.
#[test]
fn d7_both_modes_devamp_a_looping_playlist() {
    for mode in [DevampMode::LoopingCue, DevampMode::CurrentSlice] {
        let mut rig = Rig::new(vec![
            grp(1, playlist(true), vec![sound(11), sound(12)]),
            devamp_cue(2, 1, mode, false),
        ]);
        rig.start(0, 1);
        rig.start(100, 2);
        let run = rig.run(11);
        let out = rig.at(
            500,
            Input::ActionEnded {
                cue: id(11),
                run,
                at: 500,
            },
        );
        assert!(voice_starts(&out).is_empty(), "{mode:?}: {out:?}");
        assert_eq!(rig.state(1), CueState::Idle, "{mode:?}");
    }
}

// ----- F2: a Fade cue targeting a group moves each child relatively -----

#[test]
fn f2_a_fade_of_a_group_moves_each_child_by_the_fades_level() {
    let mut fade_kind = fade(1);
    if let CueKind::Fade {
        duration,
        levels,
        stop_target_when_done,
        ..
    } = &mut fade_kind
    {
        *duration = 3.0;
        *stop_target_when_done = false;
        levels.master_db = Some(-10.0);
        levels.output_db = vec![Some(-6.0)];
        levels.crosspoints = vec![FadeCrosspoint {
            input: 0,
            output: 0,
            db: -3.0,
        }];
    }
    let mut rig = Rig::new(vec![
        grp(
            1,
            GroupMode::Timeline,
            vec![
                sound_at(11, 0.0, 0.0),
                sound_at(12, -20.0, -12.0),
                grp(13, GroupMode::Timeline, vec![sound_at(131, -5.0, 0.0)]),
            ],
        ),
        cue(2, fade_kind),
    ]);
    rig.start(0, 1);
    let out = rig.start(100, 2);
    let gain = |db: f64| db_to_gain(db);
    for (n, master, output) in [(11, -10.0, -6.0), (12, -30.0, -18.0), (131, -15.0, -6.0)] {
        let r = ramps(&out, n);
        // Master and output 0 move; the unlisted (silent) crosspoint stays silent: no ramp.
        assert_eq!(r.len(), 2, "cue {n}: {r:?}");
        assert_eq!(r[0].0, LevelParam::Master);
        assert!(close(r[0].1, gain(master)), "cue {n}: {r:?}");
        assert_eq!(r[1].0, LevelParam::Output(0));
        assert!(close(r[1].1, gain(output)), "cue {n}: {r:?}");
        assert_eq!(rig.state(n), CueState::Fading);
    }
    // They end where they were going.
    rig.at(3100, Input::Tick);
    let level = rig
        .engine
        .fade_level(id(12), LevelParam::Master, 3100)
        .unwrap();
    assert!(close(level, gain(-30.0)), "{level}");
}

/// A relative fade starts from where an earlier fade left the level, never goes above the
/// loudest level a cue may have, and a Fade straight at an audio cue stays absolute.
#[test]
fn f2_relative_from_the_current_level_capped_and_absolute_for_an_audio_target() {
    let fade_by = |n: u128, target: u128, db: f64| {
        let mut k = fade(target);
        if let CueKind::Fade {
            duration,
            levels,
            stop_target_when_done,
            ..
        } = &mut k
        {
            *duration = 0.0;
            *stop_target_when_done = false;
            levels.master_db = Some(db);
        }
        cue(n, k)
    };
    let mut rig = Rig::new(vec![
        grp(1, GroupMode::Timeline, vec![sound_at(11, 0.0, 0.0)]),
        fade_by(2, 11, -20.0),
        fade_by(3, 1, -6.0),
        fade_by(4, 1, 40.0),
    ]);
    rig.start(0, 1);
    // Absolute on the audio cue itself: to -20 dB.
    let out = rig.start(100, 2);
    assert!(close(ramps(&out, 11)[0].1, db_to_gain(-20.0)));
    // Relative on the group: from -20 by -6.
    let out = rig.start(200, 3);
    assert!(close(ramps(&out, 11)[0].1, db_to_gain(-26.0)));
    // +40 from -26 would be +14: held at the maximum.
    let out = rig.start(300, 4);
    assert!(close(ramps(&out, 11)[0].1, db_to_gain(MAX_DB)));
}

// ----- S1: a second trigger on a paused cue resumes it -----

#[test]
fn s1_go_or_a_start_cue_on_a_paused_cue_resumes_it_whatever_its_option() {
    let start_cue = cue(
        3,
        CueKind::Start {
            target: Some(id(1)),
        },
    );
    let mut rig = Rig::new(vec![
        with(sound(1), SecondTrigger::HardStop),
        memo(2),
        start_cue,
    ]);
    rig.start(0, 1);
    rig.pause(100, 1);
    // GO with the playhead on the paused cue.
    rig.at(
        150,
        Input::SetPlayhead {
            list: list(),
            cue: Some(id(1)),
        },
    );
    let out = rig.at(200, Input::Go { list: list() });
    assert_eq!(resumed(&out), vec![id(1)]);
    assert!(voice_stops(&out).is_empty(), "{out:?}");
    assert_eq!(rig.state(1), CueState::Running);
    // Running again: the option applies to a trigger now.
    rig.pause(300, 1);
    let out = rig.start(400, 3);
    assert_eq!(resumed(&out), vec![id(1)]);
    assert_eq!(rig.state(1), CueState::Running);
    let out = rig.start(500, 3);
    assert_eq!(voice_stops(&out).len(), 1, "{out:?}");
    assert_eq!(rig.state(1), CueState::Idle);
}

// ----- S4: playlist "play previous" on the first child stops the group -----

#[test]
fn s4_play_previous_on_the_first_child_stops_the_playlist_and_goes_back_from_a_later_one() {
    let mut group = with(
        grp(1, playlist(false), vec![sound(11), sound(12), sound(13)]),
        SecondTrigger::PlayPrevious,
    );
    group.continue_mode = ContinueMode::AutoFollow;
    let mut rig = Rig::new(vec![group, memo(2)]);
    rig.start(0, 1);
    // On child 2 it goes back to child 1.
    let run = rig.run(11);
    rig.at(
        500,
        Input::ActionEnded {
            cue: id(11),
            run,
            at: 500,
        },
    );
    assert_eq!(rig.state(12), CueState::Running);
    let out = rig.start(600, 1);
    assert_eq!(voice_starts(&out), vec![id(11)]);
    assert_eq!(rig.state(12), CueState::Idle);
    // On child 1 it stops the playlist: nothing plays, no auto-follow.
    let eleven = rig.run(11);
    let out = rig.start(700, 1);
    assert_eq!(voice_stops(&out), vec![(id(11), eleven, 700, None)]);
    assert!(voice_starts(&out).is_empty(), "{out:?}");
    assert_eq!(rig.state(1), CueState::Idle);
    assert_eq!(rig.state(2), CueState::Idle);
    assert_eq!(rig.engine.next_deadline(), None);
}

// ----- S5: hotkey press on a paused cue resumes it; release with "stop" stops it -----

#[test]
fn s5_hotkey_press_resumes_a_paused_cue_and_release_stops_it() {
    let mut c = keyed(with(sound(1), SecondTrigger::Stop), "q");
    c.second_trigger_on_release = true;
    let mut rig = Rig::new(vec![c]);
    // Press starts, release stops (the sampler use of "on release").
    rig.press(0, "q");
    assert_eq!(rig.state(1), CueState::Running);
    rig.release(100, 1);
    assert_eq!(rig.state(1), CueState::Idle);
    // Start, pause, press: it resumes; release: it stops.
    rig.press(200, "q");
    rig.pause(300, 1);
    let out = rig.press(400, "q");
    assert_eq!(resumed(&out), vec![id(1)]);
    assert_eq!(rig.state(1), CueState::Running);
    let run = rig.run(1);
    let out = rig.release(500, 1);
    assert_eq!(voice_stops(&out), vec![(id(1), run, 500, None)]);
    assert_eq!(rig.state(1), CueState::Idle);
}

// ----- V1/V2: a group can be previewed, a disarmed cue cannot -----

#[test]
fn v1_previewing_a_group_plays_every_armed_audio_cue_in_it() {
    let mut disarmed = sound(12);
    disarmed.armed = false;
    let mut disarmed_group = grp(14, GroupMode::Timeline, vec![sound(141)]);
    disarmed_group.armed = false;
    let mut rig = Rig::new(vec![grp(
        1,
        playlist(false),
        vec![
            sound(11),
            disarmed,
            memo(13),
            disarmed_group,
            grp(15, GroupMode::Timeline, vec![sound(151)]),
        ],
    )]);
    let out = rig.preview(100, 1);
    let started: Vec<CueId> = preview_starts(&out).iter().map(|p| p.0).collect();
    assert_eq!(started, vec![id(11), id(151)]);
    assert_eq!(
        preview_changes(&out),
        vec![(id(11), true), (id(151), true), (id(1), true)]
    );
    assert!(rig.engine.is_previewing(id(1)));
    assert_eq!(
        rig.engine.previewing().collect::<Vec<_>>(),
        vec![id(1), id(11), id(151)]
    );
    // Invisible to the show.
    assert_eq!(rig.state(1), CueState::Idle);
    assert_eq!(rig.state(11), CueState::Idle);
    // A second preview of the group is ignored.
    assert_eq!(
        warnings(&rig.preview(150, 1)),
        vec![EngineWarning::PreviewIgnored {
            cue: id(1),
            reason: PreviewIgnoredReason::AlreadyPreviewing
        }]
    );
    // The group's preview ends with its last audio cue.
    let run = rig.engine.preview_run(id(11)).unwrap();
    let out = rig.at(
        500,
        Input::ActionEnded {
            cue: id(11),
            run,
            at: 500,
        },
    );
    assert_eq!(preview_changes(&out), vec![(id(11), false)]);
    assert!(rig.engine.is_previewing(id(1)));
    // StopPreview on the group stops what it still plays.
    let out = rig.at(600, Input::StopPreview { cue: Some(id(1)) });
    assert_eq!(preview_stops(&out).len(), 1);
    assert_eq!(
        preview_changes(&out),
        vec![(id(151), false), (id(1), false)]
    );
    assert!(!rig.engine.is_previewing(id(1)));
    assert_eq!(rig.engine.previewing().count(), 0);
}

#[test]
fn v1_a_group_preview_skips_broken_and_already_previewed_audio_cues() {
    let mut broken = sound(12);
    if let CueKind::Audio { media, .. } = &mut broken.kind {
        *media = None;
    }
    let mut rig = Rig::new(vec![grp(
        1,
        GroupMode::Timeline,
        vec![sound(11), broken, sound(13)],
    )]);
    rig.preview(0, 13);
    let out = rig.preview(100, 1);
    let started: Vec<CueId> = preview_starts(&out).iter().map(|p| p.0).collect();
    assert_eq!(started, vec![id(11)]);
    assert_eq!(
        warnings(&out),
        vec![EngineWarning::CueBroken { cue: id(12) }]
    );
    // 13 is its own preview: stopping the group leaves it.
    rig.at(200, Input::StopPreview { cue: Some(id(1)) });
    assert!(rig.engine.is_previewing(id(13)));
    assert!(!rig.engine.is_previewing(id(1)));
}

#[test]
fn v1_a_group_preview_ends_when_its_group_is_deleted() {
    let mut rig = Rig::new(vec![grp(1, GroupMode::Timeline, vec![sound(11)])]);
    rig.preview(0, 1);
    rig.ws.cue_lists[0].cues.clear();
    let out = rig.at(100, Input::Tick);
    assert_eq!(preview_stops(&out).len(), 1);
    assert_eq!(preview_changes(&out), vec![(id(11), false), (id(1), false)]);
    assert_eq!(rig.engine.previewing().count(), 0);
}

#[test]
fn v2_a_disarmed_cue_or_group_cannot_be_previewed() {
    let mut c = sound(1);
    c.armed = false;
    let mut g = grp(2, GroupMode::Timeline, vec![sound(21)]);
    g.armed = false;
    let mut rig = Rig::new(vec![c, g]);
    for n in [1, 2] {
        assert_eq!(
            rig.preview(0, n),
            vec![Action::Warning(EngineWarning::PreviewIgnored {
                cue: id(n),
                reason: PreviewIgnoredReason::Disarmed
            })]
        );
    }
    assert_eq!(rig.engine.previewing().count(), 0);
}

// ----- V4/V6: Pause All pauses previews; Panic fades them, a second Panic cuts them -----

#[test]
fn v4_pause_all_pauses_previews_and_resume_all_resumes_them() {
    let mut rig = Rig::new(vec![sound(1)]);
    let (_, first) = preview_starts(&rig.preview(100, 1))[0];
    let out = rig.at(400, Input::PauseAll);
    assert_eq!(
        out,
        vec![Action::PauseVoice {
            cue: id(1),
            run: first,
            at: 400,
            offset: 300
        }]
    );
    assert!(rig.engine.is_previewing(id(1)), "paused, still a preview");
    // The end report of carrying out the pause (dated after it) is ignored.
    let out = rig.at(
        410,
        Input::ActionEnded {
            cue: id(1),
            run: first,
            at: 401,
        },
    );
    assert!(out.is_empty(), "{out:?}");
    assert!(rig.engine.is_previewing(id(1)));
    // A second Pause All does nothing more.
    assert!(rig.at(450, Input::PauseAll).is_empty());
    let out = rig.at(600, Input::ResumeAll);
    let fresh = rig.engine.preview_run(id(1)).unwrap();
    assert_ne!(fresh, first);
    assert!(fresh.0 >= PREVIEW_RUN_BASE);
    assert_eq!(
        out,
        vec![Action::ResumeVoice {
            cue: id(1),
            run: fresh,
            paused: first,
            at: 600,
            offset: 300
        }]
    );
    // Paused again, it continues from where it got to.
    let out = rig.at(700, Input::PauseAll);
    assert!(matches!(out[0], Action::PauseVoice { offset: 400, .. }));
    rig.at(800, Input::ResumeAll);
    // The paused segment's id is stale; the fresh one ends the preview.
    let stale = rig.at(
        900,
        Input::ActionEnded {
            cue: id(1),
            run: fresh,
            at: 900,
        },
    );
    assert!(stale.is_empty(), "{stale:?}");
    let now = rig.engine.preview_run(id(1)).unwrap();
    let out = rig.at(
        1000,
        Input::ActionEnded {
            cue: id(1),
            run: now,
            at: 1000,
        },
    );
    assert_eq!(preview_changes(&out), vec![(id(1), false)]);
}

#[test]
fn v4_v6_panic_fades_a_playing_preview_and_a_second_panic_cuts_it() {
    let mut rig = Rig::new(vec![sound(1), sound(2)]);
    let (_, one) = preview_starts(&rig.preview(0, 1))[0];
    let (_, two) = preview_starts(&rig.preview(0, 2))[0];
    let panic = rig.at(300, Input::Panic);
    let fade = Some(StopFade {
        frames: 1000,
        curve: PANIC_CURVE,
    });
    assert_eq!(
        preview_stops(&panic),
        vec![(id(1), one, 300, fade), (id(2), two, 300, fade)]
    );
    assert_eq!(
        preview_changes(&panic),
        vec![(id(1), false), (id(2), false)]
    );
    assert_eq!(rig.engine.previewing().count(), 0);
    // A second Panic during the fade cuts the fading voices at once.
    let second = rig.at(800, Input::Panic);
    assert_eq!(
        preview_stops(&second),
        vec![(id(1), one, 800, None), (id(2), two, 800, None)]
    );
    assert!(
        preview_changes(&second).is_empty(),
        "already reported ended"
    );
    // And a third has nothing left to cut.
    assert!(preview_stops(&rig.at(900, Input::Panic)).is_empty());
}

#[test]
fn v4_a_paused_preview_stops_at_once_on_panic() {
    let mut rig = Rig::new(vec![sound(1)]);
    let (_, run) = preview_starts(&rig.preview(0, 1))[0];
    rig.at(100, Input::PauseAll);
    let panic = rig.at(300, Input::Panic);
    assert_eq!(preview_stops(&panic), vec![(id(1), run, 300, None)]);
    assert_eq!(preview_changes(&panic), vec![(id(1), false)]);
    // Nothing is left for a second Panic to cut.
    assert!(preview_stops(&rig.at(400, Input::Panic)).is_empty());
}

#[test]
fn v6_a_panicked_preview_is_forgotten_after_the_fade_or_its_end() {
    let mut rig = Rig::new(vec![sound(1), sound(2)]);
    let (_, one) = preview_starts(&rig.preview(0, 1))[0];
    rig.preview(0, 2);
    rig.at(100, Input::Panic);
    // Voice 1's end report comes in: it has nothing more to cut.
    let out = rig.at(
        1100,
        Input::ActionEnded {
            cue: id(1),
            run: one,
            at: 1100,
        },
    );
    assert!(out.is_empty(), "{out:?}");
    // A Panic after the fade's end is a new panic: nothing of the old one is cut.
    let out = rig.at(1200, Input::Panic);
    assert!(preview_stops(&out).is_empty(), "{out:?}");
    // Stop All during a panic fade cuts the fading previews too.
    rig.preview(2300, 1);
    let (_, fresh) = (id(1), rig.engine.preview_run(id(1)).unwrap());
    rig.at(2400, Input::Panic);
    let out = rig.at(2500, Input::StopAll);
    assert_eq!(preview_stops(&out), vec![(id(1), fresh, 2500, None)]);
}

// ----- K1b: a cue hotkey the workspace uses is a warning; the workspace action wins -----

#[test]
fn k1b_a_cue_hotkey_used_by_the_workspace_warns_and_never_starts_the_cue() {
    let mut rig = Rig::new(vec![keyed(sound(1), "space"), keyed(sound(2), "w")]);
    let issues = validate(&rig.ws);
    assert_eq!(
        issues
            .iter()
            .map(|i| (i.cue_id, i.kind.clone()))
            .collect::<Vec<_>>(),
        vec![(
            Some(id(1)),
            IssueKind::HotkeyUsedByWorkspace { action: "go" }
        )]
    );
    // A warning: the show still loads and the hotkey is kept.
    let loaded = load_show(&show_text(&rig.ws)).unwrap();
    assert_eq!(loaded.warnings, issues);
    assert_eq!(
        loaded.workspace.find_cue(id(1)).unwrap().triggers.hotkey,
        Some(hotkey("space"))
    );
    // Space is GO's: no cue is started by it.
    assert!(rig.ws.cues_with_hotkey(&hotkey("space")).is_empty());
    assert!(rig.press(0, "space").is_empty());
    assert_eq!(rig.state(1), CueState::Idle);
    assert_eq!(rig.ws.cues_with_hotkey(&hotkey("w")), vec![id(2)]);
    // With other modifiers it is a different key; with GO moved off Space, Space is the cue's.
    let shift_space = Hotkey {
        modifiers: Modifiers {
            shift: true,
            ..Modifiers::default()
        },
        ..hotkey("space")
    };
    assert_eq!(rig.ws.settings.hotkeys.action_of(&shift_space), None);
    rig.ws.settings.hotkeys.go = Some(hotkey("enter"));
    assert!(validate(&rig.ws).is_empty());
    assert_eq!(rig.ws.cues_with_hotkey(&hotkey("space")), vec![id(1)]);
    // Every workspace key counts, the selected-cue keys included (M5.11c).
    for (key, action) in [
        ("[", "pause_all"),
        ("]", "resume_all"),
        ("l", "load_selected"),
        ("p", "pause_resume_selected"),
        ("enter", "go"),
    ] {
        assert_eq!(
            rig.ws.settings.hotkeys.action_of(&hotkey(key)),
            Some(action)
        );
    }
    let next = rig.ws.settings.hotkeys.playhead_next.clone().unwrap();
    assert_eq!(
        rig.ws.settings.hotkeys.action_of(&next),
        Some("playhead_next")
    );
    rig.ws.settings.hotkeys.stop_selected = Some(hotkey("f2"));
    assert_eq!(
        rig.ws.settings.hotkeys.action_of(&hotkey("f2")),
        Some("stop_selected")
    );
}

/// Editing a cue's hotkey to a workspace key is allowed (QLab keeps it, with the warning).
#[test]
fn k1b_setting_a_workspace_key_as_a_cue_hotkey_is_not_refused() {
    let mut ws = workspace(vec![sound(1)]);
    let command = Command::SetCueProperty {
        id: id(1),
        property: CueProperty::Triggers {
            value: Triggers {
                hotkey: Some(hotkey("space")),
                osc: None,
            },
        },
    };
    ws.apply(command).unwrap();
    assert_eq!(
        ws.find_cue(id(1)).unwrap().triggers.hotkey,
        Some(hotkey("space"))
    );
}

/// Shuffled without loop: "play previous" on the pass's first child stops the group too, and on
/// a later child goes back to the one before it in the pass's order.
#[test]
fn s4_play_previous_on_the_first_child_of_a_shuffled_playlist_stops_it() {
    let group = with(
        grp(
            1,
            GroupMode::Playlist {
                loop_: false,
                shuffle: true,
            },
            vec![sound(11), sound(12), sound(13)],
        ),
        SecondTrigger::PlayPrevious,
    );
    let mut rig = Rig::new(vec![group]);
    let playing = |rig: &Rig| {
        [11, 12, 13]
            .into_iter()
            .filter(|n| rig.state(*n) == CueState::Running)
            .collect::<Vec<_>>()
    };
    rig.start(0, 1);
    let first = playing(&rig);
    assert_eq!(first.len(), 1);
    let first = first[0];
    let run = rig.run(first);
    rig.at(
        500,
        Input::ActionEnded {
            cue: id(first),
            run,
            at: 500,
        },
    );
    let second = playing(&rig);
    assert_eq!(second.len(), 1);
    assert_ne!(second[0], first);
    // On the pass's second child it goes back to the first.
    let out = rig.start(600, 1);
    assert_eq!(voice_starts(&out), vec![id(first)]);
    // On the pass's first child it stops the playlist.
    let out = rig.start(700, 1);
    assert_eq!(voice_stops(&out).len(), 1, "{out:?}");
    assert!(voice_starts(&out).is_empty(), "{out:?}");
    assert_eq!(rig.state(1), CueState::Idle);
    assert!(playing(&rig).is_empty());
}

/// A workspace with duplicate ids (a load error, but the engine must stay consistent): an audio
/// cue inside a group with the group's own id is not previewed, so an id is never both a group's
/// preview and an audio preview, and a repeated child id gets one preview, not a lost voice.
#[test]
fn v1_a_group_preview_skips_children_with_the_groups_or_a_repeated_id() {
    let mut rig = Rig::new(vec![grp(
        6,
        GroupMode::Timeline,
        vec![sound(6), sound(11), sound(11), sound(12)],
    )]);
    let out = rig.preview(0, 6);
    let started: Vec<CueId> = preview_starts(&out).iter().map(|p| p.0).collect();
    assert_eq!(started, vec![id(11), id(12)]);
    assert!(rig.engine.is_previewing(id(6)));
    assert_eq!(rig.engine.preview_run(id(6)), None);
    let out = rig.at(100, Input::StopPreview { cue: Some(id(6)) });
    assert_eq!(preview_stops(&out).len(), 2);
    assert_eq!(rig.engine.previewing().count(), 0);
}

/// Duplicate ids again (M2.32, cue_engine_prop seeds 3aa595d4, 6773ea6c, ccfa3e81): an audio
/// child whose id names a cue of another kind first (a Memo before the group, a group beside it)
/// is not previewed. A preview is named by its id, so it would play a cue the workspace says is
/// not audio, and the next pass over the edits would stop it as gone (an unsignalled change).
#[test]
fn v1_a_group_preview_skips_children_whose_id_names_another_cue_first() {
    let mut rig = Rig::new(vec![
        memo(2),
        grp(
            3,
            GroupMode::Timeline,
            vec![
                sound(2),
                grp(4, GroupMode::Timeline, vec![]),
                sound(4),
                sound(5),
            ],
        ),
    ]);
    let out = rig.preview(0, 3);
    let started: Vec<CueId> = preview_starts(&out).iter().map(|p| p.0).collect();
    assert_eq!(started, vec![id(5)]);
    for n in [2, 4, 5] {
        let audio = matches!(
            rig.ws.find_cue(id(n)).map(|c| &c.kind),
            Some(CueKind::Audio { .. })
        );
        assert_eq!(rig.engine.preview_run(id(n)).is_some(), audio, "cue {n}");
    }
    // A later call has nothing to undo (`handle_as_binary` checks no change was unsignalled).
    let out = rig.at(100, Input::Tick);
    assert!(preview_stops(&out).is_empty(), "{out:?}");
    assert!(rig.engine.is_previewing(id(5)));
}

/// With every audio child shadowed that way there is nothing to preview.
#[test]
fn v1_a_group_preview_of_only_shadowed_children_has_nothing_to_preview() {
    let mut rig = Rig::new(vec![memo(2), grp(3, GroupMode::Timeline, vec![sound(2)])]);
    let out = rig.preview(0, 3);
    assert!(preview_starts(&out).is_empty(), "{out:?}");
    assert_eq!(
        warnings(&out),
        vec![EngineWarning::PreviewIgnored {
            cue: id(3),
            reason: PreviewIgnoredReason::NothingToPreview
        }]
    );
    assert!(!rig.engine.is_previewing(id(3)));
}

/// "Devamp looping cue" aimed at a sliced cue that is not running says the target is not
/// running, as the other mode does, before it looks at the slices.
#[test]
fn d7_devamp_looping_cue_on_a_stopped_sliced_cue_reports_not_running() {
    let mut rig = Rig::new(vec![
        sliced_sound(1, 1),
        devamp_cue(2, 1, DevampMode::LoopingCue, false),
    ]);
    let out = rig.start(0, 2);
    assert_eq!(
        warnings(&out),
        vec![EngineWarning::TargetNotRunning {
            cue: id(2),
            target: id(1)
        }]
    );
}
