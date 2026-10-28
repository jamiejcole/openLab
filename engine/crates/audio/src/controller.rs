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
use crate::meter::MeterReader;
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
    meters: MeterReader,
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
            meters: MeterReader::new(Arc::clone(&shared)),
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
        self.hold(vec![(at, kind)]);
        self.pump();
    }

    /// Schedules commands that must reach the mixer together: they are released in one queue
    /// write, so the mixer sees all of them or none. Among equal `at`, every command applies in
    /// send order, whether it was sent alone or in a batch.
    pub fn send_batch(
        &mut self,
        cmds: Vec<(SampleTime, CommandKind)>,
    ) -> Result<(), BatchTooLarge> {
        if cmds.len() > COMMAND_CAPACITY {
            return Err(BatchTooLarge);
        }
        if !cmds.is_empty() {
            self.hold(cmds);
            self.pump();
        }
        Ok(())
    }

    /// Moves every member of a gang by the same dB offset, keeping their relative levels
    /// (AUD-08). `members` pairs each gain with its current level in dB; members at silence (or
    /// with a NaN level or offset) are silent and results are capped at [`MAX_GAIN_DB`]. Sent as
    /// one batch, so the gang moves on the same sample.
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
                let moved = db + delta_db;
                let gain = if db <= SILENCE_DB || moved.is_nan() {
                    0.0
                } else {
                    db_to_linear(moved.min(MAX_GAIN_DB))
                };
                let kind = CommandKind::Ramp {
                    voice,
                    target,
                    gain,
                    fade,
                };
                (at, kind)
            })
            .collect();
        self.send_batch(cmds)
    }

    /// Stamps each command with its send order and holds the batch, keyed by its earliest `at`.
    fn hold(&mut self, cmds: Vec<(SampleTime, CommandKind)>) {
        let first = self.seq + 1;
        let cmds: Vec<Command> = cmds
            .into_iter()
            .map(|(at, kind)| {
                self.seq += 1;
                Command {
                    at,
                    seq: self.seq,
                    kind,
                }
            })
            .collect();
        let at = cmds.iter().map(|c| c.at).min().unwrap_or(0);
        self.held_commands += cmds.len();
        self.held.push(Held {
            at,
            seq: first,
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

    /// Voices dropped because their render faulted [`VOICE_FAULT_LIMIT`] blocks in a row.
    ///
    /// [`VOICE_FAULT_LIMIT`]: crate::VOICE_FAULT_LIMIT
    pub fn faulted_voices(&self) -> u64 {
        self.shared.faulted_voices.load(Ordering::Relaxed)
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

    /// Xruns the device layer has reported so far (NFR-27). Monotonic. Voice underruns are not
    /// xruns; see [`MixerController::underrun_frames`].
    pub fn xruns(&self) -> u64 {
        self.shared.xruns.load(Ordering::Relaxed)
    }

    /// Takes the peak absolute sample of each device output since the last read (AUD-19), as
    /// sent to the device: after patch, output level, mute, polarity, delay and master. Fills
    /// `out[d]` for device output `d` and returns how many it filled; outputs beyond `out` are
    /// not read and keep their peaks. Never blocks the audio thread; allocates nothing.
    pub fn read_output_meters_into(&mut self, out: &mut [f32]) -> usize {
        self.meters.read_outputs(out)
    }

    /// [`MixerController::read_output_meters_into`] for every device output.
    pub fn output_meters(&mut self) -> Vec<f32> {
        let mut out = vec![0.0; self.meters.device_outputs()];
        self.meters.read_outputs(&mut out);
        out
    }

    /// Entries a full voice-meter read can produce: two per voice slot (a voice that ended or
    /// was re-keyed since the last read, and the slot's current voice).
    pub fn voice_meter_capacity(&self) -> usize {
        self.meters.capacity()
    }

    /// Takes each voice's peak per cue output since the last read (AUD-19). The meter point is
    /// the voice's contribution to its patch's cue-output bus: after crosspoints, cue-output
    /// level, voice master, fades, stop fade and pause declick; before the patch matrix.
    ///
    /// Entry `k` is `ids[k]` with peaks `peaks[k * cue_outputs()..(k + 1) * cue_outputs()]`;
    /// returns the number of entries. Every active voice (paused ones included) has an entry. A
    /// voice that ended, or was re-keyed by a resume, since the last read gets one more entry
    /// with its last non-zero peaks under its old id. Peaks are never reported under another
    /// voice's id; they are dropped for a voice this reader never saw (it started and ended
    /// between two reads) and for the earlier voice when its slot changed hands twice between
    /// two reads. Slots whose entries do not fit are left for the next read, which starts at
    /// the first of them; size `ids` to [`MixerController::voice_meter_capacity`] to read all at
    /// once.
    /// Never blocks the audio thread; allocates nothing.
    pub fn read_voice_meters_into(&mut self, ids: &mut [VoiceId], peaks: &mut [f32]) -> usize {
        self.meters.read_voices(ids, peaks)
    }

    /// [`MixerController::read_voice_meters_into`] collected into `(voice, peaks)` pairs.
    pub fn voice_meters(&mut self) -> Vec<(VoiceId, Vec<f32>)> {
        let outs = self.cue_outputs;
        let cap = self.meters.capacity();
        let mut ids = vec![VoiceId(0); cap];
        let mut peaks = vec![0.0; cap * outs];
        let n = self.meters.read_voices(&mut ids, &mut peaks);
        ids.into_iter()
            .take(n)
            .zip(peaks.chunks(outs.max(1)))
            .map(|(id, p)| (id, p.to_vec()))
            .collect()
    }
}
