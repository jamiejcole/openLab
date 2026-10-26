// SPDX-License-Identifier: GPL-3.0-or-later

//! openLab audio: device layer, decoding, disk streaming, real-time mixer and offline render driver.
//! See `docs/audio.md`.

// Unchecked indexing can panic on the audio thread; use `get` and handle the miss.
// This attribute covers the lib target only: any new target in this crate (bin, bench, example) must repeat it.
#![deny(clippy::indexing_slicing)]

mod controller;
mod decode;
#[cfg(feature = "device")]
mod device;
mod fuzzing;
mod gain;
mod meter;
mod mixer;
mod mp4_check;
mod mp4_priming;
mod negotiate;
mod offline;
mod output;
mod plan;
mod resample;
mod stream;
mod waveform;

pub use controller::{BatchTooLarge, MixerController};
pub use decode::{
    DEFAULT_PRELOAD_BUDGET, INDEX_SPACING, Media, MediaError, MediaInfo, MediaRead, MediaWarning,
    Preloaded, Probed, SeekIndex, Seeking, WarningKind, preload, preload_within, probe,
    probe_media,
};
#[cfg(feature = "device")]
pub use device::{
    DeviceError, DeviceInfo, MixerPending, OpenError, OpenErrorKind, OpenRequest, OutputStream,
    default_output_device_id, open_output, output_devices,
};
#[doc(hidden)]
pub use fuzzing::fuzz_decode;
pub use gain::{
    Curve, Fade, MAX_GAIN, MAX_GAIN_DB, RAMP_TOLERANCE, SILENCE_DB, db_to_linear, linear_to_db,
    ramp_value, sanitize_gain,
};
pub use meter::XrunNotifier;
pub use mixer::{
    COMMAND_CAPACITY, CommandKind, ConfigError, Event, FinishReason, GainTarget, MAX_BLOCK_FRAMES,
    MAX_OUTPUT_CHANNELS, MAX_PATCHES, MAX_SAMPLE_RATE, MAX_VOICE_CHANNELS, Mixer, MixerConfig,
    OutputSettings, PENDING_CAPACITY, RejectReason, SampleTime, VoiceId, VoiceStart,
};
pub use negotiate::{
    BufferChoice, BufferRange, ConfigRange, Latency, NegotiateError, Negotiated, Request,
    SampleFormat, negotiate, reopen_delay_ms,
};
pub use offline::OfflineRenderer;
pub use output::{
    CallbackTime, DeviceMonitor, ErrorReporter, HostError, HostLink, OutputHost, OutputSample,
    SCRATCH_FRAMES, StreamErrors,
};
pub use plan::{
    MAX_SLICES, MIN_LOOP_MS, PlanError, Plays, Slice, StreamLink, check_loop_lengths,
    min_loop_frames, plan, plan_at_rate, total_frames,
};
pub use stream::{
    DEFAULT_BUFFER_MS, DEFAULT_CACHE_BUDGET_BYTES, DEFAULT_EXIT_BUDGET_BYTES, DEFAULT_EXIT_BYTES,
    DEFAULT_EXIT_MS, DEFAULT_LOOP_CACHE_BYTES, DEFAULT_MAX_STREAMS, DEFAULT_STEP_FRAMES,
    DEFAULT_WORKERS, DiskStreamer, PreparedStream, StreamHandle, StreamRequest, StreamerConfig,
};

pub use waveform::{
    BASE_FRAMES, Level, MAX_RANGE_BUCKETS, PEAK_SCALE, Peak, Waveform, WaveformBuilder,
    WaveformError, WaveformResult, build_waveform,
};

/// Crate version, as set in the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

/// Runs `f` with allocation forbidden; any (de)allocation fails the test.
#[cfg(test)]
pub(crate) fn no_alloc_for_tests<T>(f: impl FnOnce() -> T) -> T {
    let before = assert_no_alloc::violation_count();
    let r = assert_no_alloc::assert_no_alloc(f);
    assert_eq!(
        assert_no_alloc::violation_count(),
        before,
        "allocation on the real-time path"
    );
    r
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_set() {
        assert!(!super::VERSION.is_empty());
    }
}
