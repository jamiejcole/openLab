// SPDX-License-Identifier: GPL-3.0-or-later

//! Starting the app again after it crashed (M4.9, ARCHITECTURE §8): `open -g -b <bundle id>` on
//! macOS, through Launch Services, so the app is not this process's child and does not inherit
//! anything from it. `-g`: an app that is still running (reconnecting slowly) is not brought to
//! the front over what the operator is using mid-show; a relaunched one fronts itself when it
//! starts. The network layer decides when (`net::Relaunch`); this only does it.

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

/// Launches the app with bundle identifier `id` (never activating one already running),
/// waiting for `open` to answer. `Err` says why it could not.
pub fn launch(id: &str) -> Result<(), String> {
    imp::launch(id)
}

#[cfg(target_os = "macos")]
mod imp {
    /// `open`'s arguments: `-g` keeps a running app where it is.
    pub fn open_args(id: &str) -> [&str; 3] {
        ["-g", "-b", id]
    }

    pub fn launch(id: &str) -> Result<(), String> {
        let status = std::process::Command::new("/usr/bin/open")
            .args(open_args(id))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| format!("open: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("open -g -b {id}: {status}"))
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

    /// A relaunch never pulls a running (slowly reconnecting) app over other windows.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_relaunch_does_not_activate_a_running_app() {
        assert_eq!(
            imp::open_args("dev.openlab.openLab"),
            ["-g", "-b", "dev.openlab.openLab"]
        );
    }

    /// No app has this id: `open` fails and says so, it does not hang or panic.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_unknown_app_is_an_error() {
        assert!(launch("dev.openlab.no-such-app-for-tests").is_err());
    }
}
