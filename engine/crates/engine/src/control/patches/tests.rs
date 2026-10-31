// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

use openlab_core::{
    CueList, CueListId, PatchMapping, PatchOutput, TransportHotkeys, WorkspaceId, WorkspaceSettings,
};
use uuid::Uuid;

const D0: DocId = DocId(0);

fn idle() -> BTreeSet<u16> {
    BTreeSet::new()
}

fn busy(slots: &[u16]) -> BTreeSet<u16> {
    slots.iter().copied().collect()
}

fn id(n: u128) -> PatchId {
    PatchId(Uuid::from_u128(n))
}

/// A workspace with one-output patches `ids`, in that order, at 0 dB.
fn ws_of(ids: &[u128]) -> Workspace {
    let mut ws = ws_with(vec![out(0.0, false, false)]);
    ws.patches = ids
        .iter()
        .map(|&n| OutputPatch {
            id: id(n),
            name: format!("P{n}"),
            master_db: 0.0,
            outputs: vec![out(0.0, false, false)],
        })
        .collect();
    ws
}

/// Every patch of `ws` mapped to device channel `channel(n)` of "Dev".
fn mapped(ws: &Workspace, channel: impl Fn(u128) -> u32) -> MachineSettings {
    let mut s = MachineSettings::default();
    for p in &ws.patches {
        let n = p.id.0.as_u128();
        s.set_patch_mapping(
            ws.id,
            p.id,
            PatchMapping {
                device: "Dev".into(),
                channels: (0..p.outputs.len()).map(|_| channel(n)).collect(),
            },
        )
        .unwrap();
    }
    s
}

fn slot(p: &Patches, n: u128) -> Option<u16> {
    p.mixer_patch(D0, id(n))
}

fn touched(cmds: &[CommandKind]) -> BTreeSet<u16> {
    routes(cmds).into_iter().map(|r| r.0).collect()
}

fn pid() -> PatchId {
    PatchId(Uuid::from_u128(7))
}

fn ws_with(outputs: Vec<PatchOutput>) -> Workspace {
    Workspace {
        id: WorkspaceId(Uuid::from_u128(1)),
        name: String::new(),
        settings: WorkspaceSettings {
            min_go_interval: 0.0,
            panic_duration: 1.0,
            autosave_interval: 30.0,
            osc_access: openlab_core::OscAccess::default(),
            master_db: 0.0,
            hotkeys: TransportHotkeys::default(),
            auto_number_start: 1.0,
            auto_number_increment: 1.0,
        },
        patches: vec![OutputPatch {
            id: pid(),
            name: "Main".into(),
            master_db: -6.0,
            outputs,
        }],
        network_patches: vec![],
        cue_lists: vec![CueList {
            id: CueListId(Uuid::from_u128(2)),
            name: String::new(),
            cues: vec![],
        }],
    }
}

fn out(level_db: f64, mute: bool, polarity_inverted: bool) -> PatchOutput {
    PatchOutput {
        level_db,
        mute,
        polarity_inverted,
    }
}

fn settings(ws: &Workspace, channels: Vec<u32>) -> MachineSettings {
    let mut s = MachineSettings::default();
    s.set_patch_mapping(
        ws.id,
        pid(),
        PatchMapping {
            device: "Dev".into(),
            channels,
        },
    )
    .unwrap();
    s
}

fn device(n: u32) -> Vec<DeviceInfo> {
    vec![DeviceInfo {
        name: "Dev".into(),
        output_channels: n,
    }]
}

fn routes(cmds: &[CommandKind]) -> Vec<(u16, u16, u16, f32)> {
    cmds.iter()
        .filter_map(|c| match c {
            CommandKind::SetPatchRouting {
                patch,
                cue_output,
                device_output,
                gain,
            } => Some((*patch, *cue_output, *device_output, *gain)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_mapped_patch_routes_its_outputs_with_level_mute_and_polarity() {
    let ws = ws_with(vec![
        out(0.0, false, false),
        out(-6.0, false, true),
        out(0.0, true, false),
    ]);
    let mut p = Patches::new(2, 4);
    let (cmds, changed) = p.sync(D0, &ws, &settings(&ws, vec![3, 0, 1]), &device(4), &idle());
    assert_eq!(changed.len(), 1);
    assert!(matches!(changed[0].1, PatchResolution::Mapped { .. }));
    assert_eq!(p.mixer_patch(D0, pid()), Some(0));
    let m = db_to_gain(-6.0);
    let r = routes(&cmds);
    // Identity entries of patch 0 cleared, the mapped ones set.
    assert!(r.contains(&(0, 0, 0, 0.0)));
    assert!(r.contains(&(0, 0, 3, m)));
    assert!(r.contains(&(0, 1, 0, -(m * db_to_gain(-6.0)))));
    assert!(!r.iter().any(|&(_, o, d, g)| o == 2 && d == 1 && g != 0.0));
    // Patch 1 is unused: silent.
    assert!(r.contains(&(1, 1, 1, 0.0)));
    // The workspace master is in the matrices: the mixer's own master stays at unity.
    assert!(
        !cmds
            .iter()
            .any(|c| matches!(c, CommandKind::SetMaster { .. }))
    );
    // Nothing changed: nothing sent, nothing reported.
    let (again, changed) = p.sync(D0, &ws, &settings(&ws, vec![3, 0, 1]), &device(4), &idle());
    assert!(again.is_empty());
    assert!(changed.is_empty());
}

#[test]
fn unmapped_and_short_devices_are_reported_and_silent() {
    let ws = ws_with(vec![out(0.0, false, false); 2]);
    let mut p = Patches::new(1, 2);
    let (cmds, changed) = p.sync(D0, &ws, &MachineSettings::default(), &device(2), &idle());
    assert_eq!(changed, vec![(pid(), PatchResolution::Unmapped)]);
    assert!(routes(&cmds).iter().all(|r| r.3 == 0.0));
    // The device has 8 channels but the mixer only 2 outputs.
    let (_, changed) = p.sync(D0, &ws, &settings(&ws, vec![0, 7]), &device(8), &idle());
    assert!(matches!(
        changed[0].1,
        PatchResolution::ChannelsMissing {
            needed: 8,
            available: 2,
            ..
        }
    ));
}

#[test]
fn patches_beyond_the_mixer_are_unmapped() {
    let mut ws = ws_with(vec![out(0.0, false, false)]);
    let mut second = ws.patches[0].clone();
    second.id = PatchId(Uuid::from_u128(8));
    ws.patches.push(second);
    let mut p = Patches::new(1, 1);
    let (_, changed) = p.sync(D0, &ws, &settings(&ws, vec![0]), &device(1), &idle());
    assert_eq!(p.mixer_patch(D0, PatchId(Uuid::from_u128(8))), None);
    assert!(changed.contains(&(PatchId(Uuid::from_u128(8)), PatchResolution::Unmapped)));
}

#[test]
fn a_faulted_slot_gets_its_gains_again_and_a_disabled_one_is_never_reused_until_a_rebuild() {
    let ws = ws_with(vec![out(0.0, false, false); 2]);
    let mut p = Patches::new(2, 4);
    let (cmds, _) = p.sync(D0, &ws, &settings(&ws, vec![3, 1]), &device(4), &idle());
    assert_eq!(p.owner(0), Some((D0, pid())));
    assert_eq!(p.owner(1), None);
    // Reset to silence: the mapped gains, nothing else.
    let g = db_to_gain(-6.0);
    let sent: Vec<_> = routes(&cmds)
        .into_iter()
        .filter(|r| r.0 == 0 && r.3 != 0.0)
        .collect();
    assert_eq!(sent, vec![(0, 0, 3, g), (0, 1, 1, g)]);
    let mut gains = routes(&p.resend(0));
    gains.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(gains, sent);

    // Disabled: the patch moves to slot 1 at the next sync; slot 0 never comes back, even
    // when the doc closes.
    p.quarantine(0);
    assert_eq!(p.owner(0), None);
    let (cmds, _) = p.sync(D0, &ws, &settings(&ws, vec![3, 1]), &device(4), &idle());
    assert_eq!(p.mixer_patch(D0, pid()), Some(1));
    assert!(touched(&cmds).contains(&1));
    assert!(!touched(&cmds).contains(&0));
    p.release_doc(D0);
    assert_eq!(p.free_slots(), 1);

    // A rebuild frees it and sends every slot and the master again.
    let all = p.rebuilt();
    assert_eq!(p.free_slots(), 2);
    assert!(p.take_freed());
    assert!(
        all.iter()
            .any(|c| matches!(c, CommandKind::SetMaster { gain } if *gain == 1.0))
    );
}

#[test]
fn slots_follow_patch_ids_not_list_positions() {
    let mut p = Patches::new(4, 4);
    let ws = ws_of(&[10, 11, 12]);
    let chan = |n: u128| (n - 10) as u32;
    p.sync(D0, &ws, &mapped(&ws, chan), &device(4), &idle());
    assert_eq!(
        [slot(&p, 10), slot(&p, 11), slot(&p, 12)],
        [Some(0), Some(1), Some(2)]
    );
    let before = p.sent.clone();
    // Move the last to the front, remove the first, insert a new one before the others.
    let ws = ws_of(&[13, 12, 11]);
    let (cmds, _) = p.sync(D0, &ws, &mapped(&ws, chan), &device(4), &idle());
    assert_eq!(slot(&p, 11), Some(1));
    assert_eq!(slot(&p, 12), Some(2));
    assert_eq!(slot(&p, 10), None);
    // The new patch takes the slot never used (3), not the one just freed (0).
    assert_eq!(slot(&p, 13), Some(3));
    // The kept patches' matrices are not touched; the freed slot is silenced.
    assert_eq!(touched(&cmds), busy(&[0, 3]));
    assert_eq!(p.sent[1], before[1]);
    assert_eq!(p.sent[2], before[2]);
    assert!(p.sent[0].iter().all(|&g| g == 0.0));
    assert!(p.exhausted(D0).is_empty());
}

#[test]
fn a_removed_patch_keeps_its_routing_while_a_voice_uses_its_slot() {
    let mut p = Patches::new(2, 2);
    let ws = ws_of(&[10, 11]);
    let s = mapped(&ws, |n| (n - 10) as u32);
    p.sync(D0, &ws, &s, &device(2), &idle());
    let routed = p.sent[0].clone();
    // Patch 10 goes while a voice plays through its slot 0; patch 12 arrives.
    let ws2 = ws_of(&[11, 12]);
    let s2 = mapped(&ws2, |_| 1);
    let (cmds, changed) = p.sync(D0, &ws2, &s2, &device(2), &busy(&[0]));
    assert!(
        !touched(&cmds).contains(&0),
        "the retired slot is left as it was"
    );
    assert_eq!(p.sent[0], routed);
    assert!(p.has_retired(D0));
    assert!(!p.reclaimable(D0, &busy(&[0])));
    // No slot is free: 12 cannot play, and says why.
    assert_eq!(slot(&p, 12), None);
    assert_eq!(p.exhausted(D0), &[id(12)]);
    assert!(changed.contains(&(id(12), PatchResolution::Unmapped)));
    // The voice ends: the slot is zeroed, freed and given to the waiting patch.
    assert!(p.reclaimable(D0, &idle()));
    let (cmds, changed) = p.sync(D0, &ws2, &s2, &device(2), &idle());
    assert!(!p.has_retired(D0));
    assert_eq!(slot(&p, 12), Some(0));
    assert!(p.exhausted(D0).is_empty());
    assert!(touched(&cmds).contains(&0));
    assert!(matches!(
        changed.as_slice(),
        [(pid, PatchResolution::Mapped { .. })] if *pid == id(12)
    ));
    // Patch 12 maps its output to device output 1.
    assert_eq!(p.sent[0], vec![0.0, 1.0, 0.0, 0.0]);
}

#[test]
fn a_retired_slot_still_follows_the_workspace_master() {
    let mut p = Patches::new(2, 2);
    let ws = ws_of(&[10, 11]);
    let s = mapped(&ws, |n| (n - 10) as u32);
    p.sync(D0, &ws, &s, &device(2), &idle());
    let routed = p.sent[0].clone();
    let mut ws2 = ws_of(&[11]);
    p.sync(D0, &ws2, &s, &device(2), &busy(&[0]));
    ws2.settings.master_db = -6.0;
    p.sync(D0, &ws2, &s, &device(2), &busy(&[0]));
    let m = db_to_gain(-6.0);
    let scaled: Vec<f32> = routed.iter().map(|&g| m * g).collect();
    assert_eq!(p.sent[0], scaled);
    ws2.settings.master_db = 0.0;
    p.sync(D0, &ws2, &s, &device(2), &busy(&[0]));
    assert_eq!(p.sent[0], routed);
}

#[test]
fn a_removed_patch_that_comes_back_takes_its_retired_slot_again() {
    let mut p = Patches::new(3, 2);
    let ws = ws_of(&[10, 11]);
    let s = mapped(&ws, |n| (n - 10) as u32);
    p.sync(D0, &ws, &s, &device(2), &idle());
    let ws_without = ws_of(&[11]);
    p.sync(D0, &ws_without, &s, &device(2), &busy(&[0]));
    assert_eq!(slot(&p, 10), None);
    // Undo while the voice still plays: the same slot, nothing re-sent.
    let (cmds, _) = p.sync(D0, &ws, &s, &device(2), &busy(&[0]));
    assert_eq!(slot(&p, 10), Some(0));
    assert!(!p.has_retired(D0));
    assert!(!touched(&cmds).contains(&0));
}

#[test]
fn a_resize_under_a_playing_voice_moves_the_patch_and_freezes_the_old_slot() {
    let mut p = Patches::new(3, 4);
    let mut ws = ws_of(&[10]);
    let s = mapped(&ws, |_| 1);
    p.sync(D0, &ws, &s, &device(4), &idle());
    assert_eq!(slot(&p, 10), Some(0));
    let routed = p.sent[0].clone();
    // Two outputs now, while a voice plays through slot 0.
    ws.patches[0].outputs.push(out(0.0, false, false));
    let s2 = mapped(&ws, |_| 1);
    let (cmds, changed) = p.sync(D0, &ws, &s2, &device(4), &busy(&[0]));
    assert_eq!(slot(&p, 10), Some(1), "later starts get the new layout");
    assert_eq!(p.outputs(D0, id(10)), 2);
    assert_eq!(
        p.sent[0], routed,
        "the playing voice keeps the layout it started with"
    );
    assert!(!touched(&cmds).contains(&0));
    assert!(matches!(
        changed.as_slice(),
        [(_, PatchResolution::Mapped { channels, .. })] if channels == &[1, 1]
    ));
    // The voice ends: slot 0 is silenced and freed.
    let (cmds, _) = p.sync(D0, &ws, &s2, &device(4), &idle());
    assert!(touched(&cmds).contains(&0));
    assert!(p.sent[0].iter().all(|&g| g == 0.0));
    assert_eq!(slot(&p, 10), Some(1));
    // A resize with no voice on the slot is made in place.
    ws.patches[0].outputs.pop();
    let s3 = mapped(&ws, |_| 1);
    p.sync(D0, &ws, &s3, &device(4), &idle());
    assert_eq!(slot(&p, 10), Some(1));
    assert!(!p.has_retired(D0));
}

#[test]
fn a_resize_retired_slot_follows_the_mapping_and_keeps_outputs_the_patch_lost() {
    let map = |ws: &Workspace, channels: Vec<u32>| {
        let mut s = MachineSettings::default();
        s.set_patch_mapping(
            ws.id,
            id(10),
            PatchMapping {
                device: "Dev".into(),
                channels,
            },
        )
        .unwrap();
        s
    };
    let mut p = Patches::new(3, 4);
    let mut ws = ws_of(&[10]);
    ws.patches[0].outputs.push(out(-6.0, false, false));
    p.sync(D0, &ws, &map(&ws, vec![1, 2]), &device(4), &idle());
    let lost_row = p.sent[0][4..8].to_vec();
    assert_eq!(lost_row, vec![0.0, 0.0, db_to_gain(-6.0), 0.0]);
    // One output now, remapped to device output 3, while a voice plays on slot 0.
    ws.patches[0].outputs.pop();
    p.sync(D0, &ws, &map(&ws, vec![3]), &device(4), &busy(&[0]));
    assert_eq!(slot(&p, 10), Some(1));
    // Output 1 follows the new mapping; output 2, gone from the patch, keeps its gain.
    assert_eq!(&p.sent[0][0..4], &[0.0, 0.0, 0.0, 1.0]);
    assert_eq!(&p.sent[0][4..8], lost_row.as_slice());
    // A mapping that no longer fits: the channels it had when it was retired.
    p.sync(D0, &ws, &map(&ws, vec![3, 0]), &device(4), &busy(&[0]));
    assert_eq!(&p.sent[0][0..4], &[0.0, 1.0, 0.0, 0.0]);
    assert_eq!(&p.sent[0][4..8], lost_row.as_slice());
}

#[test]
fn more_patches_than_slots_are_unmapped_with_no_panic_until_one_frees() {
    let mut p = Patches::new(1, 1);
    let ws = ws_of(&[10, 11, 12]);
    let s = mapped(&ws, |_| 0);
    let (_, changed) = p.sync(D0, &ws, &s, &device(1), &idle());
    assert_eq!(p.capacity(), 1);
    assert_eq!(p.exhausted(D0), &[id(11), id(12)]);
    assert!(changed.contains(&(id(11), PatchResolution::Unmapped)));
    assert!(changed.contains(&(id(12), PatchResolution::Unmapped)));
    let ws = ws_of(&[11, 12]);
    p.sync(D0, &ws, &s, &device(1), &idle());
    assert_eq!(slot(&p, 11), Some(0));
    assert_eq!(p.exhausted(D0), &[id(12)]);
    // A mixer with no slots at all is no panic either.
    let mut none = Patches::new(0, 1);
    let (cmds, changed) = none.sync(D0, &ws, &s, &device(1), &idle());
    assert!(
        cmds.iter()
            .all(|c| !matches!(c, CommandKind::SetPatchRouting { .. }))
    );
    assert_eq!(changed.len(), 2);
}

#[test]
fn the_workspace_master_is_folded_into_the_matrix_gains() {
    let mut ws = ws_with(vec![out(-3.0, false, false), out(-1.5, false, true)]);
    let s = settings(&ws, vec![0, 1]);
    let mut p = Patches::new(1, 2);
    p.sync(D0, &ws, &s, &device(2), &idle());
    // At 0 dB the gains are bit for bit patch master × level (as before M4.15b).
    let pm = db_to_gain(-6.0);
    assert_eq!(p.sent[0][0].to_bits(), (pm * db_to_gain(-3.0)).to_bits());
    assert_eq!(p.sent[0][3].to_bits(), (-(pm * db_to_gain(-1.5))).to_bits());
    ws.settings.master_db = -10.0;
    let (cmds, changed) = p.sync(D0, &ws, &s, &device(2), &idle());
    assert!(changed.is_empty());
    let m = db_to_gain(-10.0);
    assert_eq!(
        p.sent[0][0].to_bits(),
        (m * (pm * db_to_gain(-3.0))).to_bits()
    );
    assert_eq!(
        p.sent[0][3].to_bits(),
        (m * -(pm * db_to_gain(-1.5))).to_bits()
    );
    assert!(
        !cmds
            .iter()
            .any(|c| matches!(c, CommandKind::SetMaster { .. }))
    );
    // Silence (-inf or below the floor) mutes every route.
    ws.settings.master_db = f64::NEG_INFINITY;
    p.sync(D0, &ws, &s, &device(2), &idle());
    assert!(p.sent[0].iter().all(|&g| g == 0.0));
}

const D1: DocId = DocId(1);
const D2: DocId = DocId(2);

#[test]
fn slots_are_keyed_by_doc_and_patch_and_a_closed_docs_go_back() {
    let mut p = Patches::new(2, 2);
    // Two docs with the same patch id (a copy of a show) get a slot each.
    let ws = ws_of(&[10]);
    let s = mapped(&ws, |_| 0);
    p.sync(D0, &ws, &s, &device(2), &idle());
    p.sync(D1, &ws, &s, &device(2), &idle());
    assert_eq!(p.mixer_patch(D0, id(10)), Some(0));
    assert_eq!(p.mixer_patch(D1, id(10)), Some(1));
    assert_eq!(p.free_slots(), 0);
    // A third finds none: unmapped, said so in its own list only.
    let (_, changed) = p.sync(D2, &ws, &s, &device(2), &idle());
    assert_eq!(changed, vec![(id(10), PatchResolution::Unmapped)]);
    assert_eq!(p.exhausted(D2), &[id(10)]);
    assert!(p.exhausted(D0).is_empty());
    assert!(!p.take_freed());
    // The first closes: its slot is silenced and freed at once; the waiting doc takes it.
    let routed = p.sent[1].clone();
    let cmds = p.release_doc(D0);
    assert_eq!(touched(&cmds), busy(&[0]));
    assert!(p.sent[0].iter().all(|&g| g == 0.0));
    assert!(p.take_freed());
    assert_eq!(p.mixer_patch(D0, id(10)), None);
    p.sync(D2, &ws, &s, &device(2), &idle());
    assert_eq!(p.mixer_patch(D2, id(10)), Some(0));
    assert!(p.exhausted(D2).is_empty());
    // The other doc's slot was never touched.
    assert_eq!(p.sent[1], routed);
    assert!(p.release_doc(D0).is_empty(), "closing twice does nothing");
}
