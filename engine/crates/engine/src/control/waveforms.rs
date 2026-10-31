// SPDX-License-Identifier: GPL-3.0-or-later

//! `waveform` requests (AUD-14): an Audio cue's file waveform for the inspector. The peaks are
//! read once per file on a probe worker (the decode takes as long as a probe, so it never runs
//! on the control thread, ARCHITECTURE §3.1), kept per doc within [`CACHE_BYTES`], and every
//! later request, at any zoom, is cut from the kept pyramid at once. A read stuck on a disk that
//! stops answering is given up on by the pool's watchdog like a probe: its requests fail with
//! `io` and the worker is replaced. Everything here is bounded: requests waiting, files read at
//! once, the size of a waveform read and of an answer.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use openlab_audio::{Waveform, WaveformError};
use openlab_core::protocol::{
    ErrorCode, OkBody, Outcome, RequestId, ResultData, ServerMessage, WaveformChannel, WaveformData,
};
use openlab_core::{CueId, CueKind};

use super::inbox::{ReplyTx, reply};
use super::media::Ready;
use super::{DocCx, fail};

/// Peak memory a doc keeps for waveforms already read; the least recently used go first.
pub const CACHE_BYTES: usize = 64 * 1024 * 1024;
/// Largest waveform read at all (a very long file of many channels): refused before reading.
pub const MAX_WAVEFORM_BYTES: u64 = 256 * 1024 * 1024;
/// Files whose waveform is being read for one doc at once.
pub const MAX_READS: usize = 4;
/// Requests waiting for reads, per doc.
pub const MAX_WAITERS: usize = 64;
/// Most peaks (buckets × channels) in one answer: the bucket count is cut to fit.
pub const MAX_ANSWER_PEAKS: usize = 1 << 20;

/// A request waiting for its file's waveform.
struct Waiter {
    id: RequestId,
    to: ReplyTx,
    cue: CueId,
    start: f64,
    end: f64,
    buckets: u32,
}

/// A read under way.
struct Reading {
    key: String,
    ticket: u64,
    /// The probe it reads: a re-probe since (the file changed) makes its answer stale.
    ready: Arc<Ready>,
    cancel: Arc<AtomicBool>,
    waiters: Vec<Waiter>,
}

struct Kept {
    key: String,
    ready: Arc<Ready>,
    wave: Arc<Waveform>,
}

/// One doc's waveforms: kept ones (most recently used last) and reads under way.
#[derive(Default)]
pub(super) struct Waveforms {
    kept: Vec<Kept>,
    kept_bytes: usize,
    reading: Vec<Reading>,
}

impl Waveforms {
    /// Reads under way.
    pub(super) fn reading(&self) -> usize {
        self.reading.len()
    }

    fn waiting(&self) -> usize {
        self.reading.iter().map(|r| r.waiters.len()).sum()
    }

    /// The kept waveform of `key`, if it is of the probe `ready` (else it is dropped: stale).
    fn kept(&mut self, key: &str, ready: &Arc<Ready>) -> Option<Arc<Waveform>> {
        let i = self.kept.iter().position(|k| k.key == key)?;
        let k = self.kept.remove(i);
        if !Arc::ptr_eq(&k.ready, ready) {
            self.kept_bytes = self.kept_bytes.saturating_sub(k.wave.bytes());
            return None;
        }
        let wave = Arc::clone(&k.wave);
        self.kept.push(k);
        Some(wave)
    }

    fn keep(&mut self, key: String, ready: Arc<Ready>, wave: Arc<Waveform>) {
        let bytes = wave.bytes();
        if bytes > CACHE_BYTES {
            return;
        }
        if let Some(i) = self.kept.iter().position(|k| k.key == key) {
            let old = self.kept.remove(i);
            self.kept_bytes = self.kept_bytes.saturating_sub(old.wave.bytes());
        }
        while self.kept_bytes + bytes > CACHE_BYTES && !self.kept.is_empty() {
            let old = self.kept.remove(0);
            self.kept_bytes = self.kept_bytes.saturating_sub(old.wave.bytes());
        }
        self.kept_bytes += bytes;
        self.kept.push(Kept { key, ready, wave });
    }

    /// The pool's watchdog gave up on job `ticket` of `key`: if it was a waveform read, its
    /// requests fail with `io` and the worker is told to stop if it ever returns.
    pub(super) fn give_up(&mut self, key: &str, ticket: u64) {
        if let Some(i) = self
            .reading
            .iter()
            .position(|r| r.key == key && r.ticket == ticket)
        {
            let r = self.reading.remove(i);
            r.cancel.store(true, Ordering::Relaxed);
            for w in r.waiters {
                fail(
                    &w.to,
                    w.id,
                    ErrorCode::Io,
                    "the file could not be read in time (the disk does not answer)",
                );
            }
        }
    }

    /// The doc's files are probed afresh (the show moved folder): reads under way may have
    /// been dropped with the doc's waiting jobs, so their requests fail now (`io`, try again)
    /// rather than wait for ever.
    pub(super) fn reset(&mut self) {
        for r in std::mem::take(&mut self.reading) {
            r.cancel.store(true, Ordering::Relaxed);
            for w in r.waiters {
                fail(
                    &w.to,
                    w.id,
                    ErrorCode::Io,
                    "the workspace's media were read afresh; ask again",
                );
            }
        }
    }

    /// A worker's waveform: answers its requests and keeps it. A stale ticket is dropped.
    pub(super) fn answer(
        &mut self,
        key: &str,
        ticket: u64,
        result: Result<Arc<Waveform>, WaveformError>,
    ) {
        let Some(i) = self
            .reading
            .iter()
            .position(|r| r.key == key && r.ticket == ticket)
        else {
            return;
        };
        let r = self.reading.remove(i);
        match result {
            Ok(wave) => {
                for w in &r.waiters {
                    answer(w, &wave);
                }
                self.keep(r.key, r.ready, wave);
            }
            Err(e) => {
                let code = match &e {
                    WaveformError::Media(m) if !super::media::volume_answered(m) => ErrorCode::Io,
                    _ => ErrorCode::Unsupported,
                };
                for w in r.waiters {
                    fail(&w.to, w.id, code, format!("no waveform: {e}"));
                }
            }
        }
    }
}

/// Bytes a waveform of `info` takes (all levels: about twice level 0).
fn waveform_bytes(info: &openlab_audio::MediaInfo) -> u64 {
    info.frames
        .div_ceil(openlab_audio::BASE_FRAMES)
        .saturating_mul(u64::from(info.channels))
        .saturating_mul(std::mem::size_of::<openlab_audio::Peak>() as u64)
        .saturating_mul(2)
}

/// Cuts `w`'s answer from `wave`.
fn answer(w: &Waiter, wave: &Waveform) {
    let rate = f64::from(wave.sample_rate.max(1));
    let duration = wave.frames as f64 / rate;
    let start = w.start.min(duration);
    let end = w.end.min(duration);
    // Seconds to frames: both are finite and within the file here, so the casts cannot wrap.
    let a = (start * rate).floor() as u64;
    let b = (end * rate).ceil() as u64;
    let channels = usize::from(wave.channels).max(1);
    let buckets = (w.buckets as usize).min(MAX_ANSWER_PEAKS / channels).max(1);
    let channels = (0..usize::from(wave.channels))
        .map(|c| {
            let peaks = wave.range(c, a, b, buckets);
            WaveformChannel {
                min: peaks.iter().map(|p| p.min).collect(),
                max: peaks.iter().map(|p| p.max).collect(),
            }
        })
        .collect();
    reply(
        &w.to,
        ServerMessage::Result {
            id: w.id,
            outcome: Outcome::Ok(OkBody {
                revision: None,
                data: Some(ResultData::Waveform(Box::new(WaveformData {
                    cue: w.cue,
                    duration,
                    start,
                    end,
                    channels,
                }))),
            }),
        },
    );
}

impl DocCx<'_> {
    /// A `waveform` request: answered at once from a kept waveform, else when a worker has
    /// read the file. The cue must be an Audio cue whose file is playable.
    pub(super) fn waveform(
        &mut self,
        id: RequestId,
        cue: CueId,
        (start, end, buckets): (f64, f64, u32),
        to: &ReplyTx,
    ) {
        let key = match self.doc.model.ws().find_cue(cue).map(|c| &c.kind) {
            None => return fail(to, id, ErrorCode::NotFound, "no such cue"),
            Some(CueKind::Audio { media: Some(m), .. }) => m.as_str().to_owned(),
            Some(CueKind::Audio { media: None, .. }) => {
                return fail(to, id, ErrorCode::NotFound, "the cue has no file");
            }
            Some(_) => return fail(to, id, ErrorCode::BadRequest, "not an Audio cue"),
        };
        let Some(ready) = self.doc.media.ready(&key).cloned() else {
            return fail(
                to,
                id,
                ErrorCode::Unsupported,
                "the file is not playable, or not read yet",
            );
        };
        let waiter = Waiter {
            id,
            to: to.clone(),
            cue,
            start,
            end,
            buckets,
        };
        let waves = &mut self.doc.waveforms;
        if let Some(wave) = waves.kept(&key, &ready) {
            return answer(&waiter, &wave);
        }
        if waves.waiting() >= MAX_WAITERS {
            return fail(
                to,
                id,
                ErrorCode::CommandRefused,
                "too many waveform requests are waiting",
            );
        }
        if let Some(r) = waves
            .reading
            .iter_mut()
            .find(|r| r.key == key && Arc::ptr_eq(&r.ready, &ready))
        {
            r.waiters.push(waiter);
            return;
        }
        if waves.reading.len() >= MAX_READS {
            return fail(
                to,
                id,
                ErrorCode::CommandRefused,
                "too many waveforms are being read",
            );
        }
        if waveform_bytes(&ready.probed.info) > MAX_WAVEFORM_BYTES {
            return fail(
                to,
                id,
                ErrorCode::Unsupported,
                "the file is too long to draw",
            );
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let Some((ticket, ready)) = self.doc.media.read_waveform(&key, Arc::clone(&cancel)) else {
            return fail(to, id, ErrorCode::Internal, "the media workers are gone");
        };
        self.doc.waveforms.reading.push(Reading {
            key,
            ticket,
            ready,
            cancel,
            waiters: vec![waiter],
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlab_audio::{Level, Peak};

    /// A one-level waveform of `buckets` level-0 buckets (4 bytes each).
    fn wave(buckets: usize) -> Arc<Waveform> {
        Arc::new(Waveform {
            sample_rate: 48_000,
            frames: buckets as u64 * openlab_audio::BASE_FRAMES,
            channels: 1,
            levels: vec![Level {
                frames_per_bucket: openlab_audio::BASE_FRAMES,
                channels: vec![vec![Peak::default(); buckets]],
            }],
        })
    }

    fn ready() -> Arc<Ready> {
        Arc::new(Ready {
            path: "/a.wav".into(),
            probed: openlab_audio::Probed {
                info: openlab_audio::MediaInfo {
                    channels: 1,
                    sample_rate: 48_000,
                    frames: 0,
                    duration: std::time::Duration::ZERO,
                },
                warnings: Vec::new(),
                seeking: openlab_audio::Seeking::Direct,
            },
        })
    }

    /// The kept waveforms stay within [`CACHE_BYTES`], the least recently used going first,
    /// and one of an earlier probe of the file is never served.
    #[test]
    fn kept_waveforms_are_bounded_and_follow_the_probe() {
        let mut w = Waveforms::default();
        let r = ready();
        // 16 MiB each; one allocation shared by every key.
        let big = wave(4 * 1024 * 1024);
        let per = big.bytes();
        let fit = CACHE_BYTES / per;
        for n in 0..=fit {
            w.keep(format!("k{n}"), Arc::clone(&r), Arc::clone(&big));
        }
        assert!(w.kept_bytes <= CACHE_BYTES);
        assert_eq!(w.kept.len(), fit);
        assert!(w.kept("k0", &r).is_none(), "the oldest went first");
        assert!(w.kept(&format!("k{fit}"), &r).is_some());
        // A new probe of the file: the old waveform is stale and dropped.
        assert!(w.kept("k1", &ready()).is_none());
        assert!(w.kept("k1", &r).is_none());
        assert_eq!(
            w.kept_bytes,
            w.kept.iter().map(|k| k.wave.bytes()).sum::<usize>()
        );
    }
}
