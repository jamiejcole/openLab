// SPDX-License-Identifier: GPL-3.0-or-later

//! The device callback adapter (ARCHITECTURE §3.1, §3.4, §3.5; NFR-02, NFR-08, NFR-27): runs the
//! mixer for one host buffer in any supported sample format and channel count, watches callback
//! timestamps for gaps, and hands the mixer back to the control thread when the stream goes away.
//! No cpal here: the cpal closure calls [`OutputHost::render`], and tests drive it the same way
//! with fake buffers and timestamps. See `docs/audio.md`, Device layer.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::meter::XrunNotifier;
use crate::mixer::Mixer;

/// Frames the conversion scratch holds; longer callbacks are rendered in pieces this size.
pub const SCRATCH_FRAMES: usize = 1024;

/// A sample type the device layer writes. Integers are clamped to full scale; NaN becomes 0.
pub trait OutputSample: Copy {
    const SILENCE: Self;
    fn from_f32(x: f32) -> Self;
    /// The buffer as `f32`, so f32 output skips the scratch when no channel mapping is needed.
    fn as_f32_mut(_buf: &mut [Self]) -> Option<&mut [f32]> {
        None
    }
}

impl OutputSample for f32 {
    const SILENCE: Self = 0.0;
    fn from_f32(x: f32) -> Self {
        x
    }
    fn as_f32_mut(buf: &mut [f32]) -> Option<&mut [f32]> {
        Some(buf)
    }
}

impl OutputSample for f64 {
    const SILENCE: Self = 0.0;
    fn from_f32(x: f32) -> Self {
        f64::from(x)
    }
}

// Float-to-int `as` saturates and maps NaN to 0, so the clamp only keeps +1.0 from rounding
// past the top code.
impl OutputSample for i16 {
    const SILENCE: Self = 0;
    fn from_f32(x: f32) -> Self {
        (x.clamp(-1.0, 1.0) * 32_768.0).round() as i16
    }
}

impl OutputSample for u16 {
    const SILENCE: Self = 0x8000;
    fn from_f32(x: f32) -> Self {
        (i32::from(i16::from_f32(x)) + 0x8000) as u16
    }
}

impl OutputSample for i32 {
    const SILENCE: Self = 0;
    fn from_f32(x: f32) -> Self {
        (f64::from(x.clamp(-1.0, 1.0)) * 2_147_483_648.0).round() as i32
    }
}

/// What a host reported about the stream (its error callback) or what the callback saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HostError {
    /// The device went away (unplugged, powered off). The stream is dead: reopen.
    Lost,
    /// The stream must be rebuilt (e.g. the device's rate was changed elsewhere). Reopen.
    Invalidated,
    /// The host moved the stream to another device on its own, or the system default changed
    /// under it. The negotiated configuration no longer describes the output: reopen.
    Rerouted,
    /// The audio thread did not get real-time priority.
    RealtimeDenied,
    /// The host reported a dropout. Counted in the mixer's `xruns`, not in [`StreamErrors`].
    Xrun,
    /// The callback got a buffer it could not use (wrong sample format, or a length that is not
    /// whole frames); the unusable part stayed silent.
    BadBuffer,
    /// Any other backend error.
    Other,
}

const KINDS: usize = 6;

impl HostError {
    /// The stream no longer plays on the device it was opened for: close it and reopen.
    pub fn needs_reopen(self) -> bool {
        matches!(
            self,
            HostError::Lost | HostError::Invalidated | HostError::Rerouted
        )
    }

    fn slot(self) -> Option<usize> {
        match self {
            HostError::Lost => Some(0),
            HostError::Invalidated => Some(1),
            HostError::Rerouted => Some(2),
            HostError::RealtimeDenied => Some(3),
            HostError::BadBuffer => Some(4),
            HostError::Other => Some(5),
            HostError::Xrun => None,
        }
    }
}

/// Error counts since the previous [`DeviceMonitor::take_errors`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamErrors {
    pub lost: u64,
    pub invalidated: u64,
    pub rerouted: u64,
    pub realtime_denied: u64,
    pub bad_buffer: u64,
    pub other: u64,
}

impl StreamErrors {
    pub fn is_empty(&self) -> bool {
        *self == StreamErrors::default()
    }
}

const UNKNOWN: u64 = u64::MAX;

struct DeviceShared {
    errors: [AtomicU64; KINDS],
    needs_reopen: AtomicBool,
    host_xruns: AtomicU64,
    gap_xruns: AtomicU64,
    callbacks: AtomicU64,
    last_frames: AtomicU32,
    device_latency_ns: AtomicU64,
    callback_faults: AtomicU64,
    alloc_violations: AtomicU64,
    leaked_mixers: AtomicU64,
    /// Worst callback load since the last [`DeviceMonitor::take_engine_load`], encoded by
    /// [`LoadMeter`].
    worst_load: LoadMeter,
}

/// The worst callback load (render time over the block's duration) since it was last taken,
/// in one atomic: parts per million plus one, so 0 means "no callback measured". Integers so
/// `fetch_max` keeps the worst without a compare-exchange loop and without floats on the audio
/// thread.
struct LoadMeter(AtomicU64);

impl LoadMeter {
    const NONE: u64 = 0;

    fn new() -> Self {
        LoadMeter(AtomicU64::new(Self::NONE))
    }

    /// Records one callback that took `elapsed_ns` to fill `frames` at `rate`. Real-time safe:
    /// integer arithmetic and one atomic `fetch_max`. A block of no frames, or no rate, has no
    /// duration to compare with and is not recorded.
    fn record(&self, elapsed_ns: u64, frames: u64, rate: u32) {
        if frames == 0 || rate == 0 {
            return;
        }
        // load = elapsed / (frames / rate); in ppm: elapsed_ns * rate / (frames * 1000).
        // u128 cannot overflow for any u64 inputs.
        let ppm = u128::from(elapsed_ns) * u128::from(rate) / (u128::from(frames) * 1000);
        let encoded = u64::try_from(ppm).unwrap_or(u64::MAX - 1).saturating_add(1);
        self.0.fetch_max(encoded, Ordering::Relaxed);
    }

    /// The worst load since the previous call, as a fraction (1.0: the callback took as long
    /// as the block lasts); `None` when no callback was measured in between. Resets.
    fn take(&self) -> Option<f64> {
        match self.0.swap(Self::NONE, Ordering::Relaxed) {
            Self::NONE => None,
            v => Some((v - 1) as f64 / 1e6),
        }
    }
}

impl DeviceShared {
    fn new() -> Self {
        DeviceShared {
            errors: Default::default(),
            needs_reopen: AtomicBool::new(false),
            host_xruns: AtomicU64::new(0),
            gap_xruns: AtomicU64::new(0),
            callbacks: AtomicU64::new(0),
            last_frames: AtomicU32::new(0),
            device_latency_ns: AtomicU64::new(UNKNOWN),
            callback_faults: AtomicU64::new(0),
            alloc_violations: AtomicU64::new(0),
            leaked_mixers: AtomicU64::new(0),
            worst_load: LoadMeter::new(),
        }
    }

    fn count(&self, error: HostError) {
        match error.slot() {
            Some(k) => {
                if let Some(c) = self.errors.get(k) {
                    c.fetch_add(1, Ordering::Relaxed);
                }
                if error.needs_reopen() {
                    self.needs_reopen.store(true, Ordering::Release);
                }
            }
            None => {
                self.host_xruns.fetch_add(1, Ordering::Release);
            }
        }
    }
}

/// Records host errors from any thread (the stream's error callback). Cheap to clone,
/// `Send + Sync`, real-time safe: one or two atomic writes, no allocation.
#[derive(Clone)]
pub struct ErrorReporter {
    shared: Arc<DeviceShared>,
    xruns: XrunNotifier,
}

impl ErrorReporter {
    pub fn report(&self, error: HostError) {
        self.shared.count(error);
        if error == HostError::Xrun {
            self.xruns.note(1);
        }
    }
}

/// Control-thread view of a stream's health. Cheap to clone; reads never block the audio side.
#[derive(Clone)]
pub struct DeviceMonitor {
    shared: Arc<DeviceShared>,
}

impl DeviceMonitor {
    /// Error counts since the previous call (read-and-reset, so each report is seen once).
    /// Single consumer: the counts are shared by all clones, so one owner (the engine's control
    /// loop) should take them and pass them on; a second poller would steal reports.
    pub fn take_errors(&self) -> StreamErrors {
        let take = |k: usize| {
            self.shared
                .errors
                .get(k)
                .map_or(0, |c| c.swap(0, Ordering::Relaxed))
        };
        StreamErrors {
            lost: take(0),
            invalidated: take(1),
            rerouted: take(2),
            realtime_denied: take(3),
            bad_buffer: take(4),
            other: take(5),
        }
    }

    /// Sticky: the host reported an error for which [`HostError::needs_reopen`] holds (lost,
    /// invalidated, rerouted). The stream will not recover by itself; close it and reopen.
    /// Non-destructive, so any number of clones may poll it.
    pub fn needs_reopen(&self) -> bool {
        self.shared.needs_reopen.load(Ordering::Acquire)
    }

    /// Data callbacks so far. A stream that stops calling back without an error (some hosts
    /// on unplug) shows as this, and `samples_rendered`, standing still.
    pub fn callbacks(&self) -> u64 {
        self.shared.callbacks.load(Ordering::Relaxed)
    }

    /// Frames in the most recent callback.
    pub fn last_callback_frames(&self) -> Option<u32> {
        Some(self.shared.last_frames.load(Ordering::Relaxed)).filter(|f| *f > 0)
    }

    /// Callback-to-output time from the last callback's host timestamps.
    pub fn device_latency_seconds(&self) -> Option<f64> {
        let ns = self.shared.device_latency_ns.load(Ordering::Relaxed);
        (ns != UNKNOWN).then_some(ns as f64 * 1e-9)
    }

    /// Xruns reported by the host's error callback (all xruns, these and timestamp gaps, are in
    /// the mixer's `xruns`).
    pub fn host_xruns(&self) -> u64 {
        self.shared.host_xruns.load(Ordering::Acquire)
    }

    /// Xruns found from gaps in the callback timestamps.
    pub fn gap_xruns(&self) -> u64 {
        self.shared.gap_xruns.load(Ordering::Relaxed)
    }

    /// Panics caught in the callback outside the mixer (the mixer's own are its `faults`).
    pub fn callback_faults(&self) -> u64 {
        self.shared.callback_faults.load(Ordering::Relaxed)
    }

    /// Allocations `assert_no_alloc` caught in the callback (debug builds with its allocator
    /// installed; always 0 otherwise).
    pub fn alloc_violations(&self) -> u64 {
        self.shared.alloc_violations.load(Ordering::Relaxed)
    }

    /// Mixers that could not be handed back and were leaked (expected to stay 0).
    pub fn leaked_mixers(&self) -> u64 {
        self.shared.leaked_mixers.load(Ordering::Relaxed)
    }

    /// The audio engine load (DX-03): the worst time a callback took over the duration of the
    /// block it filled, since the previous call (read-and-reset). 1.0 means a callback used its
    /// whole block, so above about 0.7 dropouts are near. `None` when no callback ran since the
    /// previous call (the stream stopped or was lost). Single consumer, like
    /// [`DeviceMonitor::take_errors`]: the engine's status reporter owns it.
    pub fn take_engine_load(&self) -> Option<f64> {
        self.shared.worst_load.take()
    }
}

/// What goes back to the control thread: everything the host owns that would deallocate.
struct Parts {
    mixer: Mixer,
    scratch: Vec<f32>,
}

/// The control-thread end of an [`OutputHost`]: health and the mixer's way back.
pub struct HostLink {
    returned: Consumer<Parts>,
    shared: Arc<DeviceShared>,
    xruns: XrunNotifier,
}

impl HostLink {
    pub fn monitor(&self) -> DeviceMonitor {
        DeviceMonitor {
            shared: Arc::clone(&self.shared),
        }
    }

    /// For the stream's error callback.
    pub fn reporter(&self) -> ErrorReporter {
        ErrorReporter {
            shared: Arc::clone(&self.shared),
            xruns: self.xruns.clone(),
        }
    }

    /// The mixer, once its host was dropped; `None` while the host is alive. The mixer keeps
    /// its voices, queues, clock and counters, so a new host continues where this one stopped.
    pub fn take_mixer(&mut self) -> Option<Mixer> {
        self.returned.pop().ok().map(|p| p.mixer)
    }
}

/// Host callback timestamps for one buffer, in nanoseconds on the host's stream clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallbackTime {
    /// When the callback was invoked.
    pub callback_ns: u64,
    /// When the buffer's first frame is expected at the device output.
    pub playback_ns: u64,
}

/// Counts a callback that starts more than 1.5 buffers after the previous one as an xrun. The
/// buffer is the longer of the two callbacks, so a short buffer (an early WASAPI wake-up, a
/// buffer resize) followed by a normal one is not a gap.
#[derive(Default)]
struct GapDetector {
    last: Option<(u64, u64)>,
}

impl GapDetector {
    fn observe(&mut self, now_ns: u64, frames: u64, rate: u32) -> bool {
        let gap = match self.last {
            Some((prev, prev_frames)) if now_ns > prev && prev_frames > 0 && rate > 0 => {
                let longest = prev_frames.max(frames);
                let expected = longest.saturating_mul(1_000_000_000) / u64::from(rate);
                (now_ns - prev).saturating_mul(2) > expected.saturating_mul(3)
            }
            _ => false,
        };
        // A clock that steps back re-baselines rather than reporting anything.
        self.last = Some((now_ns, frames));
        gap
    }
}

/// The audio-thread side: owns the mixer while the stream runs. The device callback calls
/// [`OutputHost::render`]; dropping the host (wherever the host library drops its callback)
/// hands the mixer back through [`HostLink::take_mixer`] without allocating or blocking.
pub struct OutputHost {
    parts: Option<Parts>,
    device_channels: usize,
    rate: u32,
    gaps: GapDetector,
    seen_host_xruns: u64,
    shared: Arc<DeviceShared>,
    xruns: XrunNotifier,
    slot: Producer<Parts>,
    #[cfg(test)]
    panic_next: bool,
    #[cfg(test)]
    alloc_while_timing: bool,
    #[cfg(test)]
    panic_while_timing: bool,
}

impl OutputHost {
    /// Wraps `mixer` for a stream with `device_channels` interleaved channels (at least 1):
    /// mixer output `k` goes to device channel `k` for `k < min(outputs, device_channels)`,
    /// other device channels get silence. Allocates the scratch; call on the control thread.
    pub fn new(mixer: Mixer, device_channels: u16) -> (OutputHost, HostLink) {
        let device_channels = usize::from(device_channels.max(1));
        let scratch = vec![0.0; SCRATCH_FRAMES * mixer.output_channels()];
        let (slot, returned) = RingBuffer::new(1);
        let shared = Arc::new(DeviceShared::new());
        let xruns = mixer.xrun_notifier();
        let host = OutputHost {
            rate: mixer.sample_rate(),
            parts: Some(Parts { mixer, scratch }),
            device_channels,
            gaps: GapDetector::default(),
            seen_host_xruns: 0,
            shared: Arc::clone(&shared),
            xruns: xruns.clone(),
            slot,
            #[cfg(test)]
            panic_next: false,
            #[cfg(test)]
            alloc_while_timing: false,
            #[cfg(test)]
            panic_while_timing: false,
        };
        let link = HostLink {
            returned,
            shared,
            xruns,
        };
        (host, link)
    }

    pub fn device_channels(&self) -> usize {
        self.device_channels
    }

    /// Fills one host buffer (interleaved, `device_channels` per frame). Real-time safe; in
    /// debug builds it runs under `assert_no_alloc` (M3.12). Never panics: a panic outside the
    /// mixer's own guard leaves the buffer silent and counts a callback fault.
    pub fn render<S: OutputSample>(&mut self, out: &mut [S], time: Option<CallbackTime>) {
        let frames = (out.len() / self.device_channels) as u64;
        #[cfg(debug_assertions)]
        let before = assert_no_alloc::violation_count();
        // The timing runs inside the no-allocation guard, so debug builds count an allocation
        // in it, and inside its own catch_unwind (std's `Instant::now` panics if the clock
        // fails), so a panic never reaches the host's thread.
        let result = no_alloc_in_debug(|| {
            catch_unwind(AssertUnwindSafe(|| {
                // RT: reading the monotonic clock is the one exception to "no syscalls" in the
                // callback (ARCHITECTURE §3.4). `Instant::now` neither allocates nor locks, and
                // normally does not enter the kernel: `clock_gettime(CLOCK_UPTIME_RAW)` on macOS
                // reads the commpage (`mach_absolute_time`), `QueryPerformanceCounter` on
                // Windows normally reads the TSC (a kernel-mediated timer on hardware without an
                // invariant TSC and on some VMs), and Linux's `clock_gettime` is served by the
                // vDSO. Two reads per callback, tens of ns for the pair (the CPU budget gate
                // reports it), against a block of at least 1.3 ms (64 frames at 48 kHz).
                let started = Instant::now();
                let rendered = catch_unwind(AssertUnwindSafe(|| self.render_inner(out, time)));
                #[cfg(test)]
                self.test_fault_while_timing();
                let elapsed_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
                self.shared.worst_load.record(elapsed_ns, frames, self.rate);
                rendered
            }))
        });
        if !matches!(result, Ok(Ok(()))) {
            out.fill(S::SILENCE);
            self.shared.callback_faults.fetch_add(1, Ordering::Relaxed);
        }
        #[cfg(debug_assertions)]
        {
            let caught = assert_no_alloc::violation_count().wrapping_sub(before);
            if caught > 0 {
                self.shared
                    .alloc_violations
                    .fetch_add(u64::from(caught), Ordering::Relaxed);
            }
        }
    }

    /// Reports a buffer the host delivered in a format the stream was not opened with. The
    /// buffer is left as the host gave it (cpal pre-fills silence).
    pub fn reject_buffer(&self) {
        self.shared.count(HostError::BadBuffer);
    }

    #[cfg(test)]
    fn test_fault_while_timing(&mut self) {
        if std::mem::take(&mut self.alloc_while_timing) {
            std::hint::black_box(Vec::<u8>::with_capacity(16));
        }
        if std::mem::take(&mut self.panic_while_timing) {
            panic!("injected timing fault");
        }
    }

    fn render_inner<S: OutputSample>(&mut self, out: &mut [S], time: Option<CallbackTime>) {
        self.shared.callbacks.fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        if std::mem::take(&mut self.panic_next) {
            panic!("injected callback fault");
        }
        let dc = self.device_channels;
        let frames = out.len() / dc;
        let (body, tail) = out.split_at_mut(frames * dc);
        if !tail.is_empty() {
            tail.fill(S::SILENCE);
            self.reject_buffer();
        }
        self.shared
            .last_frames
            .store(u32::try_from(frames).unwrap_or(u32::MAX), Ordering::Relaxed);
        if let Some(t) = time {
            self.observe_time(t, frames as u64);
        }
        let Some(Parts { mixer, scratch }) = self.parts.as_mut() else {
            body.fill(S::SILENCE);
            return;
        };
        let mo = mixer.output_channels();
        if mo == dc
            && let Some(direct) = S::as_f32_mut(body)
        {
            mixer.process_guarded(direct, frames);
            return;
        }
        for chunk in body.chunks_mut(SCRATCH_FRAMES * dc) {
            let n = chunk.len() / dc;
            let Some(scratch) = scratch.get_mut(..n * mo) else {
                chunk.fill(S::SILENCE);
                continue;
            };
            mixer.process_guarded(scratch, n);
            for (dst, src) in chunk.chunks_exact_mut(dc).zip(scratch.chunks_exact(mo)) {
                for (d, x) in dst.iter_mut().zip(src) {
                    *d = S::from_f32(*x);
                }
                for d in dst.iter_mut().skip(mo) {
                    *d = S::SILENCE;
                }
            }
        }
    }

    fn observe_time(&mut self, t: CallbackTime, frames: u64) {
        let latency = t
            .playback_ns
            .checked_sub(t.callback_ns)
            .filter(|ns| *ns != UNKNOWN)
            .unwrap_or(UNKNOWN);
        self.shared
            .device_latency_ns
            .store(latency, Ordering::Relaxed);
        let gap = self.gaps.observe(t.callback_ns, frames, self.rate);
        // A dropout the host already reported is not counted again from its gap.
        let host = self.shared.host_xruns.load(Ordering::Acquire);
        let reported = host != self.seen_host_xruns;
        self.seen_host_xruns = host;
        if gap && !reported {
            self.shared.gap_xruns.fetch_add(1, Ordering::Relaxed);
            self.xruns.note(1);
        }
    }
}

/// `f` under `assert_no_alloc` in debug builds (M3.12); just `f` in release.
fn no_alloc_in_debug<T>(f: impl FnOnce() -> T) -> T {
    #[cfg(debug_assertions)]
    return assert_no_alloc::assert_no_alloc(f);
    #[cfg(not(debug_assertions))]
    return f();
}

impl Drop for OutputHost {
    fn drop(&mut self) {
        if let Some(parts) = self.parts.take()
            && let Err(rtrb::PushError::Full(parts)) = self.slot.push(parts)
        {
            // Cannot happen (one host, one slot). Never free the mixer here: this may be the
            // audio thread.
            std::mem::forget(parts);
            self.shared.leaked_mixers.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mixer::MixerConfig;

    #[test]
    fn integer_conversion_clamps_and_maps_nan_to_zero() {
        assert_eq!(i16::from_f32(0.0), 0);
        assert_eq!(i16::from_f32(1.0), i16::MAX);
        assert_eq!(i16::from_f32(-1.0), i16::MIN);
        assert_eq!(i16::from_f32(7.5), i16::MAX);
        assert_eq!(i16::from_f32(-7.5), i16::MIN);
        assert_eq!(i16::from_f32(f32::NAN), 0);
        assert_eq!(i16::from_f32(f32::INFINITY), i16::MAX);
        assert_eq!(i16::from_f32(f32::NEG_INFINITY), i16::MIN);
        assert_eq!(i16::from_f32(0.5), 16_384);
        assert_eq!(u16::from_f32(0.0), 0x8000);
        assert_eq!(u16::from_f32(1.0), u16::MAX);
        assert_eq!(u16::from_f32(-1.0), 0);
        assert_eq!(u16::from_f32(f32::NAN), 0x8000);
        assert_eq!(i32::from_f32(1.0), i32::MAX);
        assert_eq!(i32::from_f32(-1.0), i32::MIN);
        assert_eq!(i32::from_f32(f32::NAN), 0);
        assert_eq!(i32::from_f32(0.25), 1 << 29);
        assert_eq!(f64::from_f32(0.25), 0.25);
        assert_eq!(f32::from_f32(1.5), 1.5);
    }

    #[test]
    fn gap_detector_threshold_and_rebaseline() {
        let mut g = GapDetector::default();
        // 480 frames at 48 kHz = 10 ms.
        assert!(!g.observe(1_000_000_000, 480, 48_000));
        assert!(!g.observe(1_010_000_000, 480, 48_000));
        assert!(!g.observe(1_024_900_000, 480, 48_000)); // 14.9 ms: jitter
        assert!(g.observe(1_040_100_000, 480, 48_000)); // 15.2 ms: gap
        assert!(!g.observe(1_000_000_000, 480, 48_000)); // clock stepped back
        assert!(!g.observe(1_010_000_000, 480, 48_000));
        assert!(!g.observe(1_010_000_000, 480, 48_000)); // same instant
    }

    #[test]
    fn a_short_callback_then_a_normal_one_is_not_a_gap() {
        let mut g = GapDetector::default();
        assert!(!g.observe(0, 480, 48_000));
        // An early wake-up delivers 64 frames (1.3 ms) ...
        assert!(!g.observe(10_000_000, 64, 48_000));
        // ... and the next full 480-frame callback comes a normal 10 ms later.
        assert!(!g.observe(20_000_000, 480, 48_000));
        // A real gap after the short one is still seen.
        assert!(!g.observe(30_000_000, 64, 48_000));
        assert!(g.observe(60_000_000, 480, 48_000));
    }

    #[test]
    fn errors_that_need_a_reopen() {
        let reopen: Vec<HostError> = [
            HostError::Lost,
            HostError::Invalidated,
            HostError::Rerouted,
            HostError::RealtimeDenied,
            HostError::Xrun,
            HostError::BadBuffer,
            HostError::Other,
        ]
        .into_iter()
        .filter(|e| e.needs_reopen())
        .collect();
        assert_eq!(
            reopen,
            [HostError::Lost, HostError::Invalidated, HostError::Rerouted]
        );
    }

    #[test]
    fn a_panic_in_the_mixer_is_caught_and_counted_as_an_allocation_in_debug() {
        let _lock = crate::alloc_counter_lock();
        let (_ctl, mut mixer) = Mixer::new(MixerConfig::default()).unwrap();
        mixer.panic_next = true;
        let (mut host, link) = OutputHost::new(mixer, 2);
        let mut buf = [1.0f32; 64];
        host.render(&mut buf, None);
        assert!(buf.iter().all(|x| *x == 0.0));
        // The mixer's catch_unwind handled it: not a callback fault.
        assert_eq!(link.monitor().callback_faults(), 0);
        // The panic payload allocates inside the callback: debug builds count it (M3.12).
        if cfg!(debug_assertions) {
            assert!(link.monitor().alloc_violations() > 0);
        }
    }

    #[test]
    fn a_panic_outside_the_mixer_silences_the_buffer_and_counts_a_fault() {
        let _lock = crate::alloc_counter_lock();
        let (ctl, mixer) = Mixer::new(MixerConfig::default()).unwrap();
        let (mut host, mut link) = OutputHost::new(mixer, 3);
        host.panic_next = true;
        let mut buf = [7i16; 30];
        host.render(&mut buf, None);
        assert!(buf.iter().all(|x| *x == 0));
        let m = link.monitor();
        assert_eq!(m.callback_faults(), 1);
        if cfg!(debug_assertions) {
            assert!(m.alloc_violations() > 0);
        }
        // The next callback renders normally and the mixer still comes back.
        host.render(&mut buf, None);
        assert_eq!(m.callback_faults(), 1);
        assert_eq!(ctl.samples_rendered(), 10);
        drop(host);
        assert!(link.take_mixer().is_some());
    }

    #[test]
    fn the_load_meter_keeps_the_worst_until_taken() {
        let m = LoadMeter::new();
        assert_eq!(m.take(), None, "nothing measured yet");
        // 480 frames at 48 kHz last 10 ms.
        m.record(2_000_000, 480, 48_000);
        m.record(5_000_000, 480, 48_000);
        m.record(1_000_000, 480, 48_000);
        assert_eq!(m.take(), Some(0.5));
        assert_eq!(m.take(), None, "taking resets");
        // A measured zero is a load of 0, not "nothing measured".
        m.record(0, 480, 48_000);
        assert_eq!(m.take(), Some(0.0));
        // An overrun shows above 1, and different block sizes compare by their own duration.
        m.record(15_000_000, 480, 48_000);
        m.record(1_000_000, 64, 48_000);
        assert_eq!(m.take(), Some(1.5));
        // No frames or no rate: no duration to compare with, so nothing recorded.
        m.record(1_000, 0, 48_000);
        m.record(1_000, 480, 0);
        assert_eq!(m.take(), None);
        // Extreme inputs saturate instead of wrapping to a small load.
        m.record(u64::MAX, 1, u32::MAX);
        let worst = m.take().unwrap();
        assert!(worst.is_finite() && worst > 1e12, "{worst}");
        m.record(1, u64::MAX, 1);
        assert_eq!(m.take(), Some(0.0));
    }

    #[test]
    fn each_callback_is_timed_against_its_block() {
        let (_ctl, mixer) = Mixer::new(MixerConfig::default()).unwrap();
        let (mut host, link) = OutputHost::new(mixer, 2);
        let m = link.monitor();
        assert_eq!(m.take_engine_load(), None);
        let mut buf = vec![0.0f32; 2 * 256];
        host.render(&mut buf, None);
        host.render(&mut buf, None);
        let load = m.take_engine_load().expect("measured");
        assert!(load.is_finite() && load >= 0.0, "{load}");
        assert_eq!(m.take_engine_load(), None, "no callback since");
        // An empty buffer has no duration: not a measurement.
        host.render::<f32>(&mut [], None);
        assert_eq!(m.take_engine_load(), None);
    }

    #[test]
    fn an_allocation_in_the_timing_is_counted_in_debug() {
        let _lock = crate::alloc_counter_lock();
        let (_ctl, mixer) = Mixer::new(MixerConfig::default()).unwrap();
        let (mut host, link) = OutputHost::new(mixer, 2);
        host.alloc_while_timing = true;
        let mut buf = vec![0.0f32; 2 * 64];
        host.render(&mut buf, None);
        let m = link.monitor();
        if cfg!(debug_assertions) {
            assert!(m.alloc_violations() > 0, "the guard covers the timing");
        }
        assert_eq!(m.callback_faults(), 0);
        assert!(m.take_engine_load().is_some());
    }

    #[test]
    fn a_panic_in_the_timing_is_caught_and_counts_a_fault() {
        let _lock = crate::alloc_counter_lock();
        let (ctl, mixer) = Mixer::new(MixerConfig::default()).unwrap();
        let (mut host, mut link) = OutputHost::new(mixer, 2);
        host.panic_while_timing = true;
        let mut buf = [0.5f32; 20];
        host.render(&mut buf, None);
        assert!(buf.iter().all(|x| *x == 0.0));
        let m = link.monitor();
        assert_eq!(m.callback_faults(), 1);
        // The next callback renders and is timed normally.
        host.render(&mut buf, None);
        assert_eq!(m.callback_faults(), 1);
        assert_eq!(ctl.samples_rendered(), 20);
        assert!(m.take_engine_load().is_some());
        drop(host);
        assert!(link.take_mixer().is_some());
    }

    #[test]
    fn a_clean_render_counts_no_violation() {
        let _lock = crate::alloc_counter_lock();
        let (_ctl, mixer) = Mixer::new(MixerConfig::default()).unwrap();
        let (mut host, link) = OutputHost::new(mixer, 4);
        let mut buf = vec![0i16; 4 * 3000];
        host.render(&mut buf, None);
        assert_eq!(link.monitor().alloc_violations(), 0);
        assert_eq!(link.monitor().callback_faults(), 0);
    }
}
