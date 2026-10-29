// SPDX-License-Identifier: GPL-3.0-or-later

//! Ending the process at once, for a forced exit (`host::stop`): no exit handlers run. A thread
//! stuck inside the audio system (a device open that never returns) can hold locks that the
//! system libraries' own exit handlers then wait for, so a normal exit could hang as well.

/// Ends the process with `code`, now. Nothing is flushed: write what matters first.
pub fn exit_now(code: i32) -> ! {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn _exit(status: std::os::raw::c_int) -> !;
        }
        // SAFETY: `_exit` takes any int, touches no memory of ours and never returns; it is
        // async-signal-safe, so it is safe from any thread whatever the others are doing.
        unsafe { _exit(code) }
    }
    #[cfg(not(unix))]
    std::process::exit(code)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    const CHILD: &str = "OPENLAB_EXIT_NOW_CHILD";

    extern "C" fn said_at_exit() {
        use std::io::Write as _;
        let _ = std::io::stderr().write_all(b"exit handler ran");
    }

    /// Not a test on its own: does nothing unless the test below started it with [`CHILD`].
    #[test]
    fn exit_now_child() {
        if std::env::var_os(CHILD).is_none() {
            return;
        }
        unsafe extern "C" {
            fn atexit(callback: extern "C" fn()) -> std::os::raw::c_int;
        }
        // SAFETY: registers a plain function that only writes to stderr.
        assert_eq!(unsafe { atexit(said_at_exit) }, 0);
        exit_now(7);
    }

    #[test]
    fn exit_now_ends_the_process_with_its_code_and_runs_no_exit_handler() {
        // The child shares our open files until its exec: no lock may be let go meanwhile.
        let _serial = crate::diag::run_marker::tests::serialize_spawn_and_lock_release();
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::exit::tests::exit_now_child",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(7), "{stderr}");
        assert!(!stderr.contains("exit handler ran"), "{stderr}");
    }
}
