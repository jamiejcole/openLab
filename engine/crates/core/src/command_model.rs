// SPDX-License-Identifier: GPL-3.0-or-later

//! Commands for the parts of the workspace that are not the cue tree (ED-07: undo covers
//! "routing and settings"): workspace settings, output patches, network patches and cue lists.
//! The variants are on [`Command`]; this module holds their value types and their application.
//!
//! Same rules as the cue commands (see `command.rs`), plus:
//! - A new or changed value must pass the checks load applies (`settings_problems`,
//!   `patch_problems`), so the result saves and loads back unchanged. Not when `restoring`
//!   (undo, redo, a transaction's rollback) for a value load accepts as it is (a bad hotkey
//!   name, a legacy passcode, port 0): the state existed, and putting it back must not be
//!   refused. Values load repairs (patches, rights) cannot be in a workspace, so stay checked.
//! - Removing a patch that a cue uses is refused (`PatchInUse`), unless `even_if_used` is set,
//!   which only the inverse of an add does: the patch may have been added to fix a cue that
//!   already pointed at it, and undo must restore exactly. Removing a cue list that holds cues
//!   is refused (`CueListNotEmpty`); so is removing the last list.
//! - A change that equals the current value is not an edit: the inverse is an empty
//!   [`Command::Transaction`], which [`History`](crate::History) does not record.

use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, Inverse};
use crate::cue::{CueKind, Hotkey, MAX_CHANNELS};
use crate::model::{
    CueId, CueListId, NetworkPatch, NetworkPatchId, NetworkProtocol, OutputPatch, PatchId,
    PatchOutput, Workspace, WorkspaceSettings,
};
use crate::osc_access::{MAX_OSC_PASSCODES, OscPasscode, OscRights, is_valid_new_passcode};
use crate::validate::{host_problem, name_problem, patch_problems, settings_problems, visit};

/// The inverse of a change that changed nothing. `History` ignores it.
pub(crate) fn no_change() -> Inverse {
    Command::Transaction {
        commands: Vec::new(),
    }
}

// ---------------------------------------------------------------------------------------------
// Settings

/// A transport key of [`TransportHotkeys`](crate::TransportHotkeys).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKey {
    Go,
    StopAll,
    PauseAll,
    ResumeAll,
    PlayheadNext,
    PlayheadPrevious,
}

/// One workspace setting. Each variant is the whole new value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkspaceSetting {
    /// Double-GO protection (PB-02), 0 to 10 s; 0 = off.
    MinGoInterval {
        value: f64,
    },
    /// Panic fade-out time (PB-03), 0 to 60 s.
    PanicDuration {
        value: f64,
    },
    /// Autosave period (NFR-05), 5 s to 1 h.
    AutosaveInterval {
        value: f64,
    },
    /// Global master level (PB-11).
    MasterDb {
        value: f64,
    },
    Hotkey {
        key: TransportKey,
        value: Option<Hotkey>,
    },
    AutoNumberStart {
        value: f64,
    },
    AutoNumberIncrement {
        value: f64,
    },
}

impl WorkspaceSetting {
    /// The same setting with the value `s` has now.
    fn current(&self, s: &WorkspaceSettings) -> Self {
        match self {
            Self::MinGoInterval { .. } => Self::MinGoInterval {
                value: s.min_go_interval,
            },
            Self::PanicDuration { .. } => Self::PanicDuration {
                value: s.panic_duration,
            },
            Self::AutosaveInterval { .. } => Self::AutosaveInterval {
                value: s.autosave_interval,
            },
            Self::MasterDb { .. } => Self::MasterDb { value: s.master_db },
            Self::Hotkey { key, .. } => Self::Hotkey {
                key: *key,
                value: hotkey_slot(s, *key).clone(),
            },
            Self::AutoNumberStart { .. } => Self::AutoNumberStart {
                value: s.auto_number_start,
            },
            Self::AutoNumberIncrement { .. } => Self::AutoNumberIncrement {
                value: s.auto_number_increment,
            },
        }
    }

    fn write(&self, s: &mut WorkspaceSettings) {
        match self {
            Self::MinGoInterval { value } => s.min_go_interval = *value,
            Self::PanicDuration { value } => s.panic_duration = *value,
            Self::AutosaveInterval { value } => s.autosave_interval = *value,
            Self::MasterDb { value } => s.master_db = *value,
            Self::Hotkey { key, value } => hotkey_slot_mut(s, *key).clone_from(value),
            Self::AutoNumberStart { value } => s.auto_number_start = *value,
            Self::AutoNumberIncrement { value } => s.auto_number_increment = *value,
        }
    }

    /// Whether two settings are the same field (coalescing).
    fn same_slot(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Hotkey { key: a, .. }, Self::Hotkey { key: b, .. }) => a == b,
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }
}

fn hotkey_field(key: TransportKey) -> &'static str {
    match key {
        TransportKey::Go => "settings.hotkeys.go",
        TransportKey::StopAll => "settings.hotkeys.stop_all",
        TransportKey::PauseAll => "settings.hotkeys.pause_all",
        TransportKey::ResumeAll => "settings.hotkeys.resume_all",
        TransportKey::PlayheadNext => "settings.hotkeys.playhead_next",
        TransportKey::PlayheadPrevious => "settings.hotkeys.playhead_previous",
    }
}

fn hotkey_slot(s: &WorkspaceSettings, key: TransportKey) -> &Option<Hotkey> {
    let keys = &s.hotkeys;
    match key {
        TransportKey::Go => &keys.go,
        TransportKey::StopAll => &keys.stop_all,
        TransportKey::PauseAll => &keys.pause_all,
        TransportKey::ResumeAll => &keys.resume_all,
        TransportKey::PlayheadNext => &keys.playhead_next,
        TransportKey::PlayheadPrevious => &keys.playhead_previous,
    }
}

fn hotkey_slot_mut(s: &mut WorkspaceSettings, key: TransportKey) -> &mut Option<Hotkey> {
    let keys = &mut s.hotkeys;
    match key {
        TransportKey::Go => &mut keys.go,
        TransportKey::StopAll => &mut keys.stop_all,
        TransportKey::PauseAll => &mut keys.pause_all,
        TransportKey::ResumeAll => &mut keys.resume_all,
        TransportKey::PlayheadNext => &mut keys.playhead_next,
        TransportKey::PlayheadPrevious => &mut keys.playhead_previous,
    }
}

fn set_setting(
    ws: &mut Workspace,
    setting: &WorkspaceSetting,
    restoring: bool,
) -> Result<Inverse, CommandError> {
    let old = setting.current(&ws.settings);
    let mut candidate = ws.settings.clone();
    setting.write(&mut candidate);
    // Only what this edit introduces is refused: a warning the show was loaded with (a bad
    // hotkey name is kept and warned about) must not block every other setting. An undo or
    // redo puts back a state that existed, so it is not judged at all.
    let existing = settings_problems(&ws.settings);
    // A hotkey being set is judged on its own: any invalid new name is refused.
    let own = match setting {
        WorkspaceSetting::Hotkey { key, .. } => Some(hotkey_field(*key)),
        _ => None,
    };
    let issues: Vec<_> = settings_problems(&candidate)
        .into_iter()
        .filter(|i| {
            matches!(i.kind, crate::validate::IssueKind::InvalidHotkey { field } if Some(field) == own)
                || !existing.contains(i)
        })
        .collect();
    if !restoring && !issues.is_empty() {
        return Err(CommandError::InvalidModel(issues));
    }
    if old == *setting {
        return Ok(no_change());
    }
    ws.settings = candidate;
    Ok(Command::SetWorkspaceSetting { setting: old })
}

// ---------------------------------------------------------------------------------------------
// OSC passcode table (NET-02, M4.5c)

/// Which row of the OSC passcode table [`Command::SetOscRights`] sets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OscRightsTarget {
    /// The rights of a peer that has not connected, or connected without a passcode.
    NoPasscode,
    Passcode {
        passcode: String,
    },
}

/// Row index of `passcode`. The error does not repeat it: it is a secret and errors are logged.
fn osc_row(ws: &Workspace, passcode: &str) -> Result<usize, CommandError> {
    ws.settings
        .osc_access
        .passcodes
        .iter()
        .position(|e| e.passcode == passcode)
        .ok_or(CommandError::InvalidValue(
            "that OSC passcode is not in the table",
        ))
}

fn check_rights(rights: OscRights) -> Result<(), CommandError> {
    if rights.is_consistent() {
        Ok(())
    } else {
        Err(CommandError::InvalidValue(
            "Edit and Control rights need View",
        ))
    }
}

fn add_osc_passcode(
    ws: &mut Workspace,
    index: usize,
    passcode: &str,
    rights: OscRights,
    restoring: bool,
) -> Result<Inverse, CommandError> {
    let table = &mut ws.settings.osc_access.passcodes;
    // An undo puts back a row that existed, whatever load let through (a legacy passcode that
    // is not digits); a new row is held to the rule.
    if !restoring && !is_valid_new_passcode(passcode) {
        return Err(CommandError::InvalidValue(
            "an OSC passcode is 1 to 32 digits",
        ));
    }
    check_rights(rights)?;
    if table.iter().any(|e| e.passcode == passcode) {
        return Err(CommandError::InvalidValue(
            "that OSC passcode is already in the table",
        ));
    }
    if table.len() >= MAX_OSC_PASSCODES && !restoring {
        return Err(CommandError::InvalidValue("the OSC passcode table is full"));
    }
    check_index(index, table.len(), true)?;
    table.insert(
        index,
        OscPasscode {
            passcode: passcode.to_owned(),
            rights,
        },
    );
    Ok(Command::RemoveOscPasscode {
        passcode: passcode.to_owned(),
    })
}

fn remove_osc_passcode(ws: &mut Workspace, passcode: &str) -> Result<Inverse, CommandError> {
    let at = osc_row(ws, passcode)?;
    let row = ws.settings.osc_access.passcodes.remove(at);
    Ok(Command::AddOscPasscode {
        index: at,
        passcode: row.passcode,
        rights: row.rights,
    })
}

fn set_osc_rights(
    ws: &mut Workspace,
    target: &OscRightsTarget,
    rights: OscRights,
) -> Result<Inverse, CommandError> {
    check_rights(rights)?;
    let slot = match target {
        OscRightsTarget::NoPasscode => &mut ws.settings.osc_access.no_passcode,
        OscRightsTarget::Passcode { passcode } => {
            let at = osc_row(ws, passcode)?;
            let row = ws
                .settings
                .osc_access
                .passcodes
                .get_mut(at)
                .ok_or(CommandError::Internal)?;
            &mut row.rights
        }
    };
    let old = *slot;
    if old == rights {
        return Ok(no_change());
    }
    *slot = rights;
    Ok(Command::SetOscRights {
        target: target.clone(),
        rights: old,
    })
}

// ---------------------------------------------------------------------------------------------
// Output patches

/// One editable property of an output patch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PatchProperty {
    Name {
        value: String,
    },
    /// Patch master level (PB-11).
    MasterDb {
        value: f64,
    },
    OutputLevel {
        output: usize,
        value: f64,
    },
    OutputMute {
        output: usize,
        value: bool,
    },
    OutputDelay {
        output: usize,
        value: f64,
    },
    OutputPolarity {
        output: usize,
        value: bool,
    },
    /// 1 to [`MAX_CHANNELS`]. New outputs start at 0 dB, unmuted, no delay. Shrinking drops the
    /// last outputs' settings (undo brings them back); cues keep their routing to them.
    ChannelCount {
        value: usize,
    },
    /// Replaces every output. What undoes a channel-count change.
    Outputs {
        value: Vec<PatchOutput>,
    },
}

impl PatchProperty {
    /// The same property with the value `patch` has now. `Err` for an output it lacks.
    fn current(&self, patch: &OutputPatch) -> Result<Self, CommandError> {
        let out = |output: usize| {
            patch
                .outputs
                .get(output)
                .ok_or(CommandError::PatchOutputOutOfRange {
                    patch: patch.id,
                    output,
                })
        };
        Ok(match self {
            Self::Name { .. } => Self::Name {
                value: patch.name.clone(),
            },
            Self::MasterDb { .. } => Self::MasterDb {
                value: patch.master_db,
            },
            Self::OutputLevel { output, .. } => Self::OutputLevel {
                output: *output,
                value: out(*output)?.level_db,
            },
            Self::OutputMute { output, .. } => Self::OutputMute {
                output: *output,
                value: out(*output)?.mute,
            },
            Self::OutputDelay { output, .. } => Self::OutputDelay {
                output: *output,
                value: out(*output)?.delay_ms,
            },
            Self::OutputPolarity { output, .. } => Self::OutputPolarity {
                output: *output,
                value: out(*output)?.polarity_inverted,
            },
            // Shrinking loses data, so both are undone by putting every output back.
            Self::ChannelCount { .. } | Self::Outputs { .. } => Self::Outputs {
                value: patch.outputs.clone(),
            },
        })
    }

    fn write(&self, patch: &mut OutputPatch) -> Result<(), CommandError> {
        fn out(patch: &mut OutputPatch, output: usize) -> Result<&mut PatchOutput, CommandError> {
            let id = patch.id;
            patch
                .outputs
                .get_mut(output)
                .ok_or(CommandError::PatchOutputOutOfRange { patch: id, output })
        }
        match self {
            Self::Name { value } => patch.name.clone_from(value),
            Self::MasterDb { value } => patch.master_db = *value,
            Self::OutputLevel { output, value } => out(patch, *output)?.level_db = *value,
            Self::OutputMute { output, value } => out(patch, *output)?.mute = *value,
            Self::OutputDelay { output, value } => out(patch, *output)?.delay_ms = *value,
            Self::OutputPolarity { output, value } => {
                out(patch, *output)?.polarity_inverted = *value;
            }
            Self::ChannelCount { value } => {
                // Bound before allocating: the count comes from the wire.
                if !(1..=MAX_CHANNELS as usize).contains(value) {
                    return Err(CommandError::InvalidValue("a patch has 1 to 64 outputs"));
                }
                patch.outputs.resize(*value, default_output());
            }
            Self::Outputs { value } => patch.outputs.clone_from(value),
        }
        Ok(())
    }

    fn same_slot(&self, other: &Self) -> bool {
        let output = |p: &Self| match p {
            Self::OutputLevel { output, .. }
            | Self::OutputMute { output, .. }
            | Self::OutputDelay { output, .. }
            | Self::OutputPolarity { output, .. } => Some(*output),
            _ => None,
        };
        std::mem::discriminant(self) == std::mem::discriminant(other)
            && output(self) == output(other)
    }
}

fn default_output() -> PatchOutput {
    PatchOutput {
        level_db: 0.0,
        mute: false,
        delay_ms: 0.0,
        polarity_inverted: false,
    }
}

fn add_patch(
    ws: &mut Workspace,
    index: usize,
    patch: &OutputPatch,
    restoring: bool,
) -> Result<Inverse, CommandError> {
    if ws.patches.iter().any(|p| p.id == patch.id) {
        return Err(CommandError::DuplicatePatchId(patch.id));
    }
    check_index(index, ws.patches.len(), true)?;
    // Load keeps an odd name, so an undo may put one back.
    if !restoring {
        check_name(&patch.name)?;
    }
    let issues = patch_problems(patch);
    if !issues.is_empty() {
        return Err(CommandError::InvalidModel(issues));
    }
    ws.patches.insert(index, patch.clone());
    Ok(Command::RemovePatch {
        id: patch.id,
        even_if_used: true,
    })
}

fn remove_patch(
    ws: &mut Workspace,
    id: PatchId,
    even_if_used: bool,
) -> Result<Inverse, CommandError> {
    let at = patch_index(ws, id)?;
    if !even_if_used
        && let Some(cue) = first_cue(
            ws,
            |k| matches!(k, CueKind::Audio { patch, .. } if *patch == id),
        )
    {
        return Err(CommandError::PatchInUse { patch: id, cue });
    }
    let patch = ws.patches.remove(at);
    Ok(Command::AddPatch { index: at, patch })
}

fn set_patch_property(
    ws: &mut Workspace,
    id: PatchId,
    property: &PatchProperty,
    restoring: bool,
) -> Result<Inverse, CommandError> {
    if let PatchProperty::Name { value } = property
        && !restoring
    {
        check_name(value)?;
    }
    let patch = ws
        .patches
        .iter_mut()
        .find(|p| p.id == id)
        .ok_or(CommandError::PatchNotFound(id))?;
    let old = property.current(patch)?;
    let mut candidate = patch.clone();
    property.write(&mut candidate)?;
    let issues = patch_problems(&candidate);
    if !issues.is_empty() {
        return Err(CommandError::InvalidModel(issues));
    }
    if candidate == *patch {
        return Ok(no_change());
    }
    *patch = candidate;
    Ok(Command::SetPatchProperty { id, property: old })
}

fn patch_index(ws: &Workspace, id: PatchId) -> Result<usize, CommandError> {
    ws.patches
        .iter()
        .position(|p| p.id == id)
        .ok_or(CommandError::PatchNotFound(id))
}

// ---------------------------------------------------------------------------------------------
// Network patches

/// One editable property of a network patch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NetworkPatchProperty {
    Name { value: String },
    Host { value: String },
    Port { value: u16 },
    Protocol { value: NetworkProtocol },
}

impl NetworkPatchProperty {
    fn current(&self, patch: &NetworkPatch) -> Self {
        match self {
            Self::Name { .. } => Self::Name {
                value: patch.name.clone(),
            },
            Self::Host { .. } => Self::Host {
                value: patch.host.clone(),
            },
            Self::Port { .. } => Self::Port { value: patch.port },
            Self::Protocol { .. } => Self::Protocol {
                value: patch.protocol,
            },
        }
    }

    fn write(&self, patch: &mut NetworkPatch) {
        match self {
            Self::Name { value } => patch.name.clone_from(value),
            Self::Host { value } => patch.host.clone_from(value),
            Self::Port { value } => patch.port = *value,
            Self::Protocol { value } => patch.protocol = *value,
        }
    }
}

fn add_network_patch(
    ws: &mut Workspace,
    index: usize,
    patch: &NetworkPatch,
    restoring: bool,
) -> Result<Inverse, CommandError> {
    if ws.network_patches.iter().any(|p| p.id == patch.id) {
        return Err(CommandError::DuplicateNetworkPatchId(patch.id));
    }
    check_index(index, ws.network_patches.len(), true)?;
    // Load accepts any port, so an undo or rollback may put back a patch on port 0.
    if !restoring {
        check_port(patch.port)?;
        check_name(&patch.name)?;
        host_problem(&patch.host).map_or(Ok(()), |why| Err(CommandError::InvalidValue(why)))?;
    }
    ws.network_patches.insert(index, patch.clone());
    Ok(Command::RemoveNetworkPatch {
        id: patch.id,
        even_if_used: true,
    })
}

fn remove_network_patch(
    ws: &mut Workspace,
    id: NetworkPatchId,
    even_if_used: bool,
) -> Result<Inverse, CommandError> {
    let at = network_index(ws, id)?;
    if !even_if_used
        && let Some(cue) = first_cue(
            ws,
            |k| matches!(k, CueKind::Network { destination, .. } if *destination == id),
        )
    {
        return Err(CommandError::NetworkPatchInUse { patch: id, cue });
    }
    let patch = ws.network_patches.remove(at);
    Ok(Command::AddNetworkPatch { index: at, patch })
}

fn set_network_patch_property(
    ws: &mut Workspace,
    id: NetworkPatchId,
    property: &NetworkPatchProperty,
    restoring: bool,
) -> Result<Inverse, CommandError> {
    let patch = ws
        .network_patches
        .iter_mut()
        .find(|p| p.id == id)
        .ok_or(CommandError::NetworkPatchNotFound(id))?;
    let old = property.current(patch);
    if !restoring {
        match property {
            NetworkPatchProperty::Port { value } => check_port(*value)?,
            NetworkPatchProperty::Name { value } => check_name(value)?,
            NetworkPatchProperty::Host { value } => {
                host_problem(value).map_or(Ok(()), |why| Err(CommandError::InvalidValue(why)))?;
            }
            NetworkPatchProperty::Protocol { .. } => {}
        }
    }
    if old == *property {
        return Ok(no_change());
    }
    property.write(patch);
    Ok(Command::SetNetworkPatchProperty { id, property: old })
}

/// Refuses a new name that load would warn about (length, control characters).
fn check_name(name: &str) -> Result<(), CommandError> {
    name_problem(name).map_or(Ok(()), |why| Err(CommandError::InvalidValue(why)))
}

fn check_port(port: u16) -> Result<(), CommandError> {
    if port == 0 {
        return Err(CommandError::InvalidValue("a port is 1 to 65535"));
    }
    Ok(())
}

fn network_index(ws: &Workspace, id: NetworkPatchId) -> Result<usize, CommandError> {
    ws.network_patches
        .iter()
        .position(|p| p.id == id)
        .ok_or(CommandError::NetworkPatchNotFound(id))
}

// ---------------------------------------------------------------------------------------------
// Workspace name

/// Renames the workspace (the `name` of `Upsert::Settings`). Not the file: Save As names that.
fn rename_workspace(
    ws: &mut Workspace,
    name: &str,
    restoring: bool,
) -> Result<Inverse, CommandError> {
    if !restoring {
        check_name(name)?;
    }
    if ws.name == name {
        return Ok(no_change());
    }
    let old = std::mem::replace(&mut ws.name, name.to_owned());
    Ok(Command::RenameWorkspace { name: old })
}

// ---------------------------------------------------------------------------------------------
// Cue lists

fn add_cue_list(
    ws: &mut Workspace,
    index: usize,
    id: CueListId,
    name: &str,
    restoring: bool,
) -> Result<Inverse, CommandError> {
    if ws.cue_lists.iter().any(|l| l.id == id) {
        return Err(CommandError::DuplicateCueListId(id));
    }
    check_index(index, ws.cue_lists.len(), true)?;
    if !restoring {
        check_name(name)?;
    }
    ws.cue_lists.insert(
        index,
        crate::model::CueList {
            id,
            name: name.to_owned(),
            cues: Vec::new(),
        },
    );
    Ok(Command::RemoveCueList { id })
}

fn remove_cue_list(ws: &mut Workspace, id: CueListId) -> Result<Inverse, CommandError> {
    let at = list_index(ws, id)?;
    if ws.cue_lists.get(at).is_none_or(|l| !l.cues.is_empty()) {
        return Err(CommandError::CueListNotEmpty(id));
    }
    if ws.cue_lists.len() == 1 {
        return Err(CommandError::LastCueList(id));
    }
    let list = ws.cue_lists.remove(at);
    Ok(Command::AddCueList {
        index: at,
        id,
        name: list.name,
    })
}

fn rename_cue_list(
    ws: &mut Workspace,
    id: CueListId,
    name: &str,
    restoring: bool,
) -> Result<Inverse, CommandError> {
    if !restoring {
        check_name(name)?;
    }
    let list = ws
        .cue_lists
        .iter_mut()
        .find(|l| l.id == id)
        .ok_or(CommandError::CueListNotFound(id))?;
    if list.name == name {
        return Ok(no_change());
    }
    let old = std::mem::replace(&mut list.name, name.to_owned());
    Ok(Command::RenameCueList { id, name: old })
}

fn list_index(ws: &Workspace, id: CueListId) -> Result<usize, CommandError> {
    ws.cue_lists
        .iter()
        .position(|l| l.id == id)
        .ok_or(CommandError::CueListNotFound(id))
}

// ---------------------------------------------------------------------------------------------
// Shared

/// `index` may be `len` when adding (the end), else the last place.
fn check_index(index: usize, len: usize, adding: bool) -> Result<(), CommandError> {
    let last = if adding { len } else { len.saturating_sub(1) };
    if index > last {
        return Err(CommandError::IndexOutOfRange { index, len: last });
    }
    Ok(())
}

/// Moves the item at `from` to `index` (counted after taking it out). Returns whether it moved.
fn reposition<T>(items: &mut Vec<T>, from: usize, index: usize) -> Result<bool, CommandError> {
    check_index(index, items.len(), false)?;
    if from == index {
        return Ok(false);
    }
    let item = items.remove(from);
    items.insert(index, item);
    Ok(true)
}

fn first_cue(ws: &Workspace, uses: impl Fn(&CueKind) -> bool) -> Option<CueId> {
    let mut found = None;
    for list in &ws.cue_lists {
        visit(&list.cues, &mut |cue| {
            if found.is_none() && uses(&cue.kind) {
                found = Some(cue.id);
            }
        });
        if found.is_some() {
            break;
        }
    }
    found
}

/// Applies the commands of this module. `None` if `command` is not one of them.
pub(crate) fn apply_model(
    ws: &mut Workspace,
    command: &Command,
    restoring: bool,
) -> Option<Result<Inverse, CommandError>> {
    Some(match command {
        Command::SetWorkspaceSetting { setting } => set_setting(ws, setting, restoring),
        Command::AddOscPasscode {
            index,
            passcode,
            rights,
        } => add_osc_passcode(ws, *index, passcode, *rights, restoring),
        Command::RemoveOscPasscode { passcode } => remove_osc_passcode(ws, passcode),
        Command::SetOscRights { target, rights } => set_osc_rights(ws, target, *rights),
        Command::AddPatch { index, patch } => add_patch(ws, *index, patch, restoring),
        Command::RemovePatch { id, even_if_used } => remove_patch(ws, *id, *even_if_used),
        Command::MovePatch { id, index } => patch_index(ws, *id).and_then(|from| {
            let moved = reposition(&mut ws.patches, from, *index)?;
            Ok(if moved {
                Command::MovePatch {
                    id: *id,
                    index: from,
                }
            } else {
                no_change()
            })
        }),
        Command::SetPatchProperty { id, property } => {
            set_patch_property(ws, *id, property, restoring)
        }
        Command::AddNetworkPatch { index, patch } => {
            add_network_patch(ws, *index, patch, restoring)
        }
        Command::RemoveNetworkPatch { id, even_if_used } => {
            remove_network_patch(ws, *id, *even_if_used)
        }
        Command::MoveNetworkPatch { id, index } => network_index(ws, *id).and_then(|from| {
            let moved = reposition(&mut ws.network_patches, from, *index)?;
            Ok(if moved {
                Command::MoveNetworkPatch {
                    id: *id,
                    index: from,
                }
            } else {
                no_change()
            })
        }),
        Command::SetNetworkPatchProperty { id, property } => {
            set_network_patch_property(ws, *id, property, restoring)
        }
        Command::AddCueList { index, id, name } => add_cue_list(ws, *index, *id, name, restoring),
        Command::RemoveCueList { id } => remove_cue_list(ws, *id),
        Command::MoveCueList { id, index } => list_index(ws, *id).and_then(|from| {
            let moved = reposition(&mut ws.cue_lists, from, *index)?;
            Ok(if moved {
                Command::MoveCueList {
                    id: *id,
                    index: from,
                }
            } else {
                no_change()
            })
        }),
        Command::RenameCueList { id, name } => rename_cue_list(ws, *id, name, restoring),
        Command::RenameWorkspace { name } => rename_workspace(ws, name, restoring),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------------------------
// Coalescing

impl Command {
    /// What value a pure value-setting command sets, as a hashable key (its kind, the item,
    /// and which field of it); `None` for any other command, and for the patch edits that change
    /// the layout. Two commands with equal keys set the same value.
    pub(crate) fn value_key(&self) -> Option<(u8, uuid::Uuid, u64)> {
        use std::hash::{Hash, Hasher};
        let hash = |parts: &dyn Fn(&mut std::collections::hash_map::DefaultHasher)| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            parts(&mut h);
            h.finish()
        };
        Some(match self {
            Self::SetCueProperty { id, property } => {
                (0, id.0, hash(&|h| std::mem::discriminant(property).hash(h)))
            }
            Self::SetWorkspaceSetting { setting } => (
                1,
                uuid::Uuid::nil(),
                hash(&|h| {
                    std::mem::discriminant(setting).hash(h);
                    if let WorkspaceSetting::Hotkey { key, .. } = setting {
                        (*key as u8).hash(h);
                    }
                }),
            ),
            Self::SetPatchProperty { id, property } => {
                let output = match property {
                    PatchProperty::ChannelCount { .. } | PatchProperty::Outputs { .. } => {
                        return None;
                    }
                    PatchProperty::OutputLevel { output, .. }
                    | PatchProperty::OutputMute { output, .. }
                    | PatchProperty::OutputDelay { output, .. }
                    | PatchProperty::OutputPolarity { output, .. } => *output,
                    _ => usize::MAX,
                };
                (
                    2,
                    id.0,
                    hash(&|h| {
                        std::mem::discriminant(property).hash(h);
                        output.hash(h);
                    }),
                )
            }
            Self::SetNetworkPatchProperty { id, property } => {
                (3, id.0, hash(&|h| std::mem::discriminant(property).hash(h)))
            }
            Self::RenameCueList { id, .. } => (4, id.0, 0),
            Self::RenameWorkspace { .. } => (5, uuid::Uuid::nil(), 0),
            _ => return None,
        })
    }

    /// Whether `self` and `other` each set the same single value, so that applying the older of
    /// the two undoes both. Only such pairs merge into one undo step (`History` coalescing).
    pub(crate) fn same_slot(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::SetCueProperty { id: a, property: p },
                Self::SetCueProperty { id: b, property: q },
            ) => a == b && p.same_slot(q),
            (
                Self::SetWorkspaceSetting { setting: a },
                Self::SetWorkspaceSetting { setting: b },
            ) => a.same_slot(b),
            (
                Self::SetPatchProperty { id: a, property: p },
                Self::SetPatchProperty { id: b, property: q },
            ) => a == b && p.same_slot(q),
            (
                Self::SetNetworkPatchProperty { id: a, property: p },
                Self::SetNetworkPatchProperty { id: b, property: q },
            ) => a == b && std::mem::discriminant(p) == std::mem::discriminant(q),
            (Self::RenameCueList { id: a, .. }, Self::RenameCueList { id: b, .. }) => a == b,
            (Self::RenameWorkspace { .. }, Self::RenameWorkspace { .. }) => true,
            (Self::MoveCue { id: a, .. }, Self::MoveCue { id: b, .. }) => a == b,
            (Self::MovePatch { id: a, .. }, Self::MovePatch { id: b, .. }) => a == b,
            (Self::MoveNetworkPatch { id: a, .. }, Self::MoveNetworkPatch { id: b, .. }) => a == b,
            (Self::MoveCueList { id: a, .. }, Self::MoveCueList { id: b, .. }) => a == b,
            _ => false,
        }
    }
}
