// SPDX-License-Identifier: GPL-3.0-or-later

//! The output device behind the control loop (M4.2b, NFR-08; docs/control.md "Devices"): the
//! device list, loss detection, and close/reopen with backoff.
//!
//! Opening, closing and listing a device can block in the host's audio system (CoreAudio holds
//! locks while a device goes away), and a cpal stream may not leave the thread that opened it.
//! So opening and closing run on one device thread (`openlab-device`), which owns the stream,
//! and listing on another (`openlab-device-list`), so a hung open or close cannot stop the list
//! from being read. Both answer through the control inbox ([`Inbox::Device`]); the control
//! thread never calls the device layer. [`Device`] is the control thread's side: it decides
//! when to list, close and reopen, and watches for jobs that never finish, from a wall clock
//! that tests inject ([`WallClock`]). What the show does when the device goes (stopping the cues
//! that play on it, M4.2g) is the control loop's (`control/device_events.rs`).

use std::collections::VecDeque;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use openlab_audio::{
    DeviceError, DeviceMonitor, HostLink, Mixer, MixerPending, OpenError, OpenErrorKind,
    OpenRequest, OutputStream, StreamErrors, open_output, output_devices, reopen_delay_ms,
};

use super::Inbox;

/// A reopened stream that has played this long resets the reopen backoff; one lost sooner
/// keeps it growing.
pub const STABLE_UP: Duration = Duration::from_secs(5);
/// How often the device list is read (hot-plug; a device the host stops calling back for
/// without an error shows as gone from the list).
pub const DEVICE_POLL: Duration = Duration::from_secs(1);
/// Stall watchdog, first stage (M4.2g): a running stream silent for the longer of this and
/// [`STALL_WARN_PERIODS`] buffer periods raises the `device_stalled` warning. Nothing stops.
pub const STALL_WARN_MIN: Duration = Duration::from_millis(500);
/// Buffer periods of silence for the warning, for big buffers.
pub const STALL_WARN_PERIODS: u32 = 20;
/// Second stage: silence for the longer of this and [`STALL_LOSS_PERIODS`] buffer periods is a
/// device loss (reason `stalled`).
pub const STALL_LOSS_MIN: Duration = Duration::from_secs(3);
/// Buffer periods of silence for the loss, for big buffers.
pub const STALL_LOSS_PERIODS: u32 = 50;
/// Supervisor samples inside the silent window before either stage may fire: only silence the
/// supervisor watched at its normal cadence counts.
pub const STALL_MIN_SAMPLES: u32 = 10;
/// The warning goes only after callbacks have run without a silence of the warning limit for
/// one warning limit (a device that stutters keeps it up).
/// At most one stall warn log per this long, with the number of stalls since.
pub const STALL_LOG_EVERY: Duration = Duration::from_secs(10);
/// Sliding window of watched time over which a mostly silent stream is a loss.
pub const STALL_WINDOW: Duration = Duration::from_secs(10);
/// The window needs this much watched time before it can declare a loss.
pub const STALL_WINDOW_MIN: Duration = Duration::from_secs(8);
/// The least gap between two supervisor samples that is no evidence (the process was frozen,
/// the thread starved); the bound is the longer of this and an eighth of the warning limit.
pub const STALL_MAX_GAP: Duration = Duration::from_millis(100);
/// How much further the system clock (which counts sleep) may move than the monotonic one
/// between two samples before the Mac is taken to have slept.
pub const SLEEP_SLACK: Duration = Duration::from_secs(1);
/// An open or close the device thread has not finished, or a mixer the host library has not
/// handed back, after this long makes the device unrecoverable until openLab restarts (a late
/// answer still counts). Also how long a list read may take before it is reported stuck.
pub const DEVICE_JOB_TIMEOUT: Duration = Duration::from_secs(10);
/// How long [`start`] waits for the first open.
pub const FIRST_OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// An output device as the host lists it.
pub type ListedDevice = openlab_audio::DeviceInfo;

/// The wall clock of the device policy (backoff, list polling, watchdog). Tests inject
/// their own.
pub type WallClock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// The system's monotonic clock.
pub fn system_clock() -> WallClock {
    Arc::new(Instant::now)
}

/// A clock that keeps counting while the Mac sleeps (the monotonic [`WallClock`] stops, on
/// macOS): compared with it between two samples, it shows a sleep. Tests inject their own.
pub type SleepClock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

/// The system clock.
pub fn system_sleep_clock() -> SleepClock {
    Arc::new(SystemTime::now)
}

/// What the device thread uses to reach the host's audio system: cpal ([`CpalDevices`]), or a
/// stand-in in tests. Every call may block.
pub trait DeviceLayer: Send {
    /// Opens `request`'s device for `mixer` and starts it. On failure the mixer comes back in
    /// the [`OpenFailure`].
    fn open(
        &mut self,
        mixer: Mixer,
        request: &OpenRequest,
    ) -> Result<Box<dyn OpenStream>, OpenFailure>;
    /// What reads the device list, on its own thread.
    fn lister(&self) -> Box<dyn DeviceLister>;
}

/// Reads the device list (may block).
pub trait DeviceLister: Send {
    fn list(&mut self) -> Result<Vec<ListedDevice>, DeviceError>;
}

/// A running stream. It stays on the device thread.
pub trait OpenStream {
    fn info(&self) -> StreamInfo;
    /// Stops the stream and hands the mixer back (perhaps later: [`MixerBack::Pending`]).
    fn close(self: Box<Self>) -> MixerBack;
}

/// A mixer the host library has not let go of yet; polled until it comes.
pub trait PendingMixer: Send {
    fn try_take(&mut self) -> Option<Mixer>;
}

impl PendingMixer for MixerPending {
    fn try_take(&mut self) -> Option<Mixer> {
        MixerPending::try_take(self)
    }
}

impl PendingMixer for HostLink {
    fn try_take(&mut self) -> Option<Mixer> {
        self.take_mixer()
    }
}

impl PendingMixer for OpenError {
    fn try_take(&mut self) -> Option<Mixer> {
        self.take_mixer()
    }
}

/// Where the mixer is after a close or a failed open.
pub enum MixerBack {
    Mixer(Box<Mixer>),
    Pending(Box<dyn PendingMixer>),
    /// It cannot come back (the layer panicked holding it; never expected).
    Lost,
}

/// A failed open.
pub struct OpenFailure {
    pub error: DeviceError,
    pub mixer: MixerBack,
}

/// What the control loop needs to know about a running stream.
#[derive(Clone)]
pub struct StreamInfo {
    /// The concrete device id the stream is bound to (reopened by it).
    pub device_id: String,
    pub name: String,
    /// Mixer outputs that reach the device (`Negotiated::mapped`).
    pub channels: u16,
    /// The device has fewer channels than the mixer has outputs.
    pub outputs_dropped: bool,
    /// Frames per callback, if known.
    pub block_frames: Option<u32>,
    /// The stream's sample rate (sizes the stall limit).
    pub sample_rate: u32,
    pub monitor: DeviceMonitor,
}

impl fmt::Debug for StreamInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamInfo")
            .field("device_id", &self.device_id)
            .field("name", &self.name)
            .field("channels", &self.channels)
            .field("outputs_dropped", &self.outputs_dropped)
            .field("block_frames", &self.block_frames)
            .field("sample_rate", &self.sample_rate)
            .finish_non_exhaustive()
    }
}

/// The real device layer: cpal on the platform's default host.
pub struct CpalDevices;

impl DeviceLayer for CpalDevices {
    fn open(
        &mut self,
        mixer: Mixer,
        request: &OpenRequest,
    ) -> Result<Box<dyn OpenStream>, OpenFailure> {
        match open_output(mixer, request) {
            Ok(stream) => Ok(Box::new(CpalStream(stream))),
            Err(mut e) => {
                let error = e.error.clone();
                let mixer = match e.take_mixer() {
                    Some(m) => MixerBack::Mixer(Box::new(m)),
                    None => MixerBack::Pending(Box::new(e)),
                };
                Err(OpenFailure { error, mixer })
            }
        }
    }

    fn lister(&self) -> Box<dyn DeviceLister> {
        Box::new(CpalDevices)
    }
}

impl DeviceLister for CpalDevices {
    fn list(&mut self) -> Result<Vec<ListedDevice>, DeviceError> {
        output_devices()
    }
}

struct CpalStream(OutputStream);

impl OpenStream for CpalStream {
    fn info(&self) -> StreamInfo {
        let s = &self.0;
        StreamInfo {
            device_id: s.device_id().to_owned(),
            name: s.device_name().to_owned(),
            channels: s.negotiated().mapped,
            outputs_dropped: s.outputs_dropped(),
            block_frames: s.latency().buffer_frames,
            sample_rate: s.negotiated().sample_rate,
            monitor: s.monitor(),
        }
    }

    fn close(self: Box<Self>) -> MixerBack {
        match self.0.close() {
            Ok(m) => MixerBack::Mixer(Box::new(m)),
            Err(p) => MixerBack::Pending(Box::new(p)),
        }
    }
}

/// Work for the device thread.
enum Job {
    Open {
        mixer: Box<Mixer>,
        request: OpenRequest,
    },
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JobKind {
    Open,
    Close,
}

impl JobKind {
    fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Close => "close",
        }
    }
}

/// An answer through the control inbox.
pub enum DeviceAnswer {
    Listed(Result<Vec<ListedDevice>, DeviceError>),
    Opened(Result<StreamInfo, OpenFailure>),
    Closed(MixerBack),
}

impl fmt::Debug for DeviceAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Listed(r) => f.debug_tuple("Listed").field(r).finish(),
            Self::Opened(Ok(info)) => f.debug_tuple("Opened").field(info).finish(),
            Self::Opened(Err(e)) => f.debug_tuple("OpenFailed").field(&e.error).finish(),
            Self::Closed(_) => f.write_str("Closed"),
        }
    }
}

/// Why the device could not be started.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("audio device: {0}")]
    Open(DeviceError),
    #[error("audio device: no answer from the device within {} s", .0.as_secs())]
    Timeout(Duration),
    #[error("could not start a device thread: {0}")]
    Thread(std::io::Error),
}

/// Starts the device threads and opens `request`'s device for `mixer` (waiting at most
/// [`FIRST_OPEN_TIMEOUT`]). `listed` is the device list as read before.
pub fn start(
    layer: Box<dyn DeviceLayer>,
    mixer: Mixer,
    request: OpenRequest,
    listed: Vec<ListedDevice>,
    inbox: Sender<Inbox>,
    wall: WallClock,
) -> Result<Device, StartError> {
    start_with_timeout(
        layer,
        mixer,
        request,
        listed,
        inbox,
        wall,
        FIRST_OPEN_TIMEOUT,
    )
}

/// [`start`] with another wait for the first open. A first open that does not answer in time
/// is abandoned (its thread ends when the host returns, dropping what it opened).
pub fn start_with_timeout(
    layer: Box<dyn DeviceLayer>,
    mixer: Mixer,
    request: OpenRequest,
    listed: Vec<ListedDevice>,
    inbox: Sender<Inbox>,
    wall: WallClock,
    timeout: Duration,
) -> Result<Device, StartError> {
    let (jobs_tx, jobs) = crossbeam_channel::unbounded();
    let (lists_tx, lists) = crossbeam_channel::unbounded::<()>();
    let (first_tx, first) = crossbeam_channel::bounded(1);
    let first_request = request.clone();
    let mut lister = layer.lister();
    let list_inbox = inbox.clone();
    thread::Builder::new()
        .name("openlab-device".into())
        .spawn(move || {
            let mut layer = layer;
            let opened = catch_unwind(AssertUnwindSafe(|| layer.open(mixer, &first_request)));
            let stream = match opened {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    let _ = first_tx.send(Err(e.error));
                    return;
                }
                Err(_) => {
                    let _ = first_tx.send(Err(panicked("open")));
                    return;
                }
            };
            if first_tx.send(Ok(stream.info())).is_err() {
                // Given up on: the stream (and the mixer) go here.
                return;
            }
            worker(layer, &jobs, &inbox, Some(stream));
        })
        .map_err(StartError::Thread)?;
    let info = match first.recv_timeout(timeout) {
        Ok(Ok(info)) => info,
        Ok(Err(e)) => return Err(StartError::Open(e)),
        Err(RecvTimeoutError::Timeout) => return Err(StartError::Timeout(timeout)),
        Err(RecvTimeoutError::Disconnected) => {
            return Err(StartError::Open(DeviceError {
                kind: OpenErrorKind::Backend,
                message: "the device thread ended".into(),
            }));
        }
    };
    thread::Builder::new()
        .name("openlab-device-list".into())
        .spawn(move || {
            while lists.recv().is_ok() {
                let listed = catch_unwind(AssertUnwindSafe(|| lister.list()))
                    .unwrap_or_else(|_| Err(panicked("list")));
                if list_inbox
                    .send(Inbox::Device(DeviceAnswer::Listed(listed)))
                    .is_err()
                {
                    break;
                }
            }
        })
        .map_err(StartError::Thread)?;
    let now = wall();
    let sleep_clock = system_sleep_clock();
    // The stream is bound to the concrete device it opened (never the system default).
    let request = OpenRequest {
        device_id: Some(info.device_id.clone()),
        ..request
    };
    Ok(Device {
        jobs: jobs_tx,
        lists: lists_tx,
        wall,
        in_flight: VecDeque::new(),
        listing: None,
        list_stuck: false,
        listed,
        next_list: now + DEVICE_POLL,
        missing_reads: 0,
        callbacks_at_list: info.monitor.callbacks(),
        stall: Stall::new(&info, now, sleep_clock()),
        sleep_clock,
        request,
        stream: Stream::Up,
        info,
        outage: None,
        attempts: 0,
        up_since: now,
        stuck: None,
        alloc_base: 0,
    })
}

fn panicked(what: &str) -> DeviceError {
    DeviceError {
        kind: OpenErrorKind::Backend,
        message: format!("the device layer panicked during {what}"),
    }
}

/// The device thread: carries out jobs in order until the control side goes away, then drops
/// the stream (freeing the mixer here, never on the audio thread). A panic in the layer is
/// caught and answered as a failure (the mixer it held is then lost).
fn worker(
    mut layer: Box<dyn DeviceLayer>,
    jobs: &Receiver<Job>,
    inbox: &Sender<Inbox>,
    mut stream: Option<Box<dyn OpenStream>>,
) {
    while let Ok(job) = jobs.recv() {
        let answer = match job {
            Job::Open { mixer, request } => {
                // The control side closes the stream before it reopens (it has the mixer only
                // then), so no stream is open here.
                match catch_unwind(AssertUnwindSafe(|| layer.open(*mixer, &request))) {
                    Ok(Ok(s)) => {
                        let info = s.info();
                        stream = Some(s);
                        DeviceAnswer::Opened(Ok(info))
                    }
                    Ok(Err(e)) => DeviceAnswer::Opened(Err(e)),
                    Err(_) => DeviceAnswer::Opened(Err(OpenFailure {
                        error: panicked("open"),
                        mixer: MixerBack::Lost,
                    })),
                }
            }
            Job::Close => DeviceAnswer::Closed(match stream.take() {
                Some(s) => catch_unwind(AssertUnwindSafe(|| s.close())).unwrap_or(MixerBack::Lost),
                None => MixerBack::Lost,
            }),
        };
        if inbox.send(Inbox::Device(answer)).is_err() {
            break;
        }
    }
}

/// Where the mixer is, from the control thread's side.
enum Stream {
    /// On the device's audio thread.
    Up,
    /// A close was asked for.
    Closing,
    /// Closed; the host library still holds the callback (since then).
    Pending(Box<dyn PendingMixer>, Instant),
    /// In hand, waiting for the next open attempt.
    Down(Box<Mixer>),
    /// An open was asked for.
    Opening,
    /// It could not be recovered.
    Gone,
}

/// The silence of a stream of `block_frames` frames per callback at `rate` that raises the
/// warning: [`STALL_WARN_MIN`], or [`STALL_WARN_PERIODS`] buffer periods if that is longer.
pub fn stall_limit(block_frames: Option<u32>, rate: u32) -> Duration {
    periods(block_frames, rate, STALL_WARN_PERIODS).max(STALL_WARN_MIN)
}

/// The silence that is a loss: [`STALL_LOSS_MIN`] or [`STALL_LOSS_PERIODS`] buffer periods.
/// It is also the grace a stream gets for its first callback after an open or a wake.
pub fn stall_loss_limit(block_frames: Option<u32>, rate: u32) -> Duration {
    periods(block_frames, rate, STALL_LOSS_PERIODS).max(STALL_LOSS_MIN)
}

fn periods(block_frames: Option<u32>, rate: u32, n: u32) -> Duration {
    match block_frames {
        Some(frames) if frames > 0 && rate > 0 => {
            Duration::from_secs_f64(f64::from(frames) / f64::from(rate)).saturating_mul(n)
        }
        _ => Duration::ZERO,
    }
}

/// The stall watchdog's state for the running stream.
struct Stall {
    warn_limit: Duration,
    loss_limit: Duration,
    callbacks: u64,
    /// When the count last moved, or the silent window was started over.
    since: Instant,
    /// Samples inside the window.
    samples: u32,
    /// The previous supervisor sample, on both clocks.
    sampled: Instant,
    sampled_sys: SystemTime,
    /// No stage fires before this: the first callback after an open or a wake gets one loss
    /// limit. Cleared by the next callback.
    quiet_until: Option<Instant>,
    /// The warning is up.
    warned: bool,
    /// Since when callbacks have run without a silence of `warn_limit` (the warning clears
    /// once that has lasted `warn_limit`).
    steady_since: Option<Instant>,
    /// The raise of the warning that is up was logged (its clearing is logged too).
    warn_logged: bool,
    /// Stall warnings raised since the last one logged.
    unlogged: u32,
    last_warn_log: Option<Instant>,
    window: Window,
}

/// Watched time and watched silence over the last [`STALL_WINDOW`], in one-second buckets.
struct Window {
    start: Instant,
    idx: usize,
    watched: [u64; 10],
    silent: [u64; 10],
}

impl Window {
    fn new(now: Instant) -> Self {
        Window {
            start: now,
            idx: 0,
            watched: [0; 10],
            silent: [0; 10],
        }
    }

    fn add(&mut self, now: Instant, gap: Duration, silent: bool) {
        let bucket = Duration::from_secs(1);
        if now.saturating_duration_since(self.start) >= STALL_WINDOW {
            *self = Window::new(now);
        }
        while now.saturating_duration_since(self.start) >= bucket {
            self.idx = (self.idx + 1) % self.watched.len();
            if let (Some(w), Some(sl)) = (
                self.watched.get_mut(self.idx),
                self.silent.get_mut(self.idx),
            ) {
                *w = 0;
                *sl = 0;
            }
            self.start += bucket;
        }
        let micros = u64::try_from(gap.as_micros()).unwrap_or(u64::MAX);
        if let Some(w) = self.watched.get_mut(self.idx) {
            *w = w.saturating_add(micros);
        }
        if silent && let Some(sl) = self.silent.get_mut(self.idx) {
            *sl = sl.saturating_add(micros);
        }
    }

    /// (watched, silent) over the window.
    fn totals(&self) -> (Duration, Duration) {
        let sum =
            |a: &[u64; 10]| Duration::from_micros(a.iter().fold(0u64, |t, v| t.saturating_add(*v)));
        (sum(&self.watched), sum(&self.silent))
    }
}

impl Stall {
    fn new(info: &StreamInfo, now: Instant, sys: SystemTime) -> Self {
        let loss_limit = stall_loss_limit(info.block_frames, info.sample_rate);
        Stall {
            warn_limit: stall_limit(info.block_frames, info.sample_rate),
            loss_limit,
            callbacks: info.monitor.callbacks(),
            since: now,
            samples: 0,
            sampled: now,
            sampled_sys: sys,
            quiet_until: Some(now + loss_limit),
            warned: false,
            steady_since: None,
            warn_logged: false,
            unlogged: 0,
            last_warn_log: None,
            window: Window::new(now),
        }
    }
}

/// The current outage.
struct Outage {
    next_try: Instant,
}

/// What changed, for the control loop to act on.
#[derive(Debug, PartialEq)]
pub enum DeviceEvent {
    /// The stream stopped playing on its device: an outage starts.
    Lost { why: String },
    /// The device plays again (the stream was reopened).
    Restored,
    /// The stream has not called back for `ms` ms (first stage of the stall watchdog): a
    /// warning, nothing stops.
    /// `log`: the number of earlier stall warnings not logged, when this one is (at most one
    /// log per [`STALL_LOG_EVERY`]).
    Stalled { ms: u64, log: Option<u32> },
    /// The stalled stream calls back again.
    /// `log`: the raise was logged, so is this.
    StallCleared { log: bool },
    /// The device list changed.
    ListChanged,
    /// A reopen attempt failed; the next one is in `retry_in`.
    OpenFailed {
        attempt: u32,
        error: String,
        retry_in: Duration,
    },
    /// Something to log as a warning (host errors that need no reopen, a failed listing).
    Warn(String),
    /// The device thread or the host library stopped answering ([`DEVICE_JOB_TIMEOUT`]), or
    /// the mixer was lost: no sound until openLab restarts.
    Unrecoverable { why: String },
    /// A late answer after [`DeviceEvent::Unrecoverable`] for a stuck job: it can go on.
    Unstuck,
    /// A list read has not answered for [`DEVICE_JOB_TIMEOUT`].
    ListStuck,
}

/// The control thread's side of the device (the `Backend::Device` of the loop).
pub struct Device {
    jobs: Sender<Job>,
    lists: Sender<()>,
    wall: WallClock,
    /// Open and close jobs sent whose answer has not come back, oldest first.
    in_flight: VecDeque<(Instant, JobKind)>,
    /// A list read under way, since.
    listing: Option<Instant>,
    list_stuck: bool,
    listed: Vec<ListedDevice>,
    next_list: Instant,
    /// List reads in a row that did not have the open device.
    missing_reads: u32,
    /// The stream's callback count at the previous list read.
    callbacks_at_list: u64,
    stall: Stall,
    sleep_clock: SleepClock,
    request: OpenRequest,
    stream: Stream,
    /// The open stream, or the last one while it is down.
    info: StreamInfo,
    outage: Option<Outage>,
    /// Opens since the stream last stayed up [`STABLE_UP`]: the backoff step.
    attempts: u32,
    up_since: Instant,
    /// Why the device is unrecoverable (a stuck job or mixer, or a lost mixer).
    stuck: Option<String>,
    /// Allocation violations counted by streams closed since.
    alloc_base: u64,
}

impl Device {
    /// The stream (or the last one, while down).
    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    /// Every output device as last listed.
    pub fn listed(&self) -> &[ListedDevice] {
        &self.listed
    }

    /// The stream is not playing on its device (an outage).
    pub fn is_down(&self) -> bool {
        self.outage.is_some()
    }

    /// Why sound cannot come back without a restart, if it cannot.
    pub fn unrecoverable(&self) -> Option<&str> {
        self.stuck.as_deref()
    }

    /// A list read has not answered for [`DEVICE_JOB_TIMEOUT`].
    pub fn list_stuck(&self) -> bool {
        self.list_stuck
    }

    /// Jobs (open, close, list) not answered yet.
    pub fn jobs_in_flight(&self) -> usize {
        self.in_flight.len() + usize::from(self.listing.is_some())
    }

    /// A list read is under way.
    pub fn is_listing(&self) -> bool {
        self.listing.is_some()
    }

    /// Allocation violations on the audio thread over every stream so far (M3.21).
    pub fn alloc_violations(&self) -> u64 {
        let current = match self.stream {
            Stream::Up => self.info.monitor.alloc_violations(),
            _ => 0,
        };
        self.alloc_base.saturating_add(current)
    }

    /// Each control tick: host errors, the pending mixer, reopen attempts,
    /// the watchdog and the device list.
    pub fn poll(&mut self) -> Vec<DeviceEvent> {
        let now = (self.wall)();
        let mut events = Vec::new();
        if matches!(self.stream, Stream::Up) {
            let errors = self.info.monitor.take_errors();
            if self.info.monitor.needs_reopen() {
                self.lose(now, describe(&errors), &mut events);
            } else {
                if let Some(w) = minor(&errors) {
                    events.push(DeviceEvent::Warn(w));
                }
                self.check_stall(now, &mut events);
            }
            if now.saturating_duration_since(self.up_since) >= STABLE_UP {
                self.attempts = 0;
            }
        }
        if let Stream::Pending(p, _) = &mut self.stream
            && let Some(m) = p.try_take()
        {
            self.stream = Stream::Down(Box::new(m));
        }
        if let Some(o) = &self.outage
            && now >= o.next_try
            && matches!(self.stream, Stream::Down(_))
        {
            self.reopen();
        }
        self.watchdog(now, &mut events);
        if self.listing.is_none() && now >= self.next_list {
            self.next_list = now + DEVICE_POLL;
            if self.lists.send(()).is_ok() {
                self.listing = Some(now);
            }
        }
        events
    }

    /// A device-thread answer.
    pub fn answer(&mut self, answer: DeviceAnswer) -> Vec<DeviceEvent> {
        let now = (self.wall)();
        let mut events = Vec::new();
        match answer {
            DeviceAnswer::Listed(result) => {
                self.listing = None;
                self.list_stuck = false;
                match result {
                    Ok(list) => self.listed_now(list, now, &mut events),
                    Err(e) => events.push(DeviceEvent::Warn(format!(
                        "the output devices could not be listed: {e}"
                    ))),
                }
            }
            DeviceAnswer::Opened(result) => {
                self.answered(JobKind::Open);
                match result {
                    Ok(info) => {
                        self.outage = None;
                        self.callbacks_at_list = info.monitor.callbacks();
                        self.missing_reads = 0;
                        self.info = info;
                        self.arm_stall(now);
                        self.stream = Stream::Up;
                        self.up_since = now;
                        events.push(DeviceEvent::Restored);
                    }
                    Err(failure) => {
                        self.take_back(failure.mixer, now, &mut events);
                        if let Some(o) = &mut self.outage {
                            let retry_in = backoff(self.attempts);
                            o.next_try = now + retry_in;
                            events.push(DeviceEvent::OpenFailed {
                                attempt: self.attempts,
                                error: failure.error.to_string(),
                                retry_in,
                            });
                        }
                    }
                }
            }
            DeviceAnswer::Closed(back) => {
                self.answered(JobKind::Close);
                self.take_back(back, now, &mut events);
            }
        }
        self.watchdog(now, &mut events);
        events
    }

    fn listed_now(&mut self, list: Vec<ListedDevice>, now: Instant, events: &mut Vec<DeviceEvent>) {
        let was_present = self.listed.iter().any(|d| d.id == self.info.device_id);
        if list != self.listed {
            self.listed = list;
            events.push(DeviceEvent::ListChanged);
        }
        let present = self.listed.iter().any(|d| d.id == self.info.device_id);
        match &mut self.outage {
            None if matches!(self.stream, Stream::Up) => {
                let callbacks = self.info.monitor.callbacks();
                let stalled = callbacks == self.callbacks_at_list;
                self.callbacks_at_list = callbacks;
                if present {
                    self.missing_reads = 0;
                    return;
                }
                // Some hosts just stop calling back when a device goes: gone from the list is
                // lost, but one read can miss a device that is busy reconfiguring, so only
                // when it stopped calling back too, or is missing twice in a row.
                self.missing_reads = self.missing_reads.saturating_add(1);
                if stalled || self.missing_reads >= 2 {
                    let why = format!("{} is no longer listed", self.info.name);
                    self.lose(now, why, events);
                }
            }
            // Plugged back in: try now rather than at the end of the backoff.
            Some(o) if present && !was_present => o.next_try = o.next_try.min(now),
            _ => {}
        }
    }

    /// Uses another clock for sleep detection (tests).
    pub fn with_sleep_clock(mut self, clock: SleepClock) -> Self {
        self.stall.sampled_sys = clock();
        self.sleep_clock = clock;
        self
    }

    /// The first stage of the stall watchdog is up: the stream is silent but not yet lost.
    pub fn is_stalled(&self) -> bool {
        self.stall.warned
    }

    /// Starts the stall watchdog over for the stream now up: its limits from the buffer size
    /// (known once it runs), and a grace of one loss limit for the first callback.
    fn arm_stall(&mut self, now: Instant) {
        self.stall = Stall::new(&self.info, now, (self.sleep_clock)());
    }

    /// The stall watchdog (M4.2g), for a stream that is up (not one being closed, reopened or
    /// down). Silence in the callback count, with the device still listed and no host error,
    /// has two stages: a warning at `warn_limit` (cleared when callbacks resume), a loss like
    /// an unplug at `loss_limit`. Only silence the supervisor watched counts: a gap between two
    /// samples above `max(STALL_MAX_GAP, warn_limit / 8)` (the process was frozen, a thread
    /// starved) is no evidence and starts the window over, a stage needs `STALL_MIN_SAMPLES`
    /// samples in the window, and a sleep of the Mac (the system clock moved on further than
    /// the monotonic one) gives a post-wake grace of one loss limit.
    fn check_stall(&mut self, now: Instant, events: &mut Vec<DeviceEvent>) {
        let sys = (self.sleep_clock)();
        let callbacks = self.info.monitor.callbacks();
        let st = &mut self.stall;
        let gap = now.saturating_duration_since(st.sampled);
        let sys_gap = sys.duration_since(st.sampled_sys).unwrap_or(Duration::ZERO);
        st.sampled = now;
        st.sampled_sys = sys;
        let moved = callbacks != st.callbacks;
        // The warning of a stream that has run steadily for one warning limit goes: a device
        // that stutters (a callback every 0.5 s) keeps it up instead of flickering.
        if st.warned
            && let Some(since) = st.steady_since
            && now.saturating_duration_since(since) >= st.warn_limit
            && now.saturating_duration_since(st.since) < st.warn_limit
        {
            st.warned = false;
            st.steady_since = None;
            events.push(DeviceEvent::StallCleared {
                log: std::mem::take(&mut st.warn_logged),
            });
        }
        if moved {
            st.callbacks = callbacks;
            st.since = now;
            st.samples = 0;
            st.quiet_until = None;
            if st.warned && st.steady_since.is_none() {
                st.steady_since = Some(now);
            }
        }
        if sys_gap > gap.saturating_add(SLEEP_SLACK) {
            st.since = now;
            st.samples = 0;
            st.quiet_until = Some(now + st.loss_limit);
            return;
        }
        if gap > STALL_MAX_GAP.max(st.warn_limit / 8) {
            st.since = now;
            st.samples = 0;
            return;
        }
        let quiet = st.quiet_until.is_some_and(|q| now < q);
        if !quiet {
            st.window.add(now, gap, !moved);
            let (watched, silent) = st.window.totals();
            if watched >= STALL_WINDOW_MIN && silent * 2 >= watched {
                let why = format!(
                    "stalled (intermittent): no callback for {:.1} s of the last {:.1} s watched",
                    silent.as_secs_f64(),
                    watched.as_secs_f64()
                );
                self.lose(now, why, events);
                return;
            }
        }
        if moved {
            return;
        }
        st.samples = st.samples.saturating_add(1);
        if quiet || st.samples < STALL_MIN_SAMPLES {
            return;
        }
        let silent = now.saturating_duration_since(st.since);
        let ms = u64::try_from(silent.as_millis()).unwrap_or(u64::MAX);
        if silent >= st.warn_limit {
            st.steady_since = None;
        }
        if silent >= st.loss_limit {
            let why = format!("stalled: the stream has not called back for {ms} ms");
            self.lose(now, why, events);
        } else if silent >= st.warn_limit && !st.warned {
            st.warned = true;
            let due = st
                .last_warn_log
                .is_none_or(|t| now.saturating_duration_since(t) >= STALL_LOG_EVERY);
            let log = if due {
                st.last_warn_log = Some(now);
                st.warn_logged = true;
                Some(std::mem::take(&mut st.unlogged))
            } else {
                st.warn_logged = false;
                st.unlogged = st.unlogged.saturating_add(1);
                None
            };
            events.push(DeviceEvent::Stalled { ms, log });
        }
    }

    fn lose(&mut self, now: Instant, why: String, events: &mut Vec<DeviceEvent>) {
        if self.outage.is_some() {
            return;
        }
        self.alloc_base = self
            .alloc_base
            .saturating_add(self.info.monitor.alloc_violations());
        self.missing_reads = 0;
        self.stall.warned = false;
        self.outage = Some(Outage {
            next_try: now + backoff(self.attempts),
        });
        events.push(DeviceEvent::Lost { why });
        self.stream = if self.send(Job::Close, JobKind::Close, now) {
            Stream::Closing
        } else {
            Stream::Gone
        };
    }

    fn reopen(&mut self) {
        let now = (self.wall)();
        let Stream::Down(mixer) = std::mem::replace(&mut self.stream, Stream::Opening) else {
            return;
        };
        self.attempts = self.attempts.saturating_add(1);
        let job = Job::Open {
            mixer,
            request: self.request.clone(),
        };
        if !self.send(job, JobKind::Open, now) {
            // The device thread is gone, and the mixer went with the job.
            self.stream = Stream::Gone;
        }
    }

    fn take_back(&mut self, back: MixerBack, now: Instant, events: &mut Vec<DeviceEvent>) {
        self.stream = match back {
            MixerBack::Mixer(m) => Stream::Down(m),
            MixerBack::Pending(p) => Stream::Pending(p, now),
            MixerBack::Lost => {
                let why = "the audio engine was lost with the device".to_owned();
                self.stuck = Some(why.clone());
                events.push(DeviceEvent::Unrecoverable { why });
                Stream::Gone
            }
        };
    }

    /// A job that has not finished, or a mixer not handed back, within [`DEVICE_JOB_TIMEOUT`]
    /// makes the device unrecoverable; a late answer that unblocks it lifts that. A stuck list
    /// read is reported once.
    fn watchdog(&mut self, now: Instant, events: &mut Vec<DeviceEvent>) {
        if matches!(self.stream, Stream::Gone) {
            return;
        }
        let late = |since: Instant| now.saturating_duration_since(since) >= DEVICE_JOB_TIMEOUT;
        let why = if let Some(&(since, kind)) = self.in_flight.front()
            && late(since)
        {
            Some(format!(
                "the device thread has not finished a stream {} in {} s",
                kind.name(),
                DEVICE_JOB_TIMEOUT.as_secs()
            ))
        } else if let Stream::Pending(_, since) = self.stream
            && late(since)
        {
            Some(format!(
                "the host has not handed the audio engine back in {} s",
                DEVICE_JOB_TIMEOUT.as_secs()
            ))
        } else {
            None
        };
        match (&self.stuck, why) {
            (None, Some(why)) => {
                self.stuck = Some(why.clone());
                events.push(DeviceEvent::Unrecoverable { why });
            }
            (Some(_), None) => {
                self.stuck = None;
                events.push(DeviceEvent::Unstuck);
            }
            _ => {}
        }
        if !self.list_stuck && self.listing.is_some_and(late) {
            self.list_stuck = true;
            events.push(DeviceEvent::ListStuck);
        }
    }

    fn answered(&mut self, kind: JobKind) {
        if let Some(i) = self.in_flight.iter().position(|&(_, k)| k == kind) {
            self.in_flight.remove(i);
        }
    }

    fn send(&mut self, job: Job, kind: JobKind, now: Instant) -> bool {
        let sent = self.jobs.send(job).is_ok();
        if sent {
            self.in_flight.push_back((now, kind));
        }
        sent
    }
}

/// The delay before reopen attempt `attempts + 1`.
fn backoff(attempts: u32) -> Duration {
    Duration::from_millis(u64::from(reopen_delay_ms(attempts)))
}

/// Why a stream needs reopening, for the log.
fn describe(e: &StreamErrors) -> String {
    let mut parts = Vec::new();
    if e.lost > 0 {
        parts.push("the device went away");
    }
    if e.invalidated > 0 {
        parts.push("the stream was invalidated");
    }
    if e.rerouted > 0 {
        parts.push("the output was rerouted");
    }
    if parts.is_empty() {
        "the host reported an error".to_owned()
    } else {
        parts.join(", ")
    }
}

/// Host errors that need no reopen, for the log.
fn minor(e: &StreamErrors) -> Option<String> {
    let mut parts = Vec::new();
    if e.realtime_denied > 0 {
        parts.push(format!(
            "real-time priority denied ({}x)",
            e.realtime_denied
        ));
    }
    if e.bad_buffer > 0 {
        parts.push(format!("unusable host buffer ({}x)", e.bad_buffer));
    }
    if e.other > 0 {
        parts.push(format!("host error ({}x)", e.other));
    }
    (!parts.is_empty()).then(|| format!("audio device: {}", parts.join(", ")))
}
