// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    dead_code
)]

//! Soak tests (NFR-09, QA-05): a realistic show run by a random operator on the offline driver
//! for a long stretch of virtual time, with the invariants of a live show checked throughout.
//!
//! The show is generated (groups of every mode, audio cues over real small WAV files with
//! fades, slice loops and devamps, auto-follow and auto-continue chains, a GoTo loop, fade, stop,
//! pause, load, reset and arm cues) and the operator is a seeded random one: GO, start, stop,
//! pause, resume, reset, playhead moves, panic, and edits with undo and redo. Time is virtual:
//! the same seed gives the same run, and nothing depends on the wall clock (the 24 h run reads
//! it only to report how long it took and to compare the first hours with the last).
//!
//! `OfflineShow::step` and `run` keep every rendered sample (a 24 h capture is gigabytes), so
//! this drives the renderer and the control loop itself, one block at a time, the way `step`
//! does, and throws the audio away after checking it.
//!
//! What is checked, per block and per virtual second: the output is finite; the mixer reports
//! no xrun, no underrun, no fault, no dropped event; voices, streams, held streams and deferred
//! reports stay under fixed bounds; the broadcast stream has no gap (revisions and runtime
//! sequence numbers rise by one) and nothing warns that should not. At each checkpoint (a panic
//! and a quiet few seconds, every ten virtual minutes): no cue is left running or stuck, no voice,
//! stream or deferred report is left anywhere, the cue count is the base plus what the operator
//! added, what a client folded from the broadcasts equals the engine's snapshot, undo and redo
//! availability match, and a known cue starts on exactly the sample it was scheduled for and
//! plays exactly its length (no timing drift).
//!
//! Counted instead of measured: memory is the number of voices, streams, queued messages, cues
//! and history entries, never the process size. History is unlimited by design (ED-07), so its
//! depth is the count of edits the operator made, and the test bounds the edits, not the stack.
//!
//! `cargo test -p openlab-engine --test soak` runs ten virtual minutes. The 24 h run is
//! `#[ignore]`d (release only): `cargo test --release -p openlab-engine --test soak --
//! --ignored --nocapture`; `OPENLAB_SOAK_SEED=<n>` runs another operator (the default seed is
//! fixed, so CI is repeatable).

//!
//! The same show and operator run against the real control loop on the wall clock in
//! `soak_rt.rs` (`#[ignore]`d), for the real-time half of NFR-09/QA-05; the shared parts are in
//! `soak_common/`.

mod show;
mod soak_common;

use std::time::{Duration, Instant};

use openlab_core::SampleTime;
use openlab_core::protocol::{ClientMessage, RequestId, ServerMessage};
use openlab_engine::control::Control;
use openlab_engine::control::offline::OfflineShow;
use show::{BLOCK, show_with};
use soak_common::{Built, Counts, Driver, Report, Soak, assert_realistic, build};

const SETTLE: Duration = Duration::from_secs(30);

/// Virtual time: the control loop stepped block by block on the offline renderer.
///
/// `OfflineShow::step` and `run` keep every rendered sample (a 24 h capture is gigabytes), so
/// this drives the renderer and the control loop itself, one block at a time, the way `step`
/// does, and throws the audio away after checking it.
struct Offline {
    s: OfflineShow,
    buf: Vec<f32>,
    block: u64,
    probe: Option<Vec<f32>>,
}

impl Offline {
    fn new(built: &Built) -> Self {
        let config = show::config(&built.ws);
        Self {
            s: show_with(built.ws.clone(), config),
            buf: vec![0.0; BLOCK as usize * 2],
            block: 0,
            probe: None,
        }
    }
}

impl Driver for Offline {
    // The schedule is predicted exactly: a message handled now starts on `now()`.
    const START_SLACK: SampleTime = 0;

    fn send(&mut self, message: ClientMessage) -> Vec<ServerMessage> {
        self.s.send(message)
    }

    fn request_id(&mut self) -> RequestId {
        self.s.request_id()
    }

    fn take_outbound(&mut self) -> Vec<ServerMessage> {
        self.s.take_outbound()
    }

    /// One block, as `OfflineShow::step` does it, without keeping the audio.
    fn step(&mut self) {
        let end = self.s.rendered() + BLOCK;
        let mut stalls = 0;
        for (handle, needed) in self.s.control().stream_needs(end) {
            if !matches!(handle.wait_written(needed, SETTLE), Ok(true) | Err(_)) {
                stalls += 1;
            }
        }
        for handle in self.s.control().prepared_streams() {
            if !matches!(handle.wait_primed(SETTLE), Ok(true) | Err(_)) {
                stalls += 1;
            }
        }
        assert_eq!(
            stalls, 0,
            "a disk stream did not deliver at block {}",
            self.block
        );
        self.s
            .renderer_mut()
            .render_into(&mut self.buf, &[BLOCK as usize]);
        for x in &self.buf {
            assert!(
                x.is_finite(),
                "NaN or infinity in the output at block {}",
                self.block
            );
        }
        if let Some(p) = &mut self.probe {
            p.extend(self.buf.iter().step_by(2));
        }
        self.s.tick();
        self.block += 1;
    }

    fn with<T: Send + 'static>(&mut self, f: impl FnOnce(&mut Control) -> T + Send + 'static) -> T {
        f(self.s.control_mut())
    }

    fn now(&self) -> SampleTime {
        self.s.now()
    }

    fn counts(&mut self) -> Counts {
        let c = self.s.control();
        Counts {
            voices: c.live_voices(),
            streams: c.open_streams(),
            held: c.held_streams(),
            deferred: c.deferred(),
        }
    }

    fn start_probe(&mut self) -> SampleTime {
        self.probe = Some(Vec::new());
        self.s.rendered()
    }

    fn take_probe(&mut self) -> Vec<f32> {
        self.probe.take().unwrap_or_default()
    }
}

fn soak(seed: u64, total_secs: u64, checkpoint_secs: u64) -> Report {
    let built = build();
    let mut s = Soak::new(Offline::new(&built), &built, seed);
    s.run(total_secs, checkpoint_secs, |_| {});
    s.report
}

#[test]
fn ten_virtual_minutes_of_a_show_keep_every_invariant() {
    let r = soak(0x0e11_ab5a, 600, 100);
    println!("{r:#?}");
    assert_realistic(&r, true);
    assert!(r.checkpoints >= 6);
}

#[test]
fn a_soak_run_is_repeatable() {
    // Same seed, same show, same everything: virtual time and the settled driver leave nothing
    // to chance (QA-02).
    // Except `max_streams`: a disk worker closes a released stream on its own thread, so the
    // count of open streams a sample sees depends on how fast that thread is. Everything the
    // engine decides is the same.
    let run = || {
        let mut r = soak(7, 120, 60);
        r.max_streams = 0;
        r
    };
    assert_eq!(run(), run());
}

/// 24 hours of virtual time (NFR-09, QA-05). Release only. The wall time and the first and last
/// hours' speed are printed; a run that slows down by more than 3x is a performance degradation.
#[test]
#[ignore = "24 h of virtual time: run in release (see the module comment)"]
fn twenty_four_virtual_hours_of_a_show_keep_every_invariant() {
    let started = Instant::now();
    let built = build();
    let mut s = Soak::new(
        Offline::new(&built),
        &built,
        std::env::var("OPENLAB_SOAK_SEED")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0x24_0000_0001),
    );
    let mut hours: Vec<Duration> = Vec::new();
    let mut last = started;
    s.run(24 * 3600, 600, |hour| {
        let now = Instant::now();
        hours.push(now - last);
        last = now;
        if hour % 4 == 0 {
            println!("hour {hour}: {:?}", hours.last());
        }
    });
    let wall = started.elapsed();
    println!("{:#?}", s.report);
    println!("24 virtual hours in {wall:?}");
    assert_realistic(&s.report, true);
    assert_eq!(hours.len(), 24);
    let head: Duration = hours.iter().take(4).sum();
    let tail: Duration = hours.iter().skip(20).sum();
    println!("first four hours {head:?}, last four {tail:?}");
    assert!(
        tail < head * 3,
        "the last four hours took {tail:?}, the first four {head:?}: performance degraded"
    );
}
