// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Atomic save, rolling backups and the writer thread (NFR-04, NFR-06).

mod common;

use std::fs;
use std::io;

use common::*;
use openlab_core::{backup_file_name, parse_backup_name};
use openlab_engine::persist::{
    PersistError, SaveWriter, Stage, save_workspace, save_workspace_with,
};

#[test]
fn save_new_file_and_reload_equal() {
    let env = env();
    let ws = renamed("First");
    let report = save_workspace(&env.dirs, &env.show_path(), &ws, 1000, 20).unwrap();
    assert!(report.backup.is_none(), "nothing to back up yet");
    assert!(report.warnings.is_empty());
    assert_eq!(reload(&env.show_path()), ws);
    assert_eq!(
        names(&env.show_dir()),
        vec!["My Show.openlab"],
        "no tmp left"
    );
}

#[test]
fn save_over_existing_keeps_previous_as_backup() {
    let env = env();
    let (a, b) = (renamed("A"), renamed("B"));
    save_workspace(&env.dirs, &env.show_path(), &a, 1000, 20).unwrap();
    let old_bytes = fs::read(env.show_path()).unwrap();
    let report = save_workspace(&env.dirs, &env.show_path(), &b, 2000, 20).unwrap();

    assert_eq!(reload(&env.show_path()), b);
    let backup = report.backup.unwrap();
    assert_eq!(fs::read(&backup).unwrap(), old_bytes);
    assert_eq!(
        backup.file_name().unwrap().to_str().unwrap(),
        backup_file_name(2000)
    );
    assert_eq!(
        backup.parent().unwrap(),
        env.dirs.backups_dir(a.id, &env.show_path())
    );
}

#[test]
fn failure_before_rename_leaves_original_byte_identical() {
    for failing in [Stage::TmpWritten, Stage::TmpSynced] {
        let env = env();
        save_workspace(&env.dirs, &env.show_path(), &renamed("A"), 1000, 20).unwrap();
        let before = fs::read(env.show_path()).unwrap();
        let backups_before = names(&env.dirs.backups_dir(renamed("A").id, &env.show_path()));

        let result = save_workspace_with(
            &env.dirs,
            &env.show_path(),
            &renamed("B"),
            2000,
            20,
            |stage| {
                if stage == failing {
                    Err(io::Error::other("injected"))
                } else {
                    Ok(())
                }
            },
        );
        assert!(
            matches!(result, Err(PersistError::Io { .. })),
            "{failing:?}: {result:?}"
        );
        assert_eq!(fs::read(env.show_path()).unwrap(), before, "{failing:?}");
        assert_eq!(
            names(&env.show_dir()),
            vec!["My Show.openlab"],
            "tmp removed"
        );
        // The failed attempt must not consume a backup slot.
        assert_eq!(
            names(&env.dirs.backups_dir(renamed("A").id, &env.show_path())),
            backups_before
        );
    }
}

#[test]
fn leftover_tmp_files_are_never_clobbered_or_followed() {
    let env = env();
    save_workspace(&env.dirs, &env.show_path(), &renamed("A"), 1000, 20).unwrap();
    // A crash leaves a half-written temp file; a user may also have a file of the old fixed
    // temp name. Neither is touched, and neither stops the next save.
    let crash_tmp = env.show_dir().join("My Show.openlab.4242-0.tmp");
    let users = env.show_dir().join("My Show.openlab.tmp");
    fs::write(&crash_tmp, b"{\"format_version\": 1, \"workspa").unwrap();
    fs::write(&users, b"mine").unwrap();
    assert_eq!(reload(&env.show_path()).name, "A");

    save_workspace(&env.dirs, &env.show_path(), &renamed("B"), 2000, 20).unwrap();
    assert_eq!(reload(&env.show_path()).name, "B");
    assert_eq!(fs::read(&users).unwrap(), b"mine");
    assert!(crash_tmp.exists());
}

#[test]
fn each_write_uses_its_own_temp_name() {
    let env = env();
    let target = env.show_dir().join("x.bin");
    let mut seen = Vec::new();
    for _ in 0..3 {
        openlab_engine::persist::write_atomic_with(&target, b"data", |_| {
            let tmps: Vec<_> = names(&env.show_dir())
                .into_iter()
                .filter(|n| n.ends_with(".tmp"))
                .collect();
            assert_eq!(tmps.len(), 1, "exactly this write's temp file: {tmps:?}");
            seen.push(tmps[0].clone());
            Ok(())
        })
        .unwrap();
    }
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 3);
}

#[test]
fn recovery_file_survives_a_failed_save_and_goes_only_after_the_rename() {
    for failing in [Stage::TmpWritten, Stage::TmpSynced] {
        let env = env();
        let ws = renamed("A");
        save_workspace(&env.dirs, &env.show_path(), &ws, 1000, 20).unwrap();
        let mut edited = renamed("Edited");
        edited.id = ws.id;
        let recovery =
            openlab_engine::persist::write_recovery(&env.dirs, &edited, Some(&env.show_path()))
                .unwrap();
        let before = fs::read(&recovery).unwrap();

        let result = save_workspace_with(&env.dirs, &env.show_path(), &edited, 2000, 20, |stage| {
            assert!(recovery.exists(), "recovery must outlive the temp write");
            if stage == failing {
                Err(io::Error::other("injected"))
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&recovery).unwrap(), before, "{failing:?}");
    }
    // And on success it is gone, but only after every checkpoint passed.
    let env = env();
    let ws = renamed("A");
    let recovery =
        openlab_engine::persist::write_recovery(&env.dirs, &ws, Some(&env.show_path())).unwrap();
    save_workspace_with(&env.dirs, &env.show_path(), &ws, 1, 20, |_| {
        assert!(recovery.exists());
        Ok(())
    })
    .unwrap();
    assert!(!recovery.exists());
}

#[test]
fn failed_save_rotates_nothing_out_even_with_a_low_keep() {
    let env = env();
    for i in 0..3_i64 {
        save_workspace(
            &env.dirs,
            &env.show_path(),
            &renamed(&format!("v{i}")),
            i * 1000,
            20,
        )
        .unwrap();
    }
    let dir = env.dirs.backups_dir(renamed("x").id, &env.show_path());
    let before = names(&dir);
    assert_eq!(before.len(), 2);
    let result = save_workspace_with(&env.dirs, &env.show_path(), &renamed("v3"), 9000, 1, |_| {
        Err(io::Error::other("injected"))
    });
    assert!(result.is_err());
    assert_eq!(names(&dir), before, "nothing deleted, nothing added");
}

#[test]
fn unwritable_destination_is_a_typed_error() {
    let env = env();
    // Missing folder.
    let missing = env.root.path().join("nope").join("x.openlab");
    let error = save_workspace(&env.dirs, &missing, &renamed("A"), 1, 20).unwrap_err();
    assert!(matches!(error, PersistError::Io { .. }));
    assert!(!env.root.path().join("nope").exists());

    // Destination is a folder: the rename fails, the temp file is cleaned up.
    let dir_dest = env.show_dir().join("Folder.openlab");
    fs::create_dir(&dir_dest).unwrap();
    let error = save_workspace(&env.dirs, &dir_dest, &renamed("A"), 1, 20).unwrap_err();
    assert!(matches!(error, PersistError::Io { .. }));
    assert_eq!(names(&env.show_dir()), vec!["Folder.openlab"]);
}

#[test]
fn workspace_that_would_not_load_back_writes_nothing() {
    let env = env();
    save_workspace(&env.dirs, &env.show_path(), &renamed("A"), 1000, 20).unwrap();
    let before = fs::read(env.show_path()).unwrap();

    let mut bad = renamed("B");
    let first = bad.cue_lists[0].cues[0].clone();
    bad.cue_lists[0].cues.push(first); // duplicate cue id
    let error = save_workspace(&env.dirs, &env.show_path(), &bad, 2000, 20).unwrap_err();
    assert!(matches!(error, PersistError::Serialize(_)));
    assert_eq!(fs::read(env.show_path()).unwrap(), before);
    assert!(names(&env.dirs.backups_dir(bad.id, &env.show_path())).is_empty());
}

#[test]
fn rotation_keeps_exactly_the_newest_n() {
    let env = env();
    let id = example().id;
    for i in 0..8_i64 {
        save_workspace(
            &env.dirs,
            &env.show_path(),
            &renamed(&format!("v{i}")),
            i * 1000,
            3,
        )
        .unwrap();
    }
    // 8 saves make 7 backups (the first save had nothing to back up); 3 remain: the previous
    // versions v4, v5 and v6, made at t = 5000, 6000 and 7000.
    let dir = env.dirs.backups_dir(id, &env.show_path());
    let expected: Vec<String> = (5..8).map(|i| backup_file_name(i * 1000)).collect();
    assert_eq!(names(&dir), expected);
    assert_eq!(reload(&dir.join(&expected[2])).name, "v6");
    assert_eq!(reload(&dir.join(&expected[0])).name, "v4");
}

#[test]
fn rotation_ignores_foreign_files_and_odd_names() {
    let env = env();
    let id = example().id;
    let dir = env.dirs.backups_dir(id, &env.show_path());
    fs::create_dir_all(&dir).unwrap();
    for name in [
        "notes.txt",
        ".DS_Store",
        "\u{e9}t\u{e9}.openlab",
        "x.openlab.tmp",
    ] {
        fs::write(dir.join(name), b"mine").unwrap();
    }
    fs::create_dir(dir.join("20200101-000000-000.openlab.d")).unwrap();
    for i in 0..4_i64 {
        save_workspace(&env.dirs, &env.show_path(), &renamed("x"), i, 2).unwrap();
    }
    let all = names(&dir);
    assert_eq!(
        all.iter()
            .filter(|n| parse_backup_name(n).is_some())
            .count(),
        2
    );
    for kept in [
        "notes.txt",
        ".DS_Store",
        "\u{e9}t\u{e9}.openlab",
        "x.openlab.tmp",
    ] {
        assert!(all.contains(&kept.to_owned()), "{kept} was deleted");
    }
}

#[test]
fn saves_in_the_same_millisecond_do_not_overwrite_backups() {
    let env = env();
    let id = example().id;
    for i in 0..4 {
        save_workspace(
            &env.dirs,
            &env.show_path(),
            &renamed(&format!("v{i}")),
            5000,
            20,
        )
        .unwrap();
    }
    assert_eq!(names(&env.dirs.backups_dir(id, &env.show_path())).len(), 3);
}

#[test]
fn zero_keep_means_no_backups_and_no_deletions() {
    let env = env();
    let id = example().id;
    let dir = env.dirs.backups_dir(id, &env.show_path());
    fs::create_dir_all(&dir).unwrap();
    let old = dir.join(backup_file_name(1));
    fs::write(&old, b"old").unwrap();
    save_workspace(&env.dirs, &env.show_path(), &renamed("A"), 10, 0).unwrap();
    save_workspace(&env.dirs, &env.show_path(), &renamed("B"), 20, 0).unwrap();
    assert_eq!(names(&dir), vec![backup_file_name(1)]);
}

#[test]
fn failed_backup_is_a_warning_not_a_failed_save() {
    let env = env();
    let ws = renamed("A");
    save_workspace(&env.dirs, &env.show_path(), &ws, 1, 20).unwrap();
    let id = ws.id;
    // Make the backup folder impossible to create: a file where the folder should be.
    let blocker = env.dirs.backups_dir(id, &env.show_path());
    fs::create_dir_all(blocker.parent().unwrap()).unwrap();
    fs::write(&blocker, b"in the way").unwrap();
    let report = save_workspace(&env.dirs, &env.show_path(), &renamed("B"), 2, 20).unwrap();
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("backup failed"));
    assert_eq!(reload(&env.show_path()).name, "B");
}

#[test]
fn manual_save_removes_the_recovery_file() {
    let env = env();
    let ws = renamed("A");
    let recovery =
        openlab_engine::persist::write_recovery(&env.dirs, &ws, Some(&env.show_path())).unwrap();
    assert!(recovery.exists());
    save_workspace(&env.dirs, &env.show_path(), &ws, 1, 20).unwrap();
    assert!(!recovery.exists());
}

#[test]
fn writer_thread_saves_and_autosaves() {
    let env = env();
    let writer = SaveWriter::spawn(env.dirs.clone()).unwrap();
    let ws = renamed("W");
    let saved = writer
        .save(ws.clone(), env.show_path(), 1000, 20, 1)
        .recv()
        .unwrap()
        .unwrap();
    assert!(saved.warnings.is_empty());
    assert_eq!(reload(&env.show_path()), ws);

    let path = writer
        .autosave(renamed("W2"), Some(env.show_path()), 2)
        .recv()
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(path, env.dirs.recovery_path(ws.id, Some(&env.show_path())));
    assert_eq!(reload(&path).name, "W2");

    // A failing job reports on its own channel and the writer carries on.
    let bad = env.root.path().join("nope/x.openlab");
    assert!(
        writer
            .save(ws.clone(), bad, 1, 20, 3)
            .recv()
            .unwrap()
            .is_err()
    );
    assert!(
        writer
            .save(ws, env.show_path(), 2, 20, 3)
            .recv()
            .unwrap()
            .is_ok()
    );
}
