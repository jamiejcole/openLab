// SPDX-License-Identifier: GPL-3.0-or-later

//! Client messages through the control loop's inbox: transport, commands, undo/redo, requests,
//! and what goes out (docs/protocol.md, docs/control.md).

mod show;

use openlab_core::protocol::{
    ClientMessage, ErrorCode, Outcome, Request, RequestId, ServerMessage, TransportAction,
};
use openlab_core::{Command, CueKind, CueProperty, Position};
use openlab_engine::control::{
    Control, Inbox, OUTBOUND_CAPACITY, REPORT_HZ, outbound_channel, reply_channel, runner,
};
use show::*;

#[allow(clippy::panic)]
fn result(replies: &[ServerMessage]) -> &Outcome {
    match replies.last() {
        Some(ServerMessage::Result { outcome, .. }) => outcome,
        other => panic!("no result: {other:?}"),
    }
}

#[allow(clippy::panic)]
fn revision(replies: &[ServerMessage]) -> Option<u64> {
    match result(replies) {
        Outcome::Ok(ok) => ok.revision,
        Outcome::Error(e) => panic!("{e:?}"),
    }
}

#[test]
fn commands_undo_and_redo_signal_the_engine() {
    let path = wav(&dc(4000, 0.5));
    let mut s = show(workspace(vec![audio(1, &path, 0), audio(2, &path, 1)]));
    s.take_outbound();
    let id1 = s.request_id();
    let rename = Command::SetCueProperty {
        id: id(1),
        property: CueProperty::Name {
            value: "renamed".into(),
        },
    };
    let r = s.send(ClientMessage::Command {
        id: id1,
        command: Box::new(rename),
        coalesce: None,
    });
    assert_eq!(revision(&r), Some(1));
    let id2 = s.request_id();
    assert_eq!(revision(&s.send(ClientMessage::Undo { id: id2 })), Some(2));
    let id3 = s.request_id();
    assert_eq!(revision(&s.send(ClientMessage::Redo { id: id3 })), Some(3));
    let id4 = s.request_id();
    // Nothing left to redo: ok, no revision.
    assert_eq!(revision(&s.send(ClientMessage::Redo { id: id4 })), None);
    assert_eq!(
        s.control().workspace().find_cue(id(1)).unwrap().name,
        "renamed"
    );
    let msgs = s.take_outbound();
    let revisions: Vec<u64> = msgs
        .iter()
        .filter_map(|m| match m {
            ServerMessage::ModelChanged { revision, .. } => Some(*revision),
            _ => None,
        })
        .collect();
    assert_eq!(revisions, vec![1, 2, 3]);
    assert!(msgs.iter().any(
        |m| matches!(m, ServerMessage::Document { document } if document.dirty && document.can_undo)
    ));
    for m in &msgs {
        check_wire(m);
    }
    // Every change was signalled before the engine's next call.
    assert_eq!(s.control().engine().unsignalled_model_changes(), 0);
}

#[test]
fn deleting_a_running_cue_stops_it_at_once() {
    let path = wav(&dc(8000, 0.5));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    go(&mut s);
    s.run(500);
    let id1 = s.request_id();
    let r = s.send(ClientMessage::Command {
        id: id1,
        command: Box::new(Command::RemoveCue { id: id(1) }),
        coalesce: None,
    });
    assert_eq!(revision(&r), Some(1));
    let cut = s.now() as usize;
    s.run(BLOCK * 4);
    assert!(s.channel(0)[cut..].iter().all(|&v| v == 0.0));
    assert_eq!(s.control().live_voices(), 0);
    assert_eq!(s.control().engine().unsignalled_model_changes(), 0);
    assert!(
        warnings(&s.take_outbound())
            .iter()
            .any(|w| matches!(w, openlab_core::protocol::WireWarning::CueRemoved { .. }))
    );
}

#[test]
fn an_added_cue_is_probed_and_plays() {
    let path = wav(&dc(300, 0.5));
    let mut s = show(workspace(vec![]));
    let id1 = s.request_id();
    let r = s.send(ClientMessage::Command {
        id: id1,
        command: Box::new(Command::AddCue {
            at: Position {
                list: list_id(),
                parent: None,
                index: 0,
            },
            cue: audio(1, &path, 0),
            auto_number: false,
        }),
        coalesce: None,
    });
    assert_eq!(revision(&r), Some(1));
    s.settle_media();
    assert_eq!(s.control().probes_pending(), 0);
    let at = go(&mut s) as usize;
    s.run(1000);
    assert_eq!(first_nonzero(&s.channel(0)), Some(at));
    assert_eq!(s.control().engine().unsignalled_model_changes(), 0);
}

#[test]
fn a_refused_command_changes_nothing() {
    let mut s = show(workspace(vec![]));
    let id1 = s.request_id();
    let r = s.send(ClientMessage::Command {
        id: id1,
        command: Box::new(Command::RemoveCue { id: id(42) }),
        coalesce: None,
    });
    assert!(matches!(result(&r), Outcome::Error(e) if e.code == ErrorCode::CommandRefused));
    assert_eq!(s.control().revision(), 0);
}

#[test]
fn a_disarm_cue_changes_the_model_as_a_runtime_change() {
    let path = wav(&dc(300, 0.5));
    let disarm = cue(
        2,
        CueKind::Disarm {
            target: Some(id(3)),
        },
    );
    let mut s = show(workspace(vec![disarm, audio(3, &path, 0)]));
    s.take_outbound();
    go(&mut s);
    s.run(BLOCK * 2);
    assert!(!s.control().workspace().find_cue(id(3)).unwrap().armed);
    let msgs = s.take_outbound();
    assert!(
        msgs.iter()
            .any(|m| matches!(m, ServerMessage::ModelChanged { .. }))
    );
    // Not an undo step.
    assert!(!s.control().snapshot().document.can_undo);
    // The disarmed cue does not play.
    go(&mut s);
    s.run(1000);
    assert!(s.channel(0).iter().all(|&v| v == 0.0));
    assert_eq!(s.control().engine().unsignalled_model_changes(), 0);
}

#[test]
fn relink_is_not_available_yet_and_snapshot_answers() {
    let path = wav(&dc(300, 0.5));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    let id1 = s.request_id();
    let r = s.send(ClientMessage::Request {
        id: id1,
        request: Request::Relink {
            folder: "/media".into(),
        },
    });
    assert!(matches!(result(&r), Outcome::Error(e) if e.code == ErrorCode::Unsupported));
    let id2 = s.request_id();
    let r = s.send(ClientMessage::Request {
        id: id2,
        request: Request::Snapshot,
    });
    assert_eq!(r.len(), 2);
    let ServerMessage::Snapshot { snapshot } = &r[0] else {
        panic!("{r:?}")
    };
    assert_eq!(snapshot.workspace, *s.control().workspace());
    assert_eq!(snapshot.playheads.len(), 1);
    assert_eq!(snapshot.playheads[0].cue, Some(id(1)));
    for m in &r {
        check_wire(m);
    }
    assert!(matches!(r[1], ServerMessage::Result { id, .. } if id == id2));
}

#[test]
fn progress_and_meters_stay_at_or_below_30_hz() {
    let path = wav(&dc(16000, 0.5));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    s.take_outbound();
    go(&mut s);
    // Two seconds of sample time.
    s.run(u64::from(RATE) * 2);
    let msgs = s.take_outbound();
    let progress: Vec<_> = msgs
        .iter()
        .filter_map(|m| match m {
            ServerMessage::Progress { items } => Some(items.clone()),
            _ => None,
        })
        .collect();
    let meters = msgs
        .iter()
        .filter(|m| matches!(m, ServerMessage::Meters { .. }))
        .count();
    // At most 30 Hz; ticks every 64 frames make it at least 25 Hz here.
    let limit = (2 * REPORT_HZ + 1) as usize;
    assert!(
        progress.len() <= limit && progress.len() >= 48,
        "{}",
        progress.len()
    );
    assert!(meters <= limit && meters >= 48, "{meters}");
    // Elapsed grows, remaining shrinks, they add up to the cue's length.
    let first = progress.first().unwrap()[0];
    let last = progress.last().unwrap()[0];
    assert!(last.elapsed > first.elapsed);
    let total = last.elapsed + last.remaining.unwrap();
    assert!((total - 2.0).abs() < 1e-9, "{total}");
    // Meters carry the cue's peak and the patch's outputs.
    let peak = msgs.iter().find_map(|m| match m {
        ServerMessage::Meters { outputs, cues } if !cues.is_empty() => {
            Some((outputs[0].peaks[0], cues[0].peaks[0]))
        }
        _ => None,
    });
    assert_eq!(peak, Some((0.5, 0.5)));
    for m in &msgs {
        check_wire(m);
    }
}

#[test]
fn the_runner_answers_and_shuts_down() {
    // The device loop on a mixer nobody renders: the clock stands still, the loop still
    // answers its inbox and stops on Shutdown.
    let ws = workspace(vec![]);
    let config = config(&ws);
    let (ctl, _mixer) = openlab_audio::Mixer::new(mixer_config()).unwrap();
    let (inbox_tx, inbox) = crossbeam_channel::unbounded();
    let (out_tx, mut out) = outbound_channel(OUTBOUND_CAPACITY);
    let control = Control::new(
        ws,
        ctl,
        openlab_engine::control::Backend::Offline,
        config,
        inbox_tx.clone(),
        out_tx,
    )
    .unwrap();
    let thread = std::thread::spawn(move || runner::run(control, &inbox));
    let (reply, mut replies) = reply_channel();
    inbox_tx
        .send(Inbox::Client {
            message: ClientMessage::Transport {
                id: RequestId(9),
                action: TransportAction::Go { list: list_id() },
            },
            reply,
        })
        .unwrap();
    // The loop drops the reply sender once it has answered: this cannot wait for ever.
    let r = replies.blocking_recv().unwrap();
    assert!(matches!(
        r,
        ServerMessage::Result {
            id: RequestId(9),
            outcome: Outcome::Ok(_)
        }
    ));
    inbox_tx.send(Inbox::Shutdown).unwrap();
    thread.join().unwrap();
    // The playhead report of the GO went out.
    assert!(
        std::iter::from_fn(|| out.try_recv().ok())
            .any(|m| matches!(m, ServerMessage::Runtime { .. }))
    );
}

#[test]
fn an_edit_in_a_big_show_rebuilds_only_what_it_changed() {
    let path = wav(&dc(300, 0.5));
    let cues: Vec<_> = (1..=2000).map(|n| audio(n, &path, 0)).collect();
    let mut s = show(workspace(cues));
    s.run(BLOCK);
    let after_open = s.control().plan_builds();
    assert_eq!(after_open, 2000);
    let id1 = s.request_id();
    s.send(ClientMessage::Command {
        id: id1,
        command: Box::new(Command::SetCueProperty {
            id: id(7),
            property: CueProperty::Name {
                value: "renamed".into(),
            },
        }),
        coalesce: None,
    });
    s.run(BLOCK);
    assert_eq!(s.control().plan_builds(), after_open);
    // Changing one cue's trims rebuilds that cue's plan only.
    let mut kind = s
        .control()
        .workspace()
        .find_cue(id(9))
        .unwrap()
        .kind
        .clone();
    if let CueKind::Audio { start_time, .. } = &mut kind {
        *start_time = 0.01;
    }
    let id2 = s.request_id();
    s.send(ClientMessage::Command {
        id: id2,
        command: Box::new(Command::SetCueProperty {
            id: id(9),
            property: CueProperty::Kind { value: kind },
        }),
        coalesce: None,
    });
    s.run(BLOCK);
    assert_eq!(s.control().plan_builds(), after_open + 1);
    assert_eq!(s.control().engine().unsignalled_model_changes(), 0);
}

/// The cues of the last `warnings` message's `bad_plan` notices, in order.
fn bad_plans(messages: &[ServerMessage]) -> Option<Vec<openlab_core::CueId>> {
    messages.iter().rev().find_map(|m| match m {
        ServerMessage::Warnings { warnings } => Some(
            warnings
                .iter()
                .filter_map(|w| match w {
                    openlab_core::protocol::WireWarning::Notice {
                        code,
                        cue: Some(cue),
                        ..
                    } if code == "bad_plan" => Some(*cue),
                    _ => None,
                })
                .collect(),
        ),
        _ => None,
    })
}

#[test]
fn bad_plan_warnings_follow_edits_moves_removals_and_undo() {
    // M4.12c: the slice-plan checks look only at what each edit touched; the warnings still
    // name exactly the cues whose slices cannot play, in workspace order. A 300-frame file and
    // a start time past its end make a plan that cannot play.
    let path = wav(&dc(300, 0.5));
    let mut cues: Vec<_> = (1..=5).map(|n| audio(n, &path, 0)).collect();
    cues.push(openlab_core::Cue {
        kind: CueKind::Group {
            mode: openlab_core::GroupMode::Timeline,
            children: vec![audio(6, &path, 0), audio(7, &path, 0)],
        },
        ..audio(100, &path, 0)
    });
    let mut s = show(workspace(cues));
    s.settle_media();
    s.run(BLOCK);
    let edit = |s: &mut openlab_engine::control::offline::OfflineShow, command: Command| {
        s.take_outbound();
        let id = s.request_id();
        let replies = s.send(ClientMessage::Command {
            id,
            command: Box::new(command),
            coalesce: None,
        });
        assert!(matches!(result(&replies), Outcome::Ok(_)), "{replies:?}");
        s.run(BLOCK);
        bad_plans(&s.take_outbound())
    };
    let past_end = |s: &openlab_engine::control::offline::OfflineShow, n: u128| {
        let mut kind = s
            .control()
            .workspace()
            .find_cue(id(n))
            .unwrap()
            .kind
            .clone();
        if let CueKind::Audio { start_time, .. } = &mut kind {
            *start_time = 0.5;
        }
        Command::SetCueProperty {
            id: id(n),
            property: CueProperty::Kind { value: kind },
        }
    };
    let c = past_end(&s, 7);
    assert_eq!(edit(&mut s, c), Some(vec![id(7)]));
    let c = past_end(&s, 2);
    assert_eq!(edit(&mut s, c), Some(vec![id(2), id(7)]));
    // The group moves to the top: its cue's warning now comes first, though no cue in it was
    // touched.
    let moved = edit(
        &mut s,
        Command::MoveCue {
            id: id(100),
            index: 0,
        },
    );
    assert_eq!(moved, Some(vec![id(7), id(2)]));
    // Gone with its group, and back with the undo.
    assert_eq!(
        edit(&mut s, Command::RemoveCue { id: id(100) }),
        Some(vec![id(2)])
    );
    s.take_outbound();
    let undo = s.request_id();
    s.send(ClientMessage::Undo { id: undo });
    s.run(BLOCK);
    assert_eq!(bad_plans(&s.take_outbound()), Some(vec![id(7), id(2)]));
    assert_eq!(
        s.control().plan_walks(),
        1,
        "no edit walked every audio cue"
    );
    assert_eq!(s.control().model_change_walks(), 0);
}

#[test]
fn a_client_that_falls_behind_loses_meters_first_and_state_never() {
    let path = wav(&dc(16000, 0.5));
    let ws = workspace(vec![audio(1, &path, 0)]);
    let config = config(&ws);
    let mut s = openlab_engine::control::offline::OfflineShow::with_outbound_capacity(
        ws,
        mixer_config(),
        config,
        16,
    )
    .unwrap();
    s.settle_media();
    s.take_outbound();
    go(&mut s);
    // Nobody reads: the channel fills with runtime messages and meters until meters stop.
    s.run(u64::from(RATE));
    let id1 = s.request_id();
    s.send(ClientMessage::Command {
        id: id1,
        command: Box::new(Command::SetCueProperty {
            id: id(1),
            property: CueProperty::Name {
                value: "renamed".into(),
            },
        }),
        coalesce: None,
    });
    let first = s.take_outbound();
    let meters = first
        .iter()
        .filter(|m| {
            matches!(
                m,
                ServerMessage::Meters { .. } | ServerMessage::Progress { .. }
            )
        })
        .count();
    // Meters stopped once the channel was half full.
    assert!(meters <= 8, "{meters}");
    s.run(BLOCK * 2);
    let after = s.take_outbound();
    // The document change did not fit; it comes once there is room, newest.
    assert!(
        !first
            .iter()
            .any(|m| matches!(m, ServerMessage::Document { .. }))
    );
    let doc = after.iter().find_map(|m| match m {
        ServerMessage::Document { document } => Some(document.clone()),
        _ => None,
    });
    assert!(doc.is_some_and(|d| d.dirty && d.can_undo));
    assert!(after.iter().any(|m| matches!(
        m,
        ServerMessage::Log { message, .. } if message.contains("dropped")
    )));
}
