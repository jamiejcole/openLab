// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The show log through the control loop on the offline driver (M4.7; DX-02): what each kind of
//! event writes, `log_tail`, and the disk side, with a manual wall clock and no real waiting.

mod common;
mod show;

use common::env;
use openlab_core::CueState;
use openlab_core::protocol::{
    ClientMessage, LogEntry, LogKind, LogLevel, OkBody, Outcome, Request, ResultData,
    ServerMessage, TransportAction,
};
use openlab_engine::control::offline::OfflineShow;
use openlab_engine::control::{ControlConfig, Inbox, ManualClock};
use openlab_engine::diag::{LogLimits, LogSink, LogWriter, ShowLog};
use show::*;

const T0: i64 = 1_700_000_000_000;

fn timed(ws: &openlab_core::Workspace) -> (ControlConfig, ManualClock) {
    let clock = ManualClock::new(T0);
    let mut c = config(ws);
    c.clock = clock.clock();
    (c, clock)
}

fn tail(s: &mut OfflineShow, max: Option<u32>) -> (Vec<LogEntry>, u64) {
    let id = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id,
        request: Request::LogTail { max },
    });
    match replies.last() {
        Some(ServerMessage::Result {
            outcome:
                Outcome::Ok(OkBody {
                    data: Some(ResultData::LogTail { entries, dropped }),
                    ..
                }),
            ..
        }) => (entries.clone(), *dropped),
        other => panic!("no log tail: {other:?}"),
    }
}

/// The entries without the engine's own start-up record.
fn events(s: &mut OfflineShow) -> Vec<LogEntry> {
    let (all, _) = tail(s, None);
    all.into_iter()
        .filter(|e| !(e.kind == LogKind::Engine && e.message.starts_with("openlab-engine")))
        .collect()
}

fn kinds(entries: &[LogEntry]) -> Vec<LogKind> {
    entries.iter().map(|e| e.kind).collect()
}

#[test]
fn a_trigger_go_start_and_stop_are_logged_in_order_with_their_fields() {
    let path = wav(&dc(800, 0.5));
    let ws = workspace(vec![audio(1, &path, 0), audio(2, &path, 1)]);
    let (c, clock) = timed(&ws);
    let mut s = show_with(ws, c);
    // The network layer announces an input just before it arrives.
    s.deliver(Inbox::Trigger {
        source: "osc 10.0.0.9".into(),
        detail: "transport go".into(),
    });
    clock.set(T0 + 250);
    go(&mut s);
    run_until_idle(&mut s, id(1), 8000);
    let log = events(&mut s);
    assert_eq!(
        kinds(&log),
        [
            LogKind::Trigger,
            LogKind::Go,
            LogKind::CueStart,
            LogKind::CueStop
        ],
        "{log:#?}"
    );
    assert_eq!(log[0].source.as_deref(), Some("osc 10.0.0.9"));
    assert_eq!(log[0].message, "transport go");
    assert_eq!(log[0].time_ms, T0);
    assert!(
        log[1].message.starts_with("GO: cue 1"),
        "{}",
        log[1].message
    );
    assert_eq!(log[1].cue, Some(id(1)));
    assert_eq!(log[1].time_ms, T0 + 250);
    for e in &log[1..] {
        assert_eq!(e.level, LogLevel::Info);
        assert_eq!(e.cue, Some(id(1)));
    }
    assert!(log[2].message.contains("started"));
    assert!(log[3].message.contains("stopped"));
    // Show time is the audio's: the stop is the 0.1 s file's length after the start.
    let (start, stop) = (log[2].show_time.unwrap(), log[3].show_time.unwrap());
    assert!(
        stop - start >= 0.09 && stop - start < 0.2,
        "{start} -> {stop}"
    );
    assert!(log.iter().all(|e| e.show_time.is_some()));
}

#[test]
fn other_transport_actions_are_logged_with_the_cue_they_name() {
    let path = wav(&dc(8000, 0.5));
    let ws = workspace(vec![audio(1, &path, 0), audio(2, &path, 1)]);
    let (c, _) = timed(&ws);
    let mut s = show_with(ws, c);
    start(&mut s, 2);
    s.run(200);
    s.transport(TransportAction::Pause { cue: id(2) });
    s.transport(TransportAction::Resume { cue: id(2) });
    s.transport(TransportAction::Stop {
        cue: id(2),
        fade_out: None,
    });
    s.transport(TransportAction::Panic);
    let log = events(&mut s);
    let transports: Vec<(&str, Option<_>)> = log
        .iter()
        .filter(|e| e.kind == LogKind::Transport)
        .map(|e| (e.message.as_str(), e.cue))
        .collect();
    assert_eq!(
        transports,
        [
            ("start: cue 2 \"cue 2\"", Some(id(2))),
            ("pause: cue 2 \"cue 2\"", Some(id(2))),
            ("resume: cue 2 \"cue 2\"", Some(id(2))),
            ("stop: cue 2 \"cue 2\"", Some(id(2))),
            ("panic", None),
        ]
    );
    // One start for the cue, not one more for the resume.
    let starts = log.iter().filter(|e| e.kind == LogKind::CueStart).count();
    assert_eq!(starts, 1, "{log:#?}");
}

#[test]
fn a_go_with_nothing_at_the_playhead_says_so() {
    let ws = workspace(vec![]);
    let (c, _) = timed(&ws);
    let mut s = show_with(ws, c);
    go(&mut s);
    let log = events(&mut s);
    // The engine also warns about it.
    assert_eq!(kinds(&log), [LogKind::Go, LogKind::Engine]);
    assert_eq!(log[1].level, LogLevel::Warn);
    assert_eq!(log[0].message, "GO: nothing at the playhead");
    assert_eq!(log[0].cue, None);
}

#[test]
fn a_cue_that_fails_is_logged_as_an_error_with_its_reason() {
    let path = wav(&dc(2000, 0.5));
    let ws = workspace(vec![audio(1, &path, 0)]);
    let (mut c, _) = timed(&ws);
    // No prime, so the stream is opened at the GO, after the file changed.
    c.prime_cues = 0;
    let mut s = show_with(ws, c);
    std::fs::write(&path, std::fs::read(wav(&dc(1500, 0.5))).unwrap()).unwrap();
    go(&mut s);
    s.run(1500);
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);
    let log = events(&mut s);
    let fail = log
        .iter()
        .find(|e| e.kind == LogKind::CueFail)
        .unwrap_or_else(|| panic!("{log:#?}"));
    assert_eq!(fail.level, LogLevel::Error);
    assert_eq!(fail.cue, Some(id(1)));
    assert!(fail.message.contains("stream_failed"), "{}", fail.message);
    assert_eq!(
        log.iter().filter(|e| e.kind == LogKind::CueFail).count(),
        1,
        "once"
    );
}

#[test]
fn engine_warnings_and_errors_are_logged_and_still_broadcast() {
    let ws = workspace(vec![]);
    let (c, _) = timed(&ws);
    let mut s = show_with(ws, c);
    s.take_outbound();
    // A start for a cue that does not exist: the engine warns.
    s.transport(TransportAction::Start { cue: id(99) });
    // An error from another thread (the network layer).
    s.deliver(Inbox::Log {
        level: LogLevel::Error,
        message: "something broke".into(),
    });
    let log = events(&mut s);
    let warn = log
        .iter()
        .find(|e| e.level == LogLevel::Warn)
        .unwrap_or_else(|| panic!("{log:#?}"));
    assert_eq!(warn.kind, LogKind::Engine);
    assert_eq!(warn.cue, Some(id(99)));
    assert!(warn.message.contains("unknown_cue"), "{}", warn.message);
    let error = log.iter().find(|e| e.level == LogLevel::Error).unwrap();
    assert_eq!(
        (error.kind, error.message.as_str()),
        (LogKind::Engine, "something broke")
    );
    // The clients' `log` message is unchanged.
    assert!(s.take_outbound().iter().any(|m| matches!(
        m,
        ServerMessage::Log { level: LogLevel::Error, message } if message == "something broke"
    )));
}

#[test]
fn saving_and_opening_are_logged_as_document_events() {
    let e = env();
    let path = e.show_path();
    let ws = workspace(vec![]);
    let (mut c, _) = timed(&ws);
    c.app_dirs = Some(e.dirs.clone());
    let mut s = show_with(ws, c);
    for request in [
        Request::SaveAs {
            path: path.to_string_lossy().into_owned(),
        },
        Request::Open {
            path: path.to_string_lossy().into_owned(),
            discard_changes: false,
            recovery: None,
        },
    ] {
        let id = s.request_id();
        s.send(ClientMessage::Request { id, request });
    }
    let log = events(&mut s);
    let docs: Vec<&str> = log
        .iter()
        .filter(|e| e.kind == LogKind::Document)
        .map(|e| e.message.as_str())
        .collect();
    assert_eq!(
        docs,
        [
            format!("saved {}", path.display()),
            format!("workspace opened: {}", path.display())
        ]
    );
}

#[test]
fn log_tail_answers_the_newest_entries_oldest_first_and_all_without_a_limit() {
    let ws = workspace(vec![]);
    let (c, clock) = timed(&ws);
    let mut s = show_with(ws, c);
    for n in 0..5 {
        clock.set(T0 + n);
        s.deliver(Inbox::Trigger {
            source: "ui".into(),
            detail: format!("t{n}"),
        });
    }
    let (two, dropped) = tail(&mut s, Some(2));
    assert_eq!(dropped, 0);
    assert_eq!(
        two.iter().map(|e| e.message.as_str()).collect::<Vec<_>>(),
        ["t3", "t4"]
    );
    let (all, _) = tail(&mut s, None);
    assert_eq!(all.len(), 6, "start-up record and five triggers");
    assert_eq!(tail(&mut s, Some(0)).0.len(), 0);
    // The ring is bounded: a flood keeps the newest.
    for n in 0..2500 {
        s.deliver(Inbox::Trigger {
            source: "ui".into(),
            detail: format!("flood{n}"),
        });
    }
    let (all, _) = tail(&mut s, None);
    assert_eq!(all.len(), openlab_engine::diag::log::RING_CAPACITY);
    assert_eq!(all.last().unwrap().message, "flood2499");
}

#[test]
fn a_stuck_disk_writer_drops_and_counts_while_control_keeps_running() {
    let path = wav(&dc(800, 0.5));
    let ws = workspace(vec![audio(1, &path, 0)]);
    let (mut c, _) = timed(&ws);
    // A writer that never reads: two lines fit, the rest are lost.
    let (sink, _stuck) = LogSink::detached(2);
    c.show_log = ShowLog::new(Some(sink));
    let mut s = show_with(ws, c);
    s.take_outbound();
    let at = go(&mut s) as usize;
    for n in 0..50 {
        s.deliver(Inbox::Trigger {
            source: "ui".into(),
            detail: format!("t{n}"),
        });
    }
    s.run(1000);
    // Control went on: the cue played its sound on its exact sample.
    assert_eq!(first_nonzero(&s.channel(0)), Some(at));
    let (entries, dropped) = tail(&mut s, None);
    assert!(dropped >= 50, "{dropped}");
    // Nothing was lost from the in-memory log.
    assert!(
        entries
            .iter()
            .filter(|e| e.kind == LogKind::Trigger)
            .count()
            == 50
    );
    // And the loss is reported, to the clients and in the log.
    assert!(s.take_outbound().iter().any(|m| matches!(
        m,
        ServerMessage::Log { level: LogLevel::Warn, message } if message.contains("fell behind")
    )));
    assert!(entries.iter().any(|e| e.message.contains("fell behind")));
}

#[test]
fn records_reach_the_log_file_as_json_lines_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("Logs");
    let path = wav(&dc(800, 0.5));
    let ws = workspace(vec![audio(1, &path, 0)]);
    let (mut c, clock) = timed(&ws);
    let (sink, writer) =
        LogWriter::start(logs.clone(), LogLimits::default(), clock.clock()).unwrap();
    c.show_log = ShowLog::new(Some(sink));
    let mut s = show_with(ws, c);
    s.deliver(Inbox::Trigger {
        source: "ui".into(),
        detail: "transport go".into(),
    });
    go(&mut s);
    run_until_idle(&mut s, id(1), 8000);
    let (in_memory, dropped) = tail(&mut s, None);
    assert_eq!(dropped, 0);
    // Stopping the control loop ends the writer after it has written what is queued.
    drop(s);
    writer.finish();
    let text = std::fs::read_to_string(logs.join("openlab.log")).unwrap();
    let on_disk: Vec<LogEntry> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(on_disk, in_memory);
    assert!(on_disk.len() >= 5, "{text}");
}
