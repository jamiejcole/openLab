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
use openlab_engine::persist::{JobKind, PersistError, SaveReport, SaveWriter};

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
