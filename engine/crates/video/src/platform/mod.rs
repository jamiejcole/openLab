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

/// Prints a probe's one line and ends the probe process (`--probe <file>`).
fn print_probe(report: &openlab_core::video_probe::ProbeReport) -> ExitCode {
    use std::io::Write;
    let Ok(line) = report.to_line() else {
        return ExitCode::FAILURE;
    };
    let mut out = std::io::stdout().lock();
    match writeln!(out, "{line}").and_then(|()| out.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

/// Runs the stage host with `args` until the engine says `quit` or its input ends; with
/// `--probe <file>`, says what the file is and exits.
pub fn run(args: Vec<String>) -> ExitCode {
    #[cfg(target_os = "macos")]
    return mac::run(args);
    #[cfg(not(target_os = "macos"))]
    {
        if let [flag, _] = args.as_slice()
            && flag == "--probe"
        {
            return print_probe(&openlab_core::video_probe::ProbeReport::Unsupported {
                reason: "this computer's openLab has no video".into(),
            });
        }
        if !args.is_empty() {
            return ExitCode::from(USAGE_EXIT);
        }
        let mut host = crate::host::Host::new(none::NoVideo);
        crate::host::serve(
            std::io::BufReader::new(std::io::stdin()),
            &mut std::io::stdout().lock(),
            |m, out| host.step(m, out),
        );
        ExitCode::SUCCESS
    }
}
