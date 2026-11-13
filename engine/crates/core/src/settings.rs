// SPDX-License-Identifier: GPL-3.0-or-later

//! Per-machine settings (WS-06, WS-07, AUD-21): what belongs to this computer and never travels
//! with a show file: the mapping of each workspace's logical patches to physical audio devices,
//! each device's buffer size, and the backup count. Text in, text out; the engine reads and writes
//! `<app-support>/openLab/settings.json`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::backup::{DEFAULT_BACKUP_KEEP, MAX_BACKUP_KEEP};
use crate::cue::MAX_CUE_OUTPUTS;
use crate::model::{OutputPatch, PatchId, WorkspaceId};

pub const SETTINGS_VERSION: u32 = 1;
/// Larger settings files are ignored.
pub const MAX_SETTINGS_BYTES: usize = 4 * 1024 * 1024;
/// Device channel numbers above this are refused; no interface has more.
pub const MAX_DEVICE_CHANNEL: u32 = 1023;
const MAX_DEVICE_NAME_BYTES: usize = 512;
const MAX_WORKSPACES: usize = 10_000;
/// The smallest buffer size, in frames, a device may be asked for (as QLab 5).
pub const MIN_BUFFER_FRAMES: u32 = 16;
/// The largest buffer size, in frames.
pub const MAX_BUFFER_FRAMES: u32 = 1024;
/// The buffer size of a device with none set.
pub const DEFAULT_BUFFER_FRAMES: u32 = 512;
/// Devices with a buffer size kept; more in a file are dropped.
const MAX_BUFFER_ENTRIES: usize = 256;

/// Where one patch plays on this machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchMapping {
    /// The device's name as the device layer lists it.
    pub device: String,
    /// Device channel (0-based) for each logical output of the patch, in order.
    pub channels: Vec<u32>,
}

impl PatchMapping {
    /// The explicit "deliberately unmapped" entry: no device, no channels. Stored so
    /// the default mapping does not map the patch again.
    pub fn none() -> Self {
        Self {
            device: String::new(),
            channels: Vec::new(),
        }
    }

    pub fn is_none(&self) -> bool {
        self.device.is_empty() && self.channels.is_empty()
    }
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
    /// The output device the engine opens at start, by the name `list_devices` gives
    /// (`set_output_device`). None: the system default device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_device: Option<String>,
    /// Frames per callback asked of each output device, by name ([`MIN_BUFFER_FRAMES`] to
    /// [`MAX_BUFFER_FRAMES`]); a device not here is asked for [`DEFAULT_BUFFER_FRAMES`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub buffer_frames: BTreeMap<String, u32>,
    /// Keyed by workspace id (lower-case hyphenated UUID).
    #[serde(default)]
    pub workspaces: BTreeMap<String, WorkspaceMachineSettings>,
    /// Keys this version does not know, kept so a rewrite (the engine adds default mappings)
    /// does not drop what a newer or older build wrote.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

fn default_backup_keep() -> u32 {
    DEFAULT_BACKUP_KEEP
}

impl Default for MachineSettings {
    fn default() -> Self {
        Self {
            settings_version: SETTINGS_VERSION,
            backup_keep: DEFAULT_BACKUP_KEEP,
            output_device: None,
            buffer_frames: BTreeMap::new(),
            workspaces: BTreeMap::new(),
            extra: BTreeMap::new(),
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
    let unreadable = |error: serde_json::Error| {
        defaults(format!(
            "settings file is unreadable ({error}); using defaults"
        ))
    };
    let mut value: serde_json::Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => return unreadable(error),
    };
    // A wrong-typed device choice drops only itself: the mappings beside it are the operator's.
    let mut warnings = Vec::new();
    if let Some(object) = value.as_object_mut()
        && object
            .get("output_device")
            .is_some_and(|v| !v.is_string() && !v.is_null())
    {
        object.remove("output_device");
        warnings.push("output device is not a name; using the system default".to_owned());
    }
    // Likewise a buffer size that is not a frame count drops only itself.
    if let Some(sizes) = value
        .as_object_mut()
        .and_then(|o| o.get_mut("buffer_frames"))
    {
        match sizes.as_object_mut() {
            Some(sizes) => sizes.retain(|device, frames| {
                let ok = frames.as_u64().is_some_and(|f| u32::try_from(f).is_ok());
                if !ok {
                    warnings.push(format!(
                        "buffer size of \"{device}\" is not a frame count; using {DEFAULT_BUFFER_FRAMES}"
                    ));
                }
                ok
            }),
            None => {
                *sizes = serde_json::Value::Object(serde_json::Map::new());
                warnings.push(format!(
                    "buffer sizes are not a list of devices; using {DEFAULT_BUFFER_FRAMES} frames"
                ));
            }
        }
    }
    let mut settings: MachineSettings = match serde_json::from_value(value) {
        Ok(settings) => settings,
        Err(error) => return unreadable(error),
    };
    if settings.settings_version > SETTINGS_VERSION {
        return defaults(format!(
            "settings file version {} is newer than this openLab supports ({SETTINGS_VERSION}); using defaults",
            settings.settings_version
        ));
    }
    sanitise(&mut settings, &mut warnings);
    LoadedSettings { settings, warnings }
}

fn sanitise(settings: &mut MachineSettings, warnings: &mut Vec<String>) {
    settings.settings_version = SETTINGS_VERSION;
    // Written by an earlier build; recent files are app-side now (WS-08).
    settings.extra.remove("recent_files");
    if settings.backup_keep > MAX_BACKUP_KEEP {
        warnings.push(format!(
            "backup count {} is too large; using {MAX_BACKUP_KEEP}",
            settings.backup_keep
        ));
        settings.backup_keep = MAX_BACKUP_KEEP;
    }
    if settings
        .output_device
        .as_ref()
        .is_some_and(|d| d.is_empty() || d.len() > MAX_DEVICE_NAME_BYTES)
    {
        warnings.push("output device name is invalid; using the system default".into());
        settings.output_device = None;
    }
    let sizes = std::mem::take(&mut settings.buffer_frames);
    for (device, frames) in sizes {
        if settings.buffer_frames.len() >= MAX_BUFFER_ENTRIES {
            warnings.push(format!(
                "buffer sizes for more than {MAX_BUFFER_ENTRIES} devices; the extra ones were dropped"
            ));
            break;
        }
        match buffer_problem(&device, frames) {
            Some(reason) => warnings.push(format!(
                "buffer size of \"{device}\": {reason}; using {DEFAULT_BUFFER_FRAMES}"
            )),
            None => {
                settings.buffer_frames.insert(device, frames);
            }
        }
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
}

/// Why `frames` cannot be device `device`'s buffer size.
fn buffer_problem(device: &str, frames: u32) -> Option<String> {
    if device.is_empty() || device.len() > MAX_DEVICE_NAME_BYTES {
        return Some("invalid device name".into());
    }
    if !(MIN_BUFFER_FRAMES..=MAX_BUFFER_FRAMES).contains(&frames) {
        return Some(format!(
            "{frames} frames is outside {MIN_BUFFER_FRAMES} to {MAX_BUFFER_FRAMES}"
        ));
    }
    None
}

fn mapping_problem(patch: &str, mapping: &PatchMapping) -> Option<String> {
    if Uuid::parse_str(patch).is_err() {
        return Some("not a patch id".into());
    }
    if mapping.is_none() {
        return None;
    }
    if mapping.device.is_empty() || mapping.device.len() > MAX_DEVICE_NAME_BYTES {
        return Some("invalid device name".into());
    }
    if mapping.channels.is_empty() || mapping.channels.len() > MAX_CUE_OUTPUTS as usize {
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
    /// Where `patch` plays; `None` for a patch with no entry and for one the operator cleared.
    pub fn patch_mapping(&self, workspace: WorkspaceId, patch: PatchId) -> Option<&PatchMapping> {
        self.patch_entry(workspace, patch)
            .filter(|mapping| !mapping.is_none())
    }

    /// The stored entry, a cleared one included: the default mapping leaves a patch that has
    /// any entry alone.
    pub fn patch_entry(&self, workspace: WorkspaceId, patch: PatchId) -> Option<&PatchMapping> {
        self.workspaces
            .get(&workspace.to_string())?
            .patches
            .get(&patch.to_string())
    }

    /// Chooses the output device the engine opens at its next start; `None` goes back to the
    /// system default. Refuses a name no device layer could list and changes nothing.
    pub fn set_output_device(&mut self, device: Option<String>) -> Result<(), String> {
        if let Some(name) = &device
            && (name.is_empty() || name.len() > MAX_DEVICE_NAME_BYTES)
        {
            return Err("invalid device name".into());
        }
        self.output_device = device;
        Ok(())
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

    /// The buffer size, in frames, device `device` is asked for.
    pub fn buffer_frames_for(&self, device: &str) -> u32 {
        self.buffer_frames
            .get(device)
            .copied()
            .unwrap_or(DEFAULT_BUFFER_FRAMES)
    }

    /// Sets device `device`'s buffer size. Refuses a size outside [`MIN_BUFFER_FRAMES`] to
    /// [`MAX_BUFFER_FRAMES`], an invalid name, or a new device beyond the entries kept, and
    /// changes nothing.
    pub fn set_buffer_frames(&mut self, device: &str, frames: u32) -> Result<(), String> {
        if let Some(reason) = buffer_problem(device, frames) {
            return Err(reason);
        }
        if !self.buffer_frames.contains_key(device)
            && self.buffer_frames.len() >= MAX_BUFFER_ENTRIES
        {
            return Err(format!(
                "buffer sizes are kept for at most {MAX_BUFFER_ENTRIES} devices"
            ));
        }
        self.buffer_frames.insert(device.to_owned(), frames);
        Ok(())
    }

    /// Stores "deliberately unmapped" for `patch` (an empty entry, [`PatchMapping::none`]).
    pub fn clear_patch_mapping(&mut self, workspace: WorkspaceId, patch: PatchId) {
        self.workspaces
            .entry(workspace.to_string())
            .or_default()
            .patches
            .insert(patch.to_string(), PatchMapping::none());
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

    /// AUD-21: a device's buffer size is 16 to 1024 frames, 512 when none is set; anything else
    /// is refused and changes nothing.
    #[test]
    fn a_buffer_size_outside_16_to_1024_is_refused_and_512_is_the_default() {
        let mut s = MachineSettings::default();
        assert_eq!(s.buffer_frames_for("Scarlett"), DEFAULT_BUFFER_FRAMES);
        assert_eq!(DEFAULT_BUFFER_FRAMES, 512);
        for bad in [0, 15, 1025, u32::MAX] {
            assert!(s.set_buffer_frames("Scarlett", bad).is_err(), "{bad}");
        }
        assert!(s.set_buffer_frames("", 256).is_err());
        assert!(s.buffer_frames.is_empty());
        for good in [16, 128, 1024] {
            assert_eq!(s.set_buffer_frames("Scarlett", good), Ok(()));
            assert_eq!(s.buffer_frames_for("Scarlett"), good);
        }
        assert_eq!(s.buffer_frames_for("Other"), 512);
        // Kept for a bounded number of devices; an existing one can still change.
        for k in 1..MAX_BUFFER_ENTRIES {
            s.set_buffer_frames(&format!("d{k}"), 64).unwrap();
        }
        assert!(s.set_buffer_frames("one more", 64).is_err());
        assert_eq!(s.set_buffer_frames("Scarlett", 256), Ok(()));
    }

    /// A settings file's buffer sizes are checked as `set_buffer_frames` checks them: a bad one
    /// drops only itself, with a warning, and round-trips otherwise.
    #[test]
    fn a_bad_buffer_size_in_the_file_is_dropped_with_a_warning() {
        let loaded = load_settings(
            r#"{"settings_version":1,"buffer_frames":{"A":128,"B":4096,"C":-1,"D":"x","":64}}"#,
        );
        assert_eq!(
            loaded.settings.buffer_frames,
            BTreeMap::from([("A".to_owned(), 128)])
        );
        assert_eq!(loaded.warnings.len(), 4, "{:?}", loaded.warnings);
        let text = save_settings(&loaded.settings).unwrap();
        assert_eq!(load_settings(&text).settings, loaded.settings);
        let wrong = load_settings(r#"{"settings_version":1,"buffer_frames":[1,2]}"#);
        assert!(wrong.settings.buffer_frames.is_empty());
        assert_eq!(wrong.warnings.len(), 1);
        // Nothing set: nothing written.
        let empty = save_settings(&MachineSettings::default()).unwrap();
        assert!(!empty.contains("buffer_frames"));
    }

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
                    name: String::new(),
                    level_db: 0.0,
                    mute: false,
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
    fn unknown_keys_round_trip_but_recent_files_is_dropped() {
        let text = r#"{"settings_version":1,"future":{"x":1},"recent_files":["a"]}"#;
        let loaded = load_settings(text);
        assert!(loaded.warnings.is_empty());
        let out = save_settings(&loaded.settings).unwrap();
        assert!(out.contains("\"future\""), "{out}");
        assert!(!out.contains("recent_files"), "{out}");
    }

    #[test]
    fn an_invalid_output_device_name_is_dropped_with_a_warning() {
        let loaded = load_settings(r#"{"settings_version": 1, "output_device": ""}"#);
        assert_eq!(loaded.settings.output_device, None);
        assert_eq!(loaded.warnings.len(), 1);
        let loaded = load_settings(r#"{"settings_version": 1, "output_device": "Scarlett"}"#);
        assert_eq!(loaded.settings.output_device.as_deref(), Some("Scarlett"));
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn a_wrong_typed_output_device_keeps_the_mappings() {
        let ws = "00000000-0000-0000-0000-000000000001";
        let p = "00000000-0000-0000-0000-000000000002";
        let text = format!(
            r#"{{"settings_version": 1, "output_device": 5, "workspaces": {{"{ws}": {{"patches": {{"{p}": {{"device": "Scarlett", "channels": [0, 1]}}}}}}}}}}"#
        );
        let loaded = load_settings(&text);
        assert_eq!(loaded.settings.output_device, None);
        assert_eq!(loaded.warnings.len(), 1, "{:?}", loaded.warnings);
        assert_eq!(loaded.settings.workspaces.len(), 1, "mappings survive");
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
    fn a_cleared_mapping_is_stored_and_survives_a_reload() {
        let mut settings = MachineSettings::default();
        settings
            .set_patch_mapping(ws(1), pid(2), mapping("Scarlett 18i20", &[2, 3]))
            .unwrap();
        settings.clear_patch_mapping(ws(1), pid(2));
        assert_eq!(settings.patch_mapping(ws(1), pid(2)), None);
        assert_eq!(
            settings.patch_entry(ws(1), pid(2)),
            Some(&PatchMapping::none())
        );
        let loaded = load_settings(&save_settings(&settings).unwrap());
        assert_eq!(loaded.warnings, Vec::<String>::new());
        assert_eq!(loaded.settings, settings);
        assert_eq!(
            resolve_patch(&loaded.settings, ws(1), &patch(2, 2), &[]),
            PatchResolution::Unmapped
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
                .set_patch_mapping(ws(1), pid(1), mapping("A", &[0; 129]))
                .is_err()
        );
        assert_eq!(settings, MachineSettings::default());
        // One device channel per cue output: a 128-output patch can be mapped.
        assert!(
            settings
                .set_patch_mapping(ws(1), pid(1), mapping("A", &[0; 128]))
                .is_ok()
        );
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
    fn a_settings_file_with_the_retired_recent_files_key_still_loads() {
        // Recent files were kept here for a while and are now the app's (WS-08).
        let loaded = load_settings(
            r#"{"settings_version": 1, "backup_keep": 7, "recent_files": ["/a.openlab", 1]}"#,
        );
        assert!(loaded.warnings.is_empty());
        assert_eq!(loaded.settings.backup_keep, 7);
    }

    #[test]
    fn new_empty_workspace_is_valid_and_round_trips() {
        let ws = Workspace::new_empty(
            ws(1),
            CueListId(Uuid::from_u128(2)),
            pid(3),
            crate::NetworkPatchId(Uuid::from_u128(4)),
        );
        let text = crate::save_show(&ws).unwrap();
        let loaded = crate::load_show(&text).unwrap();
        assert_eq!(loaded.workspace, ws);
        assert!(loaded.warnings.is_empty());
    }

    /// QLab 5 (manual p. 111): a new workspace starts with a network patch that sends to this
    /// computer. Only `new_empty` adds it; a saved show with none keeps none when it loads.
    #[test]
    fn a_new_workspace_has_a_loopback_network_patch_but_a_loaded_show_keeps_its_own() {
        let mut ws = Workspace::new_empty(
            ws(1),
            CueListId(Uuid::from_u128(2)),
            pid(3),
            crate::NetworkPatchId(Uuid::from_u128(4)),
        );
        assert_eq!(ws.network_patches.len(), 1);
        let patch = &ws.network_patches[0];
        assert_eq!(patch.id, crate::NetworkPatchId(Uuid::from_u128(4)));
        assert_eq!(
            (patch.host.as_str(), patch.port, patch.protocol),
            ("127.0.0.1", 53000, crate::NetworkProtocol::Udp)
        );
        ws.network_patches.clear();
        let loaded = crate::load_show(&crate::save_show(&ws).unwrap()).unwrap();
        assert!(loaded.workspace.network_patches.is_empty());
        assert!(loaded.warnings.is_empty());
    }
}
