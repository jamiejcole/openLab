// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The incremental static-problem bookkeeping (M4.12a): an engine told only an edit's footprint
//! (`model_changed_cues`) behaves exactly like one that rebuilds everything (`model_changed`),
//! at a cost that follows the edit. The random-sequence version of this is
//! `commands_prop::incremental_problems_equal_a_full_rebuild`; these are the named cases,
//! above all the dependencies between cues (a problem on one cue caused by an edit to another).

mod common;

use common::*;
use openlab_core::*;

const RATE: u32 = 1000;

fn id(n: u128) -> CueId {
    cue_id(n)
}

fn sound(n: u128, file: &str) -> Cue {
    let mut c = cue(n, audio());
    if let CueKind::Audio { media, .. } = &mut c.kind {
        *media = Some(MediaRef::new(file));
    }
    c
}

/// Two engines on one workspace: `inc` is told footprints, `full` rebuilds everything. Every
/// call is checked to give both the same actions and `inc` to equal a from-scratch rebuild.
struct Twin {
    ws: Workspace,
    history: History,
    inc: CueEngine,
    full: CueEngine,
    now: SampleTime,
}

impl Twin {
    fn new(cues: Vec<Cue>) -> Self {
        let config = EngineConfig::new(RATE, 0, 1).unwrap();
        let mut twin = Self {
            ws: workspace(cues),
            history: History::new(),
            inc: CueEngine::new(config),
            full: CueEngine::new(config),
            now: 0,
        };
        twin.input(Input::Tick);
        twin
    }

    fn input(&mut self, input: Input) -> Vec<Action> {
        self.now += 10;
        let a = self.inc.handle(&self.ws, input.clone(), self.now);
        let b = self.full.handle(&self.ws, input, self.now);
        assert_eq!(a, b, "the same actions as a full rebuild");
        assert_eq!(self.inc.unsignalled_model_changes(), 0);
        self.inc.verify_problem_cache(&self.ws).unwrap();
        assert_eq!(self.inc.broken_cues(), self.full.broken_cues());
        for list in &self.ws.cue_lists {
            assert_eq!(self.inc.playhead(list.id), self.full.playhead(list.id));
        }
        a
    }

    /// `input` at sample `at` (not before the last call).
    fn at(&mut self, at: SampleTime, input: Input) -> Vec<Action> {
        self.now = at.max(self.now + 10) - 10;
        self.input(input)
    }

    fn signal(&mut self, footprint: &Footprint) -> Vec<Action> {
        self.inc.model_changed_cues(footprint);
        self.full.model_changed();
        self.input(Input::Tick)
    }

    fn edit(&mut self, command: Command) -> Vec<Action> {
        let footprint = command.footprint();
        self.history.execute(&mut self.ws, command).unwrap();
        self.signal(&footprint)
    }

    /// An edit signalled without a call: the next one adds its footprint to this one's.
    fn edit_uncalled(&mut self, command: Command) {
        let footprint = command.footprint();
        self.history.execute(&mut self.ws, command).unwrap();
        self.inc.model_changed_cues(&footprint);
        self.full.model_changed();
    }

    fn playhead(&self) -> Option<CueId> {
        self.inc.playhead(list_id(1))
    }

    fn undo(&mut self) -> Vec<Action> {
        let footprint = self.history.peek_undo().unwrap().footprint();
        assert!(self.history.undo(&mut self.ws).unwrap());
        self.signal(&footprint)
    }

    fn problems(&self, n: u128) -> Vec<CueProblem> {
        self.inc.problems(id(n)).to_vec()
    }
}

fn set(n: u128, property: CueProperty) -> Command {
    Command::SetCueProperty {
        id: id(n),
        property,
    }
}

fn problem_cues(actions: &[Action]) -> Vec<CueId> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::ProblemsChanged { cue, .. } => Some(*cue),
            _ => None,
        })
        .collect()
}

#[test]
fn editing_a_target_refreshes_the_cue_that_points_at_it() {
    // Cue 1 fades cue 2; cue 3 stops cue 2. Neither is in any edit's footprint below.
    let stop = CueKind::Stop {
        target: Some(id(2)),
        fade_out: no_fade(),
    };
    let mut twin = Twin::new(vec![
        cue(1, fade(2)),
        sound(2, "media/a.wav"),
        cue(3, stop),
        cue(4, CueKind::Memo),
    ]);
    assert!(twin.problems(1).is_empty());

    // The target becomes a memo: a fade cannot target it, a stop can.
    let actions = twin.edit(set(
        2,
        CueProperty::Kind {
            value: CueKind::Memo,
        },
    ));
    assert_eq!(
        twin.problems(1),
        vec![CueProblem::TargetWrongKind { target: id(2) }]
    );
    assert!(twin.problems(3).is_empty());
    assert_eq!(problem_cues(&actions), vec![id(1)]);

    // The target goes away: both now point at nothing.
    let actions = twin.edit(Command::RemoveCue { id: id(2) });
    assert_eq!(
        twin.problems(1),
        vec![CueProblem::TargetMissing { target: id(2) }]
    );
    assert_eq!(
        twin.problems(3),
        vec![CueProblem::TargetMissing { target: id(2) }]
    );
    assert_eq!(problem_cues(&actions), vec![id(1), id(3)]);

    // Undo puts it back (as the memo): the stop is fine again, the fade still is not.
    twin.undo();
    assert!(twin.problems(3).is_empty());
    assert_eq!(
        twin.problems(1),
        vec![CueProblem::TargetWrongKind { target: id(2) }]
    );
    // And back to audio.
    twin.undo();
    assert!(twin.problems(1).is_empty());
    assert!(twin.problems(3).is_empty());

    // A cue added with the id a broken cue points at repairs it.
    let mut twin = Twin::new(vec![cue(1, fade(9)), cue(4, CueKind::Memo)]);
    assert_eq!(
        twin.problems(1),
        vec![CueProblem::TargetMissing { target: id(9) }]
    );
    twin.edit(Command::AddCue {
        at: Position {
            list: list_id(1),
            parent: None,
            index: 1,
        },
        cue: sound(9, "media/a.wav"),
        auto_number: false,
    });
    assert!(twin.problems(1).is_empty());
}

#[test]
fn a_group_gone_or_added_refreshes_the_cues_that_target_its_descendants() {
    // The descendants are not in the footprint (a group's footprint names the group only).
    let inner = sound(12, "media/a.wav");
    let group = cue(10, group(vec![cue(11, CueKind::Memo), inner]));
    let mut twin = Twin::new(vec![cue(1, fade(12)), group]);
    assert!(twin.problems(1).is_empty());
    twin.edit(Command::RemoveCue { id: id(10) });
    assert_eq!(
        twin.problems(1),
        vec![CueProblem::TargetMissing { target: id(12) }]
    );
    twin.undo();
    assert!(twin.problems(1).is_empty());
}

#[test]
fn a_move_refreshes_nothing_but_keeps_the_groups_warnings_right() {
    // Cue 12 is broken (media missing) and in group 10; group 20 is empty.
    let mut twin = Twin::new(vec![
        cue(
            10,
            group(vec![cue(11, CueKind::Memo), sound(12, "media/x.wav")]),
        ),
        cue(20, group(vec![cue(21, CueKind::Memo)])),
        cue(
            30,
            group(vec![cue(31, group(vec![cue(32, CueKind::Memo)]))]),
        ),
    ]);
    twin.input(Input::MediaStatus {
        media: MediaRef::new("media/x.wav"),
        status: MediaStatus::Missing,
    });
    assert!(twin.inc.has_broken_descendant(id(10)));
    assert!(!twin.inc.has_broken_descendant(id(20)));

    let to = |parent, index| Position {
        list: list_id(1),
        parent,
        index,
    };
    // Into the other group: the warning moves with it.
    let actions = twin.edit(Command::ReparentCue {
        id: id(12),
        to: to(Some(id(20)), 0),
    });
    assert!(!twin.inc.has_broken_descendant(id(10)));
    assert!(twin.inc.has_broken_descendant(id(20)));
    assert!(actions.contains(&Action::GroupWarningChanged {
        group: id(10),
        warning: false
    }));
    assert!(actions.contains(&Action::GroupWarningChanged {
        group: id(20),
        warning: true
    }));
    // The group holding the broken cue moves into a nested one: every group above warns.
    twin.edit(Command::ReparentCue {
        id: id(20),
        to: to(Some(id(31)), 0),
    });
    assert!(twin.inc.has_broken_descendant(id(30)));
    assert!(twin.inc.has_broken_descendant(id(31)));
    assert!(twin.inc.has_broken_descendant(id(20)));
    // Removing the outer group takes the warnings with it, once.
    let actions = twin.edit(Command::RemoveCue { id: id(30) });
    for group in [30, 31, 20] {
        assert!(!twin.inc.has_broken_descendant(id(group)));
        assert!(actions.contains(&Action::GroupWarningChanged {
            group: id(group),
            warning: false
        }));
    }
    // And undoing it brings them all back.
    twin.undo();
    for group in [30, 31, 20] {
        assert!(twin.inc.has_broken_descendant(id(group)));
    }
}

#[test]
fn a_transaction_that_adds_edits_moves_and_removes_is_followed() {
    let mut twin = Twin::new(vec![
        cue(1, fade(2)),
        cue(2, group(vec![sound(3, "media/a.wav")])),
        cue(4, CueKind::Memo),
    ]);
    let at = |parent, index| Position {
        list: list_id(1),
        parent,
        index,
    };
    twin.edit(Command::Transaction {
        commands: vec![
            Command::AddCue {
                at: at(None, 0),
                cue: cue(5, group(vec![])),
                auto_number: false,
            },
            Command::ReparentCue {
                id: id(3),
                to: at(Some(id(5)), 0),
            },
            // The fade's target group is now empty and then removed.
            Command::RemoveCue { id: id(2) },
            Command::AddCue {
                at: at(Some(id(5)), 1),
                cue: sound(6, "media/missing.wav"),
                auto_number: false,
            },
            set(
                6,
                CueProperty::Name {
                    value: "six".into(),
                },
            ),
        ],
    });
    assert_eq!(
        twin.problems(1),
        vec![CueProblem::TargetMissing { target: id(2) }]
    );
    twin.input(Input::MediaStatus {
        media: MediaRef::new("media/missing.wav"),
        status: MediaStatus::Missing,
    });
    assert!(twin.inc.has_broken_descendant(id(5)));
    twin.undo();
    assert!(twin.problems(1).is_empty());
    assert!(!twin.inc.has_broken_descendant(id(5)));
}

fn broken_ids(twin: &Twin) -> Vec<CueId> {
    twin.inc
        .broken_cues()
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

#[test]
fn a_group_removed_and_added_again_under_its_id_is_followed_with_its_new_children() {
    // Group 10 { fade 11 without a target }, memo 1.
    let mut twin = Twin::new(vec![
        cue(10, group(vec![cue(11, fade(99))])),
        cue(1, CueKind::Memo),
    ]);
    assert_eq!(broken_ids(&twin), vec![id(11)]);
    let at = Position {
        list: list_id(1),
        parent: None,
        index: 0,
    };
    twin.edit(Command::Transaction {
        commands: vec![
            Command::RemoveCue { id: id(10) },
            Command::AddCue {
                at: at.clone(),
                cue: cue(10, group(vec![cue(12, fade(98))])),
                auto_number: false,
            },
        ],
    });
    // The old child is no longer broken, the new one is.
    assert_eq!(broken_ids(&twin), vec![id(12)]);
    // Undo puts the old group back: the same shape again.
    twin.undo();
    assert_eq!(broken_ids(&twin), vec![id(11)]);
    // The group becomes a plain cue under the same id: its child is gone with it.
    twin.edit(Command::Transaction {
        commands: vec![
            Command::RemoveCue { id: id(10) },
            Command::AddCue {
                at,
                cue: cue(10, CueKind::Memo),
                auto_number: false,
            },
        ],
    });
    assert!(broken_ids(&twin).is_empty());
}

fn readd(twin: &mut Twin, new: Cue) {
    twin.edit(Command::Transaction {
        commands: vec![
            Command::RemoveCue { id: new.id },
            Command::AddCue {
                at: Position {
                    list: list_id(1),
                    parent: None,
                    index: 0,
                },
                cue: new,
                auto_number: false,
            },
        ],
    });
}

#[test]
fn a_re_added_group_with_a_nested_group_is_new_all_the_way_down() {
    // Group 10 { group 11 { fade 12 without a target } }; the same ids come back with a
    // different fade below.
    let mut twin = Twin::new(vec![
        cue(10, group(vec![cue(11, group(vec![cue(12, fade(99))]))])),
        cue(1, CueKind::Memo),
    ]);
    assert_eq!(broken_ids(&twin), vec![id(12)]);
    readd(
        &mut twin,
        cue(10, group(vec![cue(11, group(vec![cue(13, fade(98))]))])),
    );
    assert_eq!(broken_ids(&twin), vec![id(13)]);
    twin.undo();
    assert_eq!(broken_ids(&twin), vec![id(12)]);
}

#[test]
fn a_re_added_group_whose_child_keeps_its_id_but_not_its_content_is_followed() {
    // Fade 11 targets audio cue 1; the group comes back with fade 11 aimed at a missing cue.
    let mut twin = Twin::new(vec![
        cue(10, group(vec![cue(11, fade(1))])),
        sound(1, "media/a.wav"),
    ]);
    assert!(broken_ids(&twin).is_empty());
    readd(&mut twin, cue(10, group(vec![cue(11, fade(98))])));
    assert_eq!(broken_ids(&twin), vec![id(11)]);
    assert_eq!(
        twin.problems(11),
        vec![CueProblem::TargetMissing { target: id(98) }]
    );
    twin.undo();
    assert!(broken_ids(&twin).is_empty());
}

#[test]
fn an_edit_costs_what_it_touched() {
    // 2,000 audio cues, and a fade on cue 1,500.
    let mut cues: Vec<Cue> = (0..2000)
        .map(|i| sound(100 + i, &format!("media/f{i}.wav")))
        .collect();
    cues.push(cue(5000, fade(1500)));
    let mut twin = Twin::new(cues);
    let passes = (twin.inc.whole_show_passes(), twin.full.whole_show_passes());
    // A playhead on a cue the edits below move.
    twin.input(Input::SetPlayhead {
        list: list_id(1),
        cue: Some(id(300)),
    });
    let checks = |twin: &Twin| twin.inc.problem_checks();
    let at = |index| Position {
        list: list_id(1),
        parent: None,
        index,
    };

    let before = checks(&twin);
    twin.edit(set(150, CueProperty::Name { value: "a".into() }));
    assert_eq!(checks(&twin), before + 1, "a rename: that cue");

    let before = checks(&twin);
    twin.edit(Command::BatchSet {
        ids: vec![id(200), id(201), id(202)],
        property: CueProperty::Notes { value: "n".into() },
    });
    assert_eq!(checks(&twin), before + 3, "a batch of three: those");

    let before = checks(&twin);
    twin.edit(set(1500, CueProperty::Name { value: "t".into() }));
    assert_eq!(checks(&twin), before + 2, "a target: it and the fade on it");

    let before = checks(&twin);
    twin.edit(Command::MoveCue {
        id: id(300),
        index: 1000,
    });
    assert_eq!(checks(&twin), before + 1, "a move: that cue");

    let before = checks(&twin);
    twin.edit(Command::AddCue {
        at: at(10),
        cue: sound(9000, "media/new.wav"),
        auto_number: false,
    });
    assert_eq!(checks(&twin), before + 1, "an add: the new cue");

    let before = checks(&twin);
    twin.edit(Command::RemoveCue { id: id(400) });
    assert_eq!(checks(&twin), before + 1, "a removal: the removed cue");

    let before = checks(&twin);
    twin.edit(Command::RemoveCue { id: id(1500) });
    assert_eq!(
        checks(&twin),
        before + 2,
        "a removed target: it and the fade"
    );
    assert_eq!(
        twin.problems(5000),
        vec![CueProblem::TargetMissing { target: id(1500) }]
    );
    assert_eq!(twin.inc.model_syncs(), twin.full.model_syncs());
    // M4.12c: the chain limit, the playhead and the cues present were followed without
    // walking the show (the full rebuild walks it on every edit).
    // (300 was moved to after 1,100.)
    twin.edit(Command::RemoveCue { id: id(300) });
    assert_eq!(twin.playhead(), Some(id(1101)));
    assert_eq!(twin.inc.whole_show_passes(), passes.0);
    assert!(twin.full.whole_show_passes() >= passes.1 + 9);
}

// ----- playheads, the chain limit and group visits followed from footprints (M4.12c) -----

use ContinueMode::{AutoContinue as AC, AutoFollow as AF};

fn memo(n: u128) -> Cue {
    cue(n, CueKind::Memo)
}

fn wait(n: u128, seconds: f64) -> Cue {
    cue(n, CueKind::Wait { duration: seconds })
}

fn timed(mut c: Cue, mode: ContinueMode) -> Cue {
    c.continue_mode = mode;
    c
}

fn grp(n: u128, mode: GroupMode, children: Vec<Cue>) -> Cue {
    cue(n, CueKind::Group { mode, children })
}

fn at(parent: Option<u128>, index: usize) -> Position {
    Position {
        list: list_id(1),
        parent: parent.map(id),
        index,
    }
}

fn set_playhead(twin: &mut Twin, n: u128) {
    twin.input(Input::SetPlayhead {
        list: list_id(1),
        cue: Some(id(n)),
    });
}

fn go(twin: &mut Twin, t: SampleTime) -> Vec<Action> {
    twin.at(t, Input::Go { list: list_id(1) })
}

#[test]
fn a_deleted_playhead_cue_gives_way_to_the_first_cue_after_it_that_is_left() {
    // 1, G2 { 3, 4 }, 5, 6, 7 with the playhead on 3: G2 and 5 go in one edit, so the cues
    // right after 3 are all gone with it and 6 is the first left.
    let mut twin = Twin::new(vec![
        memo(1),
        grp(2, GroupMode::Timeline, vec![memo(3), memo(4)]),
        memo(5),
        memo(6),
        memo(7),
    ]);
    set_playhead(&mut twin, 3);
    twin.edit(Command::Transaction {
        commands: vec![
            Command::RemoveCue { id: id(2) },
            Command::RemoveCue { id: id(5) },
        ],
    });
    assert_eq!(twin.playhead(), Some(id(6)));
    // Undo brings them back; the playhead stays where it is.
    twin.undo();
    assert_eq!(twin.playhead(), Some(id(6)));
    // The last cue deleted under the playhead: past the end.
    set_playhead(&mut twin, 7);
    twin.edit(Command::RemoveCue { id: id(7) });
    assert_eq!(twin.playhead(), None);
}

#[test]
fn a_deleted_playhead_cue_whose_next_cue_moved_lands_on_it_where_it_went() {
    // 1, 2, 3, 4 with the playhead on 2: 2 removed and 3 moved to the end in one edit. 3 is
    // still in the list, so it is the first cue after 2 that is left (wherever it is now).
    let mut twin = Twin::new(vec![memo(1), memo(2), memo(3), memo(4)]);
    set_playhead(&mut twin, 2);
    twin.edit(Command::Transaction {
        commands: vec![
            Command::RemoveCue { id: id(2) },
            Command::MoveCue {
                id: id(3),
                index: 2,
            },
        ],
    });
    assert_eq!(twin.playhead(), Some(id(3)));
}

#[test]
fn a_playhead_cue_moved_to_another_list_leaves_the_playhead_on_the_next_cue() {
    let mut twin = Twin::new(vec![memo(1), memo(2), memo(3)]);
    twin.ws.cue_lists.push(CueList {
        id: list_id(2),
        name: "B".into(),
        cues: vec![memo(10)],
    });
    // A new list is not a command: a full model change.
    twin.inc.model_changed();
    twin.full.model_changed();
    twin.input(Input::Tick);
    set_playhead(&mut twin, 2);
    twin.edit(Command::ReparentCue {
        id: id(2),
        to: Position {
            list: list_id(2),
            parent: None,
            index: 0,
        },
    });
    assert_eq!(twin.playhead(), Some(id(3)));
    assert_eq!(twin.inc.playhead(list_id(2)), Some(id(10)));
    // And back, onto the other list's playhead cue's place: neither playhead moves.
    twin.undo();
    assert_eq!(twin.playhead(), Some(id(3)));
    assert_eq!(twin.inc.playhead(list_id(2)), Some(id(10)));
}

#[test]
fn a_playhead_cue_removed_and_added_again_under_its_id_keeps_the_playhead() {
    // Added again elsewhere in its list (a transaction or its undo does this): still in the
    // list, so the playhead stays on it.
    let mut twin = Twin::new(vec![memo(1), memo(2), memo(3)]);
    set_playhead(&mut twin, 2);
    twin.edit(Command::Transaction {
        commands: vec![
            Command::RemoveCue { id: id(2) },
            Command::AddCue {
                at: at(None, 2),
                cue: memo(2),
                auto_number: false,
            },
        ],
    });
    assert_eq!(twin.playhead(), Some(id(2)));
    // Its group added again without it: the cue after the old group is next.
    let mut twin = Twin::new(vec![
        grp(1, GroupMode::Timeline, vec![memo(2), memo(3)]),
        memo(4),
    ]);
    set_playhead(&mut twin, 2);
    twin.edit(Command::Transaction {
        commands: vec![
            Command::RemoveCue { id: id(1) },
            Command::AddCue {
                at: at(None, 0),
                cue: grp(1, GroupMode::Timeline, vec![memo(5)]),
                auto_number: false,
            },
        ],
    });
    // In the old order 3 came next (gone with the group), then 4.
    assert_eq!(twin.playhead(), Some(id(4)));
}

#[test]
fn an_empty_list_that_gains_cues_stands_on_its_first_and_past_the_end_stays() {
    let mut twin = Twin::new(vec![]);
    assert_eq!(twin.playhead(), None);
    twin.edit(Command::AddCue {
        at: at(None, 0),
        cue: grp(1, GroupMode::Timeline, vec![memo(2)]),
        auto_number: false,
    });
    assert_eq!(twin.playhead(), Some(id(1)));
    // Past the end of a list that has cues: an added cue does not move it.
    twin.input(Input::SetPlayhead {
        list: list_id(1),
        cue: None,
    });
    twin.edit(Command::AddCue {
        at: at(None, 1),
        cue: memo(3),
        auto_number: false,
    });
    assert_eq!(twin.playhead(), None);
}

#[test]
fn edits_added_up_before_a_call_are_followed_as_one() {
    // Two removals signalled before the engine is called: the playhead cue and the cue after
    // it. The engine adds up the footprints and lands where a full rebuild does.
    let mut twin = Twin::new(vec![memo(1), memo(2), memo(3), memo(4)]);
    set_playhead(&mut twin, 2);
    twin.edit_uncalled(Command::RemoveCue { id: id(2) });
    twin.edit_uncalled(Command::RemoveCue { id: id(3) });
    twin.input(Input::Tick);
    assert_eq!(twin.playhead(), Some(id(4)));
}

#[test]
fn deleting_the_standby_cue_inside_a_group_keeps_the_visit() {
    // M2.24 H1, followed from the footprint: the playhead moves to 12, still inside the group
    // whose auto-follow has played 2, so GO from 12 passes over 2.
    let g = timed(
        grp(
            1,
            GroupMode::StartFirstAndEnter,
            vec![wait(10, 1.0), memo(11), memo(12)],
        ),
        AF,
    );
    let mut twin = Twin::new(vec![g, memo(2), memo(3)]);
    go(&mut twin, 100);
    assert_eq!(twin.playhead(), Some(id(11)));
    twin.at(1200, Input::Tick);
    twin.edit(Command::RemoveCue { id: id(11) });
    assert_eq!(twin.playhead(), Some(id(12)));
    go(&mut twin, 1400);
    assert_eq!(twin.playhead(), Some(id(3)));
}

#[test]
fn a_standby_cue_dragged_into_a_group_enters_it_afresh() {
    // M2.24 H1, followed from the footprint: the playlist ran (and played 2 by its
    // auto-follow), then the standby cue is dragged into it. A fresh visit: the group does not
    // run in it, so GO from 12 stands on 2.
    let p = timed(
        grp(
            1,
            GroupMode::Playlist {
                loop_: false,
                shuffle: false,
            },
            vec![wait(10, 1.0), wait(11, 1.0)],
        ),
        AF,
    );
    let mut twin = Twin::new(vec![p, memo(2), memo(3), wait(12, 1.0)]);
    go(&mut twin, 100);
    twin.at(1200, Input::Tick);
    twin.at(2300, Input::Tick);
    set_playhead(&mut twin, 12);
    twin.edit(Command::ReparentCue {
        id: id(12),
        to: at(Some(1), 2),
    });
    assert_eq!(twin.playhead(), Some(id(12)));
    go(&mut twin, 2600);
    assert_eq!(twin.playhead(), Some(id(2)));
}

#[test]
fn a_cue_moved_into_the_group_just_before_it_enters_it() {
    // M2.24 H1, followed from the footprint: [G [10], 12, 2, 3] → [G [10, 12], 2, 3] keeps
    // every cue's place in the order, but the playhead on 12 is now inside G: a fresh visit,
    // G's continue does not fire, so GO from 12 stands on 2.
    let g = timed(grp(1, GroupMode::Timeline, vec![memo(10)]), AC);
    let mut twin = Twin::new(vec![g, memo(12), memo(2), memo(3)]);
    go(&mut twin, 100);
    set_playhead(&mut twin, 12);
    twin.edit(Command::ReparentCue {
        id: id(12),
        to: at(Some(1), 1),
    });
    go(&mut twin, 300);
    assert_eq!(twin.playhead(), Some(id(2)));
}

#[test]
fn the_chain_limit_follows_cues_added_and_removed_by_footprint() {
    // Cue 1 starts cue 2, which starts cue 1: a cycle, cut at the chain limit (the cue count
    // plus a margin). The count follows each edit from its footprint.
    let start = |n: u128, target: u128| {
        timed(
            cue(
                n,
                CueKind::Start {
                    target: Some(id(target)),
                },
            ),
            AC,
        )
    };
    let mut twin = Twin::new(vec![start(1, 2), start(2, 1)]);
    let limit = |actions: &[Action]| {
        actions.iter().find_map(|a| match a {
            Action::Warning(EngineWarning::ChainLimit { limit, .. }) => Some(*limit),
            _ => None,
        })
    };
    let first = limit(&go(&mut twin, 100)).unwrap();
    twin.edit(Command::AddCue {
        at: at(None, 2),
        cue: grp(3, GroupMode::Timeline, vec![memo(4), memo(5)]),
        auto_number: false,
    });
    twin.input(Input::SetPlayhead {
        list: list_id(1),
        cue: Some(id(1)),
    });
    assert_eq!(limit(&go(&mut twin, 1000)), Some(first + 3));
    twin.edit(Command::RemoveCue { id: id(3) });
    twin.input(Input::SetPlayhead {
        list: list_id(1),
        cue: Some(id(1)),
    });
    assert_eq!(limit(&go(&mut twin, 2000)), Some(first));
}

#[test]
fn a_footprint_that_misses_a_cue_is_caught_and_repaired() {
    // The contract: the footprint names every cue the edit changed. One that does not is
    // counted (debug builds, and a cue about to be refused), and the next call is right.
    let mut ws = workspace(vec![cue(1, fade(2)), sound(2, "media/a.wav")]);
    let mut engine = CueEngine::new(EngineConfig::new(RATE, 0, 1).unwrap());
    engine.handle(&ws, Input::Tick, 0);
    ws.cue_lists[0].cues[1].kind = CueKind::Memo;
    engine.model_changed_cues(&Footprint::edited([id(7)]));
    engine.handle(&ws, Input::Tick, 10);
    engine.handle(&ws, Input::Tick, 20);
    if cfg!(debug_assertions) {
        assert_eq!(engine.unsignalled_model_changes(), 1);
        engine.verify_problem_cache(&ws).unwrap();
        assert_eq!(
            engine.problems(id(1)),
            [CueProblem::TargetWrongKind { target: id(2) }]
        );
    }
}

#[test]
fn a_changed_patch_set_or_cue_list_set_rebuilds_everything() {
    let mut ws = workspace(vec![sound(1, "media/a.wav")]);
    let mut engine = CueEngine::new(EngineConfig::new(RATE, 0, 1).unwrap());
    engine.handle(&ws, Input::Tick, 0);
    assert!(engine.problems(id(1)).is_empty());
    // Patches are not in any footprint: the engine notices and rebuilds.
    ws.patches.clear();
    engine.model_changed_cues(&Footprint::default());
    engine.handle(&ws, Input::Tick, 10);
    assert_eq!(
        engine.problems(id(1)),
        [CueProblem::PatchMissing { patch: patch_id(1) }]
    );
    engine.verify_problem_cache(&ws).unwrap();
    assert_eq!(engine.unsignalled_model_changes(), 0);
    // A new cue list likewise.
    ws.cue_lists.push(CueList {
        id: list_id(2),
        name: "B".into(),
        cues: vec![sound(2, "media/a.wav")],
    });
    engine.model_changed_cues(&Footprint::default());
    engine.handle(&ws, Input::Tick, 20);
    engine.verify_problem_cache(&ws).unwrap();
    assert_eq!(engine.unsignalled_model_changes(), 0);
    assert_eq!(
        engine.problems(id(2)),
        [CueProblem::PatchMissing { patch: patch_id(1) }]
    );
}
