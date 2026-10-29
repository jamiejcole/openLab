// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Save robustness: leftover temp files, backup order under clock steps, and (Unix) file
//! metadata, symlinks and read-only destinations (NFR-04, NFR-06).

mod common;

use std::fs;

use common::*;
use openlab_core::backup_file_name;
use openlab_engine::persist::save_workspace;

#[test]
fn rotation_orders_by_backup_name_not_mtime() {
    let env = env();
    let id = example().id;
    let dir = env.dirs.backups_dir(id, &env.show_path());
    fs::create_dir_all(&dir).unwrap();
    // The clock stepped backwards: the newest names carry the oldest file times.
    for (i, ms) in [(1, 9_000_000), (2, 5_000_000), (3, 1_000_000)] {
        let path = dir.join(backup_file_name(i * 1000));
        fs::write(&path, b"x").unwrap();
        set_mtime(&path, ms);
    }
    save_workspace(&env.dirs, &env.show_path(), &renamed("a"), 4000, 2).unwrap();
    save_workspace(&env.dirs, &env.show_path(), &renamed("b"), 5000, 2).unwrap();
    let expected: Vec<String> = [3000, 5000].map(backup_file_name).to_vec();
    assert_eq!(names(&dir), expected);
}

#[test]
fn rotation_removes_crash_left_backup_tmp_files_but_not_newer_or_foreign_ones() {
    let env = env();
    let id = example().id;
    let dir = env.dirs.backups_dir(id, &env.show_path());
    fs::create_dir_all(&dir).unwrap();
    let old_tmp = format!("{}.4000000000-0.tmp", backup_file_name(500));
    let own_tmp = format!("{}.{}-7.tmp", backup_file_name(600), std::process::id());
    // A write for a later time than any finished backup may be another instance's, in progress.
    let newer_tmp = format!("{}.4000000000-1.tmp", backup_file_name(9_000_000));
    let foreign = "x.openlab.tmp".to_owned();
    for name in [&old_tmp, &own_tmp, &newer_tmp, &foreign] {
        fs::write(dir.join(name), b"partial").unwrap();
    }
    save_workspace(&env.dirs, &env.show_path(), &renamed("a"), 1000, 20).unwrap();
    // The first save had nothing to back up, so no backup has finished: nothing is removed.
    assert!(names(&dir).contains(&old_tmp));
    save_workspace(&env.dirs, &env.show_path(), &renamed("b"), 2000, 20).unwrap();
    let all = names(&dir);
    assert!(!all.contains(&old_tmp));
    assert!(!all.contains(&own_tmp));
    assert!(all.contains(&newer_tmp));
    assert!(all.contains(&foreign));
    assert!(all.contains(&backup_file_name(2000)));
}

#[test]
fn save_removes_old_leftover_tmp_beside_the_show_only() {
    let env = env();
    let dir = env.show_dir();
    let show = env.show_path();
    save_workspace(&env.dirs, &show, &renamed("a"), 1, 20).unwrap();
    let old = dir.join("My Show.openlab.4000000000-0.tmp");
    let young = dir.join("My Show.openlab.4000000000-1.tmp");
    // A counter value no save in this test binary reaches, so the save never meets it.
    let own = dir.join(format!(
        "My Show.openlab.{}-999999999.tmp",
        std::process::id()
    ));
    let other_show = dir.join("Other.openlab.4000000000-0.tmp");
    let users = dir.join("My Show.openlab.tmp");
    for p in [&old, &young, &own, &other_show, &users] {
        fs::write(p, b"partial").unwrap();
    }
    for p in [&old, &own, &other_show, &users] {
        set_mtime(p, 1000);
    }
    save_workspace(&env.dirs, &show, &renamed("b"), 2, 20).unwrap();
    assert!(!old.exists(), "crash leftover removed");
    assert!(
        young.exists(),
        "could be another instance's write in progress"
    );
    assert!(own.exists(), "this process cleans up its own");
    assert!(other_show.exists());
    assert!(users.exists());
}

#[cfg(unix)]
mod unix {
    use super::*;
    use openlab_engine::persist::PersistError;
    use std::io;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::Path;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    fn name_of(path: &Path) -> String {
        openlab_engine::persist::read_text(path, 1 << 20)
            .map(|text| {
                openlab_core::load_show(&text)
                    .map(|l| l.workspace.name)
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    }

    #[test]
    fn save_keeps_the_files_permission_mode() {
        let env = env();
        let show = env.show_path();
        save_workspace(&env.dirs, &show, &renamed("a"), 1, 20).unwrap();
        fs::set_permissions(&show, fs::Permissions::from_mode(0o640)).unwrap();
        save_workspace(&env.dirs, &show, &renamed("b"), 2, 20).unwrap();
        assert_eq!(mode(&show), 0o640);
        assert_eq!(name_of(&show), "b");
        // Also a mode the default umask would never give.
        fs::set_permissions(&show, fs::Permissions::from_mode(0o666)).unwrap();
        save_workspace(&env.dirs, &show, &renamed("c"), 3, 20).unwrap();
        assert_eq!(mode(&show), 0o666);
    }

    #[test]
    fn save_writes_through_a_symlink_and_keeps_the_link() {
        let env = env();
        let real_dir = env.root.path().join("real");
        fs::create_dir(&real_dir).unwrap();
        let target = real_dir.join("Actual.openlab");
        save_workspace(&env.dirs, &target, &renamed("a"), 1, 20).unwrap();
        let link = env.show_path();
        symlink(&target, &link).unwrap();

        save_workspace(&env.dirs, &link, &renamed("b"), 2, 20).unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&link).unwrap(), target);
        assert_eq!(name_of(&target), "b");
        // No temp file beside either name.
        assert_eq!(names(&real_dir), vec!["Actual.openlab"]);
        assert_eq!(names(&env.show_dir()), vec!["My Show.openlab"]);
    }

    #[test]
    fn dangling_symlink_is_an_error_and_stays_a_link() {
        let env = env();
        let link = env.show_path();
        symlink(env.root.path().join("gone.openlab"), &link).unwrap();
        let error = save_workspace(&env.dirs, &link, &renamed("a"), 1, 20).unwrap_err();
        assert!(matches!(error, PersistError::Io { .. }), "{error}");
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(names(&env.show_dir()), vec!["My Show.openlab"]);
    }

    #[test]
    fn read_only_destination_is_refused_and_untouched() {
        let env = env();
        let show = env.show_path();
        save_workspace(&env.dirs, &show, &renamed("a"), 1, 20).unwrap();
        let before = fs::read(&show).unwrap();
        fs::set_permissions(&show, fs::Permissions::from_mode(0o444)).unwrap();

        let error = save_workspace(&env.dirs, &show, &renamed("b"), 2, 20).unwrap_err();
        match &error {
            PersistError::Io { source, .. } => {
                assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
            }
            other => panic!("unexpected {other}"),
        }
        assert!(error.to_string().contains("read-only"), "{error}");
        assert_eq!(fs::read(&show).unwrap(), before);
        assert_eq!(mode(&show), 0o444);
        assert_eq!(names(&env.show_dir()), vec!["My Show.openlab"]);
        // The pre-save backup made for the failed save is gone again.
        let dir = env.dirs.backups_dir(example().id, &show);
        assert!(names(&dir).is_empty());
    }
}

// A non-regular target must be refused, not read: opening a FIFO blocks the document worker.

#[cfg(unix)]
fn make_fifo(path: &std::path::Path) {
    let made = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .unwrap();
    assert!(made.success());
}

/// Saves through the real worker thread, with a bounded wait so a hang fails rather than stalls.
fn worker_save(
    writer: &openlab_engine::persist::SaveWriter,
    ws: &openlab_core::Workspace,
    path: &std::path::Path,
) -> Result<openlab_engine::persist::SaveReport, openlab_engine::persist::PersistError> {
    writer
        .save(ws.clone(), path.to_path_buf(), 1000, 3, 1)
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the worker hung on the target")
}

#[cfg(unix)]
#[test]
fn save_onto_a_fifo_or_a_symlink_to_one_is_refused_and_the_worker_keeps_serving() {
    let env = env();
    let writer = openlab_engine::persist::SaveWriter::spawn(env.dirs.clone()).unwrap();
    let fifo = env.show_dir().join("pipe.openlab");
    make_fifo(&fifo);
    let link = env.show_dir().join("link.openlab");
    std::os::unix::fs::symlink(&fifo, &link).unwrap();
    for target in [&fifo, &link] {
        assert!(worker_save(&writer, &example(), target).is_err());
    }
    // Still a FIFO: nothing replaced it, and no backup was made.
    assert!(!fs::metadata(&fifo).unwrap().is_file());
    let dir = env.dirs.backups_dir(example().id, &fifo);
    assert!(!dir.exists() || names(&dir).is_empty());
    worker_save(&writer, &example(), &env.show_path()).unwrap();
    assert!(env.show_path().is_file());
}

#[test]
fn save_onto_a_directory_is_refused_and_the_worker_keeps_serving() {
    let env = env();
    let writer = openlab_engine::persist::SaveWriter::spawn(env.dirs.clone()).unwrap();
    let dir = env.show_dir().join("folder.openlab");
    fs::create_dir(&dir).unwrap();
    assert!(worker_save(&writer, &example(), &dir).is_err());
    assert!(dir.is_dir());
    worker_save(&writer, &example(), &env.show_path()).unwrap();
}

#[cfg(unix)]
#[test]
fn backing_up_a_fifo_is_an_error_not_a_hang() {
    let env = env();
    let fifo = env.show_dir().join("pipe.openlab");
    make_fifo(&fifo);
    let err = openlab_engine::persist::backup_existing(&env.dirs, example().id, &fifo, 1, 3);
    assert!(err.is_err());
}

/// A backup never replaces another one, even when every name it may take is in use (the
/// save goes on and reports the failed backup).
#[test]
fn a_backup_never_replaces_another_even_with_every_name_taken() {
    let env = env();
    save_workspace(&env.dirs, &env.show_path(), &renamed("a"), 1000, 3).unwrap();
    let dir = env.dirs.backups_dir(example().id, &env.show_path());
    fs::create_dir_all(&dir).unwrap();
    for n in 0..1000 {
        fs::write(dir.join(backup_file_name(5000 + n)), "older backup").unwrap();
    }
    let err = openlab_engine::persist::backup_existing(
        &env.dirs,
        example().id,
        &env.show_path(),
        5000,
        3,
    );
    assert!(err.is_err(), "{err:?}");
    for n in 0..1000 {
        let kept = fs::read_to_string(dir.join(backup_file_name(5000 + n))).unwrap();
        assert_eq!(kept, "older backup", "backup {n} was replaced");
    }
    // Nothing is left behind but the backups that were there (and the first save's own).
    assert!(
        names(&dir).iter().all(|n| !n.ends_with(".tmp")),
        "{:?}",
        names(&dir)
    );
    // With a free name the backup is made.
    fs::remove_file(dir.join(backup_file_name(5500))).unwrap();
    let made = openlab_engine::persist::backup_existing(
        &env.dirs,
        example().id,
        &env.show_path(),
        5000,
        3,
    )
    .unwrap()
    .unwrap();
    assert_eq!(made, dir.join(backup_file_name(5500)));
    assert!(fs::read_to_string(made).unwrap().contains("\"a\""));
}

#[test]
fn a_regular_show_file_is_still_backed_up_before_it_is_replaced() {
    let env = env();
    save_workspace(&env.dirs, &env.show_path(), &renamed("a"), 1000, 3).unwrap();
    let report = save_workspace(&env.dirs, &env.show_path(), &renamed("b"), 2000, 3).unwrap();
    let backup = report.backup.expect("backup of the first save");
    assert!(fs::read_to_string(backup).unwrap().contains("\"a\""));
}

#[cfg(unix)]
#[test]
fn a_fifo_as_the_settings_file_loads_defaults_without_hanging_or_copying() {
    let env = env();
    let path = env.dirs.settings_path();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    make_fifo(&path);
    let dirs = env.dirs.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(openlab_engine::persist::load_machine_settings_checked(
            &dirs,
        ));
    });
    let (loaded, writable) = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("loading settings hung on a FIFO");
    assert!(!writable);
    assert!(!loaded.warnings.is_empty());
    assert!(!path.with_file_name("settings.json.corrupt").exists());
}

#[test]
fn an_oversized_show_file_is_replaced_with_a_backup_warning() {
    let env = env();
    let target = env.show_path();
    let big = fs::File::create(&target).unwrap();
    big.set_len(openlab_core::MAX_SHOW_FILE_BYTES as u64 + 1)
        .unwrap();
    drop(big);
    let report = save_workspace(&env.dirs, &target, &renamed("new"), 1000, 3).unwrap();
    assert!(report.backup.is_none());
    assert!(
        report.warnings.iter().any(|w| w.contains("backup failed")),
        "{:?}",
        report.warnings
    );
    assert!(fs::metadata(&target).unwrap().len() < 1_000_000);
}

#[test]
fn read_text_refuses_an_oversized_file() {
    let env = env();
    let target = env.show_path();
    fs::File::create(&target).unwrap().set_len(10_000).unwrap();
    let err = openlab_engine::persist::read_text(&target, 100).unwrap_err();
    assert!(matches!(
        err,
        openlab_engine::persist::PersistError::TooLarge { limit: 100, .. }
    ));
}
