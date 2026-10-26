// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Group cues (CT-22) and the playhead inside groups (M2.11), per docs/cue-engine.md "Groups".
//! Virtual sample clock at 1 kHz: one second is 1000 samples.

mod common;

use common::*;
use openlab_core::*;

const RATE: u32 = 1000;

use ContinueMode::{AutoContinue as AC, AutoFollow as AF, DoNotContinue as DNC};

fn id(n: u128) -> CueId {
    cue_id(n)
}

fn list() -> CueListId {
    list_id(1)
}

fn memo(n: u128) -> Cue {
    cue(n, CueKind::Memo)
}

fn wait(n: u128, seconds: f64) -> Cue {
    cue(n, CueKind::Wait { duration: seconds })
}

fn sound(n: u128) -> Cue {
    cue(n, audio())
}

fn timed(mut c: Cue, pre: f64, post: f64, mode: ContinueMode) -> Cue {
    c.pre_wait = pre;
    c.post_wait = post;
    c.continue_mode = mode;
    c
}

fn grp(n: u128, mode: GroupMode, children: Vec<Cue>) -> Cue {
    cue(n, CueKind::Group { mode, children })
}

const TIMELINE: GroupMode = GroupMode::Timeline;
const ENTER: GroupMode = GroupMode::StartFirstAndEnter;
const GO_NEXT: GroupMode = GroupMode::StartFirstAndGoToNext;
const RANDOM: GroupMode = GroupMode::StartRandom;

fn playlist(loop_: bool, shuffle: bool) -> GroupMode {
    GroupMode::Playlist { loop_, shuffle }
}

fn children_mut(c: &mut Cue) -> &mut Vec<Cue> {
    match &mut c.kind {
        CueKind::Group { children, .. } => children,
        _ => panic!("not a group"),
    }
}

struct Rig {
    ws: Workspace,
    engine: CueEngine,
    /// The workspace as the engine last saw it (see `handle_as_binary`).
    last: Option<Workspace>,
}

impl Rig {
    fn new(cues: Vec<Cue>) -> Self {
        Self::seeded(cues, 1)
    }

    fn seeded(cues: Vec<Cue>, seed: u64) -> Self {
        let mut rig = Self {
            ws: workspace(cues),
            engine: CueEngine::new(EngineConfig::new(RATE, 0, seed).unwrap()),
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
        self.at(now, Input::Go { list: list() })
    }

    fn start(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::Start { cue: id(n) })
    }

    fn stop(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(
            now,
            Input::Stop {
                cue: id(n),
                fade_out: None,
            },
        )
    }

    fn ended(&mut self, now: SampleTime, n: u128, at: SampleTime) -> Vec<Action> {
        let run = self.engine.run_of(id(n)).unwrap();
        self.at(
            now,
            Input::ActionEnded {
                cue: id(n),
                run,
                at,
            },
        )
    }

    fn failed(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        let run = self.engine.run_of(id(n)).unwrap();
        self.at(
            now,
            Input::ActionFailed {
                cue: id(n),
                run,
                reason: BrokenReason::StreamFailed,
            },
        )
    }

    fn state(&self, n: u128) -> CueState {
        self.engine.state(id(n))
    }

    fn playhead(&self) -> Option<CueId> {
        self.engine.playhead(list())
    }

    fn cues_mut(&mut self) -> &mut Vec<Cue> {
        &mut self.ws.cue_lists[0].cues
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

fn running_at(actions: &[Action], n: u128) -> Option<SampleTime> {
    changes(actions, n)
        .into_iter()
        .find(|(s, _)| *s == CueState::Running)
        .map(|(_, at)| at)
}

/// `(cue, at)` of every start (a report of pre-waiting).
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

fn voice_starts(actions: &[Action]) -> Vec<(CueId, SampleTime, SampleTime)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::StartVoice {
                cue, at, offset, ..
            } => Some((*cue, *at, *offset)),
            _ => None,
        })
        .collect()
}

fn stopped_voices(actions: &[Action]) -> Vec<CueId> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::StopVoice { cue, .. } => Some(*cue),
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

fn moved(actions: &[Action]) -> Option<Option<CueId>> {
    actions.iter().find_map(|a| match a {
        Action::PlayheadChanged { cue, .. } => Some(*cue),
        _ => None,
    })
}

// ----- timeline -----

#[test]
fn timeline_starts_every_child_on_the_group_action_sample() {
    let g = timed(
        grp(
            1,
            TIMELINE,
            vec![
                sound(10),
                timed(wait(11, 2.0), 0.5, 0.0, DNC),
                timed(memo(12), 0.25, 0.0, DNC),
            ],
        ),
        1.0,
        0.0,
        DNC,
    );
    let mut rig = Rig::new(vec![g]);
    let go = rig.go(100);
    assert_eq!(changes(&go, 1), vec![(CueState::PreWaiting, 100)]);
    assert_eq!(rig.state(10), CueState::Idle);

    let tick = rig.tick(1100);
    assert_eq!(running_at(&tick, 1), Some(1100));
    assert_eq!(
        starts(&tick),
        vec![(id(10), 1100), (id(11), 1100), (id(12), 1100)]
    );
    assert_eq!(voice_starts(&tick), vec![(id(10), 1100, 0)]);
    // Each child runs its own pre-wait.
    assert_eq!(running_at(&rig.tick(1350), 12), Some(1350));
    assert_eq!(running_at(&rig.tick(1600), 11), Some(1600));
    assert_eq!(rig.state(1), CueState::Running);
}

#[test]
fn timeline_ignores_the_children_continue_modes() {
    let g = grp(
        1,
        TIMELINE,
        vec![
            timed(wait(10, 1.0), 0.0, 0.0, AC),
            timed(wait(11, 1.0), 0.0, 0.0, AF),
            memo(12),
        ],
    );
    let mut rig = Rig::new(vec![g]);
    let go = rig.go(0);
    assert_eq!(starts(&go).len(), 4);
    // Had 10's auto-continue fired, 11 would have warned TriggerIgnored.
    assert!(warnings(&go).is_empty());
    let tick = rig.tick(1000);
    assert!(starts(&tick).is_empty());
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 1000)]);
}

#[test]
fn group_completes_with_its_last_child_and_then_auto_follows() {
    let g = timed(
        grp(1, TIMELINE, vec![wait(10, 1.0), wait(11, 3.0)]),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![g, memo(2)]);
    let go = rig.go(0);
    assert_eq!(running_at(&go, 1), Some(0));
    // GO skips the group's continue chain.
    assert_eq!(rig.playhead(), None);
    let tick = rig.tick(1000);
    assert_eq!(changes(&tick, 10), vec![(CueState::Idle, 1000)]);
    assert!(changes(&tick, 1).is_empty());
    assert_eq!(rig.state(1), CueState::Running);
    let tick = rig.tick(3000);
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 3000)]);
    assert_eq!(running_at(&tick, 2), Some(3000));
}

#[test]
fn group_auto_continue_counts_its_post_wait_from_its_action_start() {
    let g = timed(grp(1, TIMELINE, vec![wait(10, 2.0)]), 0.5, 1.0, AC);
    let mut rig = Rig::new(vec![g, memo(2)]);
    rig.go(0);
    let tick = rig.tick(1500);
    assert_eq!(running_at(&tick, 2), Some(1500));
    assert_eq!(rig.state(1), CueState::Running);
    let tick = rig.tick(2500);
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 2500)]);
}

#[test]
fn group_post_waits_after_its_children_when_its_post_wait_is_longer() {
    let g = timed(grp(1, TIMELINE, vec![wait(10, 1.0)]), 0.0, 3.0, AC);
    let mut rig = Rig::new(vec![g, memo(2)]);
    rig.go(0);
    assert_eq!(
        changes(&rig.tick(1000), 1),
        vec![(CueState::PostWaiting, 1000)]
    );
    let tick = rig.tick(3000);
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 3000)]);
    assert_eq!(running_at(&tick, 2), Some(3000));
}

#[test]
fn empty_and_disarmed_groups_complete_at_once() {
    let empty = timed(grp(1, TIMELINE, vec![]), 0.0, 0.0, AF);
    let mut disarmed = timed(grp(2, TIMELINE, vec![sound(20)]), 0.0, 0.0, AF);
    disarmed.armed = false;
    let mut rig = Rig::new(vec![empty, disarmed, memo(3)]);
    let go = rig.go(0);
    assert_eq!(running_at(&go, 3), Some(0));
    assert_eq!(rig.state(20), CueState::Idle);
    assert!(warnings(&go).is_empty());
}

// ----- start first and go to next -----

#[test]
fn start_first_and_go_to_next_chains_children_and_skips_the_group() {
    let g = grp(
        1,
        GO_NEXT,
        vec![timed(wait(10, 1.0), 0.0, 0.0, AF), wait(11, 1.0), memo(12)],
    );
    let mut rig = Rig::new(vec![g, memo(3)]);
    let go = rig.go(0);
    assert_eq!(starts(&go), vec![(id(1), 0), (id(10), 0)]);
    assert_eq!(rig.playhead(), Some(id(3)));
    let tick = rig.tick(1000);
    assert_eq!(running_at(&tick, 11), Some(1000));
    let tick = rig.tick(2000);
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 2000)]);
    // 11 does not continue, so 12 never starts.
    assert_eq!(rig.state(12), CueState::Idle);
    assert_eq!(rig.state(3), CueState::Idle);
}

// ----- start first and enter (M2.11) -----

#[test]
fn start_first_and_enter_moves_the_playhead_into_the_group() {
    let g = grp(
        1,
        ENTER,
        vec![
            timed(wait(10, 1.0), 0.0, 0.0, AC),
            wait(11, 1.0),
            memo(12),
            memo(13),
        ],
    );
    let mut rig = Rig::new(vec![g, memo(3)]);
    let go = rig.go(0);
    assert_eq!(starts(&go), vec![(id(1), 0), (id(10), 0), (id(11), 0)]);
    // The first child's continue chain (10 → 11) is skipped.
    assert_eq!(rig.playhead(), Some(id(12)));

    let go = rig.go(100);
    assert_eq!(running_at(&go, 12), Some(100));
    assert_eq!(rig.playhead(), Some(id(13)));
    // Past the group's last child the playhead leaves the group.
    rig.go(200);
    assert_eq!(rig.playhead(), Some(id(3)));
    assert_eq!(rig.state(1), CueState::Running);
    let tick = rig.tick(1000);
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 1000)]);
    assert_eq!(rig.state(3), CueState::Idle);
}

#[test]
fn leaving_an_entered_group_skips_the_group_continue_chain() {
    let g = timed(grp(1, ENTER, vec![memo(10), memo(11)]), 0.0, 0.0, AC);
    let mut rig = Rig::new(vec![g, memo(2), memo(3)]);
    rig.go(0);
    assert_eq!(rig.playhead(), Some(id(11)));
    rig.go(100);
    // 2 is the group's auto-continue target.
    assert_eq!(rig.playhead(), Some(id(3)));
}

#[test]
fn go_inside_an_idle_entered_group_runs_the_group_without_its_continue() {
    let g = timed(grp(1, ENTER, vec![memo(10), wait(11, 1.0)]), 0.0, 0.0, AF);
    let mut rig = Rig::new(vec![g, memo(3)]);
    let go = rig.go(0);
    // 10 is instant and does not continue: the group completes at once and auto-follows.
    assert_eq!(changes(&go, 1).last(), Some(&(CueState::Idle, 0)));
    assert_eq!(running_at(&go, 3), Some(0));
    assert_eq!(rig.playhead(), Some(id(11)));

    let go = rig.go(100);
    assert_eq!(running_at(&go, 11), Some(100));
    assert_eq!(changes(&go, 1), vec![(CueState::Running, 100)]);
    let tick = rig.tick(1100);
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 1100)]);
    // An adopted group fires no continue.
    assert!(changes(&tick, 3).is_empty());
    // Leaving the group skipped its continue chain (3 is its auto-follow target).
    assert_eq!(rig.playhead(), None);
}

#[test]
fn starting_a_child_makes_every_idle_group_around_it_run() {
    let inner = grp(2, TIMELINE, vec![wait(20, 1.0)]);
    let outer = timed(grp(1, TIMELINE, vec![inner]), 0.0, 0.0, AF);
    let mut rig = Rig::new(vec![outer, memo(3)]);
    let start = rig.start(0, 20);
    assert_eq!(changes(&start, 2), vec![(CueState::Running, 0)]);
    assert_eq!(changes(&start, 1), vec![(CueState::Running, 0)]);
    let tick = rig.tick(1000);
    assert_eq!(changes(&tick, 2), vec![(CueState::Idle, 1000)]);
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 1000)]);
    assert_eq!(rig.state(3), CueState::Idle);
}

#[test]
fn a_child_started_while_its_group_post_waits_makes_it_run_again() {
    let g = timed(
        grp(1, TIMELINE, vec![memo(10), wait(11, 1.0)]),
        0.0,
        5.0,
        AC,
    );
    let mut rig = Rig::new(vec![g, memo(2)]);
    rig.start(0, 1);
    rig.tick(1000);
    assert_eq!(rig.state(1), CueState::PostWaiting);
    let start = rig.start(2000, 11);
    assert_eq!(changes(&start, 1), vec![(CueState::Running, 2000)]);
    assert_eq!(
        changes(&rig.tick(3000), 1),
        vec![(CueState::PostWaiting, 3000)]
    );
    // Its own post-wait still triggers the next cue on schedule.
    assert_eq!(running_at(&rig.tick(5000), 2), Some(5000));
}

// ----- playhead navigation in and out of groups (PB-05, M2.11) -----

fn nav_rig() -> Rig {
    Rig::new(vec![
        numbered(1, "1", CueKind::Memo),
        {
            let mut g = grp(
                2,
                ENTER,
                vec![
                    numbered(20, "2.1", CueKind::Memo),
                    numbered(21, "2.2", CueKind::Memo),
                ],
            );
            g.number = Some("2".into());
            g
        },
        numbered(3, "3", CueKind::Memo),
        {
            let mut g = grp(4, TIMELINE, vec![numbered(40, "4.1", CueKind::Memo)]);
            g.number = Some("4".into());
            g
        },
    ])
}

#[test]
fn next_and_previous_step_into_and_out_of_groups_of_every_mode() {
    let mut rig = nav_rig();
    let next = Input::PlayheadNext { list: list() };
    let previous = Input::PlayheadPrevious { list: list() };
    let mut seen = vec![rig.playhead()];
    for _ in 0..7 {
        rig.at(0, next.clone());
        seen.push(rig.playhead());
    }
    let ids = |ns: &[u128]| ns.iter().map(|n| Some(id(*n))).collect::<Vec<_>>();
    let mut expected = ids(&[1, 2, 20, 21, 3, 4, 40]);
    expected.push(None);
    assert_eq!(seen, expected);

    let mut back = Vec::new();
    for _ in 0..8 {
        rig.at(0, previous.clone());
        back.push(rig.playhead());
    }
    assert_eq!(back, ids(&[40, 4, 3, 21, 20, 2, 1, 1]));
    assert_eq!(
        CueEngine::playhead_positions(&rig.ws, list()),
        vec![id(1), id(2), id(20), id(21), id(3), id(4), id(40)]
    );
}

#[test]
fn jumps_reach_cues_inside_groups_of_every_mode() {
    let mut rig = nav_rig();
    let jump = rig.at(
        0,
        Input::SetPlayheadByNumber {
            list: list(),
            number: "2.2".into(),
        },
    );
    assert_eq!(moved(&jump), Some(Some(id(21))));
    assert_eq!(
        rig.engine.standing_by(&rig.ws, list()).map(|c| c.id),
        Some(id(21))
    );
    let jump = rig.at(
        0,
        Input::SetPlayhead {
            list: list(),
            cue: Some(id(20)),
        },
    );
    assert_eq!(moved(&jump), Some(Some(id(20))));
    // Inside a timeline group too (QLab 5: any child can stand by).
    let jump = rig.at(
        0,
        Input::SetPlayheadByNumber {
            list: list(),
            number: "4.1".into(),
        },
    );
    assert_eq!(moved(&jump), Some(Some(id(40))));
    // A cue of no list warns and leaves the playhead.
    let jump = rig.at(
        0,
        Input::SetPlayhead {
            list: list(),
            cue: Some(id(999)),
        },
    );
    assert_eq!(
        warnings(&jump),
        vec![EngineWarning::NotInList {
            list: list(),
            cue: id(999)
        }]
    );
    assert_eq!(rig.playhead(), Some(id(40)));
}

fn set_playhead(rig: &mut Rig, n: u128) {
    rig.at(
        0,
        Input::SetPlayhead {
            list: list(),
            cue: Some(id(n)),
        },
    );
}

#[test]
fn go_on_a_timeline_child_starts_only_that_child_and_moves_to_its_sibling() {
    let g = timed(
        grp(
            1,
            TIMELINE,
            vec![timed(wait(10, 1.0), 0.0, 0.0, AC), wait(11, 1.0), memo(12)],
        ),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![g, memo(2), memo(3)]);
    set_playhead(&mut rig, 10);
    let go = rig.go(0);
    // Children's continue modes are ignored in a timeline: 10's auto-continue starts nothing.
    assert_eq!(starts(&go), vec![(id(10), 0)]);
    assert_eq!(changes(&go, 1), vec![(CueState::Running, 0)]);
    assert_eq!(rig.playhead(), Some(id(11)));
    rig.go(100);
    rig.go(200);
    // Past the last child: out of the group, past its continue chain (2 is its auto-follow).
    assert_eq!(rig.playhead(), Some(id(3)));
    let tick = rig.tick(1100);
    // Adopted: no auto-follow.
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 1100)]);
    assert_eq!(rig.state(2), CueState::Idle);
}

#[test]
fn go_on_a_playlist_child_plays_only_that_child() {
    let g = grp(
        1,
        playlist(false, false),
        vec![wait(10, 1.0), wait(11, 1.0)],
    );
    let mut rig = Rig::new(vec![g, memo(2)]);
    set_playhead(&mut rig, 10);
    rig.go(0);
    assert_eq!(rig.playhead(), Some(id(11)));
    let tick = rig.tick(1000);
    assert!(starts(&tick).is_empty());
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 1000)]);
}

#[test]
fn go_on_a_go_to_next_child_chains_within_the_group() {
    let g = grp(
        1,
        GO_NEXT,
        vec![memo(10), timed(memo(11), 0.0, 0.0, AF), memo(12), memo(13)],
    );
    let mut rig = Rig::new(vec![g, memo(2)]);
    set_playhead(&mut rig, 11);
    let go = rig.go(0);
    assert_eq!(starts(&go), vec![(id(11), 0), (id(12), 0)]);
    assert_eq!(rig.playhead(), Some(id(13)));
}

#[test]
fn go_on_a_cue_inside_an_entered_group_starts_only_that_cue() {
    let mut rig = nav_rig();
    rig.at(
        0,
        Input::SetPlayhead {
            list: list(),
            cue: Some(id(20)),
        },
    );
    let go = rig.go(0);
    assert_eq!(starts(&go), vec![(id(20), 0)]);
    assert_eq!(
        changes(&go, 2),
        vec![(CueState::Running, 0), (CueState::Idle, 0)]
    );
    assert_eq!(rig.playhead(), Some(id(21)));
    rig.go(10);
    assert_eq!(rig.playhead(), Some(id(3)));
}

#[test]
fn playhead_inside_a_group_follows_edits() {
    let set = |rig: &mut Rig, n: u128| {
        rig.at(
            0,
            Input::SetPlayhead {
                list: list(),
                cue: Some(id(n)),
            },
        );
    };
    // The playhead cue is deleted: the next position.
    let mut rig = nav_rig();
    set(&mut rig, 21);
    children_mut(&mut rig.cues_mut()[1]).remove(1);
    assert_eq!(moved(&rig.tick(0)), Some(Some(id(3))));

    // Its group is deleted: the first position after it that still exists.
    let mut rig = nav_rig();
    set(&mut rig, 20);
    rig.cues_mut().remove(1);
    assert_eq!(moved(&rig.tick(0)), Some(Some(id(3))));

    // Moved out of the group: still a position, so it stays.
    let mut rig = nav_rig();
    set(&mut rig, 20);
    let c = children_mut(&mut rig.cues_mut()[1]).remove(0);
    rig.cues_mut().push(c);
    assert_eq!(moved(&rig.tick(0)), None);
    assert_eq!(rig.playhead(), Some(id(20)));

    // Its group changes mode: still a position.
    let mut rig = nav_rig();
    set(&mut rig, 20);
    if let CueKind::Group { mode, .. } = &mut rig.cues_mut()[1].kind {
        *mode = TIMELINE;
    }
    assert_eq!(moved(&rig.tick(0)), None);
    assert_eq!(rig.playhead(), Some(id(20)));
}

// ----- playlist -----

#[test]
fn playlist_plays_children_one_after_another() {
    let p = timed(
        grp(
            1,
            playlist(false, false),
            vec![
                timed(wait(10, 1.0), 0.0, 5.0, AC),
                timed(wait(11, 0.5), 0.25, 0.0, AF),
                memo(12),
            ],
        ),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![p, memo(2)]);
    let go = rig.go(0);
    assert_eq!(starts(&go), vec![(id(1), 0), (id(10), 0)]);
    let tick = rig.tick(1000);
    assert_eq!(starts(&tick), vec![(id(11), 1000)]);
    // Child continue modes are ignored: no post-wait keeps 10 busy.
    assert_eq!(changes(&tick, 10), vec![(CueState::Idle, 1000)]);
    assert_eq!(running_at(&rig.tick(1250), 11), Some(1250));
    let tick = rig.tick(1750);
    assert_eq!(starts(&tick), vec![(id(12), 1750), (id(2), 1750)]);
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 1750)]);
}

#[test]
fn looping_playlist_wraps_until_it_is_stopped() {
    let p = timed(
        grp(1, playlist(true, false), vec![wait(10, 1.0), wait(11, 1.0)]),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![p, memo(2)]);
    let mut all = rig.go(0);
    for t in [1000, 2000, 3000, 4000] {
        all.extend(rig.tick(t));
    }
    let child_starts: Vec<_> = starts(&all)
        .into_iter()
        .filter(|(c, _)| *c != id(1))
        .collect();
    assert_eq!(
        child_starts,
        vec![
            (id(10), 0),
            (id(11), 1000),
            (id(10), 2000),
            (id(11), 3000),
            (id(10), 4000)
        ]
    );
    assert_eq!(rig.state(1), CueState::Running);
    let stop = rig.stop(4500, 1);
    assert_eq!(changes(&stop, 1), vec![(CueState::Idle, 4500)]);
    assert_eq!(rig.state(10), CueState::Idle);
    assert_eq!(rig.state(2), CueState::Idle);
    assert!(rig.tick(10_000).is_empty());
}

fn shuffle_order(seed: u64, passes: usize) -> Vec<CueId> {
    let kids = (10..15).map(|n| wait(n, 1.0)).collect();
    let mut rig = Rig::seeded(vec![grp(1, playlist(true, true), kids)], seed);
    let mut all = rig.go(0);
    for pass in 1..(passes * 5) as u64 {
        all.extend(rig.tick(pass * 1000));
    }
    starts(&all)
        .into_iter()
        .map(|(c, _)| c)
        .filter(|c| *c != id(1))
        .collect()
}

#[test]
fn shuffled_playlist_plays_every_child_once_per_pass_in_a_fresh_order() {
    let order = shuffle_order(1, 4);
    assert_eq!(order.len(), 20);
    let passes: Vec<Vec<CueId>> = order.chunks(5).map(<[CueId]>::to_vec).collect();
    for pass in &passes {
        let mut sorted = pass.clone();
        sorted.sort();
        let mut all: Vec<CueId> = (10..15).map(id).collect();
        all.sort();
        assert_eq!(sorted, all, "every child exactly once per pass");
    }
    assert!(
        passes.windows(2).any(|w| w[0] != w[1]),
        "passes reshuffle: {passes:?}"
    );
    assert_ne!(passes[0], (10..15).map(id).collect::<Vec<_>>());
    // Deterministic for a seed.
    assert_eq!(shuffle_order(1, 4), order);
    assert_ne!(shuffle_order(2, 4), order);
}

#[test]
fn playlist_skips_a_child_that_breaks_and_does_not_auto_follow() {
    let p = timed(
        grp(1, playlist(false, false), vec![sound(10), wait(11, 1.0)]),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![p, memo(2)]);
    rig.go(0);
    let failed = rig.failed(500, 10);
    assert_eq!(changes(&failed, 10), vec![(CueState::Broken, 500)]);
    assert_eq!(starts(&failed), vec![(id(11), 500)]);
    let tick = rig.tick(1500);
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 1500)]);
    assert_eq!(rig.state(2), CueState::Idle);

    // Next time the broken child cannot start: it is skipped at once.
    let go = rig.start(2000, 1);
    assert!(warnings(&go).contains(&EngineWarning::CueBroken { cue: id(10) }));
    assert_eq!(starts(&go), vec![(id(1), 2000), (id(11), 2000)]);
}

#[test]
fn reordering_a_playlist_while_it_plays_applies_to_the_next_child() {
    let p = grp(
        1,
        playlist(false, false),
        vec![wait(10, 1.0), wait(11, 1.0), wait(12, 1.0)],
    );
    let mut rig = Rig::new(vec![p]);
    rig.go(0);
    children_mut(&mut rig.cues_mut()[0]).swap(1, 2);
    rig.tick(500);
    assert_eq!(starts(&rig.tick(1000)), vec![(id(12), 1000)]);
    assert_eq!(starts(&rig.tick(2000)), vec![(id(11), 2000)]);
    // A child removed from the group ahead of time is simply not played.
    let mut rig = Rig::new(vec![grp(
        1,
        playlist(false, false),
        vec![wait(10, 1.0), wait(11, 1.0), wait(12, 1.0)],
    )]);
    rig.go(0);
    let c = children_mut(&mut rig.cues_mut()[0]).remove(1);
    rig.cues_mut().push(c);
    assert_eq!(starts(&rig.tick(1000)), vec![(id(12), 1000)]);
}

// ----- random -----

#[test]
fn random_group_starts_one_child_and_never_repeats_immediately() {
    let pick_sequence = |seed: u64| {
        let r = grp(1, RANDOM, vec![memo(10), memo(11), memo(12)]);
        let mut rig = Rig::seeded(vec![r], seed);
        let mut picks = Vec::new();
        for i in 0..40 {
            let start = rig.start(i * 10, 1);
            let children: Vec<_> = starts(&start)
                .into_iter()
                .map(|(c, _)| c)
                .filter(|c| *c != id(1))
                .collect();
            assert_eq!(children.len(), 1);
            picks.push(children[0]);
            assert_eq!(rig.state(1), CueState::Idle);
        }
        picks
    };
    let picks = pick_sequence(5);
    assert!(picks.windows(2).all(|w| w[0] != w[1]), "{picks:?}");
    for n in 10..13 {
        assert!(picks.contains(&id(n)));
    }
    assert_eq!(pick_sequence(5), picks);
    assert_ne!(pick_sequence(6), picks);
}

#[test]
fn random_group_with_one_child_always_starts_it() {
    let mut rig = Rig::new(vec![grp(1, RANDOM, vec![memo(10)])]);
    for i in 0..3 {
        assert!(starts(&rig.start(i, 1)).contains(&(id(10), i)));
    }
}

#[test]
fn random_group_ignores_the_chosen_child_continue_mode() {
    let r = grp(
        1,
        RANDOM,
        vec![timed(memo(10), 0.0, 0.0, AF), timed(memo(11), 0.0, 0.0, AF)],
    );
    let mut rig = Rig::new(vec![r]);
    let start = rig.start(0, 1);
    assert_eq!(starts(&start).len(), 2);
}

// ----- nesting -----

#[test]
fn timeline_inside_a_playlist_inside_a_timeline() {
    let inner = timed(
        grp(
            20,
            TIMELINE,
            vec![wait(200, 1.0), timed(wait(201, 1.0), 0.5, 0.0, DNC)],
        ),
        0.0,
        0.0,
        DNC,
    );
    let pl = grp(2, playlist(false, false), vec![inner, wait(21, 1.0)]);
    let top = timed(grp(1, TIMELINE, vec![pl, wait(3, 0.5)]), 0.0, 0.0, AF);
    let mut rig = Rig::new(vec![top, memo(9)]);
    let go = rig.go(0);
    assert_eq!(
        starts(&go),
        vec![
            (id(1), 0),
            (id(2), 0),
            (id(3), 0),
            (id(20), 0),
            (id(200), 0),
            (id(201), 0)
        ]
    );
    let tick = rig.tick(1500);
    // 201 ran from 500 to 1500, so the inner timeline completes then and the playlist moves on.
    assert_eq!(changes(&tick, 20), vec![(CueState::Idle, 1500)]);
    assert_eq!(starts(&tick), vec![(id(21), 1500)]);
    let tick = rig.tick(2500);
    assert_eq!(changes(&tick, 2), vec![(CueState::Idle, 2500)]);
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 2500)]);
    assert_eq!(running_at(&tick, 9), Some(2500));
}

#[test]
fn deepest_nesting_starts_on_one_sample() {
    let mut c = memo(1000);
    for depth in (1..MAX_GROUP_DEPTH as u128).rev() {
        c = grp(depth, TIMELINE, vec![c]);
    }
    let mut rig = Rig::new(vec![c, memo(5000)]);
    rig.cues_mut()[0].continue_mode = AF;
    let go = rig.go(0);
    assert_eq!(starts(&go).len(), MAX_GROUP_DEPTH + 1);
    assert_eq!(running_at(&go, 1000), Some(0));
    assert_eq!(running_at(&go, 5000), Some(0));
    for n in 1..MAX_GROUP_DEPTH as u128 {
        assert_eq!(rig.state(n), CueState::Idle);
    }
}

#[test]
fn looping_playlist_of_instant_children_is_cut_at_the_chain_limit() {
    let p = grp(1, playlist(true, false), vec![memo(10), memo(11)]);
    let mut rig = Rig::new(vec![p, memo(2)]);
    let limit = CueEngine::chain_limit(&rig.ws);
    let go = rig.go(0);
    // The playlist leaves a reserve of starts for what follows the group.
    assert_eq!(starts(&go).len(), limit - 8);
    assert!(
        warnings(&go)
            .iter()
            .any(|w| matches!(w, EngineWarning::ChainLimit { .. }))
    );
    assert_eq!(rig.state(1), CueState::Idle);
    assert!(!rig.engine.is_due(0));
    assert!(starts(&rig.tick(1)).is_empty());
}

#[test]
fn looping_playlist_of_zero_length_sounds_is_cut_across_feedback_calls_at_one_sample() {
    // Regression (M2.9 property seed): each lap needs the mixer's end report, and a voice that
    // ends on its start sample (loaded past its file's end) is reported on that same sample. The
    // binary hands such reports over one call each, all at one `now`; with a fresh chain budget
    // per call the playlist went round forever there and the control thread never got past it.
    let p = grp(1, playlist(true, false), vec![sound(10)]);
    let mut rig = Rig::new(vec![p, memo(2)]);
    let limit = CueEngine::chain_limit(&rig.ws);
    let go = rig.go(0);
    assert_eq!(voice_starts(&go).len(), 1);
    let mut laps = 0;
    let mut cut = false;
    while rig.state(10) == CueState::Running {
        laps += 1;
        assert!(
            laps <= limit,
            "still looping after {laps} end reports at one sample"
        );
        let ended = rig.ended(0, 10, 0);
        cut |= warnings(&ended)
            .iter()
            .any(|w| matches!(w, EngineWarning::ChainLimit { .. }));
    }
    assert!(cut, "cut with a ChainLimit warning");
    assert_eq!(rig.state(1), CueState::Idle, "the group completed");
    assert!(rig.engine.next_deadline().is_none());
    // The budget is per sample: the next one has a fresh one.
    let again = rig.start(1, 1);
    assert_eq!(voice_starts(&again), vec![(id(10), 1, 0)]);
}

#[test]
fn a_cut_looping_playlist_still_starts_the_cue_after_its_group() {
    // Regression (M2-B L1): the zero-length loop used up the call's whole budget, so the group's
    // own continue (the cue after the group) was dropped with ChainLimit.
    let mut p = grp(1, playlist(true, false), vec![memo(10), memo(11)]);
    p.continue_mode = AF;
    let sound_after = memo(2);
    let mut rig = Rig::new(vec![p, sound_after]);
    let go = rig.go(0);
    assert_eq!(
        running_at(&go, 2),
        Some(0),
        "the cue after the group started"
    );
    assert_eq!(
        warnings(&go)
            .iter()
            .filter(|w| matches!(w, EngineWarning::ChainLimit { cue, .. } if *cue == id(2)))
            .count(),
        0
    );
}

#[test]
fn chain_limit_counts_starts_inside_groups() {
    // The group's auto-follow reaches a duplicate of its own id: a cycle of 4 starts per round.
    let g = timed(
        grp(1, TIMELINE, vec![memo(10), memo(11), memo(12)]),
        0.0,
        0.0,
        AF,
    );
    let dup = memo(1);
    let mut rig = Rig::new(vec![g, dup]);
    let limit = CueEngine::chain_limit(&rig.ws);
    let go = rig.go(0);
    assert_eq!(starts(&go).len(), limit);
    assert!(
        warnings(&go)
            .iter()
            .any(|w| matches!(w, EngineWarning::ChainLimit { .. }))
    );
    assert!(!rig.engine.is_due(0));
}

// ----- operations on a group (CUE-05) -----

fn staged_rig() -> Rig {
    // At 500: 10 playing, 11 pre-waiting, 12 a Wait running, 13 inside a nested group playing.
    let g = timed(
        grp(
            1,
            TIMELINE,
            vec![
                sound(10),
                timed(wait(11, 1.0), 1.0, 0.0, DNC),
                wait(12, 2.0),
                grp(13, TIMELINE, vec![sound(130)]),
            ],
        ),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![g, memo(2)]);
    rig.go(0);
    rig.tick(500);
    rig
}

#[test]
fn stop_on_a_group_stops_every_active_descendant() {
    let mut rig = staged_rig();
    let stop = rig.stop(500, 1);
    let mut stopped = stopped_voices(&stop);
    stopped.sort();
    assert_eq!(stopped, vec![id(10), id(130)]);
    for n in [1, 10, 11, 12, 13, 130, 2] {
        assert_eq!(rig.state(n), CueState::Idle, "cue {n}");
    }
    assert!(rig.tick(5000).is_empty());
}

#[test]
fn stop_with_fade_on_a_group_is_stopping_until_its_children_have_faded() {
    let mut rig = staged_rig();
    let stop = rig.at(
        500,
        Input::Stop {
            cue: id(1),
            fade_out: Some(FadeSpec {
                duration: 1.0,
                curve: FadeCurve::Linear,
            }),
        },
    );
    assert_eq!(changes(&stop, 1), vec![(CueState::Stopping, 500)]);
    assert_eq!(changes(&stop, 13), vec![(CueState::Stopping, 500)]);
    assert_eq!(rig.state(10), CueState::Stopping);
    // Silent actions stop at once.
    assert_eq!(rig.state(11), CueState::Idle);
    assert_eq!(rig.state(12), CueState::Idle);
    let tick = rig.tick(1500);
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 1500)]);
    assert_eq!(rig.state(13), CueState::Idle);
    assert_eq!(rig.state(2), CueState::Idle);
}

#[test]
fn pause_and_resume_on_a_group_keep_every_child_timing() {
    let mut rig = staged_rig();
    let pause = rig.at(500, Input::Pause { cue: id(1) });
    for n in [1, 10, 11, 12, 13, 130] {
        assert_eq!(rig.state(n), CueState::Paused, "cue {n}");
    }
    let paused_voices = pause
        .iter()
        .filter(|a| matches!(a, Action::PauseVoice { .. }))
        .count();
    assert_eq!(paused_voices, 2);
    assert!(rig.tick(3000).is_empty());

    let resume = rig.at(1500, Input::Resume { cue: id(1) });
    assert_eq!(changes(&resume, 11), vec![(CueState::PreWaiting, 1500)]);
    assert_eq!(rig.state(1), CueState::Running);
    // 11's pre-wait had 500 left, 12's action 1500.
    assert_eq!(running_at(&rig.tick(2000), 11), Some(2000));
    assert_eq!(changes(&rig.tick(3000), 12), vec![(CueState::Idle, 3000)]);
    rig.ended(3500, 10, 3500);
    let ended = rig.ended(3600, 130, 3600);
    assert_eq!(changes(&ended, 13), vec![(CueState::Idle, 3600)]);
    assert_eq!(changes(&ended, 1), vec![(CueState::Idle, 3600)]);
    assert_eq!(running_at(&ended, 2), Some(3600));
}

#[test]
fn start_on_a_paused_group_resumes_it_and_its_children() {
    let mut rig = staged_rig();
    rig.at(500, Input::Pause { cue: id(1) });
    let start = rig.start(700, 1);
    assert!(
        start
            .iter()
            .any(|a| matches!(a, Action::ResumeVoice { .. }))
    );
    assert_eq!(rig.state(10), CueState::Running);
    assert_eq!(rig.state(11), CueState::PreWaiting);
}

#[test]
fn second_trigger_on_a_running_group_is_ignored() {
    let mut rig = staged_rig();
    let start = rig.start(600, 1);
    assert_eq!(
        warnings(&start),
        vec![EngineWarning::TriggerIgnored {
            cue: id(1),
            state: CueState::Running
        }]
    );
    assert!(starts(&start).is_empty());
}

#[test]
fn reset_on_a_group_clears_broken_children() {
    let mut rig = staged_rig();
    rig.failed(600, 10);
    assert_eq!(rig.state(10), CueState::Broken);
    rig.at(700, Input::Reset { cue: id(1) });
    for n in [1, 10, 11, 12, 13, 130] {
        assert_eq!(rig.state(n), CueState::Idle, "cue {n}");
        assert_eq!(rig.engine.broken_reason(id(n)), None);
    }
}

#[test]
fn stopping_a_child_keeps_the_group_running_unless_it_was_the_last() {
    let g = |n| {
        timed(
            grp(n, TIMELINE, vec![wait(n * 10, 1.0), sound(n * 10 + 1)]),
            0.0,
            0.0,
            AF,
        )
    };
    // Not the last: the group completes with its other child and auto-follows.
    let mut rig = Rig::new(vec![g(1), memo(2)]);
    rig.go(0);
    rig.stop(100, 11);
    assert_eq!(rig.state(1), CueState::Running);
    assert_eq!(running_at(&rig.tick(1000), 2), Some(1000));

    // The last one: the group stops with it and does not follow.
    let mut rig = Rig::new(vec![g(1), memo(2)]);
    rig.go(0);
    rig.tick(1000);
    let stop = rig.stop(1500, 11);
    assert_eq!(changes(&stop, 1), vec![(CueState::Idle, 1500)]);
    assert_eq!(rig.state(2), CueState::Idle);
}

#[test]
fn load_on_a_group_loads_the_children_it_starts() {
    let g = timed(grp(1, TIMELINE, vec![sound(10), sound(11)]), 1.0, 0.0, DNC);
    let mut rig = Rig::new(vec![g]);
    let load = rig.at(
        0,
        Input::Load {
            cue: id(1),
            time: 1.5,
        },
    );
    let loads: Vec<_> = load
        .iter()
        .filter_map(|a| match a {
            Action::LoadVoice { cue, offset, .. } => Some((*cue, *offset)),
            _ => None,
        })
        .collect();
    assert_eq!(loads, vec![(id(10), 500), (id(11), 500)]);
    assert_eq!(rig.state(1), CueState::Loaded);
    let start = rig.start(100, 1);
    assert_eq!(
        voice_starts(&start),
        vec![(id(10), 100, 500), (id(11), 100, 500)]
    );
    // Stop unloads nothing twice and leaves everything idle.
    rig.stop(200, 1);
    assert_eq!(rig.state(10), CueState::Idle);
}

#[test]
fn feedback_naming_a_group_is_ignored() {
    let mut rig = staged_rig();
    let run = rig.engine.run_of(id(1)).unwrap();
    let fb = rig.at(
        600,
        Input::ActionFailed {
            cue: id(1),
            run,
            reason: BrokenReason::VoiceRejected,
        },
    );
    assert!(fb.is_empty());
    assert!(rig.ended(700, 1, 700).is_empty());
    assert_eq!(rig.state(1), CueState::Running);
}

// ----- a child breaking (M2.13 rules inside groups) -----

#[test]
fn broken_child_keeps_its_auto_continue_and_the_group_does_not_auto_follow() {
    let g = timed(
        grp(1, GO_NEXT, vec![timed(sound(10), 0.0, 1.0, AC), memo(11)]),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![g, memo(2)]);
    rig.go(0);
    rig.failed(200, 10);
    assert_eq!(rig.state(10), CueState::Broken);
    assert_eq!(rig.state(1), CueState::Running);
    let tick = rig.tick(1000);
    assert_eq!(running_at(&tick, 11), Some(1000));
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 1000)]);
    assert_eq!(rig.state(2), CueState::Idle);
}

#[test]
fn broken_child_never_auto_follows_inside_a_group() {
    let g = timed(
        grp(1, GO_NEXT, vec![timed(sound(10), 0.0, 0.0, AF), memo(11)]),
        0.0,
        2.0,
        AC,
    );
    let mut rig = Rig::new(vec![g, memo(2)]);
    rig.go(0);
    let failed = rig.failed(200, 10);
    assert_eq!(rig.state(11), CueState::Idle);
    // The group's action is over; its own auto-continue is time-based and still fires.
    assert_eq!(changes(&failed, 1), vec![(CueState::PostWaiting, 200)]);
    assert_eq!(running_at(&rig.tick(2000), 2), Some(2000));
}

// ----- model edits while a group runs -----

#[test]
fn deleting_the_last_busy_child_stops_the_group() {
    let g = timed(
        grp(1, TIMELINE, vec![sound(10), wait(11, 2.0)]),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![g, memo(2)]);
    rig.go(0);
    children_mut(&mut rig.cues_mut()[0]).remove(1);
    let tick = rig.tick(500);
    assert_eq!(changes(&tick, 11), vec![(CueState::Idle, 500)]);
    assert_eq!(rig.state(1), CueState::Running);
    children_mut(&mut rig.cues_mut()[0]).clear();
    let tick = rig.tick(600);
    assert_eq!(stopped_voices(&tick), vec![id(10)]);
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 600)]);
    assert_eq!(rig.state(2), CueState::Idle);
}

#[test]
fn deleting_a_running_group_stops_its_children() {
    let mut rig = staged_rig();
    rig.cues_mut().remove(0);
    let tick = rig.tick(600);
    let mut stopped = stopped_voices(&tick);
    stopped.sort();
    assert_eq!(stopped, vec![id(10), id(130)]);
    for n in [1, 10, 11, 12, 13, 130] {
        assert_eq!(rig.engine.state(id(n)), CueState::Idle);
    }
    assert_eq!(rig.engine.cue_states().count(), 0);
}

#[test]
fn child_added_to_a_running_timeline_is_not_started() {
    let mut rig = Rig::new(vec![grp(1, TIMELINE, vec![wait(10, 1.0)])]);
    rig.go(0);
    children_mut(&mut rig.cues_mut()[0]).push(wait(11, 5.0));
    assert!(starts(&rig.tick(500)).is_empty());
    assert_eq!(changes(&rig.tick(1000), 1), vec![(CueState::Idle, 1000)]);
    assert_eq!(rig.state(11), CueState::Idle);
}

#[test]
fn moving_the_busy_child_out_of_a_group_ends_the_group() {
    let g = timed(grp(1, TIMELINE, vec![sound(10)]), 0.0, 0.0, AF);
    let mut rig = Rig::new(vec![g, memo(2)]);
    rig.go(0);
    let c = children_mut(&mut rig.cues_mut()[0]).remove(0);
    rig.cues_mut().push(c);
    let tick = rig.tick(500);
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 500)]);
    assert_eq!(rig.state(10), CueState::Running);
    assert_eq!(rig.state(2), CueState::Idle);
}

// ----- robustness (regressions found by the property test) -----

#[test]
fn load_on_a_group_holding_its_own_id_terminates() {
    // Invalid workspace: a child shares its group's id.
    let g = grp(1, TIMELINE, vec![memo(1), sound(10)]);
    let mut rig = Rig::new(vec![g]);
    let load = rig.at(
        0,
        Input::Load {
            cue: id(1),
            time: 0.0,
        },
    );
    assert_eq!(rig.state(1), CueState::Loaded);
    assert_eq!(rig.state(10), CueState::Loaded);
    assert!(load.len() < 10);
    // Starting it does not keep itself running through its duplicate child.
    rig.start(10, 1);
    rig.ended(20, 10, 20);
    assert_eq!(rig.state(1), CueState::Idle);
}

#[test]
fn a_running_cue_whose_id_now_names_a_group_is_stopped() {
    // Duplicate ids: the sound runs, then is deleted, leaving a group with its id.
    let mut rig = Rig::new(vec![sound(6), grp(6, TIMELINE, vec![])]);
    rig.start(0, 6);
    assert_eq!(rig.state(6), CueState::Running);
    rig.cues_mut().remove(0);
    let tick = rig.tick(100);
    assert_eq!(stopped_voices(&tick), vec![id(6)]);
    assert_eq!(rig.state(6), CueState::Idle);
}

#[test]
fn go_on_a_duplicate_id_stays_on_playhead_positions() {
    // The playhead's id also occurs inside a group; the first occurrence (depth-first) is used.
    let inner = grp(1, GO_NEXT, vec![memo(2), memo(3)]);
    let mut rig = Rig::new(vec![inner, memo(2), memo(4)]);
    set_playhead(&mut rig, 2);
    rig.go(0);
    let p = rig.playhead();
    assert!(p.is_none_or(|p| CueEngine::playhead_positions(&rig.ws, list()).contains(&p)));
    assert_eq!(p, Some(id(3)));
}

#[test]
fn nesting_beyond_the_limit_does_not_panic() {
    // Validation refuses this; the engine must still cope (iterative walks).
    let mut c = sound(100_000);
    for depth in (1..200u128).rev() {
        c = grp(
            depth,
            if depth % 2 == 0 { TIMELINE } else { ENTER },
            vec![c],
        );
    }
    let mut rig = Rig::new(vec![c]);
    let go = rig.go(0);
    assert_eq!(voice_starts(&go), vec![(id(100_000), 0, 0)]);
    rig.at(10, Input::Pause { cue: id(1) });
    rig.at(20, Input::Resume { cue: id(1) });
    rig.stop(30, 1);
    assert_eq!(rig.engine.cue_states().count(), 0);
}

// ----- fix round 1 (M2-B review) -----

fn fade(seconds: f64) -> Option<FadeSpec> {
    Some(FadeSpec {
        duration: seconds,
        curve: FadeCurve::Linear,
    })
}

#[test]
fn fade_stop_on_a_pre_waiting_group_cancels_its_action() {
    // Review H1: the pre-wait stayed armed and started every child after the stop.
    let g = timed(grp(1, TIMELINE, vec![sound(10), sound(11)]), 1.0, 0.0, DNC);
    let mut rig = Rig::new(vec![g, memo(2)]);
    rig.go(0);
    rig.start(100, 10);
    let stop = rig.at(
        200,
        Input::Stop {
            cue: id(1),
            fade_out: fade(0.5),
        },
    );
    // The group stops at once; the child it already had fades out as usual.
    assert_eq!(changes(&stop, 1), vec![(CueState::Idle, 200)]);
    assert_eq!(rig.state(10), CueState::Stopping);
    assert_eq!(changes(&rig.tick(700), 10), vec![(CueState::Idle, 700)]);
    let tick = rig.tick(1000);
    assert!(voice_starts(&tick).is_empty(), "{tick:?}");
    assert_eq!(rig.engine.cue_states().count(), 0);
    assert_eq!(rig.engine.next_deadline(), None);
}

#[test]
fn fade_stop_on_a_pre_waiting_cue_cancels_its_action() {
    let mut rig = Rig::new(vec![timed(sound(1), 1.0, 0.0, AF), memo(2)]);
    rig.go(0);
    let stop = rig.at(
        200,
        Input::Stop {
            cue: id(1),
            fade_out: fade(0.5),
        },
    );
    assert_eq!(changes(&stop, 1), vec![(CueState::Idle, 200)]);
    assert!(rig.tick(1000).is_empty());
}

fn paused_with_end_in_flight(loop_: bool, fail: bool) -> (Rig, Vec<Action>) {
    // Review M1: a child's end (dated before the pause) arrives while the playlist is paused.
    let g = grp(
        1,
        playlist(loop_, false),
        vec![sound(10), sound(11), sound(12)],
    );
    let mut rig = Rig::new(vec![g]);
    rig.go(0);
    rig.at(1000, Input::Pause { cue: id(1) });
    let run = rig.engine.run_of(id(10)).unwrap();
    let input = if fail {
        Input::ActionFailed {
            cue: id(10),
            run,
            reason: BrokenReason::StreamFailed,
        }
    } else {
        Input::ActionEnded {
            cue: id(10),
            run,
            at: 990,
        }
    };
    let report = rig.at(1010, input);
    assert!(starts(&report).is_empty(), "nothing starts while paused");
    assert_eq!(rig.state(1), CueState::Paused);
    let resume = rig.at(2000, Input::Resume { cue: id(1) });
    (rig, resume)
}

#[test]
fn paused_looping_playlist_moves_on_at_resume_after_an_end_in_flight() {
    let (rig, resume) = paused_with_end_in_flight(true, false);
    assert_eq!(starts(&resume), vec![(id(11), 2000)]);
    assert_eq!(voice_starts(&resume), vec![(id(11), 2000, 0)]);
    assert_eq!(rig.state(1), CueState::Running);
}

#[test]
fn paused_playlist_moves_on_at_resume_after_a_failure_in_flight() {
    let (rig, resume) = paused_with_end_in_flight(true, true);
    assert_eq!(rig.state(10), CueState::Broken);
    assert_eq!(starts(&resume), vec![(id(11), 2000)]);
    assert_eq!(rig.state(1), CueState::Running);
}

#[test]
fn paused_non_looping_playlist_does_not_skip_its_remaining_children() {
    let (mut rig, resume) = paused_with_end_in_flight(false, false);
    assert_eq!(starts(&resume), vec![(id(11), 2000)]);
    rig.ended(3000, 11, 3000);
    assert_eq!(rig.state(12), CueState::Running);
    let end = rig.ended(4000, 12, 4000);
    assert_eq!(changes(&end, 1), vec![(CueState::Idle, 4000)]);
}

#[test]
fn go_on_a_disarmed_enter_group_skips_it_as_a_unit() {
    // Review M2: the playhead went inside a group that started nothing.
    let mut g = grp(1, ENTER, vec![memo(10), memo(11)]);
    g.armed = false;
    let mut rig = Rig::new(vec![g, memo(2)]);
    let go = rig.go(0);
    assert_eq!(starts(&go), vec![(id(1), 0)]);
    assert_eq!(rig.playhead(), Some(id(2)));
}

#[test]
fn shuffled_loop_never_repeats_a_child_across_a_pass_boundary() {
    // Review L2.
    for seed in 0..200u64 {
        let kids = (10..12).map(|n| wait(n, 1.0)).collect();
        let mut rig = Rig::seeded(vec![grp(1, playlist(true, true), kids)], seed);
        let mut all = rig.go(0);
        for t in 1..8u64 {
            all.extend(rig.tick(t * 1000));
        }
        let order: Vec<_> = starts(&all)
            .into_iter()
            .map(|(c, _)| c)
            .filter(|c| *c != id(1))
            .collect();
        assert_eq!(order.len(), 8);
        assert!(
            order.windows(2).all(|w| w[0] != w[1]),
            "seed {seed}: {order:?}"
        );
    }
}

// ----- fix round 2 (M2-B re-review) -----

/// Group 1 [10, 11] auto-follows into memo 2; 10 has ended, 11 plays. Group 1 is paused at 2000
/// while 11's end (or failure) report is in flight; returns the actions of the resume at 3000.
fn last_child_report_in_flight(mode: GroupMode, fail: bool) -> (Rig, Vec<Action>) {
    let g = timed(grp(1, mode, vec![sound(10), sound(11)]), 0.0, 0.0, AF);
    let mut rig = Rig::new(vec![g, memo(2)]);
    rig.go(0);
    if rig.state(10) == CueState::Running {
        rig.ended(1000, 10, 1000);
    }
    if rig.state(11) != CueState::Running {
        rig.start(1000, 11);
    }
    assert_eq!(rig.state(11), CueState::Running);
    rig.at(2000, Input::Pause { cue: id(1) });
    let run = rig.engine.run_of(id(11)).unwrap();
    let report = if fail {
        Input::ActionFailed {
            cue: id(11),
            run,
            reason: BrokenReason::StreamFailed,
        }
    } else {
        Input::ActionEnded {
            cue: id(11),
            run,
            at: 1990,
        }
    };
    rig.at(2010, report);
    let resume = rig.at(3000, Input::Resume { cue: id(1) });
    (rig, resume)
}

#[test]
fn paused_playlist_whose_last_child_ended_completes_and_auto_follows_on_resume() {
    // Re-review M1: the group was stopped instead, and cue 2 never fired.
    let (rig, resume) = last_child_report_in_flight(playlist(false, false), false);
    assert_eq!(changes(&resume, 1).last(), Some(&(CueState::Idle, 3000)));
    assert_eq!(running_at(&resume, 2), Some(3000));
    assert_eq!(rig.state(1), CueState::Idle);
}

#[test]
fn unpaused_playlist_whose_last_child_ended_auto_follows() {
    // Control for the case above.
    let g = timed(
        grp(1, playlist(false, false), vec![sound(10), sound(11)]),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![g, memo(2)]);
    rig.go(0);
    rig.ended(1000, 10, 1000);
    let end = rig.ended(2010, 11, 1990);
    assert_eq!(running_at(&end, 2), Some(1990));
}

#[test]
fn paused_playlist_whose_last_child_failed_completes_without_auto_follow() {
    let (rig, resume) = last_child_report_in_flight(playlist(false, false), true);
    assert_eq!(changes(&resume, 1).last(), Some(&(CueState::Idle, 3000)));
    assert_eq!(rig.state(2), CueState::Idle);
    assert_eq!(rig.state(11), CueState::Broken);
}

#[test]
fn paused_group_whose_last_child_failed_keeps_its_auto_continue() {
    let g = timed(
        grp(1, playlist(false, false), vec![sound(10)]),
        0.0,
        5.0,
        AC,
    );
    let mut rig = Rig::new(vec![g, memo(2)]);
    rig.go(0);
    rig.at(1000, Input::Pause { cue: id(1) });
    rig.failed(1010, 10);
    let resume = rig.at(2000, Input::Resume { cue: id(1) });
    // Action over at the resume; the post-wait (frozen 1000..2000) still fires at 6000.
    assert_eq!(
        changes(&resume, 1),
        vec![(CueState::Running, 2000), (CueState::PostWaiting, 2000)]
    );
    assert_eq!(running_at(&rig.tick(6000), 2), Some(6000));
}

#[test]
fn paused_random_group_whose_child_ended_completes_on_resume() {
    let r = timed(grp(1, RANDOM, vec![sound(10)]), 0.0, 0.0, AF);
    let mut rig = Rig::new(vec![r, memo(2)]);
    rig.go(0);
    rig.at(1000, Input::Pause { cue: id(1) });
    let run = rig.engine.run_of(id(10)).unwrap();
    rig.at(
        1010,
        Input::ActionEnded {
            cue: id(10),
            run,
            at: 990,
        },
    );
    let resume = rig.at(2000, Input::Resume { cue: id(1) });
    assert_eq!(running_at(&resume, 2), Some(2000));
}

#[test]
fn paused_nested_playlist_completing_on_resume_moves_the_outer_one_on() {
    let inner = grp(5, playlist(false, false), vec![sound(10)]);
    let outer = timed(
        grp(1, playlist(false, false), vec![inner, sound(20)]),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![outer, memo(2)]);
    rig.go(0);
    assert_eq!(rig.state(10), CueState::Running);
    rig.at(1000, Input::Pause { cue: id(1) });
    let run = rig.engine.run_of(id(10)).unwrap();
    rig.at(
        1010,
        Input::ActionEnded {
            cue: id(10),
            run,
            at: 990,
        },
    );
    let resume = rig.at(2000, Input::Resume { cue: id(1) });
    assert_eq!(changes(&resume, 5).last(), Some(&(CueState::Idle, 2000)));
    assert_eq!(starts(&resume), vec![(id(20), 2000)]);
    assert_eq!(rig.state(1), CueState::Running);
    let end = rig.ended(3000, 20, 3000);
    assert_eq!(running_at(&end, 2), Some(3000));
}
