// SPDX-License-Identifier: GPL-3.0-or-later

//! openLab core: workspace model, cue engine and protocol types.
//!
//! Pure logic only: no I/O, threads or clocks. Time is passed in by the caller.

#![forbid(unsafe_code)]

/// Crate version, as set in the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_set() {
        assert!(!super::VERSION.is_empty());
    }
}
