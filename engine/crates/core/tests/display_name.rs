// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `display_name` (M2.29c, QLab check R2), driven by the fixture the Swift test will share.

mod common;

use common::*;
use openlab_core::*;
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    label: String,
    audio: bool,
    name: String,
    media: Option<String>,
    expected: String,
}

#[test]
fn fixture_cases() {
    let fixture: Fixture =
        serde_json::from_str(include_str!("../../../fixtures/display_name.json")).unwrap();
    assert!(fixture.cases.len() >= 6);
    for case in fixture.cases {
        let kind = if case.audio {
            let mut kind = audio();
            if let CueKind::Audio { media, .. } = &mut kind {
                *media = case.media.as_deref().map(MediaRef::new);
            }
            kind
        } else {
            CueKind::Memo
        };
        let mut c = cue(1, kind);
        c.name = case.name;
        assert_eq!(c.display_name(), case.expected, "{}", case.label);
        assert_eq!(display_name(&c), case.expected, "{}", case.label);
    }
}

#[test]
fn renaming_the_file_changes_the_display_name_but_not_a_typed_name() {
    let mut c = cue(1, audio());
    assert_eq!(c.display_name(), "a.wav");
    if let CueKind::Audio { media, .. } = &mut c.kind {
        *media = Some(MediaRef::new("media/b.wav"));
    }
    assert_eq!(c.display_name(), "b.wav");
    assert_eq!(c.name, "", "following the file never writes `name`");
    c.name = "Typed".into();
    assert_eq!(c.display_name(), "Typed");
}
