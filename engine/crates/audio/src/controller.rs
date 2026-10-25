// SPDX-License-Identifier: GPL-3.0-or-later

//! Control-thread handle to the mixer. Owns the producer end of the command queue and the
//! consumer ends of the event and return queues; never touches the audio thread otherwise.
//!
//! Every command goes through a control-side hold queue ordered by `(at, send order)`. [`pump`]
//! releases those due within the lookahead horizon, whole batches at a time, so the mixer only
//! ever holds near-term commands and a far-future schedule cannot delay a stop. The engine's
//! control loop calls [`pump`] on every tick; [`send`] pumps too.
//!
//! [`pump`]: MixerController::pump
//! [`send`]: MixerController::send

use std::cmp::Ordering as CmpOrdering;
use std::collections::BinaryHeap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use rtrb::{Consumer, Producer};

use crate::gain::{Fade, MAX_GAIN_DB, SILENCE_DB, db_to_linear};
use crate::mixer::{
    COMMAND_CAPACITY, Command, CommandKind, Event, GainTarget, SampleTime, Shared, VoiceId,
    VoiceStart,
};

/// A batch larger than the command queue can never be delivered atomically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatchTooLarge;

impl std::fmt::Display for BatchTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "command batch larger than {COMMAND_CAPACITY}")
    }
}

impl std::error::Error for BatchTooLarge {}

/// Commands waiting on the control thread; a batch is released whole or not at all.
struct Held {
    at: SampleTime,
    seq: u64,
    cmds: Vec<Command>,
}

impl Ord for Held {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        (other.at, other.seq).cmp(&(self.at, self.seq))
    }
}

impl PartialOrd for Held {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Held {
    fn eq(&self, other: &Self) -> bool {
        (self.at, self.seq) == (other.at, other.seq)
    }
}

impl Eq for Held {}

pub struct MixerController {
    commands: Producer<Command>,
    events: Consumer<Event>,
    returns: Consumer<Box<VoiceStart>>,
    shared: Arc<Shared>,
    cue_outputs: usize,
    lookahead: u64,
    held: BinaryHeap<Held>,
    held_commands: usize,
    seq: u64,
}

impl MixerController {
    pub(crate) fn new(
        commands: Producer<Command>,
        events: Consumer<Event>,
        returns: Consumer<Box<VoiceStart>>,
        shared: Arc<Shared>,
        cue_outputs: usize,
        lookahead: u64,
    ) -> Self {
        MixerController {
            commands,
            events,
            returns,
            shared,
            cue_outputs,
            lookahead,
            held: BinaryHeap::new(),
            held_commands: 0,
            seq: 0,
        }
    }

    /// Number of cue outputs a [`VoiceStart`] must route (= the mixer's output channels).
    pub fn cue_outputs(&self) -> usize {
        self.cue_outputs
    }

    /// Schedules a command for sample `at`. Never drops it: if it is beyond the lookahead or the
    /// queue is full, it waits on the control thread until a later [`MixerController::pump`].
    pub fn send(&mut self, at: SampleTime, kind: CommandKind) {
        self.hold(at, vec![Command { at, kind }]);
        self.pump();
    }

    /// Schedules commands that must reach the mixer together: they are released in one queue
    /// write, so the mixer sees all of them or none.
    pub fn send_batch(&mut self, cmds: Vec<Command>) -> Result<(), BatchTooLarge> {
        if cmds.len() > COMMAND_CAPACITY {
            return Err(BatchTooLarge);
        }
        if let Some(at) = cmds.iter().map(|c| c.at).min() {
            self.hold(at, cmds);
            self.pump();
        }
        Ok(())
    }

    /// Moves every member of a gang by the same dB offset, keeping their relative levels
    /// (AUD-08). `members` pairs each gain with its current level in dB; members at silence stay
    /// silent and results are capped at [`MAX_GAIN_DB`]. Sent as one batch, so the gang moves on
    /// the same sample.
    pub fn ramp_gang(
        &mut self,
        at: SampleTime,
        voice: VoiceId,
        members: &[(GainTarget, f64)],
        delta_db: f64,
        fade: Fade,
    ) -> Result<(), BatchTooLarge> {
        let cmds = members
            .iter()
            .map(|&(target, db)| {
                let gain = if db <= SILENCE_DB {
                    0.0
                } else {
                    db_to_linear((db + delta_db).min(MAX_GAIN_DB))
                };
                Command {
                    at,
                    kind: CommandKind::Ramp {
                        voice,
                        target,
                        gain,
                        fade,
                    },
                }
            })
            .collect();
        self.send_batch(cmds)
    }

    fn hold(&mut self, at: SampleTime, cmds: Vec<Command>) {
        self.seq += 1;
        self.held_commands += cmds.len();
        self.held.push(Held {
            at,
            seq: self.seq,
            cmds,
        });
    }

    /// Frees returned voice starts, then releases held commands due within the lookahead, in
    /// `(at, send order)`, stopping at the first batch that does not fit. Returns how many
    /// commands were released.
    pub fn pump(&mut self) -> usize {
        self.collect_returns();
        let horizon = self.samples_rendered().saturating_add(self.lookahead);
        let mut released = 0;
        while let Some(n) = self
            .held
            .peek()
            .filter(|h| h.at <= horizon)
            .map(|h| h.cmds.len())
        {
            // One chunk commit publishes the whole batch at once.
            let Ok(chunk) = self.commands.write_chunk_uninit(n) else {
                break;
            };
            let Some(h) = self.held.pop() else { break };
            self.held_commands -= n;
            released += chunk.fill_from_iter(h.cmds);
        }
        released
    }

    /// Commands waiting on the control thread.
    pub fn held(&self) -> usize {
        self.held_commands
    }

    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop().ok()
    }

    /// Frees voice starts (and their sources) the mixer has finished with. Returns how many.
    pub fn collect_returns(&mut self) -> usize {
        let mut n = 0;
        while self.returns.pop().is_ok() {
            n += 1;
        }
        n
    }

    /// The master clock (ARCHITECTURE §3.2): frames rendered so far.
    pub fn samples_rendered(&self) -> SampleTime {
        self.shared.samples_rendered.load(Ordering::Acquire)
    }

    /// Blocks that panicked and were replaced with silence. Non-zero = engine fault.
    pub fn faults(&self) -> u64 {
        self.shared.faults.load(Ordering::Relaxed)
    }

    /// Frames of silence output because a source ring was empty.
    pub fn underrun_frames(&self) -> u64 {
        self.shared.underrun_frames.load(Ordering::Relaxed)
    }

    /// Events lost because the event queue was full.
    pub fn dropped_events(&self) -> u64 {
        self.shared.dropped_events.load(Ordering::Relaxed)
    }

    /// Voice starts leaked because the return queue was full (should stay zero).
    pub fn leaked_starts(&self) -> u64 {
        self.shared.leaked_starts.load(Ordering::Relaxed)
    }

    /// Blocks in which commands had to wait because the mixer's pending set was full.
    pub fn pending_full_blocks(&self) -> u64 {
        self.shared.pending_full_blocks.load(Ordering::Relaxed)
    }
}
