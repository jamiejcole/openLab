// SPDX-License-Identifier: GPL-3.0-or-later

//! The control loop on the offline render driver (ARCHITECTURE §3.6): the same [`Control`] as
//! on a device, stepped one block at a time into memory, faster than real time. Used by the
//! engine's tests (sample-exact starts, fades, feedback) and for offline renders.
//!
//! Rendering faster than real time means the disk workers may lag behind the render, which a
//! device never allows for long. So before each block the driver waits (blocking, with a
//! timeout) until every playing stream has the frames that block will read and every prepared
//! stream is primed. That makes a render exact and repeatable. The control loop itself never
//! waits: only this driver does, outside [`Control::tick`].

use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use openlab_audio::{Mixer, MixerConfig, OfflineRenderer};
use openlab_core::protocol::{ClientMessage, RequestId, ServerMessage, TransportAction};
use openlab_core::{SampleTime, Workspace};

use super::{
    Backend, Control, ControlConfig, ControlError, Flow, Inbox, OUTBOUND_CAPACITY, OutboundRx,
    outbound_channel, reply_channel,
};

/// How long the driver waits for a disk worker or a probe before giving up on a block (and
/// counting it in [`OfflineShow::stalls`]).
pub const SETTLE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct OfflineShow {
    control: Control,
    renderer: OfflineRenderer,
    inbox_tx: Sender<Inbox>,
    inbox: Receiver<Inbox>,
    outbound: OutboundRx,
    block: usize,
    channels: usize,
    capture: Vec<f32>,
    buf: Vec<f32>,
    next_id: u64,
    stalls: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum OfflineError {
    #[error("mixer: {0}")]
    Mixer(openlab_audio::ConfigError),
    #[error(transparent)]
    Control(#[from] ControlError),
}

impl OfflineShow {
    /// A show of `ws` on a fresh mixer built from `mixer`, rendered in blocks of
    /// `config.block_frames` (which must not exceed the mixer's `max_block_frames`).
    pub fn new(
        ws: Workspace,
        mixer: MixerConfig,
        config: ControlConfig,
    ) -> Result<Self, OfflineError> {
        Self::with_outbound_capacity(ws, mixer, config, OUTBOUND_CAPACITY)
    }

    /// As [`Self::new`], with an outbound channel of `capacity` messages (tests of a client
    /// that does not keep up).
    pub fn with_outbound_capacity(
        ws: Workspace,
        mixer: MixerConfig,
        config: ControlConfig,
        capacity: usize,
    ) -> Result<Self, OfflineError> {
        let (ctl, mixer_rt) = Mixer::new(mixer).map_err(OfflineError::Mixer)?;
        let channels = mixer_rt.output_channels();
        let block = usize::try_from(config.block_frames.max(1)).unwrap_or(1);
        let (inbox_tx, inbox) = crossbeam_channel::unbounded();
        let (out_tx, outbound) = outbound_channel(capacity);
        let control = Control::new(ws, ctl, Backend::Offline, config, inbox_tx.clone(), out_tx)?;
        Ok(Self {
            control,
            renderer: OfflineRenderer::new(mixer_rt),
            inbox_tx,
            inbox,
            outbound,
            block,
            channels,
            capture: Vec::new(),
            buf: vec![0.0; block * channels],
            next_id: 0,
            stalls: 0,
        })
    }

    pub fn control(&self) -> &Control {
        &self.control
    }

    /// The scheduling time a message handled now gets (`rendered + one block`).
    pub fn now(&self) -> SampleTime {
        self.rendered() + self.block as u64
    }

    /// Frames rendered so far.
    pub fn rendered(&self) -> SampleTime {
        self.control.mixer().samples_rendered()
    }

    /// Sends a client message and returns the replies to it.
    pub fn send(&mut self, message: ClientMessage) -> Vec<ServerMessage> {
        let (tx, mut rx) = reply_channel();
        self.pump_inbox();
        let _ = self.control.handle(Inbox::Client { message, reply: tx });
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    /// A transport action with a fresh request id.
    pub fn transport(&mut self, action: TransportAction) -> Vec<ServerMessage> {
        let id = self.request_id();
        self.send(ClientMessage::Transport { id, action })
    }

    pub fn request_id(&mut self) -> RequestId {
        self.next_id += 1;
        RequestId(self.next_id)
    }

    /// Hands the probe workers' answers that have arrived to the control loop.
    fn pump_inbox(&mut self) {
        while let Ok(msg) = self.inbox.try_recv() {
            if self.control.handle(msg) == Flow::Shutdown {
                break;
            }
        }
    }

    /// Waits until every probe has answered (or was given up on by the watchdog) and the
    /// answers are in, then ticks. Ticks while it waits, so the probe watchdog runs.
    pub fn settle_media(&mut self) {
        let deadline = std::time::Instant::now() + SETTLE_TIMEOUT;
        while self.control.probes_pending() > 0 {
            if std::time::Instant::now() >= deadline {
                self.stalls += 1;
                break;
            }
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

    /// Renders one block: answers in, streams settled, the block rendered (and captured), then
    /// a control tick.
    pub fn step(&mut self) {
        self.pump_inbox();
        self.settle_streams();
        self.renderer.render_into(&mut self.buf, &[self.block]);
        self.capture.extend_from_slice(&self.buf);
        self.control.tick();
    }

    /// Renders `frames` (whole blocks) with no control tick in between: a control thread that
    /// is late, so mixer events pile up (tests of the event queue overflowing).
    pub fn render_only(&mut self, frames: u64) {
        let until = self.rendered() + frames;
        while self.rendered() < until {
            self.settle_streams();
            self.renderer.render_into(&mut self.buf, &[self.block]);
            self.capture.extend_from_slice(&self.buf);
        }
    }

    /// Renders `frames` (whole blocks, with control ticks) without waiting for the disk: a
    /// stream that has not delivered underruns (tests of a starved voice).
    pub fn run_unsettled(&mut self, frames: u64) {
        let until = self.rendered() + frames;
        while self.rendered() < until {
            self.pump_inbox();
            self.renderer.render_into(&mut self.buf, &[self.block]);
            self.capture.extend_from_slice(&self.buf);
            self.control.tick();
        }
    }

    /// Renders whole blocks until at least `frames` more have been rendered.
    pub fn run(&mut self, frames: u64) {
        let until = self.rendered() + frames;
        while self.rendered() < until {
            self.step();
        }
    }

    /// Renders until sample `at` has been rendered (whole blocks).
    pub fn run_until(&mut self, at: SampleTime) {
        while self.rendered() <= at {
            self.step();
        }
    }

    fn settle_streams(&mut self) {
        let end = self.rendered() + self.block as u64;
        let mut stalls = 0;
        for (handle, needed) in self.control.stream_needs(end) {
            if !matches!(
                handle.wait_written(needed, SETTLE_TIMEOUT),
                Ok(true) | Err(_)
            ) {
                stalls += 1;
            }
        }
        for handle in self.control.prepared_streams() {
            if !matches!(handle.wait_primed(SETTLE_TIMEOUT), Ok(true) | Err(_)) {
                stalls += 1;
            }
        }
        self.stalls += stalls;
    }

    /// Blocks the driver could not settle in time (a test asserts 0).
    pub fn stalls(&self) -> u64 {
        self.stalls
    }

    /// Everything rendered so far, interleaved.
    pub fn audio(&self) -> &[f32] {
        &self.capture
    }

    /// One output channel of the capture.
    pub fn channel(&self, ch: usize) -> Vec<f32> {
        self.capture
            .iter()
            .skip(ch)
            .step_by(self.channels.max(1))
            .copied()
            .collect()
    }

    /// Messages broadcast since the last call.
    pub fn take_outbound(&mut self) -> Vec<ServerMessage> {
        std::iter::from_fn(|| self.outbound.try_recv().ok()).collect()
    }

    /// The inbox, for messages from outside (as the network layer would send them).
    pub fn inbox(&self) -> Sender<Inbox> {
        self.inbox_tx.clone()
    }

    /// Hands a message to the control loop as if it came through the inbox.
    pub fn deliver(&mut self, msg: Inbox) -> Flow {
        self.control.handle(msg)
    }

    pub fn tick(&mut self) {
        self.pump_inbox();
        self.control.tick();
    }

    pub fn control_mut(&mut self) -> &mut Control {
        &mut self.control
    }

    pub fn renderer_mut(&mut self) -> &mut OfflineRenderer {
        &mut self.renderer
    }
}
