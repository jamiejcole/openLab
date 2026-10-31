// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Memory bounds on what one command can put in a cue: a Network cue's message on every path
//! that creates or replaces one (add, paste, clipboard paste, kind change), and the cue's name
//! and notes (docs/commands.md, "Size limits"). A refused edit changes nothing; a show file
//! that already holds an over-limit value still opens, with a warning.

mod common;

use common::*;
use openlab_core::*;

/// One Memo cue; `workspace` has network patch 1.
fn ws() -> Workspace {
    workspace(vec![cue(1, CueKind::Memo)])
}

fn at_end() -> Position {
    Position {
        list: list_id(1),
        parent: None,
        index: 1,
    }
}

/// Network kinds over each bound: address, argument count, string argument.
fn oversize_kinds() -> Vec<CueKind> {
    let kind = |address: String, args: Vec<OscArg>| CueKind::Network {
        destination: net_id(1),
        address,
        args,
    };
    vec![
        kind(format!("/{}", "a".repeat(64 * 1024)), vec![]),
        kind("/go".into(), vec![OscArg::Int { value: 0 }; 1025]),
        kind(
            "/go".into(),
            vec![OscArg::String {
                value: "x".repeat(64 * 1024 + 1),
            }],
        ),
    ]
}

fn with_kind(n: u128, kind: CueKind) -> Cue {
    let mut c = cue(n, CueKind::Memo);
    c.kind = kind;
    c
}

fn prepared(ws: &Workspace, command: Command) -> Result<Command, CommandError> {
    let mut next = 1000;
    command.prepare(
        ws,
        &MediaPaths {
            show_dir: None,
            style: PathStyle::Posix,
        },
        &mut || {
            next += 1;
            cue_id(next)
        },
    )
}

/// Applies `command` (prepared, as the binary does) through `History` and expects a refusal
/// that leaves the workspace and the history as they were.
fn assert_refused(ws: &Workspace, command: Command) {
    let mut work = ws.clone();
    let mut history = History::new();
    let result = prepared(&work, command)
        .and_then(|command| history.execute(&mut work, command).map(|_| ()));
    assert!(result.is_err(), "accepted");
    assert_eq!(&work, ws);
    assert!(!history.can_undo());
}

#[test]
fn an_oversize_network_message_is_refused_on_every_path_that_makes_one() {
    let w = ws();
    for kind in oversize_kinds() {
        let new = with_kind(5, kind.clone());
        assert_refused(
            &w,
            Command::AddCue {
                at: at_end(),
                cue: new.clone(),
                auto_number: false,
            },
        );
        assert_refused(
            &w,
            Command::Paste {
                at: at_end(),
                cues: vec![new.clone()],
            },
        );
        assert_refused(
            &w,
            Command::PasteClipboard {
                at: at_end(),
                cues: vec![new.clone()],
                source_dir: None,
            },
        );
        // Inside a pasted group too.
        assert_refused(
            &w,
            Command::Paste {
                at: at_end(),
                cues: vec![with_kind(6, group(vec![new]))],
            },
        );
        assert_refused(
            &w,
            Command::SetCueProperty {
                id: cue_id(1),
                property: CueProperty::Kind { value: kind },
            },
        );
    }
}

#[test]
fn an_oversize_name_or_notes_is_refused_on_every_path() {
    let w = ws();
    let long_name = "n".repeat(1025);
    let long_notes = "n".repeat(64 * 1024 + 1);
    let mut named = cue(5, CueKind::Memo);
    named.name = long_name.clone();
    let mut noted = cue(5, CueKind::Memo);
    noted.notes = long_notes.clone();
    for new in [named, noted] {
        for command in [
            Command::AddCue {
                at: at_end(),
                cue: new.clone(),
                auto_number: false,
            },
            Command::Paste {
                at: at_end(),
                cues: vec![new.clone()],
            },
            Command::PasteClipboard {
                at: at_end(),
                cues: vec![new.clone()],
                source_dir: None,
            },
        ] {
            assert_refused(&w, command);
        }
    }
    for property in [
        CueProperty::Name {
            value: long_name.clone(),
        },
        CueProperty::Notes {
            value: long_notes.clone(),
        },
    ] {
        assert_refused(
            &w,
            Command::SetCueProperty {
                id: cue_id(1),
                property: property.clone(),
            },
        );
        assert_refused(
            &w,
            Command::BatchSet {
                ids: vec![cue_id(1)],
                property,
            },
        );
    }
}

#[test]
fn values_at_the_limits_are_accepted() {
    let mut w = ws();
    // 256 four-byte characters: the cue name limit is 1024 bytes.
    w.apply(Command::SetCueProperty {
        id: cue_id(1),
        property: CueProperty::Name {
            value: "\u{1F3AD}".repeat(256),
        },
    })
    .unwrap();
    w.apply(Command::SetCueProperty {
        id: cue_id(1),
        property: CueProperty::Notes {
            value: "n".repeat(64 * 1024),
        },
    })
    .unwrap();
    w.apply(Command::SetCueProperty {
        id: cue_id(1),
        property: CueProperty::Kind {
            value: CueKind::Network {
                destination: net_id(1),
                address: "a".repeat(64 * 1024),
                args: vec![OscArg::Int { value: 0 }; 1024],
            },
        },
    })
    .unwrap();
}

#[test]
fn a_show_with_over_limit_values_opens_with_warnings_and_can_be_mended() {
    let mut w = ws();
    let mut net_cue = with_kind(2, oversize_kinds().remove(1));
    net_cue.name = "n".repeat(2000);
    net_cue.notes = "n".repeat(70 * 1024);
    w.cue_lists[0].cues.push(net_cue);
    let loaded = load_show(&save_show(&w).unwrap()).unwrap();
    assert_eq!(loaded.workspace, w, "kept as it is");
    let fields: Vec<_> = loaded
        .warnings
        .iter()
        .filter(|i| i.cue_id == Some(cue_id(2)))
        .map(|i| i.kind.clone())
        .collect();
    for field in ["name", "notes", "args"] {
        assert!(
            fields
                .iter()
                .any(|k| matches!(k, IssueKind::TooLarge { field: f, .. } if *f == field)),
            "{field}: {fields:?}"
        );
    }
    // Other edits of the cue still go through; the oversize values can be replaced.
    let mut w = loaded.workspace;
    w.apply(Command::SetCueProperty {
        id: cue_id(2),
        property: CueProperty::PreWait { value: 1.0 },
    })
    .unwrap();
    w.apply(Command::SetCueProperty {
        id: cue_id(2),
        property: CueProperty::Name { value: "ok".into() },
    })
    .unwrap();
}

/// Regression: an over-limit value from the show file may be replaced by one no longer
/// than it, never by a longer one.
#[test]
fn a_loaded_over_limit_value_cannot_be_replaced_by_a_longer_one() {
    let mut w = ws();
    let mut c = cue(2, CueKind::Memo);
    c.name = "n".repeat(2000);
    c.notes = "n".repeat(70 * 1024);
    w.cue_lists[0].cues.push(c);
    let w = load_show(&save_show(&w).unwrap()).unwrap().workspace;
    let set = |property| Command::SetCueProperty {
        id: cue_id(2),
        property,
    };
    assert_refused(
        &w,
        set(CueProperty::Name {
            value: "n".repeat(2001),
        }),
    );
    assert_refused(
        &w,
        set(CueProperty::Notes {
            value: "n".repeat(70 * 1024 + 1),
        }),
    );
    let mut w = w;
    for property in [
        CueProperty::Name {
            value: "m".repeat(2000),
        },
        CueProperty::Notes {
            value: "m".repeat(65 * 1024),
        },
    ] {
        w.apply(set(property)).unwrap();
    }
}

/// An Audio cue whose trim or first slice is `seconds` long.
fn short_region(seconds: f64, as_trim: bool) -> Cue {
    let mut kind = audio();
    if let CueKind::Audio {
        end_time, slices, ..
    } = &mut kind
    {
        *end_time = Some(if as_trim { seconds } else { 2.0 });
        if !as_trim {
            slices.push(SliceMarker {
                time: seconds,
                play_count: 0,
            });
        }
    }
    cue(5, kind)
}

#[test]
fn a_trim_or_slice_under_50_ms_is_refused_on_every_path_and_50_ms_is_accepted() {
    let w = ws();
    for as_trim in [true, false] {
        let bad = short_region(0.04, as_trim);
        for command in [
            Command::AddCue {
                at: at_end(),
                cue: bad.clone(),
                auto_number: false,
            },
            Command::Paste {
                at: at_end(),
                cues: vec![bad.clone()],
            },
            Command::PasteClipboard {
                at: at_end(),
                cues: vec![bad.clone()],
                source_dir: None,
            },
            Command::SetCueProperty {
                id: cue_id(1),
                property: CueProperty::Kind {
                    value: bad.kind.clone(),
                },
            },
        ] {
            assert_refused(&w, command);
        }
        let ok = short_region(0.05, as_trim);
        let mut work = w.clone();
        History::new()
            .execute(
                &mut work,
                Command::AddCue {
                    at: at_end(),
                    cue: ok,
                    auto_number: false,
                },
            )
            .unwrap();
    }
}

/// An Audio cue with `n` slice markers one second apart.
fn many_slices(n: usize) -> Cue {
    let mut kind = audio();
    if let CueKind::Audio { slices, .. } = &mut kind {
        *slices = (1..=n)
            .map(|i| SliceMarker {
                time: i as f64,
                play_count: 1,
            })
            .collect();
    }
    cue(5, kind)
}

#[test]
fn more_slice_markers_than_the_audio_engine_plays_are_refused_on_every_path() {
    let w = ws();
    let bad = many_slices(MAX_SLICE_MARKERS + 1);
    for command in [
        Command::AddCue {
            at: at_end(),
            cue: bad.clone(),
            auto_number: false,
        },
        Command::Paste {
            at: at_end(),
            cues: vec![bad.clone()],
        },
        Command::PasteClipboard {
            at: at_end(),
            cues: vec![bad.clone()],
            source_dir: None,
        },
        Command::SetCueProperty {
            id: cue_id(1),
            property: CueProperty::Kind {
                value: bad.kind.clone(),
            },
        },
    ] {
        assert_refused(&w, command);
    }
    let mut work = w.clone();
    History::new()
        .execute(
            &mut work,
            Command::AddCue {
                at: at_end(),
                cue: many_slices(MAX_SLICE_MARKERS),
                auto_number: false,
            },
        )
        .unwrap();
}
