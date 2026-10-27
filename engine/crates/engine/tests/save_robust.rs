// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Save robustness: leftover temp files, backup order under clock steps, and (Unix) file
//! metadata, symlinks and read-only destinations (NFR-04, NFR-06).

mod common;

use std::fs;

use common::*;
use openlab_core::backup_file_name;
use openlab_engine::persist::save_workspace;

fn renamed(name: &str) -> openlab_core::Workspace {
    let mut ws = example();
    ws.name = name.into();
    ws
}

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
    let own = dir.join(format!("My Show.openlab.{}-9.tmp", std::process::id()));
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
