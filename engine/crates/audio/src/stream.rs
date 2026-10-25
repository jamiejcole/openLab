// SPDX-License-Identifier: GPL-3.0-or-later

//! Disk streaming (AUD-05, ARCHITECTURE §3.1, §3.5): a small pool of worker threads decodes and
//! resamples each stream into its voice's ring, about 2 s ahead of the mixer. Loops are unrolled
//! into the ring following the slice table, seeking back at each loop point ahead of time, so the
//! ring never starves at a loop. Workers may block and allocate; they reuse their buffers in
//! steady state.
//!
//! [`DiskStreamer::prepare`] does no I/O on the caller's thread: the file is opened by a worker,
//! and failures arrive through [`StreamHandle::error`].

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use rtrb::{Consumer, Producer, RingBuffer};

use crate::decode::{Media, MediaError, MediaInfo, MediaWarning, contain, open_source};
use crate::mixer::{VoiceId, VoiceStart};
use crate::plan::{MAX_SLICES, Plays, Slice, StreamLink};
use crate::resample::Pipeline;

pub const DEFAULT_WORKERS: usize = 2;
pub const DEFAULT_BUFFER_MS: u32 = 2000;
pub const DEFAULT_MAX_STREAMS: usize = 256;
/// Frames decoded per step; workers visit their streams round-robin, one chunk each.
const CHUNK_FRAMES: usize = 4096;
/// Most warnings kept per stream.
const MAX_STREAM_WARNINGS: usize = 256;
/// How often a worker with streams wakes to top up rings and look for devamp cuts.
const POLL: Duration = Duration::from_millis(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamerConfig {
    /// Rate every stream is resampled to.
    pub device_rate: u32,
    pub workers: usize,
    /// Ring length per stream (read-ahead).
    pub buffer_ms: u32,
    /// Streams open at once (each holds a decoder and a ring).
    pub max_streams: usize,
}

impl StreamerConfig {
    pub fn new(device_rate: u32) -> Self {
        StreamerConfig {
            device_rate,
            workers: DEFAULT_WORKERS,
            buffer_ms: DEFAULT_BUFFER_MS,
            max_streams: DEFAULT_MAX_STREAMS,
        }
    }
}

/// What to stream: `info` is the file's probe result (the worker checks the file still matches),
/// `slices` its playback plan from [`plan`](crate::plan()) in device frames.
#[derive(Debug)]
pub struct StreamRequest {
    pub media: Media,
    pub info: MediaInfo,
    pub slices: Vec<Slice>,
}

/// A stream ready to hand to a voice. `source` fills once a worker picks the stream up.
#[derive(Debug)]
pub struct PreparedStream {
    pub source: Consumer<f32>,
    pub channels: u16,
    pub slices: Vec<Slice>,
    pub link: Arc<StreamLink>,
    pub handle: StreamHandle,
}

impl PreparedStream {
    /// A voice start playing this stream: silent routing as [`VoiceStart::new`], with the slice
    /// table and devamp link set. Keep the handle while the voice plays; dropping it cancels the
    /// stream.
    pub fn into_voice(self, voice: VoiceId, cue_outputs: usize) -> (VoiceStart, StreamHandle) {
        let mut v = VoiceStart::new(voice, self.source, self.channels, cue_outputs);
        v.slices = self.slices;
        v.link = Some(self.link);
        (v, self.handle)
    }
}

#[derive(Debug, Default)]
struct Status {
    primed: bool,
    finished: bool,
    error: Option<MediaError>,
    written: u64,
    /// Devamp cuts made: the loop's stale frames are cut off and the continuation is queued.
    cuts: u64,
    /// Damage met while streaming, each once (see `StreamHandle::warnings`).
    warnings: Vec<MediaWarning>,
}

#[derive(Debug, Default)]
struct Shared {
    status: Mutex<Status>,
    changed: Condvar,
    cancelled: AtomicBool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Status> {
        self.status.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn update(&self, f: impl FnOnce(&mut Status)) {
        f(&mut self.lock());
        self.changed.notify_all();
    }
}

/// Control-side view of one stream. Dropping it cancels the stream.
#[derive(Debug)]
pub struct StreamHandle {
    shared: Arc<Shared>,
    wake: Sender<Msg>,
}

impl StreamHandle {
    /// The first chunk is in the ring (or the whole stream, if shorter): the cue is loaded.
    pub fn is_primed(&self) -> bool {
        self.shared.lock().primed
    }

    /// Every frame of the plan is in the ring.
    pub fn is_finished(&self) -> bool {
        self.shared.lock().finished
    }

    /// Why the stream failed. Once this is set the stream has let go of its ring, so the voice
    /// plays what was written and then ends with `FinishReason::EndOfStream`.
    pub fn error(&self) -> Option<MediaError> {
        self.shared.lock().error.clone()
    }

    /// Damage met while streaming, each once, in the order met. It plays as the warning says
    /// (silence for missing frames), so the timeline is unchanged. Damage the probe already
    /// reported shows up here too; anything else is new (the file changed since its probe).
    pub fn warnings(&self) -> Vec<MediaWarning> {
        self.shared.lock().warnings.clone()
    }

    /// Frames written to the ring so far.
    pub fn frames_written(&self) -> u64 {
        self.shared.lock().written
    }

    /// Devamp cuts made so far (see [`StreamLink`]).
    pub fn cuts(&self) -> u64 {
        self.shared.lock().cuts
    }

    // The `wait_*` helpers block (with a timeout): for I/O helper threads and tests, never the
    // control thread (ARCHITECTURE §3.1).

    /// Blocks until at least `n` devamp cuts are made.
    pub fn wait_cuts(&self, n: u64, timeout: Duration) -> Result<bool, MediaError> {
        self.wait(timeout, |s| s.cuts >= n)
    }

    /// Blocks until primed. `Ok(false)` on timeout; the stream's error if it failed.
    pub fn wait_primed(&self, timeout: Duration) -> Result<bool, MediaError> {
        self.wait(timeout, |s| s.primed)
    }

    /// Blocks until at least `frames` are written (or the stream finished).
    pub fn wait_written(&self, frames: u64, timeout: Duration) -> Result<bool, MediaError> {
        self.wait(timeout, |s| s.written >= frames || s.finished)
    }

    fn wait(&self, timeout: Duration, done: impl Fn(&Status) -> bool) -> Result<bool, MediaError> {
        let deadline = Instant::now() + timeout;
        let mut s = self.shared.lock();
        loop {
            if let Some(e) = &s.error {
                return Err(e.clone());
            }
            if done(&s) {
                return Ok(true);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            s = self
                .shared
                .changed
                .wait_timeout(s, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    pub fn cancel(&self) {
        self.shared.cancelled.store(true, Ordering::Release);
        // A worker that has gone already needs no wake-up.
        let _ = self.wake.send(Msg::Wake);
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

enum Msg {
    Add(Box<Task>),
    Wake,
    Shutdown,
}

/// One slot of the open-stream limit, released when the stream's task is dropped.
struct Permit(Arc<AtomicUsize>);

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A worker thread, reached through its message channel. Never joined (see `Drop`).
struct Worker {
    tx: Sender<Msg>,
}

pub struct DiskStreamer {
    config: StreamerConfig,
    workers: Vec<Worker>,
    next: usize,
    open: Arc<AtomicUsize>,
}

impl DiskStreamer {
    pub fn new(config: StreamerConfig) -> Result<Self, MediaError> {
        if config.workers == 0 || config.device_rate == 0 || config.max_streams == 0 {
            return Err(MediaError::Worker);
        }
        let mut workers = Vec::with_capacity(config.workers);
        for i in 0..config.workers {
            let (tx, rx) = mpsc::channel();
            std::thread::Builder::new()
                .name(format!("openlab-disk-{i}"))
                .spawn(move || run_worker(rx))
                .map_err(|_| MediaError::Worker)?;
            workers.push(Worker { tx });
        }
        Ok(DiskStreamer {
            config,
            workers,
            next: 0,
            open: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub fn config(&self) -> StreamerConfig {
        self.config
    }

    /// Streams currently open.
    pub fn open_streams(&self) -> usize {
        self.open.load(Ordering::Acquire)
    }

    /// Creates the stream's ring and hands the stream to a worker. Does no I/O here; checks the
    /// slice table against `info` and the stream limit.
    pub fn prepare(&mut self, req: StreamRequest) -> Result<PreparedStream, MediaError> {
        let StreamRequest {
            media,
            info,
            slices,
        } = req;
        let total = info.frames_at(self.config.device_rate);
        let fits = |s: &Slice| {
            s.frames > 0 && s.plays != Plays::Times(0) && s.start.saturating_add(s.frames) <= total
        };
        if slices.is_empty() || slices.len() > MAX_SLICES || !slices.iter().all(fits) {
            return Err(MediaError::BadSlices);
        }
        let max = self.config.max_streams;
        self.open
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < max).then_some(n + 1)
            })
            .map_err(|_| MediaError::TooManyStreams)?;
        let permit = Permit(Arc::clone(&self.open));

        let ch = usize::from(info.channels).max(1);
        let ring_frames = (u64::from(self.config.device_rate) * u64::from(self.config.buffer_ms)
            / 1000)
            .max(CHUNK_FRAMES as u64);
        let ring_frames = usize::try_from(ring_frames).unwrap_or(CHUNK_FRAMES);
        let (producer, source) = RingBuffer::new(ring_frames * ch);
        let shared = Arc::new(Shared::default());
        let link = Arc::new(StreamLink::default());
        let task = Box::new(Task {
            shared: Arc::clone(&shared),
            link: Arc::clone(&link),
            producer,
            media: Some(media),
            info,
            device_rate: self.config.device_rate,
            slices: slices.clone(),
            pipe: None,
            entry: 0,
            play: 0,
            off: 0,
            written: 0,
            origin: (0, 0),
            handled_cut: None,
            cut: None,
            scratch: vec![0.0; CHUNK_FRAMES * ch],
            _permit: permit,
        });
        let worker = self
            .workers
            .get(self.next % self.workers.len())
            .ok_or(MediaError::Worker)?;
        self.next = self.next.wrapping_add(1);
        worker
            .tx
            .send(Msg::Add(task))
            .map_err(|_| MediaError::Worker)?;
        Ok(PreparedStream {
            source,
            channels: info.channels,
            slices,
            link,
            handle: StreamHandle {
                shared,
                wake: worker.tx.clone(),
            },
        })
    }
}

impl Drop for DiskStreamer {
    /// Tells the workers to stop and does not wait: a worker blocked in a read on a stalled
    /// disk must not hang the caller. Each worker exits after its current step, dropping its
    /// streams (their voices then end).
    fn drop(&mut self) {
        for w in &self.workers {
            let _ = w.tx.send(Msg::Shutdown);
        }
    }
}

fn run_worker(rx: Receiver<Msg>) {
    let mut tasks: Vec<Box<Task>> = Vec::new();
    let mut failed: Vec<(Arc<Shared>, MediaError)> = Vec::new();
    loop {
        loop {
            match rx.try_recv() {
                Ok(Msg::Add(t)) => tasks.push(t),
                Ok(Msg::Wake) => {}
                Ok(Msg::Shutdown) | Err(TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => break,
            }
        }
        let mut worked = false;
        tasks.retain_mut(|t| match t.step() {
            Progress::Worked => {
                worked = true;
                true
            }
            Progress::Idle => true,
            Progress::Done => false,
            Progress::Failed(e) => {
                failed.push((Arc::clone(&t.shared), e));
                false
            }
        });
        // Published only now that the failed tasks (and their ring producers) are dropped: once
        // a caller sees the error, the voice is certain to end after the frames already written.
        for (shared, e) in failed.drain(..) {
            shared.update(|s| s.error = Some(e));
        }
        if worked {
            continue;
        }
        let msg = if tasks.is_empty() {
            rx.recv().ok()
        } else {
            match rx.recv_timeout(POLL) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => Some(Msg::Wake),
                Err(RecvTimeoutError::Disconnected) => None,
            }
        };
        match msg {
            Some(Msg::Add(t)) => tasks.push(t),
            Some(Msg::Wake) => {}
            Some(Msg::Shutdown) | None => return,
        }
    }
}

enum Progress {
    Worked,
    Idle,
    Done,
    Failed(MediaError),
}

struct Task {
    shared: Arc<Shared>,
    link: Arc<StreamLink>,
    producer: Producer<f32>,
    media: Option<Media>,
    info: MediaInfo,
    device_rate: u32,
    slices: Vec<Slice>,
    pipe: Option<Pipeline>,
    /// Write cursor: slice-table entry, plays of it done, frames into the current play.
    entry: usize,
    play: u32,
    off: u64,
    /// Ring frames written so far.
    written: u64,
    /// Entry and ring position the cursor last started from without a gap (start or a cut);
    /// ring positions of later loop points follow from the table.
    origin: (usize, u64),
    handled_cut: Option<u64>,
    /// A cut not reached yet: at ring position, jump to entry.
    cut: Option<(u64, usize)>,
    scratch: Vec<f32>,
    _permit: Permit,
}

impl Task {
    fn step(&mut self) -> Progress {
        if self.shared.cancelled.load(Ordering::Acquire) || self.producer.is_abandoned() {
            return Progress::Done;
        }
        contain(|| self.fill()).unwrap_or_else(Progress::Failed)
    }

    fn fill(&mut self) -> Result<Progress, MediaError> {
        if self.pipe.is_none() {
            let media = self.media.take().ok_or(MediaError::Worker)?;
            let src = open_source(media)?;
            if src.channels() != usize::from(self.info.channels)
                || src.sample_rate() != self.info.sample_rate
            {
                return Err(MediaError::Changed);
            }
            self.pipe = Some(Pipeline::new(src, self.info.frames, self.device_rate)?);
        }
        self.check_cut()?;
        let Some(s) = self.slices.get(self.entry).copied() else {
            // Stay until the voice lets go of the ring: a devamp may still need a cut.
            if !self.shared.lock().finished {
                self.shared.update(|st| {
                    st.finished = true;
                    st.primed = true;
                });
            }
            return Ok(Progress::Idle);
        };
        let Some(pipe) = self.pipe.as_mut() else {
            return Err(MediaError::Worker);
        };
        let ch = pipe.channels();
        let free = self.producer.slots() / ch;
        let mut n = free
            .min(CHUNK_FRAMES)
            .min(usize::try_from(s.frames - self.off).unwrap_or(usize::MAX));
        if let Some((at, _)) = self.cut {
            n = n.min(usize::try_from(at - self.written).unwrap_or(usize::MAX));
        }
        // Wait for a worthwhile gap unless a short tail or a cut point limits the write.
        let worthwhile = CHUNK_FRAMES.min(self.producer.buffer().capacity() / ch / 4);
        if n == 0 || (free < worthwhile && n == free) {
            return Ok(Progress::Idle);
        }
        let at = s.start + self.off;
        if pipe.position() != at {
            pipe.seek(at)?;
        }
        let buf = self.scratch.get_mut(..n * ch).ok_or(MediaError::Worker)?;
        let got = pipe.read(buf)?;
        let new = pipe.take_warnings();
        if !new.is_empty() {
            self.shared.update(|st| {
                for w in new {
                    if st.warnings.len() < MAX_STREAM_WARNINGS && !st.warnings.contains(&w) {
                        st.warnings.push(w);
                    }
                }
            });
        }
        if got < n {
            // The slice table was checked against the probed length, so the file has changed.
            return Err(MediaError::Changed);
        }
        let chunk = self
            .producer
            .write_chunk_uninit(n * ch)
            .map_err(|_| MediaError::Worker)?;
        chunk.fill_from_iter(buf.iter().copied());
        self.written += n as u64;
        self.off += n as u64;
        if self.off == s.frames {
            self.off = 0;
            self.play = self.play.saturating_add(1);
            if let Plays::Times(k) = s.plays
                && self.play >= k
            {
                self.entry += 1;
                self.play = 0;
            }
        }
        if let Some((at, target)) = self.cut
            && at == self.written
        {
            self.jump(target);
        }
        let written = self.written;
        self.shared.update(|st| {
            st.primed = true;
            st.written = written;
        });
        Ok(Progress::Worked)
    }

    /// Handles a devamp cut the voice published: find the loop entry it ends and the slice to
    /// continue with, then cut now (stale frames already in the ring) or when the write cursor
    /// reaches the boundary. A cut that matches no loop of the table fails the stream, so the
    /// voice waiting for the continuation ends instead of waiting forever.
    fn check_cut(&mut self) -> Result<(), MediaError> {
        let Some(d) = self.link.cut_requested() else {
            return Ok(());
        };
        if self.cut.is_some() || self.handled_cut == Some(d) {
            return Ok(());
        }
        self.handled_cut = Some(d);
        let (mut k, mut pos) = self.origin;
        let found = loop {
            let Some(s) = self.slices.get(k) else {
                break None;
            };
            match s.plays {
                Plays::Times(n) => {
                    let end = pos.saturating_add(s.frames.saturating_mul(u64::from(n)));
                    if d <= end {
                        break Some(k);
                    }
                    pos = end;
                    k += 1;
                }
                Plays::Infinite => break Some(k),
            }
        };
        let Some(k) = found else {
            return Err(MediaError::BadSlices);
        };
        let index = self.slices.get(k).map(|s| s.index);
        let target = (k + 1..self.slices.len())
            .find(|&i| self.slices.get(i).map(|s| s.index) != index)
            .unwrap_or(self.slices.len());
        if self.written < d {
            self.cut = Some((d, target));
        } else {
            self.jump(target);
        }
        Ok(())
    }

    /// Continues with entry `target` from the current ring position and tells the voice where.
    fn jump(&mut self, target: usize) {
        self.cut = None;
        self.entry = target;
        self.play = 0;
        self.off = 0;
        self.origin = (target, self.written);
        self.link.resume(self.written);
        self.shared.update(|s| {
            s.cuts += 1;
            s.finished = false;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_config_and_bad_slices() {
        let mut c = StreamerConfig::new(48_000);
        c.workers = 0;
        assert!(DiskStreamer::new(c).is_err());
        let mut d = DiskStreamer::new(StreamerConfig::new(48_000)).unwrap();
        let info = MediaInfo {
            channels: 1,
            sample_rate: 48_000,
            frames: 100,
            duration: Duration::from_millis(2),
        };
        let req = |slices| StreamRequest {
            media: Media::Path("/nonexistent".into()),
            info,
            slices,
        };
        let slice = |start, frames, plays| Slice {
            index: 0,
            start,
            frames,
            plays,
        };
        for bad in [
            vec![],
            vec![slice(0, 0, Plays::Times(1))],
            vec![slice(0, 10, Plays::Times(0))],
            vec![slice(50, 51, Plays::Times(1))],
        ] {
            assert_eq!(d.prepare(req(bad)).err(), Some(MediaError::BadSlices));
        }
        assert_eq!(d.open_streams(), 0);
    }

    #[test]
    fn missing_file_fails_the_stream_not_the_caller() {
        let mut d = DiskStreamer::new(StreamerConfig::new(48_000)).unwrap();
        let info = MediaInfo {
            channels: 1,
            sample_rate: 48_000,
            frames: 100,
            duration: Duration::from_millis(2),
        };
        let p = d
            .prepare(StreamRequest {
                media: Media::Path("/nonexistent/x.wav".into()),
                info,
                slices: vec![Slice {
                    index: 0,
                    start: 0,
                    frames: 100,
                    plays: Plays::Times(1),
                }],
            })
            .unwrap();
        let r = p.handle.wait_primed(Duration::from_secs(10));
        assert_eq!(r, Err(MediaError::NotFound("/nonexistent/x.wav".into())));
    }

    #[test]
    fn a_cut_outside_the_table_fails_the_stream() {
        let mut d = DiskStreamer::new(StreamerConfig::new(48_000)).unwrap();
        let pre = crate::decode::Preloaded::from_samples(1, 48_000, vec![0.25; 1000]);
        let info = pre.info();
        let media = Media::Preloaded(pre);
        let p = d
            .prepare(StreamRequest {
                media,
                info,
                slices: vec![Slice {
                    index: 0,
                    start: 0,
                    frames: 1000,
                    plays: Plays::Times(1),
                }],
            })
            .unwrap();
        assert!(
            p.handle
                .wait_written(u64::MAX, Duration::from_secs(10))
                .unwrap()
        );
        // Past the end of the table: there is no loop to leave.
        p.link.request_cut(5_000);
        let r = p.handle.wait_cuts(1, Duration::from_secs(10));
        assert_eq!(r, Err(MediaError::BadSlices));
        // The stream let go of the ring, so the voice playing it ends.
        assert!(p.source.is_abandoned());
    }

    #[test]
    fn stream_limit_is_enforced_and_released() {
        let mut c = StreamerConfig::new(48_000);
        c.max_streams = 1;
        let mut d = DiskStreamer::new(c).unwrap();
        let info = MediaInfo {
            channels: 1,
            sample_rate: 48_000,
            frames: 100,
            duration: Duration::from_millis(2),
        };
        let req = || StreamRequest {
            media: Media::Path("/nonexistent/x.wav".into()),
            info,
            slices: vec![Slice {
                index: 0,
                start: 0,
                frames: 100,
                plays: Plays::Times(1),
            }],
        };
        let a = d.prepare(req()).unwrap();
        assert_eq!(d.prepare(req()).err(), Some(MediaError::TooManyStreams));
        // The failed stream's task is dropped by its worker, releasing the slot.
        assert!(a.handle.wait_primed(Duration::from_secs(10)).is_err());
        drop(a);
        let deadline = Instant::now() + Duration::from_secs(10);
        while d.open_streams() != 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(d.prepare(req()).is_ok());
    }
}
