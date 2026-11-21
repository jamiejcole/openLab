// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The sound going wrong while it plays, as the operator is told (NFR-27, DX-03): an output
//! device's dropouts and the disk falling behind are workspace warnings while they last, a
//! starved cue is a runtime notice naming it, and each is a show-log record; repeats are
//! counted, told at most once per ten seconds. Offline driver on a manual wall clock.

use openlab_core::CueState;
use openlab_core::protocol::{
    LogEntry, LogKind, LogLevel, ServerMessage, TransportAction, WireWarning,
};
use openlab_engine::control::offline::OfflineShow;
use openlab_engine::control::{DocId, DocSource, ManualClock};

use crate::show::*;

const T0: i64 = 1_700_000_000_000;

fn rig_with(
    ws: openlab_core::Workspace,
    tweak: impl FnOnce(&mut openlab_engine::control::ControlConfig),
) -> (OfflineShow, ManualClock) {
    let clock = ManualClock::new(T0);
    let mut c = config(&ws);
    c.clock = clock.clock();
    tweak(&mut c);
    let mut s = show_with(ws, c);
    s.take_outbound();
    (s, clock)
}

fn rig(ws: openlab_core::Workspace) -> (OfflineShow, ManualClock) {
    rig_with(ws, |_| {})
}

fn first_doc(s: &OfflineShow) -> DocId {
    *s.control().doc_ids().first().unwrap()
}

/// The workspace warning with notice `code` in doc `doc`'s list now.
fn listed(s: &OfflineShow, doc: DocId, code: &str) -> Option<WireWarning> {
    s.control()
        .doc(doc)
        .unwrap()
        .warnings()
        .iter()
        .find(|w| matches!(w, WireWarning::Notice { code: c, .. } if c == code))
        .cloned()
}

fn count_of(w: &WireWarning) -> Option<u64> {
    match w {
        WireWarning::Notice { count, .. } => *count,
        _ => None,
    }
}

/// Show-log records whose text contains `words`.
fn logged(s: &OfflineShow, words: &str) -> Vec<LogEntry> {
    s.control()
        .show_log()
        .tail(1000)
        .into_iter()
        .filter(|e| e.message.contains(words))
        .collect()
}

/// `underrun` notices sent, as (cue, count).
fn underrun_notices(msgs: &[ServerMessage]) -> Vec<(Option<openlab_core::CueId>, Option<u64>)> {
    warnings(msgs)
        .into_iter()
        .filter_map(|w| match w {
            WireWarning::Notice {
                code, cue, count, ..
            } if code == "underrun" => Some((cue, count)),
            _ => None,
        })
        .collect()
}

/// Ticks at wall-clock `ms` after the start (with a block rendered, so the loop runs as in a
/// show).
fn at(s: &mut OfflineShow, clock: &ManualClock, ms: i64) {
    clock.set(T0 + ms);
    s.step();
    s.step();
}

#[test]
fn a_device_that_drops_out_is_one_warning_with_a_count_until_ten_quiet_seconds() {
    let (mut s, clock) = rig(workspace(vec![]));
    let doc = first_doc(&s);
    assert_eq!(listed(&s, doc, "audio_dropouts"), None);

    s.renderer_mut().inject_xruns(2);
    at(&mut s, &clock, 0);
    let w = listed(&s, doc, "audio_dropouts").expect("raised at once");
    assert_eq!(count_of(&w), Some(2));
    let WireWarning::Notice { device, .. } = &w else {
        panic!("{w:?}")
    };
    assert_eq!(device.as_deref(), Some("Offline"), "names the device");
    assert_eq!(logged(&s, "dropped out").len(), 1);
    assert_eq!(logged(&s, "dropped out")[0].kind, LogKind::Device);
    assert_eq!(logged(&s, "dropped out")[0].level, LogLevel::Warn);

    // More inside ten seconds: still the one warning as it was, no more log lines.
    for ms in [1_000, 4_000, 9_000] {
        s.renderer_mut().inject_xruns(1);
        at(&mut s, &clock, ms);
        assert_eq!(
            count_of(&listed(&s, doc, "audio_dropouts").unwrap()),
            Some(2),
            "{ms}"
        );
    }
    assert_eq!(logged(&s, "dropped out").len(), 1);

    // Ten seconds after the first report: the count is brought up to date, once.
    at(&mut s, &clock, 10_000);
    assert_eq!(
        count_of(&listed(&s, doc, "audio_dropouts").unwrap()),
        Some(5)
    );
    let lines = logged(&s, "dropped out");
    assert_eq!(lines.len(), 2);
    assert!(
        lines[1].message.contains("3 more time(s) (5 since"),
        "{}",
        lines[1].message
    );

    // Ten seconds without one after the last (at 9 s): gone, and the log says so.
    at(&mut s, &clock, 18_999);
    assert!(listed(&s, doc, "audio_dropouts").is_some());
    at(&mut s, &clock, 19_000);
    assert_eq!(listed(&s, doc, "audio_dropouts"), None);
    let cleared = logged(&s, "no dropouts for 10 s (5 in all)");
    assert_eq!(cleared.len(), 1);
    assert_eq!(cleared[0].level, LogLevel::Info);

    // Later: raised again, counted afresh.
    s.renderer_mut().inject_xruns(1);
    at(&mut s, &clock, 60_000);
    assert_eq!(
        count_of(&listed(&s, doc, "audio_dropouts").unwrap()),
        Some(1)
    );
}

#[test]
fn dropouts_are_listed_in_every_workspace_that_plays_on_the_device_and_no_other() {
    let mut elsewhere = workspace(vec![]);
    elsewhere.id = openlab_core::WorkspaceId(uuid::Uuid::from_u128(901));
    let mut same = workspace(vec![]);
    same.id = openlab_core::WorkspaceId(uuid::Uuid::from_u128(902));
    let (mut s, clock) = rig_with(workspace(vec![]), |c| {
        c.mixer_patches = 4;
        // The third workspace's patch is on this device; the second's on one not here.
        for (ws, device) in [(same.id, "Offline"), (elsewhere.id, "Not Here")] {
            c.machine
                .set_patch_mapping(
                    ws,
                    patch_id(),
                    openlab_core::PatchMapping {
                        device: device.into(),
                        channels: vec![0, 1],
                        system: false,
                    },
                )
                .unwrap();
        }
    });
    let a = first_doc(&s);
    let b = s
        .open_doc(DocSource::Workspace(Box::new(elsewhere)))
        .unwrap();
    let c = s.open_doc(DocSource::Workspace(Box::new(same))).unwrap();
    s.settle_media();
    s.renderer_mut().inject_xruns(1);
    at(&mut s, &clock, 0);
    assert!(listed(&s, a, "audio_dropouts").is_some());
    assert!(listed(&s, c, "audio_dropouts").is_some());
    assert_eq!(
        listed(&s, b, "audio_dropouts"),
        None,
        "plays on no device here"
    );
    // One log line for the device, however many workspaces list it.
    assert_eq!(logged(&s, "dropped out").len(), 1);
}

#[test]
fn a_cue_starved_by_the_disk_is_named_and_the_disk_warning_comes_and_goes() {
    let path = wav(&dc(4000, 0.5));
    let (mut s, clock) = rig_with(workspace(vec![audio(1, &path, 0)]), |c| {
        c.prime_cues = 0;
        c.streamer.before_open = gated_open;
    });
    let doc = first_doc(&s);
    gate(&path);
    start(&mut s, 1);
    s.run_unsettled(4000 + 1000);
    let msgs = s.take_outbound();
    assert_eq!(
        underrun_notices(&msgs),
        vec![(Some(id(1)), Some(1))],
        "one notice naming the cue"
    );
    let disk = listed(&s, doc, "disk_behind").expect("the disk falls behind");
    assert!(matches!(
        &disk,
        WireWarning::Notice {
            cue: None,
            device: None,
            ..
        }
    ));
    assert!(msgs.iter().any(|m| matches!(
        m,
        ServerMessage::Warnings { warnings } if warnings.contains(&disk)
    )));
    assert_eq!(logged(&s, "disk reads are falling behind").len(), 1);
    assert_eq!(
        logged(&s, "the disk could not keep up").len(),
        1,
        "the notice is logged"
    );
    release(&path);
    s.run(BLOCK * 4);

    // Started again and starved again inside ten seconds: held back, then told with its count.
    gate(&path);
    clock.set(T0 + 2_000);
    start(&mut s, 1);
    s.run_unsettled(4000 + 1000);
    assert_eq!(underrun_notices(&s.take_outbound()), vec![]);
    release(&path);
    s.run(BLOCK * 4);
    at(&mut s, &clock, 9_999);
    assert_eq!(underrun_notices(&s.take_outbound()), vec![]);
    at(&mut s, &clock, 10_000);
    assert_eq!(
        underrun_notices(&s.take_outbound()),
        vec![(Some(id(1)), Some(1))]
    );

    // Ten seconds after the last underrun the disk keeps up again.
    assert!(listed(&s, doc, "disk_behind").is_some());
    at(&mut s, &clock, 12_100);
    at(&mut s, &clock, 25_000);
    assert_eq!(listed(&s, doc, "disk_behind"), None);
    assert_eq!(logged(&s, "disk reads keep up again").len(), 1);
    assert_eq!(
        logged(&s, "disk reads are falling behind").len(),
        1,
        "one episode"
    );
}

#[test]
fn a_healthy_show_raises_none_of_them() {
    let path = wav(&dc(3 * RATE as usize, 0.5));
    let other = wav(&dc(2 * RATE as usize, 0.25));
    let (mut s, clock) = rig(workspace(vec![audio(1, &path, 0), audio(2, &other, 1)]));
    let doc = first_doc(&s);
    let mut msgs = Vec::new();
    s.transport_doc(doc, TransportAction::Start { cue: id(1) });
    s.transport_doc(doc, TransportAction::Start { cue: id(2) });
    // Twenty seconds of show, the wall clock keeping pace.
    for k in 0..(20 * RATE as u64 / BLOCK) {
        clock.set(T0 + i64::try_from(k * BLOCK * 1000 / u64::from(RATE)).unwrap());
        s.step();
        msgs.extend(s.take_outbound());
        if k == RATE as u64 / BLOCK * 5 {
            s.transport_doc(doc, TransportAction::Start { cue: id(1) });
        }
    }
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    for code in ["audio_dropouts", "disk_behind"] {
        assert_eq!(listed(&s, doc, code), None, "{code}");
        assert!(
            !msgs.iter().any(|m| matches!(
                m,
                ServerMessage::Warnings { warnings }
                    if warnings.iter().any(|w| matches!(w, WireWarning::Notice { code: c, .. } if c == code))
            )),
            "{code}"
        );
    }
    assert_eq!(underrun_notices(&msgs), vec![]);
    for words in ["dropped out", "disk reads", "engine load"] {
        assert_eq!(logged(&s, words), vec![], "{words}");
    }
}

/// The pre-show check counts a device's dropouts only while some cue's patch is on that device,
/// as it does the device's other notices; the warnings list has them either way.
#[test]
fn the_preshow_check_counts_dropouts_only_on_a_device_a_cue_plays_on() {
    fn preshow(s: &mut OfflineShow) -> openlab_core::protocol::PreshowReport {
        use openlab_core::protocol::{ClientMessage, Outcome, Request, ResultData};
        let id = s.request_id();
        let replies = s.send(ClientMessage::Request {
            id,
            request: Request::PreshowCheck,
        });
        replies
            .iter()
            .find_map(|m| match m {
                ServerMessage::Result {
                    outcome: Outcome::Ok(ok),
                    ..
                } => match &ok.data {
                    Some(ResultData::PreshowCheck(r)) => Some((**r).clone()),
                    _ => None,
                },
                _ => None,
            })
            .unwrap_or_else(|| panic!("{replies:?}"))
    }
    fn counted(r: &openlab_core::protocol::PreshowReport) -> bool {
        r.system
            .iter()
            .any(|w| matches!(w, WireWarning::Notice { code, .. } if code == "audio_dropouts"))
    }
    let path = wav(&dc(4000, 0.5));
    for (cues, counts) in [(vec![], false), (vec![audio(1, &path, 0)], true)] {
        let (mut s, clock) = rig(workspace(cues));
        let doc = first_doc(&s);
        s.renderer_mut().inject_xruns(1);
        at(&mut s, &clock, 0);
        assert!(listed(&s, doc, "audio_dropouts").is_some());
        let report = preshow(&mut s);
        assert_eq!(counted(&report), counts, "{report:?}");
        assert_eq!(report.ok, !counts, "{report:?}");
    }
}
