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

mod show;

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use openlab_core::protocol::{
    ClientMessage, Outcome, RuntimeChange, ServerMessage, Snapshot, TransportAction, WireCueState,
    WireWarning,
};
use openlab_core::{
    Command, ContinueMode, Cue, CueId, CueKind, CueProperty, FadeCurve, FadeLevels, FadeSpec,
    GroupMode, Position, Workspace,
};
use openlab_engine::control::offline::OfflineShow;
use show::{BLOCK, RATE, audio, cue, dc, id, list_id, show_with, wav, with_slices, workspace};

const SETTLE: Duration = Duration::from_secs(30);
/// Bounds that hold with a wide margin on a healthy run; a leak crosses them within hours.
const MAX_VOICES: usize = 64;
const MAX_STREAMS: usize = 128;
const MAX_DEFERRED: usize = 1024;
const MAX_DRAIN: usize = 512;
const MAX_ADDED: usize = 6;
const HEART: u128 = 99;
/// Cues the operator may disarm; a checkpoint arms them again.
const DISARMABLE: [u128; 6] = [1, 4, 5, 7, 17, 25];
const HEART_FRAMES: usize = 400;

fn blocks(secs: f64) -> u64 {
    (secs * f64::from(RATE) / BLOCK as f64).ceil() as u64
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*: small, seeded, the same on every platform.
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() >> 11) as usize % n.max(1)
    }

    fn pick<T: Copy>(&mut self, of: &[T]) -> T {
        of[self.below(of.len())]
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

// ---------------------------------------------------------------------------------------------
// The show

struct Built {
    ws: Workspace,
    /// Every cue the operator may aim at (children included, the heartbeat not).
    all: Vec<CueId>,
    /// Top-level cues, for playhead moves.
    top: Vec<CueId>,
    cue_count: usize,
}

fn sine(frames: usize, amp: f32, step: f32) -> Vec<f32> {
    (0..frames).map(|i| amp * (i as f32 * step).sin()).collect()
}

fn secs(frames: usize) -> f64 {
    frames as f64 / f64::from(RATE)
}

fn fades(mut c: Cue, fade_in: f64, fade_out: f64, curve: FadeCurve) -> Cue {
    if let CueKind::Audio {
        fade_in: fi,
        fade_out: fo,
        ..
    } = &mut c.kind
    {
        *fi = FadeSpec {
            duration: fade_in,
            curve,
        };
        *fo = FadeSpec {
            duration: fade_out,
            curve,
        };
    }
    c
}

fn follow(mut c: Cue, mode: ContinueMode) -> Cue {
    c.continue_mode = mode;
    c
}

fn waits(mut c: Cue, pre: f64, post: f64) -> Cue {
    c.pre_wait = pre;
    c.post_wait = post;
    c
}

fn target(n: u128, kind: fn(Option<CueId>) -> CueKind, to: u128) -> Cue {
    cue(n, kind(Some(id(to))))
}

fn count(c: &Cue) -> usize {
    1 + match &c.kind {
        CueKind::Group { children, .. } => children.iter().map(count).sum(),
        _ => 0,
    }
}

fn ids(c: &Cue, out: &mut Vec<CueId>) {
    out.push(c.id);
    if let CueKind::Group { children, .. } = &c.kind {
        for child in children {
            ids(child, out);
        }
    }
}

fn build() -> Built {
    // Real WAVs, 8 kHz mono: 0.3 s to 3 s, and two for the loops.
    let f300 = wav(&sine(2400, 0.3, 0.05));
    let f800 = wav(&sine(6400, 0.25, 0.08));
    let f1500 = wav(&sine(12000, 0.2, 0.03));
    let f3000 = wav(&sine(24000, 0.2, 0.11));
    let loop_a = wav(&dc(3200, 0.25));
    let loop_b = wav(&sine(4800, 0.25, 0.2));
    let beat = wav(&dc(HEART_FRAMES, 0.5));

    // A slice table: a 400-frame intro, then 800 frames that repeat until devamped, then the
    // rest once.
    let looped = |n: u128, path, out: u32| {
        with_slices(audio(n, path, out), &[(secs(400), 1), (secs(1200), 0)], 1)
    };
    let cues = vec![
        fades(audio(1, &f1500, 0), 0.1, 0.2, FadeCurve::EqualPower),
        follow(audio(2, &f300, 1), ContinueMode::AutoFollow),
        follow(audio(3, &f800, 0), ContinueMode::AutoFollow),
        audio(4, &f300, 1),
        waits(
            follow(audio(5, &f800, 1), ContinueMode::AutoContinue),
            0.2,
            0.3,
        ),
        fades(audio(6, &f3000, 0), 0.0, 0.5, FadeCurve::SCurve),
        cue(
            7,
            CueKind::Group {
                mode: GroupMode::Timeline,
                children: vec![
                    audio(71, &f800, 0),
                    waits(audio(72, &f1500, 1), 0.5, 0.0),
                    audio(73, &f300, 0),
                ],
            },
        ),
        cue(
            8,
            CueKind::Group {
                mode: GroupMode::Playlist {
                    loop_: false,
                    shuffle: false,
                },
                children: vec![
                    audio(81, &f300, 0),
                    audio(82, &f800, 1),
                    audio(83, &f300, 0),
                ],
            },
        ),
        cue(
            9,
            CueKind::Group {
                mode: GroupMode::Playlist {
                    loop_: true,
                    shuffle: true,
                },
                children: vec![audio(91, &f300, 0), audio(92, &f800, 1)],
            },
        ),
        cue(
            10,
            CueKind::Group {
                mode: GroupMode::StartFirstAndEnter,
                children: vec![audio(101, &f800, 0), audio(102, &f300, 1)],
            },
        ),
        cue(
            11,
            CueKind::Group {
                mode: GroupMode::StartRandom,
                children: vec![
                    audio(111, &f300, 0),
                    audio(112, &f800, 1),
                    audio(113, &f1500, 0),
                ],
            },
        ),
        looped(12, &loop_a, 0),
        cue(
            13,
            CueKind::Devamp {
                target: Some(id(12)),
                start_next_cue: false,
                stop_target_when_slice_ends: false,
            },
        ),
        looped(14, &loop_b, 1),
        cue(
            15,
            CueKind::Devamp {
                target: Some(id(14)),
                start_next_cue: true,
                stop_target_when_slice_ends: true,
            },
        ),
        cue(
            16,
            CueKind::Fade {
                target: Some(id(6)),
                duration: 1.0,
                curve: FadeCurve::EqualPower,
                levels: FadeLevels {
                    master_db: Some(-12.0),
                    output_db: vec![],
                    crosspoints: vec![],
                },
                stop_target_when_done: true,
            },
        ),
        cue(
            17,
            CueKind::Stop {
                target: Some(id(12)),
                fade_out: FadeSpec {
                    duration: 0.3,
                    curve: FadeCurve::Linear,
                },
            },
        ),
        target(18, |t| CueKind::Pause { target: t }, 6),
        target(19, |t| CueKind::Start { target: t }, 1),
        cue(
            20,
            CueKind::Load {
                target: Some(id(6)),
                load_to_time: 0.5,
            },
        ),
        target(21, |t| CueKind::Reset { target: t }, 6),
        target(22, |t| CueKind::Arm { target: t }, 5),
        target(23, |t| CueKind::Disarm { target: t }, 5),
        follow(
            cue(24, CueKind::Wait { duration: 1.0 }),
            ContinueMode::AutoFollow,
        ),
        cue(25, CueKind::Memo),
        // An endless loop driven by the engine alone: a clip, a GoTo back to it that GOs.
        follow(audio(26, &f800, 0), ContinueMode::AutoFollow),
        cue(
            27,
            CueKind::GoTo {
                target: Some(id(26)),
                then_go: true,
            },
        ),
        audio(HEART, &beat, 0),
    ];
    let mut all = Vec::new();
    let mut top = Vec::new();
    for c in &cues {
        if c.id != id(HEART) {
            ids(c, &mut all);
            top.push(c.id);
        }
    }
    let cue_count = cues.iter().map(count).sum();
    Built {
        ws: workspace(cues),
        all,
        top,
        cue_count,
    }
}

// ---------------------------------------------------------------------------------------------
// What a client folds from the broadcasts

#[derive(Default)]
struct Fold {
    revision: u64,
    seq: u64,
    states: HashMap<CueId, WireCueState>,
    playhead: Option<CueId>,
}

#[derive(Debug, Default, PartialEq)]
struct Report {
    blocks: u64,
    actions: BTreeMap<&'static str, u64>,
    edits_applied: u64,
    edits_refused: u64,
    undone: u64,
    redone: u64,
    checkpoints: u64,
    messages: u64,
    cue_state_changes: u64,
    progress_messages: u64,
    meters_messages: u64,
    warnings: BTreeMap<String, u64>,
    max_voices: usize,
    max_streams: usize,
    max_held_streams: usize,
    max_deferred: usize,
    max_drain: usize,
    max_running_cues: usize,
    history_depth: usize,
    heartbeats: u64,
    revision: u64,
    /// Distinct cues that left idle at least once.
    cues_run: usize,
}

struct Soak {
    s: OfflineShow,
    rng: Rng,
    built_all: Vec<CueId>,
    top: Vec<CueId>,
    base_cues: usize,
    added: Vec<CueId>,
    next_added: u128,
    buf: Vec<f32>,
    block: u64,
    fold: Fold,
    undo_depth: usize,
    redo_depth: usize,
    resume_at: Option<u64>,
    probe: Option<Vec<f32>>,
    ran: std::collections::BTreeSet<CueId>,
    /// The last operator actions and cue state changes, for the failure message.
    recent: std::collections::VecDeque<String>,
    report: Report,
}

impl Soak {
    fn new(seed: u64) -> Self {
        let built = build();
        let config = show::config(&built.ws);
        let mut s = show_with(built.ws, config);
        let snap = s.control().snapshot();
        let fold = Fold {
            revision: snap.revision,
            seq: snap.runtime_seq,
            states: HashMap::new(),
            playhead: snap.playheads.iter().find_map(|p| p.cue),
        };
        let _ = s.take_outbound();
        Self {
            s,
            rng: Rng(seed | 1),
            built_all: built.all,
            top: built.top,
            base_cues: built.cue_count,
            added: Vec::new(),
            next_added: 10_000,
            buf: vec![0.0; BLOCK as usize * 2],
            block: 0,
            fold,
            undo_depth: 0,
            redo_depth: 0,
            resume_at: None,
            probe: None,
            ran: Default::default(),
            recent: Default::default(),
            report: Report::default(),
        }
    }

    // ----- time -----

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
        self.sample();
        self.drain();
        if self.block.is_multiple_of(blocks(1.0)) {
            self.every_second();
        }
    }

    fn run_blocks(&mut self, n: u64) {
        for _ in 0..n {
            self.step();
        }
    }

    // ----- the broadcast stream -----

    fn drain(&mut self) {
        let out = self.s.take_outbound();
        self.report.max_drain = self.report.max_drain.max(out.len());
        assert!(
            out.len() < MAX_DRAIN,
            "{} messages queued within one block",
            out.len()
        );
        for m in out {
            self.report.messages += 1;
            self.fold(m);
        }
    }

    fn fold(&mut self, m: ServerMessage) {
        match m {
            ServerMessage::ModelChanged { revision, .. } => {
                assert_eq!(
                    revision,
                    self.fold.revision + 1,
                    "a model_changed was skipped"
                );
                self.fold.revision = revision;
            }
            ServerMessage::Runtime { seq, changes } => {
                assert_eq!(seq, self.fold.seq + 1, "a runtime message was skipped");
                self.fold.seq = seq;
                for c in changes {
                    match c {
                        RuntimeChange::CueState { cue, state, .. } => {
                            self.report.cue_state_changes += 1;
                            self.ran.insert(cue);
                            self.remember(format!(
                                "block {}: cue {} is {state:?}",
                                self.block,
                                cue.0.as_u128()
                            ));
                            if state == WireCueState::Idle {
                                self.fold.states.remove(&cue);
                            } else {
                                self.fold.states.insert(cue, state);
                            }
                            let running = self.fold.states.len();
                            self.report.max_running_cues =
                                self.report.max_running_cues.max(running);
                        }
                        RuntimeChange::Playhead { cue, .. } => self.fold.playhead = cue,
                        RuntimeChange::Warning { warning } => self.warn(&warning),
                        _ => {}
                    }
                }
            }
            ServerMessage::Progress { .. } => self.report.progress_messages += 1,
            ServerMessage::Meters { .. } => self.report.meters_messages += 1,
            ServerMessage::Log { level, message } => {
                panic!("the control loop logged at {level:?}: {message}");
            }
            ServerMessage::Warnings { warnings } => {
                assert!(
                    warnings.len() < 1000,
                    "{} standing warnings",
                    warnings.len()
                );
            }
            ServerMessage::Error { .. } | ServerMessage::Result { .. } => {
                panic!("unexpected broadcast")
            }
            _ => {}
        }
    }

    fn warn(&mut self, w: &WireWarning) {
        let name = match w {
            WireWarning::GoIgnored { .. } => "go_ignored",
            WireWarning::TriggerIgnored { .. } => "trigger_ignored",
            WireWarning::LoadIgnored { .. } => "load_ignored",
            WireWarning::TargetNotRunning { .. } => "target_not_running",
            WireWarning::DevampIgnored { .. } => "devamp_ignored",
            WireWarning::NotInList { .. } => "not_in_list",
            WireWarning::UnknownCue { .. } => "unknown_cue",
            WireWarning::CueRemoved { .. } => "cue_removed",
            // A late start is the engine missing its schedule; the others mean something broke
            // or a chain ran away. None may happen in a healthy run.
            other => panic!(
                "the engine warned at block {}: {other:?}\nrecent actions and cue changes: {:#?}",
                self.block, self.recent
            ),
        };
        *self.report.warnings.entry(name.into()).or_default() += 1;
    }

    /// The object counts, every block: they are cheap, and a voice lives a fraction of a second.
    fn sample(&mut self) {
        let c = self.s.control();
        let (voices, streams, held, deferred) = (
            c.live_voices(),
            c.open_streams(),
            c.held_streams(),
            c.deferred(),
        );
        let r = &mut self.report;
        r.max_voices = r.max_voices.max(voices);
        r.max_streams = r.max_streams.max(streams);
        r.max_held_streams = r.max_held_streams.max(held);
        r.max_deferred = r.max_deferred.max(deferred);
        assert!(voices <= MAX_VOICES, "{voices} voices");
        assert!(streams <= MAX_STREAMS, "{streams} open streams");
        assert!(held <= MAX_STREAMS, "{held} held streams");
        assert!(deferred <= MAX_DEFERRED, "{deferred} deferred reports");
    }

    fn every_second(&mut self) {
        self.mixer_is_clean();
        assert!(self.s.control().revision() >= self.fold.revision);
    }

    fn mixer_is_clean(&self) {
        let m = self.s.control().mixer();
        assert_eq!(m.xruns(), 0, "xruns at block {}", self.block);
        assert_eq!(m.underrun_frames(), 0, "underruns at block {}", self.block);
        assert_eq!(m.faults(), 0, "mixer faults at block {}", self.block);
        assert_eq!(
            m.dropped_events(),
            0,
            "dropped mixer events at block {}",
            self.block
        );
        assert_eq!(
            m.leaked_starts(),
            0,
            "leaked starts at block {}",
            self.block
        );
    }

    // ----- the operator -----

    fn note(&mut self, what: &'static str) {
        self.remember(format!("block {}: {what}", self.block));
        *self.report.actions.entry(what).or_default() += 1;
    }

    fn remember(&mut self, line: String) {
        while self.recent.len() >= 400 {
            self.recent.pop_front();
        }
        self.recent.push_back(line);
    }

    fn transport(&mut self, what: &'static str, action: TransportAction) {
        self.note(what);
        if let Some(last) = self.recent.back_mut() {
            *last = format!("block {}: {action:?}", self.block);
        }
        let replies = self.s.transport(action);
        assert!(
            replies.iter().all(|r| matches!(
                r,
                ServerMessage::Result {
                    outcome: Outcome::Ok(_),
                    ..
                }
            )),
            "{what}: {replies:?}"
        );
    }

    fn cue_of(&mut self) -> CueId {
        let all = self.built_all.clone();
        self.rng.pick(&all)
    }

    fn act(&mut self) {
        let list = list_id();
        let roll = self.rng.below(100);
        match roll {
            0..=37 => self.transport("go", TransportAction::Go { list }),
            38..=45 => {
                let cue = self.cue_of();
                self.transport("start", TransportAction::Start { cue });
            }
            46..=52 => {
                let cue = self.cue_of();
                let fade_out = self.rng.chance(50).then_some(FadeSpec {
                    duration: 0.25,
                    curve: FadeCurve::Linear,
                });
                self.transport("stop", TransportAction::Stop { cue, fade_out });
            }
            53..=54 => {
                let cue = self.cue_of();
                self.transport("hard_stop", TransportAction::HardStop { cue });
            }
            55..=57 => {
                let cue = self.cue_of();
                self.transport("pause", TransportAction::Pause { cue });
            }
            58..=60 => {
                let cue = self.cue_of();
                self.transport("resume", TransportAction::Resume { cue });
            }
            61..=62 => {
                if self.resume_at.is_none() {
                    self.transport("pause_all", TransportAction::PauseAll);
                    self.resume_at = Some(self.block + blocks(1.0 + self.rng.below(6) as f64));
                }
            }
            63 => self.transport("resume_all", TransportAction::ResumeAll),
            64..=65 => {
                let cue = self.cue_of();
                self.transport("reset", TransportAction::Reset { cue });
            }
            66..=67 => {
                // A cue loaded past its own end would finish the moment it started (and its
                // successor, started on that, would warn as late), so only the long clips are
                // loaded into.
                let (cue, at) = if self.rng.chance(50) {
                    (self.cue_of(), 0.0)
                } else {
                    (id(self.rng.pick(&[1, 6, 12, 14])), 0.5)
                };
                self.transport("load", TransportAction::Load { cue, at });
            }
            68..=70 => self.transport("playhead_next", TransportAction::PlayheadNext { list }),
            71 => self.transport(
                "playhead_previous",
                TransportAction::PlayheadPrevious { list },
            ),
            72..=75 => {
                let top = self.top.clone();
                let cue = Some(self.rng.pick(&top));
                self.transport("set_playhead", TransportAction::SetPlayhead { list, cue });
            }
            76 => {
                if self.rng.chance(50) {
                    self.transport("panic", TransportAction::Panic);
                }
            }
            _ => self.edit(),
        }
    }

    fn send(
        &mut self,
        message: impl FnOnce(openlab_core::protocol::RequestId) -> ClientMessage,
    ) -> Outcome {
        let id = self.s.request_id();
        let replies = self.s.send(message(id));
        // The broadcast (`model_changed`) is queued before the result is: fold it first.
        self.drain();
        let mut outcome = None;
        for r in replies {
            if let ServerMessage::Result { outcome: o, .. } = r {
                outcome = Some(o);
            }
        }
        let outcome = outcome.expect("an edit is answered");
        if let Outcome::Ok(ok) = &outcome
            && let Some(rev) = ok.revision
        {
            assert_eq!(
                rev, self.fold.revision,
                "a result's revision differs from the broadcast's"
            );
        }
        outcome
    }

    fn command(&mut self, command: Command) -> bool {
        self.remember(format!("block {}: EDIT {command:?}", self.block));
        let outcome = self.send(|id| ClientMessage::Command {
            id,
            command: Box::new(command),
        });
        match outcome {
            Outcome::Ok(_) => {
                self.report.edits_applied += 1;
                self.undo_depth += 1;
                self.redo_depth = 0;
                true
            }
            Outcome::Error(_) => {
                self.report.edits_refused += 1;
                false
            }
        }
    }

    fn property(&mut self, cue: CueId, property: CueProperty) {
        self.command(Command::SetCueProperty { id: cue, property });
    }

    fn edit(&mut self) {
        self.note("edit");
        match self.rng.below(10) {
            0 | 1 => {
                let cue = self.cue_of();
                let value = format!("take {}", self.rng.below(1000));
                self.property(cue, CueProperty::Name { value });
            }
            2 => {
                let cue = self.cue_of();
                let value = self.rng.pick(&[0.0, 0.1, 0.4]);
                self.property(cue, CueProperty::PreWait { value });
            }
            3 => {
                // Not the cues of the GoTo loop: a disarmed cue in an endless chain makes the
                // chain spin without time passing, and the engine's chain limit rightly warns.
                let cue = id(self.rng.pick(&DISARMABLE));
                let value = self.rng.chance(70);
                self.property(cue, CueProperty::Armed { value });
            }
            4 => {
                // Add a memo, or remove one of ours when there are many.
                if self.added.len() >= MAX_ADDED {
                    let at = self.rng.below(self.added.len());
                    let gone = self.added.remove(at);
                    self.command(Command::RemoveCue { id: gone });
                } else {
                    self.next_added += 1;
                    let n = self.next_added;
                    let index = self.rng.below(self.top.len() + 1);
                    let applied = self.command(Command::AddCue {
                        at: Position {
                            list: list_id(),
                            parent: None,
                            index,
                        },
                        cue: cue(n, CueKind::Memo),
                        auto_number: false,
                    });
                    if applied {
                        self.added.push(id(n));
                    }
                }
            }
            5 | 6 => {
                self.note("undo");
                let outcome = self.send(|id| ClientMessage::Undo { id });
                self.stepped(&outcome, true);
            }
            7 | 8 => {
                self.note("redo");
                let outcome = self.send(|id| ClientMessage::Redo { id });
                self.stepped(&outcome, false);
            }
            _ => {
                let cue = self.cue_of();
                let value = self.rng.chance(50);
                self.property(cue, CueProperty::Flagged { value });
            }
        }
    }

    /// An undo or redo that did something moves a step between the stacks. The cues it added
    /// or removed change `added`: ours are rebuilt from the engine afterwards.
    fn stepped(&mut self, outcome: &Outcome, undo: bool) {
        let Outcome::Ok(ok) = outcome else {
            panic!("undo/redo refused: {outcome:?}")
        };
        if ok.revision.is_none() {
            return;
        }
        if undo {
            self.undo_depth -= 1;
            self.redo_depth += 1;
            self.report.undone += 1;
        } else {
            self.redo_depth -= 1;
            self.undo_depth += 1;
            self.report.redone += 1;
        }
        let ws = self.s.control().workspace();
        let present: Vec<CueId> = ws.cue_lists[0].cues.iter().map(|c| c.id).collect();
        self.added = present
            .iter()
            .copied()
            .filter(|c| c.0.as_u128() > 10_000)
            .collect();
        self.top = present
            .into_iter()
            .filter(|c| c.0.as_u128() < 10_000 && c.0.as_u128() != HEART)
            .collect();
    }

    // ----- checkpoints -----

    fn checkpoint(&mut self) {
        self.report.checkpoints += 1;
        self.resume_at = None;
        // Panic ends everything, paused or not; its fade and the streams' drain are over well
        // inside two seconds.
        self.transport("checkpoint_panic", TransportAction::Panic);
        self.run_blocks(blocks(2.0));
        self.quiet_spell();
        self.drain();

        let snap = self.s.control().snapshot();
        assert!(
            snap.cue_states.is_empty(),
            "cues left over: {:?}",
            snap.cue_states
        );
        assert!(snap.previewing.is_empty());
        let c = self.s.control();
        assert!(
            c.is_idle(),
            "voices or reports left at block {}",
            self.block
        );
        assert_eq!(c.live_voices(), 0);
        // Primed streams (the cues after the playhead, kept ready) are the only ones that may
        // remain: every other stream was released and closed.
        assert_eq!(c.held_streams(), c.primed().len(), "a stream leaked");
        assert!(c.held_streams() <= openlab_engine::control::MAX_PRIMES);
        // A disk worker closes a released stream on its own thread, a moment after the handle
        // is dropped: wait for it (a hang guard, as the audio crate's own tests do). A stream
        // that is never closed is the leak this looks for.
        let deadline = Instant::now() + SETTLE;
        while self.s.control().open_streams() != self.s.control().held_streams()
            && Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        let c = self.s.control();
        assert_eq!(
            c.open_streams(),
            c.held_streams(),
            "a stream is open but not held"
        );
        assert_eq!(c.deferred(), 0);
        assert_eq!(
            self.s.control_mut().mixer_voice_count(),
            0,
            "the mixer still holds voices"
        );
        let c = self.s.control();
        let cues: usize = c.workspace().cue_lists[0].cues.iter().map(count).sum();
        assert_eq!(
            cues,
            self.base_cues + self.added.len(),
            "the cue count is not the base plus what the operator added"
        );
        self.mixer_is_clean();
        self.consistent(&snap);
        self.rearm();
        self.heartbeat();

        let first = Some(self.top[0]);
        self.transport(
            "set_playhead",
            TransportAction::SetPlayhead {
                list: list_id(),
                cue: first,
            },
        );
        self.report.history_depth = self.undo_depth;
        self.report.revision = self.s.control().revision();
    }

    /// Arms what the operator disarmed, so every cue is back in play for the next stretch.
    fn rearm(&mut self) {
        for n in DISARMABLE {
            let mut armed = None;
            for cue in &self.s.control().workspace().cue_lists[0].cues {
                find(cue, id(n), &mut armed);
            }
            if armed == Some(false) {
                self.property(id(n), CueProperty::Armed { value: true });
            }
        }
    }

    /// A quiet few seconds: nothing may start by itself after a panic (a chain, a GoTo loop, a
    /// pre-wait that survived), whatever the operator left running.
    fn quiet_spell(&mut self) {
        for _ in 0..blocks(5.0) / 40 {
            self.run_blocks(40);
            self.drain();
            assert!(
                self.fold.states.is_empty(),
                "a cue started by itself after the panic: {:?}",
                self.fold.states
            );
        }
    }

    fn consistent(&mut self, snap: &Snapshot) {
        let f = &self.fold;
        assert_eq!(f.revision, snap.revision);
        assert_eq!(f.seq, snap.runtime_seq);
        assert_eq!(f.playhead, snap.playheads.iter().find_map(|p| p.cue));
        let engine: HashMap<_, _> = snap.cue_states.iter().map(|e| (e.cue, e.state)).collect();
        assert_eq!(f.states, engine);
        assert_eq!(snap.document.can_undo, self.undo_depth > 0);
        assert_eq!(snap.document.can_redo, self.redo_depth > 0);
    }

    /// A known cue starts on the sample it was scheduled for and plays exactly its length.
    fn heartbeat(&mut self) {
        let base = self.s.rendered();
        self.probe = Some(Vec::new());
        let at = {
            let at = self.s.now();
            self.transport("heartbeat", TransportAction::Start { cue: id(HEART) });
            at
        };
        self.run_blocks(blocks(1.0));
        self.drain();
        let captured = self.probe.take().unwrap_or_default();
        let first = captured.iter().position(|&x| x != 0.0);
        let played = captured.iter().filter(|&&x| x != 0.0).count();
        assert_eq!(
            first.map(|i| base + i as u64),
            Some(at),
            "the heartbeat did not start on its scheduled sample"
        );
        assert_eq!(
            played, HEART_FRAMES,
            "the heartbeat played the wrong length"
        );
        assert!(self.fold.states.is_empty(), "the heartbeat did not finish");
        self.report.heartbeats += 1;
        // Its completion moved the playhead; the checkpoint resets that.
    }

    // ----- the run -----

    fn run(&mut self, total_secs: u64, checkpoint_secs: u64, mut on_hour: impl FnMut(u64)) {
        let total = blocks(total_secs as f64);
        let every = blocks(checkpoint_secs as f64);
        let hour = blocks(3600.0);
        let mut hours_done = 0;
        let mut next_checkpoint = every;
        let mut next_action = 0;
        while self.block < total {
            if self.block >= next_checkpoint {
                self.checkpoint();
                next_checkpoint = self.block + every;
                continue;
            }
            if self.resume_at.is_some_and(|b| self.block >= b) {
                self.resume_at = None;
                self.transport("resume_all", TransportAction::ResumeAll);
            }
            if self.block >= next_action {
                self.act();
                next_action = self.block + 1 + self.rng.below(blocks(3.0) as usize) as u64;
            }
            self.step();
            while self.block >= (hours_done + 1) * hour {
                hours_done += 1;
                on_hour(hours_done);
            }
        }
        while self.block >= (hours_done + 1) * hour {
            hours_done += 1;
            on_hour(hours_done);
        }
        self.checkpoint();
        self.report.blocks = self.block;
        self.report.cues_run = self.ran.len();
    }
}

fn soak(seed: u64, total_secs: u64, checkpoint_secs: u64) -> Report {
    let mut s = Soak::new(seed);
    s.run(total_secs, checkpoint_secs, |_| {});
    s.report
}

fn assert_realistic(r: &Report) {
    // The run must have done what it claims, or it proves nothing.
    for what in [
        "go",
        "start",
        "stop",
        "pause",
        "panic",
        "set_playhead",
        "edit",
        "undo",
        "redo",
    ] {
        assert!(
            r.actions.get(what).copied().unwrap_or(0) > 0,
            "never did {what}: {:?}",
            r.actions
        );
    }
    assert!(r.edits_applied > 0 && r.undone > 0 && r.redone > 0, "{r:?}");
    assert!(r.max_voices >= 3, "never more than {} voices", r.max_voices);
    assert!(
        r.max_running_cues >= 3,
        "never more than {} cues running",
        r.max_running_cues
    );
    assert!(
        r.cue_state_changes > 100 && r.progress_messages > 0 && r.meters_messages > 0,
        "{r:?}"
    );
    assert_eq!(r.heartbeats, r.checkpoints, "{r:?}");
    // Nearly every cue ran (41 in all): the loops, devamps, fades, chains and groups are in play.
    assert!(r.cues_run >= 35, "only {} cues ever ran", r.cues_run);
}

#[test]
fn ten_virtual_minutes_of_a_show_keep_every_invariant() {
    let r = soak(0x0e11_ab5a, 600, 100);
    println!("{r:#?}");
    assert_realistic(&r);
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
    let mut s = Soak::new(
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
    assert_realistic(&s.report);
    assert_eq!(hours.len(), 24);
    let head: Duration = hours.iter().take(4).sum();
    let tail: Duration = hours.iter().skip(20).sum();
    println!("first four hours {head:?}, last four {tail:?}");
    assert!(
        tail < head * 3,
        "the last four hours took {tail:?}, the first four {head:?}: performance degraded"
    );
}

fn find(c: &Cue, want: CueId, out: &mut Option<bool>) {
    if c.id == want {
        *out = Some(c.armed);
    }
    if let CueKind::Group { children, .. } = &c.kind {
        for child in children {
            find(child, want, out);
        }
    }
}
