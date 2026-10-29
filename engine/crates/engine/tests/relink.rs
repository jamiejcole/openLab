// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Missing media, find-in-folder relink and media paths on disk (WS-03, NFR-32).

mod common;

use std::path::Path;

use common::*;
use openlab_core::{History, MediaRef, Relink, audio_media, relink_command};
use openlab_engine::persist::{
    ScanLimits, find_in_folder, media_path, missing_media, scan_folder, scan_folder_with,
};

fn media_strings(ws: &openlab_core::Workspace) -> Vec<String> {
    audio_media(ws)
        .into_iter()
        .map(|(_, m)| m.as_str().to_owned())
        .collect()
}

#[test]
fn missing_media_checks_the_disk() {
    let env = env();
    let mut ws = example();
    set_media(
        &mut ws,
        &["media/here.wav", "media/gone.wav", "/nowhere/abs.wav"],
    );
    touch(&env.show_dir().join("media/here.wav"));
    let missing = missing_media(&ws, Some(&env.show_dir()));
    let paths: Vec<_> = missing.iter().map(|m| m.media.as_str()).collect();
    assert_eq!(paths[0], "media/gone.wav");
    assert!(paths.contains(&"/nowhere/abs.wav"));
    assert!(!paths.contains(&"media/here.wav"));
    // Unsaved workspace: relative paths cannot be found.
    assert!(
        missing_media(&ws, None)
            .iter()
            .any(|m| m.media.as_str() == "media/here.wav")
    );
}

#[test]
fn media_path_resolves_against_the_show_folder() {
    let dir = Path::new("/Show");
    assert_eq!(
        media_path(&MediaRef::new("media/a.wav"), Some(dir)).unwrap(),
        Path::new("/Show/media/a.wav")
    );
    assert_eq!(
        media_path(&MediaRef::new("/Lib/a.wav"), Some(dir)).unwrap(),
        Path::new("/Lib/a.wav")
    );
    assert_eq!(media_path(&MediaRef::new("media/a.wav"), None), None);
    assert!(media_path(&MediaRef::new("/Lib/a.wav"), None).is_some());
}

#[test]
fn scan_walks_nested_folders_in_stable_order() {
    let env = env();
    let root = env.root.path().join("lib");
    for f in ["b.wav", "a.wav", "sub/c.wav", "sub/deep/d.wav", "z/e.wav"] {
        touch(&root.join(f));
    }
    let scan = scan_folder(&root, ScanLimits::default());
    let rel: Vec<_> = scan
        .files
        .iter()
        .map(|p| {
            p.strip_prefix(&root)
                .unwrap()
                .to_str()
                .unwrap()
                .replace('\\', "/")
        })
        .collect();
    assert_eq!(
        rel,
        ["a.wav", "b.wav", "sub/c.wav", "sub/deep/d.wav", "z/e.wav"]
    );
    assert!(!scan.truncated);
}

#[test]
fn scan_respects_depth_and_count_limits() {
    let env = env();
    let root = env.root.path().join("lib");
    for f in [
        "a.wav",
        "sub/b.wav",
        "sub/deep/c.wav",
        "sub/deep/deeper/d.wav",
    ] {
        touch(&root.join(f));
    }
    let shallow = scan_folder(
        &root,
        ScanLimits {
            max_depth: 1,
            max_files: 100,
            ..ScanLimits::default()
        },
    );
    assert_eq!(shallow.files.len(), 2);
    assert!(shallow.truncated);
    let flat = scan_folder(
        &root,
        ScanLimits {
            max_depth: 0,
            max_files: 100,
            ..ScanLimits::default()
        },
    );
    assert_eq!(flat.files.len(), 1);
    assert!(flat.truncated);
    let capped = scan_folder(
        &root,
        ScanLimits {
            max_depth: 16,
            max_files: 3,
            ..ScanLimits::default()
        },
    );
    assert_eq!(capped.files.len(), 3);
    assert!(capped.truncated);
    let exact = scan_folder(
        &root,
        ScanLimits {
            max_depth: 16,
            max_files: 4,
            ..ScanLimits::default()
        },
    );
    assert_eq!(exact.files.len(), 4);
    assert!(!exact.truncated);
    // A missing folder is an empty scan, not a failure.
    let nothing = scan_folder(&root.join("nope"), ScanLimits::default());
    assert!(nothing.files.is_empty());
    assert_eq!(nothing.unreadable, 1);
}

#[cfg(unix)]
#[test]
fn scan_does_not_follow_symlink_loops() {
    use std::os::unix::fs::symlink;
    let env = env();
    let root = env.root.path().join("lib");
    touch(&root.join("sub/a.wav"));
    symlink(&root, root.join("sub/loop")).unwrap();
    symlink(root.join("sub/a.wav"), root.join("link.wav")).unwrap();
    symlink(root.join("nowhere.wav"), root.join("dangling.wav")).unwrap();
    let scan = scan_folder(&root, ScanLimits::default());
    let mut names: Vec<_> = scan
        .files
        .iter()
        .map(|p| p.file_name().unwrap().to_str().unwrap().to_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["a.wav", "link.wav"]);
    assert!(!scan.truncated);
}

#[cfg(unix)]
#[test]
fn non_utf8_names_are_skipped_not_fatal() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let env = env();
    let mut ws = example();
    set_media(&mut ws, &["old/unique.wav"]);
    let lib = env.root.path().join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    if let Err(e) = std::fs::write(lib.join(OsStr::from_bytes(b"bad\xff\xfe.wav")), b"x") {
        // APFS (macOS) only holds valid UTF-8 names and refuses the rest with EILSEQ. Any other
        // failure is a real error, so the test cannot pass for the wrong reason.
        assert!(
            cfg!(target_os = "macos") && e.raw_os_error() == Some(92),
            "creating a non-UTF-8 file name failed: {e}"
        );
        return;
    }
    touch(&lib.join("unique.wav"));
    let found = find_in_folder(&ws, Some(&env.show_dir()), &lib, ScanLimits::default());
    assert_eq!(found.skipped, 1);
    assert_eq!(found.plan.matches.len(), 1);
}

/// #198: with no media missing the folder is not walked: a limit that would stop any walk at
/// once (no folder may be read) is never reached, and nothing is skipped or proposed.
#[test]
fn find_in_folder_with_nothing_missing_does_not_walk_the_folder() {
    let env = env();
    let mut ws = example();
    let count = audio_media(&ws).len();
    let names: Vec<String> = (0..count).map(|n| format!("media/here{n}.wav")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    set_media(&mut ws, &names);
    for name in &names {
        touch(&env.show_dir().join(name));
    }
    assert!(missing_media(&ws, Some(&env.show_dir())).is_empty());
    let lib = env.root.path().join("lib");
    touch(&lib.join("here0.wav"));
    let no_walk = ScanLimits {
        max_dirs: 0,
        ..ScanLimits::default()
    };
    let found = find_in_folder(&ws, Some(&env.show_dir()), &lib, no_walk);
    assert!(!found.truncated, "the folder was walked");
    assert_eq!(found.skipped, 0);
    assert!(found.plan.matches.is_empty() && found.plan.ambiguous.is_empty());
}

#[test]
fn find_in_folder_proposes_relinks_and_undo_restores() {
    let env = env();
    let mut ws = example();
    set_media(
        &mut ws,
        &[
            "media/preshow.wav",
            "/Volumes/Gone/thunder.aif",
            "old/dup.wav",
        ],
    );
    let before = ws.clone();
    // The files moved into a nested library inside the show folder, one with NFD spelling
    // and different case, one outside the show folder.
    touch(&env.show_dir().join("library/Sounds/PreShow.WAV"));
    let outside = env.root.path().join("elsewhere");
    touch(&outside.join("Thunder.aif"));
    // dup.wav exists twice: ambiguous.
    touch(&env.show_dir().join("library/dup.wav"));
    touch(&env.show_dir().join("library/Sounds/dup.wav"));

    let folder = env.show_dir().join("library");
    let found = find_in_folder(&ws, Some(&env.show_dir()), &folder, ScanLimits::default());
    assert!(!found.truncated);
    let cues: Vec<_> = audio_media(&ws).into_iter().map(|(id, _)| id).collect();
    assert_eq!(
        found.plan.matches,
        vec![Relink {
            cue: cues[0],
            new: MediaRef::new("library/Sounds/PreShow.WAV")
        }]
    );
    assert!(
        found.plan.unmatched.contains(&cues[1]),
        "thunder is not in this folder"
    );
    assert_eq!(found.plan.ambiguous.len(), 1);
    assert_eq!(found.plan.ambiguous[0].cue, cues[2]);
    assert_eq!(found.plan.ambiguous[0].candidates.len(), 2);

    // Scan the second folder for the one outside the show folder: stored absolute.
    let outside_found = find_in_folder(&ws, Some(&env.show_dir()), &outside, ScanLimits::default());
    let abs = outside.join("Thunder.aif");
    assert_eq!(
        outside_found.plan.matches,
        vec![Relink {
            cue: cues[1],
            new: MediaRef::new(openlab_core::to_portable(
                abs.to_str().unwrap(),
                openlab_core::PathStyle::host()
            ))
        }]
    );

    // Applying goes through the command model: one undo step.
    let mut history = History::new();
    let mut all = found.plan.matches.clone();
    all.extend(outside_found.plan.matches.clone());
    let command = relink_command(&ws, &all);
    history.execute(&mut ws, command).unwrap();
    assert!(media_strings(&ws).contains(&"library/Sounds/PreShow.WAV".to_owned()));
    let still_missing: Vec<_> = missing_media(&ws, Some(&env.show_dir()))
        .into_iter()
        .map(|m| m.cue)
        .collect();
    assert!(!still_missing.contains(&cues[0]) && !still_missing.contains(&cues[1]));
    assert!(
        still_missing.contains(&cues[2]),
        "ambiguous cue stays missing"
    );
    assert!(history.undo(&mut ws).unwrap());
    assert_eq!(ws, before);
}

#[test]
fn nfd_names_on_disk_match_nfc_media() {
    let env = env();
    let mut ws = example();
    set_media(&mut ws, &["old/Caf\u{e9}.wav"]);
    let lib = env.show_dir().join("lib");
    touch(&lib.join("Cafe\u{301}.WAV")); // macOS-style decomposed spelling
    let found = find_in_folder(&ws, Some(&env.show_dir()), &lib, ScanLimits::default());
    assert_eq!(found.plan.matches.len(), 1);
    // The stored reference is NFC.
    assert_eq!(found.plan.matches[0].new.as_str(), "lib/Caf\u{e9}.WAV");
}

#[test]
fn existing_media_is_left_out_of_the_plan() {
    let env = env();
    let mut ws = example();
    set_media(&mut ws, &["media/a.wav"]);
    touch(&env.show_dir().join("media/a.wav"));
    let lib = env.root.path().join("lib");
    touch(&lib.join("a.wav"));
    let found = find_in_folder(&ws, Some(&env.show_dir()), &lib, ScanLimits::default());
    let cue = audio_media(&ws)[0].0;
    assert!(found.plan.matches.iter().all(|m| m.cue != cue));
    assert!(found.plan.ambiguous.iter().all(|m| m.cue != cue));
    assert!(!found.plan.unmatched.contains(&cue));
}

#[test]
fn scan_stops_at_the_directory_count_bound() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for d in ["a", "b", "c", "d"] {
        touch(&root.join(d).join("f.wav"));
    }
    let scan = scan_folder(
        root,
        ScanLimits {
            max_dirs: 3,
            ..ScanLimits::default()
        },
    );
    // The root plus two sub-folders were read; the rest were not.
    assert_eq!(scan.files.len(), 2);
    assert!(scan.truncated);
    let all = scan_folder(
        root,
        ScanLimits {
            max_dirs: 5,
            ..ScanLimits::default()
        },
    );
    assert_eq!(all.files.len(), 4);
    assert!(!all.truncated);
}

#[test]
fn scan_stops_at_the_deadline_using_the_injected_clock() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for d in ["a", "b", "c", "d"] {
        touch(&root.join(d).join("f.wav"));
    }
    let limits = ScanLimits {
        max_duration: std::time::Duration::from_secs(3),
        ..ScanLimits::default()
    };
    // Each look at the clock is one second later: root, a, b are read, then time is up.
    let mut now = std::time::Duration::ZERO;
    let scan = scan_folder_with(root, limits, &mut || {
        let t = now;
        now += std::time::Duration::from_secs(1);
        t
    });
    assert_eq!(scan.files.len(), 2);
    assert!(scan.truncated);
    // A clock that never advances never stops the scan.
    let scan = scan_folder_with(root, limits, &mut || std::time::Duration::ZERO);
    assert_eq!(scan.files.len(), 4);
    assert!(!scan.truncated);
}
