// SPDX-License-Identifier: GPL-3.0-or-later

//! Output devices through cpal (AUD-01, AUD-03, AUD-21, NFR-08): list them, open one for a
//! mixer, and get the mixer back when the stream is closed or lost. The `device` feature gates
//! this module; the callback adapter ([`crate::OutputHost`]) and configuration choice
//! ([`crate::negotiate`]) are pure and built without it. Reopen policy (when, how often, what
//! the operator sees) is the engine's. See `docs/audio.md`, Device layer.

use std::fmt;
use std::str::FromStr;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::mixer::Mixer;
use crate::negotiate::{
    BufferChoice, BufferRange, ConfigRange, Latency, NegotiateError, Negotiated, Request,
    SampleFormat, negotiate,
};
use crate::output::{CallbackTime, DeviceMonitor, HostError, HostLink, OutputHost};

/// An output device as listed for settings (AUD-03).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Stable across restarts and replugs (cpal's `DeviceId`, `"host:id"`); store it in settings.
    pub id: String,
    /// The name to show.
    pub name: String,
    pub is_default: bool,
    /// Supported configurations in formats the device layer writes. Empty if the device would
    /// not say (it may have gone away while listing).
    pub configs: Vec<ConfigRange>,
}

impl DeviceInfo {
    pub fn max_channels(&self) -> u16 {
        self.configs.iter().map(|c| c.channels).max().unwrap_or(0)
    }
}

/// Why a device could not be listed or opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenErrorKind {
    /// No device with that id, or no default output device.
    NotFound,
    /// The device or the audio system is not available (unplugged, not running).
    Unavailable,
    /// Another application holds the device; retrying may succeed.
    Busy,
    PermissionDenied,
    /// The host refused the negotiated configuration.
    UnsupportedConfig,
    /// No configuration fits the mixer (see [`NegotiateError`]).
    Negotiate(NegotiateError),
    Backend,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceError {
    pub kind: OpenErrorKind,
    /// The host's description, for the log.
    pub message: String,
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for DeviceError {}

impl DeviceError {
    fn from_cpal(e: &cpal::Error) -> Self {
        DeviceError {
            kind: open_kind(e.kind()),
            message: e.to_string(),
        }
    }

    fn new(kind: OpenErrorKind, message: &str) -> Self {
        DeviceError {
            kind,
            message: message.to_owned(),
        }
    }
}

/// Lists output devices on the platform's default host (CoreAudio, WASAPI; ALSA on Linux).
/// Zero devices is not an error.
pub fn output_devices() -> Result<Vec<DeviceInfo>, DeviceError> {
    let host = cpal::default_host();
    let default = host.default_output_device().and_then(|d| d.id().ok());
    let devices = host
        .output_devices()
        .map_err(|e| DeviceError::from_cpal(&e))?;
    Ok(devices
        .filter_map(|d| {
            let id = d.id().ok()?;
            Some(DeviceInfo {
                is_default: default.as_ref() == Some(&id),
                id: id.to_string(),
                name: device_name(&d),
                configs: configs_of(&d),
            })
        })
        .collect())
}

/// The id of the current default output device, if there is one.
pub fn default_output_device_id() -> Option<String> {
    cpal::default_host()
        .default_output_device()
        .and_then(|d| d.id().ok())
        .map(|id| id.to_string())
}

/// What to open. The rate and channel count come from the mixer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenRequest {
    /// A [`DeviceInfo::id`]; `None` for the default output device.
    pub device_id: Option<String>,
    /// Frames per callback; `None` for the host's default (AUD-21).
    pub buffer_frames: Option<u32>,
}

/// A failed open. The mixer is not lost: [`OpenError::take_mixer`] returns it.
pub struct OpenError {
    pub error: DeviceError,
    mixer: Returned,
}

enum Returned {
    // Boxed to keep the error small; the mixer is a few hundred bytes of handles.
    Mixer(Box<Mixer>),
    Pending(MixerPending),
    Taken,
}

impl OpenError {
    /// The mixer, intact. `None` only if the host library still holds the callback (then
    /// retry later) or it was already taken.
    pub fn take_mixer(&mut self) -> Option<Mixer> {
        match std::mem::replace(&mut self.mixer, Returned::Taken) {
            Returned::Mixer(m) => Some(*m),
            Returned::Pending(mut p) => {
                let m = p.try_take();
                if m.is_none() {
                    self.mixer = Returned::Pending(p);
                }
                m
            }
            Returned::Taken => None,
        }
    }
}

impl fmt::Debug for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

/// A closed stream whose callback the host library has not dropped yet; poll for the mixer.
pub struct MixerPending {
    link: HostLink,
}

impl MixerPending {
    pub fn try_take(&mut self) -> Option<Mixer> {
        self.link.take_mixer()
    }
}

impl fmt::Debug for MixerPending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MixerPending")
    }
}

/// A running output stream that owns the mixer on the audio thread.
///
/// Close it with [`OutputStream::close`] to get the mixer back. Just dropping it frees the mixer
/// (on the dropping thread).
pub struct OutputStream {
    // Dropped before `link`, so the mixer has come back by the time the link goes.
    stream: cpal::Stream,
    link: HostLink,
    negotiated: Negotiated,
    requested_channels: u16,
    device_id: String,
    device_name: String,
}

impl fmt::Debug for OutputStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OutputStream")
            .field("device_id", &self.device_id)
            .field("negotiated", &self.negotiated)
            .finish_non_exhaustive()
    }
}

impl OutputStream {
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// The configuration the stream runs with.
    pub fn negotiated(&self) -> Negotiated {
        self.negotiated
    }

    /// True when the device has fewer channels than the mixer has outputs (the extra outputs are
    /// not heard; warn the operator).
    pub fn outputs_dropped(&self) -> bool {
        self.negotiated.outputs_dropped(self.requested_channels)
    }

    /// Errors, reopen flag, callback count and latency, readable from the control thread.
    pub fn monitor(&self) -> DeviceMonitor {
        self.link.monitor()
    }

    /// AUD-21 readout: the buffer (the host's current size, else the last callback's, else the
    /// fixed size asked for) and the host's callback-to-output time.
    pub fn latency(&self) -> Latency {
        let monitor = self.link.monitor();
        let fixed = match self.negotiated.buffer {
            BufferChoice::Fixed(n) => Some(n),
            BufferChoice::Default => None,
        };
        Latency {
            sample_rate: self.negotiated.sample_rate,
            buffer_frames: self
                .stream
                .buffer_size()
                .ok()
                .filter(|n| *n > 0)
                .or_else(|| monitor.last_callback_frames())
                .or(fixed),
            device_seconds: monitor.device_latency_seconds(),
        }
    }

    /// Stops the stream and returns the mixer with its voices, queues, clock and counters, ready
    /// for [`open_output`] on this or another device. Works on a lost device too.
    pub fn close(self) -> Result<Mixer, MixerPending> {
        let OutputStream {
            stream, mut link, ..
        } = self;
        drop(stream);
        link.take_mixer().ok_or(MixerPending { link })
    }
}

/// Opens `req`'s device for `mixer` at the mixer's rate and output count and starts it.
///
/// The rate must be one the device supports (else `Negotiate(SampleRate)`: rebuild the mixer
/// or pick another device). If the device has a different channel count, mixer outputs map onto
/// its first channels (see [`negotiate`]). On any failure the mixer comes back in the error.
pub fn open_output(mixer: Mixer, req: &OpenRequest) -> Result<OutputStream, OpenError> {
    let fail = |mixer: Mixer, error: DeviceError| OpenError {
        error,
        mixer: Returned::Mixer(Box::new(mixer)),
    };
    let host = cpal::default_host();
    let device = match find_device(&host, req.device_id.as_deref()) {
        Ok(d) => d,
        Err(e) => return Err(fail(mixer, e)),
    };
    let device_id = device.id().map(|i| i.to_string()).unwrap_or_default();
    let device_name = device_name(&device);
    let requested_channels = u16::try_from(mixer.output_channels()).unwrap_or(u16::MAX);
    let request = Request {
        sample_rate: mixer.sample_rate(),
        channels: requested_channels,
        buffer_frames: req.buffer_frames,
    };
    let mut negotiated = match negotiate(&configs_of(&device), request) {
        Ok(n) => n,
        Err(e) => {
            return Err(fail(
                mixer,
                DeviceError::new(OpenErrorKind::Negotiate(e), "no matching configuration"),
            ));
        }
    };
    let mut mixer = mixer;
    let mut attempt = build(&device, mixer, negotiated);
    // An unknown buffer range made the requested size a guess; the host's default is next best.
    if negotiated.buffer_unverified
        && let Err(e) = &mut attempt
        && e.error.kind == OpenErrorKind::UnsupportedConfig
        && let Some(m) = e.take_mixer()
    {
        mixer = m;
        negotiated.buffer = BufferChoice::Default;
        negotiated.buffer_unverified = false;
        attempt = build(&device, mixer, negotiated);
    }
    let (stream, link) = attempt?;
    Ok(OutputStream {
        stream,
        link,
        negotiated,
        requested_channels,
        device_id,
        device_name,
    })
}

fn build(
    device: &cpal::Device,
    mixer: Mixer,
    n: Negotiated,
) -> Result<(cpal::Stream, HostLink), OpenError> {
    let (mut host, link) = OutputHost::new(mixer, n.device_channels);
    let reporter = link.reporter();
    let config = cpal::StreamConfig {
        channels: n.device_channels,
        sample_rate: n.sample_rate,
        buffer_size: match n.buffer {
            BufferChoice::Fixed(f) => cpal::BufferSize::Fixed(f),
            BufferChoice::Default => cpal::BufferSize::Default,
        },
    };
    let built = device.build_output_stream_raw(
        config,
        cpal_format(n.format),
        move |data: &mut cpal::Data, info: &cpal::OutputCallbackInfo| {
            fill(&mut host, data, info);
        },
        // The cpal::Error is the host library's; dropping it may free its message string.
        move |e: cpal::Error| reporter.report(map_error_kind(e.kind())),
        None,
    );
    let failed = |link: HostLink, e: &cpal::Error| OpenError {
        error: DeviceError::from_cpal(e),
        mixer: Returned::Pending(MixerPending { link }),
    };
    let stream = match built {
        Ok(s) => s,
        Err(e) => return Err(failed(link, &e)),
    };
    if let Err(e) = stream.play() {
        drop(stream);
        return Err(failed(link, &e));
    }
    Ok((stream, link))
}

/// The cpal data callback: dispatch on the buffer's format to the shared adapter.
fn fill(host: &mut OutputHost, data: &mut cpal::Data, info: &cpal::OutputCallbackInfo) {
    let ts = info.timestamp();
    let time = Some(CallbackTime {
        callback_ns: nanos(ts.callback),
        playback_ns: nanos(ts.playback),
    });
    let done = match data.sample_format() {
        cpal::SampleFormat::F32 => data.as_slice_mut::<f32>().map(|b| host.render(b, time)),
        cpal::SampleFormat::F64 => data.as_slice_mut::<f64>().map(|b| host.render(b, time)),
        cpal::SampleFormat::I32 => data.as_slice_mut::<i32>().map(|b| host.render(b, time)),
        cpal::SampleFormat::I16 => data.as_slice_mut::<i16>().map(|b| host.render(b, time)),
        cpal::SampleFormat::U16 => data.as_slice_mut::<u16>().map(|b| host.render(b, time)),
        _ => None,
    };
    if done.is_none() {
        host.reject_buffer();
    }
}

fn nanos(t: cpal::StreamInstant) -> u64 {
    u64::try_from(t.as_nanos()).unwrap_or(u64::MAX)
}

/// The host error callback's kinds, as the control thread sees them.
pub(crate) fn map_error_kind(kind: cpal::ErrorKind) -> HostError {
    match kind {
        cpal::ErrorKind::DeviceNotAvailable => HostError::Lost,
        cpal::ErrorKind::StreamInvalidated => HostError::Invalidated,
        cpal::ErrorKind::DeviceChanged => HostError::Rerouted,
        cpal::ErrorKind::RealtimeDenied => HostError::RealtimeDenied,
        cpal::ErrorKind::Xrun => HostError::Xrun,
        _ => HostError::Other,
    }
}

fn open_kind(kind: cpal::ErrorKind) -> OpenErrorKind {
    match kind {
        cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::HostUnavailable => {
            OpenErrorKind::Unavailable
        }
        cpal::ErrorKind::DeviceBusy => OpenErrorKind::Busy,
        cpal::ErrorKind::PermissionDenied => OpenErrorKind::PermissionDenied,
        cpal::ErrorKind::UnsupportedConfig | cpal::ErrorKind::InvalidInput => {
            OpenErrorKind::UnsupportedConfig
        }
        _ => OpenErrorKind::Backend,
    }
}

fn find_device(host: &cpal::Host, id: Option<&str>) -> Result<cpal::Device, DeviceError> {
    let not_found = || DeviceError::new(OpenErrorKind::NotFound, "no such output device");
    let Some(id) = id else {
        return host.default_output_device().ok_or_else(not_found);
    };
    let parsed = cpal::DeviceId::from_str(id).map_err(|_| not_found())?;
    host.device_by_id(&parsed)
        .filter(DeviceTrait::supports_output)
        .ok_or_else(not_found)
}

fn device_name(d: &cpal::Device) -> String {
    d.description()
        .map(|desc| desc.name().to_owned())
        .unwrap_or_else(|_| d.to_string())
}

fn configs_of(d: &cpal::Device) -> Vec<ConfigRange> {
    d.supported_output_configs()
        .map(|it| it.filter_map(|r| config_range(&r)).collect())
        .unwrap_or_default()
}

fn config_range(r: &cpal::SupportedStreamConfigRange) -> Option<ConfigRange> {
    Some(ConfigRange {
        channels: r.channels(),
        min_rate: r.min_sample_rate(),
        max_rate: r.max_sample_rate(),
        buffer: match *r.buffer_size() {
            cpal::SupportedBufferSize::Range { min, max } => BufferRange::Range { min, max },
            cpal::SupportedBufferSize::Unknown => BufferRange::Unknown,
        },
        format: our_format(r.sample_format())?,
    })
}

fn our_format(f: cpal::SampleFormat) -> Option<SampleFormat> {
    match f {
        cpal::SampleFormat::F32 => Some(SampleFormat::F32),
        cpal::SampleFormat::F64 => Some(SampleFormat::F64),
        cpal::SampleFormat::I32 => Some(SampleFormat::I32),
        cpal::SampleFormat::I16 => Some(SampleFormat::I16),
        cpal::SampleFormat::U16 => Some(SampleFormat::U16),
        _ => None,
    }
}

fn cpal_format(f: SampleFormat) -> cpal::SampleFormat {
    match f {
        SampleFormat::F32 => cpal::SampleFormat::F32,
        SampleFormat::F64 => cpal::SampleFormat::F64,
        SampleFormat::I32 => cpal::SampleFormat::I32,
        SampleFormat::I16 => cpal::SampleFormat::I16,
        SampleFormat::U16 => cpal::SampleFormat::U16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_error_kinds_map() {
        use cpal::ErrorKind as K;
        assert_eq!(map_error_kind(K::DeviceNotAvailable), HostError::Lost);
        assert_eq!(map_error_kind(K::StreamInvalidated), HostError::Invalidated);
        assert_eq!(map_error_kind(K::DeviceChanged), HostError::Rerouted);
        assert_eq!(map_error_kind(K::RealtimeDenied), HostError::RealtimeDenied);
        assert_eq!(map_error_kind(K::Xrun), HostError::Xrun);
        assert_eq!(map_error_kind(K::BackendError), HostError::Other);
        assert_eq!(open_kind(K::DeviceBusy), OpenErrorKind::Busy);
        assert_eq!(open_kind(K::InvalidInput), OpenErrorKind::UnsupportedConfig);
        assert_eq!(open_kind(K::HostUnavailable), OpenErrorKind::Unavailable);
        assert_eq!(open_kind(K::Other), OpenErrorKind::Backend);
    }

    #[test]
    fn formats_round_trip_and_others_are_skipped() {
        for f in [
            SampleFormat::F32,
            SampleFormat::F64,
            SampleFormat::I32,
            SampleFormat::I16,
            SampleFormat::U16,
        ] {
            assert_eq!(our_format(cpal_format(f)), Some(f));
        }
        assert_eq!(our_format(cpal::SampleFormat::I24), None);
        assert_eq!(our_format(cpal::SampleFormat::U8), None);
        assert_eq!(our_format(cpal::SampleFormat::DsdU8), None);
    }

    #[test]
    fn config_ranges_map() {
        let r = cpal::SupportedStreamConfigRange::new(
            6,
            44_100,
            96_000,
            cpal::SupportedBufferSize::Range { min: 16, max: 2048 },
            cpal::SampleFormat::I16,
        );
        assert_eq!(
            config_range(&r),
            Some(ConfigRange {
                channels: 6,
                min_rate: 44_100,
                max_rate: 96_000,
                buffer: BufferRange::Range { min: 16, max: 2048 },
                format: SampleFormat::I16,
            })
        );
        let r = cpal::SupportedStreamConfigRange::new(
            2,
            48_000,
            48_000,
            cpal::SupportedBufferSize::Unknown,
            cpal::SampleFormat::I24,
        );
        assert_eq!(config_range(&r), None);
    }

    #[test]
    fn listing_tolerates_zero_devices() {
        // CI runners and VMs may have no output device; listing must still succeed or fail
        // cleanly, never panic.
        if let Ok(list) = output_devices() {
            assert!(list.iter().filter(|d| d.is_default).count() <= 1);
        }
    }

    #[test]
    fn opening_an_unknown_device_returns_the_mixer() {
        let (_ctl, mixer) = Mixer::new(crate::MixerConfig::default()).unwrap();
        let req = OpenRequest {
            device_id: Some("not-a-host:nothing".into()),
            buffer_frames: None,
        };
        let mut err = open_output(mixer, &req).unwrap_err();
        assert_eq!(err.error.kind, OpenErrorKind::NotFound);
        assert!(err.take_mixer().is_some());
        assert!(err.take_mixer().is_none());
    }
}
