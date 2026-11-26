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

use crate::show::device::{DeviceShow, listed, listed_at};
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
            curve: openlab_core::FadeCueCurve::like(FadeCurve::Linear),
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
        interface: None,
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

/// The main patch's twin as patch `n`, named `name`.
fn twin(main: &OutputPatch, n: u128, name: &str) -> OutputPatch {
    OutputPatch {
        id: PatchId(Uuid::from_u128(n)),
        name: name.into(),
        ..main.clone()
    }
}

/// Patch `patch` mapped to channels 0 and 1 of the device named `device`.
fn map_to(c: &mut ControlConfig, ws: &Workspace, patch: PatchId, device: &str) {
    c.machine
        .set_patch_mapping(
            ws.id,
            patch,
            PatchMapping {
                device: device.into(),
                channels: vec![0, 1],
                system: false,
            },
        )
        .unwrap();
}

/// A show whose one cue plays on the main patch, with a spare patch mapped to "Gone", a device
/// that is not plugged in.
fn spare_on_an_unplugged_device() -> DeviceShow {
    let good = wav(&dc(100, 0.5));
    let mut ws = workspace(vec![audio(1, &good, 0)]);
    ws.patches.push(twin(&ws.patches[0], 902, "Spare"));
    let mut c = config_main_only(&ws);
    map_to(&mut c, &ws, spare_patch(), "Gone");
    DeviceShow::new(ws, vec![listed(DEV, DEV_NAME, 2)], c)
}

/// The notices of the last `warnings` list sent, as (code, device, patch, message).
type FeedNotice = (String, Option<String>, Option<PatchId>, String);
fn feed_notices(msgs: &[ServerMessage]) -> Vec<FeedNotice> {
    let list = msgs
        .iter()
        .filter_map(|m| match m {
            ServerMessage::Warnings { warnings } => Some(warnings),
            _ => None,
        })
        .next_back()
        .expect("a warnings list was sent");
    list.iter()
        .filter_map(|w| match w {
            WireWarning::Notice {
                code,
                message,
                device,
                patch,
                ..
            } => Some((code.clone(), device.clone(), *patch, message.clone())),
            _ => None,
        })
        .collect()
}

/// A spare patch mapped to an unplugged device, no cue on it: the pre-show check passes with
/// no system problem; the Warnings list still names the patch and the device.
#[test]
fn a_spare_patch_on_an_unplugged_device_with_no_cue_passes_and_the_warnings_name_it() {
    let mut s = spare_on_an_unplugged_device();
    let msgs = s.take_outbound();
    let r = preshow(&mut s);
    assert!(r.ok, "{r:?}");
    assert!(r.system.is_empty(), "{:?}", r.system);
    assert_eq!(r.summary, PreshowSummary::default());
    // Listed among the patches with no device to play on, with no cue and not counted.
    assert_eq!(r.patches.len(), 1, "{r:?}");
    assert_eq!(r.patches[0].patch, spare_patch());
    assert_eq!(r.patches[0].device.as_deref(), Some("Gone"));
    assert!(r.patches[0].cues.is_empty());

    let notices = feed_notices(&msgs);
    assert_eq!(notices.len(), 1, "{notices:?}");
    let (code, device, patch, message) = &notices[0];
    assert_eq!(code, "patch_device_missing");
    assert_eq!(device.as_deref(), Some("Gone"));
    assert_eq!(*patch, Some(spare_patch()));
    assert!(message.contains("\"Spare\""), "{message}");
}

/// A cue added on that patch makes the pre-show check count the problem; the cue removed, it
/// goes. The Warnings list names the patch throughout.
#[test]
fn a_cue_added_on_a_spare_patch_on_an_unplugged_device_is_counted_and_goes_when_removed() {
    let mut s = spare_on_an_unplugged_device();
    let notices = feed_notices(&s.take_outbound());
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].2, Some(spare_patch()));
    let mut on_spare = audio(7, &wav(&dc(100, 0.5)), 0);
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
            cue: on_spare,
            auto_number: false,
        },
    );
    let r = preshow(&mut s);
    assert!(!r.ok, "{r:?}");
    assert_eq!(notice_codes(&r.system), vec!["patch_device_missing"]);
    assert_eq!(r.summary.system, 1, "{r:?}");
    assert_eq!(broken_ids(&r), vec![id(7)]);
    assert_eq!(r.patches[0].cues, vec![id(7)]);
    assert_eq!(r.summary.total, 2, "{r:?}");

    edit(&mut s, Command::RemoveCue { id: id(7) });
    let r = preshow(&mut s);
    assert!(r.ok, "{r:?}");
    assert!(r.system.is_empty(), "{:?}", r.system);
    assert_eq!(r.summary, PreshowSummary::default());
    // The notice itself stays in force for the operator: the list sent at the start, naming
    // the patch, is not sent again.
    assert_eq!(feed_codes(&s.take_outbound()), None);
}

const OTHER: &str = "fake:other";
const OTHER_NAME: &str = "Other";

/// An Audio cue `n` on the spare patch.
fn on_spare(n: u128) -> Cue {
    let mut c = audio(n, &wav(&dc(100, 0.5)), 0);
    if let CueKind::Audio { patch, .. } = &mut c.kind {
        *patch = spare_patch();
    }
    c
}

/// Adds cue `c` at `index` in the list.
fn add(s: &mut DeviceShow, c: Cue, index: usize) {
    edit(
        s,
        Command::AddCue {
            at: Position {
                list: list_id(),
                parent: None,
                index,
            },
            cue: c,
            auto_number: false,
        },
    );
}

/// A show whose one cue plays on the main patch, with a spare patch mapped to `spare`, opened
/// with "Other" plugged in (`other_is_default`: as the system's default output), which the
/// check passes; then "Other" is unplugged. Returns the outbound messages since the unplug.
fn spare_on_a_device_then_unplugged(
    spare: PatchMapping,
    other_is_default: bool,
    cues: Vec<Cue>,
) -> (DeviceShow, Vec<ServerMessage>) {
    let mut all = vec![audio(1, &wav(&dc(100, 0.5)), 0)];
    all.extend(cues);
    let mut ws = workspace(all);
    ws.patches.push(twin(&ws.patches[0], 902, "Spare"));
    let mut c = config_main_only(&ws);
    c.machine
        .set_patch_mapping(ws.id, spare_patch(), spare)
        .unwrap();
    let mut other = listed(OTHER, OTHER_NAME, 2);
    other.is_default = other_is_default;
    let mut s = DeviceShow::new(ws, vec![listed(DEV, DEV_NAME, 2), other], c);
    s.settle_devices();
    s.tick();
    let r = preshow(&mut s);
    assert!(r.ok, "before the unplug: {r:?}");
    s.take_outbound();
    s.fake.unplug(OTHER, true);
    s.settle_devices();
    s.tick();
    let msgs = s.take_outbound();
    (s, msgs)
}

fn on_other() -> PatchMapping {
    PatchMapping {
        device: OTHER_NAME.into(),
        channels: vec![0, 1],
        system: false,
    }
}

/// Whether the report's system problems hold `code` about the device named `device`.
fn counts(r: &PreshowReport, code: &str, device: &str) -> bool {
    r.system.iter().any(|w| {
        matches!(w, WireWarning::Notice { code: c, device: Some(d), .. }
            if c == code && d == device)
    })
}

/// Whether the report's system problems hold `code` about device "Other".
fn counts_other(r: &PreshowReport, code: &str) -> bool {
    counts(r, code, OTHER_NAME)
}

/// A spare patch mapped to a device unplugged after the show opened, no cue on it: the
/// pre-show check passes with no system problem; the Warnings list names the device.
#[test]
fn a_spare_patch_on_a_device_unplugged_after_opening_with_no_cue_passes_and_the_warnings_name_it() {
    let (mut s, msgs) = spare_on_a_device_then_unplugged(on_other(), false, vec![]);
    let notices = feed_notices(&msgs);
    assert!(
        notices
            .iter()
            .any(|(code, device, patch, _)| code == "device_lost"
                && device.as_deref() == Some(OTHER_NAME)
                && patch.is_none()),
        "{notices:?}"
    );
    let r = preshow(&mut s);
    assert!(r.ok, "{r:?}");
    assert!(r.system.is_empty(), "{:?}", r.system);
    assert_eq!(r.summary, PreshowSummary::default());
}

/// A cue on that patch: the check counts the device's loss; the cue removed, it goes.
#[test]
fn a_cue_on_the_spare_patch_counts_the_lost_device_and_it_goes_when_the_cue_is_removed() {
    let (mut s, _) = spare_on_a_device_then_unplugged(on_other(), false, vec![]);
    add(&mut s, on_spare(7), 1);
    let r = preshow(&mut s);
    assert!(!r.ok, "{r:?}");
    assert!(counts_other(&r, "device_lost"), "{:?}", r.system);
    assert_eq!(notice_codes(&r.system), vec!["device_lost"]);
    assert_eq!(r.summary.system, 1, "{r:?}");
    assert_eq!(broken_ids(&r), vec![id(7)]);

    edit(&mut s, Command::RemoveCue { id: id(7) });
    let r = preshow(&mut s);
    assert!(r.ok, "{r:?}");
    assert!(r.system.is_empty(), "{:?}", r.system);
    assert_eq!(r.summary, PreshowSummary::default());
}

/// A cue whose patch is on the system output, and the system's output device is unplugged: the
/// device's loss is counted, as the cue played on it.
#[test]
fn a_cue_on_the_system_output_counts_the_loss_of_the_device_it_played_on() {
    let (mut s, msgs) = spare_on_a_device_then_unplugged(
        PatchMapping::system_output(vec![0, 1]),
        true,
        vec![on_spare(7)],
    );
    let r = preshow(&mut s);
    assert!(!r.ok, "{r:?}");
    // The engine still names the system's device the cue played on: its loss is the problem.
    assert!(counts_other(&r, "device_lost"), "{:?}", r.system);
    assert_eq!(r.summary.system, 1, "{r:?}");
    assert!(
        feed_notices(&msgs)
            .iter()
            .any(|(code, device, _, _)| code == "device_lost"
                && device.as_deref() == Some(OTHER_NAME)),
        "the Warnings list names the device"
    );
    assert_eq!(broken_ids(&r), vec![id(7)]);
}

/// Moves the clock on by `ms` in 25 ms steps with the device threads answering; stops early
/// once the clock device is down.
fn idle_ms(s: &mut DeviceShow, ms: u64) {
    for _ in 0..ms / 25 {
        s.advance(std::time::Duration::from_millis(25));
        s.tick();
        s.wait_devices();
        if s.control().device_down() {
            return;
        }
    }
}

/// The only cue plays on a second device; the clock device, which no cue's patch is on,
/// stalls: the master clock every device's cues are dated on stands still, so the check fails
/// with `device_stalled`, and with `device_lost` once the stall is a loss.
#[test]
fn a_stall_or_loss_of_the_clock_device_fails_the_check_while_a_cue_plays_on_another_device() {
    let mut ws = workspace(vec![on_spare(7)]);
    ws.patches.push(twin(&ws.patches[0], 902, "Spare"));
    let mut c = config_main_only(&ws);
    map_to(&mut c, &ws, spare_patch(), OTHER_NAME);
    let mut s = DeviceShow::new(
        ws,
        vec![listed(DEV, DEV_NAME, 2), listed(OTHER, OTHER_NAME, 2)],
        c,
    );
    s.settle_devices();
    s.tick();
    s.run(BLOCK * 2);
    let r = preshow(&mut s);
    assert!(r.ok, "before the stall: {r:?}");

    s.fake.stall();
    idle_ms(&mut s, 600);
    assert!(!s.control().device_down(), "a stall, not yet a loss");
    let r = preshow(&mut s);
    assert!(!r.ok, "{r:?}");
    assert!(counts(&r, "device_stalled", DEV_NAME), "{:?}", r.system);
    assert_eq!(r.summary.system, 1, "{r:?}");

    idle_ms(&mut s, 3_000);
    assert!(s.control().device_down(), "the stall became a loss");
    let r = preshow(&mut s);
    assert!(!r.ok, "{r:?}");
    assert!(counts(&r, "device_lost", DEV_NAME), "{:?}", r.system);
}

/// A show of a Network cue and a Wait cue only, no cue on any patch: the clock device's stall
/// stops the wait's clock, so the check fails with `device_stalled`, then `device_lost`.
#[test]
fn a_stall_or_loss_of_the_clock_device_fails_the_check_of_a_show_with_no_audio() {
    let mut ws = workspace(vec![
        network_cue(5, net(51)),
        cue(6, CueKind::Wait { duration: 2.0 }),
    ]);
    ws.network_patches.push(lighting(NetworkProtocol::Udp));
    let c = config_main_only(&ws);
    let mut s = DeviceShow::new(ws, vec![listed(DEV, DEV_NAME, 2)], c);
    s.settle_devices();
    s.tick();
    s.run(BLOCK * 2);
    let r = preshow(&mut s);
    assert!(r.ok, "before the stall: {r:?}");

    s.fake.stall();
    idle_ms(&mut s, 600);
    assert!(!s.control().device_down(), "a stall, not yet a loss");
    let r = preshow(&mut s);
    assert!(!r.ok, "{r:?}");
    assert_eq!(notice_codes(&r.system), vec!["device_stalled"]);
    assert!(counts(&r, "device_stalled", DEV_NAME), "{:?}", r.system);

    idle_ms(&mut s, 3_000);
    assert!(s.control().device_down(), "the stall became a loss");
    let r = preshow(&mut s);
    assert!(!r.ok, "{r:?}");
    assert!(counts(&r, "device_lost", DEV_NAME), "{:?}", r.system);
}

/// The clock device has fewer outputs than the show was opened with, and no cue's patch is on
/// it: that touches neither its clock nor any cue, so the check passes and the Warnings list
/// names the device.
#[test]
fn the_clock_devices_dropped_outputs_with_no_cue_on_it_pass_the_check() {
    let mut ws = workspace(vec![on_spare(7)]);
    ws.patches.push(twin(&ws.patches[0], 902, "Spare"));
    let mut c = config_main_only(&ws);
    map_to(&mut c, &ws, spare_patch(), OTHER_NAME);
    let mut s = DeviceShow::new(
        ws,
        vec![listed(DEV, DEV_NAME, 1), listed(OTHER, OTHER_NAME, 2)],
        c,
    );
    s.settle_devices();
    s.tick();
    let msgs = s.take_outbound();
    assert!(
        feed_notices(&msgs)
            .iter()
            .any(|(code, device, patch, _)| code == "outputs_dropped"
                && device.as_deref() == Some(DEV_NAME)
                && patch.is_none()),
        "{:?}",
        feed_notices(&msgs)
    );
    let r = preshow(&mut s);
    assert!(r.ok, "{r:?}");
    assert!(r.system.is_empty(), "{:?}", r.system);
}

/// A cue whose patch has no device to name (on the system output while the system has no output
/// device): which device it will play on is not known, so every lost device counts.
#[test]
fn a_lost_device_counts_while_a_cues_patch_has_no_device_to_name() {
    let mut on_system = on_spare(7);
    if let CueKind::Audio { patch, .. } = &mut on_system.kind {
        *patch = PatchId(Uuid::from_u128(903));
    }
    let mut ws = workspace(vec![audio(1, &wav(&dc(100, 0.5)), 0), on_system]);
    ws.patches.push(twin(&ws.patches[0], 902, "Spare"));
    ws.patches.push(twin(&ws.patches[0], 903, "System"));
    let mut c = config_main_only(&ws);
    c.mixer_patches = 3;
    map_to(&mut c, &ws, spare_patch(), OTHER_NAME);
    c.machine
        .set_patch_mapping(
            ws.id,
            PatchId(Uuid::from_u128(903)),
            PatchMapping::system_output(vec![0, 1]),
        )
        .unwrap();
    let mut s = DeviceShow::new(
        ws,
        vec![listed(DEV, DEV_NAME, 2), listed(OTHER, OTHER_NAME, 2)],
        c,
    );
    s.settle_devices();
    s.tick();
    assert_eq!(
        notice_codes(&preshow(&mut s).system),
        vec!["patch_no_system_output"]
    );
    s.fake.unplug(OTHER, true);
    s.settle_devices();
    s.tick();
    let r = preshow(&mut s);
    assert!(counts_other(&r, "device_lost"), "{:?}", r.system);
    assert_eq!(
        notice_codes(&r.system),
        vec!["device_lost", "patch_no_system_output"]
    );
    assert_eq!(r.summary.system, 2, "{r:?}");
}

/// Every notice about one patch's trouble names the patch, and none about a patch no cue uses
/// is a pre-show problem: a device not plugged in, more channels than the device has, routing
/// past the device's outputs, a device that cannot be used, the system output with no device,
/// no free mixer patch.
#[test]
fn every_notice_about_a_patch_no_cue_uses_names_it_and_is_no_preshow_problem() {
    const SLOW: &str = "fake:slow";
    let good = wav(&dc(100, 0.5));
    let mut ws = workspace(vec![audio(1, &good, 0)]);
    let main = ws.patches[0].clone();
    let gone = twin(&main, 911, "On a missing device");
    let mut wide = twin(&main, 912, "Wider than the device");
    wide.outputs = vec![main.outputs[0].clone(); 4];
    let mut beyond = twin(&main, 913, "Routed past the device");
    beyond.crosspoints = vec![openlab_core::PatchCrosspoint {
        output: 0,
        device_output: 5,
        db: 0.0,
    }];
    let unusable = twin(&main, 914, "On an unusable device");
    let system = twin(&main, 915, "On the system output");
    let last = twin(&main, 916, "With no mixer patch");
    ws.patches
        .extend([gone, wide, beyond, unusable, system, last]);
    let mut c = config_main_only(&ws);
    c.mixer_patches = 6;
    map_to(&mut c, &ws, PatchId(Uuid::from_u128(911)), "Gone");
    c.machine
        .set_patch_mapping(
            ws.id,
            PatchId(Uuid::from_u128(912)),
            PatchMapping {
                device: DEV_NAME.into(),
                channels: vec![0, 1, 2, 3],
                system: false,
            },
        )
        .unwrap();
    map_to(&mut c, &ws, PatchId(Uuid::from_u128(913)), DEV_NAME);
    map_to(&mut c, &ws, PatchId(Uuid::from_u128(914)), "Slow");
    c.machine
        .set_patch_mapping(
            ws.id,
            PatchId(Uuid::from_u128(915)),
            PatchMapping::system_output(vec![0, 1]),
        )
        .unwrap();
    let mut s = DeviceShow::new(
        ws,
        vec![listed(DEV, DEV_NAME, 2), listed_at(SLOW, "Slow", 2, 4_000)],
        c,
    );
    s.settle_devices();
    s.tick();
    let notices = feed_notices(&s.take_outbound());
    let about =
        |want: &str, n: u128| {
            assert!(
                notices.iter().any(|(code, _, patch, _)| code == want
                    && *patch == Some(PatchId(Uuid::from_u128(n)))),
                "{want} about patch {n}: {notices:?}"
            );
        };
    about("patch_device_missing", 911);
    about("outputs_dropped", 912);
    about("patch_routing_beyond_device", 913);
    about("patch_device_unusable", 914);
    about("patch_no_system_output", 915);
    // Seven patches, six mixer patches: the last in the list has none.
    about("mixer_patches_exhausted", 916);

    let r = preshow(&mut s);
    assert!(r.ok, "{r:?}");
    assert!(r.system.is_empty(), "{:?}", r.system);
    assert_eq!(r.summary, PreshowSummary::default());
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
