// SPDX-License-Identifier: GPL-3.0-or-later

//! Peak meters and the xrun counter (AUD-19, NFR-27, ARCHITECTURE §3.1, §3.5).
//!
//! The audio thread keeps each block's peaks in plain `f32`s and publishes them once per
//! `process` call with one `fetch_max` per meter; the control thread takes them with `swap(0)`,
//! so a peak landing between two reads shows in the next one and the audio thread never waits.
//!
//! A peak is a non-negative finite `f32`, so its bit pattern orders like its value. Per-voice
//! peaks are tagged with the slot's epoch in the high 32 bits: the epoch changes whenever the
//! slot gets a new voice id (a start or a re-keying resume), so `fetch_max` lets a newer voice's
//! peak replace an older one's but never mixes the two, and the reader attributes each peak to
//! the id that produced it. When the epoch changes, the audio thread moves the old epoch's
//! unread peaks to a second word (`retired`), so a voice that ends or is re-keyed still has its
//! last peaks read once under its own id even if its slot is reused before the read. The slot's
//! `(id, epoch, active)` is published with a sequence lock that only the reader ever retries.
//!
//! See `docs/audio.md` (Meters and xruns).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};

use crate::mixer::{Shared, VoiceId};

/// Attempts to read a slot's identity while the audio thread is rewriting it; the slot is left
/// for the next read when they all fail.
const IDENT_TRIES: usize = 64;

/// Raises `m` to `|x|`. NaN is ignored (counts as 0.0, like a NaN gain).
#[inline]
pub(crate) fn track(m: &mut f32, x: f32) {
    let a = x.abs();
    if a > *m {
        *m = a;
    }
}

/// A tracked peak as published: ±∞ becomes `f32::MAX`, so the meter is always finite.
#[inline]
fn finite(m: f32) -> f32 {
    m.min(f32::MAX)
}

#[derive(Debug, Default)]
struct Slot {
    seq: AtomicU32,
    id: AtomicU64,
    epoch: AtomicU32,
    active: AtomicBool,
}

/// One voice meter: `epoch << 32 | f32 bits`, 0 = nothing.
#[derive(Debug, Default)]
struct Word {
    /// Peaks of the slot's current epoch.
    live: AtomicU64,
    /// Unread peaks of earlier epochs, moved here when the epoch changed.
    retired: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Ident {
    id: u64,
    epoch: u32,
    active: bool,
}

#[derive(Debug, Default)]
pub(crate) struct Meters {
    /// Device outputs, `f32` bits.
    outputs: Vec<AtomicU32>,
    cue_outputs: usize,
    slots: Vec<Slot>,
    /// `[slot][cue output]`.
    voices: Vec<Word>,
}

impl Meters {
    pub(crate) fn new(device_outputs: usize, slots: usize, cue_outputs: usize) -> Self {
        Meters {
            outputs: (0..device_outputs).map(|_| AtomicU32::new(0)).collect(),
            cue_outputs,
            slots: (0..slots).map(|_| Slot::default()).collect(),
            voices: (0..slots * cue_outputs).map(|_| Word::default()).collect(),
        }
    }

    // ---- audio thread ----

    /// Publishes device-output peaks and zeroes `local`. Skips silent outputs.
    pub(crate) fn flush_outputs(&self, local: &mut [f32]) {
        for (a, m) in self.outputs.iter().zip(local.iter_mut()) {
            if *m > 0.0 {
                a.fetch_max(finite(*m).to_bits(), Ordering::Relaxed);
            }
            *m = 0.0;
        }
    }

    /// Publishes a voice's cue-output peaks under `epoch` and zeroes `local`. Skips silent ones.
    pub(crate) fn flush_voice(&self, slot: usize, epoch: u32, local: &mut [f32]) {
        let tag = u64::from(epoch) << 32;
        let row = self.voices.iter().skip(slot * self.cue_outputs);
        for (w, m) in row.zip(local.iter_mut()) {
            if *m > 0.0 {
                // Release: a reader that sees this tag also sees the identity published for it.
                w.live
                    .fetch_max(tag | u64::from(finite(*m).to_bits()), Ordering::Release);
            }
            *m = 0.0;
        }
    }

    /// Moves the slot's unread peaks out of the way of a new epoch. Call before changing it.
    pub(crate) fn retire(&self, slot: usize) {
        let row = self.voices.iter().skip(slot * self.cue_outputs);
        for w in row.take(self.cue_outputs) {
            let old = w.live.swap(0, Ordering::AcqRel);
            if old != 0 {
                w.retired.fetch_max(old, Ordering::Release);
            }
        }
    }

    /// Publishes who owns `slot`. Single writer (the audio thread); never waits.
    pub(crate) fn assign(&self, slot: usize, id: VoiceId, epoch: u32, active: bool) {
        let Some(s) = self.slots.get(slot) else {
            return;
        };
        let seq = s.seq.load(Ordering::Relaxed);
        s.seq.store(seq.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        s.id.store(id.0, Ordering::Relaxed);
        s.epoch.store(epoch, Ordering::Relaxed);
        s.active.store(active, Ordering::Relaxed);
        s.seq.store(seq.wrapping_add(2), Ordering::Release);
    }

    // ---- control thread ----

    fn ident(&self, slot: usize) -> Option<Ident> {
        let s = self.slots.get(slot)?;
        for _ in 0..IDENT_TRIES {
            let s1 = s.seq.load(Ordering::Acquire);
            if s1 & 1 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let ident = Ident {
                id: s.id.load(Ordering::Relaxed),
                epoch: s.epoch.load(Ordering::Relaxed),
                active: s.active.load(Ordering::Relaxed),
            };
            fence(Ordering::Acquire);
            if s.seq.load(Ordering::Relaxed) == s1 {
                return Some(ident);
            }
        }
        None
    }
}

/// The epoch after `e`; 0 is never used, so a zero meter word means "nothing".
pub(crate) fn next_epoch(e: u32) -> u32 {
    e.wrapping_add(1).max(1)
}

/// Control-side meter state: what each slot held at the last read, so a voice that ended (or was
/// re-keyed) since then still gets its last peak reported under its own id.
pub(crate) struct MeterReader {
    shared: Arc<Shared>,
    seen: Vec<Ident>,
    raw: Vec<[u64; 2]>,
    cur: Vec<f32>,
    prev: Vec<f32>,
}

impl MeterReader {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        let m = &shared.meters;
        let (slots, outs) = (m.slots.len(), m.cue_outputs);
        MeterReader {
            seen: vec![Ident::default(); slots],
            raw: vec![[0; 2]; outs],
            cur: vec![0.0; outs],
            prev: vec![0.0; outs],
            shared,
        }
    }

    pub(crate) fn device_outputs(&self) -> usize {
        self.shared.meters.outputs.len()
    }

    pub(crate) fn capacity(&self) -> usize {
        2 * self.seen.len()
    }

    pub(crate) fn read_outputs(&self, out: &mut [f32]) -> usize {
        let mut n = 0;
        for (a, o) in self.shared.meters.outputs.iter().zip(out.iter_mut()) {
            *o = f32::from_bits(a.swap(0, Ordering::Relaxed));
            n += 1;
        }
        n
    }

    /// See [`crate::MixerController::read_voice_meters_into`].
    pub(crate) fn read_voices(&mut self, ids: &mut [VoiceId], peaks: &mut [f32]) -> usize {
        let MeterReader {
            shared,
            seen,
            raw,
            cur,
            prev,
        } = self;
        let m = &shared.meters;
        let outs = m.cue_outputs;
        if outs == 0 {
            return 0;
        }
        let cap = ids.len().min(peaks.len() / outs);
        let mut n = 0;
        for (slot, last) in seen.iter_mut().enumerate() {
            let row = m.voices.iter().skip(slot * outs).take(outs);
            for (r, w) in raw.iter_mut().zip(row.clone()) {
                *r = [
                    w.live.swap(0, Ordering::Acquire),
                    w.retired.swap(0, Ordering::Acquire),
                ];
            }
            let Some(now) = m.ident(slot) else {
                restore(row, raw);
                continue;
            };
            let (mut any_cur, mut any_prev) = (false, false);
            for ((r, c), p) in raw.iter().zip(cur.iter_mut()).zip(prev.iter_mut()) {
                *c = 0.0;
                *p = 0.0;
                for &x in r.iter().filter(|x| **x != 0) {
                    let (tag, v) = ((x >> 32) as u32, f32::from_bits(x as u32));
                    if tag == now.epoch {
                        *c = c.max(v);
                        any_cur = true;
                    } else if tag == last.epoch {
                        *p = p.max(v);
                        any_prev = true;
                    }
                    // Any other tag belongs to a voice that started and ended between two
                    // reads: the reader never saw its id, so its peak is dropped.
                }
            }
            let want_prev = any_prev && last.epoch != now.epoch;
            let want_cur = now.active || any_cur;
            let need = usize::from(want_prev) + usize::from(want_cur);
            if n + need > cap {
                restore(row, raw);
                break;
            }
            for (want, id, vals) in [(want_prev, last.id, &*prev), (want_cur, now.id, &*cur)] {
                if !want {
                    continue;
                }
                if let (Some(i), Some(dst)) =
                    (ids.get_mut(n), peaks.get_mut(n * outs..(n + 1) * outs))
                {
                    *i = VoiceId(id);
                    dst.copy_from_slice(vals);
                    n += 1;
                }
            }
            *last = now;
        }
        n
    }
}

/// Puts swapped-out peaks back (a newer voice's peak, if one arrived meanwhile, wins).
fn restore<'a>(row: impl Iterator<Item = &'a Word>, raw: &[[u64; 2]]) {
    for (w, [live, retired]) in row.zip(raw) {
        if *live != 0 {
            w.live.fetch_max(*live, Ordering::AcqRel);
        }
        if *retired != 0 {
            w.retired.fetch_max(*retired, Ordering::AcqRel);
        }
    }
}

/// Counts xruns reported by the device layer (NFR-27). Cheap to clone, `Send + Sync`, and
/// real-time safe: one atomic add, so the device's data callback and its error callback (another
/// thread) can both use it.
#[derive(Clone, Debug)]
pub struct XrunNotifier {
    shared: Arc<Shared>,
}

impl XrunNotifier {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        XrunNotifier { shared }
    }

    /// Records `count` xruns (dropouts or discontinuities the host reported).
    pub fn note(&self, count: u64) {
        self.shared.xruns.fetch_add(count, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_ignores_nan_and_publishes_infinity_as_max() {
        let mut m = 0.0;
        track(&mut m, f32::NAN);
        assert_eq!(m, 0.0);
        track(&mut m, -0.5);
        assert_eq!(m, 0.5);
        track(&mut m, f32::NEG_INFINITY);
        assert_eq!(finite(m), f32::MAX);
    }

    #[test]
    fn peak_bits_order_like_values() {
        let xs = [0.0f32, 1e-30, 0.25, 0.5, 1.0, 16.0, f32::MAX];
        for w in xs.windows(2) {
            assert!(w[0].to_bits() < w[1].to_bits());
        }
    }

    #[test]
    fn epochs_skip_zero() {
        assert_eq!(next_epoch(0), 1);
        assert_eq!(next_epoch(u32::MAX), 1);
        assert_eq!(next_epoch(7), 8);
    }

    #[test]
    fn a_slot_mid_rewrite_is_left_for_the_next_read() {
        let shared = Arc::new(Shared {
            meters: Meters::new(1, 1, 1),
            ..Shared::default()
        });
        let m = &shared.meters;
        m.assign(0, VoiceId(5), 1, true);
        m.flush_voice(0, 1, &mut [0.5]);
        let mut r = MeterReader::new(Arc::clone(&shared));
        // Simulate the writer stopped half-way through `assign`.
        let s = &m.slots[0];
        s.seq.fetch_add(1, Ordering::Relaxed);
        let (mut ids, mut peaks) = ([VoiceId(0); 2], [0.0; 2]);
        assert_eq!(r.read_voices(&mut ids, &mut peaks), 0);
        s.seq.fetch_add(1, Ordering::Relaxed);
        assert_eq!(r.read_voices(&mut ids, &mut peaks), 1);
        assert_eq!((ids[0], peaks[0]), (VoiceId(5), 0.5));
    }

    #[test]
    fn entries_that_do_not_fit_wait_for_the_next_read() {
        let shared = Arc::new(Shared {
            meters: Meters::new(1, 2, 1),
            ..Shared::default()
        });
        let m = &shared.meters;
        m.assign(0, VoiceId(1), 1, true);
        m.assign(1, VoiceId(2), 1, true);
        m.flush_voice(0, 1, &mut [0.25]);
        m.flush_voice(1, 1, &mut [0.75]);
        let mut r = MeterReader::new(Arc::clone(&shared));
        let (mut ids, mut peaks) = ([VoiceId(0); 1], [0.0; 1]);
        assert_eq!(r.read_voices(&mut ids, &mut peaks), 1);
        assert_eq!((ids[0], peaks[0]), (VoiceId(1), 0.25));
        let (mut ids, mut peaks) = ([VoiceId(0); 2], [0.0; 2]);
        assert_eq!(r.read_voices(&mut ids, &mut peaks), 2);
        assert_eq!(&ids, &[VoiceId(1), VoiceId(2)]);
        assert_eq!(&peaks, &[0.0, 0.75]);
    }
}
