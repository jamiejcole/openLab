// SPDX-License-Identifier: GPL-3.0-or-later

//! Starting the app again after it crashed (M4.9, ARCHITECTURE §8): `open -b <bundle id>` on
//! macOS, through Launch Services, so the app is not this process's child and does not inherit
//! anything from it. Plain `open` activates the app, which the operator needs after a crash
//! (keyboard GO must reach it; on macOS 14+ an app launched in the background cannot reliably
//! front itself). An app that is still running is never launched again (#168): the network
//! layer asks [`process_name`] whether the app's process lives (`net::Relaunch`) and decides
//! when; this only does it.

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

/// Launches the app with bundle identifier `id` and brings it to the front, waiting for `open`
/// to answer. `Err` says why it could not.
pub fn launch(id: &str) -> Result<(), String> {
    imp::launch(id)
}

/// The name of the process with id `pid` (its executable's), `None` when there is no such
/// process or it cannot be read. Compared with the name read at `hello`, so a pid reused by
/// another program does not count as the app still running.
pub fn process_name(pid: u32) -> Option<String> {
    imp::process_name(pid)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::os::raw::{c_int, c_void};

    /// `open`'s arguments. No `-g`: the relaunched app must come to the front.
    pub fn open_args(id: &str) -> [&str; 2] {
        ["-b", id]
    }

    unsafe extern "C" {
        // libproc (in libSystem): the process's name, its length as the result (0: none).
        fn proc_name(pid: c_int, buffer: *mut c_void, size: u32) -> c_int;
    }

    pub fn process_name(pid: u32) -> Option<String> {
        // 0 is the kernel, and a value over i32::MAX would read as a negative pid.
        let pid = c_int::try_from(pid).ok().filter(|&p| p > 0)?;
        // 2 * MAXCOMLEN + 1 bytes is the most it writes.
        let mut buf = [0u8; 64];
        let size = u32::try_from(buf.len()).ok()?;
        // SAFETY: `buf` is valid and writable for `size` bytes; `proc_name` writes at most that
        // many and has no other preconditions.
        let n = unsafe { proc_name(pid, buf.as_mut_ptr().cast(), size) };
        let n = usize::try_from(n).ok().filter(|&n| n > 0)?;
        let name = buf.get(..n)?;
        Some(String::from_utf8_lossy(name).into_owned())
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
            Err(format!("open -b {id}: {status}"))
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn launch(_id: &str) -> Result<(), String> {
        Err("relaunching the app is not available on this platform yet".into())
    }

    /// Not known here: the app counts as gone, as before #168 (and relaunching is not
    /// available here anyway).
    pub fn process_name(_pid: u32) -> Option<String> {
        None
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

    /// A relaunch after a crash brings the app to the front (no `-g`: on macOS 14+ an app
    /// launched in the background stays behind other windows, and keyboard GO would go to
    /// another app). A running app is never relaunched (net::relaunch), so this cannot pull a
    /// slowly reconnecting one forward.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_relaunch_is_a_plain_open_that_fronts_the_app() {
        assert_eq!(
            imp::open_args("dev.openlab.openLab"),
            ["-b", "dev.openlab.openLab"]
        );
    }

    /// This process is named; pids no process can have are not, and never panic.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_live_process_has_a_name_and_no_process_has_none() {
        let me = process_name(std::process::id()).expect("this process");
        assert!(!me.is_empty());
        for none in [0, 99_999_999, u32::try_from(i32::MAX).unwrap(), u32::MAX] {
            assert_eq!(process_name(none), None, "{none}");
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
