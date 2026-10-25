// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Recovery files (NFR-05) and per-machine settings (WS-06, WS-07).

mod common;

use std::fs;

use common::*;
use openlab_core::{DeviceInfo, MachineSettings, PatchMapping, PatchResolution, resolve_patch};
use openlab_engine::persist::{
    AppDirs, RecoveryCheck, check_recovery, clear_recovery, load_machine_settings,
    save_machine_settings, save_workspace, write_recovery,
};

const SHOW_T: u64 = 1_000_000;

fn show_with_mtime(env: &Env, ms: u64) {
    save_workspace(&env.dirs, &env.show_path(), &example(), 1, 20).unwrap();
    set_mtime(&env.show_path(), ms);
}

fn recovery_with_mtime(env: &Env, name: &str, ms: u64) {
    let mut ws = example();
    ws.name = name.into();
    let path = write_recovery(&env.dirs, &ws).unwrap();
    set_mtime(&path, ms);
}

#[test]
fn layout_under_app_support() {
    let env = env();
    let id = example().id;
    let root = env.root.path().join("support").join("openLab");
    assert_eq!(env.dirs.settings_path(), root.join("settings.json"));
    assert_eq!(
        env.dirs.backups_dir(id),
        root.join("Backups").join(id.to_string())
    );
    assert_eq!(
        env.dirs.recovery_path(id),
        root.join("Recovery").join(format!("{id}.openlab"))
    );
}

#[test]
fn no_recovery_file() {
    let env = env();
    show_with_mtime(&env, SHOW_T);
    assert!(matches!(
        check_recovery(&env.dirs, example().id, &env.show_path()),
        RecoveryCheck::None
    ));
}

#[test]
fn newer_recovery_file_is_offered() {
    let env = env();
    show_with_mtime(&env, SHOW_T);
    recovery_with_mtime(&env, "Unsaved work", SHOW_T + 1);
    let RecoveryCheck::Offer(offer) = check_recovery(&env.dirs, example().id, &env.show_path())
    else {
        panic!("expected an offer");
    };
    assert_eq!(offer.loaded.workspace.name, "Unsaved work");
    assert_eq!(offer.recovery_modified_ms, (SHOW_T + 1) as i64);
    assert_eq!(offer.show_modified_ms, Some(SHOW_T as i64));
    assert!(
        offer.path.exists(),
        "still there until the operator decides"
    );
}

#[test]
fn older_or_equal_recovery_file_is_stale_and_removed() {
    for recovery_t in [SHOW_T - 1, SHOW_T] {
        let env = env();
        show_with_mtime(&env, SHOW_T);
        recovery_with_mtime(&env, "Old", recovery_t);
        assert!(matches!(
            check_recovery(&env.dirs, example().id, &env.show_path()),
            RecoveryCheck::Stale
        ));
        assert!(!env.dirs.recovery_path(example().id).exists());
    }
}

#[test]
fn missing_show_file_offers_the_recovery() {
    let env = env();
    recovery_with_mtime(&env, "Never saved", 5);
    let check = check_recovery(&env.dirs, example().id, &env.show_path());
    assert!(matches!(check, RecoveryCheck::Offer(o) if o.show_modified_ms.is_none()));
}

#[test]
fn corrupt_recovery_file_is_set_aside_with_a_warning() {
    for junk in [
        &b"{\"format_version\": 1, \"workspa"[..],
        b"\xff\xfe\x00 not text",
        b"",
    ] {
        let env = env();
        show_with_mtime(&env, SHOW_T);
        let path = env.dirs.recovery_path(example().id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, junk).unwrap();
        set_mtime(&path, SHOW_T + 10);

        let check = check_recovery(&env.dirs, example().id, &env.show_path());
        let RecoveryCheck::Unusable { warning, .. } = check else {
            panic!("expected Unusable, got {check:?}");
        };
        assert!(warning.contains("unusable"), "{warning}");
        assert!(!path.exists());
        assert!(path.with_extension("openlab.corrupt").exists());
        // The next open is quiet.
        assert!(matches!(
            check_recovery(&env.dirs, example().id, &env.show_path()),
            RecoveryCheck::None
        ));
    }
}

#[test]
fn recovery_file_of_another_workspace_is_unusable() {
    let env = env();
    let mut other = example();
    other.id = openlab_core::WorkspaceId(uuid::Uuid::from_u128(99));
    let wrong = write_recovery(&env.dirs, &other).unwrap();
    let path = env.dirs.recovery_path(example().id);
    fs::rename(wrong, &path).unwrap();
    assert!(matches!(
        check_recovery(&env.dirs, example().id, &env.show_path()),
        RecoveryCheck::Unusable { .. }
    ));
}

#[test]
fn recovery_path_that_is_a_folder_is_unusable_not_a_crash() {
    let env = env();
    fs::create_dir_all(env.dirs.recovery_path(example().id)).unwrap();
    assert!(matches!(
        check_recovery(&env.dirs, example().id, &env.show_path()),
        RecoveryCheck::Unusable { .. }
    ));
}

#[test]
fn autosave_does_not_touch_the_show_file_and_clear_is_idempotent() {
    let env = env();
    show_with_mtime(&env, SHOW_T);
    let before = fs::read(env.show_path()).unwrap();
    let mut ws = example();
    ws.name = "Changed".into();
    let path = write_recovery(&env.dirs, &ws).unwrap();
    assert_eq!(fs::read(env.show_path()).unwrap(), before);
    assert!(path.exists());
    clear_recovery(&env.dirs, ws.id).unwrap();
    clear_recovery(&env.dirs, ws.id).unwrap();
    assert!(!path.exists());
}

// Settings

#[test]
fn missing_settings_are_defaults_without_warning() {
    let env = env();
    let loaded = load_machine_settings(&env.dirs);
    assert_eq!(loaded.settings, MachineSettings::default());
    assert!(loaded.warnings.is_empty());
}

#[test]
fn settings_round_trip_through_the_file() {
    let env = env();
    let ws = example();
    let patch = ws.patches[0].id;
    let mut settings = MachineSettings::default();
    settings
        .set_patch_mapping(
            ws.id,
            patch,
            PatchMapping {
                device: "Scarlett 18i20".into(),
                channels: vec![4, 5],
            },
        )
        .unwrap();
    save_machine_settings(&env.dirs, &settings).unwrap();
    let loaded = load_machine_settings(&env.dirs);
    assert!(loaded.warnings.is_empty());
    assert_eq!(loaded.settings, settings);
    // Device mapping lives here, never in the show file.
    save_workspace(&env.dirs, &env.show_path(), &ws, 1, 20).unwrap();
    let show = fs::read_to_string(env.show_path()).unwrap();
    assert!(!show.contains("Scarlett"));
}

#[test]
fn corrupt_settings_give_defaults_a_warning_and_keep_a_copy() {
    for junk in [&b"{ not json"[..], b"\xff\xfe\xfd", b""] {
        let env = env();
        let path = env.dirs.settings_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, junk).unwrap();
        let loaded = load_machine_settings(&env.dirs);
        assert_eq!(loaded.settings, MachineSettings::default());
        assert_eq!(loaded.warnings.len(), 1, "{:?}", loaded.warnings);
        assert_eq!(fs::read(path.with_extension("json.corrupt")).unwrap(), junk);
        // Saving afterwards works and replaces the bad file.
        save_machine_settings(&env.dirs, &loaded.settings).unwrap();
        assert!(load_machine_settings(&env.dirs).warnings.is_empty());
    }
}

#[test]
fn oversize_settings_file_is_refused_without_a_crash() {
    let env = env();
    let path = env.dirs.settings_path();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, vec![b' '; openlab_core::MAX_SETTINGS_BYTES + 10]).unwrap();
    let loaded = load_machine_settings(&env.dirs);
    assert_eq!(loaded.settings, MachineSettings::default());
    assert_eq!(loaded.warnings.len(), 1);
}

#[test]
fn unknown_device_mapping_resolves_without_error() {
    let env = env();
    let ws = example();
    let patch = &ws.patches[0];
    let mut settings = MachineSettings::default();
    settings
        .set_patch_mapping(
            ws.id,
            patch.id,
            PatchMapping {
                device: "Gone Interface".into(),
                channels: (0..patch.outputs.len() as u32).collect(),
            },
        )
        .unwrap();
    save_machine_settings(&env.dirs, &settings).unwrap();
    let loaded = load_machine_settings(&env.dirs).settings;
    let present = [DeviceInfo {
        name: "Built-in Output".into(),
        output_channels: 2,
    }];
    assert_eq!(
        resolve_patch(&loaded, ws.id, patch, &present),
        PatchResolution::DeviceMissing {
            device: "Gone Interface".into()
        }
    );
}

#[test]
fn system_dirs_end_in_open_lab() {
    // Does not touch the disk: only checks the path shape.
    if let Some(dirs) = AppDirs::system() {
        assert_eq!(
            dirs.settings_path().parent().unwrap().file_name().unwrap(),
            "openLab"
        );
    }
}
