// SPDX-License-Identifier: GPL-3.0-or-later

//! A stand-in device layer and a test driver for the control loop on a "device" (M4.2b): the
//! mixer sits in an `OutputHost` the test renders block by block, as a device callback would;
//! the stream is opened and closed on the real device thread (`control::devices`), and the
//! device policy reads a wall clock the test moves. Nothing sleeps: the driver waits for the
//! device thread's answers on the control inbox.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use openlab_audio::{
    BufferRange, ConfigRange, DeviceError, ErrorReporter, HostError, HostLink, Mixer,
    OpenErrorKind, OpenRequest, OutputHost, SampleFormat,
};
use openlab_core::Workspace;
use openlab_core::protocol::{ClientMessage, RequestId, ServerMessage, TransportAction};
use openlab_engine::control::devices::{
    self, DeviceLayer, DeviceLister, ListedDevice, MixerBack, OpenFailure, OpenStream, StreamInfo,
};
use openlab_engine::control::{
    Backend, Control, ControlConfig, DocId, DocSource, Flow, Inbox, OutboundRx, outbound_channel,
    reply_channel,
};

use super::{BLOCK, RATE, mixer_config};

/// How long the driver waits for the device thread or a disk worker before failing the test.
const WAIT: Duration = Duration::from_secs(30);

/// A listed output device with `channels` channels at [`RATE`].
pub fn listed(id: &str, name: &str, channels: u16) -> ListedDevice {
    ListedDevice {
        id: id.into(),
        name: name.into(),
        is_default: false,
        configs: vec![ConfigRange {
            channels,
            min_rate: RATE,
            max_rate: RATE,
            buffer: BufferRange::Unknown,
            format: SampleFormat::F32,
        }],
    }
}

#[derive(Default)]
struct State {
    devices: Vec<ListedDevice>,
    host: Option<OutputHost>,
    reporter: Option<ErrorReporter>,
    /// The device delivers callbacks (false once it is lost).
    rendering: bool,
    /// `close` keeps the callback (as CoreAudio does briefly on a loss): the mixer comes back
    /// only once the test lets go of it.
    hold_on_close: bool,
    held: Option<OutputHost>,
    opens: u32,
    /// The device the stream is open on.
    open_id: Option<String>,
    /// Frames per callback of the streams opened from now on.
    block: u32,
    /// The next open panics inside the layer.
    panic_on_open: bool,
    /// The next list read leaves this device out (a host busy reconfiguring it).
    hide_once: Option<String>,
    /// The next stream opens but does not call back (until `unstall`).
    silent_next_open: bool,
}

/// The stand-in host audio system, shared by the layer (device thread) and the test.
pub struct Fake {
    state: Mutex<State>,
    /// Opens may proceed (one condvar for this one mutex: macOS pthreads refuse sharing).
    gate: Mutex<bool>,
    gate_changed: Condvar,
}

impl Fake {
    pub fn new(devices: Vec<ListedDevice>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                devices,
                block: u32::try_from(BLOCK).unwrap(),
                ..State::default()
            }),
            gate: Mutex::new(true),
            gate_changed: Condvar::new(),
        })
    }

    /// Plugs a device in.
    pub fn plug(&self, device: ListedDevice) {
        self.state.lock().unwrap().devices.push(device);
    }

    /// Unplugs a device: it leaves the list. If it is the open one, its callbacks stop and,
    /// with `report`, the host reports it lost (some hosts just stop calling back).
    pub fn unplug(&self, id: &str, report: bool) {
        let mut s = self.state.lock().unwrap();
        s.devices.retain(|d| d.id != id);
        if s.open_id.as_deref() != Some(id) {
            return;
        }
        s.rendering = false;
        if report && let Some(r) = &s.reporter {
            r.report(HostError::Lost);
        }
    }

    /// The open stream stops calling back, with no error and the device still listed (a host
    /// that hangs): the M4.2g stall watchdog's case.
    pub fn stall(&self) {
        self.state.lock().unwrap().rendering = false;
    }

    /// The next stream opened is silent from the start (a host slow to start calling back).
    pub fn silent_next_open(&self) {
        self.state.lock().unwrap().silent_next_open = true;
    }

    /// The stalled stream calls back again.
    pub fn unstall(&self) {
        let mut s = self.state.lock().unwrap();
        s.rendering = s.host.is_some();
    }

    /// A callback with no frames, as a live device keeps making between the blocks the test
    /// renders (the stall watchdog counts callbacks, and a wall clock that moves without
    /// them would look like a hung stream).
    fn heartbeat(&self) {
        let mut s = self.state.lock().unwrap();
        if !s.rendering {
            return;
        }
        if let Some(host) = s.host.as_mut() {
            host.render(&mut [0.0f32; 0], None);
        }
    }

    /// Opens block until [`Fake::open_gate`] (a host that takes long to open a device).
    pub fn close_gate(&self) {
        *self.gate.lock().unwrap() = false;
    }

    pub fn open_gate(&self) {
        *self.gate.lock().unwrap() = true;
        self.gate_changed.notify_all();
    }

    pub fn hold_on_close(&self, hold: bool) {
        self.state.lock().unwrap().hold_on_close = hold;
    }

    /// Lets go of a callback held on close: its mixer goes back to the control side.
    pub fn drop_held(&self) {
        let held = self.state.lock().unwrap().held.take();
        drop(held);
    }

    /// Streams opened from now on call back with `frames` frames.
    pub fn set_block(&self, frames: u32) {
        self.state.lock().unwrap().block = frames;
    }

    /// Frames per callback of the open stream.
    pub fn block(&self) -> u32 {
        self.state.lock().unwrap().block
    }

    pub fn panic_on_open(&self) {
        self.state.lock().unwrap().panic_on_open = true;
    }

    pub fn hide_once(&self, id: &str) {
        self.state.lock().unwrap().hide_once = Some(id.to_owned());
    }

    pub fn opens(&self) -> u32 {
        self.state.lock().unwrap().opens
    }

    pub fn is_rendering(&self) -> bool {
        let s = self.state.lock().unwrap();
        s.rendering && s.host.is_some()
    }

    /// One device callback of `frames` frames, if the device is up.
    fn render(&self, frames: usize) -> Option<(usize, Vec<f32>)> {
        let mut s = self.state.lock().unwrap();
        if !s.rendering {
            return None;
        }
        let host = s.host.as_mut()?;
        let ch = host.device_channels();
        let mut buf = vec![0.0f32; frames * ch];
        host.render(&mut buf, None);
        Some((ch, buf))
    }
}

pub struct FakeLayer(pub Arc<Fake>);

impl DeviceLister for FakeLayer {
    fn list(&mut self) -> Result<Vec<ListedDevice>, DeviceError> {
        let mut s = self.0.state.lock().unwrap();
        let hidden = s.hide_once.take();
        Ok(s.devices
            .iter()
            .filter(|d| Some(&d.id) != hidden.as_ref())
            .cloned()
            .collect())
    }
}

impl DeviceLayer for FakeLayer {
    fn lister(&self) -> Box<dyn DeviceLister> {
        Box::new(FakeLayer(Arc::clone(&self.0)))
    }

    fn open(
        &mut self,
        mixer: Mixer,
        request: &OpenRequest,
    ) -> Result<Box<dyn OpenStream>, OpenFailure> {
        {
            let mut open = self.0.gate.lock().unwrap();
            while !*open {
                open = self.0.gate_changed.wait(open).unwrap();
            }
        }
        let mut s = self.0.state.lock().unwrap();
        s.opens += 1;
        if s.panic_on_open {
            drop(s);
            panic!("the stand-in layer panics during open");
        }
        let found = s
            .devices
            .iter()
            .find(|d| request.device_id.as_deref().is_none_or(|id| id == d.id))
            .cloned();
        let Some(device) = found else {
            return Err(OpenFailure {
                error: DeviceError {
                    kind: OpenErrorKind::NotFound,
                    message: "no such device".into(),
                },
                mixer: MixerBack::Mixer(Box::new(mixer)),
            });
        };
        let outputs = u16::try_from(mixer.output_channels()).unwrap();
        let channels = device.max_channels();
        let (host, link) = OutputHost::new(mixer, channels);
        s.reporter = Some(link.reporter());
        s.host = Some(host);
        s.rendering = !std::mem::take(&mut s.silent_next_open);
        s.open_id = Some(device.id.clone());
        let info = StreamInfo {
            device_id: device.id.clone(),
            name: device.name.clone(),
            channels: outputs.min(channels),
            outputs_dropped: channels < outputs,
            block_frames: Some(s.block),
            sample_rate: RATE,
            monitor: link.monitor(),
        };
        Ok(Box::new(FakeStream {
            fake: Arc::clone(&self.0),
            link,
            info,
        }))
    }
}

struct FakeStream {
    fake: Arc<Fake>,
    link: HostLink,
    info: StreamInfo,
}

impl OpenStream for FakeStream {
    fn info(&self) -> StreamInfo {
        self.info.clone()
    }

    fn close(self: Box<Self>) -> MixerBack {
        let FakeStream { fake, mut link, .. } = *self;
        {
            let mut s = fake.state.lock().unwrap();
            s.rendering = false;
            s.reporter = None;
            s.open_id = None;
            let host = s.host.take();
            if s.hold_on_close {
                s.held = host;
            } else {
                drop(host);
            }
        }
        match link.take_mixer() {
            Some(m) => MixerBack::Mixer(Box::new(m)),
            None => MixerBack::Pending(Box::new(link)),
        }
    }
}

/// The control loop on the stand-in device, stepped by the test.
pub struct DeviceShow {
    control: Control,
    pub fake: Arc<Fake>,
    wall: Arc<Mutex<Duration>>,
    /// Time the Mac slept: the sleep-counting clock has moved on, the monotonic one has not.
    slept: Arc<Mutex<Duration>>,
    inbox: Receiver<Inbox>,
    inbox_tx: Sender<Inbox>,
    outbound: OutboundRx,
    /// Captured audio per device channel.
    capture: Vec<Vec<f32>>,
    next_id: u64,
}

impl DeviceShow {
    /// A show of `ws` playing on the first of `devices`.
    pub fn new(ws: Workspace, devices: Vec<ListedDevice>, config: ControlConfig) -> Self {
        let fake = Fake::new(devices.clone());
        let (ctl, mixer) = Mixer::new(mixer_config()).unwrap();
        let (inbox_tx, inbox) = crossbeam_channel::unbounded();
        let wall = Arc::new(Mutex::new(Duration::ZERO));
        let base = Instant::now();
        let clock = {
            let wall = Arc::clone(&wall);
            Arc::new(move || base + *wall.lock().unwrap())
        };
        // The clock that counts sleep: it follows the wall clock, and `sleep` moves it alone.
        let slept = Arc::new(Mutex::new(Duration::ZERO));
        let sys_base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let sleep_clock = {
            let (wall, slept) = (Arc::clone(&wall), Arc::clone(&slept));
            Arc::new(move || sys_base + *wall.lock().unwrap() + *slept.lock().unwrap())
        };
        let request = OpenRequest {
            device_id: Some(devices[0].id.clone()),
            buffer_frames: Some(u32::try_from(BLOCK).unwrap()),
        };
        let device = devices::start(
            Box::new(FakeLayer(Arc::clone(&fake))),
            mixer,
            request,
            devices,
            inbox_tx.clone(),
            clock,
        )
        .unwrap()
        .with_sleep_clock(sleep_clock);
        let (out_tx, outbound) = outbound_channel(4096);
        let control = Control::new(
            ws,
            ctl,
            Backend::Device(Box::new(device)),
            config,
            inbox_tx.clone(),
            out_tx,
        )
        .unwrap();
        let mut show = Self {
            control,
            fake,
            wall,
            slept,
            inbox,
            inbox_tx,
            outbound,
            capture: Vec::new(),
            next_id: 0,
        };
        show.settle_media();
        show
    }

    pub fn control(&self) -> &Control {
        &self.control
    }

    pub fn control_mut(&mut self) -> &mut Control {
        &mut self.control
    }

    pub fn rendered(&self) -> u64 {
        self.control.mixer().samples_rendered()
    }

    pub fn now(&self) -> u64 {
        self.rendered() + self.control.block_frames()
    }

    /// The Mac sleeps for `d`: the clock that counts sleep moves, the wall clock does not (as
    /// `Instant` on macOS).
    pub fn sleep(&mut self, d: Duration) {
        *self.slept.lock().unwrap() += d;
    }

    /// Moves the wall clock on (the device policy's clock). A live device keeps calling back
    /// while time passes, so a callback is made first (`Fake::heartbeat`): the tests written
    /// before the stall watchdog move the clock by seconds without rendering a block, which
    /// the watchdog would otherwise take for a hung stream. A stalled fake makes none.
    pub fn advance(&mut self, d: Duration) {
        self.fake.heartbeat();
        *self.wall.lock().unwrap() += d;
    }

    fn pump_inbox(&mut self) {
        while let Ok(msg) = self.inbox.try_recv() {
            if self.control.handle(msg) == Flow::Shutdown {
                break;
            }
        }
    }

    fn settle_media(&mut self) {
        let deadline = Instant::now() + WAIT;
        while self.control.probes_pending() > 0 {
            assert!(Instant::now() < deadline, "probes never answered");
            match self.inbox.recv_timeout(Duration::from_millis(20)) {
                Ok(msg) => {
                    let _ = self.control.handle(msg);
                }
                Err(_) => self.control.tick(),
            }
        }
        self.pump_inbox();
        self.control.tick();
    }

    /// Ticks, then waits until the device thread has answered every job (and the answers are
    /// handled), then ticks again so what they started (a reopen) goes out.
    pub fn settle_devices(&mut self) {
        for _ in 0..4 {
            self.tick();
            self.wait_devices();
        }
    }

    /// Waits for the device thread's answers, without ticking.
    pub fn wait_devices(&mut self) {
        while self.control.device_jobs_pending() > 0 {
            match self.inbox.recv_timeout(WAIT) {
                Ok(msg) => {
                    let _ = self.control.handle(msg);
                }
                Err(RecvTimeoutError::Timeout) => panic!("the device thread did not answer"),
                Err(RecvTimeoutError::Disconnected) => panic!("the inbox closed"),
            }
        }
    }

    /// Waits for a list read under way (only: an open or close may be stuck).
    pub fn wait_list(&mut self) {
        while self.control.device_listing() {
            match self.inbox.recv_timeout(WAIT) {
                Ok(msg) => {
                    let _ = self.control.handle(msg);
                }
                Err(_) => panic!("the list thread did not answer"),
            }
        }
    }

    /// Moves the wall clock by `d` in `steps` steps, settling the device after each.
    pub fn wait_wall(&mut self, d: Duration, steps: u32) {
        for _ in 0..steps {
            self.advance(d / steps);
            self.settle_devices();
        }
    }

    pub fn tick(&mut self) {
        self.pump_inbox();
        self.control.tick();
    }

    /// One block: answers in, streams settled, one device callback (if the device is up),
    /// then a control tick.
    pub fn step(&mut self) {
        self.pump_inbox();
        if self.fake.is_rendering() {
            self.settle_streams();
        }
        if let Some((ch, buf)) = self.fake.render(self.fake.block() as usize) {
            if self.capture.len() < ch {
                self.capture.resize(ch, Vec::new());
            }
            for (k, c) in self.capture.iter_mut().enumerate() {
                c.extend(buf.iter().skip(k).step_by(ch));
            }
        }
        self.control.tick();
    }

    /// Steps until `frames` more have been rendered (the device must be up).
    pub fn run(&mut self, frames: u64) {
        let until = self.rendered() + frames;
        while self.rendered() < until {
            assert!(self.fake.is_rendering(), "the device is not playing");
            self.step();
        }
    }

    fn settle_streams(&mut self) {
        let end = self.rendered() + u64::from(self.fake.block());
        for (handle, needed) in self.control.stream_needs(end) {
            assert!(
                matches!(handle.wait_written(needed, WAIT), Ok(true) | Err(_)),
                "a disk worker stalled"
            );
        }
        for handle in self.control.prepared_streams() {
            assert!(
                matches!(handle.wait_primed(WAIT), Ok(true) | Err(_)),
                "a stream was never primed"
            );
        }
    }

    /// Captured audio of device channel `ch`.
    pub fn channel(&self, ch: usize) -> Vec<f32> {
        self.capture.get(ch).cloned().unwrap_or_default()
    }

    pub fn send(&mut self, message: ClientMessage) -> Vec<ServerMessage> {
        let (tx, mut rx) = reply_channel();
        self.pump_inbox();
        let _ = self.control.handle(Inbox::Client { message, reply: tx });
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    pub fn transport(&mut self, action: TransportAction) -> Vec<ServerMessage> {
        let id = self.request_id();
        self.send(ClientMessage::Transport { id, action })
    }

    pub fn request_id(&mut self) -> RequestId {
        self.next_id += 1;
        RequestId(self.next_id)
    }

    /// Opens `ws` as another doc and waits until it is installed and its files probed.
    pub fn open_doc(&mut self, ws: Workspace) -> DocId {
        let (tx, _rx) = reply_channel();
        let id = self.request_id();
        self.control
            .open_doc(DocSource::Workspace(Box::new(ws)), id, &tx);
        let deadline = Instant::now() + WAIT;
        let doc = loop {
            if let Some((_, doc)) = self.control.take_opened().pop() {
                break doc;
            }
            assert!(Instant::now() < deadline, "the doc was never installed");
            match self.inbox.recv_timeout(Duration::from_millis(20)) {
                Ok(msg) => {
                    let _ = self.control.handle(msg);
                }
                Err(_) => self.control.tick(),
            }
        };
        self.settle_media();
        doc
    }

    /// A transport action for doc `doc`.
    pub fn transport_doc(&mut self, doc: DocId, action: TransportAction) -> Vec<ServerMessage> {
        let (tx, mut rx) = reply_channel();
        let id = self.request_id();
        self.pump_inbox();
        let _ = self
            .control
            .handle_doc(doc, ClientMessage::Transport { id, action }, &tx);
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    pub fn take_outbound(&mut self) -> Vec<ServerMessage> {
        std::iter::from_fn(|| self.outbound.try_recv().ok()).collect()
    }

    pub fn inbox(&self) -> Sender<Inbox> {
        self.inbox_tx.clone()
    }
}
