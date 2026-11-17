// SPDX-License-Identifier: GPL-3.0-or-later

//! The pre-show check and the warnings feed (SM-06, DX-01, NFR-07): one report of every broken
//! cue, missing file, unmapped or missing device, network destination and system problem,
//! computed from state the control thread holds. Broken cues are not in the `warnings` feed:
//! clients get them from the snapshot's `problems`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use crate::show::device::{DeviceShow, listed};
use crate::show::*;
use openlab_core::protocol::{
    ClientMessage, Outcome, PreshowMediaStatus, PreshowReport, PreshowSummary, PreshowTargetStatus,
    Request, ResultData, ServerMessage, WireProblem, WireResolution, WireWarning,
};
use openlab_core::{
    Command, Cue, CueId, CueKind, CueProperty, FadeCurve, FadeLevels, MachineSettings,
    NetworkPatch, NetworkPatchId, NetworkProtocol, OutputPatch, PatchId, PatchMapping, Position,
    Workspace,
};
use openlab_engine::control::ControlConfig;
use uuid::Uuid;

const DEV: &str = "fake:1";
const DEV_NAME: &str = "Fake 1";

fn spare_patch() -> PatchId {
    PatchId(Uuid::from_u128(902))
}

/// A second, wider patch, left with no device on this machine (`config_for`): what these tests
/// need an unmapped patch for.
fn wide_spare(main: &OutputPatch) -> OutputPatch {
    let mut spare = OutputPatch {
        id: spare_patch(),
        name: "Spare".into(),
        ..main.clone()
    };
    spare.outputs = vec![main.outputs[0].clone(); 4];
    spare
}

fn net(n: u128) -> NetworkPatchId {
    NetworkPatchId(Uuid::from_u128(n))
}

fn network_cue(n: u128, destination: NetworkPatchId) -> Cue {
    cue(
        n,
        CueKind::Network {
            destination,
            address: "/go".into(),
            args: vec![],
        },
    )
}

fn fade_to(n: u128, target: u128) -> Cue {
    cue(
        n,
        CueKind::Fade {
            target: Some(id(target)),
            duration: 1.0,
            curve: FadeCurve::Linear,
            levels: FadeLevels {
                master_db: Some(-6.0),
                output_db: vec![],
                crosspoints: vec![],
            },
            stop_target_when_done: false,
            mode: Default::default(),
            geometry: Default::default(),
        },
    )
}

fn lighting(protocol: NetworkProtocol) -> NetworkPatch {
    NetworkPatch {
        id: net(51),
        name: "Lighting".into(),
        host: "10.0.0.5".into(),
        port: 8000,
        protocol,
    }
}

fn config_for(ws: &Workspace) -> ControlConfig {
    let mut c = config_main_only(ws);
    c.machine
        .set_patch_mapping(ws.id, spare_patch(), PatchMapping::none())
        .unwrap();
    c
}

/// The main patch mapped; nothing stored for any other.
fn config_main_only(ws: &Workspace) -> ControlConfig {
    let mut c = ControlConfig::new(RATE, BLOCK);
    c.seed = 7;
    let mut machine = MachineSettings::default();
    machine
        .set_patch_mapping(
            ws.id,
            patch_id(),
            PatchMapping {
                device: DEV_NAME.into(),
                channels: vec![0, 1],
                system: false,
            },
        )
        .unwrap();
    c.machine = machine;
    c.mixer_patches = 2;
    c
}

fn device_show(ws: Workspace) -> DeviceShow {
    let c = config_for(&ws);
    DeviceShow::new(ws, vec![listed(DEV, DEV_NAME, 2)], c)
}

fn preshow(s: &mut DeviceShow) -> PreshowReport {
    let id = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id,
        request: Request::PreshowCheck,
    });
    let [ServerMessage::Result { outcome, .. }] = replies.as_slice() else {
        panic!("{replies:?}");
    };
    match outcome {
        Outcome::Ok(ok) => match &ok.data {
            Some(ResultData::PreshowCheck(report)) => (**report).clone(),
            other => panic!("{other:?}"),
        },
        Outcome::Error(e) => panic!("{e:?}"),
    }
}

/// The codes of the notices in the last `warnings` list sent, if any was sent.
fn feed_codes(msgs: &[ServerMessage]) -> Option<Vec<String>> {
    msgs.iter()
        .filter_map(|m| match m {
            ServerMessage::Warnings { warnings } => Some(
                warnings
                    .iter()
                    .filter_map(|w| match w {
                        WireWarning::Notice { code, .. } => Some(code.clone()),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        })
        .next_back()
}

fn broken_ids(r: &PreshowReport) -> Vec<CueId> {
    r.broken_cues.iter().map(|c| c.cue).collect()
}

fn show_with_every_kind_of_problem() -> DeviceShow {
    let good = wav(&dc(100, 0.5));
    let missing = std::env::temp_dir().join("openlab-preshow-no-such-file.wav");
    let mut disarmed = audio(3, &good, 0);
    disarmed.armed = false;
    let mut on_spare = audio(7, &good, 0);
    if let CueKind::Audio { patch, .. } = &mut on_spare.kind {
        *patch = spare_patch();
    }
    let mut ws = workspace(vec![
        audio(1, &good, 0),
        audio(2, &missing, 0),
        disarmed,
        fade_to(4, 99),
        network_cue(5, net(50)),
        network_cue(6, net(51)),
        on_spare,
    ]);
    let spare = wide_spare(&ws.patches[0]);
    ws.patches.push(spare);
    ws.network_patches.push(lighting(NetworkProtocol::Udp));
    device_show(ws)
}

#[test]
fn the_report_lists_exactly_the_problems_and_counts_them() {
    let mut s = show_with_every_kind_of_problem();
    let msgs = s.take_outbound();
    let r = preshow(&mut s);

    assert!(!r.ok);
    // Broken: the missing file, the fade with no target cue, the missing destination, the cue
    // on the unmapped patch. Not the disarmed cue, not the cue with a destination.
    let broken = broken_ids(&r);
    assert_eq!(broken, vec![id(2), id(4), id(5), id(7)]);
    let two = &r.broken_cues[0];
    assert_eq!((two.list, two.number.as_deref()), (list_id(), Some("2")));
    assert_eq!(two.name, "cue 2");
    assert!(matches!(
        &two.problems[..],
        [WireProblem::MediaMissing { .. }]
    ));
    assert!(matches!(
        &r.broken_cues[1].problems[..],
        [WireProblem::TargetMissing { target }] if *target == id(99)
    ));
    assert!(matches!(
        &r.broken_cues[2].problems[..],
        [WireProblem::NetworkPatchMissing { patch }] if *patch == net(50)
    ));

    assert_eq!(r.media.len(), 1);
    assert_eq!(r.media[0].status, PreshowMediaStatus::Missing);
    assert!(
        r.media[0]
            .path
            .ends_with("openlab-preshow-no-such-file.wav")
    );
    assert_eq!(r.media[0].cues, vec![id(2)]);

    // The spare patch has no mapping on this machine; the main patch is fine.
    assert_eq!(r.patches.len(), 1);
    let p = &r.patches[0];
    assert_eq!(p.patch, spare_patch());
    assert_eq!(p.name.as_deref(), Some("Spare"));
    assert!(!p.mapped);
    assert_eq!(p.device, None);
    assert_eq!(p.resolution, Some(WireResolution::Unmapped));
    assert_eq!(p.cues, vec![id(7)]);

    // One destination is missing (a problem), one exists and is not probed (not a problem).
    assert_eq!(r.network.len(), 2);
    let known = &r.network[0];
    assert_eq!(known.patch, net(51));
    assert_eq!(known.status, PreshowTargetStatus::NotChecked);
    assert_eq!(known.host.as_deref(), Some("10.0.0.5"));
    assert_eq!(known.cues, vec![id(6)]);
    let gone = &r.network[1];
    assert_eq!(
        (gone.patch, gone.status),
        (net(50), PreshowTargetStatus::Missing)
    );
    assert_eq!(gone.name, None);
    assert_eq!(gone.cues, vec![id(5)]);

    // The spare patch was set to no device: that is said once, as a system problem.
    assert_eq!(notice_codes(&r.system), vec!["patch_no_device"]);
    assert_eq!(
        r.summary,
        PreshowSummary {
            broken_cues: 4,
            media: 1,
            patches: 1,
            network: 1,
            network_unchecked: 1,
            system: 1,
            total: 5,
        }
    );

    // Broken cues are not warnings: the feed has nothing for them (the snapshot's problems do),
    // only the spare patch's notice.
    assert_eq!(feed_codes(&msgs), Some(vec!["patch_no_device".to_owned()]));
}

#[test]
fn a_lost_device_adds_the_system_problem_the_device_and_the_cues_it_breaks() {
    let mut s = show_with_every_kind_of_problem();
    s.take_outbound();
    s.fake.unplug(DEV, true);
    s.settle_devices();
    assert!(s.control().device_down());
    let msgs = s.take_outbound();
    let r = preshow(&mut s);

    assert!(!r.ok);
    assert_eq!(
        notice_codes(&r.system),
        vec!["device_lost", "patch_no_device"]
    );
    // The main patch now lacks its device; the spare one is still unmapped.
    assert_eq!(r.patches.len(), 2);
    let main = r.patches.iter().find(|p| p.patch == patch_id()).unwrap();
    assert!(main.mapped);
    assert_eq!(main.device.as_deref(), Some(DEV_NAME));
    assert!(matches!(
        main.resolution,
        Some(WireResolution::DeviceMissing { .. })
    ));
    assert_eq!(main.cues, vec![id(1), id(2), id(3)]);
    let broken = broken_ids(&r);
    // The disarmed cue 3 is broken too: disarming hides nothing from this check.
    assert_eq!(broken, vec![id(1), id(2), id(3), id(4), id(5), id(7)]);
    assert_eq!(r.summary.system, 2);
    // Both patches have cues, the main one three.
    assert_eq!(r.summary.patches, 2);
    // The problem rows: the six broken cues and the two system problems.
    assert_eq!(r.summary.total, 6 + 2);
    // The feed has the device notice and the spare patch's, and nothing per cue.
    assert_eq!(
        feed_codes(&msgs),
        Some(vec!["device_lost".to_owned(), "patch_no_device".to_owned()])
    );
}

#[test]
fn removing_a_cue_updates_the_report() {
    let mut s = show_with_every_kind_of_problem();
    s.take_outbound();
    let rid = s.request_id();
    s.send(ClientMessage::Command {
        id: rid,
        command: Box::new(Command::RemoveCue { id: id(4) }),
        coalesce: None,
    });
    s.tick();
    let r = preshow(&mut s);
    assert_eq!(broken_ids(&r), vec![id(2), id(5), id(7)]);
}

#[test]
fn a_clean_show_is_ok_and_every_list_is_empty() {
    let good = wav(&dc(100, 0.5));
    let mut s = device_show(workspace(vec![
        audio(1, &good, 0),
        cue(2, CueKind::Memo),
        fade_to(3, 1),
    ]));
    let msgs = s.take_outbound();
    let r = preshow(&mut s);
    assert!(r.ok, "{r:?}");
    assert_eq!(feed_codes(&msgs), None);
    assert!(r.broken_cues.is_empty());
    assert!(r.media.is_empty());
    assert!(r.patches.is_empty());
    assert!(r.network.is_empty());
    assert!(r.system.is_empty());
    assert_eq!(r.summary, PreshowSummary::default());
}

#[test]
fn a_network_destination_that_exists_is_reported_unchecked_and_does_not_fail_the_check() {
    let mut ws = workspace(vec![network_cue(1, net(51))]);
    ws.network_patches.push(lighting(NetworkProtocol::Tcp));
    let mut s = device_show(ws);
    let r = preshow(&mut s);
    assert!(r.ok);
    assert_eq!(r.network.len(), 1);
    assert_eq!(r.network[0].status, PreshowTargetStatus::NotChecked);
    assert_eq!(r.summary.network, 0);
    assert_eq!(r.summary.network_unchecked, 1);
    assert_eq!(r.summary.total, 0);
}

/// A patch no cue uses, set to no device: listed with no cues and breaking none, and no
/// problem of the show at all: no notice, nothing counted, the check passes.
#[test]
fn a_spare_patch_no_cue_uses_is_listed_but_breaks_nothing() {
    let good = wav(&dc(100, 0.5));
    let mut ws = workspace(vec![audio(1, &good, 0)]);
    let spare = wide_spare(&ws.patches[0]);
    ws.patches.push(spare);
    let mut s = device_show(ws);
    let msgs = s.take_outbound();
    let r = preshow(&mut s);
    assert!(r.ok, "{r:?}");
    assert_eq!(r.patches.len(), 1);
    assert_eq!(r.patches[0].patch, spare_patch());
    assert!(r.patches[0].cues.is_empty());
    assert!(r.broken_cues.is_empty());
    assert!(r.system.is_empty(), "{:?}", r.system);
    assert_eq!(r.summary, PreshowSummary::default());
    assert_eq!(feed_codes(&msgs), None);
}

/// Sends `command` and lets the edit take effect.
fn edit(s: &mut DeviceShow, command: Command) {
    let rid = s.request_id();
    let replies = s.send(ClientMessage::Command {
        id: rid,
        command: Box::new(command),
        coalesce: None,
    });
    assert!(
        replies.iter().any(|m| matches!(
            m,
            ServerMessage::Result {
                outcome: Outcome::Ok(_),
                ..
            }
        )),
        "{replies:?}"
    );
    s.tick();
}

/// The spare patch's no-device notice follows its users: a cue added on it raises it, the cue
/// moved to the main patch clears it, the undo of that move raises it again and the cue's
/// removal clears it; the feed and the check agree at each step.
#[test]
fn the_no_device_notice_of_a_spare_patch_follows_the_cues_on_it() {
    let good = wav(&dc(100, 0.5));
    let mut ws = workspace(vec![audio(1, &good, 0)]);
    let spare = wide_spare(&ws.patches[0]);
    ws.patches.push(spare);
    let mut s = device_show(ws);
    s.take_outbound();
    let raised = |s: &mut DeviceShow, msgs: &[ServerMessage], on: bool| {
        let r = preshow(s);
        let want: Vec<&str> = if on { vec!["patch_no_device"] } else { vec![] };
        assert_eq!(notice_codes(&r.system), want, "{r:?}");
        assert_eq!(r.ok, !on, "{r:?}");
        assert_eq!(r.summary.system, u64::from(on), "{r:?}");
        assert_eq!(
            feed_codes(msgs),
            Some(want.iter().map(|c| (*c).to_owned()).collect())
        );
    };

    let mut on_spare = audio(7, &good, 0);
    if let CueKind::Audio { patch, .. } = &mut on_spare.kind {
        *patch = spare_patch();
    }
    edit(
        &mut s,
        Command::AddCue {
            at: Position {
                list: list_id(),
                parent: None,
                index: 1,
            },
            cue: on_spare.clone(),
            auto_number: false,
        },
    );
    let msgs = s.take_outbound();
    raised(&mut s, &msgs, true);

    let mut moved = on_spare.kind.clone();
    if let CueKind::Audio { patch, .. } = &mut moved {
        *patch = patch_id();
    }
    edit(
        &mut s,
        Command::SetCueProperty {
            id: id(7),
            property: CueProperty::Kind { value: moved },
        },
    );
    let msgs = s.take_outbound();
    raised(&mut s, &msgs, false);

    let rid = s.request_id();
    s.send(ClientMessage::Undo { id: rid });
    s.tick();
    let msgs = s.take_outbound();
    raised(&mut s, &msgs, true);

    edit(&mut s, Command::RemoveCue { id: id(7) });
    let msgs = s.take_outbound();
    raised(&mut s, &msgs, false);
}

/// The codes of the notices in `list`.
fn notice_codes(list: &[WireWarning]) -> Vec<&str> {
    list.iter()
        .map(|w| match w {
            WireWarning::Notice { code, .. } => code.as_str(),
            _ => "other",
        })
        .collect()
}

#[test]
fn a_fresh_patch_the_device_can_take_is_mapped_by_default_and_not_reported() {
    let good = wav(&dc(100, 0.5));
    let mut ws = workspace(vec![audio(1, &good, 0)]);
    ws.patches.push(OutputPatch {
        id: spare_patch(),
        name: "Spare".into(),
        ..ws.patches[0].clone()
    });
    let c = config_main_only(&ws);
    let mut s = DeviceShow::new(ws, vec![listed(DEV, DEV_NAME, 2)], c);
    let r = preshow(&mut s);
    assert!(r.ok, "{r:?}");
    // Mapped patches are not listed; an unmapped one would be (see the test above).
    assert!(r.patches.is_empty(), "{r:?}");
    assert_eq!(r.summary, PreshowSummary::default());
}

#[test]
fn a_file_streamed_for_want_of_preload_memory_is_information_not_a_problem() {
    let good = wav(&dc(100, 0.5));
    let mut asks = audio(1, &good, 0);
    if let CueKind::Audio { preload, .. } = &mut asks.kind {
        *preload = true;
    }
    let ws = workspace(vec![asks]);
    let mut c = config_for(&ws);
    c.preload_budget = 1;
    let mut s = DeviceShow::new(ws, vec![listed(DEV, DEV_NAME, 2)], c);
    // The probe answers and the preload is refused over the next blocks.
    s.run(u64::from(RATE) / 10);
    let msgs = s.take_outbound();
    assert_eq!(
        feed_codes(&msgs),
        Some(vec!["preload_over_budget".to_owned()])
    );
    let r = preshow(&mut s);
    // Listed for the operator, not counted, and the check passes.
    assert_eq!(r.system.len(), 1);
    assert!(r.ok, "{r:?}");
    assert_eq!(r.summary.system, 0);
    assert_eq!(r.summary.total, 0);
}
