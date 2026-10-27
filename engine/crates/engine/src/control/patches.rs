// SPDX-License-Identifier: GPL-3.0-or-later

//! Output patches (AUD-07) on the mixer: which mixer patch matrix (slot) each workspace patch
//! uses, its resolution against the devices present (sent to the cue engine as `PatchStatus`),
//! and the matrix gains (workspace master × patch master × output level, mute, polarity) on the
//! mapped device outputs. The mixer's own master stays at unity (M4.15: the mixer is shared, the
//! master level is the workspace's).
//!
//! Slots are keyed by `PatchId`, not by a patch's place in the list, so adding, removing or
//! moving a patch never re-routes or silences a voice already playing (docs/control.md "Patch
//! slots"). A voice keeps the slot it started on; a slot whose patch is gone, or whose patch
//! changed its output count while a voice plays through it, is *retired*: it keeps the routing
//! it had until no voice uses it, then it is zeroed and freed.
//!
//! Patches resolve against the device the mixer plays on (the control loop's `playable` list:
//! empty during a device outage; docs/control.md "Devices"). Per
//! output alignment delay is a device-output setting in the mixer (`SetOutput`), not a matrix
//! gain: each device output gets the delay of the patch outputs mapped to it (the longest, if
//! patches disagree, listed as a conflict).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::num::NonZeroU32;

use openlab_audio::{CommandKind, MAX_PATCHES, OutputSettings};
use openlab_core::{
    DeviceInfo, MachineSettings, OutputPatch, PatchId, PatchOutput, PatchResolution, Workspace,
    db_to_gain, resolve_patch,
};

/// What a retired mixer slot was given to: a patch, at the output count it had then, and the
/// device channels its outputs were mapped to (empty if it was not mapped).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Holder {
    patch: PatchId,
    outputs: usize,
    channels: Vec<u32>,
}

pub struct Patches {
    /// The mixer slot of each workspace patch that has one (and the output count it was laid
    /// out for), stable while the patch exists.
    slots: HashMap<PatchId, (u16, usize)>,
    /// Slots a voice may still play through whose patch is gone or was resized, until no voice
    /// uses them. A removed patch's slot keeps its routing; a resized one keeps its layout but
    /// follows the patch's settings ([`Self::retired_base`]). Both stay under the workspace
    /// master.
    retired: BTreeMap<u16, Holder>,
    /// Free slots, the least recently freed first (a slot is reused as late as possible).
    free: VecDeque<u16>,
    /// Workspace patches with no slot: every slot is taken.
    exhausted: Vec<PatchId>,
    /// Patch output count, for meters.
    outputs: HashMap<PatchId, usize>,
    resolutions: HashMap<PatchId, PatchResolution>,
    /// Gains sent to the mixer, per mixer slot, `[cue output][device output]` row-major.
    sent: Vec<Vec<f32>>,
    /// The same before the workspace master (patch master × output level), so a retired slot
    /// follows a master change too.
    bases: Vec<Vec<f32>>,
    device_outputs: usize,
    rate: NonZeroU32,
    /// Delay sent for each device output (frames; the mixer starts at 0).
    delays: Vec<u32>,
    /// Device outputs that patches give different delays.
    conflicts: BTreeSet<u16>,
}

impl Patches {
    /// `mixer_patches` matrices of `device_outputs` square, identity as the mixer starts them.
    pub fn new(mixer_patches: usize, device_outputs: usize, rate: NonZeroU32) -> Self {
        let identity: Vec<f32> = (0..device_outputs * device_outputs)
            .map(|i| {
                if i / device_outputs == i % device_outputs {
                    1.0
                } else {
                    0.0
                }
            })
            .collect();
        let count = mixer_patches.min(MAX_PATCHES);
        Self {
            slots: HashMap::new(),
            retired: BTreeMap::new(),
            free: (0..count).filter_map(|p| u16::try_from(p).ok()).collect(),
            exhausted: Vec::new(),
            outputs: HashMap::new(),
            resolutions: HashMap::new(),
            sent: vec![identity; count],
            bases: vec![vec![0.0; device_outputs * device_outputs]; count],
            device_outputs,
            rate,
            delays: vec![0; device_outputs],
            conflicts: BTreeSet::new(),
        }
    }

    /// Follows the workspace's patches and the devices present. `busy`: the slots a voice in
    /// the mixer may still play through (`Voices::slots_in_use`). Returns the mixer commands
    /// that bring the matrices up to date, and the patches whose resolution changed (for
    /// `Input::PatchStatus`).
    pub fn sync(
        &mut self,
        ws: &Workspace,
        settings: &MachineSettings,
        devices: &[DeviceInfo],
        busy: &BTreeSet<u16>,
    ) -> (Vec<CommandKind>, Vec<(PatchId, PatchResolution)>) {
        let mut commands = Vec::new();
        let mut changed = Vec::new();
        self.allocate(ws, busy, &mut commands);
        let ws_master = db_to_gain(ws.settings.master_db);
        self.outputs.clear();
        let mut seen = HashMap::new();
        for patch in &ws.patches {
            let resolution = match self.slots.get(&patch.id) {
                Some(&(slot, _)) => {
                    let r = self.resolve(settings, ws, patch, devices);
                    let base = self.matrix(patch, &r);
                    self.apply(slot, base, ws_master, &mut commands);
                    r
                }
                // Every mixer slot is taken: the patch cannot play.
                None => PatchResolution::Unmapped,
            };
            self.outputs.insert(patch.id, patch.outputs.len());
            if self.resolutions.get(&patch.id) != Some(&resolution) {
                changed.push((patch.id, resolution.clone()));
            }
            seen.insert(patch.id, resolution);
        }
        self.resolutions = seen;
        let retired: Vec<(u16, Holder)> =
            self.retired.iter().map(|(&s, h)| (s, h.clone())).collect();
        for (slot, holder) in retired {
            let base = match ws.patches.iter().find(|p| p.id == holder.patch) {
                Some(patch) => self.retired_base(slot, &holder, patch),
                None => self.bases.get(usize::from(slot)).cloned(),
            };
            if let Some(base) = base {
                self.apply(slot, base, ws_master, &mut commands);
            }
        }
        self.sync_delays(ws, &mut commands);
        (commands, changed)
    }

    /// The matrix of a slot retired by a resize while its patch still exists: the layout the
    /// playing voices started with (their first `min(old, new)` outputs) follows the patch's
    /// current master, levels, mutes, polarity and, when it is mapped, its device channels (else
    /// the channels it had when it was retired). Outputs the patch no longer has keep their last
    /// gains: nothing controls them any more, and cutting them would change what is playing.
    fn retired_base(&self, slot: u16, holder: &Holder, patch: &OutputPatch) -> Option<Vec<f32>> {
        let n = self.device_outputs;
        let mut m = self.bases.get(usize::from(slot))?.clone();
        let channels: &[u32] = match self.resolutions.get(&patch.id) {
            Some(PatchResolution::Mapped { channels, .. }) => channels,
            _ => &holder.channels,
        };
        let master = db_to_gain(patch.master_db);
        let rows = holder.outputs.min(patch.outputs.len()).min(n);
        for o in 0..rows {
            if let Some(row) = m.get_mut(o * n..(o + 1) * n) {
                row.fill(0.0);
            }
            let (Some(out), Some(&ch)) = (patch.outputs.get(o), channels.get(o)) else {
                continue;
            };
            let Ok(d) = usize::try_from(ch) else { continue };
            if d >= n {
                continue;
            }
            if let Some(g) = m.get_mut(o * n + d) {
                *g = output_gain(master, out);
            }
        }
        Some(m)
    }

    /// Sends slot `slot` the gains `base` × the workspace master (exactly `base` at 0 dB).
    fn apply(&mut self, slot: u16, base: Vec<f32>, master: f32, commands: &mut Vec<CommandKind>) {
        self.diff(slot, &base, master, commands);
        if let Some(b) = self.bases.get_mut(usize::from(slot)) {
            *b = base;
        }
    }

    /// Gives every workspace patch a slot, keeping the one it has; retires and frees the others.
    fn allocate(&mut self, ws: &Workspace, busy: &BTreeSet<u16>, commands: &mut Vec<CommandKind>) {
        let live: HashMap<PatchId, usize> =
            ws.patches.iter().map(|p| (p.id, p.outputs.len())).collect();
        // A slot leaves its patch when the patch is gone, or resized while a voice plays
        // through it (the voice keeps the layout it started with; later starts get the new
        // one). A resize with no voice on the slot is made in place.
        let mut leaving = Vec::new();
        for (&patch, (slot, outputs)) in &mut self.slots {
            match live.get(&patch) {
                Some(&n) if n == *outputs => {}
                Some(&n) if !busy.contains(slot) => *outputs = n,
                _ => leaving.push((
                    *slot,
                    Holder {
                        patch,
                        outputs: *outputs,
                        channels: match self.resolutions.get(&patch) {
                            Some(PatchResolution::Mapped { channels, .. }) => channels.clone(),
                            _ => Vec::new(),
                        },
                    },
                )),
            }
        }
        for (slot, holder) in leaving {
            self.slots.remove(&holder.patch);
            self.retired.insert(slot, holder);
        }
        // A retired slot goes back to its patch when that has the same id and size again (an
        // undo) and no slot; else it is zeroed and freed once no voice uses it.
        let zero = vec![0.0; self.device_outputs * self.device_outputs];
        let retired: Vec<(u16, PatchId, usize)> = self
            .retired
            .iter()
            .map(|(&s, h)| (s, h.patch, h.outputs))
            .collect();
        for (slot, patch, outputs) in retired {
            if live.get(&patch) == Some(&outputs) && !self.slots.contains_key(&patch) {
                self.retired.remove(&slot);
                self.slots.insert(patch, (slot, outputs));
            } else if !busy.contains(&slot) {
                self.retired.remove(&slot);
                if let Some(b) = self.bases.get_mut(usize::from(slot)) {
                    b.fill(0.0);
                }
                self.free.push_back(slot);
            }
        }
        // New patches, in list order, take the least recently freed slots.
        self.exhausted.clear();
        for patch in &ws.patches {
            if self.slots.contains_key(&patch.id) {
                continue;
            }
            match self.free.pop_front() {
                Some(slot) => {
                    self.slots.insert(patch.id, (slot, patch.outputs.len()));
                }
                None => self.exhausted.push(patch.id),
            }
        }
        // Free slots are silent (the mixer starts them as identity; a freed one is zeroed).
        let free: Vec<u16> = self.free.iter().copied().collect();
        for slot in free {
            self.diff(slot, &zero, 1.0, commands);
        }
    }

    /// Whether a retired slot could be freed now that only `busy` slots are in use (the
    /// control loop then syncs again, so a patch waiting for a slot gets it).
    pub fn reclaimable(&self, busy: &BTreeSet<u16>) -> bool {
        self.retired.keys().any(|s| !busy.contains(s))
    }

    /// Whether any slot is retired (waiting for its last voice to end).
    pub fn has_retired(&self) -> bool {
        !self.retired.is_empty()
    }

    /// Workspace patches that got no mixer slot: all are taken.
    pub fn exhausted(&self) -> &[PatchId] {
        &self.exhausted
    }

    /// Mixer slots the patches can have.
    pub fn capacity(&self) -> usize {
        self.sent.len()
    }

    /// Each device output's delay: that of the mapped patch outputs routed to it.
    fn sync_delays(&mut self, ws: &Workspace, commands: &mut Vec<CommandKind>) {
        let n = self.device_outputs;
        let mut want: Vec<Option<u32>> = vec![None; n];
        self.conflicts.clear();
        for patch in &ws.patches {
            if !self.slots.contains_key(&patch.id) {
                continue;
            }
            let Some(PatchResolution::Mapped { channels, .. }) = self.resolutions.get(&patch.id)
            else {
                continue;
            };
            for (out, &ch) in patch.outputs.iter().zip(channels) {
                let Some(slot) = usize::try_from(ch).ok().and_then(|d| want.get_mut(d)) else {
                    continue;
                };
                let frames = delay_frames(out.delay_ms, self.rate);
                if let Some(old) = *slot
                    && old != frames
                    && let Ok(d) = u16::try_from(ch)
                {
                    self.conflicts.insert(d);
                }
                *slot = Some(slot.map_or(frames, |old| old.max(frames)));
            }
        }
        for (d, (sent, want)) in self.delays.iter_mut().zip(want).enumerate() {
            let want = want.unwrap_or(0);
            if *sent == want {
                continue;
            }
            let Ok(output) = u16::try_from(d) else {
                continue;
            };
            *sent = want;
            // Level, mute and polarity are in the matrix; the output itself only delays.
            commands.push(CommandKind::SetOutput {
                output,
                settings: OutputSettings {
                    delay_frames: want,
                    ..OutputSettings::default()
                },
            });
        }
    }

    /// Device outputs that patches give different delays (the longest is used).
    pub fn delay_conflicts(&self) -> impl Iterator<Item = u16> + '_ {
        self.conflicts.iter().copied()
    }

    /// A workspace patch's current resolution.
    pub fn resolution(&self, patch: PatchId) -> Option<&PatchResolution> {
        self.resolutions.get(&patch)
    }

    fn resolve(
        &self,
        settings: &MachineSettings,
        ws: &Workspace,
        patch: &OutputPatch,
        devices: &[DeviceInfo],
    ) -> PatchResolution {
        let r = resolve_patch(settings, ws.id, patch, devices);
        // The mixer was opened with fewer outputs than the device has: what is beyond them
        // cannot be heard.
        if let PatchResolution::Mapped { device, channels } = &r {
            let needed = channels.iter().max().map_or(0, |&m| m.saturating_add(1));
            if usize::try_from(needed).map_or(true, |n| n > self.device_outputs) {
                return PatchResolution::ChannelsMissing {
                    device: device.clone(),
                    needed,
                    available: u32::try_from(self.device_outputs).unwrap_or(u32::MAX),
                };
            }
        }
        r
    }

    /// The matrix a patch should have before the workspace master: cue output `o` → its mapped
    /// device channel at the patch's master × the output's level; silent when unmapped. The
    /// workspace master multiplies it last ([`Self::apply`]), so at 0 dB (exactly 1.0) the
    /// gains are bit for bit those sent when the master was the mixer's.
    fn matrix(&self, patch: &OutputPatch, r: &PatchResolution) -> Vec<f32> {
        let n = self.device_outputs;
        let mut m = vec![0.0; n * n];
        let PatchResolution::Mapped { channels, .. } = r else {
            return m;
        };
        let master = db_to_gain(patch.master_db);
        for (o, (out, &ch)) in patch.outputs.iter().zip(channels).enumerate() {
            let Ok(d) = usize::try_from(ch) else { continue };
            if o >= n || d >= n {
                continue;
            }
            if let Some(slot) = m.get_mut(o * n + d) {
                *slot = output_gain(master, out);
            }
        }
        m
    }

    /// Sends slot `p` the entries of `want` × `scale` that differ from what it has.
    fn diff(&mut self, p: u16, want: &[f32], scale: f32, commands: &mut Vec<CommandKind>) {
        let n = self.device_outputs;
        let Some(sent) = self.sent.get_mut(usize::from(p)) else {
            return;
        };
        for (i, (s, &w)) in sent.iter_mut().zip(want).enumerate() {
            let w = scale * w;
            if s.to_bits() == w.to_bits() {
                continue;
            }
            let (Ok(cue_output), Ok(device_output)) = (u16::try_from(i / n), u16::try_from(i % n))
            else {
                continue;
            };
            *s = w;
            commands.push(CommandKind::SetPatchRouting {
                patch: p,
                cue_output,
                device_output,
                gain: w,
            });
        }
    }

    /// The mixer slot a cue on `patch` starts through.
    pub fn mixer_patch(&self, patch: PatchId) -> Option<u16> {
        self.slots.get(&patch).map(|&(slot, _)| slot)
    }

    /// Output count of a workspace patch.
    pub fn outputs(&self, patch: PatchId) -> usize {
        self.outputs.get(&patch).copied().unwrap_or(0)
    }

    /// Every patch's current resolution (for `Input::Availability`).
    pub fn resolutions(&self) -> Vec<(PatchId, PatchResolution)> {
        self.resolutions
            .iter()
            .map(|(p, r)| (*p, r.clone()))
            .collect()
    }

    /// Device channel of each output of a mapped patch.
    pub fn channels(&self, patch: PatchId) -> Option<&[u32]> {
        match self.resolutions.get(&patch)? {
            PatchResolution::Mapped { channels, .. } => Some(channels),
            _ => None,
        }
    }
}

/// One patch output's gain: patch master × level (muted: 0; polarity: negative).
fn output_gain(master: f32, out: &PatchOutput) -> f32 {
    let g = if out.mute {
        0.0
    } else {
        master * db_to_gain(out.level_db)
    };
    if out.polarity_inverted { -g } else { g }
}

/// A delay in ms (the show file allows 0–1000) in frames, rounded; the mixer caps it at 1 s.
fn delay_frames(ms: f64, rate: NonZeroU32) -> u32 {
    if !ms.is_finite() || ms <= 0.0 {
        return 0;
    }
    let frames = (ms.min(1000.0) * f64::from(rate.get()) / 1000.0).round();
    // At most `rate` (1 s), so it fits.
    frames as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlab_core::{
        CueList, CueListId, PatchMapping, PatchOutput, TransportHotkeys, WorkspaceId,
        WorkspaceSettings,
    };
    use uuid::Uuid;

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
        p.mixer_patch(id(n))
    }

    fn touched(cmds: &[CommandKind]) -> BTreeSet<u16> {
        routes(cmds).into_iter().map(|r| r.0).collect()
    }

    fn rate() -> NonZeroU32 {
        NonZeroU32::new(8000).unwrap()
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
                osc_passcode: None,
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
            delay_ms: 0.0,
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
        let mut p = Patches::new(2, 4, rate());
        let (cmds, changed) = p.sync(&ws, &settings(&ws, vec![3, 0, 1]), &device(4), &idle());
        assert_eq!(changed.len(), 1);
        assert!(matches!(changed[0].1, PatchResolution::Mapped { .. }));
        assert_eq!(p.mixer_patch(pid()), Some(0));
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
        let (again, changed) = p.sync(&ws, &settings(&ws, vec![3, 0, 1]), &device(4), &idle());
        assert!(again.is_empty());
        assert!(changed.is_empty());
    }

    #[test]
    fn unmapped_and_short_devices_are_reported_and_silent() {
        let ws = ws_with(vec![out(0.0, false, false); 2]);
        let mut p = Patches::new(1, 2, rate());
        let (cmds, changed) = p.sync(&ws, &MachineSettings::default(), &device(2), &idle());
        assert_eq!(changed, vec![(pid(), PatchResolution::Unmapped)]);
        assert!(routes(&cmds).iter().all(|r| r.3 == 0.0));
        // The device has 8 channels but the mixer only 2 outputs.
        let (_, changed) = p.sync(&ws, &settings(&ws, vec![0, 7]), &device(8), &idle());
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
        let mut p = Patches::new(1, 1, rate());
        let (_, changed) = p.sync(&ws, &settings(&ws, vec![0]), &device(1), &idle());
        assert_eq!(p.mixer_patch(PatchId(Uuid::from_u128(8))), None);
        assert!(changed.contains(&(PatchId(Uuid::from_u128(8)), PatchResolution::Unmapped)));
    }

    fn delays(cmds: &[CommandKind]) -> Vec<(u16, u32)> {
        cmds.iter()
            .filter_map(|c| match c {
                CommandKind::SetOutput { output, settings } => {
                    assert_eq!(settings.level, 1.0);
                    assert!(!settings.mute && !settings.invert);
                    Some((*output, settings.delay_frames))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn output_delays_go_to_the_mapped_device_outputs() {
        let mut ws = ws_with(vec![out(0.0, false, false); 2]);
        ws.patches[0].outputs[0].delay_ms = 2.5;
        ws.patches[0].outputs[1].delay_ms = 1000.0;
        let mut p = Patches::new(2, 4, rate());
        let (cmds, _) = p.sync(&ws, &settings(&ws, vec![3, 1]), &device(4), &idle());
        // 2.5 ms at 8 kHz = 20 frames; 1 s = 8000 frames. Outputs left at 0 are not sent.
        assert_eq!(delays(&cmds), vec![(1, 8000), (3, 20)]);
        assert_eq!(p.delay_conflicts().count(), 0);
        // Unchanged: nothing sent. The device gone: the delays go back to 0.
        let (again, _) = p.sync(&ws, &settings(&ws, vec![3, 1]), &device(4), &idle());
        assert!(delays(&again).is_empty());
        let (gone, _) = p.sync(&ws, &settings(&ws, vec![3, 1]), &[], &idle());
        assert_eq!(delays(&gone), vec![(1, 0), (3, 0)]);
    }

    #[test]
    fn patches_that_disagree_on_an_output_delay_use_the_longest_and_say_so() {
        let mut ws = ws_with(vec![out(0.0, false, false)]);
        ws.patches[0].outputs[0].delay_ms = 1.0;
        let mut second = ws.patches[0].clone();
        second.id = PatchId(Uuid::from_u128(8));
        second.outputs[0].delay_ms = 3.0;
        ws.patches.push(second);
        let mut s = settings(&ws, vec![0]);
        s.set_patch_mapping(
            ws.id,
            PatchId(Uuid::from_u128(8)),
            PatchMapping {
                device: "Dev".into(),
                channels: vec![0],
            },
        )
        .unwrap();
        let mut p = Patches::new(2, 2, rate());
        let (cmds, _) = p.sync(&ws, &s, &device(2), &idle());
        assert_eq!(delays(&cmds), vec![(0, 24)]);
        assert_eq!(p.delay_conflicts().collect::<Vec<_>>(), vec![0]);
    }

    #[test]
    fn slots_follow_patch_ids_not_list_positions() {
        let mut p = Patches::new(4, 4, rate());
        let ws = ws_of(&[10, 11, 12]);
        let chan = |n: u128| (n - 10) as u32;
        p.sync(&ws, &mapped(&ws, chan), &device(4), &idle());
        assert_eq!(
            [slot(&p, 10), slot(&p, 11), slot(&p, 12)],
            [Some(0), Some(1), Some(2)]
        );
        let before = p.sent.clone();
        // Move the last to the front, remove the first, insert a new one before the others.
        let ws = ws_of(&[13, 12, 11]);
        let (cmds, _) = p.sync(&ws, &mapped(&ws, chan), &device(4), &idle());
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
        assert!(p.exhausted().is_empty());
    }

    #[test]
    fn a_removed_patch_keeps_its_routing_while_a_voice_uses_its_slot() {
        let mut p = Patches::new(2, 2, rate());
        let ws = ws_of(&[10, 11]);
        let s = mapped(&ws, |n| (n - 10) as u32);
        p.sync(&ws, &s, &device(2), &idle());
        let routed = p.sent[0].clone();
        // Patch 10 goes while a voice plays through its slot 0; patch 12 arrives.
        let ws2 = ws_of(&[11, 12]);
        let s2 = mapped(&ws2, |_| 1);
        let (cmds, changed) = p.sync(&ws2, &s2, &device(2), &busy(&[0]));
        assert!(
            !touched(&cmds).contains(&0),
            "the retired slot is left as it was"
        );
        assert_eq!(p.sent[0], routed);
        assert!(p.has_retired());
        assert!(!p.reclaimable(&busy(&[0])));
        // No slot is free: 12 cannot play, and says why.
        assert_eq!(slot(&p, 12), None);
        assert_eq!(p.exhausted(), &[id(12)]);
        assert!(changed.contains(&(id(12), PatchResolution::Unmapped)));
        // The voice ends: the slot is zeroed, freed and given to the waiting patch.
        assert!(p.reclaimable(&idle()));
        let (cmds, changed) = p.sync(&ws2, &s2, &device(2), &idle());
        assert!(!p.has_retired());
        assert_eq!(slot(&p, 12), Some(0));
        assert!(p.exhausted().is_empty());
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
        let mut p = Patches::new(2, 2, rate());
        let ws = ws_of(&[10, 11]);
        let s = mapped(&ws, |n| (n - 10) as u32);
        p.sync(&ws, &s, &device(2), &idle());
        let routed = p.sent[0].clone();
        let mut ws2 = ws_of(&[11]);
        p.sync(&ws2, &s, &device(2), &busy(&[0]));
        ws2.settings.master_db = -6.0;
        p.sync(&ws2, &s, &device(2), &busy(&[0]));
        let m = db_to_gain(-6.0);
        let scaled: Vec<f32> = routed.iter().map(|&g| m * g).collect();
        assert_eq!(p.sent[0], scaled);
        ws2.settings.master_db = 0.0;
        p.sync(&ws2, &s, &device(2), &busy(&[0]));
        assert_eq!(p.sent[0], routed);
    }

    #[test]
    fn a_removed_patch_that_comes_back_takes_its_retired_slot_again() {
        let mut p = Patches::new(3, 2, rate());
        let ws = ws_of(&[10, 11]);
        let s = mapped(&ws, |n| (n - 10) as u32);
        p.sync(&ws, &s, &device(2), &idle());
        let ws_without = ws_of(&[11]);
        p.sync(&ws_without, &s, &device(2), &busy(&[0]));
        assert_eq!(slot(&p, 10), None);
        // Undo while the voice still plays: the same slot, nothing re-sent.
        let (cmds, _) = p.sync(&ws, &s, &device(2), &busy(&[0]));
        assert_eq!(slot(&p, 10), Some(0));
        assert!(!p.has_retired());
        assert!(!touched(&cmds).contains(&0));
    }

    #[test]
    fn a_resize_under_a_playing_voice_moves_the_patch_and_freezes_the_old_slot() {
        let mut p = Patches::new(3, 4, rate());
        let mut ws = ws_of(&[10]);
        let s = mapped(&ws, |_| 1);
        p.sync(&ws, &s, &device(4), &idle());
        assert_eq!(slot(&p, 10), Some(0));
        let routed = p.sent[0].clone();
        // Two outputs now, while a voice plays through slot 0.
        ws.patches[0].outputs.push(out(0.0, false, false));
        let s2 = mapped(&ws, |_| 1);
        let (cmds, changed) = p.sync(&ws, &s2, &device(4), &busy(&[0]));
        assert_eq!(slot(&p, 10), Some(1), "later starts get the new layout");
        assert_eq!(p.outputs(id(10)), 2);
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
        let (cmds, _) = p.sync(&ws, &s2, &device(4), &idle());
        assert!(touched(&cmds).contains(&0));
        assert!(p.sent[0].iter().all(|&g| g == 0.0));
        assert_eq!(slot(&p, 10), Some(1));
        // A resize with no voice on the slot is made in place.
        ws.patches[0].outputs.pop();
        let s3 = mapped(&ws, |_| 1);
        p.sync(&ws, &s3, &device(4), &idle());
        assert_eq!(slot(&p, 10), Some(1));
        assert!(!p.has_retired());
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
        let mut p = Patches::new(3, 4, rate());
        let mut ws = ws_of(&[10]);
        ws.patches[0].outputs.push(out(-6.0, false, false));
        p.sync(&ws, &map(&ws, vec![1, 2]), &device(4), &idle());
        let lost_row = p.sent[0][4..8].to_vec();
        assert_eq!(lost_row, vec![0.0, 0.0, db_to_gain(-6.0), 0.0]);
        // One output now, remapped to device output 3, while a voice plays on slot 0.
        ws.patches[0].outputs.pop();
        p.sync(&ws, &map(&ws, vec![3]), &device(4), &busy(&[0]));
        assert_eq!(slot(&p, 10), Some(1));
        // Output 1 follows the new mapping; output 2, gone from the patch, keeps its gain.
        assert_eq!(&p.sent[0][0..4], &[0.0, 0.0, 0.0, 1.0]);
        assert_eq!(&p.sent[0][4..8], lost_row.as_slice());
        // A mapping that no longer fits: the channels it had when it was retired.
        p.sync(&ws, &map(&ws, vec![3, 0]), &device(4), &busy(&[0]));
        assert_eq!(&p.sent[0][0..4], &[0.0, 1.0, 0.0, 0.0]);
        assert_eq!(&p.sent[0][4..8], lost_row.as_slice());
    }

    #[test]
    fn more_patches_than_slots_are_unmapped_with_no_panic_until_one_frees() {
        let mut p = Patches::new(1, 1, rate());
        let ws = ws_of(&[10, 11, 12]);
        let s = mapped(&ws, |_| 0);
        let (_, changed) = p.sync(&ws, &s, &device(1), &idle());
        assert_eq!(p.capacity(), 1);
        assert_eq!(p.exhausted(), &[id(11), id(12)]);
        assert!(changed.contains(&(id(11), PatchResolution::Unmapped)));
        assert!(changed.contains(&(id(12), PatchResolution::Unmapped)));
        let ws = ws_of(&[11, 12]);
        p.sync(&ws, &s, &device(1), &idle());
        assert_eq!(slot(&p, 11), Some(0));
        assert_eq!(p.exhausted(), &[id(12)]);
        // A mixer with no slots at all is no panic either.
        let mut none = Patches::new(0, 1, rate());
        let (cmds, changed) = none.sync(&ws, &s, &device(1), &idle());
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
        let mut p = Patches::new(1, 2, rate());
        p.sync(&ws, &s, &device(2), &idle());
        // At 0 dB the gains are bit for bit patch master × level (as before M4.15b).
        let pm = db_to_gain(-6.0);
        assert_eq!(p.sent[0][0].to_bits(), (pm * db_to_gain(-3.0)).to_bits());
        assert_eq!(p.sent[0][3].to_bits(), (-(pm * db_to_gain(-1.5))).to_bits());
        ws.settings.master_db = -10.0;
        let (cmds, changed) = p.sync(&ws, &s, &device(2), &idle());
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
        p.sync(&ws, &s, &device(2), &idle());
        assert!(p.sent[0].iter().all(|&g| g == 0.0));
    }

    #[test]
    fn delay_frames_round_and_reject_bad_values() {
        assert_eq!(delay_frames(0.0, rate()), 0);
        assert_eq!(delay_frames(-1.0, rate()), 0);
        assert_eq!(delay_frames(f64::NAN, rate()), 0);
        assert_eq!(delay_frames(0.0626, rate()), 1);
        assert_eq!(delay_frames(5000.0, rate()), 8000);
    }
}
