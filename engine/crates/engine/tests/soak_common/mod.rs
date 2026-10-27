// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    dead_code
)]

//! What the virtual-time soak (`soak.rs`) and the real-time soak (`soak_rt.rs`) share: the
//! generated show, the seeded random operator, the fold of the broadcast stream and every
//! invariant check. They differ only in the [`Driver`]: how time passes and how the control
//! loop is reached (stepped on the offline driver, or running on its own thread against a
//! device that renders on a wall-clock cadence).
//!
//! See `soak.rs` for what is checked and why.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use openlab_core::protocol::{
    ClientMessage, Outcome, RequestId, RuntimeChange, ServerMessage, Snapshot, TransportAction,
    WireCueState, WireWarning,
};
use openlab_core::{
    Command, ContinueMode, Cue, CueId, CueKind, CueProperty, FadeCurve, FadeLevels, FadeSpec,
    GroupMode, Position, SampleTime, Workspace,
};
use openlab_engine::control::Control;

use crate::show::{BLOCK, RATE, audio, cue, dc, id, list_id, wav, with_slices, workspace};

const SETTLE: Duration = Duration::from_secs(30);
/// Bounds that hold with a wide margin on a healthy run; a leak crosses them within hours.
pub const MAX_VOICES: usize = 64;
pub const MAX_STREAMS: usize = 128;
pub const MAX_DEFERRED: usize = 1024;
const MAX_DRAIN: usize = 512;
const MAX_ADDED: usize = 6;
const HEART: u128 = 99;
/// Cues the operator may disarm; a checkpoint arms them again.
const DISARMABLE: [u128; 6] = [1, 4, 5, 7, 17, 25];
const HEART_FRAMES: usize = 400;

/// Object counts of the control loop at one moment (or the largest since the last read).
#[derive(Clone, Copy, Debug, Default)]
pub struct Counts {
    pub voices: usize,
    pub streams: usize,
    pub held: usize,
    pub deferred: usize,
}

/// How time passes in a soak and how the operator reaches the control loop.
pub trait Driver {
    /// How many frames after the sample an action was sent at a cue's first sample may fall.
    /// Zero where the schedule is exact (the offline driver predicts it); wider on a loop that
    /// handles a message whenever its thread wakes.
    const START_SLACK: SampleTime;

    /// Sends a client message; returns the replies to it (ending with its `result`).
    fn send(&mut self, message: ClientMessage) -> Vec<ServerMessage>;
    fn request_id(&mut self) -> RequestId;
    /// Messages broadcast since the last call.
    fn take_outbound(&mut self) -> Vec<ServerMessage>;
    /// Time passes by one block: when this returns the block has been rendered and the control
    /// loop has ticked (offline), or has been rendered by the device (real time).
    fn step(&mut self);
    /// Runs `f` against the control loop's state and returns what it found. On a running
    /// loop `f` runs on the control thread, between ticks.
    fn with<T: Send + 'static>(&mut self, f: impl FnOnce(&mut Control) -> T + Send + 'static) -> T;
    /// The sample a message sent now is scheduled at, or the nearest known sample before it.
    fn now(&self) -> SampleTime;
    /// Object counts: now (offline), or the largest since the last call (real time).
    fn counts(&mut self) -> Counts;
    /// Starts keeping output channel 0; returns the sample the first kept frame is.
    fn start_probe(&mut self) -> SampleTime;
    fn take_probe(&mut self) -> Vec<f32>;

    /// A transport action with a fresh request id.
    fn transport(&mut self, action: TransportAction) -> Vec<ServerMessage> {
        let id = self.request_id();
        self.send(ClientMessage::Transport { id, action })
    }
}

pub fn blocks(secs: f64) -> u64 {
    (secs * f64::from(RATE) / BLOCK as f64).ceil() as u64
}

pub struct Rng(u64);

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

pub struct Built {
    pub ws: Workspace,
    /// Every cue the operator may aim at (children included, the heartbeat not).
    pub all: Vec<CueId>,
    /// Top-level cues, for playhead moves.
    pub top: Vec<CueId>,
    pub cue_count: usize,
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

pub fn count(c: &Cue) -> usize {
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

pub fn build() -> Built {
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
                devamp_mode: openlab_core::DevampMode::CurrentSlice,
            },
        ),
        looped(14, &loop_b, 1),
        cue(
            15,
            CueKind::Devamp {
                target: Some(id(14)),
                start_next_cue: true,
                stop_target_when_slice_ends: true,
                devamp_mode: openlab_core::DevampMode::CurrentSlice,
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
pub struct Fold {
    revision: u64,
    seq: u64,
    states: HashMap<CueId, WireCueState>,
    playhead: Option<CueId>,
}

#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub blocks: u64,
    pub actions: BTreeMap<&'static str, u64>,
    pub edits_applied: u64,
    pub edits_refused: u64,
    pub undone: u64,
    pub redone: u64,
    pub checkpoints: u64,
    pub messages: u64,
    pub cue_state_changes: u64,
    pub progress_messages: u64,
    pub meters_messages: u64,
    pub warnings: BTreeMap<String, u64>,
    pub max_voices: usize,
    pub max_streams: usize,
    pub max_held_streams: usize,
    pub max_deferred: usize,
    pub max_drain: usize,
    pub max_running_cues: usize,
    pub history_depth: usize,
    pub heartbeats: u64,
    pub revision: u64,
    /// Distinct cues that left idle at least once.
    pub cues_run: usize,
}

pub struct Soak<D: Driver> {
    pub d: D,
    rng: Rng,
    built_all: Vec<CueId>,
    top: Vec<CueId>,
    base_cues: usize,
    added: Vec<CueId>,
    next_added: u128,
    block: u64,
    fold: Fold,
    undo_depth: usize,
    redo_depth: usize,
    resume_at: Option<u64>,
    ran: std::collections::BTreeSet<CueId>,
    /// The last operator actions and cue state changes, for the failure message.
    recent: std::collections::VecDeque<String>,
    /// A late start is the engine missing its schedule: a failure on the offline driver, where
    /// nothing can make it late, and counted (and bounded by the caller) on a real-time run.
    pub late_starts_allowed: bool,
    /// The operator waits between 1 and this many blocks between actions.
    pub action_gap: u64,
    /// Frames from sending each checkpoint heartbeat to its first sample.
    pub heartbeat_lag: Vec<u64>,
    pub report: Report,
}

impl<D: Driver> Soak<D> {
    /// A soak of `built`, which `d` already runs, as a seeded operator.
    pub fn new(mut d: D, built: &Built, seed: u64) -> Self {
        let snap = d.with(|c| c.snapshot());
        let fold = Fold {
            revision: snap.revision,
            seq: snap.runtime_seq,
            states: HashMap::new(),
            playhead: snap.playheads.iter().find_map(|p| p.cue),
        };
        let _ = d.take_outbound();
        Self {
            d,
            rng: Rng(seed | 1),
            built_all: built.all.clone(),
            top: built.top.clone(),
            base_cues: built.cue_count,
            added: Vec::new(),
            next_added: 10_000,
            block: 0,
            fold,
            undo_depth: 0,
            redo_depth: 0,
            resume_at: None,
            ran: Default::default(),
            recent: Default::default(),
            late_starts_allowed: false,
            action_gap: blocks(3.0),
            heartbeat_lag: Vec::new(),
            report: Report::default(),
        }
    }

    // ----- time -----

    /// One block of time: the driver's step, then the checks that run every block.
    fn step(&mut self) {
        self.d.step();
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
        let out = self.d.take_outbound();
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
            WireWarning::LateStart { .. } if self.late_starts_allowed => "late_start",
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
        let Counts {
            voices,
            streams,
            held,
            deferred,
        } = self.d.counts();
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
        let revision = self.d.with(|c| c.revision());
        assert!(revision >= self.fold.revision);
    }

    fn mixer_is_clean(&mut self) {
        let [xruns, underruns, faults, dropped, leaked] = self.d.with(|c| {
            let m = c.mixer();
            [
                m.xruns(),
                m.underrun_frames(),
                m.faults(),
                m.dropped_events(),
                m.leaked_starts(),
            ]
        });
        let block = self.block;
        assert_eq!(xruns, 0, "xruns at block {block}");
        assert_eq!(underruns, 0, "underruns at block {block}");
        assert_eq!(faults, 0, "mixer faults at block {block}");
        assert_eq!(dropped, 0, "dropped mixer events at block {block}");
        assert_eq!(leaked, 0, "leaked starts at block {block}");
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
        let replies = self.d.transport(action);
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
        let id = self.d.request_id();
        let replies = self.d.send(message(id));
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
        let revision = self.d.with(|c| c.revision());
        let outcome = self.send(|id| ClientMessage::Command {
            id,
            command: Box::new(command),
            coalesce: None,
        });
        match outcome {
            Outcome::Ok(_) => {
                self.report.edits_applied += 1;
                // An edit that sets what is already there is accepted but is no undo step.
                if self.d.with(|c| c.revision()) != revision {
                    self.undo_depth += 1;
                    self.redo_depth = 0;
                }
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
        let present: Vec<CueId> = self.d.with(|c| {
            c.workspace().cue_lists[0]
                .cues
                .iter()
                .map(|c| c.id)
                .collect()
        });
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

        let snap = self.d.with(|c| c.snapshot());
        assert!(
            snap.cue_states.is_empty(),
            "cues left over: {:?}",
            snap.cue_states
        );
        assert!(snap.previewing.is_empty());
        let mut q = self.d.with(Quiet::of);
        assert!(q.idle, "voices or reports left at block {}", self.block);
        assert_eq!(q.voices, 0);
        // Primed streams (the cues after the playhead, kept ready) are the only ones that may
        // remain: every other stream was released and closed.
        assert_eq!(q.held, q.primed, "a stream leaked");
        assert!(q.held <= openlab_engine::control::MAX_PRIMES);
        // A disk worker closes a released stream on its own thread, a moment after the handle
        // is dropped: wait for it (a hang guard, as the audio crate's own tests do). A stream
        // that is never closed is the leak this looks for.
        let deadline = Instant::now() + SETTLE;
        while q.open != q.held && Instant::now() < deadline {
            std::thread::yield_now();
            q = self.d.with(Quiet::of);
        }
        assert_eq!(q.open, q.held, "a stream is open but not held");
        assert_eq!(q.deferred, 0);
        assert_eq!(q.mixer_voices, 0, "the mixer still holds voices");
        assert_eq!(
            q.cues,
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
        self.report.revision = self.d.with(|c| c.revision());
    }

    /// Arms what the operator disarmed, so every cue is back in play for the next stretch.
    fn rearm(&mut self) {
        let armed: Vec<Option<bool>> = self.d.with(|c| {
            DISARMABLE
                .iter()
                .map(|&n| {
                    let mut armed = None;
                    for cue in &c.workspace().cue_lists[0].cues {
                        find(cue, id(n), &mut armed);
                    }
                    armed
                })
                .collect()
        });
        for (n, armed) in DISARMABLE.into_iter().zip(armed) {
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
        let base = self.d.start_probe();
        let at = {
            let at = self.d.now();
            self.transport("heartbeat", TransportAction::Start { cue: id(HEART) });
            at
        };
        self.run_blocks(blocks(1.0));
        self.drain();
        let captured = self.d.take_probe();
        let first = captured.iter().position(|&x| x != 0.0);
        let played = captured.iter().filter(|&&x| x != 0.0).count();
        let onset = first.map(|i| base + i as u64);
        if let Some(o) = onset {
            self.heartbeat_lag.push(o.saturating_sub(at));
        }
        assert!(
            onset.is_some_and(|o| (at..=at + D::START_SLACK).contains(&o)),
            "the heartbeat did not start on its scheduled sample: sent at {at}, first heard at \
             {onset:?}, slack {}",
            D::START_SLACK
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

    pub fn run(&mut self, total_secs: u64, checkpoint_secs: u64, mut on_hour: impl FnMut(u64)) {
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
                next_action = self.block + 1 + self.rng.below(self.action_gap as usize) as u64;
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
/// What a checkpoint reads of the control loop after a panic and a quiet spell.
struct Quiet {
    idle: bool,
    voices: usize,
    held: usize,
    primed: usize,
    open: usize,
    deferred: usize,
    mixer_voices: usize,
    cues: usize,
}

impl Quiet {
    fn of(c: &mut Control) -> Self {
        Self {
            idle: c.is_idle(),
            voices: c.live_voices(),
            held: c.held_streams(),
            primed: c.primed().len(),
            open: c.open_streams(),
            deferred: c.deferred(),
            mixer_voices: c.mixer_voice_count(),
            cues: c.workspace().cue_lists[0].cues.iter().map(count).sum(),
        }
    }
}

/// The run must have done what it claims, or it proves nothing. A long run (ten virtual minutes
/// or more) runs at least 35 of the 41 cues and redoes an edit; a short one, 20 cues.
pub fn assert_realistic(r: &Report, long_run: bool) {
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
    assert!(r.edits_applied > 0 && r.undone > 0, "{r:?}");
    // A redo needs an undo before it: a run of a minute or two may never get to one.
    assert!(!long_run || r.redone > 0, "{r:?}");
    let min_cues = if long_run { 35 } else { 20 };
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
    // Nearly every cue ran in a long run (41 in all): the loops, devamps, fades, chains and
    // groups are in play.
    assert!(r.cues_run >= min_cues, "only {} cues ever ran", r.cues_run);
}

pub fn find(c: &Cue, want: CueId, out: &mut Option<bool>) {
    if c.id == want {
        *out = Some(c.armed);
    }
    if let CueKind::Group { children, .. } = &c.kind {
        for child in children {
            find(child, want, out);
        }
    }
}
