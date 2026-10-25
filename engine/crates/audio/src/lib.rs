// SPDX-License-Identifier: GPL-3.0-or-later

//! openLab audio: device layer, decoding, disk streaming, real-time mixer and offline render driver.

// Unchecked indexing can panic on the audio thread; use `get` and handle the miss.
// This attribute covers the lib target only: any new target in this crate (bin, bench, example) must repeat it.
#![deny(clippy::indexing_slicing)]

/// Crate version, as set in the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_set() {
        assert!(!super::VERSION.is_empty());
    }
}
