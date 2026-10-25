// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! One test per validation rule (NFR-03, CUE-01, CUE-06, CUE-08).

mod common;

use common::*;
use openlab_core::*;

fn issue(severity: Severity, cue: Option<u128>, kind: IssueKind) -> ValidationIssue {
    ValidationIssue {
        severity,
        cue_id: cue.map(cue_id),
        kind,
    }
}

fn warning(cue: u128, kind: IssueKind) -> ValidationIssue {
    issue(Severity::Warning, Some(cue), kind)
}

fn refused(ws: &Workspace) -> Vec<ValidationIssue> {
    match load_show(&show_text(ws)) {
        Err(LoadError::Invalid(issues)) => issues,
        other => panic!("expected refusal, got {other:?}"),
    }
}

#[test]
fn clean_workspace_has_no_issues() {
    let ws = workspace(vec![
        numbered(1, "1", audio()),
        numbered(2, "2", fade(1)),
        cue(
            3,
            group(vec![cue(
                4,
                CueKind::Start {
                    target: Some(cue_id(1)),
                },
            )]),
        ),
    ]);
    assert_eq!(validate(&ws), vec![]);
}

#[test]
fn duplicate_cue_id_in_nested_group_is_refused() {
    let ws = workspace(vec![
        cue(1, CueKind::Memo),
        cue(2, group(vec![cue(1, CueKind::Memo)])),
    ]);
    let expected = vec![issue(Severity::Error, Some(1), IssueKind::DuplicateCueId)];
    assert_eq!(validate(&ws), expected);
    assert_eq!(refused(&ws), expected);
    assert!(matches!(
        save_show(&ws),
        Err(SaveError::WouldNotLoad(LoadError::Invalid(_)))
    ));
}

#[test]
fn duplicate_cue_id_across_lists_is_refused() {
    let mut ws = workspace(vec![cue(1, CueKind::Memo)]);
    ws.cue_lists.push(CueList {
        id: list_id(2),
        name: "B".into(),
        cues: vec![cue(1, CueKind::Memo)],
    });
    assert_eq!(
        refused(&ws),
        vec![issue(Severity::Error, Some(1), IssueKind::DuplicateCueId)]
    );
}

#[test]
fn duplicate_list_and_patch_ids_are_refused() {
    let mut ws = workspace(vec![]);
    ws.cue_lists.push(ws.cue_lists[0].clone());
    ws.patches.push(ws.patches[0].clone());
    ws.network_patches.push(ws.network_patches[0].clone());
    assert_eq!(
        refused(&ws),
        vec![
            issue(
                Severity::Error,
                None,
                IssueKind::DuplicateCueListId { id: list_id(1) }
            ),
            issue(
                Severity::Error,
                None,
                IssueKind::DuplicatePatchId { id: patch_id(1) }
            ),
            issue(
                Severity::Error,
                None,
                IssueKind::DuplicateNetworkPatchId { id: net_id(1) }
            ),
        ]
    );
}

#[test]
fn duplicate_cue_number_clears_the_later_cue_in_tree_order() {
    let mut ws = workspace(vec![
        numbered(1, "1", CueKind::Memo),
        numbered(
            2,
            "2",
            group(vec![
                numbered(3, "1", CueKind::Memo),
                numbered(4, "3", CueKind::Memo),
            ]),
        ),
    ]);
    ws.cue_lists.push(CueList {
        id: list_id(2),
        name: "B".into(),
        cues: vec![numbered(5, "3", CueKind::Memo)],
    });
    let loaded = load_show(&show_text(&ws)).unwrap();
    assert_eq!(
        loaded.warnings,
        vec![
            warning(
                3,
                IssueKind::DuplicateCueNumber {
                    number: "1".into(),
                    first: cue_id(1)
                }
            ),
            warning(
                5,
                IssueKind::DuplicateCueNumber {
                    number: "3".into(),
                    first: cue_id(4)
                }
            ),
        ]
    );
    let number = |n| loaded.workspace.find_cue(cue_id(n)).unwrap().number.clone();
    assert_eq!(number(1).as_deref(), Some("1"));
    assert_eq!(number(3), None);
    assert_eq!(number(4).as_deref(), Some("3"));
    assert_eq!(number(5), None);
}

#[test]
fn empty_cue_number_becomes_unset_silently() {
    let mut ws = workspace(vec![
        numbered(1, "", CueKind::Memo),
        numbered(2, "", CueKind::Memo),
    ]);
    assert_eq!(repair(&mut ws), vec![]);
    assert_eq!(ws.find_cue(cue_id(1)).unwrap().number, None);
    assert_eq!(ws.find_cue(cue_id(2)).unwrap().number, None);
}

#[test]
fn missing_target_warns_and_keeps_the_target() {
    let mut ws = workspace(vec![cue(
        1,
        group(vec![cue(
            2,
            CueKind::Start {
                target: Some(cue_id(99)),
            },
        )]),
    )]);
    assert_eq!(
        repair(&mut ws),
        vec![warning(2, IssueKind::TargetMissing { target: cue_id(99) })]
    );
    assert_eq!(
        ws.find_cue(cue_id(2)).unwrap().kind.target(),
        Some(cue_id(99))
    );
}

#[test]
fn wrong_target_kind_warns() {
    let ws = workspace(vec![
        cue(1, CueKind::Memo),
        cue(2, group(vec![cue(3, fade(1))])),
        cue(
            4,
            CueKind::Devamp {
                target: Some(cue_id(2)),
                start_next_cue: false,
                stop_target_when_slice_ends: false,
            },
        ),
    ]);
    assert_eq!(
        validate(&ws),
        vec![
            warning(3, IssueKind::TargetWrongKind { target: cue_id(1) }),
            warning(4, IssueKind::TargetWrongKind { target: cue_id(2) }),
        ]
    );
}

#[test]
fn fade_may_target_a_group_and_start_may_target_anything() {
    let ws = workspace(vec![
        cue(1, CueKind::Memo),
        cue(2, group(vec![cue(3, audio())])),
        cue(4, fade(2)),
        cue(
            5,
            CueKind::Start {
                target: Some(cue_id(1)),
            },
        ),
    ]);
    assert_eq!(validate(&ws), vec![]);
}

#[test]
fn self_target_warns() {
    let ws = workspace(vec![cue(
        1,
        group(vec![cue(
            2,
            CueKind::GoTo {
                target: Some(cue_id(2)),
                then_go: true,
            },
        )]),
    )]);
    assert_eq!(validate(&ws), vec![warning(2, IssueKind::TargetSelf)]);
}

#[test]
fn unset_target_is_not_a_load_warning() {
    let ws = workspace(vec![cue(
        1,
        CueKind::Stop {
            target: None,
            fade_out: no_fade(),
        },
    )]);
    assert_eq!(validate(&ws), vec![]);
}

#[test]
fn missing_patches_warn() {
    let mut a = audio();
    if let CueKind::Audio { patch, .. } = &mut a {
        *patch = patch_id(9);
    }
    let net = CueKind::Network {
        destination: net_id(9),
        address: "/go".into(),
        args: vec![],
    };
    let ws = workspace(vec![cue(1, group(vec![cue(2, a), cue(3, net)]))]);
    assert_eq!(
        validate(&ws),
        vec![
            warning(2, IssueKind::PatchMissing { patch: patch_id(9) }),
            warning(3, IssueKind::NetworkPatchMissing { patch: net_id(9) }),
        ]
    );
}

#[test]
fn invalid_times_are_set_to_zero() {
    let mut memo = cue(1, CueKind::Memo);
    memo.pre_wait = -1.0;
    memo.post_wait = f64::NAN;
    let mut ws = workspace(vec![
        memo,
        cue(
            2,
            group(vec![cue(
                3,
                CueKind::Wait {
                    duration: f64::INFINITY,
                },
            )]),
        ),
        cue(
            4,
            CueKind::Load {
                target: Some(cue_id(1)),
                load_to_time: -0.5,
            },
        ),
        cue(
            5,
            CueKind::Stop {
                target: None,
                fade_out: FadeSpec {
                    duration: -2.0,
                    curve: FadeCurve::Linear,
                },
            },
        ),
    ]);
    ws.settings.panic_duration = f64::NEG_INFINITY;
    let time = |field| IssueKind::InvalidTime { field };
    assert_eq!(
        repair(&mut ws),
        vec![
            issue(Severity::Warning, None, time("panic_duration")),
            warning(1, time("pre_wait")),
            warning(1, time("post_wait")),
            warning(3, time("duration")),
            warning(4, time("load_to_time")),
            warning(5, time("fade_out.duration")),
        ]
    );
    let memo = ws.find_cue(cue_id(1)).unwrap();
    assert_eq!((memo.pre_wait, memo.post_wait), (0.0, 0.0));
    assert_eq!(
        ws.find_cue(cue_id(3)).unwrap().kind,
        CueKind::Wait { duration: 0.0 }
    );
    assert_eq!(ws.settings.panic_duration, 0.0);
}

#[test]
fn invalid_audio_times_and_levels_are_clamped() {
    let mut a = audio();
    if let CueKind::Audio {
        start_time,
        fade_in,
        slices,
        levels,
        ..
    } = &mut a
    {
        *start_time = -1.0;
        fade_in.duration = f64::NAN;
        slices.push(SliceMarker {
            time: -3.0,
            play_count: 2,
        });
        levels.master_db = f64::NAN;
        levels.output_db = vec![f64::INFINITY, 20.0, -200.0, f64::NEG_INFINITY, -6.0];
        levels.crosspoints.push(Crosspoint {
            input: 0,
            output: 0,
            db: 13.0,
        });
    }
    let mut ws = workspace(vec![cue(1, group(vec![cue(2, a)]))]);
    let time = |field| warning(2, IssueKind::InvalidTime { field });
    let level = |field| warning(2, IssueKind::InvalidLevel { field });
    assert_eq!(
        repair(&mut ws),
        vec![
            time("start_time"),
            time("fade_in.duration"),
            time("slices.time"),
            level("levels.master_db"),
            level("levels.output_db"),
            level("levels.output_db"),
            level("levels.crosspoints.db"),
        ]
    );
    let CueKind::Audio {
        start_time,
        fade_in,
        slices,
        levels,
        ..
    } = &ws.find_cue(cue_id(2)).unwrap().kind
    else {
        panic!("not audio");
    };
    assert_eq!(
        (*start_time, fade_in.duration, slices[0].time),
        (0.0, 0.0, 0.0)
    );
    assert_eq!(levels.master_db, SILENCE_DB);
    assert_eq!(
        levels.output_db,
        vec![MAX_DB, MAX_DB, SILENCE_DB, SILENCE_DB, -6.0]
    );
    assert_eq!(levels.crosspoints[0].db, MAX_DB);
}

#[test]
fn invalid_fade_cue_values_are_clamped() {
    let mut f = fade(2);
    if let CueKind::Fade {
        duration, levels, ..
    } = &mut f
    {
        *duration = -5.0;
        levels.master_db = Some(f64::NAN);
        levels.output_db = vec![None, Some(99.0)];
    }
    let mut ws = workspace(vec![cue(1, f), cue(2, audio())]);
    assert_eq!(
        repair(&mut ws),
        vec![
            warning(1, IssueKind::InvalidTime { field: "duration" }),
            warning(
                1,
                IssueKind::InvalidLevel {
                    field: "levels.master_db"
                }
            ),
            warning(
                1,
                IssueKind::InvalidLevel {
                    field: "levels.output_db"
                }
            ),
        ]
    );
    let CueKind::Fade { levels, .. } = &ws.find_cue(cue_id(1)).unwrap().kind else {
        panic!("not fade");
    };
    assert_eq!(levels.master_db, Some(SILENCE_DB));
    assert_eq!(levels.output_db, vec![None, Some(MAX_DB)]);
}

#[test]
fn end_time_not_after_start_is_cleared() {
    let with_trim = |n, start: f64, end: f64| {
        let mut a = audio();
        if let CueKind::Audio {
            start_time,
            end_time,
            ..
        } = &mut a
        {
            *start_time = start;
            *end_time = Some(end);
        }
        cue(n, a)
    };
    let mut ws = workspace(vec![
        with_trim(1, 5.0, 5.0),
        with_trim(2, 1.0, f64::NAN),
        with_trim(3, 1.0, 2.0),
    ]);
    assert_eq!(
        repair(&mut ws),
        vec![
            warning(1, IssueKind::InvalidEndTime),
            warning(2, IssueKind::InvalidEndTime)
        ]
    );
    let end = |n| match ws.find_cue(cue_id(n)).unwrap().kind {
        CueKind::Audio { end_time, .. } => end_time,
        _ => panic!("not audio"),
    };
    assert_eq!((end(1), end(2), end(3)), (None, None, Some(2.0)));
}

fn nested(depth: usize) -> Cue {
    let mut c = cue(depth as u128, CueKind::Memo);
    for n in (1..depth).rev() {
        c = cue(n as u128, group(vec![c]));
    }
    c
}

#[test]
fn nesting_beyond_the_limit_is_refused() {
    let ws = workspace(vec![nested(MAX_GROUP_DEPTH + 2)]);
    assert_eq!(
        refused(&ws),
        vec![issue(
            Severity::Error,
            Some(MAX_GROUP_DEPTH as u128 + 1),
            IssueKind::NestingTooDeep
        )]
    );
}

#[test]
fn nesting_at_the_limit_saves_and_loads() {
    let ws = workspace(vec![nested(MAX_GROUP_DEPTH)]);
    let loaded = load_show(&save_show(&ws).unwrap()).unwrap();
    assert_eq!(loaded.warnings, vec![]);
    assert_eq!(loaded.workspace, ws);
}

#[test]
fn validate_leaves_the_workspace_unchanged() {
    let mut memo = numbered(2, "1", CueKind::Memo);
    memo.pre_wait = -1.0;
    let ws = workspace(vec![numbered(1, "1", CueKind::Memo), memo]);
    let before = ws.clone();
    assert_eq!(validate(&ws).len(), 2);
    assert_eq!(ws, before);
}

#[test]
fn find_cue_searches_every_list_and_depth() {
    let mut ws = workspace(vec![cue(
        1,
        group(vec![cue(2, group(vec![cue(3, CueKind::Memo)]))]),
    )]);
    ws.cue_lists.push(CueList {
        id: list_id(2),
        name: "B".into(),
        cues: vec![cue(4, CueKind::Memo)],
    });
    for n in 1..=4 {
        assert_eq!(ws.find_cue(cue_id(n)).unwrap().id, cue_id(n));
    }
    assert!(ws.find_cue(cue_id(5)).is_none());
}

#[test]
fn issues_render_as_readable_text() {
    let text = warning(
        3,
        IssueKind::DuplicateCueNumber {
            number: "1".into(),
            first: cue_id(1),
        },
    )
    .to_string();
    assert!(
        text.contains("\"1\"") && text.contains(&cue_id(3).0.to_string()),
        "{text}"
    );
}
