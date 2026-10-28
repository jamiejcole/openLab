// SPDX-License-Identifier: GPL-3.0-or-later

//! Output patches (AUD-07) on the mixer: which mixer patch matrix (slot) each workspace patch
//! uses, its resolution against the devices present (sent to the cue engine as `PatchStatus`),
//! and the matrix gains (workspace master × patch master × output level, mute, polarity) on the
//! mapped device outputs. The mixer's own master stays at unity (M4.15: the mixer is shared, the
//! master level is the workspace's).
//!
//! The mixer is shared by every open doc (ARCHITECTURE §3.5): slots are keyed by `(DocId,
//! PatchId)`, not by a patch's place in a list, so adding, removing or moving a patch never
//! re-routes or silences a voice already playing (docs/control.md "Patch slots"). A slot belongs
//! to one doc's patch at a time, so only that doc's voices can play through it. A voice keeps
//! the slot it started on; a slot whose patch is gone, or whose patch changed its output count
//! while a voice plays through it, is *retired*: it keeps the routing it had until no voice
//! uses it, then it is zeroed and freed. A closed doc's slots are zeroed and freed at once
//! ([`Patches::release_doc`]): its voices were stopped first.
//!
//! Patches resolve against the device the mixer plays on (the control loop's `playable` list:
//! empty during a device outage; docs/control.md "Devices"). Per output alignment delay is a
//! device-output setting in the mixer (`SetOutput`), not a matrix gain: each device output gets
//! the delay of the patch outputs mapped to it, over every doc's patches (the longest, if
//! patches disagree, listed as a conflict in each doc that maps to that output).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::num::NonZeroU32;

use openlab_audio::{CommandKind, MAX_PATCHES, OutputSettings};
use openlab_core::{
    DeviceInfo, MachineSettings, OutputPatch, PatchId, PatchOutput, PatchResolution, Workspace,
    db_to_gain, resolve_patch,
};

use super::DocId;

/// What a retired mixer slot was given to: a patch, at the output count it had then, and the
/// device channels its outputs were mapped to (empty if it was not mapped).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Holder {
    patch: PatchId,
    outputs: usize,
    channels: Vec<u32>,
}

/// One doc's part of the slots.
#[derive(Default)]
struct DocSlots {
    /// The mixer slot of each workspace patch that has one (and the output count it was laid
    /// out for), stable while the patch exists.
    slots: HashMap<PatchId, (u16, usize)>,
    /// Slots a voice may still play through whose patch is gone or was resized, until no voice
    /// uses them. A removed patch's slot keeps its routing; a resized one keeps its layout but
    /// follows the patch's settings ([`Patches::retired_base`]). Both stay under the workspace
    /// master.
    retired: BTreeMap<u16, Holder>,
    /// Workspace patches with no slot: every slot is taken.
    exhausted: Vec<PatchId>,
    /// Patch output count, for meters.
    outputs: HashMap<PatchId, usize>,
    resolutions: HashMap<PatchId, PatchResolution>,
    /// The delay each device output should have for this doc's mapped patches (`None`: none
    /// of them maps to it).
    delays: Vec<Option<u32>>,
    /// Device outputs this doc's own patches give different delays.
    conflicts: BTreeSet<u16>,
}

pub struct Patches {
    docs: BTreeMap<DocId, DocSlots>,
    /// Free slots, the least recently freed first (a slot is reused as late as possible).
    free: VecDeque<u16>,
    /// A slot went back to the free list since [`Self::take_freed`]: a doc waiting for one
    /// can have it.
    freed: bool,
    /// Gains sent to the mixer, per mixer slot, `[cue output][device output]` row-major.
    sent: Vec<Vec<f32>>,
    /// The same before the workspace master (patch master × output level), so a retired slot
    /// follows a master change too.
    bases: Vec<Vec<f32>>,
    device_outputs: usize,
    rate: NonZeroU32,
    /// Delay sent for each device output (frames; the mixer starts at 0).
    delays: Vec<u32>,
    /// Device outputs that patches (of one doc or of several) give different delays.
    conflicts: BTreeSet<u16>,
    /// The conflicts changed since [`Self::take_conflicts_changed`]: every doc's notices
    /// should be looked at again (another doc's change can make or end one).
    conflicts_changed: bool,
    /// Slots the mixer disabled after repeated faults ([`Self::quarantine`]).
    quarantined: BTreeSet<u16>,
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
            docs: BTreeMap::new(),
            free: (0..count).filter_map(|p| u16::try_from(p).ok()).collect(),
            freed: false,
            sent: vec![identity; count],
            bases: vec![vec![0.0; device_outputs * device_outputs]; count],
            device_outputs,
            rate,
            delays: vec![0; device_outputs],
            conflicts: BTreeSet::new(),
            conflicts_changed: false,
            quarantined: BTreeSet::new(),
        }
    }

    /// Follows doc `doc`'s patches and the devices present. `busy`: the slots a voice of that
    /// doc in the mixer may still play through (`Voices::slots_in_use`; no other doc's voice
    /// can be on its slots). Returns the mixer commands that bring the matrices (and the
    /// device outputs' delays, over every doc) up to date, and the patches whose resolution
    /// changed (for `Input::PatchStatus`).
    pub fn sync(
        &mut self,
        doc: DocId,
        ws: &Workspace,
        settings: &MachineSettings,
        devices: &[DeviceInfo],
        busy: &BTreeSet<u16>,
    ) -> (Vec<CommandKind>, Vec<(PatchId, PatchResolution)>) {
        // A doc's first sync (it was just opened): its slots, which nothing plays through, take
        // their gains at once instead of ramping in (M4.15e review L1).
        let first = !self.docs.contains_key(&doc);
        let mut d = self.docs.remove(&doc).unwrap_or_default();
        let mut commands = Vec::new();
        let mut changed = Vec::new();
        self.allocate(&mut d, ws, busy, &mut commands);
        let ws_master = db_to_gain(ws.settings.master_db);
        d.outputs.clear();
        let mut seen = HashMap::new();
        for patch in &ws.patches {
            let resolution = match d.slots.get(&patch.id) {
                Some(&(slot, _)) => {
                    let r = self.resolve(settings, ws, patch, devices);
                    let base = self.matrix(patch, &r);
                    self.apply(slot, base, ws_master, &mut commands);
                    r
                }
                // Every mixer slot is taken: the patch cannot play.
                None => PatchResolution::Unmapped,
            };
            d.outputs.insert(patch.id, patch.outputs.len());
            if d.resolutions.get(&patch.id) != Some(&resolution) {
                changed.push((patch.id, resolution.clone()));
            }
            seen.insert(patch.id, resolution);
        }
        d.resolutions = seen;
        let retired: Vec<(u16, Holder)> = d.retired.iter().map(|(&s, h)| (s, h.clone())).collect();
        for (slot, holder) in retired {
            let base = match ws.patches.iter().find(|p| p.id == holder.patch) {
                Some(patch) => self.retired_base(&d, slot, &holder, patch),
                None => self.bases.get(usize::from(slot)).cloned(),
            };
            if let Some(base) = base {
                self.apply(slot, base, ws_master, &mut commands);
            }
        }
        if first {
            let mut slots: Vec<u16> = d.slots.values().map(|&(slot, _)| slot).collect();
            slots.sort_unstable();
            commands.extend(
                slots
                    .into_iter()
                    .map(|patch| CommandKind::SnapPatch { patch }),
            );
        }
        self.doc_delays(&mut d, ws);
        self.docs.insert(doc, d);
        self.sync_delays(&mut commands);
        (commands, changed)
    }

    /// A doc closed (its voices were stopped first): every slot it had, in use or retired, is
    /// zeroed and freed, and the device outputs' delays follow the docs left. Returns the
    /// mixer commands.
    pub fn release_doc(&mut self, doc: DocId) -> Vec<CommandKind> {
        let mut commands = Vec::new();
        let Some(d) = self.docs.remove(&doc) else {
            return commands;
        };
        let zero = vec![0.0; self.device_outputs * self.device_outputs];
        let mut slots: Vec<u16> = d
            .slots
            .values()
            .map(|&(slot, _)| slot)
            .chain(d.retired.keys().copied())
            .collect();
        slots.sort_unstable();
        for slot in slots {
            self.diff(slot, &zero, 1.0, &mut commands);
            if let Some(b) = self.bases.get_mut(usize::from(slot)) {
                b.fill(0.0);
            }
            self.free.push_back(slot);
            self.freed = true;
        }
        self.sync_delays(&mut commands);
        commands
    }

    /// The matrix of a slot retired by a resize while its patch still exists: the layout the
    /// playing voices started with (their first `min(old, new)` outputs) follows the patch's
    /// current master, levels, mutes, polarity and, when it is mapped, its device channels (else
    /// the channels it had when it was retired). Outputs the patch no longer has keep their last
    /// gains: nothing controls them any more, and cutting them would change what is playing.
    fn retired_base(
        &self,
        d: &DocSlots,
        slot: u16,
        holder: &Holder,
        patch: &OutputPatch,
    ) -> Option<Vec<f32>> {
        let n = self.device_outputs;
        let mut m = self.bases.get(usize::from(slot))?.clone();
        let channels: &[u32] = match d.resolutions.get(&patch.id) {
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
            let Ok(dev) = usize::try_from(ch) else {
                continue;
            };
            if dev >= n {
                continue;
            }
            if let Some(g) = m.get_mut(o * n + dev) {
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

    /// Gives every patch of the doc a slot, keeping the one it has; retires and frees the
    /// others.
    fn allocate(
        &mut self,
        d: &mut DocSlots,
        ws: &Workspace,
        busy: &BTreeSet<u16>,
        commands: &mut Vec<CommandKind>,
    ) {
        let live: HashMap<PatchId, usize> =
            ws.patches.iter().map(|p| (p.id, p.outputs.len())).collect();
        // A slot leaves its patch when the patch is gone, or resized while a voice plays
        // through it (the voice keeps the layout it started with; later starts get the new
        // one). A resize with no voice on the slot is made in place.
        let mut leaving = Vec::new();
        for (&patch, (slot, outputs)) in &mut d.slots {
            match live.get(&patch) {
                Some(&n) if n == *outputs => {}
                Some(&n) if !busy.contains(slot) => *outputs = n,
                _ => leaving.push((
                    *slot,
                    Holder {
                        patch,
                        outputs: *outputs,
                        channels: match d.resolutions.get(&patch) {
                            Some(PatchResolution::Mapped { channels, .. }) => channels.clone(),
                            _ => Vec::new(),
                        },
                    },
                )),
            }
        }
        for (slot, holder) in leaving {
            d.slots.remove(&holder.patch);
            d.retired.insert(slot, holder);
        }
        // A retired slot goes back to its patch when that has the same id and size again (an
        // undo) and no slot; else it is zeroed and freed once no voice uses it.
        let zero = vec![0.0; self.device_outputs * self.device_outputs];
        let retired: Vec<(u16, PatchId, usize)> = d
            .retired
            .iter()
            .map(|(&s, h)| (s, h.patch, h.outputs))
            .collect();
        for (slot, patch, outputs) in retired {
            if live.get(&patch) == Some(&outputs) && !d.slots.contains_key(&patch) {
                d.retired.remove(&slot);
                d.slots.insert(patch, (slot, outputs));
            } else if !busy.contains(&slot) {
                d.retired.remove(&slot);
                if let Some(b) = self.bases.get_mut(usize::from(slot)) {
                    b.fill(0.0);
                }
                self.free.push_back(slot);
                self.freed = true;
            }
        }
        // New patches, in list order, take the least recently freed slots.
        d.exhausted.clear();
        for patch in &ws.patches {
            if d.slots.contains_key(&patch.id) {
                continue;
            }
            match self.free.pop_front() {
                Some(slot) => {
                    d.slots.insert(patch.id, (slot, patch.outputs.len()));
                }
                None => d.exhausted.push(patch.id),
            }
        }
        // Free slots are silent (the mixer starts them as identity; a freed one is zeroed).
        let free: Vec<u16> = self.free.iter().copied().collect();
        for slot in free {
            self.diff(slot, &zero, 1.0, commands);
        }
    }

    /// Whether a retired slot of `doc` could be freed now that only `busy` slots are in use
    /// (the control loop then syncs again, so a patch waiting for a slot gets it).
    pub fn reclaimable(&self, doc: DocId, busy: &BTreeSet<u16>) -> bool {
        self.docs
            .get(&doc)
            .is_some_and(|d| d.retired.keys().any(|s| !busy.contains(s)))
    }

    /// Whether any slot of `doc` is retired (waiting for its last voice to end).
    pub fn has_retired(&self, doc: DocId) -> bool {
        self.docs.get(&doc).is_some_and(|d| !d.retired.is_empty())
    }

    /// Patches of `doc` that got no mixer slot: all are taken.
    pub fn exhausted(&self, doc: DocId) -> &[PatchId] {
        self.docs.get(&doc).map_or(&[], |d| d.exhausted.as_slice())
    }

    /// True once after a slot went back to the free list: the docs with exhausted patches
    /// should sync again.
    pub fn take_freed(&mut self) -> bool {
        std::mem::take(&mut self.freed)
    }

    /// True once after the delay conflicts changed.
    pub fn take_conflicts_changed(&mut self) -> bool {
        std::mem::take(&mut self.conflicts_changed)
    }

    /// Mixer slots the patches can have.
    pub fn capacity(&self) -> usize {
        self.sent.len()
    }

    /// Slots no doc has (in use or retired).
    pub fn free_slots(&self) -> usize {
        self.free.len()
    }

    /// The delay each device output should have for the doc's mapped patches.
    fn doc_delays(&self, d: &mut DocSlots, ws: &Workspace) {
        let n = self.device_outputs;
        let mut want: Vec<Option<u32>> = vec![None; n];
        d.conflicts.clear();
        for patch in &ws.patches {
            if !d.slots.contains_key(&patch.id) {
                continue;
            }
            let Some(PatchResolution::Mapped { channels, .. }) = d.resolutions.get(&patch.id)
            else {
                continue;
            };
            for (out, &ch) in patch.outputs.iter().zip(channels) {
                let Some(slot) = usize::try_from(ch).ok().and_then(|o| want.get_mut(o)) else {
                    continue;
                };
                let frames = delay_frames(out.delay_ms, self.rate);
                if let Some(old) = *slot
                    && old != frames
                    && let Ok(o) = u16::try_from(ch)
                {
                    d.conflicts.insert(o);
                }
                *slot = Some(slot.map_or(frames, |old| old.max(frames)));
            }
        }
        d.delays = want;
    }

    /// Each device output's delay: the longest any doc's mapped patches want for it (one value
    /// per physical output, so docs that disagree are a conflict too).
    fn sync_delays(&mut self, commands: &mut Vec<CommandKind>) {
        let n = self.device_outputs;
        let mut want: Vec<Option<u32>> = vec![None; n];
        let before = std::mem::take(&mut self.conflicts);
        for d in self.docs.values() {
            self.conflicts.extend(d.conflicts.iter().copied());
            for (o, (slot, doc_want)) in want.iter_mut().zip(&d.delays).enumerate() {
                let Some(frames) = *doc_want else { continue };
                if let Some(old) = *slot
                    && old != frames
                    && let Ok(o) = u16::try_from(o)
                {
                    self.conflicts.insert(o);
                }
                *slot = Some(slot.map_or(frames, |old| old.max(frames)));
            }
        }
        if before != self.conflicts {
            self.conflicts_changed = true;
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

    /// Device outputs `doc`'s patches map to that patches give different delays (within the
    /// doc or across docs; the longest is used).
    pub fn delay_conflicts(&self, doc: DocId) -> impl Iterator<Item = u16> + '_ {
        let wants = self.docs.get(&doc).map_or(&[][..], |d| d.delays.as_slice());
        self.conflicts.iter().copied().filter(move |&o| {
            wants
                .get(usize::from(o))
                .is_some_and(std::option::Option::is_some)
        })
    }

    /// A patch's current resolution.
    pub fn resolution(&self, doc: DocId, patch: PatchId) -> Option<&PatchResolution> {
        self.docs.get(&doc)?.resolutions.get(&patch)
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

    /// The mixer slot a cue of `doc` on `patch` starts through.
    pub fn mixer_patch(&self, doc: DocId, patch: PatchId) -> Option<u16> {
        self.docs
            .get(&doc)?
            .slots
            .get(&patch)
            .map(|&(slot, _)| slot)
    }

    /// Output count of a workspace patch.
    pub fn outputs(&self, doc: DocId, patch: PatchId) -> usize {
        self.docs
            .get(&doc)
            .and_then(|d| d.outputs.get(&patch))
            .copied()
            .unwrap_or(0)
    }

    /// Every patch's current resolution in `doc` (for `Input::Availability`).
    pub fn resolutions(&self, doc: DocId) -> Vec<(PatchId, PatchResolution)> {
        self.docs.get(&doc).map_or_else(Vec::new, |d| {
            d.resolutions.iter().map(|(p, r)| (*p, r.clone())).collect()
        })
    }

    /// Device channel of each output of a mapped patch.
    pub fn channels(&self, doc: DocId, patch: PatchId) -> Option<&[u32]> {
        match self.docs.get(&doc)?.resolutions.get(&patch)? {
            PatchResolution::Mapped { channels, .. } => Some(channels),
            _ => None,
        }
    }

    /// The doc and patch slot `slot` is given to, in use or retired (`None`: it is free).
    pub fn owner(&self, slot: u16) -> Option<(DocId, PatchId)> {
        self.docs.iter().find_map(|(&doc, d)| {
            d.slots
                .iter()
                .find(|(_, (s, _))| *s == slot)
                .map(|(&patch, _)| patch)
                .or_else(|| d.retired.get(&slot).map(|h| h.patch))
                .map(|patch| (doc, patch))
        })
    }

    /// The mixer reset slot `slot` to silence after a fault: what brings it back to the gains
    /// last sent (`docs/control.md` "Mixer faults").
    pub fn resend(&self, slot: u16) -> Vec<CommandKind> {
        let mut commands = Vec::new();
        self.resend_into(slot, &mut commands);
        commands
    }

    fn resend_into(&self, slot: u16, commands: &mut Vec<CommandKind>) {
        let n = self.device_outputs;
        let Some(sent) = self.sent.get(usize::from(slot)) else {
            return;
        };
        for (i, &gain) in sent.iter().enumerate() {
            if gain == 0.0 {
                continue;
            }
            let (Ok(cue_output), Ok(device_output)) = (u16::try_from(i / n), u16::try_from(i % n))
            else {
                continue;
            };
            commands.push(CommandKind::SetPatchRouting {
                patch: slot,
                cue_output,
                device_output,
                gain,
            });
        }
    }

    /// The mixer disabled slot `slot` after repeated faults (it is silent and ignores routing):
    /// it is never handed out again until the mixer is rebuilt. The patch that had it gets
    /// another at the doc's next sync (or none is free: it cannot play, as when the slots run
    /// out).
    pub fn quarantine(&mut self, slot: u16) {
        for d in self.docs.values_mut() {
            d.slots.retain(|_, &mut (s, _)| s != slot);
            d.retired.remove(&slot);
        }
        self.free.retain(|&s| s != slot);
        let i = usize::from(slot);
        if let (Some(sent), Some(base)) = (self.sent.get_mut(i), self.bases.get_mut(i)) {
            sent.fill(0.0);
            base.fill(0.0);
            self.quarantined.insert(slot);
        }
    }

    /// The mixer rebuilt its non-voice state after repeated faults (every slot silent and
    /// enabled, every device output silent with no delay, the master silent): the commands
    /// that bring all of it back to what was last sent. Quarantined slots are free again.
    pub fn rebuilt(&mut self) -> Vec<CommandKind> {
        let back = std::mem::take(&mut self.quarantined);
        if !back.is_empty() {
            self.free.extend(back);
            self.freed = true;
        }
        self.resend_all()
    }

    /// Every slot's gains, every device output's settings and the master, as last sent. Sent
    /// again to a mixer that did not reset them changes nothing.
    pub fn resend_all(&self) -> Vec<CommandKind> {
        let mut commands = Vec::new();
        for slot in (0..self.sent.len()).filter_map(|s| u16::try_from(s).ok()) {
            self.resend_into(slot, &mut commands);
        }
        commands.extend(
            (0..self.delays.len())
                .filter_map(|o| u16::try_from(o).ok())
                .filter_map(|o| self.resend_output(o)),
        );
        // The mixer's master stays at unity (the workspace master is in the matrices).
        commands.push(CommandKind::SetMaster { gain: 1.0 });
        commands
    }

    /// The mixer reset device output `output` (silent, no delay) after a fault: its settings
    /// as last sent.
    pub fn resend_output(&self, output: u16) -> Option<CommandKind> {
        let &delay_frames = self.delays.get(usize::from(output))?;
        Some(CommandKind::SetOutput {
            output,
            settings: OutputSettings {
                delay_frames,
                ..OutputSettings::default()
            },
        })
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
        let mut p = Patches::new(1, 2, rate());
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
        let mut p = Patches::new(1, 1, rate());
        let (_, changed) = p.sync(D0, &ws, &settings(&ws, vec![0]), &device(1), &idle());
        assert_eq!(p.mixer_patch(D0, PatchId(Uuid::from_u128(8))), None);
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
        let (cmds, _) = p.sync(D0, &ws, &settings(&ws, vec![3, 1]), &device(4), &idle());
        // 2.5 ms at 8 kHz = 20 frames; 1 s = 8000 frames. Outputs left at 0 are not sent.
        assert_eq!(delays(&cmds), vec![(1, 8000), (3, 20)]);
        assert_eq!(p.delay_conflicts(D0).count(), 0);
        // Unchanged: nothing sent. The device gone: the delays go back to 0.
        let (again, _) = p.sync(D0, &ws, &settings(&ws, vec![3, 1]), &device(4), &idle());
        assert!(delays(&again).is_empty());
        let (gone, _) = p.sync(D0, &ws, &settings(&ws, vec![3, 1]), &[], &idle());
        assert_eq!(delays(&gone), vec![(1, 0), (3, 0)]);
    }

    #[test]
    fn a_faulted_slot_gets_its_gains_again_and_a_disabled_one_is_never_reused_until_a_rebuild() {
        let mut ws = ws_with(vec![out(0.0, false, false); 2]);
        ws.patches[0].outputs[0].delay_ms = 2.5;
        let mut p = Patches::new(2, 4, rate());
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
        assert_eq!(
            p.resend_output(3).map(|c| delays(&[c])),
            Some(vec![(3, 20)])
        );

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

        // A rebuild frees it and sends every slot, output and the master again.
        let all = p.rebuilt();
        assert_eq!(p.free_slots(), 2);
        assert!(p.take_freed());
        assert!(
            all.iter()
                .any(|c| matches!(c, CommandKind::SetMaster { gain } if *gain == 1.0))
        );
        assert_eq!(
            delays(&all),
            vec![(0, 0), (1, 0), (2, 0), (3, 0)],
            "every device output is sent, at the delay it has now"
        );
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
        let (cmds, _) = p.sync(D0, &ws, &s, &device(2), &idle());
        assert_eq!(delays(&cmds), vec![(0, 24)]);
        assert_eq!(p.delay_conflicts(D0).collect::<Vec<_>>(), vec![0]);
    }

    #[test]
    fn slots_follow_patch_ids_not_list_positions() {
        let mut p = Patches::new(4, 4, rate());
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
        let mut p = Patches::new(2, 2, rate());
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
        let mut p = Patches::new(2, 2, rate());
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
        let mut p = Patches::new(3, 2, rate());
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
        let mut p = Patches::new(3, 4, rate());
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
        let mut p = Patches::new(3, 4, rate());
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
        let mut p = Patches::new(1, 1, rate());
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
        let mut none = Patches::new(0, 1, rate());
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
        let mut p = Patches::new(1, 2, rate());
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
        let mut p = Patches::new(2, 2, rate());
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

    #[test]
    fn docs_that_disagree_on_an_output_delay_use_the_longest_and_each_is_told() {
        let mut a = ws_with(vec![out(0.0, false, false)]);
        a.patches[0].outputs[0].delay_ms = 1.0;
        let mut b = a.clone();
        b.patches[0].outputs[0].delay_ms = 3.0;
        let mut c = a.clone();
        c.patches[0].outputs[0].delay_ms = 1.0;
        let mut p = Patches::new(3, 2, rate());
        let (cmds, _) = p.sync(D0, &a, &settings(&a, vec![0]), &device(2), &idle());
        assert_eq!(delays(&cmds), vec![(0, 8)]);
        assert!(!p.take_conflicts_changed());
        let (cmds, _) = p.sync(D1, &b, &settings(&b, vec![0]), &device(2), &idle());
        assert_eq!(delays(&cmds), vec![(0, 24)]);
        assert!(p.take_conflicts_changed());
        assert_eq!(p.delay_conflicts(D0).collect::<Vec<_>>(), vec![0]);
        assert_eq!(p.delay_conflicts(D1).collect::<Vec<_>>(), vec![0]);
        // A doc that maps to the other output only is not part of it.
        p.sync(D2, &c, &settings(&c, vec![1]), &device(2), &idle());
        assert_eq!(p.delay_conflicts(D2).count(), 0);
        // The doc that wanted the longer delay closes: back to 1 ms, no conflict.
        let cmds = p.release_doc(D1);
        assert_eq!(delays(&cmds), vec![(0, 8)]);
        assert!(p.take_conflicts_changed());
        assert_eq!(p.delay_conflicts(D0).count(), 0);
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
