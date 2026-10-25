// SPDX-License-Identifier: GPL-3.0-or-later

//! openlab-engine: the headless openLab show-playback engine.

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn output(args: &[String]) -> String {
    if args.iter().any(|a| a == "--version") {
        format!("openlab-engine {VERSION}")
    } else {
        "openlab-engine: nothing to run yet".to_owned()
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    println!("{}", output(&args));
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
