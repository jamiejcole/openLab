// SPDX-License-Identifier: GPL-3.0-or-later

//! Autosave and crash recovery decisions (NFR-05). Times are passed in; reading file times and
//! the files themselves is the engine's job.

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

#[cfg(test)]
mod tests {
    use super::*;

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
