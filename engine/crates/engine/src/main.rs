// SPDX-License-Identifier: GPL-3.0-or-later

//! openlab-engine: the headless openLab show-playback engine.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

/// Debug builds forbid allocation where `assert_no_alloc` guards (the device callback, M3.12):
/// without this global allocator the guard sees nothing (M3.21). Violations are counted, not
/// fatal (`warn_debug`), and the control loop reports them as errors.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const VERSION: &str = env!("CARGO_PKG_VERSION");

enum Mode<'a> {
    Print(String),
    /// Play a show on the default output device until stdin closes.
    Play(&'a Path),
}

fn mode(args: &[String]) -> Mode<'_> {
    if args.iter().any(|a| a == "--version") {
        return Mode::Print(format!("openlab-engine {VERSION}"));
    }
    match args {
        [path] if !path.starts_with('-') => Mode::Play(Path::new(path)),
        _ => Mode::Print("usage: openlab-engine [--version] [SHOW_FILE]".to_owned()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match mode(&args) {
        // `println!` panics if stdout is closed (e.g. a broken pipe); a failed write is just a
        // failed run.
        Mode::Print(text) => match writeln!(std::io::stdout().lock(), "{text}") {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        },
        Mode::Play(path) => match openlab_engine::host::run_show(path) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                let _ = writeln!(std::io::stderr().lock(), "openlab-engine: {e}");
                ExitCode::FAILURE
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_flag_prints_version() {
        assert!(
            matches!(mode(&["--version".to_owned()]), Mode::Print(t) if t == format!("openlab-engine {VERSION}"))
        );
        assert!(matches!(mode(&[]), Mode::Print(t) if t.starts_with("usage")));
        assert!(matches!(mode(&["show.openlab".to_owned()]), Mode::Play(_)));
    }

    /// M3.21: the debug binary's global allocator is `assert_no_alloc`'s, so an allocation
    /// inside a guarded section is detected (counted: `warn_debug`).
    #[cfg(debug_assertions)]
    #[test]
    fn allocations_in_a_guarded_section_are_detected() {
        let before = assert_no_alloc::violation_count();
        assert_no_alloc::assert_no_alloc(|| {
            let v = vec![1u8; 16];
            std::hint::black_box(&v);
        });
        assert!(assert_no_alloc::violation_count() > before);
    }
}
