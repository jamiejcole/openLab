// SPDX-License-Identifier: GPL-3.0-or-later

//! The output device behind the control loop (M4.2b, NFR-08; docs/control.md "Devices"): the
//! device list, loss detection, and close/reopen with backoff.
//!
//! Opening, closing and listing a device can block in the host's audio system (CoreAudio holds
//! locks while a device goes away), and a cpal stream may not leave the thread that opened it.
//! So all of it runs on one device thread (`openlab-device`), which owns the stream and answers
//! through the control inbox ([`Inbox::Device`]); the control thread never calls the device
//! layer. [`Device`] is the control thread's side: it decides when to list, close and reopen,
//! from a wall clock that tests inject ([`WallClock`]). What the show does during an outage
//! (the grace period, failing the cues) is the control loop's (`control/mod.rs`).

use std::fmt;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use openlab_audio::{
    DeviceError, DeviceMonitor, HostLink, Mixer, MixerPending, OpenError, OpenRequest,
    OutputStream, StreamErrors, open_output, output_devices, reopen_delay_ms,
};

use super::Inbox;

/// How long running cues wait, frozen, for a lost device to come back before the runs playing
/// on it fail (`device_lost`). Wall clock: sample time stands still while no device runs.
pub const DEVICE_LOSS_GRACE: Duration = Duration::from_secs(10);
/// How often the device list is read (hot-plug; a device the host stops calling back for
/// without an error shows as gone from the list).
pub const DEVICE_POLL: Duration = Duration::from_secs(1);

/// An output device as the host lists it.
pub type ListedDevice = openlab_audio::DeviceInfo;

/// The wall clock of the device policy (grace, backoff, list polling). Tests inject their own.
pub type WallClock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// The system's monotonic clock.
pub fn system_clock() -> WallClock {
    Arc::new(Instant::now)
}

/// What the device thread uses to reach the host's audio system: cpal ([`CpalDevices`]), or a
/// stand-in in tests. Every call may block.
pub trait DeviceLayer: Send {
    fn list(&mut self) -> Result<Vec<ListedDevice>, DeviceError>;
    /// Opens `request`'s device for `mixer` and starts it. On failure the mixer comes back in
    /// the [`OpenFailure`].
    fn open(
        &mut self,
        mixer: Mixer,
        request: &OpenRequest,
    ) -> Result<Box<dyn OpenStream>, OpenFailure>;
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
    /// It cannot come back (a host library that never drops the callback; never expected).
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
            .finish_non_exhaustive()
    }
}

/// The real device layer: cpal on the platform's default host.
pub struct CpalDevices;

impl DeviceLayer for CpalDevices {
    fn list(&mut self) -> Result<Vec<ListedDevice>, DeviceError> {
        output_devices()
    }

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
    List,
    Open {
        mixer: Box<Mixer>,
        request: OpenRequest,
    },
    Close,
}

/// The device thread's answer, through the control inbox.
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
    #[error("could not start the device thread: {0}")]
    Thread(std::io::Error),
}

/// Starts the device thread and opens `request`'s device for `mixer` on it (waiting for the
/// answer). `listed` is the device list as read before.
pub fn start(
    layer: Box<dyn DeviceLayer>,
    mixer: Mixer,
    request: OpenRequest,
    listed: Vec<ListedDevice>,
    inbox: Sender<Inbox>,
    wall: WallClock,
) -> Result<Device, StartError> {
    let (jobs_tx, jobs) = crossbeam_channel::unbounded();
    let (first_tx, first) = crossbeam_channel::bounded(1);
    let first_request = request.clone();
    thread::Builder::new()
        .name("openlab-device".into())
        .spawn(move || {
            let mut layer = layer;
            let stream = match layer.open(mixer, &first_request) {
                Ok(s) => {
                    let _ = first_tx.send(Ok(s.info()));
                    s
                }
                Err(e) => {
                    let _ = first_tx.send(Err(e.error));
                    return;
                }
            };
            worker(layer, &jobs, &inbox, Some(stream));
        })
        .map_err(StartError::Thread)?;
    let info = match first.recv() {
        Ok(Ok(info)) => info,
        Ok(Err(e)) => return Err(StartError::Open(e)),
        Err(_) => {
            return Err(StartError::Open(DeviceError {
                kind: openlab_audio::OpenErrorKind::Backend,
                message: "the device thread ended".into(),
            }));
        }
    };
    let now = wall();
    // The stream is bound to the concrete device it opened (never the system default).
    let request = OpenRequest {
        device_id: Some(info.device_id.clone()),
        ..request
    };
    Ok(Device {
        jobs: jobs_tx,
        wall,
        in_flight: 0,
        listing: false,
        listed,
        next_list: now + DEVICE_POLL,
        request,
        stream: Stream::Up,
        info,
        outage: None,
        alloc_base: 0,
    })
}

/// The device thread: carries out jobs in order until the control side goes away, then drops
/// the stream (freeing the mixer here, never on the audio thread).
fn worker(
    mut layer: Box<dyn DeviceLayer>,
    jobs: &Receiver<Job>,
    inbox: &Sender<Inbox>,
    mut stream: Option<Box<dyn OpenStream>>,
) {
    while let Ok(job) = jobs.recv() {
        let answer = match job {
            Job::List => DeviceAnswer::Listed(layer.list()),
            Job::Open { mixer, request } => {
                // The control side closes the stream before it reopens (it has the mixer only
                // then), so no stream is open here.
                match layer.open(*mixer, &request) {
                    Ok(s) => {
                        let info = s.info();
                        stream = Some(s);
                        DeviceAnswer::Opened(Ok(info))
                    }
                    Err(e) => DeviceAnswer::Opened(Err(e)),
                }
            }
            Job::Close => DeviceAnswer::Closed(match stream.take() {
                Some(s) => s.close(),
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
    /// Closed; the host library still holds the callback.
    Pending(Box<dyn PendingMixer>),
    /// In hand, waiting for the next open attempt.
    Down(Box<Mixer>),
    /// An open was asked for.
    Opening,
    /// It could not be recovered.
    Gone,
}

/// The current outage.
struct Outage {
    since: Instant,
    /// Failed open attempts so far.
    attempts: u32,
    next_try: Instant,
    expired: bool,
}

/// What changed, for the control loop to act on.
#[derive(Debug, PartialEq)]
pub enum DeviceEvent {
    /// The stream stopped playing on its device: an outage starts.
    Lost { why: String },
    /// The outage has lasted [`DEVICE_LOSS_GRACE`].
    GraceExpired,
    /// The device plays again (the stream was reopened).
    Restored,
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
    /// The mixer could not be recovered: no sound until the engine restarts.
    MixerGone,
}

/// The control thread's side of the device (the `Backend::Device` of the loop).
pub struct Device {
    jobs: Sender<Job>,
    wall: WallClock,
    /// Jobs sent whose answer has not come back.
    in_flight: usize,
    listing: bool,
    listed: Vec<ListedDevice>,
    next_list: Instant,
    request: OpenRequest,
    stream: Stream,
    /// The open stream, or the last one while it is down.
    info: StreamInfo,
    outage: Option<Outage>,
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

    /// The outage has outlasted the grace period.
    pub fn grace_expired(&self) -> bool {
        self.outage.as_ref().is_some_and(|o| o.expired)
    }

    /// The mixer could not be recovered from the lost device.
    pub fn is_gone(&self) -> bool {
        matches!(self.stream, Stream::Gone)
    }

    /// Jobs the device thread has not answered yet.
    pub fn jobs_in_flight(&self) -> usize {
        self.in_flight
    }

    /// Allocation violations on the audio thread over every stream so far (M3.21).
    pub fn alloc_violations(&self) -> u64 {
        let current = match self.stream {
            Stream::Up => self.info.monitor.alloc_violations(),
            _ => 0,
        };
        self.alloc_base.saturating_add(current)
    }

    /// Each control tick: host errors, the pending mixer, reopen attempts, the grace period
    /// and the device list.
    pub fn poll(&mut self) -> Vec<DeviceEvent> {
        let now = (self.wall)();
        let mut events = Vec::new();
        if matches!(self.stream, Stream::Up) {
            let errors = self.info.monitor.take_errors();
            if self.info.monitor.needs_reopen() {
                self.lose(now, describe(&errors), &mut events);
            } else if let Some(w) = minor(&errors) {
                events.push(DeviceEvent::Warn(w));
            }
        }
        if let Stream::Pending(p) = &mut self.stream
            && let Some(m) = p.try_take()
        {
            self.stream = Stream::Down(Box::new(m));
        }
        if let Some(o) = &mut self.outage {
            if !o.expired && now >= o.since + DEVICE_LOSS_GRACE {
                o.expired = true;
                events.push(DeviceEvent::GraceExpired);
            }
            if now >= o.next_try && matches!(self.stream, Stream::Down(_)) {
                self.reopen();
            }
        }
        if !self.listing && now >= self.next_list {
            self.next_list = now + DEVICE_POLL;
            if self.send(Job::List) {
                self.listing = true;
            }
        }
        events
    }

    /// A device-thread answer.
    pub fn answer(&mut self, answer: DeviceAnswer) -> Vec<DeviceEvent> {
        self.in_flight = self.in_flight.saturating_sub(1);
        let now = (self.wall)();
        let mut events = Vec::new();
        match answer {
            DeviceAnswer::Listed(Ok(list)) => {
                self.listing = false;
                if list != self.listed {
                    self.listed = list;
                    events.push(DeviceEvent::ListChanged);
                }
                let present = self.listed.iter().any(|d| d.id == self.info.device_id);
                match &mut self.outage {
                    // Some hosts just stop calling back when a device goes: gone from the
                    // list is lost.
                    None if !present && matches!(self.stream, Stream::Up) => {
                        let why = format!("{} is no longer listed", self.info.name);
                        self.lose(now, why, &mut events);
                    }
                    // Plugged back in: try now rather than at the end of the backoff.
                    Some(o) if present => o.next_try = o.next_try.min(now),
                    _ => {}
                }
            }
            DeviceAnswer::Listed(Err(e)) => {
                self.listing = false;
                events.push(DeviceEvent::Warn(format!(
                    "the output devices could not be listed: {e}"
                )));
            }
            DeviceAnswer::Opened(Ok(info)) => {
                self.info = info;
                self.stream = Stream::Up;
                self.outage = None;
                events.push(DeviceEvent::Restored);
            }
            DeviceAnswer::Opened(Err(failure)) => {
                self.take_back(failure.mixer, &mut events);
                if let Some(o) = &mut self.outage {
                    o.attempts = o.attempts.saturating_add(1);
                    let retry_in = Duration::from_millis(u64::from(reopen_delay_ms(o.attempts)));
                    o.next_try = now + retry_in;
                    events.push(DeviceEvent::OpenFailed {
                        attempt: o.attempts,
                        error: failure.error.to_string(),
                        retry_in,
                    });
                }
            }
            DeviceAnswer::Closed(back) => self.take_back(back, &mut events),
        }
        events
    }

    fn lose(&mut self, now: Instant, why: String, events: &mut Vec<DeviceEvent>) {
        if self.outage.is_some() {
            return;
        }
        self.alloc_base = self
            .alloc_base
            .saturating_add(self.info.monitor.alloc_violations());
        self.outage = Some(Outage {
            since: now,
            attempts: 0,
            next_try: now + Duration::from_millis(u64::from(reopen_delay_ms(0))),
            expired: false,
        });
        events.push(DeviceEvent::Lost { why });
        self.stream = if self.send(Job::Close) {
            Stream::Closing
        } else {
            Stream::Gone
        };
    }

    fn reopen(&mut self) {
        let Stream::Down(mixer) = std::mem::replace(&mut self.stream, Stream::Opening) else {
            return;
        };
        let job = Job::Open {
            mixer,
            request: self.request.clone(),
        };
        if !self.send(job) {
            // The device thread is gone, and the mixer went with the job.
            self.stream = Stream::Gone;
        }
    }

    fn take_back(&mut self, back: MixerBack, events: &mut Vec<DeviceEvent>) {
        self.stream = match back {
            MixerBack::Mixer(m) => Stream::Down(m),
            MixerBack::Pending(p) => Stream::Pending(p),
            MixerBack::Lost => {
                events.push(DeviceEvent::MixerGone);
                Stream::Gone
            }
        };
    }

    fn send(&mut self, job: Job) -> bool {
        let sent = self.jobs.send(job).is_ok();
        if sent {
            self.in_flight += 1;
        }
        sent
    }
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
