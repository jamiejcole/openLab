// SPDX-License-Identifier: GPL-3.0-or-later

//! Starting the app again after it crashed (M4.9, ARCHITECTURE §8): `open -b <bundle id>` on
//! macOS, through Launch Services, so the app is not this process's child and does not inherit
//! anything from it. The network layer decides when (`net::Relaunch`); this only does it.

/// Whether `id` can be a bundle identifier: 1 to 255 of `A-Z a-z 0-9 . -`, not starting with
/// `-` (it is passed to `open` as an argument, and must never read as an option).
pub fn valid_bundle_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

/// Launches (or brings forward) the app with bundle identifier `id`, waiting for `open` to
/// answer. `Err` says why it could not.
pub fn launch(id: &str) -> Result<(), String> {
    imp::launch(id)
}

#[cfg(target_os = "macos")]
mod imp {
    pub fn launch(id: &str) -> Result<(), String> {
        let status = std::process::Command::new("/usr/bin/open")
            .args(["-b", id])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| format!("open: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("open -b {id}: {status}"))
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn launch(_id: &str) -> Result<(), String> {
        Err("relaunching the app is not available on this platform yet".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_bundle_ids_are_taken() {
        assert!(valid_bundle_id("dev.openlab.openLab"));
        assert!(valid_bundle_id("a-b.c9"));
        for bad in [
            "",
            "-a",
            "--args",
            "a b",
            "a/b",
            "a;b",
            "é",
            &"a".repeat(256),
        ] {
            assert!(!valid_bundle_id(bad), "{bad:?}");
        }
    }

    /// No app has this id: `open` fails and says so, it does not hang or panic.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_unknown_app_is_an_error() {
        let _serial = crate::diag::run_marker::tests::serialize_spawn_and_lock_release();
        assert!(launch("dev.openlab.no-such-app-for-tests").is_err());
    }
}
