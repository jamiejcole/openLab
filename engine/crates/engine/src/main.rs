// SPDX-License-Identifier: GPL-3.0-or-later

//! openlab-engine: the headless openLab show-playback engine.

use std::io::Write;
use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn output(args: &[String]) -> String {
    if args.iter().any(|a| a == "--version") {
        format!("openlab-engine {VERSION}")
    } else {
        "openlab-engine: nothing to run yet".to_owned()
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `println!` panics if stdout is closed (e.g. a broken pipe); a failed write is just a failed run.
    match writeln!(std::io::stdout().lock(), "{}", output(&args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_flag_prints_version() {
        assert_eq!(
            output(&["--version".to_owned()]),
            format!("openlab-engine {VERSION}")
        );
        assert_eq!(output(&[]), "openlab-engine: nothing to run yet");
    }
}
