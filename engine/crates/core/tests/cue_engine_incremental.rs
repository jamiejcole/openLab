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
        a
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

#[test]
fn an_edit_costs_what_it_touched() {
    // 2,000 audio cues, and a fade on cue 1,500.
    let mut cues: Vec<Cue> = (0..2000)
        .map(|i| sound(100 + i, &format!("media/f{i}.wav")))
        .collect();
    cues.push(cue(5000, fade(1500)));
    let mut twin = Twin::new(cues);
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
