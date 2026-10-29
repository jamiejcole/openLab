// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Listing `Recovery/` when the show file cannot be loaded (NFR-05).

use std::fs;

use crate::common::*;
use openlab_engine::persist::{scan_recovery, write_recovery};

#[test]
fn missing_or_empty_recovery_folder_lists_nothing() {
    let env = env();
    let scan = scan_recovery(&env.dirs, 100);
    assert!(scan.entries.is_empty() && scan.warnings.is_empty() && !scan.truncated);
    fs::create_dir_all(env.dirs.recovery_dir()).unwrap();
    let scan = scan_recovery(&env.dirs, 100);
    assert!(scan.entries.is_empty() && scan.warnings.is_empty() && !scan.truncated);
}

#[test]
fn lists_ids_names_and_times_newest_first_without_touching_files() {
    let env = env();
    let mut ws = example();
    ws.name = "Hamlet".into();
    let saved = write_recovery(&env.dirs, &ws, Some(&env.show_path())).unwrap();
    set_mtime(&saved, 5_000_000);
    let mut other = example();
    other.name = "Never saved".into();
    let unsaved = write_recovery(&env.dirs, &other, None).unwrap();
    set_mtime(&unsaved, 9_000_000);
    let before = names(&env.dirs.recovery_dir());

    let scan = scan_recovery(&env.dirs, 100);
    assert!(scan.warnings.is_empty(), "{:?}", scan.warnings);
    assert_eq!(scan.entries.len(), 2);
    let (first, second) = (&scan.entries[0], &scan.entries[1]);
    assert_eq!(first.path, unsaved);
    assert_eq!(first.workspace_id, other.id);
    assert_eq!(first.path_hash, None);
    assert_eq!(first.show_name.as_deref(), Some("Never saved"));
    assert_eq!(first.modified_ms, Some(9_000_000));
    assert_eq!(second.path, saved);
    assert_eq!(second.show_name.as_deref(), Some("Hamlet"));
    assert_eq!(second.path_hash.as_ref().map(String::len), Some(16));
    assert_eq!(names(&env.dirs.recovery_dir()), before);
}

#[test]
fn corrupt_file_is_still_listed_with_the_id_from_its_name() {
    let env = env();
    let ws = example();
    let path = env.dirs.recovery_path(ws.id, None);
    fs::create_dir_all(env.dirs.recovery_dir()).unwrap();
    fs::write(&path, "{ not a show").unwrap();
    let scan = scan_recovery(&env.dirs, 100);
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(scan.entries[0].workspace_id, ws.id);
    assert_eq!(scan.entries[0].show_name, None);
    assert_eq!(scan.warnings.len(), 1, "{:?}", scan.warnings);
    // Not renamed or removed: listing is read-only.
    assert!(path.exists());
}

#[test]
fn file_holding_another_workspace_is_listed_without_a_name() {
    let env = env();
    let ws = example();
    let mut other = example();
    other.id = openlab_core::WorkspaceId(uuid_for(7));
    let text = openlab_core::save_show(&other).unwrap();
    fs::create_dir_all(env.dirs.recovery_dir()).unwrap();
    fs::write(env.dirs.recovery_path(ws.id, None), text).unwrap();
    let scan = scan_recovery(&env.dirs, 100);
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(scan.entries[0].workspace_id, ws.id);
    assert_eq!(scan.entries[0].show_name, None);
    assert_eq!(scan.warnings.len(), 1);
}

fn uuid_for(n: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128(n)
}

#[test]
fn odd_names_and_non_files_are_skipped_with_a_reason() {
    let env = env();
    let ws = example();
    write_recovery(&env.dirs, &ws, None).unwrap();
    let dir = env.dirs.recovery_dir();
    for junk in [
        "notes.txt",
        ".openlab",
        "UPPER-CASE-NOT-AN-ID.openlab",
        "123e4567-e89b-42d3-a456-426614174000-short.openlab",
        "\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}.openlab",
    ] {
        fs::write(dir.join(junk), "x").unwrap();
    }
    // A folder with a valid name, and the files this module moves aside itself.
    fs::create_dir(dir.join(format!("{}.openlab", uuid_for(1).hyphenated()))).unwrap();
    fs::write(dir.join(format!("{}.openlab.corrupt", ws.id)), "x").unwrap();
    fs::write(dir.join(format!("{}.openlab.corrupt.2", ws.id)), "x").unwrap();

    let scan = scan_recovery(&env.dirs, 100);
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(scan.entries[0].workspace_id, ws.id);
    // The five junk names and the folder; the `.corrupt` files are silent.
    assert_eq!(scan.warnings.len(), 6, "{:?}", scan.warnings);
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_name_is_skipped_not_a_crash() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let env = env();
    let dir = env.dirs.recovery_dir();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(OsStr::from_bytes(b"bad-\xff\xfe.openlab")), "x").unwrap();
    write_recovery(&env.dirs, &example(), None).unwrap();
    let scan = scan_recovery(&env.dirs, 100);
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(scan.warnings.len(), 1);
    assert!(
        scan.warnings[0].contains("not UTF-8"),
        "{:?}",
        scan.warnings
    );
}

#[test]
fn many_entries_are_bounded() {
    let env = env();
    let dir = env.dirs.recovery_dir();
    fs::create_dir_all(&dir).unwrap();
    for n in 0..40u128 {
        // Corrupt content: cheap to "read", still listed.
        fs::write(
            dir.join(format!("{}.openlab", uuid_for(n + 1).hyphenated())),
            "x",
        )
        .unwrap();
    }
    let scan = scan_recovery(&env.dirs, 10);
    assert_eq!(scan.entries.len(), 10);
    assert!(scan.truncated);
    // The same files are returned every time.
    let again = scan_recovery(&env.dirs, 10);
    let ids = |s: &openlab_engine::persist::RecoveryScan| {
        let mut v: Vec<_> = s.entries.iter().map(|e| e.workspace_id).collect();
        v.sort();
        v
    };
    assert_eq!(ids(&scan), ids(&again));
    let all = scan_recovery(&env.dirs, 40);
    assert_eq!(all.entries.len(), 40);
    assert!(!all.truncated);
}

#[test]
fn stale_files_are_listed_flagged_with_the_time_in_their_name() {
    let env = env();
    let mut ws = example();
    ws.name = "Older".into();
    let dir = env.dirs.recovery_dir();
    fs::create_dir_all(&dir).unwrap();
    let stale = dir.join(format!("{}.000000000000123.stale.openlab", ws.id));
    fs::write(&stale, openlab_core::save_show(&ws).unwrap()).unwrap();
    let odd_time = dir.join(format!("{}.notanumber.stale.openlab", ws.id));
    fs::write(&odd_time, "x").unwrap();
    // Not a stale file of any workspace.
    fs::write(dir.join("x.stale.openlab"), "x").unwrap();
    let scan = scan_recovery(&env.dirs, 100);
    assert_eq!(scan.entries.len(), 2);
    let named = scan.entries.iter().find(|e| e.path == stale).unwrap();
    assert!(named.stale);
    assert_eq!(named.modified_ms, Some(123));
    assert_eq!(named.show_name.as_deref(), Some("Older"));
    // An unparseable time falls back to the file's own.
    let odd = scan.entries.iter().find(|e| e.path == odd_time).unwrap();
    assert!(odd.stale && odd.modified_ms.is_some_and(|t| t > 123));
    // The unreadable one is listed without a name; the bad name is skipped.
    assert_eq!(scan.warnings.len(), 2, "{:?}", scan.warnings);
}

#[test]
fn oversize_file_is_listed_without_a_name() {
    let env = env();
    let ws = example();
    fs::create_dir_all(env.dirs.recovery_dir()).unwrap();
    let mut text = openlab_core::save_show(&ws).unwrap().into_bytes();
    text.resize(openlab_engine::persist::MAX_NAME_READ_BYTES + 1, b' ');
    fs::write(env.dirs.recovery_path(ws.id, None), text).unwrap();
    let scan = scan_recovery(&env.dirs, 100);
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(scan.entries[0].show_name, None);
    assert_eq!(scan.warnings.len(), 1);
}

#[test]
fn the_newest_files_survive_the_bound() {
    let env = env();
    let dir = env.dirs.recovery_dir();
    fs::create_dir_all(&dir).unwrap();
    // Ids ascend while times descend, so name order is the opposite of newest first.
    for n in 1..=6u128 {
        let p = dir.join(format!("{}.openlab", uuid_for(n).hyphenated()));
        fs::write(&p, "x").unwrap();
        set_mtime(&p, 10_000_000 - n as u64 * 1000);
    }
    let scan = scan_recovery(&env.dirs, 3);
    assert!(scan.truncated);
    let times: Vec<_> = scan.entries.iter().map(|e| e.modified_ms).collect();
    assert_eq!(times, [Some(9_999_000), Some(9_998_000), Some(9_997_000)]);
}

#[cfg(unix)]
#[test]
fn symlinks_are_not_followed() {
    let env = env();
    let dir = env.dirs.recovery_dir();
    fs::create_dir_all(&dir).unwrap();
    let real = dir.join("elsewhere.txt");
    fs::write(&real, "x").unwrap();
    let link = |n: u128| dir.join(format!("{}.openlab", uuid_for(n).hyphenated()));
    std::os::unix::fs::symlink(&real, link(1)).unwrap();
    // A link back to the folder itself.
    std::os::unix::fs::symlink(&dir, link(2)).unwrap();
    let scan = scan_recovery(&env.dirs, 100);
    assert!(scan.entries.is_empty());
    // Two links, and the unrelated text file.
    assert_eq!(scan.warnings.len(), 3, "{:?}", scan.warnings);
}

#[test]
fn backup_folder_names_give_the_workspace_id() {
    let env = env();
    let ws = example();
    let folder = env.dirs.backups_dir(ws.id, &env.show_path());
    let name = folder.file_name().unwrap().to_str().unwrap();
    let parsed = openlab_core::parse_store_name(name).unwrap();
    assert_eq!(parsed.workspace_id, ws.id);
    assert_eq!(parsed.path_hash.as_ref().map(String::len), Some(16));
}
