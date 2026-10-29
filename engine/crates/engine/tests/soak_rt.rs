// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stdout,
    clippy::print_stderr,
    dead_code
)]

//! The real-time soak (NFR-09, QA-05): the generated show and the seeded random operator of
//! `soak.rs`, run for minutes of WALL-CLOCK time against the real control loop
//! (`runner::run_observed` on its own thread, waking on real timers), with a stand-in device
//! that renders the mixer in a thread on a real-time cadence.
//!
//! This is the one test category allowed to read the wall clock: real wake jitter is the thing
//! it measures, and the virtual-time soak cannot. It is `#[ignore]`d and kept out of the normal
//! suite for that reason (and because it takes minutes). Run it in release:
//!
//! ```text
//! cargo test --release -p openlab-engine --test soak_rt -- --ignored --nocapture
//! ```
//!
//! `OPENLAB_SOAK_RT_SECS=<n>` sets the duration (default 120; CI's nightly run uses 180;
//! add the final checkpoint's ten seconds or so), `OPENLAB_SOAK_SEED=<n>` the operator.
//!
//! Each run first measures the host: a host whose one-block sleeps overshoot by more than a
//! block at p50 (or past the device lateness bound at p99) cannot run the stand-in device on a
//! block cadence, and the run is skipped with the reason, or fails when
//! `OPENLAB_SOAK_RT_REQUIRE` is set. GitHub's hosted macOS VMs are such hosts.
//! When a run fails, it prints what the threads measured and the stalls just before.
//!
//! The stand-in device is a thread that renders one block every block-length of wall time,
//! scheduled from a fixed start so a late block is followed by catch-up blocks. Like a host
//! with a small buffer it reports an xrun when a block is rendered more than
//! [`DEVICE_BUFFER`] after it was due; the shared checks assert the backend counts none,
//! alongside no underrun, fault or dropped event. The audio and disk threads are the real ones.
//!
//! Checked on top of what `soak.rs` checks: wake jitter of the control loop (how far past its
//! requested wait the timer woke it: p50/p99/max, bounded), the longest gap between ticks,
//! how late the stand-in device's blocks ran, the delay from sending a start to its first
//! sample, late-start warnings (bounded, not forbidden: a loaded machine may be late),
//! the inbox backlog, and the object counts (voices, streams, queues; never RSS).
//! A cue's start cannot be predicted to the sample here (the loop handles a message whenever
//! it wakes), so the heartbeat must start within a quarter second of being sent and play its
//! exact length.

mod show;
mod soak_common;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use openlab_audio::{Mixer, OfflineRenderer};
use openlab_core::SampleTime;
use openlab_core::protocol::{ClientMessage, RequestId, ServerMessage};
use openlab_engine::control::runner::{Wake, run_observed};
use openlab_engine::control::{
    Backend, Control, Inbox, OUTBOUND_CAPACITY, OutboundRx, outbound_channel, reply_channel,
};
use show::{BLOCK, RATE};
use soak_common::{Built, Counts, Driver, Soak, assert_realistic, build};

/// The wall time one block takes to play.
fn period() -> Duration {
    Duration::from_secs_f64(BLOCK as f64 / f64::from(RATE))
}

/// A block rendered later than this after it was due is an xrun, as on a host with a buffer
/// this deep. Not below `TICK_GAP_MAX`: a stall the other limits tolerate must not fail the run
/// through the xrun count.
const DEVICE_BUFFER: Duration = Duration::from_millis(250);
/// Wake jitter: the timer may overshoot a wait by this much at p99, and by this much at worst
/// (half the mixer's lookahead). A healthy machine is two orders of magnitude under both.
const WAKE_P99_MAX: Duration = Duration::from_millis(50);
const WAKE_MAX: Duration = Duration::from_millis(250);
/// The longest gap between two ticks of the control loop.
const TICK_GAP_MAX: Duration = Duration::from_millis(250);
/// How late the stand-in device's blocks may run, at p99.
const DEVICE_LATE_P99_MAX: Duration = Duration::from_millis(50);
/// A start sent now must be heard within this many frames (250 ms).
const START_SLACK_FRAMES: SampleTime = (RATE as SampleTime) / 4;
/// The block (about 3 s in) at which the stall knob freezes the stand-in device.
const STALL_AT_BLOCK: u32 = 375;
/// Messages waiting for the control loop.
const MAX_INBOX: usize = 1024;
/// How long to wait for the control loop to answer before declaring it hung.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);
/// A wake overshoot, tick gap or late block this long is listed when a run fails.
const NOTABLE: Duration = Duration::from_millis(20);
/// How far back from the failure the report lists notable events.
const REPORT_WINDOW_SECS: u64 = 5;

// ---------------------------------------------------------------------------------------------
// Numbers

/// Nearest-rank percentile of `values` (`p` in 0..=1); 0 for an empty set.
fn percentile(values: &[u32], p: f64) -> u32 {
    if values.is_empty() {
        return 0;
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    let rank = ((v.len() - 1) as f64 * p).round() as usize;
    v[rank.min(v.len() - 1)]
}

fn micros(d: Duration) -> u32 {
    u32::try_from(d.as_micros()).unwrap_or(u32::MAX)
}

/// p50, p99 and max of microsecond samples, as a line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Spread {
    n: usize,
    p50: u32,
    p99: u32,
    max: u32,
}

impl Spread {
    fn of(us: &[u32]) -> Self {
        Self {
            n: us.len(),
            p50: percentile(us, 0.5),
            p99: percentile(us, 0.99),
            max: us.iter().copied().max().unwrap_or(0),
        }
    }

    fn show(&self, what: &str) {
        println!(
            "{what}: n={} p50={:.2} ms p99={:.2} ms max={:.2} ms",
            self.n,
            f64::from(self.p50) / 1e3,
            f64::from(self.p99) / 1e3,
            f64::from(self.max) / 1e3
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Threads

type Job = Box<dyn FnOnce(&mut Control) + Send>;

#[derive(Default)]
struct Capture {
    /// Frames the device has rendered (the sample the next block starts at).
    rendered: SampleTime,
    active: bool,
    base: SampleTime,
    data: Vec<f32>,
}

#[derive(Default)]
struct Stats {
    /// Overshoot of each timed-out wait of the control loop, in microseconds.
    wake_over_us: Vec<u32>,
    ticks: u64,
    tick_gap_max: Duration,
    /// Timed-out waits that overshot by more than a block: the loop was being starved, and a
    /// start committed in such a stretch may legitimately be late.
    slow_wakes: u64,
    /// How late each block of the stand-in device began after it was due, in microseconds.
    device_late_us: Vec<u32>,
    /// Wake overshoots and tick gaps past [`NOTABLE`], with the frame rendered when they ended,
    /// for the failure report.
    notable_wakes: Vec<(SampleTime, u32)>,
    notable_gaps: Vec<(SampleTime, u32)>,
    /// The frame rendered when the control loop first saw an underrun counted.
    first_underrun: Option<SampleTime>,
}

/// What the threads share. Locks are held for a push or a copy, never across work.
#[derive(Default)]
struct Shared {
    rendered: AtomicU64,
    stop: AtomicBool,
    /// Test knob: the device thread stalls this long (ms) once, a few seconds in; 0 = never.
    stall_ms: AtomicU64,
    /// The first thing a thread found wrong, for the operator thread to fail with.
    failure: Mutex<Option<String>>,
    capture: Mutex<Capture>,
    counts: Mutex<Counts>,
    stats: Mutex<Stats>,
    jobs: Mutex<VecDeque<Job>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn fail(&self, what: String) {
        lock(&self.failure).get_or_insert(what);
    }
}

/// The stand-in device: one block of wall time per block, from a fixed start.
fn device_thread(sh: &Shared, mut renderer: OfflineRenderer, channels: usize) {
    let mut buf = vec![0.0f32; BLOCK as usize * channels];
    let start = Instant::now();
    let mut k: u32 = 0;
    while !sh.stop.load(Ordering::Acquire) {
        let due = start + period() * k;
        let now = Instant::now();
        if now < due {
            thread::sleep(due - now);
            continue;
        }
        let late = now - due;
        if k == STALL_AT_BLOCK {
            let stall = sh.stall_ms.swap(0, Ordering::AcqRel);
            if stall > 0 {
                thread::sleep(Duration::from_millis(stall));
                continue;
            }
        }
        // Late past the host's buffer is a dropout the host would report.
        if late > DEVICE_BUFFER {
            renderer.inject_xruns(1);
        }
        renderer.render_into(&mut buf, &[BLOCK as usize]);
        if let Some(i) = buf.iter().position(|x| !x.is_finite()) {
            sh.fail(format!(
                "NaN or infinity in the output at block {k} (sample {i})"
            ));
        }
        {
            let mut cap = lock(&sh.capture);
            if cap.active {
                cap.data.extend(buf.iter().step_by(channels));
            }
            cap.rendered += BLOCK;
            sh.rendered.store(cap.rendered, Ordering::Release);
        }
        lock(&sh.stats).device_late_us.push(micros(late));
        k = k.wrapping_add(1);
    }
}

/// What the control thread does after every tick: jobs for the operator, counters, jitter.
fn observer(sh: Arc<Shared>) -> impl FnMut(&mut Control, Wake) {
    let mut last: Option<Instant> = None;
    move |c, wake| {
        let now = Instant::now();
        loop {
            let job = lock(&sh.jobs).pop_front();
            match job {
                Some(job) => job(c),
                None => break,
            }
        }
        {
            let mut k = lock(&sh.counts);
            k.voices = k.voices.max(c.live_voices());
            k.streams = k.streams.max(c.open_streams());
            k.held = k.held.max(c.held_streams());
            k.deferred = k.deferred.max(c.deferred());
        }
        let underrun = c.mixer().underrun_frames() > 0;
        let at = sh.rendered.load(Ordering::Acquire);
        let mut s = lock(&sh.stats);
        s.ticks += 1;
        if underrun && s.first_underrun.is_none() {
            s.first_underrun = Some(at);
        }
        if wake.timed_out {
            let over = wake.waited.saturating_sub(wake.wanted);
            s.wake_over_us.push(micros(over));
            if over > period() {
                s.slow_wakes += 1;
            }
            if over >= NOTABLE {
                s.notable_wakes.push((at, micros(over)));
            }
        }
        if let Some(prev) = last {
            let gap = now - prev;
            s.tick_gap_max = s.tick_gap_max.max(gap);
            if gap >= NOTABLE {
                s.notable_gaps.push((at, micros(gap)));
            }
        }
        last = Some(now);
    }
}

// ---------------------------------------------------------------------------------------------
// The driver

/// What a finished run measured.
struct Summary {
    wake: Spread,
    device_late: Spread,
    ticks: u64,
    tick_gap_max: Duration,
    slow_wakes: u64,
    max_inbox: usize,
}

struct Realtime {
    sh: Arc<Shared>,
    inbox: Sender<Inbox>,
    outbound: OutboundRx,
    control: Option<JoinHandle<()>>,
    device: Option<JoinHandle<()>>,
    next_id: u64,
    steps: u64,
    max_inbox: usize,
}

/// Builds the show's control loop and its mixer; nothing runs yet.
fn start_control(
    built: &Built,
) -> (
    Control,
    OfflineRenderer,
    usize,
    Sender<Inbox>,
    Receiver<Inbox>,
    OutboundRx,
) {
    let (ctl, mixer) = Mixer::new(show::mixer_config()).unwrap();
    let channels = mixer.output_channels();
    let (inbox_tx, inbox) = crossbeam_channel::unbounded();
    let (out_tx, outbound) = outbound_channel(OUTBOUND_CAPACITY);
    let control = Control::new(
        built.ws.clone(),
        ctl,
        Backend::Offline,
        show::config(&built.ws),
        inbox_tx.clone(),
        out_tx,
    )
    .unwrap();
    (
        control,
        OfflineRenderer::new(mixer),
        channels,
        inbox_tx,
        inbox,
        outbound,
    )
}

impl Realtime {
    /// Starts the control loop, waits for its media probes to answer, then starts the device,
    /// so sample time and the run's clock begin together.
    fn start(built: &Built) -> Self {
        Self::start_stalled(built, 0)
    }

    fn start_stalled(built: &Built, stall_ms: u64) -> Self {
        let (control, renderer, channels, inbox_tx, inbox, outbound) = start_control(built);
        let sh = Arc::new(Shared::default());
        sh.stall_ms.store(stall_ms, Ordering::Release);
        let observe = observer(sh.clone());
        let control = thread::Builder::new()
            .name("soak-control".into())
            .spawn(move || run_observed(control, &inbox, observe))
            .unwrap();
        let mut me = Self {
            sh: sh.clone(),
            inbox: inbox_tx,
            outbound,
            control: Some(control),
            device: None,
            next_id: 0,
            steps: 0,
            max_inbox: 0,
        };
        let deadline = Instant::now() + ANSWER_TIMEOUT;
        while me.with(|c| c.probes_pending() + c.preloads_pending()) > 0 {
            assert!(Instant::now() < deadline, "media probes did not finish");
            thread::sleep(Duration::from_millis(2));
        }
        me.device = Some(
            thread::Builder::new()
                .name("soak-device".into())
                .spawn(move || device_thread(&sh, renderer, channels))
                .unwrap(),
        );
        me
    }

    /// Fails the test, on this thread, if another one found something wrong or died.
    fn check(&self) {
        if let Some(what) = lock(&self.sh.failure).take() {
            panic!("{what}");
        }
        for (name, t) in [("control", &self.control), ("device", &self.device)] {
            assert!(
                t.as_ref().is_none_or(|t| !t.is_finished()),
                "the {name} thread ended early"
            );
        }
    }

    /// Stops both threads and returns what they measured. Panics from them are re-raised.
    fn finish(&mut self) -> Summary {
        self.sh.stop.store(true, Ordering::Release);
        let _ = self.inbox.send(Inbox::Shutdown);
        for t in [self.control.take(), self.device.take()]
            .into_iter()
            .flatten()
        {
            if let Err(e) = t.join() {
                std::panic::resume_unwind(e);
            }
        }
        let s = lock(&self.sh.stats);
        Summary {
            wake: Spread::of(&s.wake_over_us),
            device_late: Spread::of(&s.device_late_us),
            ticks: s.ticks,
            tick_gap_max: s.tick_gap_max,
            slow_wakes: s.slow_wakes,
            max_inbox: self.max_inbox,
        }
    }
}

impl Drop for Realtime {
    fn drop(&mut self) {
        // A failed assertion must not leave a device thread rendering into the next test.
        self.sh.stop.store(true, Ordering::Release);
        let _ = self.inbox.send(Inbox::Shutdown);
        for t in [self.control.take(), self.device.take()]
            .into_iter()
            .flatten()
        {
            let _ = t.join();
        }
    }
}

impl Driver for Realtime {
    const START_SLACK: SampleTime = START_SLACK_FRAMES;

    fn send(&mut self, message: ClientMessage) -> Vec<ServerMessage> {
        let (reply, mut rx) = reply_channel();
        self.inbox
            .send(Inbox::Client { message, reply })
            .expect("the control loop is gone");
        let deadline = Instant::now() + ANSWER_TIMEOUT;
        let mut out = Vec::new();
        loop {
            while let Ok(m) = rx.try_recv() {
                let done = matches!(m, ServerMessage::Result { .. });
                out.push(m);
                if done {
                    return out;
                }
            }
            self.check();
            assert!(Instant::now() < deadline, "the control loop did not answer");
            thread::sleep(Duration::from_micros(200));
        }
    }

    fn request_id(&mut self) -> RequestId {
        self.next_id += 1;
        RequestId(self.next_id)
    }

    fn take_outbound(&mut self) -> Vec<ServerMessage> {
        std::iter::from_fn(|| self.outbound.try_recv().ok().map(|o| o.message)).collect()
    }

    /// Waits for the device to render the next block.
    fn step(&mut self) {
        self.steps += 1;
        let target = self.steps * BLOCK;
        let deadline = Instant::now() + ANSWER_TIMEOUT;
        while self.sh.rendered.load(Ordering::Acquire) < target {
            self.check();
            assert!(Instant::now() < deadline, "the device rendered no block");
            thread::sleep(Duration::from_micros(500));
        }
        self.check();
        self.max_inbox = self.max_inbox.max(self.inbox.len());
        assert!(
            self.max_inbox < MAX_INBOX,
            "{} messages waiting for the control loop",
            self.max_inbox
        );
    }

    fn with<T: Send + 'static>(&mut self, f: impl FnOnce(&mut Control) -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        lock(&self.sh.jobs).push_back(Box::new(move |c| {
            let _ = tx.send(f(c));
        }));
        let deadline = Instant::now() + ANSWER_TIMEOUT;
        loop {
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(v) => return v,
                Err(_) => {
                    self.check();
                    assert!(Instant::now() < deadline, "the control loop did not answer");
                }
            }
        }
    }

    fn now(&self) -> SampleTime {
        self.sh.rendered.load(Ordering::Acquire)
    }

    /// The largest counts the control loop saw since the last call.
    fn counts(&mut self) -> Counts {
        std::mem::take(&mut *lock(&self.sh.counts))
    }

    fn start_probe(&mut self) -> SampleTime {
        let mut cap = lock(&self.sh.capture);
        cap.active = true;
        cap.data.clear();
        cap.base = cap.rendered;
        cap.base
    }

    fn take_probe(&mut self) -> Vec<f32> {
        let mut cap = lock(&self.sh.capture);
        cap.active = false;
        std::mem::take(&mut cap.data)
    }
}

// ---------------------------------------------------------------------------------------------
// The run

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

/// A soak of `secs` wall seconds with a checkpoint (a panic, a quiet spell, a heartbeat) about
/// every quarter of it; returns the soak and what the threads measured.
fn soak_rt(secs: u64, seed: u64, stall_ms: u64) -> (Soak<Realtime>, Summary) {
    let built = build();
    let rt = Realtime::start_stalled(&built, stall_ms);
    let mut s = Soak::new(rt, &built, seed);
    // A late start is bounded below, not forbidden: this is a real machine.
    s.late_starts_allowed = true;
    // A few minutes cannot visit every cue at the pace of a 24 h run: the operator is busier.
    s.action_gap = soak_common::blocks(1.0);
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        s.run(secs, (secs / 4).clamp(20, 120), |_| {});
    }));
    if let Err(e) = ran {
        explain_failure(&s.d.sh);
        std::panic::resume_unwind(e);
    }
    let summary = s.d.finish();
    (s, summary)
}

/// Seconds of sample time, for the failure report.
fn at_secs(frames: SampleTime) -> f64 {
    frames as f64 / f64::from(RATE)
}

/// What the threads measured up to a failure, and the stalls just before it: whether the run
/// failed on the engine or on a host that stopped running its threads on time.
fn explain_failure(sh: &Shared) {
    let rendered = sh.rendered.load(Ordering::Acquire);
    let from = rendered.saturating_sub(REPORT_WINDOW_SECS * u64::from(RATE));
    let s = lock(&sh.stats);
    eprintln!(
        "--- real-time soak failed at {:.3} s of sample time",
        at_secs(rendered)
    );
    Spread::of(&s.wake_over_us).show("control wake overshoot");
    Spread::of(&s.device_late_us).show("device block lateness");
    eprintln!(
        "{} control ticks, longest gap between ticks {:.2} ms, {} slow wakes",
        s.ticks,
        s.tick_gap_max.as_secs_f64() * 1e3,
        s.slow_wakes
    );
    if let Some(at) = s.first_underrun {
        eprintln!(
            "first underrun seen by the control loop at {:.3} s",
            at_secs(at)
        );
    }
    let list = |what: &str, events: &mut dyn Iterator<Item = (SampleTime, u32)>| {
        let events: Vec<_> = events.filter(|&(at, _)| at >= from).collect();
        eprintln!(
            "{what} of {} ms or more in the last {REPORT_WINDOW_SECS} s: {}",
            NOTABLE.as_millis(),
            events.len()
        );
        for (at, us) in events.iter().rev().take(40).rev() {
            eprintln!("  at {:.3} s: {:.1} ms", at_secs(*at), f64::from(*us) / 1e3);
        }
    };
    let notable = micros(NOTABLE);
    list(
        "late device blocks",
        &mut s
            .device_late_us
            .iter()
            .enumerate()
            .filter(|&(_, &us)| us >= notable)
            .map(|(k, &us)| (k as u64 * BLOCK, us)),
    );
    list(
        "control wake overshoots",
        &mut s.notable_wakes.iter().copied(),
    );
    list(
        "gaps between control ticks",
        &mut s.notable_gaps.iter().copied(),
    );
}

/// Whether a host whose one-block sleeps overshoot by `sleep` can run the stand-in device on a
/// block cadence, as a real device's clock-driven callback runs. Past one block at the median,
/// almost every block is rendered late and then in a catch-up burst, so a start committed a
/// block ahead races the disk worker that fills its ring: the run would measure the host's
/// timer, not the engine (issue GitHub's Intel macOS VM oversleeps 8 ms by 26-36 ms with
/// its CPU idle). Past [`DEVICE_LATE_P99_MAX`] at p99 the run's own lateness bound cannot hold.
fn host_keeps_real_time(sleep: Spread) -> Result<(), String> {
    let p50 = Duration::from_micros(sleep.p50.into());
    let p99 = Duration::from_micros(sleep.p99.into());
    if p50 > period() || p99 > DEVICE_LATE_P99_MAX {
        return Err(format!(
            "this host cannot keep real time for the stand-in device: a {:?} sleep overshoots \
             by {p50:?} at p50 (limit one block) and {p99:?} at p99 (limit \
             {DEVICE_LATE_P99_MAX:?})",
            period()
        ));
    }
    Ok(())
}

/// Set (non-empty) on a machine that is meant to keep real time, so a host that cannot fails
/// the run instead of skipping it.
const REQUIRE_ENV: &str = "OPENLAB_SOAK_RT_REQUIRE";

/// Measures the host first (see [`host_keeps_real_time`]); `false` when the run is skipped.
fn host_ready() -> bool {
    let Err(why) = host_keeps_real_time(host_timing()) else {
        return true;
    };
    let required = std::env::var_os(REQUIRE_ENV).is_some_and(|v| !v.is_empty());
    assert!(!required, "{why} ({REQUIRE_ENV} is set)");
    eprintln!("skipped: {why}");
    false
}

/// How well this host runs a thread on time, before the soak asks it to: sleeps of one block
/// (their overshoot, returned), and a spinning thread's longest time off the CPU (preemption
/// or, on a virtual machine, time the host took the CPU away).
fn host_timing() -> Spread {
    let mut over = Vec::new();
    for _ in 0..250 {
        let t = Instant::now();
        thread::sleep(period());
        over.push(micros(t.elapsed().saturating_sub(period())));
    }
    let sleep = Spread::of(&over);
    sleep.show("host: one-block sleep overshoot");
    let (mut off, mut long, mut worst) = (0u32, 0u32, Duration::ZERO);
    let end = Instant::now() + Duration::from_secs(1);
    let mut last = Instant::now();
    while last < end {
        let now = Instant::now();
        let gap = now - last;
        if gap >= Duration::from_millis(1) {
            off += 1;
        }
        if gap >= NOTABLE {
            long += 1;
        }
        worst = worst.max(gap);
        last = now;
    }
    println!(
        "host: a spinning thread was off the CPU {off} times for 1 ms or more in 1 s, \
         {long} for {} ms or more, longest {:.2} ms",
        NOTABLE.as_millis(),
        worst.as_secs_f64() * 1e3
    );
    sleep
}

#[test]
#[ignore = "real-time soak: minutes of wall clock; run in release (see the module comment)"]
fn minutes_of_a_show_on_the_wall_clock_keep_every_invariant() {
    let secs = env_u64("OPENLAB_SOAK_RT_SECS").unwrap_or(120);
    let seed = env_u64("OPENLAB_SOAK_SEED").unwrap_or(0x0e11_0000_0001);
    if !host_ready() {
        return;
    }
    let started = Instant::now();
    let (s, m) = soak_rt(secs, seed, 0);
    let wall = started.elapsed();

    println!("{:#?}", s.report);
    println!("{secs} s requested, {wall:?} of wall time, seed {seed}");
    m.wake.show("control wake overshoot");
    m.device_late.show("device block lateness");
    println!(
        "{} control ticks, longest gap between ticks {:.2} ms, most messages waiting in the inbox {}",
        m.ticks,
        m.tick_gap_max.as_secs_f64() * 1e3,
        m.max_inbox
    );
    let lag_ms: Vec<u32> = s
        .heartbeat_lag
        .iter()
        .map(|&f| (f * 1000 / u64::from(RATE)) as u32)
        .collect();
    println!(
        "heartbeat: sent to first sample (ms) {lag_ms:?}; late starts {}",
        s.report.warnings.get("late_start").copied().unwrap_or(0)
    );

    // The same checks as the virtual soak; a 2 minute run reaches fewer cues than 24 h.
    assert_realistic(&s.report, false);

    // Real time.
    assert!(
        m.wake.n > 100,
        "only {} timed-out waits: the loop was not running",
        m.wake.n
    );
    assert!(
        Duration::from_micros(m.wake.p99.into()) <= WAKE_P99_MAX,
        "wake jitter p99 {:?} over {WAKE_P99_MAX:?}",
        Duration::from_micros(m.wake.p99.into())
    );
    assert!(
        Duration::from_micros(m.wake.max.into()) <= WAKE_MAX,
        "wake jitter max {:?} over {WAKE_MAX:?}",
        Duration::from_micros(m.wake.max.into())
    );
    assert!(
        m.tick_gap_max <= TICK_GAP_MAX,
        "the control loop went {:?} between ticks (limit {TICK_GAP_MAX:?})",
        m.tick_gap_max
    );
    assert!(
        Duration::from_micros(m.device_late.p99.into()) <= DEVICE_LATE_P99_MAX,
        "the stand-in device ran {:?} late at p99 (limit {DEVICE_LATE_P99_MAX:?})",
        Duration::from_micros(m.device_late.p99.into())
    );
    let late_starts = s.report.warnings.get("late_start").copied().unwrap_or(0);
    // Starts in stretches where the timer ran a block late are allowed to be late too; a quiet
    // machine has none of those and keeps the small fixed allowance.
    let allowed = 2 + secs / 60 + m.slow_wakes / 20;
    assert!(
        late_starts <= allowed,
        "{late_starts} late-start warnings in {secs} s (allowed {allowed}, {} slow wakes)",
        m.slow_wakes
    );
    assert!(m.max_inbox < MAX_INBOX);
}

/// The checks are not vacuous: a stand-in device frozen for 500 ms (past `DEVICE_BUFFER`) is a
/// dropout the backend counts, and the run fails on it.
#[test]
#[ignore = "real-time soak: 20 s of wall clock; run in release (see the module comment)"]
fn a_stalled_device_fails_the_run() {
    if !host_ready() {
        return;
    }
    let Err(e) = std::panic::catch_unwind(|| soak_rt(20, 3, 500)) else {
        panic!("a run with a stalled device passed");
    };
    let what = e
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| e.downcast_ref::<&str>().copied())
        .unwrap_or("");
    assert!(what.contains("xruns"), "failed, but not on xruns: {what}");
}

// ---------------------------------------------------------------------------------------------
// The harness itself. These are in the normal suite: they test `run_observed` and the numbers
// without waiting on anything but the loop's own 5 ms timeout.

/// Issue GitHub's hosted macOS VMs (idle CPUs; 8 ms sleeps overshooting by 26-36 ms at
/// p50 and 63 ms at p99 on Intel, 13-18 ms and 34 ms on arm64) are refused; an M4 Mac (1.9 ms /
/// 2.1 ms, and 2.8 ms / 4.5 ms with twice as many busy processes as cores) is not.
#[test]
fn a_host_that_oversleeps_a_block_cannot_run_the_soak() {
    let ms = |p50: u32, p99: u32| Spread {
        n: 250,
        p50: p50 * 1000,
        p99: p99 * 1000,
        max: p99 * 1000,
    };
    assert!(host_keeps_real_time(ms(36, 63)).is_err());
    assert!(host_keeps_real_time(ms(29, 45)).is_err());
    assert!(host_keeps_real_time(ms(13, 34)).is_err());
    // Within a block at the median, but the p99 is past the run's own lateness bound.
    assert!(host_keeps_real_time(ms(3, 60)).is_err());
    assert!(host_keeps_real_time(ms(2, 2)).is_ok());
    assert!(host_keeps_real_time(ms(3, 5)).is_ok());
    // The limits themselves pass: one block at p50, the lateness bound at p99.
    let edge = Spread {
        n: 250,
        p50: micros(period()),
        p99: micros(DEVICE_LATE_P99_MAX),
        max: micros(DEVICE_LATE_P99_MAX),
    };
    assert!(host_keeps_real_time(edge).is_ok());
}

#[test]
fn percentiles_use_the_nearest_rank() {
    assert_eq!(percentile(&[], 0.5), 0);
    assert_eq!(percentile(&[7], 0.99), 7);
    let v: Vec<u32> = (1..=100).rev().collect();
    assert_eq!(percentile(&v, 0.5), 51);
    assert_eq!(percentile(&v, 0.99), 99);
    assert_eq!(percentile(&v, 1.0), 100);
    assert_eq!(Spread::of(&v).max, 100);
}

#[test]
fn the_observer_sees_a_wake_by_message_and_a_wake_by_timeout() {
    let built = build();
    let (control, _renderer, _channels, inbox_tx, inbox, _outbound) = start_control(&built);
    // A message is waiting before the loop starts: the first wake is by message.
    inbox_tx
        .send(Inbox::Log {
            level: openlab_core::protocol::LogLevel::Info,
            message: "wake".into(),
        })
        .unwrap();
    let mut wakes = Vec::new();
    let stop = inbox_tx.clone();
    // Ends the loop at the first wake by timeout. The control loop's own feedback (its media
    // workers) may add message wakes before it, so only the first and last are pinned.
    run_observed(control, &inbox, |_, wake| {
        wakes.push(wake);
        if wake.timed_out {
            stop.send(Inbox::Shutdown).unwrap();
        }
    });
    let (first, last) = (wakes[0], wakes[wakes.len() - 1]);
    assert!(!first.timed_out, "{wakes:?}");
    assert!(last.timed_out, "{wakes:?}");
    assert!(wakes[..wakes.len() - 1].iter().all(|w| !w.timed_out));
    // A timed-out wait lasts at least what was asked.
    assert!(last.waited >= last.wanted, "{wakes:?}");
}
