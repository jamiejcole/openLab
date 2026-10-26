// SPDX-License-Identifier: GPL-3.0-or-later

//! Autosave and crash recovery decisions (NFR-05). Times are passed in; reading file times and
//! the files themselves is the engine's job.

use uuid::Uuid;

use crate::model::WorkspaceId;

/// What to do about a recovery file found when opening a workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryDecision {
    /// No recovery file.
    None,
    /// The recovery file holds changes the show file lacks: offer to restore it.
    Offer,
    /// The recovery file is not newer than the show file, so it is leftover from before the last
    /// save; it can be deleted.
    Stale,
}

/// Compares file modification times (milliseconds since the Unix epoch). A recovery file for a
/// show file that no longer exists (a never-saved workspace, or a deleted file) is offered.
/// Equal times are stale: after a save the show file is the later write.
pub fn decide_recovery(
    show_modified: Option<i64>,
    recovery_modified: Option<i64>,
) -> RecoveryDecision {
    match (show_modified, recovery_modified) {
        (_, None) => RecoveryDecision::None,
        (None, Some(_)) => RecoveryDecision::Offer,
        (Some(show), Some(recovery)) if recovery > show => RecoveryDecision::Offer,
        (Some(_), Some(_)) => RecoveryDecision::Stale,
    }
}

/// True when a dirty workspace is due an autosave: `interval` seconds have passed since the
/// last autosave, or since it became dirty when there has been none. Times are seconds on any
/// monotonic scale the caller keeps; a clock that goes backwards never makes this true early.
pub fn autosave_due(
    dirty_since: Option<f64>,
    last_autosave: Option<f64>,
    now: f64,
    interval: f64,
) -> bool {
    let Some(dirty_since) = dirty_since else {
        return false;
    };
    let reference = match last_autosave {
        Some(last) if last > dirty_since => last,
        _ => dirty_since,
    };
    // Written so that NaN anywhere gives false.
    now - reference >= interval
}

/// What a recovery file name or a backup folder name says about its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreName {
    pub workspace_id: WorkspaceId,
    /// Hash of the show path ([`crate::path_hash`]); `None` for the recovery file of a workspace
    /// that was never saved.
    pub path_hash: Option<String>,
}

/// Why a recovery or backup name was not understood.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StoreNameError {
    #[error("not a recovery file name (expected <workspace-id>[-<path-hash>].openlab)")]
    NotRecoveryFile,
    #[error("the workspace id is not a canonical lower-case UUID")]
    BadWorkspaceId,
    #[error("the path hash is not 16 lower-case hex digits")]
    BadPathHash,
    #[error("unexpected text after the workspace id and path hash")]
    Trailing,
}

const UUID_LEN: usize = 36;
const HASH_LEN: usize = 16;
const RECOVERY_EXTENSION: &str = ".openlab";

/// Parses `<workspace-id>` or `<workspace-id>-<path-hash>`, the stem of a recovery file and the
/// whole name of a backup folder (`Backups/<workspace-id>-<path-hash>`). Strict: only the
/// spelling the engine writes is accepted (hyphenated lower-case UUID, 16 lower-case hex
/// digits), so a stray user file is never mistaken for a workspace's data. The time of a
/// backup is in the names of the files inside the folder ([`crate::parse_backup_name`]).
pub fn parse_store_name(name: &str) -> Result<StoreName, StoreNameError> {
    // `get` on a byte offset: a multi-byte character straddling it is a plain error.
    let id_text = name.get(..UUID_LEN).ok_or(StoreNameError::BadWorkspaceId)?;
    let uuid = Uuid::parse_str(id_text).map_err(|_| StoreNameError::BadWorkspaceId)?;
    // The parser also takes upper case; the engine writes lower case only.
    if uuid.hyphenated().to_string() != id_text {
        return Err(StoreNameError::BadWorkspaceId);
    }
    let workspace_id = WorkspaceId(uuid);
    let rest = name.get(UUID_LEN..).unwrap_or_default();
    if rest.is_empty() {
        return Ok(StoreName {
            workspace_id,
            path_hash: None,
        });
    }
    let hash = rest.strip_prefix('-').ok_or(StoreNameError::Trailing)?;
    if hash.len() != HASH_LEN || !hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(StoreNameError::BadPathHash);
    }
    Ok(StoreName {
        workspace_id,
        path_hash: Some(hash.to_owned()),
    })
}

/// Parses the name of an autosave file in `Recovery/`. Files moved aside (`.stale.openlab`,
/// `.corrupt`) and anything else are errors.
pub fn parse_recovery_file_name(name: &str) -> Result<StoreName, StoreNameError> {
    let stem = name
        .strip_suffix(RECOVERY_EXTENSION)
        .ok_or(StoreNameError::NotRecoveryFile)?;
    parse_store_name(stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "123e4567-e89b-42d3-a456-426614174000";
    const HASH: &str = "0123456789abcdef";

    fn id() -> WorkspaceId {
        WorkspaceId(Uuid::parse_str(ID).unwrap())
    }

    #[test]
    fn store_names_round_trip() {
        let named = parse_store_name(&format!("{ID}-{HASH}")).unwrap();
        assert_eq!(named.workspace_id, id());
        assert_eq!(named.path_hash.as_deref(), Some(HASH));
        let unsaved = parse_store_name(ID).unwrap();
        assert_eq!(unsaved.path_hash, None);
        assert_eq!(
            parse_recovery_file_name(&format!("{ID}-{HASH}.openlab")),
            Ok(named)
        );
        assert_eq!(
            parse_recovery_file_name(&format!("{ID}.openlab")),
            Ok(unsaved)
        );
    }

    #[test]
    fn malformed_store_names_are_rejected() {
        use StoreNameError::*;
        let wide = "\u{e9}".repeat(19);
        for (name, want) in [
            ("", BadWorkspaceId),
            ("abc", BadWorkspaceId),
            ("123E4567-E89B-42D3-A456-426614174000", BadWorkspaceId),
            ("123e4567e89b42d3a456426614174000", BadWorkspaceId),
            ("{123e4567-e89b-42d3-a456-426614174000}", BadWorkspaceId),
            ("123e4567-e89b-42d3-a456-42661417400g", BadWorkspaceId),
            // A multi-byte character straddling the id's end, and one all of them.
            ("123e4567-e89b-42d3-a456-42661417400\u{e9}", BadWorkspaceId),
            (wide.as_str(), BadWorkspaceId),
            ("123e4567-e89b-42d3-a456-426614174000x", Trailing),
            ("123e4567-e89b-42d3-a456-426614174000.stale", Trailing),
            ("123e4567-e89b-42d3-a456-426614174000-", BadPathHash),
            ("123e4567-e89b-42d3-a456-426614174000-0123", BadPathHash),
            (
                "123e4567-e89b-42d3-a456-426614174000-0123456789ABCDEF",
                BadPathHash,
            ),
            (
                "123e4567-e89b-42d3-a456-426614174000-0123456789abcdef0",
                BadPathHash,
            ),
            (
                "123e4567-e89b-42d3-a456-426614174000-0123456789abcdeg",
                BadPathHash,
            ),
        ] {
            assert_eq!(parse_store_name(name), Err(want), "{name:?}");
        }
        for name in [
            "",
            ".openlab",
            ID,
            &format!("{ID}.stale.openlab"),
            &format!("{ID}.openlab.corrupt"),
            &format!("{ID}-{HASH}.000000000000123.stale.openlab"),
        ] {
            assert!(parse_recovery_file_name(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn decisions() {
        assert_eq!(decide_recovery(Some(10), None), RecoveryDecision::None);
        assert_eq!(decide_recovery(None, None), RecoveryDecision::None);
        assert_eq!(decide_recovery(Some(10), Some(11)), RecoveryDecision::Offer);
        assert_eq!(decide_recovery(Some(10), Some(10)), RecoveryDecision::Stale);
        assert_eq!(decide_recovery(Some(10), Some(9)), RecoveryDecision::Stale);
        assert_eq!(decide_recovery(None, Some(0)), RecoveryDecision::Offer);
        assert_eq!(decide_recovery(Some(-5), Some(-4)), RecoveryDecision::Offer);
    }

    #[test]
    fn autosave_timing() {
        assert!(!autosave_due(None, None, 1000.0, 30.0), "clean never saves");
        assert!(!autosave_due(Some(100.0), None, 129.9, 30.0));
        assert!(autosave_due(Some(100.0), None, 130.0, 30.0));
        // After an autosave the next is one interval later, even if still dirty from before.
        assert!(!autosave_due(Some(100.0), Some(130.0), 150.0, 30.0));
        assert!(autosave_due(Some(100.0), Some(130.0), 160.0, 30.0));
        // An autosave from a previous dirty period does not delay a new one.
        assert!(autosave_due(Some(200.0), Some(130.0), 230.0, 30.0));
        assert!(!autosave_due(Some(200.0), Some(130.0), 229.0, 30.0));
        // Clock going backwards, NaN.
        assert!(!autosave_due(Some(100.0), None, 50.0, 30.0));
        assert!(!autosave_due(Some(100.0), None, f64::NAN, 30.0));
        assert!(!autosave_due(Some(f64::NAN), None, 500.0, 30.0));
    }
}
