// SPDX-License-Identifier: GPL-3.0-or-later

//! Output device configuration, pure (AUD-03, AUD-21): picks a stream configuration from what a
//! device supports, and the reopen backoff schedule (NFR-08). No cpal types here, so it builds
//! and is tested without the `device` feature. See `docs/audio.md`, Device layer.

use crate::mixer::MAX_OUTPUT_CHANNELS;

/// Sample formats the device layer can write. Others a device offers are skipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SampleFormat {
    F32,
    F64,
    I32,
    I16,
    U16,
}

impl SampleFormat {
    /// Preference when a device offers several formats at the chosen channel count: floats
    /// first (no conversion for f32), then the widest integers.
    fn rank(self) -> u8 {
        match self {
            SampleFormat::F32 => 0,
            SampleFormat::F64 => 1,
            SampleFormat::I32 => 2,
            SampleFormat::I16 => 3,
            SampleFormat::U16 => 4,
        }
    }
}

/// Buffer sizes a device supports, in frames per callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferRange {
    Range {
        min: u32,
        max: u32,
    },
    /// The host cannot say before a stream is open.
    Unknown,
}

/// One supported output configuration range of a device (cpal's `SupportedStreamConfigRange`,
/// with formats this layer cannot write already dropped).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfigRange {
    pub channels: u16,
    pub min_rate: u32,
    pub max_rate: u32,
    pub buffer: BufferRange,
    pub format: SampleFormat,
}

/// What the engine asks for. The rate is the mixer's; the channel count is the mixer's outputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub sample_rate: u32,
    pub channels: u16,
    /// Frames per callback; `None` lets the host choose.
    pub buffer_frames: Option<u32>,
}

/// The buffer size to open the stream with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferChoice {
    Fixed(u32),
    /// The host's default size.
    Default,
}

/// The configuration to open, and how the mixer's outputs map onto it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Negotiated {
    pub sample_rate: u32,
    /// Channels the stream is opened with (the device's channel count; may exceed 64).
    pub device_channels: u16,
    /// Mixer outputs that reach the device: outputs `0..mapped` go to device channels
    /// `0..mapped`. Less than the request when the device has fewer channels; the rest of the
    /// device's channels get silence.
    pub mapped: u16,
    pub format: SampleFormat,
    pub buffer: BufferChoice,
    /// True when the device's buffer range is unknown and `buffer` is the requested size as a
    /// guess: the opener falls back to [`BufferChoice::Default`] if the host refuses it.
    pub buffer_unverified: bool,
}

impl Negotiated {
    /// True when some of the requested mixer outputs have no device channel.
    pub fn outputs_dropped(&self, requested: u16) -> bool {
        self.mapped < requested
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NegotiateError {
    /// The device reported no output configuration in a format this layer writes.
    NoConfigs,
    /// No configuration runs at the mixer's rate. The engine decides whether to rebuild the
    /// mixer at a rate the device supports.
    SampleRate { requested: u32 },
    /// The request is outside what the mixer allows (0 or more than 64 channels, rate 0).
    BadRequest,
}

/// Chooses a configuration for `req` from `ranges`.
///
/// - The rate must be supported exactly: the mixer's rate is fixed by its config.
/// - Channels: the requested count if offered; else the smallest count above it (the extra
///   device channels get silence); else the largest below it (only the first ones are mapped).
/// - Format at that channel count: f32, f64, i32, i16, u16 in that order.
/// - Buffer: the requested size if the range includes it, else the nearest end of the range;
///   with an unknown range, the requested size, marked unverified. No request: the default.
pub fn negotiate(ranges: &[ConfigRange], req: Request) -> Result<Negotiated, NegotiateError> {
    let wanted = usize::from(req.channels);
    if req.sample_rate == 0 || !(1..=MAX_OUTPUT_CHANNELS).contains(&wanted) {
        return Err(NegotiateError::BadRequest);
    }
    let usable = || ranges.iter().filter(|r| r.channels > 0);
    if usable().next().is_none() {
        return Err(NegotiateError::NoConfigs);
    }
    let at_rate = || usable().filter(|r| (r.min_rate..=r.max_rate).contains(&req.sample_rate));
    let channels = at_rate()
        .map(|r| r.channels)
        .min_by_key(|&c| channel_preference(c, req.channels))
        .ok_or(NegotiateError::SampleRate {
            requested: req.sample_rate,
        })?;
    let best = at_rate()
        .filter(|r| r.channels == channels)
        .min_by_key(|r| r.format.rank())
        .ok_or(NegotiateError::SampleRate {
            requested: req.sample_rate,
        })?;
    let (buffer, buffer_unverified) = match (req.buffer_frames, best.buffer) {
        (None, _) => (BufferChoice::Default, false),
        (Some(n), BufferRange::Range { min, max }) if min <= max => {
            (BufferChoice::Fixed(n.clamp(min.max(1), max.max(1))), false)
        }
        (Some(n), _) => (BufferChoice::Fixed(n.max(1)), true),
    };
    Ok(Negotiated {
        sample_rate: req.sample_rate,
        device_channels: channels,
        mapped: channels.min(req.channels),
        format: best.format,
        buffer,
        buffer_unverified,
    })
}

/// Lower is better: exact, then the fewest extra channels, then the fewest missing ones.
fn channel_preference(offered: u16, wanted: u16) -> (u8, u16) {
    match offered.cmp(&wanted) {
        std::cmp::Ordering::Equal => (0, 0),
        std::cmp::Ordering::Greater => (1, offered - wanted),
        std::cmp::Ordering::Less => (2, wanted - offered),
    }
}

/// Output latency as the device layer can measure it (AUD-21).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Latency {
    pub sample_rate: u32,
    /// Frames per callback: the host's figure, else the last callback's size; `None` before the
    /// first callback when the host cannot say.
    pub buffer_frames: Option<u32>,
    /// Time from the callback to the device output, from the host's callback timestamps
    /// (playback − callback, last callback). `None` when the host gives no usable timestamps.
    /// Hosts differ in whether it includes the buffer itself; CoreAudio's includes the device
    /// latency and safety offset.
    pub device_seconds: Option<f64>,
}

impl Latency {
    /// One buffer at the stream's rate.
    pub fn buffer_seconds(&self) -> Option<f64> {
        let frames = self.buffer_frames?;
        (self.sample_rate > 0).then(|| f64::from(frames) / f64::from(self.sample_rate))
    }
}

/// Reopen backoff (NFR-08): the delay before reopen attempt `attempt` (0 = the first try after
/// a loss): 250 ms doubling to a 4 s cap, so a replugged interface is back within about 4 s and
/// a missing one costs at most one attempt per 4 s. The engine owns the clock and the loop.
pub fn reopen_delay_ms(attempt: u32) -> u32 {
    const FIRST_MS: u32 = 250;
    const CAP_MS: u32 = 4000;
    // 250 << 4 = 4000: from the fifth attempt on, the cap.
    if attempt >= 4 {
        CAP_MS
    } else {
        FIRST_MS << attempt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(channels: u16, min_rate: u32, max_rate: u32, format: SampleFormat) -> ConfigRange {
        ConfigRange {
            channels,
            min_rate,
            max_rate,
            buffer: BufferRange::Range { min: 32, max: 4096 },
            format,
        }
    }

    fn req(rate: u32, channels: u16, buffer: Option<u32>) -> Request {
        Request {
            sample_rate: rate,
            channels,
            buffer_frames: buffer,
        }
    }

    #[test]
    fn exact_match_is_chosen() {
        let r = [
            range(2, 44_100, 48_000, SampleFormat::I16),
            range(2, 44_100, 48_000, SampleFormat::F32),
            range(8, 44_100, 48_000, SampleFormat::F32),
        ];
        let n = negotiate(&r, req(48_000, 2, Some(256))).unwrap();
        assert_eq!(n.device_channels, 2);
        assert_eq!(n.mapped, 2);
        assert_eq!(n.format, SampleFormat::F32);
        assert_eq!(n.buffer, BufferChoice::Fixed(256));
        assert!(!n.buffer_unverified);
        assert!(!n.outputs_dropped(2));
    }

    #[test]
    fn buffer_outside_range_takes_the_nearest_end() {
        let r = [range(2, 48_000, 48_000, SampleFormat::F32)];
        assert_eq!(
            negotiate(&r, req(48_000, 2, Some(8))).unwrap().buffer,
            BufferChoice::Fixed(32)
        );
        assert_eq!(
            negotiate(&r, req(48_000, 2, Some(100_000))).unwrap().buffer,
            BufferChoice::Fixed(4096)
        );
        assert_eq!(
            negotiate(&r, req(48_000, 2, None)).unwrap().buffer,
            BufferChoice::Default
        );
    }

    #[test]
    fn unknown_buffer_range_tries_the_request_unverified() {
        let mut r = [range(2, 48_000, 48_000, SampleFormat::F32)];
        r[0].buffer = BufferRange::Unknown;
        let n = negotiate(&r, req(48_000, 2, Some(512))).unwrap();
        assert_eq!(n.buffer, BufferChoice::Fixed(512));
        assert!(n.buffer_unverified);
        let n = negotiate(&r, req(48_000, 2, None)).unwrap();
        assert_eq!(n.buffer, BufferChoice::Default);
        assert!(!n.buffer_unverified);
    }

    #[test]
    fn unsupported_rate_is_a_typed_error() {
        let r = [range(2, 44_100, 48_000, SampleFormat::F32)];
        assert_eq!(
            negotiate(&r, req(96_000, 2, None)),
            Err(NegotiateError::SampleRate { requested: 96_000 })
        );
    }

    #[test]
    fn more_device_channels_open_the_smallest_superset() {
        let r = [
            range(16, 48_000, 48_000, SampleFormat::F32),
            range(4, 48_000, 48_000, SampleFormat::F32),
            range(1, 48_000, 48_000, SampleFormat::F32),
        ];
        let n = negotiate(&r, req(48_000, 2, None)).unwrap();
        assert_eq!(n.device_channels, 4);
        assert_eq!(n.mapped, 2);
    }

    #[test]
    fn fewer_device_channels_map_the_first_and_report_it() {
        let r = [
            range(1, 48_000, 48_000, SampleFormat::F32),
            range(2, 48_000, 48_000, SampleFormat::F32),
        ];
        let n = negotiate(&r, req(48_000, 8, None)).unwrap();
        assert_eq!(n.device_channels, 2);
        assert_eq!(n.mapped, 2);
        assert!(n.outputs_dropped(8));
    }

    #[test]
    fn sixty_four_channel_device() {
        let r = [range(64, 48_000, 96_000, SampleFormat::I32)];
        let n = negotiate(&r, req(96_000, 64, Some(128))).unwrap();
        assert_eq!((n.device_channels, n.mapped), (64, 64));
        assert_eq!(n.format, SampleFormat::I32);
        // A larger device still maps at most the mixer's 64 outputs.
        let r = [range(128, 48_000, 48_000, SampleFormat::F32)];
        let n = negotiate(&r, req(48_000, 64, None)).unwrap();
        assert_eq!((n.device_channels, n.mapped), (128, 64));
    }

    #[test]
    fn empty_or_zero_channel_lists_are_no_configs() {
        assert_eq!(
            negotiate(&[], req(48_000, 2, None)),
            Err(NegotiateError::NoConfigs)
        );
        let r = [range(0, 48_000, 48_000, SampleFormat::F32)];
        assert_eq!(
            negotiate(&r, req(48_000, 2, None)),
            Err(NegotiateError::NoConfigs)
        );
    }

    #[test]
    fn bad_requests_are_refused() {
        let r = [range(2, 48_000, 48_000, SampleFormat::F32)];
        assert_eq!(
            negotiate(&r, req(0, 2, None)),
            Err(NegotiateError::BadRequest)
        );
        assert_eq!(
            negotiate(&r, req(48_000, 0, None)),
            Err(NegotiateError::BadRequest)
        );
        assert_eq!(
            negotiate(&r, req(48_000, 65, None)),
            Err(NegotiateError::BadRequest)
        );
    }

    #[test]
    fn rate_filter_applies_before_channel_choice() {
        // The exact channel count exists only at another rate: take what runs at ours.
        let r = [
            range(2, 44_100, 44_100, SampleFormat::F32),
            range(6, 48_000, 48_000, SampleFormat::F32),
        ];
        let n = negotiate(&r, req(48_000, 2, None)).unwrap();
        assert_eq!(n.device_channels, 6);
    }

    #[test]
    fn format_preference_order() {
        let r = [
            range(2, 48_000, 48_000, SampleFormat::U16),
            range(2, 48_000, 48_000, SampleFormat::I16),
            range(2, 48_000, 48_000, SampleFormat::I32),
        ];
        assert_eq!(
            negotiate(&r, req(48_000, 2, None)).unwrap().format,
            SampleFormat::I32
        );
        assert_eq!(
            negotiate(&r[..2], req(48_000, 2, None)).unwrap().format,
            SampleFormat::I16
        );
    }

    #[test]
    fn inverted_buffer_range_is_treated_as_unknown() {
        let mut r = [range(2, 48_000, 48_000, SampleFormat::F32)];
        r[0].buffer = BufferRange::Range { min: 512, max: 64 };
        let n = negotiate(&r, req(48_000, 2, Some(256))).unwrap();
        assert_eq!(n.buffer, BufferChoice::Fixed(256));
        assert!(n.buffer_unverified);
    }

    #[test]
    fn latency_readout() {
        let l = Latency {
            sample_rate: 48_000,
            buffer_frames: Some(480),
            device_seconds: None,
        };
        assert_eq!(l.buffer_seconds(), Some(0.01));
        let l = Latency {
            buffer_frames: None,
            ..l
        };
        assert_eq!(l.buffer_seconds(), None);
    }

    #[test]
    fn backoff_doubles_to_a_cap() {
        let d: Vec<u32> = (0..8).map(reopen_delay_ms).collect();
        assert_eq!(d, [250, 500, 1000, 2000, 4000, 4000, 4000, 4000]);
        assert_eq!(reopen_delay_ms(31), 4000);
        assert_eq!(reopen_delay_ms(u32::MAX), 4000);
    }
}
