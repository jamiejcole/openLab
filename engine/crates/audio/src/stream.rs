// SPDX-License-Identifier: GPL-3.0-or-later

//! Disk streaming (AUD-05, ARCHITECTURE §3.1, §3.5): a small pool of worker threads decodes and
//! resamples each stream into its voice's ring, about 2 s ahead of the mixer. Loops are unrolled
//! into the ring following the slice table ahead of time, so the ring never starves at a loop:
//! a loop's later plays come from its loop cache (its first play, as written), or for loops
//! beyond the cache's bounds from a seek back. Workers may block and allocate (fallibly, within
//! the cache budget, for loop caches and exit buffers); they reuse their buffers in steady state.
//!
//! [`DiskStreamer::prepare`] does no I/O on the caller's thread: the file is opened by a worker,
//! and failures arrive through [`StreamHandle::error`].

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use rtrb::{Consumer, Producer, RingBuffer};

use crate::decode::{Media, MediaError, MediaInfo, MediaWarning, Seeking, contain, open_source};
use crate::mixer::{VoiceId, VoiceStart};
use crate::plan::{Exit, MAX_SLICES, Plays, Slice, StreamLink};

/// Bytes per sample in the ring, the loop caches and the exit buffers.
const SAMPLE_BYTES: usize = std::mem::size_of::<f32>();
use crate::resample::Pipeline;

pub const DEFAULT_WORKERS: usize = 2;
pub const DEFAULT_BUFFER_MS: u32 = 2000;
pub const DEFAULT_MAX_STREAMS: usize = 256;
/// Loop exit buffer length. After a devamp leaves a loop the worker must, within this time:
/// notice the freed ring (its poll, 5 ms), seek (a fresh demuxer plus pre-roll: up to about
/// 10 ms on long compressed files, see docs/audio.md) and decode its first chunk (4096 frames),
/// while the device still has to be fed a block ahead (up to 8192 frames = 170 ms at 48 kHz).
/// 250 ms covers that with margin.
pub const DEFAULT_EXIT_MS: u32 = 250;
/// Most memory one stream's loop exit buffers may hold (L-R2): 32 MiB is every exit of a
/// 255-slice stereo table (about 96 KB each at 48 kHz), 43 exits at 16 channels, 10 at 64.
pub const DEFAULT_EXIT_BYTES: usize = 32 << 20;
/// Longest loop one stream keeps in its loop cache (M3.13): 16 MiB is about 43 s of stereo or
/// 1.3 s of 64 channels at 48 kHz. Longer loops seek once per play, which costs little next to
/// their length.
pub const DEFAULT_LOOP_CACHE_BYTES: usize = 16 << 20;
/// Memory all streams' loop caches may hold together.
pub const DEFAULT_CACHE_BUDGET_BYTES: usize = 256 << 20;
/// Memory all streams' loop exit buffers may hold together: 2730 stereo exits or 85 of 64
/// channels. Separate from the loop caches', so optional cache memory never crowds out the
/// exits that make a devamp gapless.
pub const DEFAULT_EXIT_BUDGET_BYTES: usize = 256 << 20;
/// Per-step seek decoding: 65536 frames (1.4 s at 48 kHz) take a few ms to decode, so the
/// worker gets back to its other streams long before their rings (2 s) run low.
pub const DEFAULT_STEP_FRAMES: u64 = 1 << 16;
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
    /// Length of each loop exit's buffer (see [`StreamLink`]): how long the worker has, after a
    /// devamp leaves a loop, to refill the ring with the continuation.
    pub exit_ms: u32,
    /// Most source frames one stream decodes towards a seek target in one worker step, so a
    /// long seek (a damaged file decoding from its start) never holds up the worker's other
    /// streams.
    pub step_frames: u64,
    /// Most bytes of loop exit buffers per stream; exits beyond it are not built (see
    /// docs/audio.md, Loop exits).
    pub exit_bytes: usize,
    /// Longest loop (in bytes of device-rate frames) a stream plays from its loop cache instead
    /// of seeking back for every play; 0 turns the cache off.
    pub loop_cache_bytes: usize,
    /// Most bytes all streams' loop caches hold together.
    pub cache_budget_bytes: usize,
    /// Most bytes all streams' loop exit buffers hold together.
    pub exit_budget_bytes: usize,
}

impl StreamerConfig {
    pub fn new(device_rate: u32) -> Self {
        StreamerConfig {
            device_rate,
            workers: DEFAULT_WORKERS,
            buffer_ms: DEFAULT_BUFFER_MS,
            max_streams: DEFAULT_MAX_STREAMS,
            exit_ms: DEFAULT_EXIT_MS,
            step_frames: DEFAULT_STEP_FRAMES,
            exit_bytes: DEFAULT_EXIT_BYTES,
            loop_cache_bytes: DEFAULT_LOOP_CACHE_BYTES,
            cache_budget_bytes: DEFAULT_CACHE_BUDGET_BYTES,
            exit_budget_bytes: DEFAULT_EXIT_BUDGET_BYTES,
        }
    }
}

/// What to stream: `info` and `seeking` are the file's probe result ([`Probed`](crate::Probed);
/// the worker checks the file still matches, and seeks as the probe found so a damaged file
/// plays on the probe's timeline), `slices` its playback plan from [`plan`](crate::plan()) in
/// device frames.
#[derive(Debug)]
pub struct StreamRequest {
    pub media: Media,
    pub info: MediaInfo,
    pub seeking: Seeking,
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
    /// The latest cut (voice frame) the worker has looked at: made, or waiting for the write
    /// cursor to reach its boundary.
    cut_seen: Option<u64>,
    /// Damage met while streaming, each once (see `StreamHandle::warnings`).
    warnings: Vec<MediaWarning>,
    /// Ring position where the latest devamp's continuation starts.
    resume: Option<u64>,
    /// Bytes held by the loop cache and the exit buffers built so far.
    cache_bytes: usize,
    /// Frames written to the ring from the loop cache.
    cached_frames: u64,
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
    link: Arc<StreamLink>,
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

    /// Ring position (frames written before it) where the latest devamp's continuation starts,
    /// after the frames the voice plays from the loop exit's buffer. For diagnostics and tests.
    pub fn last_resume(&self) -> Option<u64> {
        self.shared.lock().resume
    }

    /// Bytes this stream holds in its loop cache and loop exit buffers (counted against
    /// [`StreamerConfig::cache_budget_bytes`]).
    pub fn cache_bytes(&self) -> usize {
        self.shared.lock().cache_bytes
    }

    /// Frames written to the ring from the loop cache rather than decoded (M3.13).
    pub fn cached_frames(&self) -> u64 {
        self.shared.lock().cached_frames
    }

    // The `wait_*` helpers block (with a timeout): for I/O helper threads and tests, never the
    // control thread (ARCHITECTURE §3.1).

    /// Blocks until the worker has looked at the latest devamp cut the voice published (made, or
    /// waiting for the write cursor to reach its boundary); at once if there is none. The
    /// worker notices a cut on its next poll, in real time, so a driver that renders faster than
    /// real time (the offline render) must wait for it before rendering on: live, the loop exit
    /// buffer covers that latency, but it is measured in rendered frames.
    pub fn wait_cut_seen(&self, timeout: Duration) -> Result<bool, MediaError> {
        self.wait(timeout, |s| {
            self.link
                .cut_requested()
                .is_none_or(|(d, _)| s.cut_seen == Some(d))
        })
    }

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
    budget: Arc<Budget>,
    exit_budget: Arc<Budget>,
}

/// The memory all streams' loop caches and exit buffers may hold together.
#[derive(Debug)]
pub(crate) struct Budget {
    used: AtomicUsize,
    limit: usize,
}

/// Bytes taken from a [`Budget`]; given back when dropped (on a disk worker or the control
/// thread, never the audio thread: exit buffers are freed with their link).
#[derive(Debug)]
pub(crate) struct Reservation {
    budget: Arc<Budget>,
    bytes: usize,
}

impl Budget {
    fn new(limit: usize) -> Self {
        Budget {
            used: AtomicUsize::new(0),
            limit,
        }
    }

    fn room(&self) -> usize {
        self.limit.saturating_sub(self.used.load(Ordering::Acquire))
    }
}

impl Reservation {
    fn take(budget: &Arc<Budget>, bytes: usize) -> Option<Self> {
        let limit = budget.limit;
        budget
            .used
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_add(bytes).filter(|&t| t <= limit)
            })
            .ok()?;
        Some(Reservation {
            budget: Arc::clone(budget),
            bytes,
        })
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
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
            budget: Arc::new(Budget::new(config.cache_budget_bytes)),
            exit_budget: Arc::new(Budget::new(config.exit_budget_bytes)),
        })
    }

    pub fn config(&self) -> StreamerConfig {
        self.config
    }

    /// Streams currently open.
    pub fn open_streams(&self) -> usize {
        self.open.load(Ordering::Acquire)
    }

    /// Bytes all streams hold in loop caches and exit buffers (at most
    /// [`StreamerConfig::cache_budget_bytes`] plus [`StreamerConfig::exit_budget_bytes`]).
    pub fn cache_bytes(&self) -> usize {
        self.budget.used.load(Ordering::Acquire) + self.exit_budget.used.load(Ordering::Acquire)
    }

    /// Creates the stream's ring and hands the stream to a worker. Does no I/O here; checks the
    /// slice table against `info` and the stream limit.
    pub fn prepare(&mut self, req: StreamRequest) -> Result<PreparedStream, MediaError> {
        let worker = self
            .workers
            .get(self.next % self.workers.len())
            .ok_or(MediaError::Worker)?;
        self.next = self.next.wrapping_add(1);
        let wake = worker.tx.clone();
        let (task, prepared) = self.task(req, wake)?;
        worker
            .tx
            .send(Msg::Add(task))
            .map_err(|_| MediaError::Worker)?;
        Ok(prepared)
    }

    /// The task a worker runs for `req`, and the stream it fills.
    fn task(
        &self,
        req: StreamRequest,
        wake: Sender<Msg>,
    ) -> Result<(Box<Task>, PreparedStream), MediaError> {
        let StreamRequest {
            media,
            info,
            seeking,
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
        let link = Arc::new(StreamLink::with_entries(slices.len()));
        let entries = slices.len();
        let task = Box::new(Task {
            shared: Arc::clone(&shared),
            link: Arc::clone(&link),
            producer,
            media: Some(media),
            seeking,
            info,
            device_rate: self.config.device_rate,
            slices: slices.clone(),
            pipe: None,
            entry: 0,
            play: 0,
            off: 0,
            written: 0,
            origin: (0, 0),
            offset: 0,
            exit_frames: usize::try_from(
                u64::from(self.config.device_rate) * u64::from(self.config.exit_ms) / 1000,
            )
            .unwrap_or(0),
            exit_state: vec![ExitState::Unknown; entries],
            closure_ready: vec![false; entries],
            closure_from: 0,
            direct_from: 0,
            exit_cap: self.config.exit_bytes,
            exit_used: 0,
            exit_wip: None,
            work: Vec::new(),
            seen: vec![false; entries],
            cache: None,
            cache_cap: self.config.loop_cache_bytes,
            budget: Arc::clone(&self.budget),
            exit_budget: Arc::clone(&self.exit_budget),
            cached_frames: 0,
            step_frames: self.config.step_frames.max(1),
            handled_cut: None,
            cut: None,
            scratch: vec![0.0; CHUNK_FRAMES * ch],
            _permit: permit,
        });
        Ok((
            task,
            PreparedStream {
                source,
                channels: info.channels,
                slices,
                handle: StreamHandle {
                    shared,
                    link: Arc::clone(&link),
                    wake,
                },
                link,
            },
        ))
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
        let mut i = 0;
        while let Some(t) = tasks.get_mut(i) {
            match t.step() {
                Progress::Worked => {
                    worked = true;
                    i += 1;
                }
                Progress::Idle => i += 1,
                Progress::Done => drop(tasks.swap_remove(i)),
                Progress::Failed(e) => {
                    let shared = Arc::clone(&t.shared);
                    drop(tasks.swap_remove(i));
                    // Published right after the task (and its ring producer) is dropped, before
                    // any other stream's step can block: once a caller sees the error, the voice
                    // is certain to end after the frames already written.
                    shared.update(|s| s.error = Some(e));
                }
            }
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

/// A loop exit buffer being decoded: the table unrolled from `target`, cursor at `entry`,
/// `play`, `off`. `play0` is where in `samples` the current entry's first play starts, when it
/// starts at its beginning: later plays of a loop are copied from there instead of decoded.
struct ExitWip {
    target: usize,
    samples: Vec<f32>,
    entry: usize,
    play: u32,
    off: u64,
    play0: Option<usize>,
    budget: Reservation,
}

/// Whether the exit buffer for leaving into an entry exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExitState {
    Unknown,
    Ready,
    /// Not built: over the per-stream cap, or the memory was not available. A devamp into it
    /// waits for the disk (see docs/audio.md, Loop exits).
    Refused,
    /// Not built yet: the global exit budget was full. Retried while the stream is idle.
    Starved,
}

/// The device-rate frames of one play of a loop entry, recorded as they were first written to
/// the ring; once `complete`, later plays are written from here (M3.13).
struct LoopCache {
    entry: usize,
    samples: Vec<f32>,
    complete: bool,
    _budget: Reservation,
}

/// A devamp cut the write cursor has not reached yet.
#[derive(Clone, Copy, Debug)]
struct Cut {
    /// Ring position of the boundary.
    ring: u64,
    target: usize,
    /// Voice frame of the boundary (the cut's id on the link).
    vpos: u64,
    uses_exit: bool,
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
    /// How seeks reach the probe's timeline.
    seeking: Seeking,
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
    /// Entry and voice frame the cursor last started from without a gap (start or a cut); voice
    /// frames of later loop points follow from the table.
    origin: (usize, u64),
    /// Ring position minus voice frame, for the frames written since `origin`: 0 from the start;
    /// after a cut the exit buffer's frames come before the ring's, so it can be negative.
    offset: i64,
    /// Frames per loop exit buffer, at the device rate.
    exit_frames: usize,
    /// Per entry: the exit buffer for leaving a loop into it.
    exit_state: Vec<ExitState>,
    /// Per entry: every exit a devamp of this loop can need is decided (built or refused).
    closure_ready: Vec<bool>,
    /// Entries before this one have their exits decided or can no longer be devamped.
    closure_from: usize,
    /// The same for each entry's own exit only.
    direct_from: usize,
    exit_cap: usize,
    exit_used: usize,
    exit_wip: Option<ExitWip>,
    /// Scratch for the exit closure walk.
    work: Vec<usize>,
    seen: Vec<bool>,
    cache: Option<LoopCache>,
    cache_cap: usize,
    budget: Arc<Budget>,
    exit_budget: Arc<Budget>,
    cached_frames: u64,
    step_frames: u64,
    handled_cut: Option<u64>,
    cut: Option<Cut>,
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
            let src = open_source(media, self.seeking.clone())?;
            if src.channels() != usize::from(self.info.channels)
                || src.sample_rate() != self.info.sample_rate
            {
                return Err(MediaError::Changed);
            }
            self.pipe = Some(Pipeline::new(src, self.info.frames, self.device_rate)?);
        }
        self.check_cut()?;
        // An exit buffer under way is finished first: it has the decoder's position.
        if self.exit_wip.is_some() {
            return self.build_exit_step();
        }
        // The first chunk goes into the ring before any exit is built, so a cue primes as fast
        // as one without loops. From then on no frame of a loop is written before its own exit
        // is decided (loops within the first chunk are caught up on first), so a devamp of it
        // finds the exit ready. The exits of loops that an exit buffer reaches are built
        // whenever the ring is at least two chunks ahead of the voice, so building them can
        // never starve the ring.
        let at_end = self.entry >= self.slices.len();
        if self.written >= CHUNK_FRAMES as u64 || at_end {
            if let Some(target) = self.direct_exit() {
                self.start_exit(target)?;
                return Ok(Progress::Worked);
            }
            let ch = usize::from(self.info.channels).max(1);
            let cap = self.producer.buffer().capacity() / ch;
            let lead = cap.saturating_sub(self.producer.slots() / ch);
            if (at_end || lead >= (2 * CHUNK_FRAMES).min(cap / 2))
                && let Some(target) = self.next_exit()
            {
                self.start_exit(target)?;
                return Ok(Progress::Worked);
            }
        }
        let Some(s) = self.slices.get(self.entry).copied() else {
            // Stay until the voice lets go of the ring: a devamp may still need a cut.
            if !self.shared.lock().finished {
                self.shared.update(|st| {
                    st.finished = true;
                    st.primed = true;
                });
            }
            return self.idle();
        };
        if self.cache.as_ref().is_some_and(|c| c.entry != self.entry) {
            self.drop_cache();
        }
        if self.cache.as_ref().is_some_and(|c| c.complete) {
            return self.write_cached(s);
        }
        let Some(pipe) = self.pipe.as_mut() else {
            return Err(MediaError::Worker);
        };
        let ch = pipe.channels();
        let free = self.producer.slots() / ch;
        let mut n = free
            .min(CHUNK_FRAMES)
            .min(usize::try_from(s.frames - self.off).unwrap_or(usize::MAX));
        if let Some(c) = self.cut {
            n = n.min(usize::try_from(c.ring - self.written).unwrap_or(usize::MAX));
        }
        // Wait for a worthwhile gap unless a short tail or a cut point limits the write.
        let worthwhile = CHUNK_FRAMES.min(self.producer.buffer().capacity() / ch / 4);
        if n == 0 || (free < worthwhile && n == free) {
            return self.idle();
        }
        let at = s.start + self.off;
        if pipe.position() != at {
            pipe.seek(at)?;
        }
        // A long seek continues over several steps, between the other streams' turns.
        if !pipe.prepare(self.step_frames)? {
            return Ok(Progress::Worked);
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
        self.record(s, ch, n);
        self.written += n as u64;
        self.advance_cursor(s, n as u64);
        self.after_write();
        Ok(Progress::Worked)
    }

    /// Records the `n` frames just written from `scratch` into the loop cache: a play of a loop
    /// entry that has more plays to come, from its first frame, if it fits the caps.
    fn record(&mut self, s: Slice, ch: usize, n: usize) {
        let more = match s.plays {
            Plays::Infinite => true,
            Plays::Times(k) => self.play.saturating_add(1) < k,
        };
        if self.off == 0 && self.cache.is_none() && more {
            self.start_cache(s, ch);
        }
        let off = usize::try_from(self.off).unwrap_or(usize::MAX);
        let Some(c) = self.cache.as_mut() else {
            return;
        };
        if c.complete {
            return;
        }
        if c.samples.len() != off.saturating_mul(ch) {
            // Not one unbroken play (cannot happen: a cut drops the cache); of no use.
            self.drop_cache();
            return;
        }
        // Capacity was reserved for the whole play: this never reallocates.
        if let Some(buf) = self.scratch.get(..n * ch) {
            c.samples.extend_from_slice(buf);
        }
        c.complete = c.samples.len() as u64 == s.frames.saturating_mul(ch as u64);
    }

    /// Starts a loop cache for entry `self.entry` if one play fits the per-stream cap and the
    /// global budget, and the memory can be had. Otherwise its plays are decoded as before.
    fn start_cache(&mut self, s: Slice, ch: usize) {
        let Some(len) = usize::try_from(s.frames)
            .ok()
            .and_then(|f| f.checked_mul(ch))
        else {
            return;
        };
        let bytes = len.saturating_mul(SAMPLE_BYTES);
        if bytes > self.cache_cap {
            return;
        }
        let Some(budget) = Reservation::take(&self.budget, bytes) else {
            return;
        };
        let mut samples = Vec::new();
        if samples.try_reserve_exact(len).is_err() {
            return;
        }
        self.cache = Some(LoopCache {
            entry: self.entry,
            samples,
            complete: false,
            _budget: budget,
        });
        self.publish_cache_bytes();
    }

    fn drop_cache(&mut self) {
        if self.cache.take().is_some() {
            self.publish_cache_bytes();
        }
    }

    fn publish_cache_bytes(&self) {
        let bytes = self.exit_used.saturating_add(
            self.cache
                .as_ref()
                .map_or(0, |c| c.samples.capacity().saturating_mul(SAMPLE_BYTES)),
        );
        self.shared.update(|st| st.cache_bytes = bytes);
    }

    /// Writes the next frames of the current loop entry from its cache: several plays in one
    /// step if the loop is short, up to one chunk, the end of the entry or a cut point.
    fn write_cached(&mut self, s: Slice) -> Result<Progress, MediaError> {
        let ch = usize::from(self.info.channels).max(1);
        let free = self.producer.slots() / ch;
        let in_entry = match s.plays {
            Plays::Infinite => u64::MAX,
            Plays::Times(k) => u64::from(k.saturating_sub(self.play))
                .saturating_mul(s.frames)
                .saturating_sub(self.off),
        };
        let mut n = free
            .min(CHUNK_FRAMES)
            .min(usize::try_from(in_entry).unwrap_or(usize::MAX));
        if let Some(c) = self.cut {
            n = n.min(usize::try_from(c.ring - self.written).unwrap_or(usize::MAX));
        }
        let worthwhile = CHUNK_FRAMES.min(self.producer.buffer().capacity() / ch / 4);
        if n == 0 || (free < worthwhile && n == free) {
            return self.idle();
        }
        let first = usize::try_from(self.off)
            .unwrap_or(usize::MAX)
            .saturating_mul(ch);
        let (producer, cache) = (&mut self.producer, &self.cache);
        let Some(c) = cache.as_ref() else {
            return Err(MediaError::Worker);
        };
        let head = c.samples.get(first..).ok_or(MediaError::Worker)?;
        let chunk = producer
            .write_chunk_uninit(n * ch)
            .map_err(|_| MediaError::Worker)?;
        chunk.fill_from_iter(head.iter().chain(c.samples.iter().cycle()).copied());
        self.written += n as u64;
        self.cached_frames += n as u64;
        self.advance_cursor(s, n as u64);
        self.after_write();
        Ok(Progress::Worked)
    }

    /// Moves the write cursor `n` frames on within entry `s` (never past its last play).
    fn advance_cursor(&mut self, s: Slice, n: u64) {
        let per = s.frames.max(1);
        let total = self.off + n;
        self.play = self
            .play
            .saturating_add(u32::try_from(total / per).unwrap_or(u32::MAX));
        self.off = total % per;
        if let Plays::Times(k) = s.plays
            && self.play >= k
        {
            self.entry += 1;
            self.play = 0;
            self.off = 0;
        }
    }

    /// After a ring write: carries out a cut the cursor reached and publishes progress.
    fn after_write(&mut self) {
        if let Some(c) = self.cut
            && c.ring == self.written
        {
            self.jump(c.target, c.vpos, c.uses_exit);
        }
        let (written, cached) = (self.written, self.cached_frames);
        self.shared.update(|st| {
            st.primed = true;
            st.written = written;
            st.cached_frames = cached;
        });
    }

    /// Handles a devamp cut the voice published: find the loop entry it ends and the slice to
    /// continue with, then cut now (stale frames already in the ring) or when the write cursor
    /// reaches the boundary. A cut that matches no loop of the table fails the stream, so the
    /// voice waiting for the continuation ends instead of waiting forever.
    fn check_cut(&mut self) -> Result<(), MediaError> {
        let Some((d, uses_exit)) = self.link.cut_requested() else {
            return Ok(());
        };
        if self.cut.is_some() || self.handled_cut == Some(d) {
            return Ok(());
        }
        self.handled_cut = Some(d);
        let (mut k, origin) = self.origin;
        let mut pos = i64::try_from(origin).unwrap_or(i64::MAX);
        let dv = i64::try_from(d).unwrap_or(i64::MAX);
        let found = loop {
            let Some(s) = self.slices.get(k) else {
                break None;
            };
            let end = match s.plays {
                Plays::Times(n) => pos.saturating_add(
                    i64::try_from(s.frames.saturating_mul(u64::from(n))).unwrap_or(i64::MAX),
                ),
                Plays::Infinite => i64::MAX,
            };
            if dv <= end {
                break Some((k, pos));
            }
            pos = end;
            k += 1;
        };
        // The voice only cuts at the end of a play of a loop. Anything else means the voice and
        // the stream disagree about where it is: fail loudly rather than cut somewhere else.
        let per = |k: usize| self.slices.get(k).map_or(0, |s| s.frames);
        let on_play_end = |(k, pos): (usize, i64)| {
            let per = i64::try_from(per(k)).unwrap_or(i64::MAX).max(1);
            let loops = self.slices.get(k).is_some_and(|s| {
                s.plays != Plays::Times(1)
                    || self.slices.get(k + 1).is_some_and(|n| n.index == s.index)
            });
            dv > pos && (dv - pos) % per == 0 && loops
        };
        let Some((k, _)) = found.filter(|&f| on_play_end(f)) else {
            return Err(MediaError::BadSlices);
        };
        let index = self.slices.get(k).map(|s| s.index);
        let target = (k + 1..self.slices.len())
            .find(|&i| self.slices.get(i).map(|s| s.index) != index)
            .unwrap_or(self.slices.len());
        // Ring position of the boundary: before what is written already when the voice cuts
        // out of frames it plays from an earlier exit buffer (or the ring is ahead of it).
        match u64::try_from(dv.saturating_add(self.offset)) {
            Ok(ring) if self.written < ring => {
                self.cut = Some(Cut {
                    ring,
                    target,
                    vpos: d,
                    uses_exit,
                });
            }
            _ => self.jump(target, d, uses_exit),
        }
        // After the cut (or its wait for the cursor), so a driver that waits for this renders
        // on only once the voice can follow it.
        self.shared.update(|s| s.cut_seen = Some(d));
        Ok(())
    }

    /// Continues with entry `target` from the current ring position, after the frames the
    /// voice plays from that exit's buffer (if it said it would), and tells the voice where.
    fn jump(&mut self, target: usize, vpos: u64, uses_exit: bool) {
        let e = if uses_exit {
            self.link.exit(target).map_or(0, |e| e.frames)
        } else {
            0
        };
        let (entry, play, off) = self.advance(target, e as u64);
        self.cut = None;
        self.entry = entry;
        self.play = play;
        self.off = off;
        self.origin = (target, vpos);
        let written = i64::try_from(self.written).unwrap_or(i64::MAX);
        let start = i64::try_from(vpos.saturating_add(e as u64)).unwrap_or(i64::MAX);
        self.offset = written.saturating_sub(start);
        self.drop_cache();
        self.closure_from = self.closure_from.max(target);
        self.direct_from = self.direct_from.max(target);
        self.link.resume(vpos, self.written);
        let written = self.written;
        self.shared.update(|s| {
            s.resume = Some(written);
            s.cuts += 1;
            s.finished = false;
        });
    }
}

impl Task {
    /// Cursor position `frames` into the table unrolled from entry `from`.
    fn advance(&self, from: usize, mut frames: u64) -> (usize, u32, u64) {
        let mut k = from;
        while let Some(s) = self.slices.get(k) {
            let per = s.frames.max(1);
            match s.plays {
                Plays::Infinite => {
                    return (
                        k,
                        u32::try_from(frames / per).unwrap_or(u32::MAX),
                        frames % per,
                    );
                }
                Plays::Times(n) => {
                    let all = per.saturating_mul(u64::from(n));
                    if frames < all {
                        return (
                            k,
                            u32::try_from(frames / per).unwrap_or(u32::MAX),
                            frames % per,
                        );
                    }
                    frames -= all;
                    k += 1;
                }
            }
        }
        (k, 0, 0)
    }

    /// The entry a devamp of entry `k` continues into, if `k` is a loop (plays again, or is the
    /// first play of a slice split by a start offset) with a slice after it. Matches the voice.
    fn loop_target(&self, k: usize) -> Option<usize> {
        let s = self.slices.get(k)?;
        let next_same = self.slices.get(k + 1).is_some_and(|n| n.index == s.index);
        if matches!(s.plays, Plays::Times(1)) && !next_same {
            return None;
        }
        (k + 1..self.slices.len()).find(|&i| self.slices.get(i).map(|x| x.index) != Some(s.index))
    }

    /// The entry the voice is playing, at the latest: from the ring frames it has taken so far
    /// (it takes none while it plays an exit buffer, so this lags rather than leads). Exits of
    /// loops before it can no longer be devamped.
    fn voice_entry(&self) -> usize {
        let ch = usize::from(self.info.channels).max(1);
        let cap = self.producer.buffer().capacity() / ch;
        let lead = cap.saturating_sub(self.producer.slots() / ch);
        let consumed = self.written.saturating_sub(lead as u64);
        let vframe = i64::try_from(consumed)
            .unwrap_or(i64::MAX)
            .saturating_sub(self.offset);
        let rel = vframe.saturating_sub(i64::try_from(self.origin.1).unwrap_or(i64::MAX));
        match u64::try_from(rel) {
            Ok(rel) => self.advance(self.origin.0, rel).0,
            Err(_) => self.origin.0,
        }
    }

    /// With nothing else to do: starts an exit put off for the global budget, if the budget now
    /// has room, nearest first among the loops the voice can still reach.
    fn idle(&mut self) -> Result<Progress, MediaError> {
        let ch = usize::from(self.info.channels).max(1);
        let bytes = self
            .exit_frames
            .saturating_mul(ch)
            .saturating_mul(SAMPLE_BYTES);
        if self.exit_budget.room() >= bytes {
            let from = self.voice_entry();
            let starved = (from..self.exit_state.len())
                .find(|&t| self.exit_state.get(t) == Some(&ExitState::Starved));
            if let Some(t) = starved {
                self.start_exit(t)?;
                if self.exit_wip.is_some() {
                    return Ok(Progress::Worked);
                }
            }
        }
        Ok(Progress::Idle)
    }

    /// The undecided exit of the nearest loop from the latest cut's target up to the write
    /// cursor: the voice can reach that loop through the ring.
    fn direct_exit(&mut self) -> Option<usize> {
        let upto = self.entry.min(self.slices.len().saturating_sub(1));
        while self.direct_from <= upto {
            if let Some(t) = self.loop_target(self.direct_from)
                && self.exit_state.get(t) == Some(&ExitState::Unknown)
            {
                return Some(t);
            }
            self.direct_from += 1;
        }
        None
    }

    /// The next exit to build for the entries the voice can be in (from the latest cut's target
    /// up to the write cursor), nearest loop first.
    fn next_exit(&mut self) -> Option<usize> {
        let upto = self.entry.min(self.slices.len().saturating_sub(1));
        while self.closure_from <= upto {
            if let Some(t) = self.exit_needed(self.closure_from) {
                return Some(t);
            }
            self.closure_from += 1;
        }
        None
    }

    /// The nearest undecided exit a devamp of entry `k` can need: its own, and those of the
    /// loops its exit buffer reaches (the voice can be in them, and devamp them, before the ring
    /// has any of their frames). `None` once all are decided. Nearest first, so a cap keeps the
    /// exits the voice reaches first.
    fn exit_needed(&mut self, k: usize) -> Option<usize> {
        if self.closure_ready.get(k) != Some(&false) {
            return None;
        }
        self.seen.fill(false);
        self.work.clear();
        self.work.extend(self.loop_target(k));
        let mut want: Option<usize> = None;
        while let Some(t) = self.work.pop() {
            match self.seen.get_mut(t) {
                Some(seen) if !*seen => *seen = true,
                _ => continue,
            }
            if self.exit_state.get(t) == Some(&ExitState::Unknown) {
                want = Some(want.map_or(t, |w| w.min(t)));
            }
            let mut left = self.exit_frames as u64;
            let mut j = t;
            while left > 0
                && let Some(s) = self.slices.get(j).copied()
            {
                if let Some(next) = self.loop_target(j) {
                    self.work.push(next);
                }
                match s.plays {
                    Plays::Infinite => break,
                    Plays::Times(n) => {
                        left = left.saturating_sub(s.frames.saturating_mul(u64::from(n)));
                        j += 1;
                    }
                }
            }
        }
        if want.is_none()
            && let Some(r) = self.closure_ready.get_mut(k)
        {
            *r = true;
        }
        want
    }

    /// Starts the exit buffer for entry `target`, or refuses it when it would pass the
    /// per-stream cap or the memory cannot be had, or puts it off while the global exit budget
    /// is full.
    fn start_exit(&mut self, target: usize) -> Result<(), MediaError> {
        let ch = self.pipe.as_ref().ok_or(MediaError::Worker)?.channels();
        let want = self.exit_frames.saturating_mul(ch);
        let bytes = want.saturating_mul(SAMPLE_BYTES);
        let fits = want > 0 && self.exit_used.saturating_add(bytes) <= self.exit_cap;
        let refuse = |task: &mut Self, state| {
            if let Some(st) = task.exit_state.get_mut(target) {
                *st = state;
            }
        };
        if !fits {
            refuse(self, ExitState::Refused);
            return Ok(());
        }
        let Some(budget) = Reservation::take(&self.exit_budget, bytes) else {
            refuse(self, ExitState::Starved);
            return Ok(());
        };
        let mut samples = Vec::new();
        if samples.try_reserve_exact(want).is_err() {
            refuse(self, ExitState::Refused);
            return Ok(());
        }
        self.exit_used += bytes;
        self.exit_wip = Some(ExitWip {
            target,
            samples,
            entry: target,
            play: 0,
            off: 0,
            play0: Some(0),
            budget,
        });
        self.publish_cache_bytes();
        Ok(())
    }

    /// One bounded step of decoding an exit buffer (the first `exit_frames` after leaving a
    /// loop into its target): at most one chunk, or `step_frames` of seek decoding. Published
    /// on the link when complete.
    fn build_exit_step(&mut self) -> Result<Progress, MediaError> {
        let Some(mut wip) = self.exit_wip.take() else {
            return Ok(Progress::Idle);
        };
        let Some(pipe) = self.pipe.as_mut() else {
            return Err(MediaError::Worker);
        };
        let ch = pipe.channels();
        let want = self.exit_frames * ch;
        let slice = self.slices.get(wip.entry).copied();
        let Some(s) = slice.filter(|_| wip.samples.len() < want) else {
            let target = wip.target;
            self.link.set_exit(
                target,
                Exit {
                    frames: wip.samples.len() / ch,
                    samples: wip.samples.into_boxed_slice(),
                    _budget: wip.budget,
                },
            );
            if let Some(st) = self.exit_state.get_mut(target) {
                *st = ExitState::Ready;
            }
            return Ok(Progress::Worked);
        };
        let n = CHUNK_FRAMES
            .min(usize::try_from(s.frames - wip.off).unwrap_or(usize::MAX))
            .min((want - wip.samples.len()) / ch);
        let per = usize::try_from(s.frames)
            .unwrap_or(usize::MAX)
            .saturating_mul(ch);
        let off = usize::try_from(wip.off).unwrap_or(usize::MAX) * ch;
        match wip.play0 {
            // A later play of a loop whose first play is in the buffer: copy it.
            Some(p0) if wip.play > 0 && p0.saturating_add(per) <= wip.samples.len() => {
                let from = p0 + off;
                wip.samples.extend_from_within(from..from + n * ch);
            }
            _ => {
                let at = s.start + wip.off;
                if pipe.position() != at {
                    pipe.seek(at)?;
                }
                if !pipe.prepare(self.step_frames)? {
                    self.exit_wip = Some(wip);
                    return Ok(Progress::Worked);
                }
                let buf = self.scratch.get_mut(..n * ch).ok_or(MediaError::Worker)?;
                if pipe.read(buf)? < n {
                    return Err(MediaError::Changed);
                }
                wip.samples.extend_from_slice(buf);
            }
        }
        wip.off += n as u64;
        if wip.off == s.frames {
            wip.off = 0;
            wip.play = wip.play.saturating_add(1);
            if let Plays::Times(m) = s.plays
                && wip.play >= m
            {
                wip.entry += 1;
                wip.play = 0;
                wip.play0 = Some(wip.samples.len());
            }
        }
        self.exit_wip = Some(wip);
        Ok(Progress::Worked)
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
            seeking: Seeking::Direct,
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
                seeking: Seeking::Direct,
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
                seeking: Seeking::Direct,
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
        p.link.request_cut(5_000, false);
        let r = p.handle.wait_cuts(1, Duration::from_secs(10));
        assert_eq!(r, Err(MediaError::BadSlices));
        // The stream let go of the ring, so the voice playing it ends.
        assert!(p.source.is_abandoned());
    }

    /// Review H-R1: a seek that decodes a long way (here a damaged file seeking by decoding from
    /// its start) is split across worker steps, so the other streams on the worker get their
    /// turns while it runs. Steps are driven by hand: no threads, no clocks.
    #[test]
    fn a_long_seek_never_holds_up_the_other_streams_on_its_worker() {
        let mut c = StreamerConfig::new(44_100);
        c.step_frames = 4_096;
        let d = DiskStreamer::new(c).unwrap();
        let (tx, _rx) = mpsc::channel();
        let flac =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tone.flac");
        let info = crate::decode::probe(&flac).unwrap().info;
        let one = |range: std::ops::Range<u64>| {
            vec![Slice {
                index: 0,
                start: range.start,
                frames: range.end - range.start,
                plays: Plays::Times(1),
            }]
        };
        // B starts 20000 frames in and has to decode all of them first.
        let (mut b, pb) = d
            .task(
                StreamRequest {
                    media: Media::Path(flac),
                    info,
                    seeking: Seeking::FromStart,
                    slices: one(20_000..22_050),
                },
                tx.clone(),
            )
            .unwrap();
        let pre = crate::decode::Preloaded::from_samples(2, 44_100, vec![0.25; 2 * 10_000]);
        let (mut a, pa) = d
            .task(
                StreamRequest {
                    media: Media::Preloaded(pre.clone()),
                    info: pre.info(),
                    seeking: Seeking::Direct,
                    slices: one(0..10_000),
                },
                tx,
            )
            .unwrap();
        // Round-robin, as the worker does: B, A, B, A, ...
        let mut b_steps = 0;
        while !pb.handle.is_primed() {
            assert!(
                matches!(b.step(), Progress::Worked),
                "{:?}",
                pb.handle.error()
            );
            b_steps += 1;
            assert!(matches!(a.step(), Progress::Worked | Progress::Idle));
            assert!(b_steps < 100, "B never got there");
        }
        // A was served from its first turn, long before B reached its target.
        assert!(pa.handle.frames_written() >= 4_096);
        // Open, then at least 20000 / 4096 bounded seek steps.
        assert!(b_steps >= 5, "B's seek took only {b_steps} steps");
        assert_eq!(pb.handle.error(), None);
    }

    #[test]
    fn stream_limit_is_enforced_and_released() {
        let mut c = StreamerConfig::new(48_000);
        c.max_streams = 1;
        let mut d = DiskStreamer::new(c).unwrap();
        let pre = crate::decode::Preloaded::from_samples(1, 48_000, vec![0.25; 100]);
        let info = pre.info();
        let req = |media| StreamRequest {
            media,
            info,
            seeking: Seeking::Direct,
            slices: vec![Slice {
                index: 0,
                start: 0,
                frames: 100,
                plays: Plays::Times(1),
            }],
        };
        // A healthy stream holds its slot until its handle and ring are dropped.
        let a = d.prepare(req(Media::Preloaded(pre.clone()))).unwrap();
        assert!(
            a.handle
                .wait_written(u64::MAX, Duration::from_secs(10))
                .unwrap()
        );
        assert_eq!(
            d.prepare(req(Media::Preloaded(pre.clone()))).err(),
            Some(MediaError::TooManyStreams)
        );
        drop(a);
        let deadline = Instant::now() + Duration::from_secs(10);
        while d.open_streams() != 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        // A failed stream releases its slot too.
        let b = d
            .prepare(req(Media::Path("/nonexistent/x.wav".into())))
            .unwrap();
        assert!(b.handle.wait_primed(Duration::from_secs(10)).is_err());
        let deadline = Instant::now() + Duration::from_secs(10);
        while d.open_streams() != 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(d.prepare(req(Media::Preloaded(pre))).is_ok());
    }

    fn mark(i: u64) -> f32 {
        ((i % 60_000) + 1) as f32 / 65_536.0
    }

    /// A worker task for `pre` (driven by hand: no threads, no clocks) and its stream.
    fn hand_task(
        cfg: StreamerConfig,
        pre: Arc<crate::decode::Preloaded>,
        slices: Vec<Slice>,
    ) -> (DiskStreamer, Box<Task>, PreparedStream) {
        let d = DiskStreamer::new(cfg).unwrap();
        let (tx, _rx) = mpsc::channel();
        let (t, p) = d
            .task(
                StreamRequest {
                    media: Media::Preloaded(pre.clone()),
                    info: pre.info(),
                    seeking: Seeking::Direct,
                    slices,
                },
                tx,
            )
            .unwrap();
        (d, t, p)
    }

    /// Steps the task until it has nothing to do (bounded).
    fn settle(t: &mut Task) {
        for _ in 0..10_000 {
            match t.step() {
                Progress::Worked => {}
                Progress::Idle => return,
                Progress::Done | Progress::Failed(_) => panic!("stream ended"),
            }
        }
        panic!("never settled");
    }

    fn mixer() -> (crate::MixerController, crate::OfflineRenderer) {
        let (ctl, m) = crate::Mixer::new(crate::MixerConfig {
            output_channels: 1,
            max_block_frames: 256,
            max_voices: 4,
            ..crate::MixerConfig::default()
        })
        .unwrap();
        (ctl, crate::OfflineRenderer::new(m))
    }

    fn play(ctl: &mut crate::MixerController, p: PreparedStream) -> StreamHandle {
        let (mut v, h) = p.into_voice(VoiceId(1), 1);
        v.set_crosspoint(0, 0, 1.0);
        ctl.send(0, crate::CommandKind::StartVoice(Box::new(v)));
        h
    }

    /// Renders up to voice frame `to` in 64-frame blocks; with `worker`, steps it until idle
    /// before each block, as a disk thread that always keeps up.
    fn render_to(
        r: &mut crate::OfflineRenderer,
        out: &mut Vec<f32>,
        to: usize,
        mut worker: Option<&mut Task>,
    ) {
        while out.len() < to {
            if let Some(t) = worker.as_deref_mut() {
                settle(t);
            }
            let n = 64.min(to - out.len());
            let mut buf = vec![0.0; n];
            crate::no_alloc_for_tests(|| r.render_into(&mut buf, &[n]));
            out.extend(buf);
        }
    }

    fn devamp(ctl: &mut crate::MixerController, at: u64) {
        ctl.send(
            at,
            crate::CommandKind::Devamp {
                voice: VoiceId(1),
                stop: false,
            },
        );
    }

    /// Review H1: a cut that is not on the end of a play of a loop (the voice and the stream
    /// would disagree about where it is) fails the stream instead of cutting elsewhere.
    #[test]
    fn a_cut_off_a_loop_boundary_fails_the_stream() {
        let pre = crate::decode::Preloaded::from_samples(1, 48_000, (0..3_000).map(mark).collect());
        let slices = || {
            crate::plan(
                0..3_000,
                &[1_000, 2_000],
                &[Plays::Times(1), Plays::Infinite, Plays::Times(1)],
                0,
            )
            .unwrap()
        };
        // Mid-play of the infinite loop; the end of the intro (not a loop); a play end: fine.
        for (cut, ok) in [(2_500, false), (1_000, false), (4_000, true)] {
            let (_d, mut t, p) = hand_task(StreamerConfig::new(48_000), pre.clone(), slices());
            settle(&mut t);
            p.link.request_cut(cut, false);
            let r = t.step();
            assert_eq!(
                matches!(r, Progress::Failed(MediaError::BadSlices)),
                !ok,
                "cut {cut}"
            );
        }
    }

    /// L-R2: the first ring write comes before any exit is built, so a cue that starts in a loop
    /// with many exits primes in one step. Exits stay within the per-stream cap (64 channels:
    /// 3 MB each, 10 of the 40 fit in 32 MiB); the rest are refused, and no frame of a loop is
    /// written past the first chunk before its exits are decided.
    #[test]
    fn exits_are_built_after_priming_and_kept_within_the_cap() {
        const CH: usize = 64;
        const LEN: u64 = 480;
        const PAIRS: u64 = 40;
        let frames = PAIRS * 2 * LEN + 2_000;
        let pre = crate::decode::Preloaded::from_samples(
            CH as u16,
            48_000,
            vec![0.25; frames as usize * CH],
        );
        let marks: Vec<u64> = (1..=2 * PAIRS).map(|k| k * LEN).collect();
        let plays: Vec<Plays> = (0..=2 * PAIRS)
            .map(|k| {
                if k % 2 == 0 && k < 2 * PAIRS {
                    Plays::Times(2)
                } else {
                    Plays::Times(1)
                }
            })
            .collect();
        let slices = crate::plan(0..frames, &marks, &plays, 0).unwrap();
        let (d, mut t, p) = hand_task(StreamerConfig::new(48_000), pre, slices);
        assert!(matches!(t.step(), Progress::Worked));
        assert!(p.handle.is_primed(), "primed in the first step");
        assert!(p.link.exit(1).is_none());
        let exit = 12_000 * CH * 4;
        let loop_cache = LEN as usize * CH * 4;
        let mut steps = 0;
        loop {
            let before = t.written;
            match t.step() {
                Progress::Worked => {}
                Progress::Idle => break,
                _ => panic!("{:?}", p.handle.error()),
            }
            steps += 1;
            assert!(steps < 100_000);
            assert!(t.exit_used <= DEFAULT_EXIT_BYTES);
            assert!(d.cache_bytes() <= DEFAULT_EXIT_BYTES + loop_cache);
            // A write past the first chunk inside a loop: its exits were decided first.
            if t.written > before && before >= CHUNK_FRAMES as u64 {
                let k = if t.off == 0 && t.play == 0 {
                    t.entry.saturating_sub(1)
                } else {
                    t.entry
                };
                if let Some(target) = t.loop_target(k) {
                    assert_ne!(t.exit_state[target], ExitState::Unknown, "entry {k}");
                }
            }
        }
        assert!(p.handle.is_finished());
        let ready = t
            .exit_state
            .iter()
            .filter(|s| **s == ExitState::Ready)
            .count();
        let refused = t
            .exit_state
            .iter()
            .filter(|s| **s == ExitState::Refused)
            .count();
        assert_eq!((ready, refused), (DEFAULT_EXIT_BYTES / exit, 40 - ready));
        assert_eq!(t.exit_used, ready * exit);
        assert!(
            (1..2 * PAIRS as usize)
                .step_by(2)
                .take(ready)
                .all(|k| p.link.exit(k).is_some())
        );
        assert!(p.link.exit(2 * PAIRS as usize - 1).is_none());
        // The memory goes back with the stream (the link holds the exits).
        drop((t, p));
        assert_eq!(d.cache_bytes(), 0);
    }

    /// M3.17 at the worker: with the loop cache, even a one-frame loop (which `plan` accepts;
    /// `plan_at_rate` does not) is written a whole chunk per step, not a seek per play.
    #[test]
    fn a_one_frame_loop_fills_the_ring_in_whole_chunks() {
        let pre = crate::decode::Preloaded::from_samples(2, 48_000, (0..2_000).map(mark).collect());
        let plays = [Plays::Times(1), Plays::Infinite, Plays::Times(1)];
        let slices = crate::plan(0..1_000, &[100, 101], &plays, 0).unwrap();
        let (_d, mut t, p) = hand_task(StreamerConfig::new(48_000), pre, slices);
        let mut steps = 0;
        while !matches!(t.step(), Progress::Idle) {
            steps += 1;
            assert!(steps < 100, "{steps} steps");
        }
        // 96000 frames: the intro, one decoded play, the rest from the cache.
        assert!(t.written > 96_000 - 4_096, "{}", t.written);
        assert_eq!(p.handle.cached_frames(), t.written - 101);
    }

    /// The exit flag travels with the cut: a devamp accepted before its exit buffer was built
    /// continues from the ring, even though the exit is published before the voice reaches the
    /// boundary.
    #[test]
    fn a_devamp_before_its_exit_is_built_continues_from_the_ring() {
        let pre = crate::decode::Preloaded::from_samples(1, 48_000, (0..3_000).map(mark).collect());
        let plays = [Plays::Infinite, Plays::Times(1)];
        let slices = crate::plan(0..3_000, &[1_000], &plays, 0).unwrap();
        let (_d, mut t, p) = hand_task(StreamerConfig::new(48_000), pre, slices);
        let link = Arc::clone(&p.link);
        // Open and prime (one play), a chunk from the loop cache, then the exit is started.
        for _ in 0..3 {
            assert!(matches!(t.step(), Progress::Worked));
        }
        assert!(t.exit_wip.is_some() && link.exit(1).is_none());
        let (mut ctl, mut r) = mixer();
        let h = play(&mut ctl, p);
        let mut out = Vec::new();
        render_to(&mut r, &mut out, 100, None);
        devamp(&mut ctl, 100);
        render_to(&mut r, &mut out, 164, None);
        assert_eq!(link.cut_requested(), Some((1_000, false)));
        // The cut is handled first; the exit is published right after it.
        assert!(matches!(t.step(), Progress::Worked));
        assert_eq!(h.cuts(), 1);
        settle(&mut t);
        assert!(link.exit(1).is_some());
        render_to(&mut r, &mut out, 3_200, Some(&mut t));
        let want: Vec<f32> = (0..3_000).map(mark).collect();
        assert_eq!(&out[..3_000], &want[..]);
        assert!(out[3_000..].iter().all(|&s| s == 0.0));
        assert_eq!(ctl.underrun_frames(), 0);
    }

    /// A driver that renders faster than the worker polls waits for it to see the cut before
    /// rendering on (the offline render; flaked `devamp_leaves_the_loop_and_the_cue_ends`).
    #[test]
    fn a_driver_can_wait_for_the_worker_to_see_a_cut() {
        let pre = crate::decode::Preloaded::from_samples(1, 48_000, (0..3_000).map(mark).collect());
        let plays = [Plays::Infinite, Plays::Times(1)];
        let slices = crate::plan(0..3_000, &[1_000], &plays, 0).unwrap();
        let (_d, mut t, p) = hand_task(StreamerConfig::new(48_000), pre, slices);
        for _ in 0..3 {
            assert!(matches!(t.step(), Progress::Worked));
        }
        let (mut ctl, mut r) = mixer();
        let h = play(&mut ctl, p);
        let none = Duration::ZERO;
        let mut out = Vec::new();
        render_to(&mut r, &mut out, 100, None);
        // No cut asked yet: nothing to wait for.
        assert_eq!(h.wait_cut_seen(none), Ok(true));
        devamp(&mut ctl, 100);
        render_to(&mut r, &mut out, 164, None);
        // Asked, and the worker has not polled: rendering on would outrun it.
        assert_eq!(h.wait_cut_seen(none), Ok(false));
        assert!(matches!(t.step(), Progress::Worked));
        assert_eq!(h.wait_cut_seen(none), Ok(true));
        assert_eq!(h.cuts(), 1);
    }

    /// L-R5: a devamp while the voice still waits for the disk's answer to the previous cut (it
    /// plays that exit's buffer meanwhile) is kept and its cut sent once the answer is in:
    /// before its boundary, and after it (the voice then plays the next exit buffer).
    /// Intro 0..1000, vamp A, a 100-frame bridge, vamp C, outro D; one play of each vamp.
    #[test]
    fn a_devamp_while_the_previous_cut_is_unanswered_is_deferred() {
        let pre = crate::decode::Preloaded::from_samples(1, 48_000, (0..6_000).map(mark).collect());
        let (once, inf) = (Plays::Times(1), Plays::Infinite);
        let slices = crate::plan(
            0..6_000,
            &[1_000, 2_000, 2_100, 3_100],
            &[once, inf, once, inf, once],
            0,
        )
        .unwrap();
        // 20 ms exits: B's (2000..2960) ends inside C, so C's boundary comes from the ring;
        // 250 ms: C's boundary (3100) comes while B's exit still plays.
        for (exit_ms, passed) in [(20, false), (250, false), (250, true)] {
            let mut cfg = StreamerConfig::new(48_000);
            cfg.exit_ms = exit_ms;
            let (_d, mut t, p) = hand_task(cfg, pre.clone(), slices.clone());
            let link = Arc::clone(&p.link);
            settle(&mut t);
            let (mut ctl, mut r) = mixer();
            let h = play(&mut ctl, p);
            let mut out = Vec::new();
            render_to(&mut r, &mut out, 1_900, Some(&mut t));
            devamp(&mut ctl, 1_900);
            // Past A's boundary with the worker held: the voice plays B's exit buffer.
            render_to(&mut r, &mut out, 2_300, None);
            assert_eq!(link.cut_requested(), Some((2_000, true)));
            assert_eq!(h.cuts(), 0);
            devamp(&mut ctl, 2_300);
            render_to(&mut r, &mut out, if passed { 3_200 } else { 2_364 }, None);
            // Still the first cut: the second waits for its answer.
            assert_eq!(
                link.cut_requested(),
                Some((2_000, true)),
                "{exit_ms} {passed}"
            );
            render_to(&mut r, &mut out, 6_200, Some(&mut t));
            assert_eq!(
                link.cut_requested(),
                Some((3_100, true)),
                "{exit_ms} {passed}"
            );
            assert_eq!(h.cuts(), 2);
            let want: Vec<f32> = (0..6_000).map(mark).collect();
            assert_eq!(&out[..6_000], &want[..], "{exit_ms} {passed}");
            assert!(out[6_000..].iter().all(|&s| s == 0.0));
            assert_eq!(ctl.underrun_frames(), 0, "{exit_ms} {passed}");
            let ev: Vec<crate::Event> = std::iter::from_fn(|| ctl.poll_event()).collect();
            for at in [2_000, 3_100] {
                assert!(
                    ev.contains(&crate::Event::DevampScheduled {
                        voice: VoiceId(1),
                        at
                    }),
                    "{ev:?}"
                );
            }
            assert!(ev.contains(&crate::Event::VoiceFinished {
                voice: VoiceId(1),
                at: 6_000,
                reason: crate::FinishReason::OutPoint
            }));
        }
    }

    /// Reference model: the frames a voice plays for `slices` (frame `i` of the source holds
    /// `mark(i)`), leaving the loop under way at the end of the play that ends on each voice
    /// frame in `leaves` (`true`: ending there), `n` frames, silence after the end. Panics if a
    /// leave is not on the end of a loop's play.
    fn model(slices: &[Slice], leaves: &[(u64, bool)], n: usize) -> Vec<f32> {
        let mut out = Vec::new();
        let (mut entry, mut play, mut off, mut t) = (0usize, 0u32, 0u64, 0u64);
        let mut used = 0;
        'play: while out.len() < n {
            let Some(s) = slices.get(entry) else { break };
            out.push(mark(s.start + off));
            off += 1;
            t += 1;
            if off < s.frames {
                continue;
            }
            off = 0;
            if let Some(&(_, stop)) = leaves.iter().find(|(at, _)| *at == t) {
                used += 1;
                if stop {
                    break 'play;
                }
                let index = s.index;
                entry = (entry + 1..slices.len())
                    .find(|&i| slices[i].index != index)
                    .unwrap_or(slices.len());
                play = 0;
                continue;
            }
            play += 1;
            if let Plays::Times(k) = s.plays
                && play >= k
            {
                entry += 1;
                play = 0;
            }
        }
        let due = leaves.iter().filter(|(at, _)| *at <= t).count();
        assert_eq!(used, due, "a devamp boundary is not a play end: {leaves:?}");
        out.resize(n, 0.0);
        out
    }

    /// Equal, or the first frame that differs.
    fn same(out: &[f32], want: &[f32]) -> Result<(), String> {
        match out.iter().zip(want).position(|(a, b)| a != b) {
            None if out.len() == want.len() => Ok(()),
            None => Err(format!("lengths {} {}", out.len(), want.len())),
            Some(i) => Err(format!(
                "frame {i}: {} (source {}) want {}",
                out[i],
                out[i] * 65_536.0 - 1.0,
                want[i] * 65_536.0 - 1.0
            )),
        }
    }

    /// Plays `slices` from a hand-stepped worker, sending a devamp (stop?) at the start of the
    /// blocks in `devamps` and holding the worker for the blocks in `held` (64-frame block
    /// indices); otherwise the worker settles before each block. Returns the output and the
    /// leaves the mixer announced, for `model`.
    fn drive(
        slices: &[Slice],
        exit_ms: u32,
        devamps: &[(usize, bool)],
        held: &dyn Fn(usize) -> bool,
        n: usize,
    ) -> (Vec<f32>, Vec<(u64, bool)>, crate::MixerController) {
        let frames = slices.iter().map(|s| s.start + s.frames).max().unwrap();
        let pre =
            crate::decode::Preloaded::from_samples(1, 48_000, (0..frames).map(mark).collect());
        let mut cfg = StreamerConfig::new(48_000);
        cfg.exit_ms = exit_ms;
        cfg.buffer_ms = 200;
        let (_d, mut t, p) = hand_task(cfg, pre, slices.to_vec());
        settle(&mut t);
        let (mut ctl, mut r) = mixer();
        let _h = play(&mut ctl, p);
        let mut out = Vec::new();
        let mut leaves: Vec<(u64, bool)> = Vec::new();
        let mut block = 0;
        let mut ended = false;
        while out.len() < n {
            // Steps until idle; a stream whose voice has let go of the ring is done.
            for _ in 0..10_000 {
                if ended || held(block) {
                    break;
                }
                match t.step() {
                    Progress::Worked => {}
                    Progress::Idle => break,
                    Progress::Done => ended = true,
                    Progress::Failed(e) => panic!(
                        "stream failed: {e:?} cut {:?} origin {:?} {slices:?}",
                        t.link.cut_requested(),
                        t.origin
                    ),
                }
            }
            let stop = devamps.iter().find(|(b, _)| *b == block).map(|(_, s)| *s);
            if let Some(stop) = stop {
                ctl.send(
                    out.len() as u64,
                    crate::CommandKind::Devamp {
                        voice: VoiceId(1),
                        stop,
                    },
                );
            }
            let mut buf = vec![0.0; 64];
            crate::no_alloc_for_tests(|| r.render_into(&mut buf, &[64]));
            out.extend(buf);
            ctl.pump();
            while let Some(e) = ctl.poll_event() {
                if let crate::Event::DevampScheduled { at, .. } = e
                    && !leaves.iter().any(|(l, _)| *l == at)
                {
                    leaves.push((at, stop.unwrap_or(false)));
                }
            }
            block += 1;
        }
        (out, leaves, ctl)
    }

    /// Review H1: a third devamp while the second one's cut still waits to be sent (the worker
    /// has not answered the first) must not replace it: it is ignored, and the voice plays
    /// exactly what it announced. Intro | A inf | bridge | C inf | bridge | E inf | F.
    #[test]
    fn a_devamp_while_a_deferred_cut_waits_is_ignored_not_lost() {
        let (once, inf) = (Plays::Times(1), Plays::Infinite);
        let slices = crate::plan(
            0..30_000,
            &[1_000, 2_000, 2_100, 3_100, 3_200, 4_200],
            &[once, inf, once, inf, once, inf, once],
            0,
        )
        .unwrap();
        // Devamps in blocks starting 1920 (A), 2304 (C, inside B's exit), 3264 (E, after C's
        // boundary); the worker is held from the first devamp until block 60 (3840).
        let devamps = [(30, false), (36, false), (51, false)];
        let held = |b: usize| (30..60).contains(&b);
        let (out, leaves, ctl) = drive(&slices, 250, &devamps, &held, 20_000);
        assert_eq!(leaves, vec![(2_000, false), (3_100, false)]);
        same(&out, &model(&slices, &leaves, out.len())).unwrap();
        assert_eq!(ctl.underrun_frames(), 0);
    }

    /// Random tables and devamp sequences against the reference model, with the worker held
    /// for stretches shorter than an exit buffer after some devamps.
    #[test]
    fn random_devamp_sequences_play_as_the_model() {
        let mut x = 0x9e37_79b9_u32;
        let mut rnd = move |n: u32| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x % n
        };
        for case in 0..500 {
            let count = 2 + rnd(8) as usize;
            let mut marks = Vec::new();
            let mut at = 0u64;
            for _ in 0..count {
                let span = if rnd(2) == 0 { 400 } else { 1_400 };
                at += 100 + u64::from(rnd(span));
                marks.push(at);
            }
            // A long last slice, so that the ring (not only an exit buffer) plays it.
            let end = marks.pop().unwrap() + 30_000;
            let plays: Vec<Plays> = (0..count)
                .map(|_| match rnd(5) {
                    0 | 1 => Plays::Infinite,
                    2 => Plays::Times(2 + rnd(3)),
                    _ => Plays::Times(1),
                })
                .collect();
            let start = if rnd(3) == 0 { u64::from(rnd(100)) } else { 0 };
            let slices = crate::plan(0..end, &marks, &plays, start).unwrap();
            let exit_ms = if rnd(3) == 0 { 20 } else { 250 };
            // Hold at most half an exit buffer (in 64-frame blocks) after a devamp.
            let max_hold = (48 * exit_ms as usize / 64 / 2).max(1);
            let mut devamps = Vec::new();
            let mut b = 0;
            while b < 600 {
                b += 1 + rnd(12) as usize;
                devamps.push((b, rnd(30) == 0));
            }
            // Holds start anywhere (devamps fall inside them) and are at least a block apart.
            let mut holds = Vec::new();
            let mut b = 0;
            while b < 600 {
                b += 1 + rnd(30) as usize;
                let h = 1 + rnd(max_hold as u32) as usize;
                holds.push(b..b + h);
                b += h;
            }
            let held = |blk: usize| holds.iter().any(|h| h.contains(&blk));
            let (out, leaves, ctl) = drive(&slices, exit_ms, &devamps, &held, 600 * 64);
            assert_eq!(ctl.underrun_frames(), 0, "case {case}: {slices:?}");
            let want = std::panic::catch_unwind(|| model(&slices, &leaves, out.len()))
                .unwrap_or_else(|_| panic!("case {case}: {slices:?} exit {exit_ms} {devamps:?}"));
            if let Err(e) = same(&out, &want) {
                panic!("case {case}: {e}: {slices:?} exit {exit_ms} leaves {leaves:?}");
            }
        }
    }

    /// Review M2: a cue started as soon as it is primed, on a dense table (60 slices of 100 ms,
    /// every other one looping, so every exit buffer reaches further loops), with one worker
    /// step per 64-frame block: exit building never lets the ring run dry.
    #[test]
    fn a_cue_started_at_priming_on_a_dense_table_never_underruns() {
        const LEN: u64 = 4_800;
        let frames = 60 * LEN;
        let pre =
            crate::decode::Preloaded::from_samples(1, 48_000, (0..frames).map(mark).collect());
        let marks: Vec<u64> = (1..60).map(|k| k * LEN).collect();
        let plays: Vec<Plays> = (0..60)
            .map(|k| {
                if k % 2 == 0 {
                    Plays::Times(2)
                } else {
                    Plays::Times(1)
                }
            })
            .collect();
        let slices = crate::plan(0..frames, &marks, &plays, 0).unwrap();
        let (_d, mut t, p) = hand_task(StreamerConfig::new(48_000), pre, slices.clone());
        while !p.handle.is_primed() {
            assert!(matches!(t.step(), Progress::Worked));
        }
        let (mut ctl, mut r) = mixer();
        let _h = play(&mut ctl, p);
        let mut out = Vec::new();
        while out.len() < 200_000 {
            assert!(matches!(t.step(), Progress::Worked | Progress::Idle));
            let mut buf = vec![0.0; 64];
            crate::no_alloc_for_tests(|| r.render_into(&mut buf, &[64]));
            out.extend(buf);
            assert_eq!(ctl.underrun_frames(), 0, "underrun by frame {}", out.len());
        }
        same(&out, &model(&slices, &[], out.len())).unwrap();
        // Every exit was still built along the way.
        assert!(
            (1..59)
                .step_by(2)
                .all(|k| t.exit_state[k] == ExitState::Ready)
        );
    }

    /// Loops re-review L1: the retry of an exit put off for the budget skips the loops the voice
    /// has played past (their exits can no longer be needed).
    #[test]
    fn the_exit_retry_skips_loops_behind_the_voice() {
        let mut cfg = StreamerConfig::new(48_000);
        cfg.exit_ms = 20; // 960 frames
        let exit = 960 * SAMPLE_BYTES;
        cfg.exit_budget_bytes = exit; // one mono exit
        let pre = crate::decode::Preloaded::from_samples(1, 48_000, vec![0.25; 12_000]);
        // Entries: A, B (loop), C, D (loop), E; the exits of C and E are the loops' targets.
        let slices = crate::plan(
            0..12_000,
            &[1_000, 3_000, 4_000, 6_000],
            &[
                Plays::Times(1),
                Plays::Times(2),
                Plays::Times(1),
                Plays::Times(2),
                Plays::Times(1),
            ],
            0,
        )
        .unwrap();
        let (d, mut t, mut p) = hand_task(cfg, pre, slices);
        // Another stream holds the exit budget, so both exits are put off.
        let hold = Reservation::take(&d.exit_budget, exit).unwrap();
        settle(&mut t);
        assert_eq!(t.exit_state[2], ExitState::Starved);
        assert_eq!(t.exit_state[4], ExitState::Starved);
        // The voice plays into the second loop (entry 3 starts at voice frame 6000).
        for _ in 0..7_000 {
            p.source.pop().unwrap();
        }
        assert_eq!(t.voice_entry(), 3);
        drop(hold);
        settle(&mut t);
        assert_eq!(t.exit_state[4], ExitState::Ready, "the loop ahead");
        assert_eq!(t.exit_state[2], ExitState::Starved, "the loop behind");
    }

    /// Review M1: loop caches cannot crowd out exit buffers (separate budgets), and an exit
    /// refused because other streams held the exit budget is built once they let it go.
    #[test]
    fn exits_get_their_own_budget_and_are_retried_when_it_frees() {
        let exit = 12_000 * 2 * SAMPLE_BYTES;
        let mut cfg = StreamerConfig::new(48_000);
        cfg.exit_budget_bytes = exit; // one stereo exit
        cfg.cache_budget_bytes = 4_800 * 2 * SAMPLE_BYTES; // one 4800-frame loop
        let d = DiskStreamer::new(cfg).unwrap();
        let pre = crate::decode::Preloaded::from_samples(2, 48_000, vec![0.25; 2 * 20_000]);
        let slices = crate::plan(
            0..20_000,
            &[1_000, 5_800],
            &[Plays::Times(1), Plays::Infinite, Plays::Times(1)],
            0,
        )
        .unwrap();
        let task = || {
            let (tx, _rx) = mpsc::channel();
            d.task(
                StreamRequest {
                    media: Media::Preloaded(pre.clone()),
                    info: pre.info(),
                    seeking: Seeking::Direct,
                    slices: slices.clone(),
                },
                tx,
            )
            .unwrap()
        };
        // A takes the whole cache budget and the exit budget.
        let (mut a, pa) = task();
        settle(&mut a);
        assert!(pa.link.exit(2).is_some());
        assert!(pa.handle.cached_frames() > 0);
        // B: its exit does not fit now; its ring is filled all the same.
        let (mut b, pb) = task();
        settle(&mut b);
        assert!(pb.link.exit(2).is_none());
        assert!(pb.handle.frames_written() > 90_000);
        // A ends: B builds its exit while idle.
        drop((a, pa));
        settle(&mut b);
        assert!(pb.link.exit(2).is_some(), "exit not retried");
        assert_eq!(d.cache_bytes(), exit);
    }
}
