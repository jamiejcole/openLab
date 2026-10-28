// SPDX-License-Identifier: GPL-3.0-or-later

//! openlab-engine: the headless openLab show-playback engine.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use openlab_engine::host::Options;

/// Debug builds forbid allocation where `assert_no_alloc` guards (the device callback, M3.12):
/// without this global allocator the guard sees nothing (M3.21). Violations are counted, not
/// fatal (`warn_debug`), and the control loop reports them as errors.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "usage: openlab-engine [--version] [--open SHOW_FILE]... \
[--app-support DIR] [--watch-stdin]";

enum Mode {
    Print(String),
    /// Run the engine (starts with no workspace open; `--open` opens shows through `open`).
    Run(Options),
}

fn mode(args: &[String]) -> Mode {
    if args.iter().any(|a| a == "--version") {
        return Mode::Print(format!("openlab-engine {VERSION}"));
    }
    let mut options = Options::default();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--open" => match rest.next() {
                Some(path) => options.open.push(PathBuf::from(path)),
                None => return Mode::Print(USAGE.to_owned()),
            },
            "--app-support" => match rest.next() {
                Some(dir) => options.app_support = Some(PathBuf::from(dir)),
                None => return Mode::Print(USAGE.to_owned()),
            },
            "--watch-stdin" => options.watch_stdin = true,
            _ => return Mode::Print(USAGE.to_owned()),
        }
    }
    Mode::Run(options)
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
        Mode::Run(options) => match openlab_engine::host::run(options) {
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
        assert!(matches!(mode(&[]), Mode::Run(o) if o.open.is_empty() && !o.watch_stdin));
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    /// M4.15g: no positional show file any more; `--open` (repeatable) is the dev/test path.
    #[test]
    fn options_parse_and_anything_else_prints_the_usage() {
        let Mode::Run(o) = mode(&args(&[
            "--open",
            "a.openlab",
            "--open",
            "b.openlab",
            "--app-support",
            "/tmp/x",
            "--watch-stdin",
        ])) else {
            panic!("not run")
        };
        assert_eq!(
            o.open,
            [PathBuf::from("a.openlab"), PathBuf::from("b.openlab")]
        );
        assert_eq!(o.app_support, Some(PathBuf::from("/tmp/x")));
        assert!(o.watch_stdin);
        for bad in [
            &["show.openlab"][..],
            &["--open"],
            &["--app-support"],
            &["--nope"],
        ] {
            assert!(
                matches!(mode(&args(bad)), Mode::Print(t) if t.starts_with("usage")),
                "{bad:?}"
            );
        }
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
