// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    dead_code
)]

//! Performance budgets (NFR-24, NFR-25; M4.12). Hardware testing is out of scope, so the
//! budgets are CI stand-ins for the 2014 Mac mini (NFR-24's minimum spec): the requirement is
//! divided by [`HEADROOM`], the same 2x as cpu_budget (ARCHITECTURE 3.6), and CI must stay
//! under the divided figure.
//!
//! Everything that can be proven without a wall clock is: the idle loop's wake rate is counted
//! in virtual time, and edit cost is bounded by counters. The timed gates are `#[ignore]`d
//! (a debug build is far slower) and run in release on the Intel runner:
//!
//! `cargo test --release -p openlab-engine --test perf -- --ignored --nocapture`
//!
//! One timed test function runs the measurements in sequence so parallel tests do not skew
//! each other. `Instant` is used in the timed gates only.

mod show;

use std::time::{Duration, Instant};

use openlab_core::protocol::{ClientMessage, Outcome, ServerMessage, encode_server};
use openlab_core::{
    Command, Cue, CueKind, CueProperty, FadeCurve, FadeLevels, FadeSpec, GroupMode,
    MAX_SHOW_FILE_BYTES, Position, load_show, save_show,
};
use openlab_engine::control::MAX_WAIT;
use openlab_engine::control::offline::OfflineShow;
use openlab_engine::persist::read_text;
use show::*;

/// Headroom for the minimum-spec 2014 Mac mini: CI budgets are the requirement divided by
/// this, the same 2x cpu_budget keeps (ARCHITECTURE 3.6) for hardware slower than the runner.
const HEADROOM: u32 = 2;
/// Independent timed runs; a gate passes as soon as one is under budget.
const RUNS: usize = 5;
/// Four times the cues may cost at most this many times the time (linear is 4, quadratic 16).
const MAX_SCALING: f64 = 12.0;
/// NFR-24: a workspace of at least 2,000 cues opens in under this on minimum spec.
const OPEN_LIMIT: Duration = Duration::from_secs(3);
/// NFR-24: an edit is under a frame (60 Hz) on minimum spec.
const EDIT_LIMIT: Duration = Duration::from_millis(16);
/// NFR-25: idle CPU on minimum spec, as a fraction of one core.
const IDLE_CPU_LIMIT: f64 = 0.02;

const GROUPS: u128 = 60;
const PER_GROUP: u128 = 10;
const TOP_AUDIO: u128 = 1_200;
const FADES: u128 = 150;
const MEMOS: u128 = 100;

/// A realistic mix: groups of audio cues, plain audio cues each with its own file, fade cues,
/// memos with notes, and a few cues with slices.
fn big_workspace() -> openlab_core::Workspace {
    scaled_workspace(1)
}

/// [`big_workspace`] with `k` times as many of each kind of cue.
fn scaled_workspace(k: u128) -> openlab_core::Workspace {
    let (groups, top_audio, fades, memos) = (GROUPS * k, TOP_AUDIO * k, FADES * k, MEMOS * k);
    let mut next = 1_000;
    let mut take = || {
        next += 1;
        next
    };
    let audio_cue = |n: u128| {
        let path = wav(&dc(64, 0.25));
        let mut c = audio(n, &path, (n % 2) as u32);
        c.notes = format!("note for cue {n}: check levels against the last rehearsal");
        if let CueKind::Audio {
            fade_in, fade_out, ..
        } = &mut c.kind
        {
            let curve =
                [FadeCurve::Linear, FadeCurve::EqualPower, FadeCurve::SCurve][n as usize % 3];
            *fade_in = FadeSpec {
                duration: 0.5,
                curve,
            };
            *fade_out = FadeSpec {
                duration: 1.5,
                curve,
            };
        }
        if n.is_multiple_of(50) {
            c = with_slices(c, &[(0.002, 1), (0.004, 2)], 1);
        }
        c
    };
    let mut cues = Vec::new();
    let mut targets = Vec::new();
    for g in 0..groups {
        let children: Vec<Cue> = (0..PER_GROUP)
            .map(|_| {
                let n = take();
                targets.push(n);
                audio_cue(n)
            })
            .collect();
        let mode = if g % 2 == 0 {
            GroupMode::Timeline
        } else {
            GroupMode::StartFirstAndEnter
        };
        cues.push(cue(take(), CueKind::Group { mode, children }));
    }
    for _ in 0..top_audio {
        let n = take();
        targets.push(n);
        cues.push(audio_cue(n));
    }
    for i in 0..fades {
        cues.push(cue(
            take(),
            CueKind::Fade {
                target: Some(id(targets[(i as usize * 7) % targets.len()])),
                duration: 2.0,
                curve: FadeCurve::EqualPower,
                levels: FadeLevels {
                    master_db: Some(-12.0),
                    output_db: vec![],
                    crosspoints: vec![],
                },
                stop_target_when_done: i % 2 == 0,
            },
        ));
    }
    for _ in 0..memos {
        let mut c = cue(take(), CueKind::Memo);
        c.notes = "stage manager: hold for applause".repeat(3);
        cues.push(c);
    }
    workspace(cues)
}

fn count(cues: &[Cue]) -> usize {
    cues.iter()
        .map(|c| {
            1 + if let CueKind::Group { children, .. } = &c.kind {
                count(children)
            } else {
                0
            }
        })
        .sum()
}

/// A show file on disk for `ws`, as the engine reads it.
fn show_file(ws: &openlab_core::Workspace) -> std::path::PathBuf {
    file("openlab", save_show(ws).unwrap().as_bytes())
}

/// The whole open path the engine runs (`host::run_show`, then `Control::new`): read, parse,
/// validate, migrate, build the control state, probe every file, build the snapshot a client
/// is sent and encode it. Returns the show and the phase timings (read and parse, control
/// state, probes, snapshot).
fn open(path: &std::path::Path) -> (OfflineShow, [Duration; 4]) {
    let t0 = Instant::now();
    let text = read_text(path, MAX_SHOW_FILE_BYTES).unwrap();
    let ws = load_show(&text).unwrap().workspace;
    let t1 = Instant::now();
    let c = config(&ws);
    let mut s = OfflineShow::new(ws, mixer_config(), c).unwrap();
    let t2 = Instant::now();
    s.settle_media();
    let t3 = Instant::now();
    let snapshot = ServerMessage::Snapshot {
        snapshot: Box::new(s.control().snapshot()),
    };
    let text = encode_server(&snapshot).unwrap();
    assert!(text.len() > 1_000_000, "the snapshot is the whole show");
    let t4 = Instant::now();
    (s, [t1 - t0, t2 - t1, t3 - t2, t4 - t3])
}

fn command(s: &mut OfflineShow, command: Command) -> Vec<ServerMessage> {
    let id = s.request_id();
    s.send(ClientMessage::Command {
        id,
        command: Box::new(command),
    })
}

fn first_audio_ids(ws: &openlab_core::Workspace, n: usize) -> Vec<openlab_core::CueId> {
    openlab_core::audio_media(ws)
        .into_iter()
        .map(|(id, _)| id)
        .take(n)
        .collect()
}

/// The edits a user makes, each one command.
fn edits(ws: &openlab_core::Workspace) -> Vec<(&'static str, Command)> {
    let group = ws.cue_lists[0].cues[0].id;
    let leaf = ws.cue_lists[0].cues[(GROUPS + 5) as usize].id;
    vec![
        (
            "rename one cue",
            Command::SetCueProperty {
                id: leaf,
                property: CueProperty::Name {
                    value: "renamed".into(),
                },
            },
        ),
        (
            "edit notes",
            Command::SetCueProperty {
                id: leaf,
                property: CueProperty::Notes {
                    value: "edited".into(),
                },
            },
        ),
        (
            "add a cue inside a group",
            Command::AddCue {
                at: Position {
                    list: list_id(),
                    parent: Some(group),
                    index: 0,
                },
                cue: cue(9_000_001, CueKind::Memo),
                auto_number: false,
            },
        ),
        (
            "batch set 200 cues",
            Command::BatchSet {
                ids: first_audio_ids(ws, 200),
                property: CueProperty::Armed { value: false },
            },
        ),
        (
            "move a cue to the end",
            Command::MoveCue {
                id: leaf,
                index: ws.cue_lists[0].cues.len() - 1,
            },
        ),
        ("remove a group", Command::RemoveCue { id: group }),
    ]
}

#[test]
fn the_generated_workspace_is_a_realistic_two_thousand_cues() {
    let ws = big_workspace();
    let n = count(&ws.cue_lists[0].cues);
    assert!(n >= 2_000, "{n} cues");
    assert_eq!(
        openlab_core::audio_media(&ws).len(),
        (GROUPS * PER_GROUP + TOP_AUDIO) as usize
    );
    // It is a valid show: it survives a save and a load unchanged.
    let text = save_show(&ws).unwrap();
    assert_eq!(load_show(&text).unwrap().workspace, ws);
}

#[test]
fn opening_two_thousand_cues_builds_a_complete_show() {
    let path = show_file(&big_workspace());
    let (mut s, _) = open(&path);
    assert_eq!(s.control().probes_pending(), 0);
    assert_eq!(s.stalls(), 0);
    let snapshot = s.control().snapshot();
    let cues = count(&snapshot.workspace.cue_lists[0].cues);
    assert!(cues >= 2_000);
    assert!(snapshot.problems.is_empty(), "{}", snapshot.problems.len());
    assert_eq!(snapshot.playheads.len(), 1);
    // Idle ticks plan no slices again.
    let builds = s.control().plan_builds();
    s.run(BLOCK * 20);
    assert_eq!(s.control().plan_builds(), builds);
}

#[test]
fn an_edit_rebuilds_only_what_it_changed() {
    let path = show_file(&big_workspace());
    let (mut s, _) = open(&path);
    assert!(s.control().plan_builds() >= 1, "the show has slice cues");
    for (name, edit) in edits(s.control().workspace()) {
        s.take_outbound();
        let before = s.control().plan_builds();
        let replies = command(&mut s, edit);
        s.tick();
        assert!(
            replies.iter().any(|m| matches!(
                m,
                ServerMessage::Result {
                    outcome: Outcome::Ok(_),
                    ..
                }
            )),
            "{name}: {replies:?}"
        );
        let model_changed = s
            .take_outbound()
            .iter()
            .filter(|m| matches!(m, ServerMessage::ModelChanged { .. }))
            .count();
        assert_eq!(model_changed, 1, "{name}");
        // No slice cue was touched: no plan may be rebuilt (an edit never replans the show).
        assert_eq!(s.control().plan_builds(), before, "{name}");
    }
}

/// Runs the control loop the way `runner::run` does, on virtual time: it waits `wait_hint()`,
/// the device renders that long, then the loop ticks. Returns the wake count and the longest
/// and shortest wait over `seconds` of virtual time.
fn idle_loop(s: &mut OfflineShow, seconds: u64) -> (u64, Duration, Duration) {
    let rate = u128::from(RATE);
    let mut elapsed = Duration::ZERO;
    let end = Duration::from_secs(seconds);
    let (mut wakes, mut longest, mut shortest) = (0, Duration::ZERO, Duration::MAX);
    while elapsed < end {
        let hint = s.control().wait_hint();
        longest = longest.max(hint);
        shortest = shortest.min(hint);
        elapsed += hint.max(Duration::from_micros(1));
        // The device keeps rendering whole blocks while the loop sleeps.
        let due = (elapsed.as_nanos() * rate / 1_000_000_000) as u64 / BLOCK * BLOCK;
        if due > s.rendered() {
            s.render_only(due - s.rendered());
        }
        s.tick();
        wakes += 1;
    }
    (wakes, longest, shortest)
}

#[test]
fn an_idle_loop_sleeps_instead_of_spinning() {
    let path = show_file(&big_workspace());
    let (mut s, _) = open(&path);
    s.run(BLOCK * 8);
    s.take_outbound();
    assert!(s.control().is_idle());
    let builds = s.control().plan_builds();
    let seconds = 20;
    let (wakes, longest, shortest) = idle_loop(&mut s, seconds);
    // Nothing is due, so the loop waits the full bound: never zero (a spin), never longer.
    assert_eq!(shortest, MAX_WAIT);
    assert_eq!(longest, MAX_WAIT);
    let expected = seconds * 1_000_000_000 / MAX_WAIT.as_nanos() as u64;
    assert_eq!(wakes, expected, "wakes over {seconds} virtual seconds");
    // And an idle wake does no work: nothing is sent, nothing is rebuilt.
    assert!(s.take_outbound().is_empty());
    assert_eq!(s.control().plan_builds(), builds);
    assert!(s.control().is_idle());
    assert_eq!(s.stalls(), 0);
}

#[test]
fn an_empty_workspace_idles_at_the_same_rate() {
    let mut s = show(workspace(vec![]));
    s.run(BLOCK);
    let (wakes, longest, shortest) = idle_loop(&mut s, 2);
    assert_eq!((longest, shortest), (MAX_WAIT, MAX_WAIT));
    assert_eq!(wakes, 400);
}

#[test]
fn probe_answers_do_not_rescan_the_workspace() {
    // Each probe answer once scanned every cue for a preload request: 2,000 files, 2,000
    // scans (quadratic). The catalog now counts the cues that ask for a preload as the
    // workspace and its edits come in, so an answer reads a counter; no scan exists to count.
    let path = show_file(&big_workspace());
    let (mut s, _) = open(&path);
    s.run(BLOCK * 4);
    // An edit that adds a file that wants preloading: its answer sees the new model.
    let wav = wav(&dc(64, 0.5));
    let mut c = audio(9_000_002, &wav, 0);
    if let CueKind::Audio { preload, .. } = &mut c.kind {
        *preload = true;
    }
    command(
        &mut s,
        Command::AddCue {
            at: Position {
                list: list_id(),
                parent: None,
                index: 0,
            },
            cue: c,
            auto_number: false,
        },
    );
    s.settle_media();
    assert!(
        s.control().preload_reserved() > 0,
        "the new cue is preloaded"
    );
}

/// Best of `runs` (the gate passes as soon as one run is under budget, as in cpu_budget).
fn best(runs: usize, mut f: impl FnMut() -> Duration) -> Duration {
    (0..runs).map(|_| f()).min().unwrap_or(Duration::MAX)
}

/// The timed gates. A no-op in a debug build, which is far slower than any release.
#[test]
#[ignore = "wall-clock timing; run in release (CI: performance budget gate)"]
fn performance_budgets() {
    if cfg!(debug_assertions) {
        eprintln!("debug build: budgets not measured");
        return;
    }
    let ws = big_workspace();
    let path = show_file(&ws);
    eprintln!(
        "show file: {} bytes",
        std::fs::metadata(&path).unwrap().len()
    );

    // NFR-24 open: read, parse, validate, control state, probes, snapshot, encode.
    let open_limit = OPEN_LIMIT / HEADROOM;
    let mut phases = [Duration::ZERO; 4];
    let opened = best(RUNS, || {
        let t = Instant::now();
        let (_, p) = open(&path);
        let total = t.elapsed();
        phases = p;
        total
    });
    eprintln!(
        "open: {opened:?} (limit {open_limit:?}); last run's phases read+parse/control/probes/snapshot {phases:?}"
    );

    // NFR-24 edit: command, model_changed, engine follow-up and the next tick.
    // Tighter than a frame but looser than HEADROOM: edits measure ~3-4 ms on a dev machine,
    // so a 2x gate (8 ms) would trip on a slow or noisy shared Intel runner (M4.12 review M1).
    let edit_limit = EDIT_LIMIT * 3 / 4;
    let names: Vec<_> = edits(&ws).into_iter().map(|e| e.0).collect();
    let mut worst = vec![Duration::MAX; names.len()];
    for _ in 0..RUNS {
        let (mut s, _) = open(&path);
        let w = s.control().workspace().clone();
        for (i, (_, edit)) in edits(&w).into_iter().enumerate() {
            let t = Instant::now();
            command(&mut s, edit);
            s.tick();
            worst[i] = worst[i].min(t.elapsed());
        }
    }
    for (name, took) in names.iter().zip(&worst) {
        eprintln!("edit {name}: {took:?} (limit {edit_limit:?})");
    }

    // NFR-25: what one idle wake costs, at the wake rate the virtual-time test proves.
    let (mut s, _) = open(&path);
    s.run(BLOCK * 8);
    let tick = best(RUNS, || {
        let t = Instant::now();
        for _ in 0..1_000 {
            s.tick();
        }
        t.elapsed() / 1_000
    });
    let cpu = tick.as_secs_f64() / MAX_WAIT.as_secs_f64();
    eprintln!(
        "idle tick: {tick:?} = {:.4}% of a core at one wake per {MAX_WAIT:?}",
        cpu * 100.0
    );

    // Linear, not quadratic: four times the cues costs well under 16 times the time.
    let big = show_file(&scaled_workspace(4));
    let open4 = best(RUNS, || {
        let t = Instant::now();
        let _ = open(&big);
        t.elapsed()
    });
    let rename4 = best(RUNS, || {
        let (mut s, _) = open(&big);
        let w = s.control().workspace().clone();
        let leaf = w.cue_lists[0].cues[(GROUPS * 4 + 5) as usize].id;
        let t = Instant::now();
        command(&mut s, rename(leaf));
        s.tick();
        t.elapsed()
    });
    let rename1 = best(RUNS, || {
        let (mut s, _) = open(&path);
        let leaf = s.control().workspace().cue_lists[0].cues[(GROUPS + 5) as usize].id;
        let t = Instant::now();
        command(&mut s, rename(leaf));
        s.tick();
        t.elapsed()
    });
    eprintln!(
        "4x cues: open {open4:?} ({:.1}x), rename {rename4:?} ({:.1}x)",
        ratio(open4, opened),
        ratio(rename4, rename1)
    );

    assert!(opened < open_limit, "open {opened:?} over {open_limit:?}");
    for (name, took) in names.iter().zip(&worst) {
        assert!(
            *took < edit_limit,
            "edit {name}: {took:?} over {edit_limit:?}"
        );
    }
    assert!(
        cpu < IDLE_CPU_LIMIT / f64::from(HEADROOM),
        "idle tick {tick:?}"
    );
    assert!(ratio(open4, opened) < MAX_SCALING, "open is super-linear");
    assert!(
        ratio(rename4, rename1) < MAX_SCALING,
        "an edit is super-linear"
    );
}

fn ratio(a: Duration, b: Duration) -> f64 {
    a.as_secs_f64() / b.as_secs_f64().max(1e-9)
}

fn rename(id: openlab_core::CueId) -> Command {
    Command::SetCueProperty {
        id,
        property: CueProperty::Name { value: "x".into() },
    }
}
