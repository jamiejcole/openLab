// SPDX-License-Identifier: GPL-3.0-or-later

//! Sleep prevention (SM-07, M4.9; ARCHITECTURE §8): while any open workspace is in Show Mode
//! or any cue is running, the engine holds an assertion that keeps the system, the display and
//! the screen saver from idling to sleep. The engine holds it, not the UI, so it stays while
//! the UI restarts.
//!
//! The control loop only says what it wants (a `bool` on a channel, when it changes); the
//! `openlab-power` thread talks to the power manager (an IPC call on macOS, which must not run
//! on the control thread). The operating system drops the assertion when the process ends,
//! however it ends.
//!
//! macOS: `IOPMAssertionCreateWithName` with `PreventUserIdleDisplaySleep` (it also holds off
//! idle system sleep and the screen saver), declared here by hand as `diag::cpu` does (no new
//! dependency). Elsewhere nothing is asserted and the first request logs that once.

use std::io;

use crossbeam_channel::{Receiver, Sender};
use openlab_core::protocol::LogLevel;

use crate::control::Inbox;

/// Starts the thread. Send `true` to hold the assertion and `false` to let it go; dropping
/// every sender lets it go and ends the thread. Failures are logged through `inbox`.
pub fn spawn(inbox: Sender<Inbox>) -> io::Result<Sender<bool>> {
    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::Builder::new()
        .name("openlab-power".into())
        .spawn(move || serve(&rx, &inbox))?;
    Ok(tx)
}

fn serve(wants: &Receiver<bool>, inbox: &Sender<Inbox>) {
    let mut held: Option<imp::Assertion> = None;
    let mut warned = false;
    let log = |level, message: String| {
        let _ = inbox.send(Inbox::Log { level, message });
    };
    while let Ok(want) = wants.recv() {
        if want && held.is_none() {
            match imp::Assertion::take() {
                Ok(a) => {
                    held = Some(a);
                    warned = false;
                    // Debug: it changes with every cue that runs in an otherwise idle show.
                    log(
                        LogLevel::Debug,
                        "keeping the computer and display awake".into(),
                    );
                }
                Err(e) if !warned => {
                    warned = true;
                    log(
                        LogLevel::Warn,
                        format!(
                            "could not keep the computer awake: {e}; it may sleep during the show"
                        ),
                    );
                }
                Err(_) => {}
            }
        } else if !want && held.is_some() {
            // Dropping it releases the assertion.
            held = None;
            log(
                LogLevel::Debug,
                "the computer and display may sleep again".into(),
            );
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;

    type CfStringRef = *const c_void;

    const UTF8: u32 = 0x0800_0100;
    /// `kIOPMAssertionLevelOn`.
    const LEVEL_ON: u32 = 255;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(
            alloc: *const c_void,
            bytes: *const u8,
            len: isize,
            encoding: u32,
            external: u8,
        ) -> CfStringRef;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            kind: CfStringRef,
            level: u32,
            name: CfStringRef,
            id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    /// An owned `CFString`, released on drop.
    struct CfString(CfStringRef);

    impl CfString {
        fn new(text: &str) -> Option<Self> {
            let len = isize::try_from(text.len()).ok()?;
            // SAFETY: `text` is valid for `len` bytes for the call (the bytes are copied); a
            // null allocator is the default one.
            let s =
                unsafe { CFStringCreateWithBytes(std::ptr::null(), text.as_ptr(), len, UTF8, 0) };
            (!s.is_null()).then_some(Self(s))
        }
    }

    impl Drop for CfString {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a non-null CFString this value owns (created above).
            unsafe { CFRelease(self.0) }
        }
    }

    /// A held power assertion, released on drop.
    pub struct Assertion(u32);

    impl Assertion {
        pub fn take() -> Result<Self, String> {
            let kind = CfString::new("PreventUserIdleDisplaySleep")
                .ok_or("could not make the assertion type")?;
            let name = CfString::new("openLab: a show is running or in Show Mode")
                .ok_or("could not make the assertion name")?;
            let mut id = 0u32;
            // SAFETY: both strings are live CFStrings for the call; `id` is a valid out-pointer.
            let rc = unsafe { IOPMAssertionCreateWithName(kind.0, LEVEL_ON, name.0, &mut id) };
            if rc == 0 {
                Ok(Self(id))
            } else {
                Err(format!("the power manager refused (IOReturn {rc:#x})"))
            }
        }
    }

    impl Drop for Assertion {
        fn drop(&mut self) {
            // SAFETY: `self.0` is an assertion id this process created and has not released.
            let _ = unsafe { IOPMAssertionRelease(self.0) };
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub struct Assertion;

    impl Assertion {
        pub fn take() -> Result<Self, String> {
            Err("sleep prevention is not available on this platform yet".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logs(inbox: &Receiver<Inbox>) -> Vec<(LogLevel, String)> {
        inbox
            .iter()
            .filter_map(|m| match m {
                Inbox::Log { level, message } => Some((level, message)),
                _ => None,
            })
            .collect()
    }

    /// Held on `true`, let go on `false`, once each however often asked; the thread ends when
    /// the senders go. (On macOS this takes a real assertion for a moment.)
    #[test]
    fn the_assertion_follows_what_the_loop_wants() {
        let (inbox_tx, inbox) = crossbeam_channel::unbounded();
        let wants = spawn(inbox_tx).unwrap();
        for want in [true, true, false, false, true] {
            wants.send(want).unwrap();
        }
        drop(wants);
        let logs = logs(&inbox);
        if cfg!(target_os = "macos") {
            let messages: Vec<&str> = logs.iter().map(|(_, m)| m.as_str()).collect();
            assert_eq!(
                messages,
                [
                    "keeping the computer and display awake",
                    "the computer and display may sleep again",
                    "keeping the computer and display awake",
                ]
            );
        } else {
            // Said once, not on every request.
            assert_eq!(logs.len(), 1, "{logs:?}");
            assert_eq!(logs[0].0, LogLevel::Warn);
        }
    }
}
