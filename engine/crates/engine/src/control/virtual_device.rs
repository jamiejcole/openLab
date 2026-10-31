// SPDX-License-Identifier: GPL-3.0-or-later

//! The virtual output device (`--virtual-device`, built only with the `test-support` feature;
//! docs/control.md "Virtual device"): the real engine with no audio hardware, for the app's
//! tests. It is a [`DeviceLayer`] like cpal's, so the stream is opened, closed, lost and reopened
//! through the same device policy (`devices.rs`) a real interface goes through.
//!
//! - **Stepped**: device time moves only on an `advance { frames }` request. The control thread
//!   waits for the engine's background work, then renders the frames block by block into a
//!   discarded buffer, with a control tick after each, exactly as the offline render driver
//!   does. The device policy's clocks, the engine's epoch clock (log times, autosave, status) and
//!   the random seed follow the frames rendered, so the same requests give the same messages.
//! - **Realtime**: blocks are rendered at the device rate into the discarded buffer, on the
//!   system clock (UI tests).
//!
//! Each open stream has a render thread (`openlab-virtual-audio`) that owns the mixer's
//! [`OutputHost`] and calls [`OutputHost::render`] into a buffer allocated before it starts: the
//! "callback" takes no lock and allocates nothing. It learns what to render from atomics, and
//! parks (stepped) or sleeps (realtime) in between, as a host's own audio thread would.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, RecvTimeoutError};
use openlab_audio::{
    BufferRange, ConfigRange, DeviceError, DeviceMonitor, ErrorReporter, HostError, HostLink,
    Mixer, OpenErrorKind, OpenRequest, OutputHost, SampleFormat,
};
use openlab_core::protocol::{ErrorCode, RequestId};

use super::devices::{
    DeviceLayer, DeviceLister, ListedDevice, MixerBack, OpenFailure, OpenStream, SleepClock,
    StreamInfo, WallClock, system_clock, system_sleep_clock,
};
use super::inbox::reply;
use super::offline::SETTLE_TIMEOUT;
use super::{Control, EpochClock, Flow, Inbox, ReplyTx, fail, ok};

/// The device's id, as the device list gives it.
pub const VIRTUAL_DEVICE_ID: &str = "openlab:virtual";
/// The device's name, as `list_devices` shows it.
pub const VIRTUAL_DEVICE_NAME: &str = "openLab Virtual Output";
pub const VIRTUAL_RATE: u32 = 48_000;
pub const VIRTUAL_CHANNELS: u16 = 2;
/// Where the stepped engine's epoch clock starts: 2026-01-01 00:00:00 UTC (a fixed time, so log
/// times and backup names are the same from run to run).
pub const STEPPED_EPOCH_MS: i64 = 1_767_225_600_000;
/// The random seed (random groups, shuffled playlists) of an engine on a virtual device.
pub const VIRTUAL_SEED: u64 = 0x6f70_656e_4c61_6221;
/// How long the control thread waits for the render thread to render a block or to go silent
/// before the request fails: a hang guard, far beyond the microseconds it takes.
const RENDER_TIMEOUT: Duration = Duration::from_secs(10);

/// How the virtual device keeps time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VirtualMode {
    /// Time moves only on `advance`.
    Stepped,
    /// Time moves at the device rate, on the system clock.
    Realtime,
}

impl VirtualMode {
    /// The `--virtual-device` value.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "stepped" => Some(Self::Stepped),
            "realtime" => Some(Self::Realtime),
            _ => None,
        }
    }
}

/// What the control thread, the device thread and a stream's render thread share. Only atomics,
/// apart from the render thread's handle, which the render thread never touches.
struct Shared {
    mode: VirtualMode,
    /// The device is listed and can be opened.
    present: AtomicBool,
    /// The device went away: the open stream reports it lost once and never calls back again.
    lost: AtomicBool,
    /// A stream is open and calling back (it renders what it is given).
    rendering: AtomicBool,
    /// Frames the control thread has asked the stream to render (stepped), counted since the
    /// device started; `done` follows it.
    granted: AtomicU64,
    done: AtomicU64,
    /// Frames device time has moved (stepped): the virtual clock every injected clock reads.
    elapsed: AtomicU64,
    /// An `advance` is under way (a second one is refused).
    stepping: AtomicBool,
    /// A stepped request is waiting for background work: workers' answers are taken in.
    settling: AtomicBool,
    /// Stepped: workers' answers that came while no request waited for them, in order. They
    /// are taken in at the next wait, so what they change is reported at the same point of a
    /// test's script whenever they arrived.
    held: Mutex<VecDeque<Inbox>>,
    /// The open stream's render thread, to wake it.
    render_thread: Mutex<Option<Thread>>,
}

/// The virtual device: the handle the host builds the engine around and the control loop
/// steps. Cheap to clone.
#[derive(Clone)]
pub struct VirtualDevice {
    shared: Arc<Shared>,
    /// Where the stepped wall clock starts (only differences of it are ever used).
    base: Instant,
    /// The control inbox, which a stepped `advance` reads while it waits for background work
    /// (the control thread is inside the request, so the runner is not reading it).
    inbox: Option<Receiver<Inbox>>,
}

impl std::fmt::Debug for VirtualDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VirtualDevice")
            .field("mode", &self.shared.mode)
            .finish_non_exhaustive()
    }
}

impl VirtualDevice {
    pub fn new(mode: VirtualMode) -> Self {
        Self {
            shared: Arc::new(Shared {
                mode,
                present: AtomicBool::new(true),
                lost: AtomicBool::new(false),
                rendering: AtomicBool::new(false),
                granted: AtomicU64::new(0),
                done: AtomicU64::new(0),
                elapsed: AtomicU64::new(0),
                stepping: AtomicBool::new(false),
                settling: AtomicBool::new(false),
                held: Mutex::new(VecDeque::new()),
                render_thread: Mutex::new(None),
            }),
            base: Instant::now(),
            inbox: None,
        }
    }

    /// The control inbox, for a stepped `advance` to wait on.
    pub fn with_inbox(mut self, inbox: Receiver<Inbox>) -> Self {
        self.inbox = Some(inbox);
        self
    }

    pub fn mode(&self) -> VirtualMode {
        self.shared.mode
    }

    /// The device list as the host would give it: the virtual device, unless it was lost.
    pub fn listed(&self) -> Vec<ListedDevice> {
        list(&self.shared)
    }

    /// The device layer for `devices::start`.
    pub fn layer(&self) -> Box<dyn DeviceLayer> {
        Box::new(VirtualLayer(Arc::clone(&self.shared)))
    }

    /// Device time so far (stepped).
    fn elapsed(&self) -> Duration {
        frames_to_duration(self.shared.elapsed.load(Ordering::Acquire))
    }

    /// The device policy's monotonic clock: device time (stepped) or the system's.
    pub fn wall_clock(&self) -> WallClock {
        match self.shared.mode {
            VirtualMode::Realtime => system_clock(),
            VirtualMode::Stepped => {
                let me = self.clone();
                Arc::new(move || me.base + me.elapsed())
            }
        }
    }

    /// The clock that counts sleep (the device policy's sleep detection): stepped, it moves
    /// with the monotonic one, so a stepped engine never sees a sleep.
    pub fn sleep_clock(&self) -> SleepClock {
        match self.shared.mode {
            VirtualMode::Realtime => system_sleep_clock(),
            VirtualMode::Stepped => {
                let me = self.clone();
                let epoch =
                    SystemTime::UNIX_EPOCH + Duration::from_millis(STEPPED_EPOCH_MS.unsigned_abs());
                Arc::new(move || epoch + me.elapsed())
            }
        }
    }

    /// The engine's epoch clock (`ControlConfig::clock`): stepped, [`STEPPED_EPOCH_MS`] plus
    /// device time.
    pub fn epoch_clock(&self) -> EpochClock {
        match self.shared.mode {
            VirtualMode::Realtime => EpochClock::system(),
            VirtualMode::Stepped => {
                let me = self.clone();
                EpochClock::new(move || {
                    let ms = i64::try_from(me.elapsed().as_millis()).unwrap_or(i64::MAX);
                    STEPPED_EPOCH_MS.saturating_add(ms)
                })
            }
        }
    }

    fn wake(&self) {
        if let Some(t) = crate::lock(&self.shared.render_thread).as_ref() {
            t.unpark();
        }
    }

    /// The device goes away: unlisted, and the open stream reports it lost and goes silent.
    fn lose(&self) {
        self.shared.present.store(false, Ordering::Release);
        self.shared.lost.store(true, Ordering::Release);
        self.wake();
    }

    /// The device is back in the list (a stream lost before stays silent; the policy reopens).
    fn restore(&self) {
        self.shared.lost.store(false, Ordering::Release);
        self.shared.present.store(true, Ordering::Release);
    }

    /// Waits until no stream calls back (the open one has seen the loss).
    fn wait_silent(&self) -> Result<(), String> {
        let deadline = Instant::now() + RENDER_TIMEOUT;
        while self.shared.rendering.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                return Err("the virtual device's stream did not stop".into());
            }
            self.wake();
            thread::yield_now();
        }
        Ok(())
    }

    /// Stepped: the open stream renders `frames` (at most one block), and this returns once it
    /// has. Nothing renders while no stream calls back (the device is down).
    fn render(&self, frames: u64) -> Result<(), String> {
        let s = &self.shared;
        if !s.rendering.load(Ordering::Acquire) {
            return Ok(());
        }
        let target = s.done.load(Ordering::Acquire).saturating_add(frames);
        s.granted.store(target, Ordering::Release);
        self.wake();
        let deadline = Instant::now() + RENDER_TIMEOUT;
        while s.done.load(Ordering::Acquire) < target {
            // Closed or lost meanwhile: the frames are not rendered, as on a device that went.
            if !s.rendering.load(Ordering::Acquire) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("the virtual device's stream did not render".into());
            }
            thread::yield_now();
        }
        Ok(())
    }
}

fn frames_to_duration(frames: u64) -> Duration {
    let rate = u64::from(VIRTUAL_RATE);
    let nanos = u128::from(frames % rate) * 1_000_000_000 / u128::from(rate);
    Duration::from_secs(frames / rate) + Duration::from_nanos(u64::try_from(nanos).unwrap_or(0))
}

fn list(shared: &Shared) -> Vec<ListedDevice> {
    if !shared.present.load(Ordering::Acquire) {
        return Vec::new();
    }
    vec![ListedDevice {
        id: VIRTUAL_DEVICE_ID.into(),
        name: VIRTUAL_DEVICE_NAME.into(),
        is_default: true,
        configs: vec![ConfigRange {
            channels: VIRTUAL_CHANNELS,
            min_rate: VIRTUAL_RATE,
            max_rate: VIRTUAL_RATE,
            buffer: BufferRange::Unknown,
            format: SampleFormat::F32,
        }],
    }]
}

struct VirtualLayer(Arc<Shared>);

impl DeviceLister for VirtualLayer {
    fn list(&mut self) -> Result<Vec<ListedDevice>, DeviceError> {
        Ok(list(&self.0))
    }
}

impl DeviceLayer for VirtualLayer {
    fn lister(&self) -> Box<dyn DeviceLister> {
        Box::new(VirtualLayer(Arc::clone(&self.0)))
    }

    fn open(
        &mut self,
        mixer: Mixer,
        request: &OpenRequest,
    ) -> Result<Box<dyn OpenStream>, OpenFailure> {
        let shared = &self.0;
        let wanted = request
            .device_id
            .as_deref()
            .is_none_or(|id| id == VIRTUAL_DEVICE_ID);
        if !wanted || !shared.present.load(Ordering::Acquire) {
            return Err(OpenFailure {
                error: DeviceError {
                    kind: OpenErrorKind::NotFound,
                    message: "the virtual device is not available".into(),
                },
                mixer: MixerBack::Mixer(Box::new(mixer)),
            });
        }
        let block = request.buffer_frames.unwrap_or(256).clamp(
            1,
            u32::try_from(openlab_audio::MAX_BLOCK_FRAMES).unwrap_or(u32::MAX),
        );
        let outputs = u16::try_from(mixer.output_channels()).unwrap_or(u16::MAX);
        let (host, mut link) = OutputHost::new(mixer, VIRTUAL_CHANNELS);
        let stop = Arc::new(AtomicBool::new(false));
        // Frames asked for before this stream opened (while none was open) are not owed.
        shared
            .done
            .store(shared.granted.load(Ordering::Acquire), Ordering::Release);
        shared.rendering.store(true, Ordering::Release);
        let render = Render {
            shared: Arc::clone(shared),
            stop: Arc::clone(&stop),
            reporter: link.reporter(),
            monitor: link.monitor(),
            block: block as usize,
        };
        let spawned = thread::Builder::new()
            .name("openlab-virtual-audio".into())
            .spawn(move || render.run(host));
        let thread = match spawned {
            Ok(t) => t,
            Err(e) => {
                shared.rendering.store(false, Ordering::Release);
                // The closure, and the host in it, were dropped: the mixer is back.
                let mixer = match link.take_mixer() {
                    Some(m) => MixerBack::Mixer(Box::new(m)),
                    None => MixerBack::Pending(Box::new(link)),
                };
                return Err(OpenFailure {
                    error: DeviceError {
                        kind: OpenErrorKind::Backend,
                        message: format!("could not start the virtual device's thread: {e}"),
                    },
                    mixer,
                });
            }
        };
        *crate::lock(&shared.render_thread) = Some(thread.thread().clone());
        let info = StreamInfo {
            device_id: VIRTUAL_DEVICE_ID.into(),
            name: VIRTUAL_DEVICE_NAME.into(),
            channels: outputs.min(VIRTUAL_CHANNELS),
            outputs_dropped: VIRTUAL_CHANNELS < outputs,
            block_frames: Some(block),
            sample_rate: VIRTUAL_RATE,
            monitor: link.monitor(),
        };
        Ok(Box::new(VirtualStream {
            shared: Arc::clone(shared),
            stop,
            thread: Some(thread),
            link,
            info,
        }))
    }
}

struct VirtualStream {
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<OutputHost>>,
    link: HostLink,
    info: StreamInfo,
}

impl OpenStream for VirtualStream {
    fn info(&self) -> StreamInfo {
        self.info.clone()
    }

    fn close(mut self: Box<Self>) -> MixerBack {
        self.shared.rendering.store(false, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            // Dropping the host (here, or in the render thread's unwinding) hands the mixer back
            // through the link.
            drop(thread.join());
        }
        *crate::lock(&self.shared.render_thread) = None;
        match self.link.take_mixer() {
            Some(m) => MixerBack::Mixer(Box::new(m)),
            None => MixerBack::Pending(Box::new(self.link)),
        }
    }
}

/// A stream's render thread.
struct Render {
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    reporter: ErrorReporter,
    monitor: DeviceMonitor,
    block: usize,
}

impl Render {
    /// Renders until the stream is closed, then hands the host back to be dropped off this
    /// thread. The buffer is allocated here, before the first render; the loop allocates
    /// nothing and takes no lock.
    fn run(self, mut host: OutputHost) -> OutputHost {
        let channels = host.device_channels();
        let mut buf = vec![0.0f32; self.block * channels];
        let period = Duration::from_secs_f64(self.block as f64 / f64::from(VIRTUAL_RATE));
        let mut next = Instant::now();
        let mut reported = false;
        while !self.stop.load(Ordering::Acquire) {
            if reported || self.shared.lost.load(Ordering::Acquire) {
                // Lost: reported once, then silent for good, as a device that went away.
                if !reported {
                    self.reporter.report(HostError::Lost);
                    reported = true;
                    self.shared.rendering.store(false, Ordering::Release);
                }
                self.idle(period);
                continue;
            }
            match self.shared.mode {
                VirtualMode::Stepped => {
                    let done = self.shared.done.load(Ordering::Acquire);
                    let granted = self.shared.granted.load(Ordering::Acquire);
                    if granted <= done {
                        thread::park();
                        continue;
                    }
                    let frames = usize::try_from(granted - done)
                        .unwrap_or(usize::MAX)
                        .min(self.block);
                    if let Some(out) = buf.get_mut(..frames * channels) {
                        host.render(out, None);
                    }
                    // Not a real-time device: its render time is no engine load, and leaving
                    // it would make the `status` messages differ from run to run.
                    let _ = self.monitor.take_engine_load();
                    self.shared
                        .done
                        .store(done + frames as u64, Ordering::Release);
                }
                VirtualMode::Realtime => {
                    host.render(&mut buf, None);
                    next += period;
                    let now = Instant::now();
                    match next.checked_duration_since(now) {
                        Some(wait) => thread::sleep(wait),
                        // Far behind (a stalled machine): start again from now, as a host
                        // would after a dropout, rather than catch up in a burst.
                        None if now.duration_since(next) > period * 8 => next = now,
                        None => {}
                    }
                }
            }
        }
        host
    }

    fn idle(&self, period: Duration) {
        match self.shared.mode {
            VirtualMode::Stepped => thread::park(),
            VirtualMode::Realtime => thread::sleep(period),
        }
    }
}

/// Why a stepped request stopped early.
enum Stop {
    Shutdown,
    Failed(String),
}

impl Control {
    fn virtual_device(&self) -> Option<VirtualDevice> {
        self.eng.config.virtual_device.clone()
    }

    /// `advance { frames }` (stepped virtual device only; module docs).
    pub(super) fn advance(&mut self, id: RequestId, frames: u64, to: &ReplyTx) -> Flow {
        let Some(vd) = self.virtual_device() else {
            fail(
                to,
                id,
                ErrorCode::Unsupported,
                "advance needs an engine started with --virtual-device stepped",
            );
            return Flow::Continue;
        };
        let Some(inbox) = vd
            .inbox
            .clone()
            .filter(|_| vd.mode() == VirtualMode::Stepped)
        else {
            fail(
                to,
                id,
                ErrorCode::Unsupported,
                "advance needs --virtual-device stepped; this engine's device runs in real time",
            );
            return Flow::Continue;
        };
        if vd.shared.stepping.swap(true, Ordering::AcqRel) {
            fail(
                to,
                id,
                ErrorCode::BadRequest,
                "an advance is already under way",
            );
            return Flow::Continue;
        }
        let result = self.step_frames(&vd, &inbox, frames);
        vd.shared.stepping.store(false, Ordering::Release);
        self.stepped_outcome(id, result, to)
    }

    /// `lose_device` (either virtual mode). Stepped, the loss is handled before the answer: the
    /// stream has gone silent, the control loop has seen it and closed the stream.
    pub(super) fn lose_device(&mut self, id: RequestId, to: &ReplyTx) -> Flow {
        let Some(vd) = self.virtual_device() else {
            return not_virtual(id, "lose_device", to);
        };
        vd.lose();
        let result = match (&vd.inbox, vd.mode()) {
            (Some(inbox), VirtualMode::Stepped) => {
                vd.wait_silent().map_err(Stop::Failed).and_then(|()| {
                    self.tick();
                    self.settle(inbox)
                })
            }
            _ => Ok(()),
        };
        self.stepped_outcome(id, result, to)
    }

    /// `restore_device` (either virtual mode): the device is listed again; the device policy
    /// reopens it at its next list read (stepped: once an `advance` passes it).
    pub(super) fn restore_device(&mut self, id: RequestId, to: &ReplyTx) -> Flow {
        let Some(vd) = self.virtual_device() else {
            return not_virtual(id, "restore_device", to);
        };
        vd.restore();
        reply(to, ok(id, None));
        Flow::Continue
    }

    fn stepped_outcome(&self, id: RequestId, result: Result<(), Stop>, to: &ReplyTx) -> Flow {
        match result {
            Ok(()) => {
                reply(to, ok(id, None));
                Flow::Continue
            }
            Err(Stop::Failed(why)) => {
                fail(to, id, ErrorCode::Internal, why);
                Flow::Continue
            }
            Err(Stop::Shutdown) => {
                fail(to, id, ErrorCode::Internal, "the engine is shutting down");
                Flow::Shutdown
            }
        }
    }

    /// Renders `frames` block by block (the offline driver's step: streams settled, the block
    /// rendered, device time moved, a tick, background work settled).
    fn step_frames(
        &mut self,
        vd: &VirtualDevice,
        inbox: &Receiver<Inbox>,
        frames: u64,
    ) -> Result<(), Stop> {
        self.settle(inbox)?;
        let mut left = frames;
        while left > 0 {
            let n = left.min(self.eng.block.max(1));
            if vd.shared.rendering.load(Ordering::Acquire) {
                let end = self.eng.mixer.samples_rendered().saturating_add(n);
                if self.wait_streams(end, SETTLE_TIMEOUT) > 0 {
                    return Err(Stop::Failed(format!(
                        "a disk stream did not deliver within {} s",
                        SETTLE_TIMEOUT.as_secs()
                    )));
                }
                vd.render(n).map_err(Stop::Failed)?;
            }
            vd.shared.elapsed.fetch_add(n, Ordering::AcqRel);
            if self.pump(inbox) == Flow::Shutdown {
                return Err(Stop::Shutdown);
            }
            self.tick();
            self.settle(inbox)?;
            left -= n;
        }
        Ok(())
    }

    fn pump(&mut self, inbox: &Receiver<Inbox>) -> Flow {
        while let Ok(msg) = inbox.try_recv() {
            if self.handle(msg) == Flow::Shutdown {
                return Flow::Shutdown;
            }
        }
        Flow::Continue
    }

    /// Stepped, a worker's answer (a probe, the device thread, a network send, the media
    /// watch) that arrives while no request waits for background work is held for the next
    /// wait ([`Self::settle`]) instead of being handled now; anything else is handed back.
    /// Document answers and waveforms are not held: they answer a client's request.
    pub(super) fn hold_while_stepped(&mut self, msg: Inbox) -> Option<Inbox> {
        let Some(vd) = &self.eng.config.virtual_device else {
            return Some(msg);
        };
        if vd.mode() != VirtualMode::Stepped || vd.shared.settling.load(Ordering::Acquire) {
            return Some(msg);
        }
        match msg {
            Inbox::Probe(..) | Inbox::Device(_) | Inbox::Net(..) | Inbox::Watch(_) => {
                crate::lock(&vd.shared.held).push_back(msg);
                None
            }
            other => Some(other),
        }
    }

    /// Waits until no media probe, preload or waveform read, device job, document read or
    /// save, or network send is outstanding, taking in the held answers first, then what
    /// arrives. Device time stands still meanwhile. The loop ticks only once no worker's
    /// answer is outstanding, so the changes those answers make go out together in one tick
    /// whatever order and timing they came in (a tick also collects saves, and may start
    /// work, a reopen after a list read, which is waited for in turn). A worker silent for
    /// [`QUIET_TICK`] gets a tick anyway (the watchdogs run in ticks).
    fn settle(&mut self, inbox: &Receiver<Inbox>) -> Result<(), Stop> {
        let Some(vd) = self.virtual_device() else {
            return Ok(());
        };
        // A settle inside another (a `lose_device` taken in during an `advance`) leaves the
        // outer one settling.
        let outer = vd.shared.settling.swap(true, Ordering::AcqRel);
        let result = self.settle_answers(&vd, inbox);
        vd.shared.settling.store(outer, Ordering::Release);
        result
    }

    fn settle_answers(&mut self, vd: &VirtualDevice, inbox: &Receiver<Inbox>) -> Result<(), Stop> {
        let deadline = Instant::now() + SETTLE_TIMEOUT;
        let mut quiet = Duration::ZERO;
        loop {
            let held: Vec<Inbox> = crate::lock(&vd.shared.held).drain(..).collect();
            for msg in held {
                if self.handle(msg) == Flow::Shutdown {
                    return Err(Stop::Shutdown);
                }
            }
            if self.pump(inbox) == Flow::Shutdown {
                return Err(Stop::Shutdown);
            }
            if !self.answers_outstanding() || quiet >= QUIET_TICK {
                quiet = Duration::ZERO;
                self.tick();
                if !self.answers_outstanding() && self.waits_for_ticks() == 0 {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                return Err(Stop::Failed(format!(
                    "the engine's background work did not finish within {} s",
                    SETTLE_TIMEOUT.as_secs()
                )));
            }
            match inbox.recv_timeout(SETTLE_POLL) {
                Ok(msg) => {
                    if self.handle(msg) == Flow::Shutdown {
                        return Err(Stop::Shutdown);
                    }
                }
                Err(RecvTimeoutError::Timeout) => quiet += SETTLE_POLL,
                Err(RecvTimeoutError::Disconnected) => return Err(Stop::Shutdown),
            }
        }
    }

    /// Work a worker answers through the inbox, handled as it arrives.
    fn answers_outstanding(&self) -> bool {
        self.probes_pending() > 0
            || self.preloads_pending() > 0
            || self.device_jobs_pending() > 0
            || self.net_busy() > 0
    }

    /// Work the control loop collects in a tick (document reads and saves) or that answers a
    /// request (waveforms).
    fn waits_for_ticks(&self) -> usize {
        self.docs_busy() + self.waveforms_pending()
    }
}

/// How often a settle looks for answers.
const SETTLE_POLL: Duration = Duration::from_millis(2);
/// A settle with answers outstanding ticks after this long without one.
const QUIET_TICK: Duration = Duration::from_millis(50);

fn not_virtual(id: RequestId, what: &str, to: &ReplyTx) -> Flow {
    fail(
        to,
        id,
        ErrorCode::Unsupported,
        format!("{what} needs an engine started with --virtual-device"),
    );
    Flow::Continue
}

#[cfg(test)]
mod tests {
    use super::*;

    use openlab_audio::MixerConfig;
    use openlab_core::protocol::{ClientMessage, Outcome, Request, ServerMessage};

    use crate::control::devices;
    use crate::control::{Backend, ControlConfig, ReplyRx, outbound_channel, reply_channel};

    const BLOCK: u32 = 256;

    /// The control loop on a virtual device, as the host builds it (`host::run_with`).
    struct Rig {
        control: Control,
        vd: VirtualDevice,
        inbox: crossbeam_channel::Sender<Inbox>,
        _outbound: crate::control::OutboundRx,
    }

    fn rig(mode: VirtualMode) -> Rig {
        let vd = VirtualDevice::new(mode);
        let (ctl, mixer) = Mixer::new(MixerConfig {
            sample_rate: VIRTUAL_RATE,
            output_channels: usize::from(VIRTUAL_CHANNELS),
            ..MixerConfig::default()
        })
        .unwrap();
        let (tx, rx) = crossbeam_channel::unbounded();
        let request = OpenRequest {
            device_id: Some(VIRTUAL_DEVICE_ID.into()),
            buffer_frames: Some(BLOCK),
        };
        let device = devices::start(
            vd.layer(),
            mixer,
            request,
            vd.listed(),
            tx.clone(),
            vd.wall_clock(),
        )
        .unwrap()
        .with_sleep_clock(vd.sleep_clock());
        let mut config = ControlConfig::new(VIRTUAL_RATE, u64::from(BLOCK));
        config.clock = vd.epoch_clock();
        config.virtual_device = Some(vd.clone().with_inbox(rx));
        let (out, outbound) = outbound_channel(4096);
        let control = Control::empty(
            ctl,
            Backend::Device(Box::new(device)),
            config,
            tx.clone(),
            out,
        )
        .unwrap();
        Rig {
            control,
            vd,
            inbox: tx,
            _outbound: outbound,
        }
    }

    fn conn(id: u64, request: Request) -> (Inbox, ReplyRx) {
        let (reply, rx) = reply_channel();
        let msg = Inbox::Conn {
            doc: None,
            message: ClientMessage::Request {
                id: RequestId(id),
                request,
            },
            reply,
        };
        (msg, rx)
    }

    /// Sends `request` as an app connection and returns the outcome of its result.
    fn ask(r: &mut Rig, request: Request) -> (Flow, Outcome) {
        let (msg, mut rx) = conn(1, request);
        let flow = r.control.handle(msg);
        (flow, outcome(&mut rx, 1))
    }

    fn outcome(rx: &mut ReplyRx, id: u64) -> Outcome {
        std::iter::from_fn(|| rx.try_recv().ok())
            .find_map(|m| match m {
                ServerMessage::Result { id: i, outcome } if i == RequestId(id) => Some(outcome),
                _ => None,
            })
            .expect("no result")
    }

    fn code(o: &Outcome) -> Option<ErrorCode> {
        match o {
            Outcome::Error(e) => Some(e.code),
            Outcome::Ok(_) => None,
        }
    }

    /// QA-01: device time moves by exactly the frames asked for, rendered block by block, and
    /// every clock the engine is given follows it.
    #[test]
    fn advance_renders_exactly_the_frames_asked_and_moves_every_clock() {
        let mut r = rig(VirtualMode::Stepped);
        let wall = r.vd.wall_clock();
        let start = wall();
        let epoch = r.vd.epoch_clock();
        assert_eq!(epoch.now_ms(), STEPPED_EPOCH_MS);
        let (flow, o) = ask(&mut r, Request::Advance { frames: 0 });
        assert_eq!((flow, code(&o)), (Flow::Continue, None), "{o:?}");
        assert_eq!(r.control.mixer().samples_rendered(), 0, "0 only waits");
        // Not a whole number of blocks: the last one is short.
        let (_, o) = ask(&mut r, Request::Advance { frames: 4_900 });
        assert_eq!(code(&o), None, "{o:?}");
        assert_eq!(r.control.mixer().samples_rendered(), 4_900);
        assert_eq!(wall() - start, frames_to_duration(4_900));
        assert_eq!(epoch.now_ms(), STEPPED_EPOCH_MS + 102);
        assert_eq!(
            (r.vd.sleep_clock())(),
            std::time::UNIX_EPOCH
                + Duration::from_millis(STEPPED_EPOCH_MS.unsigned_abs())
                + frames_to_duration(4_900)
        );
    }

    #[test]
    fn device_time_is_counted_exactly_in_frames() {
        assert_eq!(frames_to_duration(48_000), Duration::from_secs(1));
        assert_eq!(frames_to_duration(48), Duration::from_millis(1));
        assert_eq!(frames_to_duration(1), Duration::from_nanos(20_833));
        assert_eq!(
            frames_to_duration(u64::MAX),
            Duration::from_secs(u64::MAX / 48_000) + frames_to_duration(u64::MAX % 48_000)
        );
    }

    /// A second `advance` that arrives while one runs (on another connection) is refused and
    /// the first goes on; a shutdown that arrives meanwhile ends it and is passed on.
    #[test]
    fn an_advance_during_an_advance_is_refused_and_a_shutdown_ends_it() {
        let mut r = rig(VirtualMode::Stepped);
        let (second, mut second_rx) = conn(2, Request::Advance { frames: 256 });
        r.inbox.send(second).unwrap();
        let (flow, o) = ask(&mut r, Request::Advance { frames: 512 });
        assert_eq!((flow, code(&o)), (Flow::Continue, None), "{o:?}");
        assert_eq!(
            code(&outcome(&mut second_rx, 2)),
            Some(ErrorCode::BadRequest)
        );
        assert_eq!(r.control.mixer().samples_rendered(), 512);

        r.inbox.send(Inbox::Shutdown).unwrap();
        let (flow, o) = ask(&mut r, Request::Advance { frames: 512 });
        assert_eq!(flow, Flow::Shutdown);
        assert_eq!(code(&o), Some(ErrorCode::Internal));
        // And the next advance is not refused as one already under way.
        let (_, o) = ask(&mut r, Request::Advance { frames: 256 });
        assert_eq!(code(&o), None, "{o:?}");
    }

    /// Stepped, a worker's answer that comes between requests is held for the next wait:
    /// device time and the device list do not change behind the test's back.
    #[test]
    fn a_workers_answer_between_requests_waits_for_the_next_advance() {
        let mut r = rig(VirtualMode::Stepped);
        let is_default = |r: &Rig| r.control.eng.device_list().first().map(|d| d.is_default);
        let mut relisted = r.vd.listed();
        relisted[0].is_default = false;
        let flow = r
            .control
            .handle(Inbox::Device(devices::DeviceAnswer::Listed(Ok(relisted))));
        assert_eq!(flow, Flow::Continue);
        assert_eq!(crate::lock(&r.vd.shared.held).len(), 1);
        assert_eq!(is_default(&r), Some(true), "not taken in yet");
        let (_, o) = ask(&mut r, Request::Advance { frames: 0 });
        assert_eq!(code(&o), None, "{o:?}");
        assert!(crate::lock(&r.vd.shared.held).is_empty());
        assert_eq!(is_default(&r), Some(false), "taken in by the advance");
    }

    /// A `lose_device` from another connection, taken in while an `advance` waits, settles
    /// inside it; the advance goes on taking answers in (none is left held) and renders
    /// nothing on the lost device.
    #[test]
    fn a_loss_during_an_advance_is_handled_inside_it() {
        let mut r = rig(VirtualMode::Stepped);
        let (lose, mut lose_rx) = conn(2, Request::LoseDevice);
        r.inbox.send(lose).unwrap();
        let (_, o) = ask(&mut r, Request::Advance { frames: 48_000 });
        assert_eq!(code(&o), None, "{o:?}");
        assert_eq!(code(&outcome(&mut lose_rx, 2)), None);
        assert!(r.control.device_down());
        assert_eq!(r.control.mixer().samples_rendered(), 0);
        // A second of reopen attempts and list reads ran inside the advance: all answered.
        assert!(crate::lock(&r.vd.shared.held).is_empty());
        assert_eq!(r.control.device_jobs_pending(), 0);
        assert!(!r.vd.shared.settling.load(Ordering::Acquire));
    }

    /// `lose_device`, stepped: answered once the loss is handled (the device is down and its
    /// stream closed). Time passes without rendering while it is gone; after
    /// `restore_device`, the next list read finds it and it plays again.
    #[test]
    fn a_lost_device_renders_nothing_until_restored_and_reopened() {
        let mut r = rig(VirtualMode::Stepped);
        let (_, o) = ask(&mut r, Request::Advance { frames: 512 });
        assert_eq!(code(&o), None, "{o:?}");
        let (flow, o) = ask(&mut r, Request::LoseDevice);
        assert_eq!((flow, code(&o)), (Flow::Continue, None), "{o:?}");
        assert!(r.control.device_down());
        assert_eq!(r.control.device_jobs_pending(), 0, "the stream was closed");
        assert!(r.vd.listed().is_empty());
        let (_, o) = ask(&mut r, Request::Advance { frames: 48_000 });
        assert_eq!(code(&o), None, "{o:?}");
        assert_eq!(
            r.control.mixer().samples_rendered(),
            512,
            "nothing rendered"
        );
        assert!(r.control.device_down(), "reopens fail while it is gone");

        let (_, o) = ask(&mut r, Request::RestoreDevice);
        assert_eq!(code(&o), None, "{o:?}");
        assert_eq!(r.vd.listed().len(), 1);
        // Within a list period (1 s) and the backoff, it is open and rendering again.
        let (_, o) = ask(&mut r, Request::Advance { frames: 96_000 });
        assert_eq!(code(&o), None, "{o:?}");
        assert!(!r.control.device_down());
        assert!(r.control.mixer().samples_rendered() > 512);
    }

    /// Realtime: the stream renders on its own, `advance` is refused, and a lost device
    /// reports itself to the policy as a host would.
    #[test]
    fn a_realtime_device_renders_by_itself_and_refuses_advance() {
        let mut r = rig(VirtualMode::Realtime);
        crate::spin::spin_until("the realtime device renders", || {
            r.control.mixer().samples_rendered() > 0
        });
        let (_, o) = ask(&mut r, Request::Advance { frames: 1 });
        assert_eq!(code(&o), Some(ErrorCode::Unsupported));
        let (_, o) = ask(&mut r, Request::LoseDevice);
        assert_eq!(code(&o), None, "{o:?}");
        crate::spin::spin_until("the loss is seen", || {
            r.control.tick();
            r.control.device_down()
        });
    }

    /// Without a virtual device (a show on a real device or offline) the test-only requests
    /// are refused, not acted on.
    #[test]
    fn without_a_virtual_device_the_test_requests_are_unsupported() {
        let mut show = crate::control::tests::memo_show();
        for request in [
            Request::Advance { frames: 1 },
            Request::LoseDevice,
            Request::RestoreDevice,
        ] {
            let replies = show.send_app(ClientMessage::Request {
                id: RequestId(9),
                request: request.clone(),
            });
            let refused = replies.iter().any(|m| {
                matches!(
                    m,
                    ServerMessage::Result {
                        outcome: Outcome::Error(e),
                        ..
                    } if e.code == ErrorCode::Unsupported
                )
            });
            assert!(refused, "{request:?}: {replies:?}");
        }
    }
}
