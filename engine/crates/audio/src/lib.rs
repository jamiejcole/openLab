// SPDX-License-Identifier: GPL-3.0-or-later

//! openLab audio: device layer, decoding, disk streaming, real-time mixer and offline render driver.
//! See `docs/audio.md`.

// Unchecked indexing can panic on the audio thread; use `get` and handle the miss.
// This attribute covers the lib target only: any new target in this crate (bin, bench, example) must repeat it.
#![deny(clippy::indexing_slicing)]

mod controller;
mod gain;
mod mixer;
mod offline;

pub use controller::{MixerController, QueueFull};
pub use gain::{Curve, Fade, SILENCE_DB, db_to_linear, linear_to_db, ramp_value};
pub use mixer::{
    COMMAND_CAPACITY, Command, CommandKind, ConfigError, Event, FinishReason, GainTarget,
    MAX_OUTPUT_CHANNELS, MAX_VOICE_CHANNELS, Mixer, MixerConfig, OutputSettings, RejectReason,
    SampleTime, VoiceId, VoiceStart,
};
pub use offline::OfflineRenderer;

/// Crate version, as set in the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_set() {
        assert!(!super::VERSION.is_empty());
    }
}
