// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The writer thread's queue: coalescing and shutdown with a busy disk. The writer is held
//! busy by blocking in its test hook on a channel; there are no sleeps.

mod common;

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use common::*;
use openlab_core::Workspace;
use openlab_engine::persist::{
    JobKind, MAX_FORCED_TARGETS, MAX_PENDING_TARGETS, PersistError, SaveReport, SaveWriter,
};

struct Gate {
    writer: SaveWriter,
    /// One message per job that has reached the hook.
    started: Receiver<JobKind>,
    /// Send one `()` to let the next job through.
    release: Sender<()>,
}

fn gated(env: &Env) -> Gate {
    let (started_tx, started) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let writer = SaveWriter::spawn_with_hook(env.dirs.clone(), move |kind| {
        let _ = started_tx.send(kind);
        let _ = release_rx.lock().unwrap().recv();
    })
    .unwrap();
    Gate {
        writer,
        started,
        release,
    }
}

fn named(name: &str) -> Workspace {
    let mut ws = example();
    ws.name = name.into();
    ws
}

type AutosaveAnswer = Receiver<Result<Option<std::path::PathBuf>, PersistError>>;
type SaveAnswer = Receiver<Result<SaveReport, PersistError>>;

/// Starts a save and waits until the writer is stuck inside it.
fn occupy(gate: &Gate, env: &Env) -> SaveAnswer {
    let answer = gate.writer.save(
        named("first"),
        env.show_dir().join("first.openlab"),
        1,
        20,
        1,
    );
    assert_eq!(gate.started.recv().unwrap(), JobKind::Save);
    answer
}

#[test]
fn pending_autosaves_coalesce_to_the_newest() {
    let env = env();
    let gate = gated(&env);
    let first = occupy(&gate, &env);
    let a: Vec<_> = (2..5)
        .map(|rev| {
            gate.writer
                .autosave(named(&format!("a{rev}")), Some(env.show_path()), rev)
        })
        .collect();
    // Replaced autosaves are answered at once, without having run.
    assert!(matches!(a[0].try_recv().unwrap(), Ok(None)));
    assert!(matches!(a[1].try_recv().unwrap(), Ok(None)));
    assert!(a[2].try_recv().is_err());

    gate.release.send(()).unwrap();
    gate.release.send(()).unwrap();
    first.recv().unwrap().unwrap();
    let path = a[2].recv().unwrap().unwrap().unwrap();
    assert!(
        std::fs::read_to_string(path)
            .unwrap()
            .contains("\"name\": \"a4\"")
    );
}

#[test]
fn pending_saves_of_one_file_coalesce_but_other_files_do_not() {
    let env = env();
    let gate = gated(&env);
    let first = occupy(&gate, &env);
    let (p, q) = (env.show_path(), env.show_dir().join("Other.openlab"));
    let s2 = gate.writer.save(named("s2"), p.clone(), 10, 20, 2);
    let s3 = gate.writer.save(named("s3"), p.clone(), 11, 20, 3);
    let other = gate.writer.save(named("other"), q.clone(), 12, 20, 3);
    assert!(matches!(
        s2.try_recv().unwrap(),
        Err(PersistError::Superseded)
    ));
    for _ in 0..3 {
        gate.release.send(()).unwrap();
    }
    first.recv().unwrap().unwrap();
    s3.recv().unwrap().unwrap();
    other.recv().unwrap().unwrap();
    assert!(std::fs::read_to_string(p).unwrap().contains("\"s3\""));
    assert!(std::fs::read_to_string(q).unwrap().contains("\"other\""));
}

#[test]
fn autosave_covered_by_a_manual_save_never_recreates_the_recovery_file() {
    let env = env();
    let gate = gated(&env);
    let first = occupy(&gate, &env);
    // Revision 5 is autosaved, then manually saved, while the writer is busy.
    let auto = gate.writer.autosave(named("r5"), Some(env.show_path()), 5);
    let save = gate.writer.save(named("r5"), env.show_path(), 2, 20, 5);
    gate.release.send(()).unwrap();
    gate.release.send(()).unwrap();
    first.recv().unwrap().unwrap();
    save.recv().unwrap().unwrap();
    assert!(matches!(auto.recv().unwrap(), Ok(None)));
    let recovery = env.dirs.recovery_path(example().id, Some(&env.show_path()));
    assert!(!recovery.exists());

    // A later revision is a real change and is written.
    gate.release.send(()).unwrap();
    let later = gate.writer.autosave(named("r6"), Some(env.show_path()), 6);
    assert!(later.recv().unwrap().unwrap().is_some());
    assert!(recovery.exists());
}

#[test]
fn a_failed_manual_save_does_not_cancel_the_autosave() {
    let env = env();
    let gate = gated(&env);
    let first = occupy(&gate, &env);
    let bad = env.root.path().join("nope/x.openlab");
    let save = gate.writer.save(named("r5"), bad, 2, 20, 5);
    let auto = gate.writer.autosave(named("r5"), Some(env.show_path()), 5);
    for _ in 0..3 {
        gate.release.send(()).unwrap();
    }
    first.recv().unwrap().unwrap();
    assert!(save.recv().unwrap().is_err());
    assert!(auto.recv().unwrap().unwrap().is_some());
}

#[test]
fn drop_finishes_manual_saves_and_discards_pending_autosaves() {
    let env = env();
    let gate = gated(&env);
    let first = occupy(&gate, &env);
    let second = gate.writer.save(named("second"), env.show_path(), 2, 20, 2);
    let auto = gate
        .writer
        .autosave(named("auto"), Some(env.show_path()), 9);
    let Gate {
        writer, release, ..
    } = gate;
    let closer = thread::spawn(move || drop(writer));
    // Drop is waiting on the busy writer; let the two saves through.
    release.send(()).unwrap();
    release.send(()).unwrap();
    closer.join().unwrap();
    first.recv().unwrap().unwrap();
    second.recv().unwrap().unwrap();
    assert!(matches!(auto.recv().unwrap(), Ok(None)));
    assert!(
        !env.dirs
            .recovery_path(example().id, Some(&env.show_path()))
            .exists()
    );
}

/// A save stuck on a hung volume does not keep the drop (and so the engine's exit)
/// waiting past its bound; the detached thread still finishes the save when the disk answers.
#[test]
fn drop_of_a_writer_stuck_on_a_hung_disk_returns_after_its_bound() {
    let env = env();
    let mut gate = gated(&env);
    gate.writer.set_drop_wait(std::time::Duration::ZERO);
    let first = occupy(&gate, &env);
    let Gate {
        writer, release, ..
    } = gate;
    // On this thread: with an unbounded wait this would never return (nothing releases it).
    drop(writer);
    assert!(first.try_recv().is_err(), "still stuck in the save");
    release.send(()).unwrap();
    first.recv().unwrap().unwrap();
}

#[test]
fn queue_stays_bounded_under_a_stuck_disk() {
    let env = env();
    let gate = gated(&env);
    let first = occupy(&gate, &env);
    let answers: Vec<_> = (2..2002_u64)
        .map(|rev| gate.writer.autosave(named("x"), Some(env.show_path()), rev))
        .collect();
    // Every autosave but the last was dropped on arrival.
    assert!(
        answers[..1999]
            .iter()
            .all(|a| matches!(a.try_recv(), Ok(Ok(None))))
    );
    gate.release.send(()).unwrap();
    gate.release.send(()).unwrap();
    first.recv().unwrap().unwrap();
    assert!(answers[1999].recv().unwrap().unwrap().is_some());
}

fn target(env: &Env, n: u64) -> (Workspace, Option<std::path::PathBuf>) {
    let mut ws = named(&format!("w{n}"));
    ws.id = openlab_core::WorkspaceId(uuid::Uuid::from_u128(u128::from(n)));
    (ws, Some(env.show_dir().join(format!("T{n}.openlab"))))
}

#[test]
fn autosaves_of_different_targets_are_both_written() {
    let env = env();
    let gate = gated(&env);
    let first = occupy(&gate, &env);
    let (a, pa) = target(&env, 1);
    let (b, pb) = target(&env, 2);
    let ra = gate.writer.autosave(a, pa, 2);
    let rb = gate.writer.autosave(b, pb, 2);
    assert!(ra.try_recv().is_err() && rb.try_recv().is_err());
    for _ in 0..3 {
        gate.release.send(()).unwrap();
    }
    first.recv().unwrap().unwrap();
    assert!(ra.recv().unwrap().unwrap().unwrap().exists());
    assert!(rb.recv().unwrap().unwrap().unwrap().exists());
}

#[test]
fn a_newer_autosave_replaces_only_its_own_target() {
    let env = env();
    let gate = gated(&env);
    let first = occupy(&gate, &env);
    let (a, pa) = target(&env, 1);
    let (b, pb) = target(&env, 2);
    let mut a2 = a.clone();
    a2.name = "a-new".into();
    let ra = gate.writer.autosave(a, pa.clone(), 2);
    let rb = gate.writer.autosave(b, pb, 2);
    let ra2 = gate.writer.autosave(a2, pa, 3);
    assert!(matches!(ra.try_recv().unwrap(), Ok(None)));
    assert!(rb.try_recv().is_err());
    for _ in 0..3 {
        gate.release.send(()).unwrap();
    }
    first.recv().unwrap().unwrap();
    assert!(rb.recv().unwrap().unwrap().is_some());
    let path = ra2.recv().unwrap().unwrap().unwrap();
    assert!(std::fs::read_to_string(path).unwrap().contains("a-new"));
}

#[test]
fn clear_for_one_target_leaves_another_targets_pending_and_in_flight_autosaves() {
    let env = env();
    let gate = gated(&env);
    let (a, pa) = target(&env, 1);
    let (b, pb) = target(&env, 2);
    let (c, pc) = target(&env, 3);
    // B is in flight (stuck in the hook), A and C are pending, then A's is cleared.
    let rb = gate.writer.autosave(b.clone(), pb.clone(), 2);
    assert_eq!(gate.started.recv().unwrap(), JobKind::Autosave);
    let ra = gate.writer.autosave(a.clone(), pa.clone(), 2);
    let rc = gate.writer.autosave(c.clone(), pc.clone(), 2);
    let clear = gate.writer.clear_recovery(a.id, pa.clone());
    assert!(matches!(ra.try_recv().unwrap(), Ok(None)));
    for _ in 0..2 {
        gate.release.send(()).unwrap();
    }
    assert!(rb.recv().unwrap().unwrap().is_some());
    assert!(rc.recv().unwrap().unwrap().is_some());
    clear.recv().unwrap().unwrap();
    assert!(!env.dirs.recovery_path(a.id, pa.as_deref()).exists());
    assert!(env.dirs.recovery_path(b.id, pb.as_deref()).exists());
    assert!(env.dirs.recovery_path(c.id, pc.as_deref()).exists());
}

#[test]
fn clear_runs_after_the_in_flight_write_of_its_own_target() {
    let env = env();
    let gate = gated(&env);
    let (a, pa) = target(&env, 1);
    let ra = gate.writer.autosave(a.clone(), pa.clone(), 2);
    assert_eq!(gate.started.recv().unwrap(), JobKind::Autosave);
    let clear = gate.writer.clear_recovery(a.id, pa.clone());
    // The in-flight write is not cancelled; it finishes and is then removed.
    gate.release.send(()).unwrap();
    assert!(ra.recv().unwrap().unwrap().is_some());
    clear.recv().unwrap().unwrap();
    assert!(!env.dirs.recovery_path(a.id, pa.as_deref()).exists());
}

#[test]
fn a_manual_save_of_one_workspace_does_not_cover_anothers_autosave() {
    let env = env();
    let gate = gated(&env);
    let first = occupy(&gate, &env);
    let (a, pa) = target(&env, 1);
    let (b, _) = target(&env, 2);
    let ra = gate.writer.autosave(a.clone(), pa.clone(), 5);
    let save = gate
        .writer
        .save(b, env.show_dir().join("B.openlab"), 2, 20, 9);
    for _ in 0..3 {
        gate.release.send(()).unwrap();
    }
    first.recv().unwrap().unwrap();
    save.recv().unwrap().unwrap();
    assert!(ra.recv().unwrap().unwrap().is_some());
    assert!(env.dirs.recovery_path(a.id, pa.as_deref()).exists());
}

/// Holds the writer, then queues `n` distinct targets 1..=n. Targets beyond the cap move to the
/// write-through queue in arrival order.
fn overflowed(env: &Env, gate: &Gate, n: u64) -> (SaveAnswer, Vec<AutosaveAnswer>) {
    let first = occupy(gate, env);
    let answers = (1..=n)
        .map(|i| {
            let (w, p) = target(env, i);
            gate.writer.autosave(w, p, 2)
        })
        .collect();
    (first, answers)
}

#[test]
fn write_through_autosaves_run_first_and_nothing_is_lost_to_the_normal_cap() {
    let env = env();
    let gate = gated(&env);
    let n = MAX_PENDING_TARGETS as u64 + 3;
    let (first, answers) = overflowed(&env, &gate, n);
    assert!(answers.iter().all(|a| a.try_recv().is_err()));
    gate.release.send(()).unwrap();
    first.recv().unwrap().unwrap();
    // Targets 1..=3 were pushed out of the normal queue and must run before 4..=n.
    for (i, answer) in answers.iter().enumerate() {
        assert_eq!(gate.started.recv().unwrap(), JobKind::Autosave);
        assert!(answers[i + 1..].iter().all(|a| a.try_recv().is_err()));
        gate.release.send(()).unwrap();
        assert!(
            answer.recv().unwrap().unwrap().is_some(),
            "target {}",
            i + 1
        );
    }
    assert!(gate.started.try_recv().is_err());
    assert_eq!(gate.writer.shed_autosaves(), 0);
}

#[test]
fn a_newer_autosave_replaces_one_waiting_in_the_write_through_queue() {
    let env = env();
    let gate = gated(&env);
    let n = MAX_PENDING_TARGETS as u64 + 3;
    let (first, answers) = overflowed(&env, &gate, n);
    // Target 1 sits in the write-through queue.
    let (mut w, p) = target(&env, 1);
    w.name = "one-new".into();
    let newer = gate.writer.autosave(w, p, 3);
    assert!(matches!(answers[0].try_recv().unwrap(), Ok(None)));
    gate.release.send(()).unwrap();
    first.recv().unwrap().unwrap();
    // The replacement keeps target 1's slot, so it runs first, and exactly once.
    assert_eq!(gate.started.recv().unwrap(), JobKind::Autosave);
    gate.release.send(()).unwrap();
    let path = newer.recv().unwrap().unwrap().unwrap();
    assert!(std::fs::read_to_string(path).unwrap().contains("one-new"));
    for answer in answers.iter().skip(1) {
        assert_eq!(gate.started.recv().unwrap(), JobKind::Autosave);
        gate.release.send(()).unwrap();
        assert!(answer.recv().unwrap().unwrap().is_some());
    }
    assert!(gate.started.try_recv().is_err());
}

#[test]
fn clearing_a_write_through_target_cancels_its_write() {
    let env = env();
    let gate = gated(&env);
    let n = MAX_PENDING_TARGETS as u64 + 3;
    let (first, answers) = overflowed(&env, &gate, n);
    let (w, p) = target(&env, 2);
    let clear = gate.writer.clear_recovery(w.id, p.clone());
    assert!(matches!(answers[1].try_recv().unwrap(), Ok(None)));
    gate.release.send(()).unwrap();
    first.recv().unwrap().unwrap();
    for (i, answer) in answers.iter().enumerate() {
        if i == 1 {
            continue;
        }
        assert_eq!(gate.started.recv().unwrap(), JobKind::Autosave);
        gate.release.send(()).unwrap();
        assert!(answer.recv().unwrap().unwrap().is_some());
    }
    gate.release.send(()).unwrap();
    clear.recv().unwrap().unwrap();
    assert!(!env.dirs.recovery_path(w.id, p.as_deref()).exists());
}

#[test]
fn the_write_through_queue_is_capped_and_sheds_the_oldest_with_a_count() {
    let env = env();
    let gate = gated(&env);
    let extra = 5u64;
    let n = (MAX_PENDING_TARGETS + MAX_FORCED_TARGETS) as u64 + extra;
    let (first, answers) = overflowed(&env, &gate, n);
    // The oldest `extra` targets were shed unwritten; the rest wait.
    for answer in &answers[..extra as usize] {
        assert!(matches!(answer.try_recv().unwrap(), Ok(None)));
    }
    assert!(
        answers[extra as usize..]
            .iter()
            .all(|a| a.try_recv().is_err())
    );
    assert_eq!(gate.writer.shed_autosaves(), extra);
    gate.release.send(()).unwrap();
    first.recv().unwrap().unwrap();
    for answer in &answers[extra as usize..] {
        assert_eq!(gate.started.recv().unwrap(), JobKind::Autosave);
        gate.release.send(()).unwrap();
        assert!(answer.recv().unwrap().unwrap().is_some());
    }
    assert!(gate.started.try_recv().is_err());
}

#[test]
fn saved_revisions_are_remembered_for_the_most_recent_workspaces_only() {
    let env = env();
    let writer = SaveWriter::spawn(env.dirs.clone()).unwrap();
    let n = MAX_PENDING_TARGETS as u64 + 1;
    for i in 1..=n {
        let (w, _) = target(&env, i);
        let path = env.show_dir().join(format!("S{i}.openlab"));
        writer.save(w, path, 1, 20, 5).recv().unwrap().unwrap();
    }
    // The newest is covered: no recovery file for already-saved state.
    let (w, p) = target(&env, n);
    assert!(matches!(writer.autosave(w, p, 5).recv().unwrap(), Ok(None)));
    // The oldest was evicted: the worst case is one redundant autosave, never a lost one.
    let (w, p) = target(&env, 1);
    assert!(writer.autosave(w, p, 5).recv().unwrap().unwrap().is_some());
}

/// A writer whose first job blocks in the hook until released (a write stuck on a volume that
/// stopped answering); later jobs go through.
fn stall_first(env: &Env) -> (SaveWriter, Receiver<JobKind>, Sender<()>) {
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let first = std::sync::atomic::AtomicBool::new(true);
    let writer = SaveWriter::spawn_with_hook(env.dirs.clone(), move |kind| {
        if first.swap(false, std::sync::atomic::Ordering::SeqCst) {
            let _ = entered_tx.send(kind);
            let _ = release_rx.lock().unwrap().recv();
        }
    })
    .unwrap();
    (writer, entered, release)
}

/// A save stuck on a hung volume is answered `Stalled` after the timeout and other shows go on
/// autosaving; the stuck show is refused at once while its write is out, and once that write
/// returns, a later save of it is written (after the stale one, never under it).
#[test]
fn a_stalled_save_does_not_stop_other_shows_and_its_show_saves_again_after() {
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
    let hang_guard = std::time::Duration::from_secs(20);
    let env = env();
    let (mut writer, entered, release) = stall_first(&env);
    let (a, pa) = target(&env, 1);
    let (b, pb) = target(&env, 2);
    let pa = pa.unwrap();
    let stuck = writer.save(a.clone(), pa.clone(), 1, 20, 1);
    assert_eq!(entered.recv().unwrap(), JobKind::Save);
    let other = writer.autosave(b, pb, 2);
    let t0 = std::time::Instant::now();
    let at = |ms: u64| t0 + std::time::Duration::from_millis(ms);
    assert!(writer.check_stalled(at(0), TIMEOUT).is_empty());
    assert!(writer.check_stalled(at(19_999), TIMEOUT).is_empty());
    assert!(other.try_recv().is_err(), "queued behind the stuck save");
    let logs = writer.check_stalled(at(20_000), TIMEOUT);
    assert_eq!(logs.len(), 1, "{logs:?}");
    assert!(matches!(
        stuck.try_recv().unwrap(),
        Err(PersistError::Stalled { secs: 20 })
    ));
    assert_eq!(writer.stalled_threads(), 1);
    // The replacement thread writes the other show.
    assert!(
        other
            .recv_timeout(hang_guard)
            .unwrap()
            .unwrap()
            .unwrap()
            .exists()
    );
    // The stuck show is refused at once, not queued behind the write that may never return.
    let mut newer = a.clone();
    newer.name = "newer".into();
    assert!(matches!(
        writer
            .save(newer.clone(), pa.clone(), 2, 20, 2)
            .try_recv()
            .unwrap(),
        Err(PersistError::Stalled { .. })
    ));
    // The volume answers again: the stale write lands, then a new save goes over it.
    release.send(()).unwrap();
    stuck.recv_timeout(hang_guard).unwrap().unwrap();
    let deadline = std::time::Instant::now() + hang_guard;
    while writer.stalled_threads() > 0 && std::time::Instant::now() < deadline {
        thread::yield_now();
    }
    assert_eq!(writer.stalled_threads(), 0);
    writer
        .save(newer, pa.clone(), 3, 20, 3)
        .recv_timeout(hang_guard)
        .unwrap()
        .unwrap();
    assert!(std::fs::read_to_string(&pa).unwrap().contains("\"newer\""));
}

/// While a save of a show is stuck on its hung volume, the same workspace's edits can still be
/// kept: Save As to another file and its recovery autosave both go through. When the stale
/// save returns at last it leaves the recovery file alone (it holds newer edits than it does).
#[test]
fn a_stalled_save_still_lets_its_workspace_be_saved_elsewhere_and_autosaved() {
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
    let hang_guard = std::time::Duration::from_secs(20);
    let env = env();
    let (mut writer, entered, release) = stall_first(&env);
    let (w, stuck_path) = target(&env, 1);
    let stuck_path = stuck_path.unwrap();
    let stuck = writer.save(w.clone(), stuck_path.clone(), 1, 20, 1);
    assert_eq!(entered.recv().unwrap(), JobKind::Save);
    let t0 = std::time::Instant::now();
    assert!(writer.check_stalled(t0, TIMEOUT).is_empty());
    assert_eq!(writer.check_stalled(t0 + TIMEOUT, TIMEOUT).len(), 1);
    assert!(matches!(
        stuck.try_recv().unwrap(),
        Err(PersistError::Stalled { .. })
    ));
    let mut newer = w.clone();
    newer.name = "newer".into();
    let recovery = writer
        .autosave(newer.clone(), Some(stuck_path.clone()), 2)
        .recv_timeout(hang_guard)
        .unwrap()
        .unwrap()
        .unwrap();
    let elsewhere = env.show_dir().join("Elsewhere.openlab");
    writer
        .save(newer, elsewhere.clone(), 2, 20, 3)
        .recv_timeout(hang_guard)
        .unwrap()
        .unwrap();
    assert!(
        std::fs::read_to_string(&elsewhere)
            .unwrap()
            .contains("\"newer\"")
    );
    // A second autosave of the stuck show's recovery file after the Save As (rev 4 > 3).
    let mut newest = w.clone();
    newest.name = "newest".into();
    writer
        .autosave(newest, Some(stuck_path.clone()), 4)
        .recv_timeout(hang_guard)
        .unwrap()
        .unwrap()
        .unwrap();
    release.send(()).unwrap();
    stuck.recv_timeout(hang_guard).unwrap().unwrap();
    let deadline = std::time::Instant::now() + hang_guard;
    while writer.stalled_threads() > 0 && std::time::Instant::now() < deadline {
        thread::yield_now();
    }
    assert_eq!(writer.stalled_threads(), 0);
    assert!(
        std::fs::read_to_string(&recovery)
            .unwrap()
            .contains("\"newest\""),
        "the late stale save removed a newer recovery file"
    );
}
