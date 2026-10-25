// SPDX-License-Identifier: GPL-3.0-or-later

//! Control-thread handle to the mixer. Owns the producer ends of the command queue and the
//! consumer ends of the event and return queues; never touches the audio thread otherwise.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use rtrb::{Consumer, Producer};

use crate::gain::{Fade, SILENCE_DB, db_to_linear};
use crate::mixer::{Command, CommandKind, Event, GainTarget, SampleTime, Shared, VoiceId};

/// The command queue is full; the command was dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueFull;

impl std::fmt::Display for QueueFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("mixer command queue full")
    }
}

impl std::error::Error for QueueFull {}

pub struct MixerController {
    commands: Producer<Command>,
    events: Consumer<Event>,
    returns: Consumer<Consumer<f32>>,
    shared: Arc<Shared>,
}

impl MixerController {
    pub(crate) fn new(
        commands: Producer<Command>,
        events: Consumer<Event>,
        returns: Consumer<Consumer<f32>>,
        shared: Arc<Shared>,
    ) -> Self {
        MixerController {
            commands,
            events,
            returns,
            shared,
        }
    }

    /// Queues a command for sample `at`. Drains the return queue first, which keeps it within
    /// the capacity the mixer relies on.
    pub fn send(&mut self, at: SampleTime, kind: CommandKind) -> Result<(), QueueFull> {
        self.collect_returns();
        self.commands
            .push(Command { at, kind })
            .map_err(|_| QueueFull)
    }

    /// Moves every member of a gang by the same dB offset, keeping their relative levels
    /// (AUD-08). `members` pairs each gain with its current level in dB; members at silence stay
    /// silent. All ramps are queued or none are, so the gang moves on the same sample.
    pub fn ramp_gang(
        &mut self,
        at: SampleTime,
        voice: VoiceId,
        members: &[(GainTarget, f64)],
        delta_db: f64,
        fade: Fade,
    ) -> Result<(), QueueFull> {
        self.collect_returns();
        if self.commands.slots() < members.len() {
            return Err(QueueFull);
        }
        for &(target, db) in members {
            let gain = if db <= SILENCE_DB {
                0.0
            } else {
                db_to_linear(db + delta_db)
            };
            self.commands
                .push(Command {
                    at,
                    kind: CommandKind::Ramp {
                        voice,
                        target,
                        gain,
                        fade,
                    },
                })
                .map_err(|_| QueueFull)?;
        }
        Ok(())
    }

    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop().ok()
    }

    /// Frees sources the mixer has finished with. Returns how many.
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

    /// Sources leaked because the return queue was full (should stay zero).
    pub fn leaked_sources(&self) -> u64 {
        self.shared.leaked_sources.load(Ordering::Relaxed)
    }
}
