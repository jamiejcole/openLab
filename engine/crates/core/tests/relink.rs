// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Relink planning and the undoable relink command (WS-03), plus path properties (NFR-32).

mod common;

use common::*;
use openlab_core::*;
use proptest::prelude::*;

fn with_media(n: u128, path: &str) -> Cue {
    let mut kind = audio();
    if let CueKind::Audio { media, .. } = &mut kind {
        *media = Some(MediaRef::new(path));
    }
    cue(n, kind)
}

fn media_of(ws: &Workspace, n: u128) -> String {
    match &ws.find_cue(cue_id(n)).unwrap().kind {
        CueKind::Audio { media, .. } => media.as_ref().unwrap().as_str().to_owned(),
        other => panic!("not audio: {other:?}"),
    }
}

fn missing(n: u128, path: &str) -> MissingMedia {
    MissingMedia {
        cue: cue_id(n),
        media: MediaRef::new(path),
    }
}

fn refs(paths: &[&str]) -> Vec<MediaRef> {
    paths.iter().map(|p| MediaRef::new(*p)).collect()
}

fn relink(n: u128, path: &str) -> Relink {
    Relink {
        cue: cue_id(n),
        new: MediaRef::new(path),
    }
}

#[test]
fn audio_media_lists_nested_cues_and_skips_empty() {
    let mut empty = audio();
    if let CueKind::Audio { media, .. } = &mut empty {
        *media = None;
    }
    let ws = workspace(vec![
        with_media(1, "a.wav"),
        cue(2, group(vec![with_media(3, "b.wav"), cue(4, empty)])),
        cue(5, CueKind::Memo),
    ]);
    let found: Vec<_> = audio_media(&ws)
        .into_iter()
        .map(|(id, m)| (id, m.as_str().to_owned()))
        .collect();
    assert_eq!(
        found,
        vec![
            (cue_id(1), "a.wav".to_owned()),
            (cue_id(3), "b.wav".to_owned())
        ]
    );
}

#[test]
fn plan_matches_by_normalised_file_name() {
    let plan = plan_relinks(
        &[
            missing(1, "old/Caf\u{e9}.WAV"),
            missing(2, "/gone/thunder.wav"),
            missing(3, "C:/Show/nothing.wav"),
        ],
        &refs(&[
            "new/sub/Cafe\u{301}.wav",
            "new/Thunder.WAV",
            "new/other.wav",
        ]),
    );
    assert_eq!(
        plan.matches,
        vec![
            relink(1, "new/sub/Cafe\u{301}.wav"),
            relink(2, "new/Thunder.WAV")
        ]
    );
    assert_eq!(plan.unmatched, vec![cue_id(3)]);
    assert!(plan.ambiguous.is_empty());
}

#[test]
fn duplicates_are_reported_not_guessed() {
    let plan = plan_relinks(
        &[missing(1, "a.wav"), missing(2, "b.wav")],
        &refs(&["x/a.wav", "y/A.wav", "z/b.wav", "z/../z/b.wav"]),
    );
    assert!(
        plan.matches.iter().all(|m| m.cue != cue_id(1)),
        "ambiguous cue must not be relinked"
    );
    assert_eq!(plan.ambiguous.len(), 1);
    assert_eq!(plan.ambiguous[0].cue, cue_id(1));
    assert_eq!(plan.ambiguous[0].candidates, refs(&["x/a.wav", "y/A.wav"]));
    // The same file listed twice is not ambiguous.
    assert_eq!(plan.matches, vec![relink(2, "z/b.wav")]);
}

#[test]
fn two_cues_with_one_missing_file_both_relink() {
    let plan = plan_relinks(
        &[missing(1, "a.wav"), missing(2, "sub/a.wav")],
        &refs(&["new/a.wav"]),
    );
    assert_eq!(plan.matches.len(), 2);
}

#[test]
fn relink_is_one_undoable_step() {
    let mut ws = workspace(vec![
        with_media(1, "old/a.wav"),
        cue(2, group(vec![with_media(3, "old/b.wav")])),
        cue(4, CueKind::Memo),
    ]);
    let before = ws.clone();
    let relinks = vec![
        relink(1, "new/a.wav"),
        relink(3, "/Lib/b.wav"),
        // Not audio, and not there: skipped.
        relink(4, "x.wav"),
        relink(99, "x.wav"),
    ];
    let command = relink_command(&ws, &relinks);
    let mut history = History::new();
    history.execute(&mut ws, command).unwrap();
    assert_eq!(media_of(&ws, 1), "new/a.wav");
    assert_eq!(media_of(&ws, 3), "/Lib/b.wav");
    assert_eq!(history.undo_len(), 1);
    assert!(save_show(&ws).is_ok());

    assert!(history.undo(&mut ws).unwrap());
    assert_eq!(ws, before);
    assert!(history.redo(&mut ws).unwrap());
    assert_eq!(media_of(&ws, 3), "/Lib/b.wav");
}

#[test]
fn nothing_to_relink_is_ignored_by_history() {
    let mut ws = workspace(vec![with_media(1, "a.wav")]);
    let mut history = History::new();
    let command = relink_command(&ws, &[]);
    history.execute(&mut ws, command).unwrap();
    assert_eq!(history.undo_len(), 0);
}

fn component() -> impl Strategy<Value = String> {
    // Ordinary names, spaces, non-ASCII (composed and decomposed); never "." / ".." / separators.
    prop::string::string_regex(
        "[A-Za-z0-9 _\u{e9}\u{fc}\u{4e2d}\u{1f600}-]{1,8}|e\u{301}x|\u{d55c}",
    )
    .unwrap()
}

proptest! {
    /// A file under the show folder is stored relative and resolves back to where it was; a file
    /// outside is stored absolute and resolves to itself. Posix and Windows spellings agree.
    #[test]
    fn relative_absolute_round_trip(
        dir in prop::collection::vec(component(), 1..4),
        tail in prop::collection::vec(component(), 1..4),
        other in prop::collection::vec(component(), 1..4),
        windows in any::<bool>(),
    ) {
        let (root, style) = if windows { ("C:/", PathStyle::Windows) } else { ("/", PathStyle::Posix) };
        let dir_p = format!("{root}{}", dir.join("/"));
        let abs_p = format!("{dir_p}/{}", tail.join("/"));
        let native = |p: &str| if windows { p.replace('/', "\\") } else { p.to_owned() };

        let inside = portable_ref(&native(&abs_p), Some(&native(&dir_p)), style);
        prop_assert!(!is_absolute(inside.as_str()), "{}", inside.as_str());
        prop_assert!(!inside.as_str().contains('\\'));
        let back = resolve(inside.as_str(), &normalize_nfc(&dir_p));
        prop_assert!(paths_equal(&back, &abs_p), "{back} vs {abs_p}");
        // Making the stored reference again from the resolved path is stable.
        let again = portable_ref(&native(&back), Some(&native(&dir_p)), style);
        prop_assert_eq!(&again, &inside);

        // A path whose first component differs from the show folder's is outside it.
        let outside_p = format!("{root}~{}/{}", other.join("/"), tail.join("/"));
        let outside = portable_ref(&native(&outside_p), Some(&native(&dir_p)), style);
        prop_assert!(is_absolute(outside.as_str()));
        prop_assert!(paths_equal(&resolve(outside.as_str(), &dir_p), &outside_p));
    }

    #[test]
    fn comparison_key_is_idempotent(s in "\\PC{0,24}") {
        let key = comparison_key(&s);
        prop_assert_eq!(comparison_key(&key), key);
    }

    #[test]
    fn arbitrary_text_never_panics(s in "\\PC{0,40}", d in "\\PC{0,40}", windows in any::<bool>()) {
        let style = if windows { PathStyle::Windows } else { PathStyle::Posix };
        let _ = portable_ref(&s, Some(&d), style);
        let _ = portable_ref(&s, None, style);
        let _ = relativize(&s, &d);
    }
}
