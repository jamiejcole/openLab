// SPDX-License-Identifier: GPL-3.0-or-later

//! The `.openlab` show file (NFR-40): versioned JSON, specified in `docs/file-format.md`.
//! Works on strings only; reading and writing files is the engine's job.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cue::{Cue, CueKind};
use crate::model::Workspace;
use crate::validate::{IssueKind, Severity, ValidationIssue, repair};

/// The format version this build writes, and the newest it reads.
pub const FORMAT_VERSION: u32 = 4;
/// Larger inputs are refused before parsing.
pub const MAX_SHOW_FILE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct LoadedShow {
    pub workspace: Workspace,
    /// Problems found and repaired, or left for the affected cues to show as broken.
    pub warnings: Vec<ValidationIssue>,
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("show file is {size} bytes; the limit is {limit}")]
    TooLarge { size: usize, limit: usize },
    #[error("not a valid show file: {0}")]
    Syntax(serde_json::Error),
    #[error("show file has no format_version")]
    MissingVersion,
    #[error(
        "show file format version {found} is newer than this openLab supports \
         (version {supported}); open it with a newer openLab"
    )]
    TooNew { found: u64, supported: u32 },
    #[error("cannot upgrade show file from format version {from}: {reason}")]
    Migration { from: u32, reason: String },
    #[error("show file content is invalid: {0}")]
    Model(serde_json::Error),
    #[error("show file refused: {}", join(.0))]
    Invalid(Vec<ValidationIssue>),
}

fn join(issues: &[ValidationIssue]) -> String {
    issues
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("could not serialise workspace: {0}")]
    Serialize(serde_json::Error),
    /// The workspace breaks an invariant (e.g. duplicate ids, a non-finite number), so the
    /// written file would be refused on load. Nothing should be written.
    #[error("workspace would not load back: {0}")]
    WouldNotLoad(LoadError),
}

#[derive(Deserialize)]
struct VersionProbe {
    format_version: Option<u64>,
}

#[derive(Deserialize)]
struct ShowFileIn {
    workspace: Workspace,
}

#[derive(Serialize)]
struct ShowFileOut<'a> {
    format_version: u32,
    workspace: &'a Workspace,
}

/// Parses, migrates to [`FORMAT_VERSION`] and validates a show file. Never panics.
pub fn load_show(text: &str) -> Result<LoadedShow, LoadError> {
    if text.len() > MAX_SHOW_FILE_BYTES {
        return Err(LoadError::TooLarge {
            size: text.len(),
            limit: MAX_SHOW_FILE_BYTES,
        });
    }
    // Some Windows editors prepend a byte-order mark (NFR-32).
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    // serde_json's recursion limit bounds nesting in every parse below.
    let probe: VersionProbe = serde_json::from_str(text).map_err(LoadError::Syntax)?;
    let version = probe.format_version.ok_or(LoadError::MissingVersion)?;
    let mut from = FORMAT_VERSION;
    let file: ShowFileIn = match u32::try_from(version) {
        // Parse straight from text so errors carry line and column.
        Ok(FORMAT_VERSION) => serde_json::from_str(text).map_err(LoadError::Model)?,
        Ok(old) if old < FORMAT_VERSION => {
            from = old;
            let value = serde_json::from_str(text).map_err(LoadError::Syntax)?;
            serde_json::from_value(migrate(value, old)?).map_err(LoadError::Model)?
        }
        _ => {
            return Err(LoadError::TooNew {
                found: version,
                supported: FORMAT_VERSION,
            });
        }
    };
    let mut workspace = file.workspace;
    let mut issues = repair(&mut workspace);
    if from < 3 {
        issues.extend(group_fades_now_relative(&workspace));
    }
    if issues.iter().any(|issue| issue.severity == Severity::Error) {
        let errors = issues
            .into_iter()
            .filter(|i| i.severity == Severity::Error)
            .collect();
        return Err(LoadError::Invalid(errors));
    }
    Ok(LoadedShow {
        workspace,
        warnings: issues,
    })
}

/// Before v3 a Fade aimed at a group set each child's levels to its values; since v3 (M2.28,
/// QLab check F2) it moves them by its values. No migration can keep the old meaning (it depends
/// on each child's level when the Fade runs), so every such Fade of an older file is pointed
/// out for the user to check. Iterative, so nesting depth cannot overflow the stack.
fn group_fades_now_relative(workspace: &Workspace) -> Vec<ValidationIssue> {
    let mut out = Vec::new();
    for list in &workspace.cue_lists {
        let mut stack: Vec<std::slice::Iter<'_, Cue>> = vec![list.cues.iter()];
        while let Some(level) = stack.last_mut() {
            let Some(cue) = level.next() else {
                stack.pop();
                continue;
            };
            if let CueKind::Fade {
                target: Some(target),
                ..
            } = &cue.kind
                && matches!(
                    workspace.find_cue(*target).map(|t| &t.kind),
                    Some(CueKind::Group { .. })
                )
            {
                out.push(ValidationIssue {
                    severity: Severity::Warning,
                    cue_id: Some(cue.id),
                    kind: IssueKind::GroupFadeNowRelative,
                });
            }
            stack.push(cue.children().iter());
        }
    }
    out
}

/// Serialises a workspace: pretty-printed, fields in a fixed order, trailing newline, so equal
/// workspaces give byte-identical files. Refuses a workspace whose file would not load back.
pub fn save_show(workspace: &Workspace) -> Result<String, SaveError> {
    let file = ShowFileOut {
        format_version: FORMAT_VERSION,
        workspace,
    };
    let mut text = serde_json::to_string_pretty(&file).map_err(SaveError::Serialize)?;
    text.push('\n');
    load_show(&text).map_err(SaveError::WouldNotLoad)?;
    Ok(text)
}

/// Upgrades a parsed show file from format version `from` to [`FORMAT_VERSION`], one version
/// at a time. Each step is a pure function of the document.
pub fn migrate(mut document: Value, from: u32) -> Result<Value, LoadError> {
    for version in from..FORMAT_VERSION {
        document = match version {
            0 => v0_to_v1(document),
            1 => v1_to_v2(document),
            2 => v2_to_v3(document),
            3 => v3_to_v4(document),
            _ => Err(format!("no upgrade step from version {version}")),
        }
        .map_err(|reason| LoadError::Migration {
            from: version,
            reason,
        })?;
    }
    Ok(document)
}

/// v0 was a pre-release development format; it called `cue_lists` `lists`. It exists so the
/// migration chain is exercised before the first real format change.
fn v0_to_v1(mut document: Value) -> Result<Value, String> {
    let root = document
        .as_object_mut()
        .ok_or("top level is not an object")?;
    let workspace = root
        .get_mut("workspace")
        .and_then(Value::as_object_mut)
        .ok_or("workspace is not an object")?;
    if let Some(lists) = workspace.remove("lists") {
        workspace.insert("cue_lists".into(), lists);
    }
    root.insert("format_version".into(), 1.into());
    Ok(document)
}

/// v2 added each cue's `second_trigger` and `second_trigger_on_release` (M2.14). A v1 reader
/// ignored unknown keys, so every v1 cue meant the defaults: they are written over anything a
/// hand edit may have put there.
fn v1_to_v2(document: Value) -> Result<Value, String> {
    upgrade_cues(document, 2, |cue| {
        cue.insert("second_trigger".into(), "nothing".into());
        cue.insert("second_trigger_on_release".into(), false.into());
    })
}

/// v3 added each Devamp cue's `devamp_mode` (M2.28, QLab check D7). A v2 Devamp cue left
/// whatever loop was playing, which is the mode `current_slice` ("Devamp currently looping
/// slice"); a v2 reader ignored unknown keys, so it is written over anything a hand edit put
/// there.
fn v2_to_v3(document: Value) -> Result<Value, String> {
    upgrade_cues(document, 3, |cue| {
        if let Some(Value::Object(kind)) = cue.get_mut("kind")
            && kind.get("type").and_then(Value::as_str) == Some("devamp")
        {
            kind.insert("devamp_mode".into(), "current_slice".into());
        }
    })
}

/// v4 replaced the single `osc_passcode` by the rights table `osc_access` (M4.5c). A passcode
/// becomes the one row of the table with all rights and No Passcode gets none, which is what the
/// show did (nothing without `/connect`, everything with it); no passcode (null or empty) becomes
/// No Passcode with all rights, an open show as before. A v3 reader ignored unknown keys, so a
/// hand-added `osc_access` is replaced. A document that is not shaped like this (no settings
/// object, a passcode that is not text) is left for the model parse to refuse, as the cue steps
/// do.
fn v3_to_v4(mut document: Value) -> Result<Value, String> {
    let root = document
        .as_object_mut()
        .ok_or("top level is not an object")?;
    if let Some(settings) = root
        .get_mut("workspace")
        .and_then(|w| w.get_mut("settings"))
        .and_then(Value::as_object_mut)
    {
        let all = serde_json::json!({ "view": true, "edit": true, "control": true });
        let none = serde_json::json!({ "view": false, "edit": false, "control": false });
        let access = match settings.get("osc_passcode") {
            None | Some(Value::Null) => Some(serde_json::json!({
                "no_passcode": all,
                "passcodes": [],
            })),
            Some(Value::String(code)) if code.is_empty() => Some(serde_json::json!({
                "no_passcode": all,
                "passcodes": [],
            })),
            Some(Value::String(code)) => Some(serde_json::json!({
                "no_passcode": none,
                "passcodes": [{ "passcode": code, "rights": all }],
            })),
            Some(_) => None,
        };
        if let Some(access) = access {
            settings.remove("osc_passcode");
            settings.insert("osc_access".into(), access);
        }
    }
    root.insert("format_version".into(), 4.into());
    Ok(document)
}

/// Applies `upgrade` to every cue object of the document (any depth) and sets its
/// `format_version` to `to`. Iterative, so deep nesting cannot overflow the stack. Anything
/// that is not shaped like a cue is left for the model parse to refuse.
fn upgrade_cues(
    mut document: Value,
    to: u32,
    mut upgrade: impl FnMut(&mut serde_json::Map<String, Value>),
) -> Result<Value, String> {
    let root = document
        .as_object_mut()
        .ok_or("top level is not an object")?;
    let workspace = root
        .get_mut("workspace")
        .and_then(Value::as_object_mut)
        .ok_or("workspace is not an object")?;
    let mut stack: Vec<&mut Value> = Vec::new();
    if let Some(Value::Array(lists)) = workspace.get_mut("cue_lists") {
        for list in lists {
            if let Some(Value::Array(cues)) = list.get_mut("cues") {
                stack.extend(cues.iter_mut());
            }
        }
    }
    while let Some(cue) = stack.pop() {
        let Some(cue) = cue.as_object_mut() else {
            continue;
        };
        upgrade(cue);
        if let Some(Value::Array(children)) = cue
            .get_mut("kind")
            .and_then(|kind| kind.get_mut("children"))
        {
            stack.extend(children.iter_mut());
        }
    }
    root.insert("format_version".into(), to.into());
    Ok(document)
}
