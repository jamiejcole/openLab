// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Preview / audition (CUE-05), per docs/cue-engine.md "Preview". Virtual sample clock at
//! 1 kHz: one second is 1000 samples.

mod common;

use common::*;
use openlab_core::*;

use ContinueMode::{AutoContinue as AC, AutoFollow as AF};

impl Rig {
    fn preview(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::Preview { cue: id(n) })
    }

    fn stop_preview(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::StopPreview { cue: Some(id(n)) })
    }
}

/// Everything that is not a preview action or a warning: what the show would notice.
fn show_actions(actions: &[Action]) -> Vec<&Action> {
    actions
        .iter()
        .filter(|a| {
            !matches!(
                a,
                Action::PreviewStart { .. }
                    | Action::PreviewStop { .. }
                    | Action::PreviewChanged { .. }
                    | Action::Warning(_)
            )
        })
        .collect()
}

fn preview_start(actions: &[Action]) -> Option<(CueId, RunId, SampleTime)> {
    actions.iter().find_map(|a| match a {
        Action::PreviewStart { cue, run, at } => Some((*cue, *run, *at)),
        _ => None,
    })
}

fn changed(actions: &[Action]) -> Vec<(CueId, bool, SampleTime)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::PreviewChanged { cue, active, at } => Some((*cue, *active, *at)),
            _ => None,
        })
        .collect()
}

fn stops(actions: &[Action]) -> Vec<(CueId, RunId, SampleTime, Option<StopFade>)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::PreviewStop { cue, run, at, fade } => Some((*cue, *run, *at, *fade)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_preview_plays_the_cue_from_its_start_now() {
    let mut rig = Rig::new(vec![timed(sound(1), 2.0, 3.0, AC), sound(2)]);
    let p = rig.preview(500, 1);
    let (cue, run, at) = preview_start(&p).unwrap();
    assert_eq!((cue, at), (id(1), 500), "no pre-wait");
    assert_eq!(changed(&p), vec![(id(1), true, 500)]);
    assert!(show_actions(&p).is_empty(), "{p:?}");
    assert!(rig.engine.is_previewing(id(1)));
    assert_eq!(rig.engine.preview_run(id(1)), Some(run));
    assert_eq!(rig.engine.previewing().collect::<Vec<_>>(), vec![id(1)]);
    // Not a show run: far from the show's ids, and the cue has no show state.
    assert!(run.0 >= PREVIEW_RUN_BASE);
    assert_eq!(rig.state(1), CueState::Idle);
    assert_eq!(rig.engine.run_of(id(1)), None);
    assert_eq!(rig.engine.cue_states().count(), 0);
}

#[test]
fn a_preview_triggers_no_continue_and_ends_with_the_voice() {
    // Auto-follow and auto-continue are both the show's business.
    for mode in [AF, AC] {
        let mut rig = Rig::new(vec![timed(sound(1), 0.0, 1.0, mode), sound(2)]);
        let p = rig.preview(0, 1);
        let (_, run, _) = preview_start(&p).unwrap();
        // The post-wait would have fired at 1000; the voice ends at 1500.
        let tick = rig.at(1200, Input::Tick);
        assert!(tick.is_empty(), "{tick:?}");
        let end = rig.at(
            1500,
            Input::ActionEnded {
                cue: id(1),
                run,
                at: 1500,
            },
        );
        assert_eq!(changed(&end), vec![(id(1), false, 1500)]);
        assert!(
            show_actions(&end).is_empty() && warnings(&end).is_empty(),
            "{end:?}"
        );
        assert!(!rig.engine.is_previewing(id(1)));
        assert_eq!(rig.state(2), CueState::Idle);
        assert_eq!(rig.engine.next_deadline(), None);
    }
}

#[test]
fn a_preview_leaves_the_playhead_the_double_go_timer_and_groups_alone() {
    let child = sound(11);
    let g = cue(10, group(vec![child]));
    let mut rig = Rig::new(vec![sound(1), g, sound(2)]);
    rig.ws.settings.min_go_interval = 1.0;
    assert_eq!(rig.engine.playhead(list_id(1)), Some(id(1)));
    // Previewing neither starts the double-GO interval...
    let p = rig.preview(0, 2);
    assert!(show_actions(&p).is_empty());
    let go = rig.go(10);
    assert!(!go.iter().any(|a| matches!(a, Action::GoIgnored { .. })));
    // ...nor restarts it.
    let playhead = rig.engine.playhead(list_id(1));
    rig.preview(900, 11);
    assert_eq!(rig.engine.playhead(list_id(1)), playhead);
    // A previewed child does not make its idle group run (nor does a started one).
    assert_eq!(rig.state(10), CueState::Idle);
    let early = rig.go(950);
    assert!(early.contains(&Action::GoIgnored {
        list: list_id(1),
        reason: GoIgnoredReason::DoubleGo
    }));
    let ok = rig.go(1010);
    assert!(!ok.iter().any(|a| matches!(a, Action::GoIgnored { .. })));
    assert!(rig.engine.is_previewing(id(11)) && rig.engine.is_previewing(id(2)));
}

#[test]
fn a_preview_is_independent_of_the_same_cue_in_the_show() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.go(0);
    let show_run = rig.engine.run_of(id(1)).unwrap();
    let p = rig.preview(100, 1);
    let (_, run, _) = preview_start(&p).unwrap();
    assert_ne!(run, show_run);
    assert_eq!(rig.state(1), CueState::Running);
    // Stopping the show's cue does not stop the preview...
    let stop = rig.at(
        200,
        Input::Stop {
            cue: id(1),
            fade_out: None,
        },
    );
    assert!(stops(&stop).is_empty());
    assert!(rig.engine.is_previewing(id(1)));
    assert_eq!(rig.state(1), CueState::Idle);
    // ...and the show's end report does not end it: only its own run does.
    let end = rig.at(
        300,
        Input::ActionEnded {
            cue: id(1),
            run: show_run,
            at: 300,
        },
    );
    assert!(end.is_empty(), "stale show run: {end:?}");
    assert!(rig.engine.is_previewing(id(1)));
    // Resetting and starting the show's cue again leave it too; stopping the preview leaves the show.
    rig.at(400, Input::Start { cue: id(1) });
    assert_eq!(rig.state(1), CueState::Running);
    let sp = rig.stop_preview(500, 1);
    assert_eq!(stops(&sp).len(), 1);
    assert_eq!(rig.state(1), CueState::Running);
    assert!(show_actions(&sp).is_empty());
}

#[test]
fn the_shows_run_ids_do_not_depend_on_previews() {
    let ids = |preview: bool| {
        let mut rig = Rig::new(vec![sound(1), sound(2), sound(3)]);
        if preview {
            rig.preview(0, 2);
            rig.preview(0, 3);
        }
        rig.go(10);
        let a = rig.engine.run_of(id(1));
        if preview {
            rig.stop_preview(20, 2);
            rig.preview(20, 2);
        }
        rig.go(30);
        (a, rig.engine.run_of(id(2)))
    };
    assert_eq!(ids(false), ids(true));
}

#[test]
fn a_second_preview_of_a_previewing_cue_is_ignored() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.preview(0, 1);
    let run = rig.engine.preview_run(id(1));
    let again = rig.preview(100, 1);
    assert_eq!(
        again,
        vec![Action::Warning(EngineWarning::PreviewIgnored {
            cue: id(1),
            reason: PreviewIgnoredReason::AlreadyPreviewing
        })]
    );
    assert_eq!(rig.engine.preview_run(id(1)), run, "never restarted");
}

#[test]
fn only_playable_audio_cues_are_previewed() {
    let mut rig = Rig::new(vec![memo(1), sound(2), cue(3, group(vec![memo(4)]))]);
    assert_eq!(
        rig.preview(0, 1),
        vec![Action::Warning(EngineWarning::PreviewIgnored {
            cue: id(1),
            reason: PreviewIgnoredReason::NotAudio
        })]
    );
    // A group with no audio cue in it has nothing to play (QLab check V1).
    assert_eq!(
        rig.preview(0, 3),
        vec![Action::Warning(EngineWarning::PreviewIgnored {
            cue: id(3),
            reason: PreviewIgnoredReason::NothingToPreview
        })]
    );
    assert!(!rig.engine.is_previewing(id(3)));
    assert_eq!(
        rig.preview(0, 99),
        vec![Action::Warning(EngineWarning::UnknownCue { cue: id(99) })]
    );
    // A cue with problems is refused, as any start is.
    rig.at(
        1,
        Input::MediaStatus {
            media: MediaRef::new("media/a.wav"),
            status: MediaStatus::Missing,
        },
    );
    let broken = rig.preview(2, 2);
    assert_eq!(
        warnings(&broken),
        vec![EngineWarning::CueBroken { cue: id(2) }]
    );
    assert!(preview_start(&broken).is_none());
    assert!(!rig.engine.is_previewing(id(2)));
}

/// QLab check V2: QLab does not play a preview of a disarmed cue.
#[test]
fn a_disarmed_cue_cannot_be_previewed() {
    let mut c = sound(1);
    c.armed = false;
    let mut rig = Rig::new(vec![c]);
    assert_eq!(
        rig.preview(0, 1),
        vec![Action::Warning(EngineWarning::PreviewIgnored {
            cue: id(1),
            reason: PreviewIgnoredReason::Disarmed
        })]
    );
    assert!(!rig.engine.is_previewing(id(1)));
}

#[test]
fn stopping_a_preview_fades_it_out_briefly_and_is_idempotent() {
    let mut rig = Rig::new(vec![sound(1), sound(2)]);
    rig.preview(100, 1);
    rig.preview(100, 2);
    let (_, run1) = (id(1), rig.engine.preview_run(id(1)).unwrap());
    let stop = rig.stop_preview(400, 1);
    let fade = StopFade {
        frames: 50,
        curve: FadeCurve::Linear,
    };
    assert_eq!(stops(&stop), vec![(id(1), run1, 400, Some(fade))]);
    assert_eq!(changed(&stop), vec![(id(1), false, 400)]);
    assert!(rig.engine.is_previewing(id(2)), "only that cue");
    // Already stopped: nothing. The voice's late end report is stale.
    assert!(rig.stop_preview(410, 1).is_empty());
    let late = rig.at(
        460,
        Input::ActionEnded {
            cue: id(1),
            run: run1,
            at: 450,
        },
    );
    assert!(late.is_empty(), "{late:?}");
    // None stops every preview, in id order.
    let all = rig.at(500, Input::StopPreview { cue: None });
    assert_eq!(stops(&all).len(), 1);
    assert!(rig.engine.previewing().next().is_none());
    // And it can be previewed again, on a new voice.
    let p = rig.preview(600, 1);
    assert_ne!(preview_start(&p).unwrap().1, run1);
}

#[test]
fn a_stop_is_never_dated_before_the_preview_started() {
    // The clock going back (device restart) must not date a stop before its start.
    let mut rig = Rig::new(vec![sound(1)]);
    rig.preview(1000, 1);
    let stop = rig.stop_preview(10, 1);
    assert_eq!(stops(&stop)[0].2, 1000);
}

#[test]
fn a_panic_with_no_panic_time_stops_previews_at_once() {
    let mut rig = Rig::new(vec![sound(1), sound(2)]);
    rig.ws.settings.panic_duration = 0.0;
    rig.preview(0, 1);
    rig.preview(0, 2);
    let panic = rig.at(300, Input::Panic);
    let s = stops(&panic);
    assert_eq!(s.len(), 2);
    assert!(
        s.iter()
            .all(|(_, _, at, fade)| *at == 300 && fade.is_none())
    );
    assert_eq!(changed(&panic).len(), 2);
    assert_eq!(rig.engine.previewing().count(), 0);
    assert_eq!(rig.engine.next_deadline(), None);
}

#[test]
fn a_failed_preview_frees_its_voice_and_does_not_break_the_cue() {
    let mut rig = Rig::new(vec![sound(1)]);
    let (_, run, _) = preview_start(&rig.preview(0, 1)).unwrap();
    let fail = rig.at(
        50,
        Input::ActionFailed {
            cue: id(1),
            run,
            reason: BrokenReason::StreamFailed,
        },
    );
    assert_eq!(stops(&fail), vec![(id(1), run, 50, None)]);
    assert_eq!(changed(&fail), vec![(id(1), false, 50)]);
    assert_eq!(
        warnings(&fail),
        vec![EngineWarning::PreviewFailed {
            cue: id(1),
            reason: BrokenReason::StreamFailed
        }]
    );
    assert_eq!(rig.state(1), CueState::Idle, "the show's cue is not broken");
    assert_eq!(rig.engine.broken_reason(id(1)), None);
}

#[test]
fn deleting_or_changing_a_previewed_cue_stops_its_preview() {
    let mut rig = Rig::new(vec![sound(1), sound(2), memo(3)]);
    rig.preview(0, 1);
    rig.preview(0, 2);
    rig.ws.cue_lists[0].cues.remove(0);
    let edit = rig.at(10, Input::Tick);
    assert_eq!(stops(&edit).len(), 1);
    assert_eq!(stops(&edit)[0].3, None, "hard");
    assert!(!rig.engine.is_previewing(id(1)));
    // Changed into a cue that is not audio: the voice no longer belongs to it.
    rig.ws.cue_lists[0].cues[0].kind = CueKind::Memo;
    let edit = rig.at(20, Input::Tick);
    assert_eq!(stops(&edit).len(), 1);
    assert_eq!(rig.engine.previewing().count(), 0);
}

#[test]
fn a_preview_does_not_use_the_chain_budget_nor_start_other_cues() {
    // A Start cue's target and a cycle are the show's; previewing a sound next to them does
    // nothing to either.
    let mut rig = Rig::new(vec![
        cue(
            1,
            CueKind::Start {
                target: Some(id(2)),
            },
        ),
        sound(2),
    ]);
    let p = rig.preview(0, 2);
    assert!(show_actions(&p).is_empty());
    assert_eq!(rig.state(1), CueState::Idle);
    assert_eq!(rig.state(2), CueState::Idle);
}
