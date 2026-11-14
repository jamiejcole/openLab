// SPDX-License-Identifier: GPL-3.0-or-later

//! The stage host's platforms: macOS draws with Metal in AppKit windows; elsewhere there is no
//! video output yet.

use std::process::ExitCode;

#[cfg(target_os = "macos")]
mod mac;
#[cfg(all(target_os = "macos", feature = "test-support"))]
pub use mac::{WindowReport, window_report};
#[cfg(any(not(target_os = "macos"), test))]
mod none;

/// The exit status of a bad command line.
const USAGE_EXIT: u8 = 2;

/// Runs the stage host with `args` until the engine says `quit` or its input ends.
pub fn run(args: Vec<String>) -> ExitCode {
    #[cfg(target_os = "macos")]
    return mac::run(args);
    #[cfg(not(target_os = "macos"))]
    {
        if !args.is_empty() {
            return ExitCode::from(USAGE_EXIT);
        }
        let mut host = crate::host::Host::new(none::NoVideo);
        let stdin = std::io::stdin();
        crate::host::serve(
            &mut stdin.lock(),
            &mut std::io::stdout().lock(),
            |m, out| host.handle(m, out),
        );
        ExitCode::SUCCESS
    }
}
