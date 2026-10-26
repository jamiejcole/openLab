// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

//! Property tests for the cue engine (M2.9, QA-01, QA-02): random workspaces and random input
//! sequences against a simulated mixer, checked against the rules of `docs/cue-engine.md`.
//!
//! **Generator.** Lists with nested groups of every mode, control and Fade cues with random
//! targets (so Start/GoTo cycles, self-targets and missing targets), duplicate ids (more
//! cycles), and random input sequences: every operation, model edits (deletes, moves out of
//! groups, reorders, mode and timing changes), Arm/Disarm changes applied to the model as the
//! binary does, Panic, Pause All / Resume All, random media and device availability (static
//! broken cues), previews, and faults of the simulated mixer (a voice that ends early or fails).
//!
//! **Simulated mixer.** [`Sim`] answers the engine's voice actions like the real mixer would,
//! on virtual time: a voice ends after its length (end reports are delivered late, at the next
//! call, with the true end sample), a looping voice plays until it is devamped or stopped, a
//! devamp is answered with the sample the loop is left on, one kind of voice fails part-way,
//! stops (with or without a fade) end the voice and are reported, paused voices stay put. It
//! also checks the actions against what a mixer needs: commands name voices it knows, pause and
//! resume carry the position the voice has really reached, a voice is started once.
//!
//! **Invariants** (each cites its rule in `docs/cue-engine.md`):
//! - Exactly one state per cue, the one last reported; every runtime is reachable from the
//!   reported state and the other way round (States; `cue_states`); run ids are unique, and a
//!   deadline always belongs to a visible cue.
//! - The playhead is past the end or on a position of its list (Playhead).
//! - A running group has a busy child (Groups, "Its action").
//! - Chain bound: the calls at one `now` start at most `chain_limit` cues between them (Chains
//!   and the chain bound).
//! - Commands for one voice never go backwards in time, and none is dated outside the call's
//!   window `now ..= now + lookahead - 1` (Deadlines, lookahead and late ticks).
//! - Everything due has been done after each call (`is_due`).
//! - A second trigger on an active cue never restarts it (rule 1 / GO and double-GO), except a
//!   cue fading out in a panic.
//! - After a panic has ended, nothing is active, loaded or pending, and no voice is left in the
//!   mixer (Panic, "After it").
//! - No voice leaks: every voice the mixer holds belongs to the run the engine reports, and
//!   every running audio cue has its voice (Feedback; Operations).
//! - Liveness: once inputs stop and time advances, either something is still scheduled (a timed
//!   loop is legitimate) or every cue is idle, loaded, broken, paused, an audio cue whose voice
//!   loops until stopped, or a group around one — never stuck pre-waiting, post-waiting,
//!   stopping or running on nothing (Timing and continue modes; Devamp). And stopping every cue
//!   ends everything, resetting every cue clears everything but static problems.
//! - Previews are invisible to the show: the same run without them gives the same show actions
//!   (Preview), and a preview and its mixer voice exist together.
//! - Determinism: the same case twice gives identical actions (Shape).
//!
//! `broken_cues_never_auto_follow` covers the auto-follow rule (rule 2) on lists where every
//! start but the first can only come from a continue. Undo and redo of commands are
//! property-tested in `commands_prop.rs`.
//!
//! The case count is CI-friendly (256); set `OPENLAB_PROP_CASES` to run more, e.g.
//! `OPENLAB_PROP_CASES=100000 cargo test --release -p openlab-core --test cue_engine_prop`.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::*;
use openlab_core::*;
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

type Check = Result<(), TestCaseError>;

fn cases() -> u32 {
    std::env::var("OPENLAB_PROP_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256)
}

// ----- generator -----

#[derive(Clone, Debug)]
struct Spec {
    kind: u8,
    pre: u8,
    post: u8,
    mode: u8,
    armed: bool,
    /// Reuse the previous cue's id.
    dup: bool,
    /// Nesting level wanted (clamped to the groups open at this point).
    level: u8,
    /// Group mode, for groups.
    group_mode: u8,
    /// Control and Fade cues: which cue they target (by position; past the end = missing).
    target: u8,
    flag: bool,
}

#[derive(Clone, Debug)]
struct Step {
    dt: u64,
    op: u8,
    a: u8,
    b: u8,
}

#[derive(Clone, Debug)]
struct Case {
    specs: Vec<Spec>,
    steps: Vec<Step>,
    lookahead: u64,
    min_go: u8,
    panic: u8,
}

fn spec() -> impl Strategy<Value = Spec> {
    (
        (0u8..31, 0u8..3, 0u8..3, 0u8..3, any::<bool>()),
        (
            prop::bool::weighted(0.04),
            0u8..4,
            0u8..7,
            any::<u8>(),
            any::<bool>(),
        ),
    )
        .prop_map(
            |((kind, pre, post, mode, armed), (dup, level, group_mode, target, flag))| Spec {
                kind,
                pre,
                post,
                mode,
                armed,
                dup,
                level,
                group_mode,
                target,
                flag,
            },
        )
}

/// The operations a step can be, with repeats for the ones that matter most: most of the show is
/// GO and Start, with stops, pauses and resumes in between.
const OPS: [u8; 52] = [
    1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 0, 0, 0, 3, 3, 3, 4, 4, 4, 5, 5, 5, 6, 7, 8, 9, 10, 11, 12, 12,
    13, 14, 15, 16, 17, 18, 19, 20, 21, 21, 22, 22, 23, 24, 25, 25, 26, 27, 28, 20, 4, 5,
];

fn step() -> impl Strategy<Value = Step> {
    // Mostly close together, so inputs land inside waits, fades and each other.
    let dt = prop_oneof![3 => 0u64..20, 2 => 0u64..1500, 1 => 0u64..4000];
    (dt, 0..OPS.len(), any::<u8>(), any::<u8>()).prop_map(|(dt, op, a, b)| Step {
        dt,
        op: OPS[op],
        a,
        b,
    })
}

fn case() -> impl Strategy<Value = Case> {
    (
        prop::collection::vec(spec(), 0..12),
        prop::collection::vec(step(), 1..40),
        // The lookahead commits deadlines inside its window; a long one (up to 0.4 s) puts
        // several commands for one voice there at once.
        prop_oneof![2 => 0u64..32, 1 => 0u64..400],
        0u8..3,
        0u8..3,
    )
        .prop_map(|(specs, steps, lookahead, min_go, panic)| Case {
            specs,
            steps,
            lookahead,
            min_go,
            panic,
        })
}

fn mode(n: u8) -> ContinueMode {
    match n % 3 {
        0 => ContinueMode::DoNotContinue,
        1 => ContinueMode::AutoContinue,
        _ => ContinueMode::AutoFollow,
    }
}

fn group_mode(n: u8) -> GroupMode {
    match n % 7 {
        0 => GroupMode::Timeline,
        1 => GroupMode::StartFirstAndEnter,
        2 => GroupMode::StartFirstAndGoToNext,
        3 => GroupMode::StartRandom,
        4 => GroupMode::Playlist {
            loop_: false,
            shuffle: false,
        },
        5 => GroupMode::Playlist {
            loop_: true,
            shuffle: false,
        },
        _ => GroupMode::Playlist {
            loop_: true,
            shuffle: true,
        },
    }
}

fn attach(stack: &mut [Cue], top: &mut Vec<Cue>, c: Cue) {
    match stack.last_mut().map(|g| &mut g.kind) {
        Some(CueKind::Group { children, .. }) => children.push(c),
        _ => top.push(c),
    }
}

fn build(specs: &[Spec]) -> Vec<Cue> {
    let mut top: Vec<Cue> = Vec::new();
    // Groups still open, outermost first.
    let mut open: Vec<Cue> = Vec::new();
    let mut last_id: Option<u128> = None;
    for (i, s) in specs.iter().enumerate() {
        let n = match last_id {
            Some(last) if s.dup => last,
            _ => i as u128 + 1,
        };
        last_id = Some(n);
        // Targets name cues by position, so cycles and self-targets come up; past the end is a
        // missing target. Most of them name an audio cue, or a group, which is what control and
        // Fade cues act on.
        let of_kind = |kinds: &[u8]| -> Vec<u128> {
            specs
                .iter()
                .enumerate()
                .filter(|(_, o)| kinds.contains(&o.kind))
                .map(|(j, _)| j as u128 + 1)
                .collect()
        };
        let audio_specs = of_kind(&[2, 3, 14, 15, 16, 17, 19, 20, 21]);
        let group_specs = of_kind(&[4, 18, 22]);
        let t = usize::from(s.target);
        let picked = match t % 4 {
            0 | 1 => audio_specs.get(t / 4 % audio_specs.len().max(1)).copied(),
            2 => group_specs.get(t / 4 % group_specs.len().max(1)).copied(),
            // The cue just before: [sound, Devamp / Fade / Stop] with a continue between.
            _ => (i > 0).then_some(i as u128),
        };
        let target = Some(cue_id(
            picked.unwrap_or(u128::from(s.target) % (specs.len() as u128 + 2) + 1),
        ));
        let kind = match s.kind {
            0 => CueKind::Memo,
            1 => CueKind::Wait {
                duration: f64::from(s.post) * 0.7,
            },
            2 | 3 | 14..=17 | 19..=21 => audio(),
            // Static problems of its own (no media file), for the auto-follow rule below.
            31 => {
                let mut k = audio();
                if let CueKind::Audio { media, .. } = &mut k {
                    *media = None;
                }
                k
            }
            4 => CueKind::Group {
                mode: group_mode(s.group_mode),
                children: vec![],
            },
            5 => CueKind::Start { target },
            6 => CueKind::Stop {
                target,
                fade_out: FadeSpec {
                    duration: f64::from(s.pre) * 0.3,
                    curve: FadeCurve::Linear,
                },
            },
            7 => CueKind::Pause { target },
            8 => CueKind::Load {
                target,
                load_to_time: f64::from(s.post) * 0.4,
            },
            9 => CueKind::Reset { target },
            10 | 23 | 24 => CueKind::Devamp {
                target,
                start_next_cue: s.flag,
                stop_target_when_slice_ends: s.armed,
            },
            11 => CueKind::GoTo {
                target,
                then_go: s.flag,
            },
            12 if s.flag => CueKind::Arm { target },
            12 => CueKind::Disarm { target },
            13 | 25 | 26 => CueKind::Fade {
                target,
                duration: f64::from(s.post) * 0.6,
                curve: FadeCurve::SCurve,
                levels: FadeLevels {
                    master_db: Some(f64::from(s.group_mode) * -10.0),
                    output_db: vec![Some(-3.0), None, Some(-6.0)],
                    crosspoints: vec![FadeCrosspoint {
                        input: u32::from(s.level),
                        output: u32::from(s.pre),
                        db: -12.0,
                    }],
                },
                stop_target_when_done: s.flag,
            },
            _ => CueKind::Group {
                mode: group_mode(s.group_mode),
                children: vec![],
            },
        };
        // Kinds 27..=30 are a looping sound followed by a cue that acts on it, the way shows use
        // them (devamp the loop, fade it out, stop it with a fade, pause it): the sound
        // continues into that cue after a short post-wait.
        let pair = (27..=30).contains(&s.kind).then(|| {
            let sound = 2000 + 6 * i as u128;
            let acts = match s.kind {
                27 => CueKind::Devamp {
                    target: Some(cue_id(sound)),
                    start_next_cue: s.flag,
                    stop_target_when_slice_ends: s.armed,
                },
                28 => CueKind::Fade {
                    target: Some(cue_id(sound)),
                    duration: f64::from(s.post) * 0.3,
                    curve: FadeCurve::EqualPower,
                    levels: FadeLevels {
                        master_db: Some(-20.0),
                        output_db: vec![],
                        crosspoints: vec![],
                    },
                    stop_target_when_done: s.flag,
                },
                29 => CueKind::Stop {
                    target: Some(cue_id(sound)),
                    fade_out: FadeSpec {
                        duration: f64::from(s.pre) * 0.2,
                        curve: FadeCurve::SCurve,
                    },
                },
                _ => CueKind::Pause {
                    target: Some(cue_id(sound)),
                },
            };
            (sound, cue(sound + 1, acts))
        });
        let mut c = match &pair {
            Some((sound, _)) => cue(*sound, audio()),
            None => cue(n, kind),
        };
        c.number = Some(format!("{i}"));
        c.pre_wait = f64::from(s.pre) * 0.5;
        c.post_wait = f64::from(s.post) * 0.5;
        c.continue_mode = mode(s.mode);
        c.armed = s.armed;
        if pair.is_some() {
            c.post_wait = 0.1 + f64::from(s.post) * 0.1;
            c.continue_mode = ContinueMode::AutoContinue;
            c.armed = true;
        }
        let level = usize::from(s.level).min(open.len());
        while open.len() > level {
            let g = open.pop().unwrap();
            attach(&mut open, &mut top, g);
        }
        if matches!(c.kind, CueKind::Group { .. }) {
            open.push(c);
        } else {
            attach(&mut open, &mut top, c);
        }
        if let Some((_, acts)) = pair {
            attach(&mut open, &mut top, acts);
        }
    }
    while let Some(g) = open.pop() {
        attach(&mut open, &mut top, g);
    }
    top
}

/// Every cue, depth-first.
fn flatten(cues: &[Cue], out: &mut Vec<CueId>) {
    for c in cues {
        out.push(c.id);
        flatten(c.children(), out);
    }
}

fn all_ids(ws: &Workspace) -> Vec<CueId> {
    let mut out = Vec::new();
    flatten(&ws.cue_lists[0].cues, &mut out);
    out
}

/// Removes the first cue with this id, anywhere.
fn take(cues: &mut Vec<Cue>, id: CueId) -> Option<Cue> {
    if let Some(i) = cues.iter().position(|c| c.id == id) {
        return Some(cues.remove(i));
    }
    cues.iter_mut().find_map(|c| match &mut c.kind {
        CueKind::Group { children, .. } => take(children, id),
        _ => None,
    })
}

fn find_mut(cues: &mut [Cue], id: CueId) -> Option<&mut Cue> {
    for c in cues {
        if c.id == id {
            return Some(c);
        }
        if let CueKind::Group { children, .. } = &mut c.kind
            && let Some(found) = find_mut(children, id)
        {
            return Some(found);
        }
    }
    None
}

fn pick(ws: &Workspace, a: u8) -> CueId {
    let ids = all_ids(ws);
    if ids.is_empty() || a == u8::MAX {
        cue_id(999)
    } else {
        ids[usize::from(a) % ids.len()]
    }
}

// ----- simulated mixer -----

/// What a voice does, from its cue's id: a loop that plays until devamped or stopped, voices
/// of fixed lengths (some shorter than a Wait, some outliving most scenarios), and one that
/// fails part-way.
#[derive(Clone, Copy, Debug)]
enum Flavor {
    Loop,
    Finite(u64),
    FailsAfter { len: u64, after: u64 },
}

fn flavor(cue: CueId) -> Flavor {
    match cue.0.as_u128() % 6 {
        0 | 2 | 4 => Flavor::Loop,
        1 => Flavor::Finite(300),
        3 => Flavor::Finite(2500),
        _ => Flavor::FailsAfter {
            len: 1400,
            after: 500,
        },
    }
}

#[derive(Clone, Copy, Debug)]
enum Cmd {
    Start {
        offset: u64,
    },
    Stop {
        frames: u64,
    },
    Pause {
        offset: u64,
    },
    Resume {
        run: RunId,
        offset: u64,
    },
    Devamp {
        stop: bool,
    },
    /// Fault injection: the voice ends early / fails.
    ForceEnd,
    ForceFail(BrokenReason),
}

#[derive(Clone, Debug)]
struct Voice {
    cue: CueId,
    run: RunId,
    preview: bool,
    flavor: Flavor,
    started: bool,
    playing: bool,
    /// Position (samples into the action) at sample `since`, while playing.
    pos: u64,
    since: SampleTime,
    /// Where a devamped loop is left, and where the voice ends.
    leave: Option<u64>,
    end_pos: Option<u64>,
    fail_pos: Option<u64>,
    stop_end: Option<SampleTime>,
    cmds: Vec<(SampleTime, u64, Cmd)>,
    dead: bool,
}

impl Voice {
    fn new(cue: CueId, run: RunId, preview: bool) -> Self {
        Self {
            cue,
            run,
            preview,
            flavor: flavor(cue),
            started: false,
            playing: false,
            pos: 0,
            since: 0,
            leave: None,
            end_pos: None,
            fail_pos: None,
            stop_end: None,
            cmds: Vec::new(),
            dead: false,
        }
    }

    fn has_pending_stop(&self) -> bool {
        self.cmds
            .iter()
            .any(|(_, _, c)| matches!(c, Cmd::Stop { .. }))
    }

    /// The next thing that happens by itself while playing: `(sample, end?, fail reason)`.
    fn natural(&self) -> Option<(SampleTime, Nat)> {
        if !self.started || !self.playing {
            return None;
        }
        let mut best: Option<(SampleTime, Nat)> = None;
        let mut offer = |t: SampleTime, n: Nat| {
            if best.is_none_or(|(b, _)| t < b) {
                best = Some((t, n));
            }
        };
        if let Some(e) = self.end_pos {
            offer(self.since + e.saturating_sub(self.pos), Nat::End);
        }
        if let Some(f) = self.fail_pos {
            offer(self.since + f.saturating_sub(self.pos), Nat::Fail);
        }
        if let Some(s) = self.stop_end {
            offer(s, Nat::End);
        }
        best
    }

    fn next_time(&self) -> Option<SampleTime> {
        if self.dead {
            return None;
        }
        let cmd = self.cmds.first().map(|c| c.0);
        let nat = self.natural().map(|n| n.0);
        match (cmd, nat) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// A voice that has ended: of what is still on its way, only a resume does anything, and
    /// it reports the end again, with the fresh id.
    fn drain_dead(&mut self, t: SampleTime, events: &mut Vec<(SampleTime, Input)>) {
        while self.cmds.first().is_some_and(|c| c.0 <= t) {
            let (at, _, cmd) = self.cmds.remove(0);
            if let Cmd::Resume { run, .. } = cmd {
                self.run = run;
                events.push((
                    at,
                    Input::ActionEnded {
                        cue: self.cue,
                        run,
                        at,
                    },
                ));
            }
        }
    }

    fn advance(
        &mut self,
        t: SampleTime,
        events: &mut Vec<(SampleTime, Input)>,
        errors: &mut Vec<String>,
    ) {
        loop {
            if self.dead {
                return;
            }
            let cmd_t = self.cmds.first().map(|c| c.0);
            let nat = self.natural();
            // Commands win ties: a pause on the sample the voice would end keeps it.
            let take_cmd = match (cmd_t, nat) {
                (Some(c), Some((n, _))) => c <= t && c <= n,
                (Some(c), None) => c <= t,
                (None, _) => false,
            };
            if take_cmd {
                let (at, _, cmd) = self.cmds.remove(0);
                self.command(at, cmd, events, errors);
                continue;
            }
            match nat {
                Some((n, Nat::End)) if n <= t => {
                    events.push((
                        n,
                        Input::ActionEnded {
                            cue: self.cue,
                            run: self.run,
                            at: n,
                        },
                    ));
                    self.dead = true;
                }
                Some((n, Nat::Fail)) if n <= t => {
                    // The disk stream failed; the voice itself plays on (a failed stream still
                    // plays out its ring) until the engine stops it.
                    events.push((
                        n,
                        Input::ActionFailed {
                            cue: self.cue,
                            run: self.run,
                            reason: BrokenReason::StreamFailed,
                        },
                    ));
                    self.fail_pos = None;
                }
                _ => return,
            }
        }
    }

    fn command(
        &mut self,
        at: SampleTime,
        cmd: Cmd,
        events: &mut Vec<(SampleTime, Input)>,
        errors: &mut Vec<String>,
    ) {
        if self.playing {
            self.pos += at.saturating_sub(self.since);
            self.since = self.since.max(at);
        }
        match cmd {
            Cmd::Start { offset } => {
                self.started = true;
                self.playing = true;
                self.pos = offset;
                self.since = at;
                match self.flavor {
                    Flavor::Loop => {}
                    Flavor::Finite(len) => self.end_pos = Some(len),
                    Flavor::FailsAfter { len, after } => {
                        self.end_pos = Some(len);
                        self.fail_pos = Some(after);
                    }
                }
            }
            Cmd::Pause { offset } => {
                if self.started && self.playing {
                    if offset != self.pos {
                        errors.push(format!(
                            "{:?}: paused at {at} at position {} but the action says {offset}",
                            self.run, self.pos
                        ));
                    }
                    self.playing = false;
                }
            }
            Cmd::Resume { run, offset } => {
                if self.started && !self.playing {
                    if offset != self.pos {
                        errors.push(format!(
                            "{:?}: resumed at {at} from position {} but the action says {offset}",
                            self.run, self.pos
                        ));
                    }
                    self.playing = true;
                    self.since = at;
                    self.run = run;
                }
            }
            Cmd::Stop { frames } => {
                if frames == 0 || !self.playing {
                    // A paused voice is silent already: any stop ends it at once.
                    events.push((
                        at,
                        Input::ActionEnded {
                            cue: self.cue,
                            run: self.run,
                            at,
                        },
                    ));
                    self.dead = true;
                } else {
                    self.stop_end = Some(at + frames);
                }
            }
            Cmd::Devamp { stop } => {
                let pos = self.pos;
                let answer = match (self.flavor, self.leave) {
                    (Flavor::Loop, None) if self.started => {
                        let leave = pos + 100;
                        self.leave = Some(leave);
                        self.end_pos = Some(leave + if stop { 0 } else { 200 });
                        Some(leave)
                    }
                    (Flavor::Loop, Some(leave)) if pos < leave => Some(leave),
                    _ => None,
                };
                events.push((
                    at,
                    match answer {
                        Some(leave) => Input::DevampScheduled {
                            cue: self.cue,
                            run: self.run,
                            at: at + (leave - pos),
                        },
                        None => Input::DevampIgnored {
                            cue: self.cue,
                            run: self.run,
                        },
                    },
                ));
            }
            // A paused voice cannot end by itself.
            Cmd::ForceEnd if !self.playing => {}
            Cmd::ForceEnd => {
                events.push((
                    at,
                    Input::ActionEnded {
                        cue: self.cue,
                        run: self.run,
                        at,
                    },
                ));
                self.dead = true;
            }
            Cmd::ForceFail(reason) => {
                events.push((
                    at,
                    Input::ActionFailed {
                        cue: self.cue,
                        run: self.run,
                        reason,
                    },
                ));
                // A rejected voice never existed, a lost device took its voices; a failed
                // stream leaves the voice playing.
                if reason != BrokenReason::StreamFailed {
                    self.dead = true;
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Nat {
    End,
    Fail,
}

/// The mixer as the engine binary sees it.
#[derive(Default)]
struct Sim {
    /// Routed by the id the engine addresses now (a resume re-keys it at once).
    voices: BTreeMap<RunId, Voice>,
    loaded: BTreeMap<RunId, CueId>,
    /// Every run the mixer has ever been told about.
    known: BTreeSet<RunId>,
    /// Voices that have ended but still have commands on their way (a pause and a resume
    /// committed in the same window). The end of a voice that the engine thought paused was
    /// dated on the pause sample, which the engine takes for the pause being carried out, so
    /// the binary reports it again when the voice is resumed, with the fresh id (Feedback).
    dead: BTreeMap<RunId, Voice>,
    /// Voices that have ended and have nothing left on their way, for a resume that comes
    /// later still.
    ended: BTreeMap<RunId, CueId>,
    late: Vec<(SampleTime, Input)>,
    seq: u64,
    errors: Vec<String>,
}

impl Sim {
    fn queue(&mut self, run: RunId, at: SampleTime, cmd: Cmd) {
        self.seq += 1;
        let seq = self.seq;
        match self
            .voices
            .get_mut(&run)
            .or_else(|| self.dead.get_mut(&run))
        {
            Some(v) => {
                let i = v
                    .cmds
                    .iter()
                    .position(|(t, s, _)| (*t, *s) > (at, seq))
                    .unwrap_or(v.cmds.len());
                v.cmds.insert(i, (at, seq, cmd));
            }
            // A command for a voice that already ended is a no-op, as in the mixer.
            None => {
                if !self.known.contains(&run) {
                    self.errors
                        .push(format!("{cmd:?} for unknown voice {run:?}"));
                }
            }
        }
    }

    /// Takes the engine's voice actions.
    fn apply(&mut self, actions: &[Action]) {
        for action in actions {
            match action {
                Action::LoadVoice { cue, run, .. } => {
                    self.known.insert(*run);
                    self.loaded.insert(*run, *cue);
                }
                Action::UnloadVoice { run, .. } => {
                    if self.loaded.remove(run).is_none() {
                        self.errors
                            .push(format!("unload of {run:?} which is not loaded"));
                    }
                }
                Action::StartVoice {
                    cue,
                    run,
                    at,
                    offset,
                } => {
                    self.loaded.remove(run);
                    self.start(*cue, *run, *at, *offset, false);
                }
                Action::PreviewStart { cue, run, at } => self.start(*cue, *run, *at, 0, true),
                Action::StopVoice { run, at, fade, .. }
                | Action::PreviewStop { run, at, fade, .. } => {
                    let frames = fade.map_or(0, |f| f.frames);
                    self.queue(*run, *at, Cmd::Stop { frames });
                }
                Action::PauseVoice {
                    run, at, offset, ..
                } => self.queue(*run, *at, Cmd::Pause { offset: *offset }),
                Action::ResumeVoice {
                    run,
                    paused,
                    at,
                    offset,
                    ..
                } => {
                    self.known.insert(*run);
                    self.queue(
                        *paused,
                        *at,
                        Cmd::Resume {
                            run: *run,
                            offset: *offset,
                        },
                    );
                    // Route what comes next for the fresh id to the same voice.
                    if let Some(v) = self.voices.remove(paused) {
                        self.voices.insert(*run, v);
                    } else if let Some(v) = self.dead.remove(paused) {
                        self.dead.insert(*run, v);
                    } else if let Some(cue) = self.ended.remove(paused) {
                        self.ended.insert(*run, cue);
                        self.late.push((
                            *at,
                            Input::ActionEnded {
                                cue,
                                run: *run,
                                at: *at,
                            },
                        ));
                    }
                }
                Action::DevampVoice { run, at, stop, .. } => {
                    self.queue(*run, *at, Cmd::Devamp { stop: *stop });
                }
                _ => {}
            }
        }
    }

    fn start(&mut self, cue: CueId, run: RunId, at: SampleTime, offset: u64, preview: bool) {
        if self.voices.contains_key(&run) {
            self.errors.push(format!("{run:?} started twice"));
            return;
        }
        self.known.insert(run);
        self.voices.insert(run, Voice::new(cue, run, preview));
        self.queue(run, at, Cmd::Start { offset });
    }

    /// Runs every voice up to sample `t`; returns what the engine is told, earliest first.
    fn step_to(&mut self, t: SampleTime) -> Vec<Input> {
        let mut events = Vec::new();
        for v in self.voices.values_mut() {
            v.advance(t, &mut events, &mut self.errors);
        }
        let died: Vec<RunId> = self
            .voices
            .iter()
            .filter(|(_, v)| v.dead)
            .map(|(k, _)| *k)
            .collect();
        for key in died {
            if let Some(v) = self.voices.remove(&key) {
                self.dead.insert(key, v);
            }
        }
        for v in self.dead.values_mut() {
            v.drain_dead(t, &mut events);
        }
        let done: Vec<RunId> = self
            .dead
            .iter()
            .filter(|(_, v)| v.cmds.is_empty())
            .map(|(k, _)| *k)
            .collect();
        for key in done {
            if let Some(v) = self.dead.remove(&key) {
                self.ended.insert(key, v.cue);
            }
        }
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.late)
            .into_iter()
            .partition(|(at, _)| *at <= t);
        self.late = later;
        events.extend(due);
        events.sort_by_key(|(at, _)| *at);
        events.into_iter().map(|(_, e)| e).collect()
    }

    /// When the show next hears from the mixer. Previews are not the show's: they must not
    /// move the clock, or a run with previews and one without would not share a timeline.
    fn next_event_time(&self) -> Option<SampleTime> {
        // Reports already on their way (the end of a resumed voice that had ended) count too.
        let late = self.late.iter().filter(
            |(_, e)| !matches!(e, Input::ActionEnded { run, .. } if run.0 >= PREVIEW_RUN_BASE),
        );
        self.voices
            .values()
            .filter(|v| !v.preview)
            .filter_map(Voice::next_time)
            .chain(late.map(|(at, _)| *at))
            .min()
    }
}

// ----- the run -----

/// What the call was, for the rules that depend on it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// An operator input; `trigger` if it may start cues.
    Operator { trigger: bool },
    /// Mixer feedback about a show voice.
    Feedback,
    /// Preview input, or feedback about a preview voice: invisible to the show.
    PreviewSide,
    /// The tick on the sample a panic fade ends on.
    PanicEnd,
}

struct Outcome {
    /// Every action, in order.
    raw: Vec<Action>,
    /// What the show can notice: no preview actions, no warnings of preview calls.
    shown: Vec<Action>,
}

struct Run {
    ws: Workspace,
    engine: CueEngine,
    last: Option<Workspace>,
    sim: Sim,
    previews: bool,
    dup: bool,
    /// `broken_cues_never_auto_follow`: the cues that broke so far, and the rule being checked.
    follow_rule: bool,
    tainted: BTreeSet<CueId>,
    lookahead: u64,
    list: CueListId,
    now: SampleTime,
    /// A panic fade under way: its end, and whether no operator trigger came during it.
    watch: Option<(SampleTime, bool)>,
    /// The last sample the panic's call committed deadlines on.
    panic_horizon: SampleTime,
    /// The state of every cue as the reported changes tell it.
    reported: BTreeMap<CueId, CueState>,
    seen: BTreeSet<CueId>,
    /// The latest sample a command was sent for, per voice (run id), across calls.
    last_at: BTreeMap<RunId, SampleTime>,
    /// Cue starts at one `now` so far, across calls, and the largest chain limit then:
    /// `(now, starts, limit)`.
    starts_at: (SampleTime, usize, usize),
    input: Input,
    held: Vec<Input>,
    raw: Vec<Action>,
    shown: Vec<Action>,
}

fn preview_action(a: &Action) -> bool {
    matches!(
        a,
        Action::PreviewStart { .. } | Action::PreviewStop { .. } | Action::PreviewChanged { .. }
    )
}

impl Run {
    fn new(case: &Case, previews: bool) -> Self {
        let mut ws = workspace(build(&case.specs));
        ws.settings.min_go_interval = f64::from(case.min_go) * 0.3;
        ws.settings.panic_duration = [0.0, 0.01, 0.4][usize::from(case.panic)];
        let ids = all_ids(&ws);
        let seen: BTreeSet<CueId> = ids.iter().copied().collect();
        let dup = seen.len() != ids.len();
        Self {
            ws,
            engine: CueEngine::new(EngineConfig::new(1000, case.lookahead, 7).unwrap()),
            last: None,
            sim: Sim::default(),
            previews,
            dup,
            follow_rule: false,
            tainted: BTreeSet::new(),
            lookahead: case.lookahead,
            list: list_id(1),
            now: 0,
            watch: None,
            panic_horizon: 0,
            reported: BTreeMap::new(),
            seen,
            last_at: BTreeMap::new(),
            starts_at: (0, 0, 0),
            input: Input::Tick,
            held: Vec::new(),
            raw: Vec::new(),
            shown: Vec::new(),
        }
    }

    fn horizon(&self) -> SampleTime {
        self.now + self.lookahead.saturating_sub(1)
    }

    /// Whether every id has named one cue all along. A duplicate id shares one runtime between
    /// cues (of kinds that can differ, once the first of them is deleted), so the rules about
    /// what a cue's runtime holds do not apply to such a workspace; duplicates are there to make
    /// trigger cycles.
    fn unique_ids(&self) -> bool {
        !self.dup
    }

    /// One `handle` call, with the per-call rules.
    fn call(&mut self, input: Input, kind: Kind) -> Check {
        if kind == Kind::PreviewSide {
            // What is due is carried out in a call of its own, so the one that only concerns a
            // preview has nothing of the show's to mix its warnings with.
            self.call(Input::Tick, Kind::Operator { trigger: false })?;
        }
        self.input = input.clone();
        let now = self.now;
        let is_panic = input == Input::Panic;
        let actions = handle_as_binary(&mut self.engine, &self.ws, &mut self.last, input, now);
        // Deadlines in `now .. now + lookahead` are committed (end excluded).
        let horizon = self.horizon();
        if std::env::var_os("OPENLAB_PROP_TRACE").is_some() {
            eprintln!("t={now} {:?}\n    -> {:?}", self.input, actions);
        }
        self.raw.extend(actions.iter().cloned());
        self.shown.extend(
            actions
                .iter()
                .filter(|a| {
                    !preview_action(a)
                        && !(kind == Kind::PreviewSide && matches!(a, Action::Warning(_)))
                })
                .cloned(),
        );

        let mut starts = 0usize;
        let mut arm = Vec::new();
        for action in &actions {
            match action {
                Action::StateChanged { cue, state, at } => {
                    let previous = self.reported.get(cue).copied().unwrap_or(CueState::Idle);
                    prop_assert_ne!(
                        previous,
                        *state,
                        "repeated report of {} ({:?})",
                        cue,
                        self.input
                    );
                    if *state == CueState::PreWaiting && previous != CueState::Paused {
                        starts += 1;
                    }
                    // Deadlines in the window are committed; none beyond it.
                    prop_assert!(*at <= horizon, "state change dated {at} after {horizon}");
                    self.reported.insert(*cue, *state);
                    self.seen.insert(*cue);
                }
                Action::StartVoice { at, .. }
                | Action::StopVoice { at, .. }
                | Action::PauseVoice { at, .. }
                | Action::ResumeVoice { at, .. }
                | Action::Ramp { at, .. }
                | Action::DevampVoice { at, .. }
                | Action::PreviewStart { at, .. }
                | Action::PreviewStop { at, .. } => {
                    prop_assert!(
                        *at >= now && *at <= horizon,
                        "voice action {:?} dated {} outside {}..={}",
                        action,
                        at,
                        now,
                        horizon
                    );
                }
                Action::SetArmed { cue, armed } => arm.push((*cue, *armed)),
                _ => {}
            }
            // Commands for one voice never go backwards in time (committed ones cannot be
            // withdrawn or overtaken); a resume continues the paused voice's sequence.
            let timed = match action {
                Action::StartVoice { run, at, .. }
                | Action::StopVoice { run, at, .. }
                | Action::PauseVoice { run, at, .. }
                | Action::Ramp { run, at, .. }
                | Action::DevampVoice { run, at, .. }
                | Action::PreviewStart { run, at, .. }
                | Action::PreviewStop { run, at, .. } => Some((*run, *run, *at)),
                Action::ResumeVoice {
                    run, paused, at, ..
                } => Some((*paused, *run, *at)),
                _ => None,
            };
            if let Some((before, after, at)) = timed {
                let previous = self.last_at.get(&before).copied().unwrap_or(0);
                prop_assert!(
                    at >= previous,
                    "voice {:?} went back from {} to {}",
                    before,
                    previous,
                    at
                );
                self.last_at.insert(after, at);
            }
        }
        // Chain bound: across every call at one `now` (against the largest limit the workspace
        // had at that sample, as edits between those calls can change it).
        let limit = CueEngine::chain_limit(&self.ws);
        if self.starts_at.0 != now {
            self.starts_at = (now, 0, 0);
        }
        self.starts_at.1 += starts;
        self.starts_at.2 = self.starts_at.2.max(limit);
        prop_assert!(
            self.starts_at.1 <= self.starts_at.2,
            "{} starts at {} (limit {})",
            self.starts_at.1,
            now,
            self.starts_at.2
        );
        // Preview actions only for audio cues, with ids apart from the show's.
        for action in &actions {
            if let Action::PreviewStart { run, .. } | Action::PreviewStop { run, .. } = action {
                prop_assert!(run.0 >= PREVIEW_RUN_BASE);
            }
            if let Action::StartVoice { run, .. } | Action::LoadVoice { run, .. } = action {
                prop_assert!(run.0 < PREVIEW_RUN_BASE);
            }
        }
        self.sim.apply(&actions);
        prop_assert!(self.sim.errors.is_empty(), "{:?}", self.sim.errors);
        // Only audio cues have levels to fade.
        for (id, state) in self.engine.cue_states() {
            if state == CueState::Fading {
                let audio = matches!(
                    self.ws.find_cue(id).map(|c| &c.kind),
                    Some(CueKind::Audio { .. })
                );
                prop_assert!(audio, "cue {} fading", id);
            }
        }
        // The binary applies Arm/Disarm changes to the model before the next call (M4.2).
        for (cue, armed) in arm {
            if let Some(c) = find_mut(&mut self.ws.cue_lists[0].cues, cue) {
                c.armed = armed;
            }
        }

        // A broken cue never auto-follows (rule 2): in a list where only a cue's own continue
        // can start the cue after it (no auto-continue, no controls), a start that does not come
        // from a GO is the previous cue's auto-follow, so that cue must have run to its end.
        if self.follow_rule {
            for (id, state) in self.engine.cue_states() {
                if state == CueState::Broken {
                    self.tainted.insert(id);
                }
            }
            if !matches!(self.input, Input::Go { .. }) {
                let cues = &self.ws.cue_lists[0].cues;
                for action in &actions {
                    let Action::StateChanged {
                        cue,
                        state: CueState::PreWaiting,
                        ..
                    } = action
                    else {
                        continue;
                    };
                    let i = cues.iter().position(|c| c.id == *cue).unwrap();
                    prop_assert!(i > 0, "cue {} started with no GO", cue);
                    let before = &cues[i - 1];
                    prop_assert!(
                        before.continue_mode == ContinueMode::AutoFollow
                            && !self.tainted.contains(&before.id),
                        "cue {} started by the auto-follow of {} which {}",
                        cue,
                        before.id,
                        if self.tainted.contains(&before.id) {
                            "broke"
                        } else {
                            "does not follow"
                        }
                    );
                }
            }
        }
        // Panic bookkeeping (Panic, "After it").
        let trigger = matches!(kind, Kind::Operator { trigger: true });
        if kind == Kind::PanicEnd {
            if let Some((_, true)) = self.watch.take() {
                prop_assert!(self.engine.next_deadline().is_none());
                prop_assert!(self.engine.panic_end().is_none());
                for (id, state) in self.engine.cue_states() {
                    prop_assert!(!state.is_active(), "cue {} {:?} after a panic", id, state);
                }
                // The mixer holds no show voice either (a stop dated on the end sample has
                // been carried out by the time the sim reaches it).
                // (A voice committed to start beyond the end, in the lookahead window, is
                // stopped on its own start sample.)
                // (Nor is one whose own fade-out was committed to end inside that window.)
                for v in self.sim.voices.values() {
                    prop_assert!(
                        v.preview
                            || v.has_pending_stop()
                            || v.stop_end.is_some_and(|e| e <= self.panic_horizon),
                        "voice {:?} of {} survived a panic",
                        v.run,
                        v.cue
                    );
                }
            }
        } else if let Some((_, clean)) = self.watch.as_mut()
            && !is_panic
        {
            if *clean && !trigger {
                for action in &actions {
                    let started = matches!(
                        action,
                        Action::StartVoice { .. }
                            | Action::StateChanged {
                                state: CueState::PreWaiting,
                                ..
                            }
                    );
                    prop_assert!(
                        !started,
                        "{:?} during a panic fade ({:?})",
                        action,
                        self.input
                    );
                }
            }
            if trigger {
                *clean = false;
            }
        }
        if is_panic {
            self.panic_horizon = horizon;
            // Previews go with a panic (Preview).
            prop_assert!(self.engine.previewing().next().is_none());
            match self.engine.panic_end() {
                Some(end) => {
                    prop_assert!(end > now);
                    self.watch = Some((end, true));
                }
                None => {
                    // An immediate panic (or a second press): nothing left at once.
                    self.watch = None;
                    prop_assert!(self.engine.next_deadline().is_none());
                    for (id, state) in self.engine.cue_states() {
                        prop_assert_eq!(state, CueState::Broken, "cue {} after a panic", id);
                    }
                }
            }
        }
        Ok(())
    }

    /// Delivers what the mixer has to report by `now`, as the binary would: one call each.
    /// Reports about previews are held for [`Run::flush_previews`]: a call made only for them
    /// would carry out the show's deadlines at a moment the run without previews does not,
    /// before an edit the step is about to make.
    fn deliver(&mut self) -> Check {
        for _ in 0..10_000 {
            let events = self.sim.step_to(self.now);
            prop_assert!(self.sim.errors.is_empty(), "{:?}", self.sim.errors);
            if events.is_empty() {
                return Ok(());
            }
            for event in events {
                let preview = match &event {
                    Input::ActionEnded { run, .. } | Input::ActionFailed { run, .. } => {
                        run.0 >= PREVIEW_RUN_BASE
                    }
                    _ => false,
                };
                if preview {
                    self.held.push(event);
                } else {
                    self.call(event, Kind::Feedback)?;
                }
            }
        }
        prop_assert!(false, "the mixer kept reporting at {}", self.now);
        Ok(())
    }

    /// Reports what previews' voices did, after the step's own call.
    fn flush_previews(&mut self) -> Check {
        for event in std::mem::take(&mut self.held) {
            self.call(event, Kind::PreviewSide)?;
        }
        Ok(())
    }

    /// Moves the clock to `t`, stopping on the way where a panic fade ends.
    fn move_to(&mut self, t: SampleTime) -> Check {
        if let Some((end, _)) = self.watch
            && t >= end
            && end >= self.now
        {
            self.now = end;
            self.deliver()?;
            self.call(Input::Tick, Kind::PanicEnd)?;
            self.deliver()?;
            self.flush_previews()?;
            self.settle()?;
        }
        self.now = self.now.max(t);
        self.deliver()
    }

    /// The rules that hold between calls.
    fn settle(&mut self) -> Check {
        let now = self.now;
        let horizon = self.horizon();
        let list = self.list;
        // Exactly one state per cue, and it is the one last reported.
        for id in &self.seen {
            let reported = self.reported.get(id).copied().unwrap_or(CueState::Idle);
            prop_assert_eq!(
                self.engine.state(*id),
                reported,
                "cue {} ({:?})",
                id,
                self.input
            );
        }
        // Cues no longer in the workspace are idle.
        for (id, _) in self.engine.cue_states() {
            prop_assert!(self.ws.find_cue(id).is_some());
        }
        // The playhead is past the end or on a position of its list.
        if let Some(p) = self.engine.playhead(list) {
            prop_assert!(CueEngine::playhead_positions(&self.ws, list).contains(&p));
            prop_assert_eq!(
                self.engine.standing_by(&self.ws, list).map(|c| c.id),
                Some(p)
            );
        } else {
            prop_assert!(self.engine.standing_by(&self.ws, list).is_none());
        }
        // Everything due has been done.
        prop_assert!(self.engine.next_deadline().is_none_or(|d| d > horizon));
        prop_assert!(!self.engine.is_due(now));
        // Static problems and the broken state agree: an idle cue with problems is broken.
        for (id, problems) in self.engine.broken_cues() {
            prop_assert!(!problems.is_empty());
            prop_assert!(self.ws.find_cue(id).is_some());
            prop_assert_ne!(self.engine.state(id), CueState::Idle);
        }
        // A pending deadline belongs to a cue that reports it (not idle, loaded or paused).
        if self.engine.next_deadline().is_some() {
            let owner = self.engine.cue_states().any(|(_, s)| {
                matches!(
                    s,
                    CueState::PreWaiting
                        | CueState::Running
                        | CueState::Fading
                        | CueState::PostWaiting
                        | CueState::Stopping
                        | CueState::Broken
                )
            });
            prop_assert!(owner, "a deadline with no cue to own it");
        }
        // Previews and their voices exist together (Preview).
        for cue in self.engine.previewing() {
            let run = self.engine.preview_run(cue).unwrap();
            prop_assert!(run.0 >= PREVIEW_RUN_BASE);
            let v = self.sim.voices.get(&run);
            prop_assert!(
                v.is_some_and(|v| v.preview && v.cue == cue),
                "preview of {cue} has no voice"
            );
            let audio = matches!(
                self.ws.find_cue(cue).map(|c| &c.kind),
                Some(CueKind::Audio { .. })
            );
            prop_assert!(audio, "preview of {cue} which is not an audio cue");
        }
        for v in self.sim.voices.values().filter(|v| v.preview) {
            // A stop is carried out on its sample, which may still be ahead, and a stopped
            // preview is over for the engine while its fade-out is the mixer's.
            if !v.has_pending_stop() && v.stop_end.is_none() {
                prop_assert_eq!(
                    self.engine.preview_run(v.cue),
                    Some(v.run),
                    "preview voice of {} leaked",
                    v.cue
                );
            }
        }
        if !self.unique_ids() {
            // A duplicate id shares one runtime between cues; the rules below need ids that
            // name one cue.
            return Ok(());
        }
        let ids = all_ids(&self.ws);
        let mut runs = BTreeSet::new();
        for id in &ids {
            let Some(c) = self.ws.find_cue(*id) else {
                continue;
            };
            let state = self.engine.state(*id);
            let run = self.engine.run_of(*id);
            // Every runtime is reachable from the reported state, and the other way round
            // (States: idle cues have no runtime).
            match state {
                CueState::Idle => prop_assert_eq!(run, None, "idle cue {} has a runtime", id),
                CueState::Broken => {}
                _ => prop_assert!(run.is_some(), "{:?} cue {} has no runtime", state, id),
            }
            if let Some(run) = run {
                prop_assert!(run.0 < PREVIEW_RUN_BASE);
                prop_assert!(runs.insert(run), "run {:?} belongs to two cues", run);
            }
            // A running group has a child that is not idle or loaded.
            if matches!(c.kind, CueKind::Group { .. }) && state == CueState::Running {
                prop_assert!(
                    c.children().iter().any(|k| !matches!(
                        self.engine.state(k.id),
                        CueState::Idle | CueState::Loaded
                    )),
                    "group {} runs with no busy child",
                    id
                );
            }
            // A running audio cue has its voice in the mixer.
            if matches!(c.kind, CueKind::Audio { .. })
                && matches!(
                    state,
                    CueState::Running | CueState::Fading | CueState::Stopping
                )
            {
                // (A voice that has ended with a resume still on its way is reported again when
                // the resume comes. So is one that had ended before the engine, which had not
                // heard of it yet, paused and resumed it: its end with the fresh id is on its
                // way, dated on the resume, which can lie in the lookahead window.)
                let voice = run.and_then(|r| {
                    self.sim
                        .voices
                        .get(&r)
                        .or_else(|| self.sim.dead.get(&r).filter(|v| !v.cmds.is_empty()))
                });
                let end_on_its_way = run.is_some_and(|r| {
                    self.sim.late.iter().any(|(_, e)| {
                        matches!(e, Input::ActionEnded { cue, run, .. } if *cue == *id && *run == r)
                    })
                });
                prop_assert!(
                    end_on_its_way || voice.is_some_and(|v| v.cue == *id && !v.preview),
                    "{:?} audio cue {} has no voice in the mixer ({:?})",
                    state,
                    id,
                    self.input
                );
            }
        }
        // No voice leaks: every voice the mixer holds is the run the engine reports (unless a
        // stop for it is still ahead, or a fade-out ends inside the window).
        for (key, v) in self.sim.voices.iter().filter(|(_, v)| !v.preview) {
            let excused = v.has_pending_stop() || v.stop_end.is_some_and(|e| e <= horizon);
            if !excused {
                // (The key is the id the engine addresses; a resume re-keys the voice at once.)
                prop_assert_eq!(
                    self.engine.run_of(v.cue),
                    Some(*key),
                    "the mixer holds {:?} of {} that the engine forgot ({:?})",
                    key,
                    v.cue,
                    self.input
                );
            }
        }
        // Loaded streams are owned too.
        for (run, cue) in &self.sim.loaded {
            prop_assert_eq!(
                self.engine.run_of(*cue),
                Some(*run),
                "stream of {} leaked ({:?})",
                cue,
                self.input
            );
        }
        Ok(())
    }

    /// Applies a step's input and checks the rules that depend on what it was.
    fn step(&mut self, s: &Step) -> Check {
        self.move_to(self.now + s.dt)?;
        let list = self.list;
        // Half the time the target is a cue that is doing something (operations on idle cues
        // are mostly no-ops).
        let active: Vec<CueId> = self
            .engine
            .cue_states()
            .filter(|(_, st)| st.is_active())
            .map(|(id, _)| id)
            .collect();
        let target = if s.a.is_multiple_of(2) && !active.is_empty() {
            active[usize::from(s.a / 2) % active.len()]
        } else {
            pick(&self.ws, s.a / 2)
        };
        let mut kind = Kind::Operator { trigger: false };
        let input = match s.op {
            0 => Input::Tick,
            1 => {
                kind = Kind::Operator { trigger: true };
                Input::Go { list }
            }
            2 => {
                kind = Kind::Operator { trigger: true };
                Input::Start { cue: target }
            }
            3 => Input::Stop {
                cue: target,
                fade_out: (s.b % 2 == 1).then_some(FadeSpec {
                    duration: f64::from(s.b) / 100.0,
                    curve: FadeCurve::Linear,
                }),
            },
            4 => Input::Pause { cue: target },
            5 => Input::Resume { cue: target },
            6 => Input::Load {
                cue: target,
                time: f64::from(s.b) / 50.0,
            },
            7 => Input::Reset { cue: target },
            8 => Input::PlayheadNext { list },
            9 => Input::PlayheadPrevious { list },
            10 => Input::SetPlayhead {
                list,
                cue: (!s.b.is_multiple_of(4)).then_some(target),
            },
            11 => {
                // Model edit: delete a cue anywhere.
                take(&mut self.ws.cue_lists[0].cues, target);
                Input::Tick
            }
            12 | 13 => {
                // The mixer ends a voice early / fails one: a show voice, or a preview's.
                let run = if s.b % 2 == 1 {
                    self.engine.preview_run(target)
                } else {
                    self.engine.run_of(target)
                };
                let now = self.now;
                if let Some(run) = run.filter(|r| self.sim.voices.contains_key(r)) {
                    let cmd = if s.op == 12 {
                        Cmd::ForceEnd
                    } else {
                        Cmd::ForceFail(match s.b % 3 {
                            0 => BrokenReason::StreamFailed,
                            1 => BrokenReason::DeviceLost,
                            _ => BrokenReason::VoiceRejected,
                        })
                    };
                    self.sim.queue(run, now, cmd);
                }
                Input::Tick
            }
            15 => {
                // Model edit: move a cue to the end of the top level.
                if let Some(c) = take(&mut self.ws.cue_lists[0].cues, target) {
                    self.ws.cue_lists[0].cues.push(c);
                }
                Input::Tick
            }
            16 => {
                // Model edit: reverse a group's children, or change its mode.
                if let Some(CueKind::Group { mode, children }) =
                    find_mut(&mut self.ws.cue_lists[0].cues, target).map(|c| &mut c.kind)
                {
                    if s.b.is_multiple_of(2) {
                        children.reverse();
                    } else {
                        *mode = group_mode(s.b / 2);
                    }
                }
                Input::Tick
            }
            17 => Input::SetPlayheadByNumber {
                list,
                number: format!("{}", s.b % 12),
            },
            // Feedback for runs the mixer never had: ignored whatever it names.
            18 => Input::DevampScheduled {
                cue: target,
                run: RunId(u64::MAX - u64::from(s.b)),
                at: (self.now + u64::from(s.b) * 4).saturating_sub(300),
            },
            19 => Input::DevampIgnored {
                cue: target,
                run: RunId(u64::MAX - u64::from(s.b)),
            },
            20 => Input::Panic,
            21 => Input::PauseAll,
            22 => Input::ResumeAll,
            23 => Input::MediaStatus {
                media: MediaRef::new("media/a.wav"),
                // Mostly playable: a show that is broken from the start covers little.
                status: match s.b % 8 {
                    0 => MediaStatus::Unknown,
                    1 => MediaStatus::Missing,
                    2 => MediaStatus::Unsupported {
                        reason: "codec".into(),
                    },
                    _ => MediaStatus::Available,
                },
            },
            24 => Input::PatchStatus {
                patch: patch_id(1),
                resolution: if !s.b.is_multiple_of(4) {
                    PatchResolution::Mapped {
                        device: "D".into(),
                        channels: vec![0, 1],
                    }
                } else {
                    PatchResolution::DeviceMissing { device: "D".into() }
                },
            },
            25..=27 if !self.previews => Input::Tick,
            25 => {
                kind = Kind::PreviewSide;
                Input::Preview { cue: target }
            }
            26 => {
                kind = Kind::PreviewSide;
                Input::StopPreview { cue: Some(target) }
            }
            27 => {
                kind = Kind::PreviewSide;
                Input::StopPreview { cue: None }
            }
            28 => {
                // Feedback of a run that is not any cue's.
                let run = RunId(u64::MAX - u64::from(s.b));
                if s.b.is_multiple_of(2) {
                    Input::ActionEnded {
                        cue: target,
                        run,
                        at: self.now.saturating_sub(u64::from(s.b)),
                    }
                } else {
                    Input::ActionFailed {
                        cue: target,
                        run,
                        reason: BrokenReason::StreamFailed,
                    }
                }
            }
            _ => {
                // Model edit: change a cue's timing and continue mode.
                if let Some(c) = find_mut(&mut self.ws.cue_lists[0].cues, target) {
                    c.post_wait = f64::from(s.b % 4) * 0.25;
                    c.continue_mode = mode(s.b);
                }
                Input::Tick
            }
        };
        // A second trigger on an active cue never restarts it (rule 1; GO). Checked when
        // nothing else can touch the cue in the call: no deadline falls in the window, and a
        // cue fading out in a panic is not active for the rule.
        let trigger_on = match &input {
            Input::Start { cue } => Some(*cue),
            Input::Go { .. } => self.engine.playhead(list),
            _ => None,
        };
        let guard = trigger_on.filter(|cue| {
            let state = self.engine.state(*cue);
            let active = matches!(
                state,
                CueState::PreWaiting
                    | CueState::Running
                    | CueState::Fading
                    | CueState::PostWaiting
                    | CueState::Stopping
            );
            active
                && self.unique_ids()
                && self.engine.panic_end().is_none()
                && self
                    .engine
                    .next_deadline()
                    .is_none_or(|d| d > self.horizon())
        });
        let before = guard.map(|cue| (cue, self.engine.run_of(cue)));
        let start = self.raw.len();
        self.call(input, kind)?;
        if let Some((cue, run)) = before {
            prop_assert_eq!(
                self.engine.run_of(cue),
                run,
                "a second trigger restarted {}",
                cue
            );
            for a in &self.raw[start..] {
                let restarted = match a {
                    Action::StartVoice { cue: c, .. } => *c == cue,
                    Action::StateChanged {
                        cue: c,
                        state: CueState::PreWaiting,
                        ..
                    } => *c == cue,
                    _ => false,
                };
                prop_assert!(!restarted, "a second trigger restarted {}: {:?}", cue, a);
            }
        }
        self.deliver()?;
        self.flush_previews()?;
        self.settle()
    }

    /// Inputs stop, time advances, the mixer answers: either something is still scheduled (a
    /// timed loop) or nothing is stuck (Timing and continue modes; Devamp).
    fn drain(&mut self) -> Check {
        let end = self.now + 120_000;
        let mut quiet = false;
        for _ in 0..200_000 {
            let deadline = self.engine.next_deadline();
            let event = self.sim.next_event_time();
            let panic_end = self.watch.map(|(e, _)| e);
            let Some(t) = [deadline, event, panic_end].into_iter().flatten().min() else {
                quiet = true;
                break;
            };
            if t > end {
                break;
            }
            self.move_to(t.max(self.now))?;
            self.call(Input::Tick, Kind::Operator { trigger: false })?;
            self.deliver()?;
            self.flush_previews()?;
            self.settle()?;
        }
        // (A duplicate id shares one runtime between two cues, possibly of different kinds.)
        if quiet && self.unique_ids() {
            // Nothing scheduled and nothing owed: every cue that is not idle is waiting for the
            // operator or for a looping voice to be stopped.
            for (id, state) in self.engine.cue_states() {
                let fine = matches!(state, CueState::Idle | CueState::Loaded | CueState::Broken)
                    || self.waits_for_the_operator(id);
                prop_assert!(fine, "cue {} stuck {:?} with nothing scheduled", id, state);
            }
        }
        Ok(())
    }

    /// Whether a cue can rest with nothing scheduled: it is paused, or its voice loops until it
    /// is stopped, or it is a group that is running or stopping around such a cue.
    fn waits_for_the_operator(&self, id: CueId) -> bool {
        let Some(c) = self.ws.find_cue(id) else {
            return false;
        };
        match (self.engine.state(id), &c.kind) {
            (CueState::Paused, _) => true,
            (CueState::Running, CueKind::Audio { .. }) => {
                // Its voice has no end of its own (a loop), or the mixer would be due to
                // report it.
                self.engine
                    .run_of(id)
                    .is_some_and(|r| self.sim.voices.get(&r).is_some_and(|v| v.playing))
            }
            (CueState::Running | CueState::Stopping, CueKind::Group { children, .. }) => {
                // (A broken child may be a paused one: Pause freezes a broken cue's timers
                // too, and it reports `broken` all the same.)
                children.iter().any(|k| {
                    self.engine.state(k.id) == CueState::Broken || self.waits_for_the_operator(k.id)
                })
            }
            _ => false,
        }
    }

    /// Stopping every cue ends everything; resetting every cue clears all but static
    /// problems; stopping previews ends them (CUE-05).
    fn wind_down(&mut self) -> Check {
        // (With a duplicate id, an operation by id reaches one of the cues that share it.)
        let strict = self.unique_ids();
        let ids: Vec<CueId> = all_ids(&self.ws);
        for id in &ids {
            self.call(
                Input::Stop {
                    cue: *id,
                    fade_out: None,
                },
                Kind::Operator { trigger: false },
            )?;
        }
        self.deliver()?;
        self.flush_previews()?;
        self.settle()?;
        if strict {
            prop_assert!(
                self.engine.next_deadline().is_none(),
                "a deadline survived Stop on every cue"
            );
            for (id, state) in self.engine.cue_states() {
                prop_assert!(
                    !state.is_active() && state != CueState::Loaded,
                    "cue {} {:?} after Stop on every cue",
                    id,
                    state
                );
            }
        }
        for id in &ids {
            self.call(Input::Reset { cue: *id }, Kind::Operator { trigger: false })?;
        }
        self.deliver()?;
        self.flush_previews()?;
        self.settle()?;
        let broken: BTreeSet<CueId> = self
            .engine
            .broken_cues()
            .into_iter()
            .map(|(c, _)| c)
            .collect();
        if strict {
            for (id, state) in self.engine.cue_states() {
                prop_assert!(
                    state == CueState::Broken && broken.contains(&id),
                    "cue {} {:?} after Reset on every cue",
                    id,
                    state
                );
            }
        }
        self.call(Input::StopPreview { cue: None }, Kind::PreviewSide)?;
        // Fades are carried out by the mixer; advance past the longest.
        self.now += 1000;
        self.deliver()?;
        self.flush_previews()?;
        self.settle()?;
        prop_assert!(self.engine.previewing().next().is_none());
        if strict {
            // Mixer voices are gone: show voices by the stops above, previews by their stop.
            prop_assert!(
                self.sim.voices.is_empty(),
                "voices left: {:?}",
                self.sim.voices.keys().collect::<Vec<_>>()
            );
            prop_assert!(
                self.sim.loaded.is_empty(),
                "streams left: {:?}",
                self.sim.loaded
            );
        }
        Ok(())
    }

    fn finish(self) -> Outcome {
        Outcome {
            raw: self.raw,
            shown: self.shown,
        }
    }
}

fn run(case: &Case, previews: bool) -> Result<Outcome, TestCaseError> {
    run_with(case, previews, false)
}

fn run_with(case: &Case, previews: bool, follow_rule: bool) -> Result<Outcome, TestCaseError> {
    let mut run = Run::new(case, previews);
    run.follow_rule = follow_rule;
    for s in &case.steps {
        run.step(s)?;
    }
    run.drain()?;
    run.wind_down()?;
    Ok(run.finish())
}

/// Top-level lists of sounds (some with no media file, so broken from the start), memos and
/// waits, each with an auto-follow or none and a pre-wait or none; the operator presses GO, and
/// sounds fail or end early.
fn follow_case() -> impl Strategy<Value = Case> {
    let spec = (0u8..4, 0u8..3, any::<bool>()).prop_map(|(kind, pre, follow)| Spec {
        kind: [0, 1, 2, 31][usize::from(kind)],
        pre,
        post: 1,
        mode: if follow { 2 } else { 0 },
        armed: true,
        dup: false,
        level: 0,
        group_mode: 0,
        target: 0,
        flag: false,
    });
    let ops = [1u8, 1, 1, 1, 0, 0, 13, 12];
    let step = (
        prop_oneof![2 => 0u64..30, 1 => 0u64..1500],
        0..ops.len(),
        any::<u8>(),
        any::<u8>(),
    )
        .prop_map(move |(dt, op, a, b)| Step {
            dt,
            op: ops[op],
            // Even: a cue that is doing something. Never a preview voice.
            a: a & !1,
            b: b & !1,
        });
    (
        prop::collection::vec(spec, 2..8),
        prop::collection::vec(step, 1..30),
        0u64..24,
    )
        .prop_map(|(specs, steps, lookahead)| Case {
            specs,
            steps,
            lookahead,
            min_go: 0,
            panic: 0,
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn broken_cues_never_auto_follow(case in follow_case()) {
        run_with(&case, false, true)?;
    }

    #[test]
    fn engine_invariants(case in case()) {
        let without = run(&case, false)?;
        let with = run(&case, true)?;
        let again = run(&case, true)?;
        // Determinism (Shape): the same workspace, inputs, times and seed give the same actions.
        prop_assert!(with.raw == again.raw, "two runs of the same case differ");
        // Previews are invisible to the show (Preview).
        let diff = with
            .shown
            .iter()
            .zip(&without.shown)
            .position(|(a, b)| a != b)
            .or_else(|| (with.shown.len() != without.shown.len()).then_some(with.shown.len().min(without.shown.len())));
        prop_assert!(
            diff.is_none(),
            "previews changed what the show does at action {:?}: with {:?} without {:?}",
            diff,
            diff.and_then(|i| with.shown.get(i)),
            diff.and_then(|i| without.shown.get(i))
        );
        prop_assert!(without.raw.iter().all(|a| !preview_action(a)));
    }
}

/// The generator must keep reaching what the rules are about: a property test that never gets
/// to a devamp, a pause or a panic proves nothing about them. A fixed seed, so it is stable.
#[test]
fn the_generator_reaches_what_the_rules_are_about() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    let mut runner = TestRunner::deterministic();
    let mut seen: BTreeMap<String, u32> = BTreeMap::new();
    let strategy = case();
    for _ in 0..400 {
        let case = strategy.new_tree(&mut runner).unwrap().current();
        let out = run(&case, true).unwrap_or_else(|e| panic!("{e}"));
        for a in &out.raw {
            let name = match a {
                Action::StateChanged { state, .. } => format!("state {state:?}"),
                Action::StopVoice { fade: Some(_), .. } => "StopVoice with a fade".into(),
                Action::StopVoice { fade: None, .. } => "StopVoice".into(),
                Action::Warning(w) => format!("{w:?}")
                    .split([' ', '{'])
                    .next()
                    .map(|n| format!("warning {n}"))
                    .unwrap_or_default(),
                Action::GoIgnored { reason, .. } => format!("GoIgnored {reason:?}"),
                other => format!("{other:?}")
                    .split([' ', '{'])
                    .next()
                    .unwrap_or_default()
                    .to_string(),
            };
            *seen.entry(name).or_default() += 1;
        }
    }
    let wanted = [
        "StartVoice",
        "LoadVoice",
        "UnloadVoice",
        "StopVoice",
        "StopVoice with a fade",
        "PauseVoice",
        "ResumeVoice",
        "DevampVoice",
        "Ramp",
        "SetArmed",
        "PreviewStart",
        "PreviewStop",
        "ProblemsChanged",
        "state Loaded",
        "state PreWaiting",
        "state Running",
        "state Paused",
        "state PostWaiting",
        "state Fading",
        "state Stopping",
        "state Broken",
        "warning TriggerIgnored",
        "warning CueBroken",
        "warning LateStart",
        "warning ChainLimit",
        "warning CueRemoved",
        "warning PreviewIgnored",
        "warning PreviewFailed",
        "GoIgnored DoubleGo",
    ];
    let missing: Vec<&str> = wanted
        .iter()
        .copied()
        .filter(|w| seen.get(*w).copied().unwrap_or(0) < 3)
        .collect();
    assert!(
        missing.is_empty(),
        "never (or hardly) reached: {missing:?} in {seen:?}"
    );
}
