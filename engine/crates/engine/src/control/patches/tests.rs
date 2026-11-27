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
            crosspoints: Vec::new(),
            device_outputs: Vec::new(),
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
                system: false,
            },
        )
        .unwrap();
    }
    s
}

fn slot(p: &Patches, n: u128) -> Option<u16> {
    p.mixer_patch(D0, id(n))
}

fn touched(cmds: &[(OutputKey, CommandKind)]) -> BTreeSet<u16> {
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
            output_max_db: 12.0,
            output_min_db: -60.0,
            hotkeys: TransportHotkeys::default(),
            auto_number_start: 1.0,
            auto_number_increment: 1.0,
            new_cues_auto_load: false,
            auto_number_new_cues: true,
            lock_playhead: true,
            go_needs_key_up: false,
            osc_enabled: true,
            copy_media_on_add: true,
        },
        patches: vec![OutputPatch {
            crosspoints: Vec::new(),
            device_outputs: Vec::new(),
            id: pid(),
            name: "Main".into(),
            master_db: -6.0,
            outputs,
        }],
        network_patches: vec![],
        routes: Vec::new(),
        stages: Vec::new(),
        show_mode: false,
        cue_lists: vec![CueList {
            id: CueListId(Uuid::from_u128(2)),
            name: String::new(),
            cues: vec![],
        }],
    }
}

fn out(level_db: f64, mute: bool, polarity_inverted: bool) -> PatchOutput {
    PatchOutput {
        name: String::new(),
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
            system: false,
        },
    )
    .unwrap();
    s
}

fn device(n: u32) -> Vec<(DeviceInfo, OutputKey)> {
    vec![(
        DeviceInfo {
            name: "Dev".into(),
            output_channels: n,
        },
        0,
    )]
}

fn routes(cmds: &[(OutputKey, CommandKind)]) -> Vec<(u16, u16, u16, f32)> {
    cmds.iter()
        .filter_map(|(_, c)| match c {
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
    let mut p = Patches::new(2, 4, 4);
    let (cmds, changed) = p.sync(
        D0,
        &ws,
        &settings(&ws, vec![3, 0, 1]),
        &device(4),
        None,
        &idle(),
    );
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
    // The levels are in the matrices: the mixer's own master stays at unity.
    assert!(
        !cmds
            .iter()
            .any(|(_, c)| matches!(c, CommandKind::SetMaster { .. }))
    );
    // Nothing changed: nothing sent, nothing reported.
    let (again, changed) = p.sync(
        D0,
        &ws,
        &settings(&ws, vec![3, 0, 1]),
        &device(4),
        None,
        &idle(),
    );
    assert!(again.is_empty());
    assert!(changed.is_empty());
}

#[test]
fn unmapped_and_short_devices_are_reported_and_silent() {
    let ws = ws_with(vec![out(0.0, false, false); 2]);
    let mut p = Patches::new(1, 2, 2);
    let (cmds, changed) = p.sync(
        D0,
        &ws,
        &MachineSettings::default(),
        &device(2),
        None,
        &idle(),
    );
    assert_eq!(changed, vec![(pid(), PatchResolution::Unmapped)]);
    assert!(routes(&cmds).iter().all(|r| r.3 == 0.0));
    // The device has 8 channels but the mixer only 2 outputs.
    let (_, changed) = p.sync(
        D0,
        &ws,
        &settings(&ws, vec![0, 7]),
        &device(8),
        None,
        &idle(),
    );
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
    let mut p = Patches::new(1, 1, 1);
    let (_, changed) = p.sync(D0, &ws, &settings(&ws, vec![0]), &device(1), None, &idle());
    assert_eq!(p.mixer_patch(D0, PatchId(Uuid::from_u128(8))), None);
    assert!(changed.contains(&(PatchId(Uuid::from_u128(8)), PatchResolution::Unmapped)));
}

#[test]
fn a_faulted_slot_gets_its_gains_again_and_a_disabled_one_is_never_reused_until_a_rebuild() {
    let ws = ws_with(vec![out(0.0, false, false); 2]);
    let mut p = Patches::new(2, 4, 4);
    let (cmds, _) = p.sync(
        D0,
        &ws,
        &settings(&ws, vec![3, 1]),
        &device(4),
        None,
        &idle(),
    );
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
    p.quarantine(0, 0);
    assert_eq!(p.owner(0), None);
    let (cmds, _) = p.sync(
        D0,
        &ws,
        &settings(&ws, vec![3, 1]),
        &device(4),
        None,
        &idle(),
    );
    assert_eq!(p.mixer_patch(D0, pid()), Some(1));
    assert!(touched(&cmds).contains(&1));
    assert!(!touched(&cmds).contains(&0));
    p.release_doc(D0);
    assert_eq!(p.free_slots(), 1);

    // A rebuild frees it and sends every slot and the master again.
    let all = p.rebuilt(0);
    assert_eq!(p.free_slots(), 2);
    assert!(p.take_freed());
    assert!(
        all.iter()
            .any(|(_, c)| matches!(c, CommandKind::SetMaster { gain } if *gain == 1.0))
    );
}

#[test]
fn slots_follow_patch_ids_not_list_positions() {
    let mut p = Patches::new(4, 4, 4);
    let ws = ws_of(&[10, 11, 12]);
    let chan = |n: u128| (n - 10) as u32;
    p.sync(D0, &ws, &mapped(&ws, chan), &device(4), None, &idle());
    assert_eq!(
        [slot(&p, 10), slot(&p, 11), slot(&p, 12)],
        [Some(0), Some(1), Some(2)]
    );
    let before = p.sent.clone();
    // Move the last to the front, remove the first, insert a new one before the others.
    let ws = ws_of(&[13, 12, 11]);
    let (cmds, _) = p.sync(D0, &ws, &mapped(&ws, chan), &device(4), None, &idle());
    assert_eq!(slot(&p, 11), Some(1));
    assert_eq!(slot(&p, 12), Some(2));
    assert_eq!(slot(&p, 10), None);
    // The new patch takes the slot never used (3), not the one just freed (0).
    assert_eq!(slot(&p, 13), Some(3));
    // The kept patches' matrices are not touched; the freed slot is silenced.
    assert_eq!(touched(&cmds), busy(&[0, 3]));
    assert_eq!(p.sent[&(0, 1)], before[&(0, 1)]);
    assert_eq!(p.sent[&(0, 2)], before[&(0, 2)]);
    assert!(p.sent[&(0, 0)].iter().all(|&g| g == 0.0));
    assert!(p.exhausted(D0).is_empty());
}

#[test]
fn a_removed_patch_keeps_its_routing_while_a_voice_uses_its_slot() {
    let mut p = Patches::new(2, 2, 2);
    let ws = ws_of(&[10, 11]);
    let s = mapped(&ws, |n| (n - 10) as u32);
    p.sync(D0, &ws, &s, &device(2), None, &idle());
    let routed = p.sent[&(0, 0)].clone();
    // Patch 10 goes while a voice plays through its slot 0; patch 12 arrives.
    let ws2 = ws_of(&[11, 12]);
    let s2 = mapped(&ws2, |_| 1);
    let (cmds, changed) = p.sync(D0, &ws2, &s2, &device(2), None, &busy(&[0]));
    assert!(
        !touched(&cmds).contains(&0),
        "the retired slot is left as it was"
    );
    assert_eq!(p.sent[&(0, 0)], routed);
    assert!(p.has_retired(D0));
    assert!(!p.reclaimable(D0, &busy(&[0])));
    // No slot is free: 12 cannot play, and says why.
    assert_eq!(slot(&p, 12), None);
    assert_eq!(p.exhausted(D0), &[id(12)]);
    assert!(changed.contains(&(id(12), PatchResolution::Unmapped)));
    // The voice ends: the slot is zeroed, freed and given to the waiting patch.
    assert!(p.reclaimable(D0, &idle()));
    let (cmds, changed) = p.sync(D0, &ws2, &s2, &device(2), None, &idle());
    assert!(!p.has_retired(D0));
    assert_eq!(slot(&p, 12), Some(0));
    assert!(p.exhausted(D0).is_empty());
    assert!(touched(&cmds).contains(&0));
    assert!(matches!(
        changed.as_slice(),
        [(pid, PatchResolution::Mapped { .. })] if *pid == id(12)
    ));
    // Patch 12 maps its output to device output 1.
    assert_eq!(p.sent[&(0, 0)], vec![0.0, 1.0, 0.0, 0.0]);
}

#[test]
fn a_removed_patch_that_comes_back_takes_its_retired_slot_again() {
    let mut p = Patches::new(3, 2, 2);
    let ws = ws_of(&[10, 11]);
    let s = mapped(&ws, |n| (n - 10) as u32);
    p.sync(D0, &ws, &s, &device(2), None, &idle());
    let ws_without = ws_of(&[11]);
    p.sync(D0, &ws_without, &s, &device(2), None, &busy(&[0]));
    assert_eq!(slot(&p, 10), None);
    // Undo while the voice still plays: the same slot, nothing re-sent.
    let (cmds, _) = p.sync(D0, &ws, &s, &device(2), None, &busy(&[0]));
    assert_eq!(slot(&p, 10), Some(0));
    assert!(!p.has_retired(D0));
    assert!(!touched(&cmds).contains(&0));
}

#[test]
fn a_resize_under_a_playing_voice_moves_the_patch_and_freezes_the_old_slot() {
    let mut p = Patches::new(3, 4, 4);
    let mut ws = ws_of(&[10]);
    let s = mapped(&ws, |_| 1);
    p.sync(D0, &ws, &s, &device(4), None, &idle());
    assert_eq!(slot(&p, 10), Some(0));
    let routed = p.sent[&(0, 0)].clone();
    // Two outputs now, while a voice plays through slot 0.
    ws.patches[0].outputs.push(out(0.0, false, false));
    let s2 = mapped(&ws, |_| 1);
    let (cmds, changed) = p.sync(D0, &ws, &s2, &device(4), None, &busy(&[0]));
    assert_eq!(slot(&p, 10), Some(1), "later starts get the new layout");
    assert_eq!(p.outputs(D0, id(10)), 2);
    assert_eq!(
        p.sent[&(0, 0)],
        routed,
        "the playing voice keeps the layout it started with"
    );
    assert!(!touched(&cmds).contains(&0));
    assert!(matches!(
        changed.as_slice(),
        [(_, PatchResolution::Mapped { channels, .. })] if channels == &[1, 1]
    ));
    // The voice ends: slot 0 is silenced and freed.
    let (cmds, _) = p.sync(D0, &ws, &s2, &device(4), None, &idle());
    assert!(touched(&cmds).contains(&0));
    assert!(p.sent[&(0, 0)].iter().all(|&g| g == 0.0));
    assert_eq!(slot(&p, 10), Some(1));
    // A resize with no voice on the slot is made in place.
    ws.patches[0].outputs.pop();
    let s3 = mapped(&ws, |_| 1);
    p.sync(D0, &ws, &s3, &device(4), None, &idle());
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
                system: false,
            },
        )
        .unwrap();
        s
    };
    let mut p = Patches::new(3, 4, 4);
    let mut ws = ws_of(&[10]);
    ws.patches[0].outputs.push(out(-6.0, false, false));
    p.sync(D0, &ws, &map(&ws, vec![1, 2]), &device(4), None, &idle());
    let lost_row = p.sent[&(0, 0)][4..8].to_vec();
    assert_eq!(lost_row, vec![0.0, 0.0, db_to_gain(-6.0), 0.0]);
    // One output now, remapped to device output 3, while a voice plays on slot 0.
    ws.patches[0].outputs.pop();
    p.sync(D0, &ws, &map(&ws, vec![3]), &device(4), None, &busy(&[0]));
    assert_eq!(slot(&p, 10), Some(1));
    // Output 1 follows the new mapping; output 2, gone from the patch, keeps its gain.
    assert_eq!(&p.sent[&(0, 0)][0..4], &[0.0, 0.0, 0.0, 1.0]);
    assert_eq!(&p.sent[&(0, 0)][4..8], lost_row.as_slice());
    // A mapping that no longer fits: the channels it had when it was retired.
    p.sync(
        D0,
        &ws,
        &map(&ws, vec![3, 0]),
        &device(4),
        None,
        &busy(&[0]),
    );
    assert_eq!(&p.sent[&(0, 0)][0..4], &[0.0, 1.0, 0.0, 0.0]);
    assert_eq!(&p.sent[&(0, 0)][4..8], lost_row.as_slice());
}

#[test]
fn more_patches_than_slots_are_unmapped_with_no_panic_until_one_frees() {
    let mut p = Patches::new(1, 1, 1);
    let ws = ws_of(&[10, 11, 12]);
    let s = mapped(&ws, |_| 0);
    let (_, changed) = p.sync(D0, &ws, &s, &device(1), None, &idle());
    assert_eq!(p.capacity(), 1);
    assert_eq!(p.exhausted(D0), &[id(11), id(12)]);
    assert!(changed.contains(&(id(11), PatchResolution::Unmapped)));
    assert!(changed.contains(&(id(12), PatchResolution::Unmapped)));
    let ws = ws_of(&[11, 12]);
    p.sync(D0, &ws, &s, &device(1), None, &idle());
    assert_eq!(slot(&p, 11), Some(0));
    assert_eq!(p.exhausted(D0), &[id(12)]);
    // A mixer with no slots at all is no panic either.
    let mut none = Patches::new(0, 1, 1);
    let (cmds, changed) = none.sync(D0, &ws, &s, &device(1), None, &idle());
    assert!(
        cmds.iter()
            .all(|(_, c)| !matches!(c, CommandKind::SetPatchRouting { .. }))
    );
    assert_eq!(changed.len(), 2);
}

#[test]
fn the_matrix_gains_are_patch_master_times_output_level() {
    let ws = ws_with(vec![out(-3.0, false, false), out(-1.5, false, true)]);
    let s = settings(&ws, vec![0, 1]);
    let mut p = Patches::new(1, 2, 2);
    p.sync(D0, &ws, &s, &device(2), None, &idle());
    // Bit for bit patch master × level, with polarity.
    let pm = db_to_gain(-6.0);
    assert_eq!(
        p.sent[&(0, 0)][0].to_bits(),
        (pm * db_to_gain(-3.0)).to_bits()
    );
    assert_eq!(
        p.sent[&(0, 0)][3].to_bits(),
        (-(pm * db_to_gain(-1.5))).to_bits()
    );
}

fn xp(output: u32, device_output: u32, db: f64) -> openlab_core::PatchCrosspoint {
    openlab_core::PatchCrosspoint {
        output,
        device_output,
        db,
    }
}

/// Bit for bit master × cue output level (polarity) × crosspoint × device output level; a
/// crosspoint on the mapped pair replaces the mapping's 0 dB; device outputs past the mixer's
/// width are never sent and never alias into the next row; a muted device output is silent.
#[test]
fn crosspoints_and_device_outputs_multiply_into_the_matrix() {
    let mut ws = ws_with(vec![out(-3.0, false, false), out(-1.5, false, true)]);
    ws.patches[0].crosspoints = vec![
        xp(0, 0, -6.0),
        xp(0, 2, -12.0),
        xp(0, 3, 0.0),
        xp(1, 0, 0.0),
        xp(1, 9, 0.0),
    ];
    ws.patches[0].device_outputs = vec![
        openlab_core::DeviceOutputLevel {
            device_output: 0,
            level_db: -10.0,
            mute: false,
            solo: false,
        },
        openlab_core::DeviceOutputLevel {
            device_output: 3,
            level_db: 0.0,
            mute: true,
            solo: false,
        },
    ];
    let s = settings(&ws, vec![0, 1]);
    let mut p = Patches::new(1, 2, 4);
    p.sync(D0, &ws, &s, &device(4), None, &idle());
    let pm = db_to_gain(-6.0);
    let (o0, o1) = (pm * db_to_gain(-3.0), -(pm * db_to_gain(-1.5)));
    let m = &p.sent[&(0, 0)];
    let want = [
        o0 * db_to_gain(-6.0) * db_to_gain(-10.0),
        0.0,
        o0 * db_to_gain(-12.0),
        0.0,
        o1 * db_to_gain(-10.0),
        o1,
        0.0,
        0.0,
    ];
    let bits = |v: &[f32]| v.iter().map(|g| g.to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(m), bits(&want));
    // Device output 10 is past this mixer's four: kept in the model, reported, not heard.
    assert_eq!(p.routing_beyond(D0, pid()), Some((10, 4)));
}

/// A soloed device output silences the patch's other device outputs (their mapped route and their
/// crosspoints alike) and leaves its own at its level; a muted one stays silent soloed; taking the
/// solo off brings the others back.
#[test]
fn a_soloed_device_output_silences_the_others_of_its_patch() {
    let mut ws = ws_with(vec![out(0.0, false, false), out(0.0, false, false)]);
    ws.patches[0].crosspoints = vec![xp(0, 1, -6.0)];
    let level = |device_output, mute, solo| openlab_core::DeviceOutputLevel {
        device_output,
        level_db: 0.0,
        mute,
        solo,
    };
    let s = settings(&ws, vec![0, 1]);
    let mut p = Patches::new(1, 2, 2);
    p.sync(D0, &ws, &s, &device(2), None, &idle());
    let bits = |v: &[f32]| v.iter().map(|g| g.to_bits()).collect::<Vec<_>>();
    // Cue output 0 goes to device output 0 (the mapping) and to 1 at -6 dB (a crosspoint); cue
    // output 1 goes to device output 1 (the mapping).
    // The test patch's master is -6 dB.
    let (o, x) = (db_to_gain(-6.0), db_to_gain(-6.0) * db_to_gain(-6.0));
    let open = bits(&p.sent[&(0, 0)]);
    assert_eq!(open, bits(&[o, x, 0.0, o]));
    ws.patches[0].device_outputs = vec![level(0, false, true)];
    p.sync(D0, &ws, &s, &device(2), None, &idle());
    assert_eq!(
        bits(&p.sent[&(0, 0)]),
        bits(&[o, 0.0, 0.0, 0.0]),
        "only device output 0 is heard"
    );
    ws.patches[0].device_outputs = vec![level(1, false, true)];
    p.sync(D0, &ws, &s, &device(2), None, &idle());
    assert_eq!(
        bits(&p.sent[&(0, 0)]),
        bits(&[0.0, x, 0.0, o]),
        "only device output 1 is heard, with what it carried"
    );
    ws.patches[0].device_outputs = vec![level(1, true, true)];
    p.sync(D0, &ws, &s, &device(2), None, &idle());
    assert_eq!(
        bits(&p.sent[&(0, 0)]),
        bits(&[0.0; 4]),
        "mute wins over solo"
    );
    ws.patches[0].device_outputs = vec![];
    p.sync(D0, &ws, &s, &device(2), None, &idle());
    assert_eq!(bits(&p.sent[&(0, 0)]), open, "no solo: all are heard");
}

/// A resize under a playing voice retires its slot; the retired slot still follows the patch's
/// crosspoints and device-output levels for the outputs both sizes have.
#[test]
fn a_retired_slot_follows_crosspoint_and_device_output_edits() {
    let mut ws = ws_with(vec![out(0.0, false, false), out(0.0, false, false)]);
    let mut p = Patches::new(2, 2, 2);
    p.sync(
        D0,
        &ws,
        &settings(&ws, vec![0, 1]),
        &device(2),
        None,
        &idle(),
    );
    ws.patches[0].outputs.pop();
    ws.patches[0].crosspoints = vec![xp(0, 1, -6.0)];
    ws.patches[0].device_outputs = vec![openlab_core::DeviceOutputLevel {
        device_output: 0,
        level_db: 0.0,
        mute: true,
        solo: false,
    }];
    p.sync(
        D0,
        &ws,
        &settings(&ws, vec![0]),
        &device(2),
        None,
        &busy(&[0]),
    );
    assert_eq!(slot(&p, 7), Some(1));
    // The patch master is -6 dB.
    assert_eq!(
        &p.sent[&(0, 0)][0..2],
        &[0.0, db_to_gain(-6.0) * db_to_gain(-6.0)]
    );
}

const D1: DocId = DocId(1);
const D2: DocId = DocId(2);

#[test]
fn slots_are_keyed_by_doc_and_patch_and_a_closed_docs_go_back() {
    let mut p = Patches::new(2, 2, 2);
    // Two docs with the same patch id (a copy of a show) get a slot each.
    let ws = ws_of(&[10]);
    let s = mapped(&ws, |_| 0);
    p.sync(D0, &ws, &s, &device(2), None, &idle());
    p.sync(D1, &ws, &s, &device(2), None, &idle());
    assert_eq!(p.mixer_patch(D0, id(10)), Some(0));
    assert_eq!(p.mixer_patch(D1, id(10)), Some(1));
    assert_eq!(p.free_slots(), 0);
    // A third finds none: unmapped, said so in its own list only.
    let (_, changed) = p.sync(D2, &ws, &s, &device(2), None, &idle());
    assert_eq!(changed, vec![(id(10), PatchResolution::Unmapped)]);
    assert_eq!(p.exhausted(D2), &[id(10)]);
    assert!(p.exhausted(D0).is_empty());
    assert!(!p.take_freed());
    // The first closes: its slot is silenced and freed at once; the waiting doc takes it.
    let routed = p.sent[&(0, 1)].clone();
    let cmds = p.release_doc(D0);
    assert_eq!(touched(&cmds), busy(&[0]));
    assert!(p.sent[&(0, 0)].iter().all(|&g| g == 0.0));
    assert!(p.take_freed());
    assert_eq!(p.mixer_patch(D0, id(10)), None);
    p.sync(D2, &ws, &s, &device(2), None, &idle());
    assert_eq!(p.mixer_patch(D2, id(10)), Some(0));
    assert!(p.exhausted(D2).is_empty());
    // The other doc's slot was never touched.
    assert_eq!(p.sent[&(0, 1)], routed);
    assert!(p.release_doc(D0).is_empty(), "closing twice does nothing");
}

fn routes_on(cmds: &[(OutputKey, CommandKind)], out: OutputKey) -> Vec<(u16, u16, u16, f32)> {
    routes(cmds)
        .into_iter()
        .zip(
            cmds.iter()
                .filter(|(_, c)| matches!(c, CommandKind::SetPatchRouting { .. })),
        )
        .filter(|(_, (o, _))| *o == out)
        .map(|(r, _)| r)
        .collect()
}

/// Two devices, "Dev" on output 0 and "Two" on output 1.
fn two_devices(n: u32) -> Vec<(DeviceInfo, OutputKey)> {
    let mut d = device(n);
    d.push((
        DeviceInfo {
            name: "Two".into(),
            output_channels: n,
        },
        1,
    ));
    d
}

fn on_device(ws: &Workspace, device: &str, channels: Vec<u32>) -> MachineSettings {
    let mut s = MachineSettings::default();
    s.set_patch_mapping(
        ws.id,
        pid(),
        PatchMapping {
            device: device.into(),
            channels,
            system: false,
        },
    )
    .unwrap();
    s
}

/// AUD-02: a slot's gains go to its patch's device's mixer. Moved to another device with no
/// voice on it, the slot follows in place: silenced on the device it leaves, routed on the new
/// one. Under a playing voice it is retired where it is and the patch takes a fresh slot there.
#[test]
fn a_slot_follows_its_patch_to_another_device_unless_a_voice_plays_through_it() {
    let ws = ws_with(vec![out(0.0, false, false), out(0.0, false, false)]);
    let mut p = Patches::new(2, 2, 2);
    let (cmds, _) = p.sync(
        D0,
        &ws,
        &on_device(&ws, "Dev", vec![0, 1]),
        &two_devices(2),
        None,
        &idle(),
    );
    let slot = p.mixer_patch(D0, pid()).unwrap();
    assert_eq!(p.slot_output(slot), Some(0));
    assert!(cmds.iter().all(|(o, _)| *o == 0));
    // Idle: moves in place.
    let (cmds, changed) = p.sync(
        D0,
        &ws,
        &on_device(&ws, "Two", vec![0, 1]),
        &two_devices(2),
        None,
        &idle(),
    );
    assert_eq!(changed.len(), 1);
    assert_eq!(p.mixer_patch(D0, pid()), Some(slot));
    assert_eq!(p.slot_output(slot), Some(1));
    let left = routes_on(&cmds, 0);
    assert!(
        !left.is_empty() && left.iter().all(|r| r.3 == 0.0),
        "{left:?}"
    );
    let gains: Vec<_> = routes_on(&cmds, 1)
        .into_iter()
        .filter(|r| r.3 != 0.0)
        .collect();
    assert_eq!(gains.len(), 2, "{gains:?}");
    // Busy: retired on "Two"; the patch takes a fresh slot on "Dev".
    let (_, _) = p.sync(
        D0,
        &ws,
        &on_device(&ws, "Dev", vec![0, 1]),
        &two_devices(2),
        None,
        &busy(&[slot]),
    );
    let fresh = p.mixer_patch(D0, pid()).unwrap();
    assert_ne!(fresh, slot);
    assert_eq!(p.slot_output(fresh), Some(0));
    assert_eq!(p.slot_output(slot), Some(1));
    assert!(p.has_retired(D0));
    // Once the voice has gone, the retired slot is freed.
    p.sync(
        D0,
        &ws,
        &on_device(&ws, "Dev", vec![0, 1]),
        &two_devices(2),
        None,
        &idle(),
    );
    assert!(!p.has_retired(D0));
}

/// AUD-02: each device's mixer has its own width. A wider second device widens the matrices
/// kept without changing what the first holds; a patch on it routes to its upper outputs and
/// sends nothing for columns a mixer does not have; on the narrower device, a channel beyond
/// its width is `channels_missing` with that device's own count.
#[test]
fn each_output_has_its_own_width() {
    let ws = ws_with(vec![out(0.0, false, false), out(0.0, false, false)]);
    let mut p = Patches::new(2, 2, 2);
    let devices = || {
        let mut d = two_devices(2);
        d[1].0.output_channels = 8;
        d
    };
    p.sync(
        D0,
        &ws,
        &on_device(&ws, "Dev", vec![0, 1]),
        &devices(),
        None,
        &idle(),
    );
    let on_dev = p.sent.clone();
    p.set_width(1, 8);
    for (k, m) in &p.sent {
        let was = &on_dev[k];
        // Rows of 2 widened to rows of 8, the same gains in the first two columns.
        assert_eq!(m.len(), 2 * 8);
        for o in 0..2 {
            assert_eq!(&m[o * 8..o * 8 + 2], &was[o * 2..o * 2 + 2]);
            assert!(m[o * 8 + 2..o * 8 + 8].iter().all(|&g| g == 0.0));
        }
    }
    // On the 8-output device's outputs 7 and 8.
    let (cmds, changed) = p.sync(
        D0,
        &ws,
        &on_device(&ws, "Two", vec![6, 7]),
        &devices(),
        None,
        &idle(),
    );
    assert!(matches!(changed[0].1, PatchResolution::Mapped { .. }));
    let gains: Vec<_> = routes_on(&cmds, 1)
        .into_iter()
        .filter(|r| r.3 != 0.0)
        .map(|r| (r.0, r.1, r.2))
        .collect();
    let slot = p.mixer_patch(D0, pid()).unwrap();
    assert_eq!(gains, vec![(slot, 0, 6), (slot, 1, 7)]);
    // Nothing for a column the 2-output mixer lacks.
    assert!(routes_on(&cmds, 0).iter().all(|r| r.2 < 2), "{cmds:?}");
    assert!(
        p.resend_all(0)
            .iter()
            .all(|(_, c)| !matches!(c, CommandKind::SetPatchRouting { device_output, .. } if *device_output >= 2))
    );
    // Back on the 2-output device, mapped beyond it: its own count is what is missing.
    let (_, changed) = p.sync(
        D0,
        &ws,
        &on_device(&ws, "Dev", vec![2, 3]),
        &two_devices(8),
        None,
        &idle(),
    );
    assert_eq!(
        changed[0].1,
        PatchResolution::ChannelsMissing {
            device: "Dev".into(),
            needed: 4,
            available: 2,
        }
    );
}
