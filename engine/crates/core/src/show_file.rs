// SPDX-License-Identifier: GPL-3.0-or-later

//! The `.openlab` show file (NFR-40): versioned JSON, specified in `docs/file-format.md`.
//! Works on strings only; reading and writing files is the engine's job.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::Workspace;
use crate::validate::{Severity, ValidationIssue, repair};

/// The format version this build writes, and the newest it reads.
pub const FORMAT_VERSION: u32 = 1;
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
    let file: ShowFileIn = match u32::try_from(version) {
        // Parse straight from text so errors carry line and column.
        Ok(FORMAT_VERSION) => serde_json::from_str(text).map_err(LoadError::Model)?,
        Ok(old) if old < FORMAT_VERSION => {
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
    let issues = repair(&mut workspace);
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
