// SPDX-License-Identifier: GPL-3.0-or-later

//! The engine's one waveform reader (AUD-14). A waveform read decodes a whole file, so it runs
//! on a thread of its own, never on the control thread and never on a probe worker: probes and
//! preloads are what a GO waits for, and an inspector click must never stand in their way. One
//! read runs at a time for every open doc; the rest wait in a short queue. A read that takes
//! longer than its timeout (a disk that stopped answering) is given up on: its thread is left to
//! end when its read returns and the next read starts on a fresh one. That never touches the
//! probe pool's stuck-worker count; after [`MAX_ABANDONED_READS`] reads given up on, waveforms
//! are refused for the rest of the run, so a dead disk cannot make the engine start threads
//! without end.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use openlab_audio::{Media, MediaError, MediaInfo, WaveformError, WaveformResult, build_waveform};

use super::DocId;
use super::inbox::Inbox;
use super::media::ProbeClock;

/// How a file's waveform is read: [`read_waveform`] in the engine; tests inject a stand-in that
/// holds on `path` as a stalled disk would. `media` is the file or its preload.
pub type WaveformFn =
    fn(&Path, Media, &MediaInfo, &AtomicBool) -> Result<WaveformResult, WaveformError>;

/// [`openlab_audio::build_waveform`] of `media` (`path` only names it).
pub fn read_waveform(
    _path: &Path,
    media: Media,
    info: &MediaInfo,
    cancel: &AtomicBool,
) -> Result<WaveformResult, WaveformError> {
    build_waveform(media, info, cancel)
}

/// A read running longer than this is given up on. Far longer than a probe's limit: a three-hour
/// file on a slow disk decodes for a while, and nothing in the show waits for it.
pub const WAVEFORM_TIMEOUT: Duration = Duration::from_secs(120);
/// Reads waiting to start, every doc's.
pub const MAX_QUEUED: usize = 8;
/// Reads given up on before waveforms are refused for the rest of the run.
pub const MAX_ABANDONED_READS: usize = 4;

struct Job {
    doc: DocId,
    ticket: u64,
    path: PathBuf,
    media: Media,
    info: MediaInfo,
    cancel: Arc<AtomicBool>,
}

struct Running {
    doc: DocId,
    ticket: u64,
    since: Instant,
    cancel: Arc<AtomicBool>,
}

/// Why a read was not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// [`MAX_QUEUED`] reads are waiting already.
    Busy,
    /// [`MAX_ABANDONED_READS`] reads hung: no more are started.
    Stuck,
}

pub struct WaveformReader {
    inbox: Sender<Inbox>,
    read: WaveformFn,
    timeout: Duration,
    clock: ProbeClock,
    next_ticket: u64,
    running: Option<Running>,
    queue: VecDeque<Job>,
    abandoned: usize,
}

impl WaveformReader {
    pub fn new(
        inbox: Sender<Inbox>,
        read: WaveformFn,
        timeout: Duration,
        clock: ProbeClock,
    ) -> Self {
        Self {
            inbox,
            read,
            timeout,
            clock,
            next_ticket: 0,
            running: None,
            queue: VecDeque::new(),
            abandoned: 0,
        }
    }

    /// Queues a read of `media` for `doc`; its answer comes as [`Inbox::Waveform`] with the
    /// returned ticket. `cancel` is set once nobody wants it.
    pub fn submit(
        &mut self,
        doc: DocId,
        path: PathBuf,
        media: Media,
        info: MediaInfo,
    ) -> Result<(u64, Arc<AtomicBool>), Refused> {
        if self.abandoned >= MAX_ABANDONED_READS {
            return Err(Refused::Stuck);
        }
        if self.queue.len() >= MAX_QUEUED {
            return Err(Refused::Busy);
        }
        self.next_ticket += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        self.queue.push_back(Job {
            doc,
            ticket: self.next_ticket,
            path,
            media,
            info,
            cancel: Arc::clone(&cancel),
        });
        self.start_next();
        Ok((self.next_ticket, cancel))
    }

    /// Read `ticket` answered (or failed): the next one starts.
    pub fn finished(&mut self, ticket: u64) {
        if self.running.as_ref().is_some_and(|r| r.ticket == ticket) {
            self.running = None;
            self.start_next();
        }
    }

    /// A doc closed or reset: its waiting reads go, and a running one is told to stop (its
    /// answer is dropped; the reader moves on when it comes or is given up on).
    pub fn forget(&mut self, doc: DocId) {
        self.queue.retain(|j| j.doc != doc);
        if let Some(r) = self.running.as_ref().filter(|r| r.doc == doc) {
            r.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Gives up on a read past its timeout. Returns the reads that will not answer: the one
    /// given up on and, once the limit is reached, every waiting one.
    pub fn watchdog(&mut self) -> Vec<(DocId, u64)> {
        let mut out = Vec::new();
        let now = self.clock.now();
        if let Some(r) = self
            .running
            .take_if(|r| now.saturating_duration_since(r.since) >= self.timeout)
        {
            r.cancel.store(true, Ordering::Relaxed);
            self.abandoned += 1;
            out.push((r.doc, r.ticket));
            if self.abandoned >= MAX_ABANDONED_READS {
                out.extend(self.queue.drain(..).map(|j| (j.doc, j.ticket)));
            }
            self.start_next();
        }
        out
    }

    /// Reads running or waiting.
    pub fn pending(&self) -> usize {
        self.queue.len() + usize::from(self.running.is_some())
    }

    /// Reads given up on so far.
    pub fn abandoned(&self) -> usize {
        self.abandoned
    }

    fn start_next(&mut self) {
        while self.running.is_none() {
            let Some(job) = self.queue.pop_front() else {
                return;
            };
            if job.cancel.load(Ordering::Relaxed) {
                continue;
            }
            let Job {
                doc,
                ticket,
                path,
                media,
                info,
                cancel,
            } = job;
            let (inbox, read, flag) = (self.inbox.clone(), self.read, Arc::clone(&cancel));
            // Never joined: a read stuck on a dead disk must not hang shutdown or the next read.
            let spawned = std::thread::Builder::new()
                .name("openlab-waveform".to_owned())
                .spawn(move || {
                    // A panic inside the decoder is a bad file, not a dead reader.
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        read(&path, media, &info, &flag)
                    }))
                    .unwrap_or_else(|_| Err(MediaError::Corrupt("decoder failed".into()).into()))
                    .map(|r| Arc::new(r.waveform));
                    let _ = inbox.send(Inbox::Waveform {
                        doc,
                        ticket,
                        result,
                    });
                });
            match spawned {
                Ok(_) => {
                    self.running = Some(Running {
                        doc,
                        ticket,
                        since: self.clock.now(),
                        cancel,
                    });
                }
                Err(_) => {
                    // Out of threads: this read fails (as a worker that died would) and the next
                    // is tried.
                    let _ = self.inbox.send(Inbox::Waveform {
                        doc,
                        ticket,
                        result: Err(MediaError::Worker.into()),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
