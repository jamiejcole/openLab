// SPDX-License-Identifier: GPL-3.0-or-later

//! Per-machine settings (WS-06, WS-07): what belongs to this computer and never travels with a
//! show file. Today that is the mapping of each workspace's logical patches to physical audio
//! devices, and the backup count. Text in, text out; the engine reads and writes
//! `<app-support>/openLab/settings.json`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::backup::{DEFAULT_BACKUP_KEEP, MAX_BACKUP_KEEP};
use crate::cue::MAX_CHANNELS;
use crate::media_path::{PathStyle, comparison_key, to_portable};
use crate::model::{OutputPatch, PatchId, WorkspaceId};

pub const SETTINGS_VERSION: u32 = 1;
/// Larger settings files are ignored.
pub const MAX_SETTINGS_BYTES: usize = 4 * 1024 * 1024;
/// Device channel numbers above this are refused; no interface has more.
pub const MAX_DEVICE_CHANNEL: u32 = 1023;
const MAX_DEVICE_NAME_BYTES: usize = 512;
const MAX_WORKSPACES: usize = 10_000;
/// Recently opened shows kept (WS-08).
pub const MAX_RECENT_FILES: usize = 10;
const MAX_RECENT_PATH_BYTES: usize = 4096;

/// Where one patch plays on this machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchMapping {
    /// The device's name as the device layer lists it.
    pub device: String,
    /// Device channel (0-based) for each logical output of the patch, in order.
    pub channels: Vec<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceMachineSettings {
    /// Keyed by patch id (lower-case hyphenated UUID).
    #[serde(default)]
    pub patches: BTreeMap<String, PatchMapping>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineSettings {
    pub settings_version: u32,
    /// Backups kept per workspace; 0 turns backups off (NFR-06).
    #[serde(default = "default_backup_keep")]
    pub backup_keep: u32,
    /// Keyed by workspace id (lower-case hyphenated UUID).
    #[serde(default)]
    pub workspaces: BTreeMap<String, WorkspaceMachineSettings>,
    /// Show files opened or saved lately, newest first, at most [`MAX_RECENT_FILES`] (WS-08).
    /// Native paths as the OS spells them.
    #[serde(default)]
    pub recent_files: Vec<String>,
}

fn default_backup_keep() -> u32 {
    DEFAULT_BACKUP_KEEP
}

impl Default for MachineSettings {
    fn default() -> Self {
        Self {
            settings_version: SETTINGS_VERSION,
            backup_keep: DEFAULT_BACKUP_KEEP,
            workspaces: BTreeMap::new(),
            recent_files: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedSettings {
    pub settings: MachineSettings,
    /// Why anything was replaced by defaults or dropped; empty for a clean file.
    pub warnings: Vec<String>,
}

/// Parses a settings file. Never fails: unreadable content gives the defaults and a warning,
/// invalid entries are dropped with a warning, out-of-range values are clamped.
pub fn load_settings(text: &str) -> LoadedSettings {
    let defaults = |warning: String| LoadedSettings {
        settings: MachineSettings::default(),
        warnings: vec![warning],
    };
    if text.len() > MAX_SETTINGS_BYTES {
        return defaults(format!(
            "settings file is {} bytes; the limit is {MAX_SETTINGS_BYTES}; using defaults",
            text.len()
        ));
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut settings: MachineSettings = match serde_json::from_str(text) {
        Ok(settings) => settings,
        Err(error) => {
            return defaults(format!(
                "settings file is unreadable ({error}); using defaults"
            ));
        }
    };
    if settings.settings_version > SETTINGS_VERSION {
        return defaults(format!(
            "settings file version {} is newer than this openLab supports ({SETTINGS_VERSION}); using defaults",
            settings.settings_version
        ));
    }
    let mut warnings = Vec::new();
    sanitise(&mut settings, &mut warnings);
    LoadedSettings { settings, warnings }
}

fn sanitise(settings: &mut MachineSettings, warnings: &mut Vec<String>) {
    settings.settings_version = SETTINGS_VERSION;
    if settings.backup_keep > MAX_BACKUP_KEEP {
        warnings.push(format!(
            "backup count {} is too large; using {MAX_BACKUP_KEEP}",
            settings.backup_keep
        ));
        settings.backup_keep = MAX_BACKUP_KEEP;
    }
    if settings.workspaces.len() > MAX_WORKSPACES {
        warnings.push(format!(
            "settings for more than {MAX_WORKSPACES} workspaces; the extra ones were dropped"
        ));
        let first_extra = settings.workspaces.keys().nth(MAX_WORKSPACES).cloned();
        if let Some(key) = first_extra {
            settings.workspaces.split_off(&key);
        }
    }
    // Keys are rebuilt in the one spelling lookups use (lower-case, hyphenated).
    let mut workspaces = BTreeMap::new();
    for (workspace, machine) in std::mem::take(&mut settings.workspaces) {
        let Ok(workspace_id) = Uuid::parse_str(&workspace) else {
            warnings.push(format!(
                "settings for \"{workspace}\" dropped: not a workspace id"
            ));
            continue;
        };
        let mut patches = BTreeMap::new();
        for (patch, mapping) in machine.patches {
            if let Some(reason) = mapping_problem(&patch, &mapping) {
                warnings.push(format!(
                    "workspace {workspace}, patch {patch}: {reason}; mapping dropped"
                ));
            } else if let Ok(patch_id) = Uuid::parse_str(&patch) {
                patches.entry(patch_id.to_string()).or_insert(mapping);
            }
        }
        workspaces
            .entry(workspace_id.to_string())
            .or_insert(WorkspaceMachineSettings { patches });
    }
    settings.workspaces = workspaces;
    let before = settings.recent_files.len();
    let mut seen = Vec::new();
    settings.recent_files.retain(|path| {
        let key = comparison_key(&to_portable(path, PathStyle::host()));
        let keep = !path.is_empty()
            && path.len() <= MAX_RECENT_PATH_BYTES
            && !path.contains('\0')
            && !seen.contains(&key);
        if keep {
            seen.push(key);
        }
        keep
    });
    settings.recent_files.truncate(MAX_RECENT_FILES);
    if settings.recent_files.len() != before {
        warnings.push("recent files: invalid, duplicate or surplus entries dropped".into());
    }
}

fn mapping_problem(patch: &str, mapping: &PatchMapping) -> Option<String> {
    if Uuid::parse_str(patch).is_err() {
        return Some("not a patch id".into());
    }
    if mapping.device.is_empty() || mapping.device.len() > MAX_DEVICE_NAME_BYTES {
        return Some("invalid device name".into());
    }
    if mapping.channels.is_empty() || mapping.channels.len() > MAX_CHANNELS as usize {
        return Some("wrong number of channels".into());
    }
    if mapping.channels.iter().any(|&c| c > MAX_DEVICE_CHANNEL) {
        return Some("device channel out of range".into());
    }
    None
}

/// Serialises settings (pretty-printed, trailing newline).
pub fn save_settings(settings: &MachineSettings) -> Result<String, serde_json::Error> {
    let mut text = serde_json::to_string_pretty(settings)?;
    text.push('\n');
    Ok(text)
}

impl MachineSettings {
    pub fn patch_mapping(&self, workspace: WorkspaceId, patch: PatchId) -> Option<&PatchMapping> {
        self.workspaces
            .get(&workspace.to_string())?
            .patches
            .get(&patch.to_string())
    }

    /// Records where `patch` plays on this machine. Refuses an invalid mapping and changes
    /// nothing.
    pub fn set_patch_mapping(
        &mut self,
        workspace: WorkspaceId,
        patch: PatchId,
        mapping: PatchMapping,
    ) -> Result<(), String> {
        if let Some(reason) = mapping_problem(&patch.to_string(), &mapping) {
            return Err(reason);
        }
        self.workspaces
            .entry(workspace.to_string())
            .or_default()
            .patches
            .insert(patch.to_string(), mapping);
        Ok(())
    }

    /// Puts `path` first in the recent files (WS-08), dropping an earlier entry for the same
    /// file (compared like media paths: case, NFC and `..` ignored) and the oldest beyond
    /// [`MAX_RECENT_FILES`]. An empty or absurdly long path is ignored.
    pub fn push_recent_file(&mut self, path: &str) {
        if path.is_empty() || path.len() > MAX_RECENT_PATH_BYTES || path.contains('\0') {
            return;
        }
        let key = comparison_key(&to_portable(path, PathStyle::host()));
        self.recent_files
            .retain(|p| comparison_key(&to_portable(p, PathStyle::host())) != key);
        self.recent_files.insert(0, path.to_owned());
        self.recent_files.truncate(MAX_RECENT_FILES);
    }

    /// Keeps the recent files for which `exists` is true. Returns whether any was dropped.
    pub fn prune_recent_files(&mut self, exists: impl Fn(&str) -> bool) -> bool {
        let before = self.recent_files.len();
        self.recent_files.retain(|p| exists(p));
        self.recent_files.len() != before
    }

    pub fn clear_patch_mapping(&mut self, workspace: WorkspaceId, patch: PatchId) {
        if let Some(machine) = self.workspaces.get_mut(&workspace.to_string()) {
            machine.patches.remove(&patch.to_string());
        }
    }
}

/// An output device this machine currently has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    pub output_channels: u32,
}

/// Where a patch can play right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatchResolution {
    /// Every logical output has a device channel.
    Mapped { device: String, channels: Vec<u32> },
    /// The operator has not mapped this patch on this machine (WS-07): ask once.
    Unmapped,
    /// Mapped to a device that is not present: the patch is silent and its cues show as broken.
    DeviceMissing { device: String },
    /// The device is present but has fewer channels than the mapping needs.
    ChannelsMissing {
        device: String,
        needed: u32,
        available: u32,
    },
    /// The mapping lists a different number of channels than the patch has outputs.
    OutputCountMismatch { mapped: usize, outputs: usize },
}

/// Resolves one patch against the devices present.
pub fn resolve_patch(
    settings: &MachineSettings,
    workspace: WorkspaceId,
    patch: &OutputPatch,
    devices: &[DeviceInfo],
) -> PatchResolution {
    let Some(mapping) = settings.patch_mapping(workspace, patch.id) else {
        return PatchResolution::Unmapped;
    };
    if mapping.channels.len() != patch.outputs.len() {
        return PatchResolution::OutputCountMismatch {
            mapped: mapping.channels.len(),
            outputs: patch.outputs.len(),
        };
    }
    let Some(device) = devices.iter().find(|d| d.name == mapping.device) else {
        return PatchResolution::DeviceMissing {
            device: mapping.device.clone(),
        };
    };
    let needed = mapping
        .channels
        .iter()
        .max()
        .map_or(0, |&max| max.saturating_add(1));
    if needed > device.output_channels {
        return PatchResolution::ChannelsMissing {
            device: mapping.device.clone(),
            needed,
            available: device.output_channels,
        };
    }
    PatchResolution::Mapped {
        device: mapping.device.clone(),
        channels: mapping.channels.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CueListId, PatchOutput, Workspace};

    fn ws(n: u128) -> WorkspaceId {
        WorkspaceId(Uuid::from_u128(n))
    }

    fn pid(n: u128) -> PatchId {
        PatchId(Uuid::from_u128(n))
    }

    fn patch(n: u128, outputs: usize) -> OutputPatch {
        OutputPatch {
            id: pid(n),
            name: "Main".into(),
            master_db: 0.0,
            outputs: vec![
                PatchOutput {
                    level_db: 0.0,
                    mute: false,
                    delay_ms: 0.0,
                    polarity_inverted: false,
                };
                outputs
            ],
        }
    }

    fn mapping(device: &str, channels: &[u32]) -> PatchMapping {
        PatchMapping {
            device: device.into(),
            channels: channels.to_vec(),
        }
    }

    fn device(name: &str, channels: u32) -> DeviceInfo {
        DeviceInfo {
            name: name.into(),
            output_channels: channels,
        }
    }

    #[test]
    fn round_trip() {
        let mut settings = MachineSettings {
            backup_keep: 7,
            ..MachineSettings::default()
        };
        settings
            .set_patch_mapping(ws(1), pid(2), mapping("Scarlett 18i20", &[2, 3]))
            .unwrap();
        settings
            .set_patch_mapping(ws(1), pid(3), mapping("Built-in Output", &[0]))
            .unwrap();
        let text = save_settings(&settings).unwrap();
        assert!(text.ends_with('\n'));
        let loaded = load_settings(&text);
        assert_eq!(loaded.warnings, Vec::<String>::new());
        assert_eq!(loaded.settings, settings);
        assert_eq!(
            save_settings(&loaded.settings).unwrap(),
            text,
            "stable bytes"
        );
    }

    #[test]
    fn minimal_file_gets_defaults_without_warning() {
        let loaded = load_settings(r#"{"settings_version": 1}"#);
        assert_eq!(loaded.settings, MachineSettings::default());
        assert!(loaded.warnings.is_empty());
        assert_eq!(loaded.settings.backup_keep, 20);
    }

    #[test]
    fn corrupt_text_gives_defaults_and_a_warning() {
        for text in [
            "",
            "not json",
            "{",
            "[]",
            "null",
            r#"{"settings_version": "x"}"#,
            "\0\0\0",
            r#"{}"#,
        ] {
            let loaded = load_settings(text);
            assert_eq!(loaded.settings, MachineSettings::default(), "{text:?}");
            assert_eq!(loaded.warnings.len(), 1, "{text:?}");
        }
    }

    #[test]
    fn newer_version_and_oversize_give_defaults() {
        let loaded = load_settings(r#"{"settings_version": 99, "backup_keep": 3}"#);
        assert_eq!(loaded.settings, MachineSettings::default());
        assert_eq!(loaded.warnings.len(), 1);
        let huge = " ".repeat(MAX_SETTINGS_BYTES + 1);
        assert_eq!(load_settings(&huge).warnings.len(), 1);
    }

    #[test]
    fn bom_and_unknown_keys_are_accepted() {
        let loaded = load_settings("\u{feff}{\"settings_version\": 1, \"window\": {\"x\": 1}}");
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn invalid_entries_are_dropped_not_fatal() {
        let good_ws = ws(1).to_string();
        let good_patch = pid(2).to_string();
        let text = format!(
            r#"{{"settings_version": 1, "backup_keep": 5000, "workspaces": {{
                "{good_ws}": {{"patches": {{
                    "{good_patch}": {{"device": "A", "channels": [0, 1]}},
                    "nope": {{"device": "A", "channels": [0]}},
                    "{p3}": {{"device": "", "channels": [0]}},
                    "{p4}": {{"device": "A", "channels": []}},
                    "{p5}": {{"device": "A", "channels": [5000]}}
                }}}},
                "not-a-uuid": {{"patches": {{}}}}
            }}}}"#,
            p3 = pid(3),
            p4 = pid(4),
            p5 = pid(5)
        );
        let loaded = load_settings(&text);
        assert_eq!(loaded.settings.backup_keep, MAX_BACKUP_KEEP);
        assert_eq!(loaded.warnings.len(), 6, "{:?}", loaded.warnings);
        assert_eq!(loaded.settings.workspaces.len(), 1);
        assert_eq!(
            loaded.settings.workspaces[&good_ws]
                .patches
                .keys()
                .collect::<Vec<_>>(),
            vec![&good_patch]
        );
    }

    #[test]
    fn upper_case_ids_are_normalised_so_lookups_match() {
        let (w, p) = (ws(0xabc), pid(0xdef));
        let text = format!(
            r#"{{"settings_version": 1, "workspaces": {{"{}": {{"patches": {{"{}":
                {{"device": "A", "channels": [1]}}}}}}}}}}"#,
            w.to_string().to_uppercase(),
            p.to_string().to_uppercase()
        );
        let loaded = load_settings(&text);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert_eq!(
            loaded.settings.patch_mapping(w, p),
            Some(&mapping("A", &[1]))
        );
    }

    #[test]
    fn too_many_workspaces_are_truncated_quickly() {
        let mut settings = MachineSettings::default();
        for n in 0..(MAX_WORKSPACES as u128 + 50) {
            settings
                .workspaces
                .insert(ws(n).to_string(), WorkspaceMachineSettings::default());
        }
        let loaded = load_settings(&save_settings(&settings).unwrap());
        assert_eq!(loaded.settings.workspaces.len(), MAX_WORKSPACES);
        assert_eq!(loaded.warnings.len(), 1);
    }

    #[test]
    fn set_refuses_invalid_mapping() {
        let mut settings = MachineSettings::default();
        assert!(
            settings
                .set_patch_mapping(ws(1), pid(1), mapping("", &[0]))
                .is_err()
        );
        assert!(
            settings
                .set_patch_mapping(ws(1), pid(1), mapping("A", &[]))
                .is_err()
        );
        assert!(
            settings
                .set_patch_mapping(ws(1), pid(1), mapping("A", &[0; 65]))
                .is_err()
        );
        assert_eq!(settings, MachineSettings::default());
    }

    #[test]
    fn clear_removes_mapping() {
        let mut settings = MachineSettings::default();
        settings
            .set_patch_mapping(ws(1), pid(1), mapping("A", &[0]))
            .unwrap();
        settings.clear_patch_mapping(ws(1), pid(1));
        assert_eq!(settings.patch_mapping(ws(1), pid(1)), None);
        settings.clear_patch_mapping(ws(9), pid(9));
    }

    #[test]
    fn resolution() {
        let mut settings = MachineSettings::default();
        settings
            .set_patch_mapping(ws(1), pid(1), mapping("A", &[2, 3]))
            .unwrap();
        let devices = [device("A", 4), device("B", 2)];
        let p = patch(1, 2);
        assert_eq!(
            resolve_patch(&settings, ws(1), &p, &devices),
            PatchResolution::Mapped {
                device: "A".into(),
                channels: vec![2, 3]
            }
        );
        // Other workspace and other patch: unmapped, not an error.
        assert_eq!(
            resolve_patch(&settings, ws(2), &p, &devices),
            PatchResolution::Unmapped
        );
        assert_eq!(
            resolve_patch(&settings, ws(1), &patch(2, 2), &devices),
            PatchResolution::Unmapped
        );
        // Unknown device.
        assert_eq!(
            resolve_patch(&settings, ws(1), &p, &[device("B", 8)]),
            PatchResolution::DeviceMissing { device: "A".into() }
        );
        assert_eq!(
            resolve_patch(&settings, ws(1), &p, &[]),
            PatchResolution::DeviceMissing { device: "A".into() }
        );
        // Device too small.
        assert_eq!(
            resolve_patch(&settings, ws(1), &p, &[device("A", 3)]),
            PatchResolution::ChannelsMissing {
                device: "A".into(),
                needed: 4,
                available: 3
            }
        );
        // The patch grew since it was mapped.
        assert_eq!(
            resolve_patch(&settings, ws(1), &patch(1, 3), &devices),
            PatchResolution::OutputCountMismatch {
                mapped: 2,
                outputs: 3
            }
        );
    }

    #[test]
    fn recent_files_are_newest_first_deduplicated_and_bounded() {
        let mut settings = MachineSettings::default();
        for n in 0..MAX_RECENT_FILES + 5 {
            settings.push_recent_file(&format!("/shows/show{n}.openlab"));
        }
        assert_eq!(settings.recent_files.len(), MAX_RECENT_FILES);
        assert_eq!(
            settings.recent_files[0],
            format!("/shows/show{}.openlab", MAX_RECENT_FILES + 4)
        );
        // Opening an older one again moves it to the front, spelled as just given.
        let oldest = settings.recent_files[MAX_RECENT_FILES - 1].clone();
        settings.push_recent_file(&oldest.to_uppercase());
        assert_eq!(settings.recent_files[0], oldest.to_uppercase());
        assert_eq!(settings.recent_files.len(), MAX_RECENT_FILES);
        assert!(!settings.recent_files.contains(&oldest));
        settings.push_recent_file("");
        settings.push_recent_file(&"x".repeat(5000));
        assert_eq!(settings.recent_files.len(), MAX_RECENT_FILES);
    }

    #[test]
    fn missing_recent_files_are_pruned() {
        let mut settings = MachineSettings::default();
        settings.push_recent_file("/a.openlab");
        settings.push_recent_file("/b.openlab");
        assert!(settings.prune_recent_files(|p| p == "/b.openlab"));
        assert_eq!(settings.recent_files, vec!["/b.openlab".to_owned()]);
        assert!(!settings.prune_recent_files(|_| true));
    }

    #[test]
    fn loading_cleans_recent_files_and_old_files_without_the_key_load() {
        let loaded = load_settings(r#"{"settings_version": 1}"#);
        assert!(loaded.warnings.is_empty());
        assert!(loaded.settings.recent_files.is_empty());
        let many: Vec<String> = (0..30).map(|n| format!("\"/s/{n}.openlab\"")).collect();
        let text = format!(
            r#"{{"settings_version": 1, "recent_files": ["", "/s/0.openlab", "/S/0.openlab", {}]}}"#,
            many.join(",")
        );
        let loaded = load_settings(&text);
        assert_eq!(loaded.settings.recent_files.len(), MAX_RECENT_FILES);
        assert_eq!(loaded.settings.recent_files[0], "/s/0.openlab");
        assert_eq!(loaded.warnings.len(), 1);
        let wrong = load_settings(r#"{"settings_version": 1, "recent_files": [1]}"#);
        assert!(wrong.settings.recent_files.is_empty());
    }

    #[test]
    fn new_empty_workspace_is_valid_and_round_trips() {
        let ws = Workspace::new_empty(ws(1), CueListId(Uuid::from_u128(2)), pid(3));
        let text = crate::save_show(&ws).unwrap();
        let loaded = crate::load_show(&text).unwrap();
        assert_eq!(loaded.workspace, ws);
        assert!(loaded.warnings.is_empty());
    }
}
