// SPDX-License-Identifier: GPL-3.0-or-later

//! Output patches (AUD-07) on the mixer: which mixer patch matrix (slot) each workspace patch
//! uses, its resolution against the devices present (sent to the cue engine as `PatchStatus`),
//! and the matrix gains (patch master × output level, mute, polarity) on the mapped device
//! outputs. The mixer's own master stays at unity (the mixer is shared by every open doc; a
//! workspace has no global master).
//!
//! The mixer is shared by every open doc: slots are keyed by `(DocId,
//! PatchId)`, not by a patch's place in a list, so adding, removing or moving a patch never
//! re-routes or silences a voice already playing. A slot belongs
//! to one doc's patch at a time, so only that doc's voices can play through it. A voice keeps
//! the slot it started on; a slot whose patch is gone, or whose patch changed its output count
//! while a voice plays through it, is *retired*: it keeps the routing it had until no voice
//! uses it, then it is zeroed and freed. A closed doc's slots are zeroed and freed at once
//! ([`Patches::release_doc`]): its voices were stopped first.
//!
//! Patches resolve against the devices that play (the control loop's `playable` list: a device
//! is left out while it is away). Each device has a mixer of its own; slot numbers are shared by
//! every mixer, and a slot's gains are sent to the mixer of its patch's device. A slot follows
//! its patch to another device in place while no voice plays through it; under a playing voice
//! it is retired (the voice plays on where it started) and the patch takes a fresh slot, as for
//! a resize. Output level, mute and polarity are in the matrix: the mixer has no other
//! per-output setting.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use openlab_audio::{CommandKind, MAX_PATCHES};
use openlab_core::{
    DeviceInfo, MachineSettings, OutputPatch, PatchId, PatchOutput, PatchResolution, Workspace,
    db_to_gain, resolve_patch,
};

use super::DocId;
use super::devices::OutputKey;

/// Mixer commands for patch slots, each with the output whose mixer it is for.
pub type PatchCommands = Vec<(OutputKey, CommandKind)>;

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
    /// follows the patch's settings ([`Patches::retired_base`]).
    retired: BTreeMap<u16, Holder>,
    /// Workspace patches with no slot: every slot is taken.
    exhausted: Vec<PatchId>,
    /// Patch output count, for meters.
    outputs: HashMap<PatchId, usize>,
    resolutions: HashMap<PatchId, PatchResolution>,
}

pub struct Patches {
    docs: BTreeMap<DocId, DocSlots>,
    /// Free slots, the least recently freed first (a slot is reused as late as possible).
    free: VecDeque<u16>,
    /// A slot went back to the free list since [`Self::take_freed`]: a doc waiting for one
    /// can have it.
    freed: bool,
    /// Gains sent to each output's mixer, per slot, `[cue output][device output]` row-major. A
    /// slot no gain was sent to on an output is identity there, as a mixer starts it.
    sent: HashMap<(OutputKey, u16), Vec<f32>>,
    /// The output each slot's gains are on (the device of the patch that has it).
    slot_out: Vec<OutputKey>,
    /// The same as sent, so a retired slot follows a change of its patch's levels too.
    bases: Vec<Vec<f32>>,
    device_outputs: usize,
    /// Slots a mixer disabled after repeated faults ([`Self::quarantine`]), with that mixer's
    /// output.
    quarantined: BTreeMap<u16, OutputKey>,
    /// Slots that ever existed (the mixers' patch count).
    count: usize,
}

impl Patches {
    /// `mixer_patches` matrices of `device_outputs` square, identity as the mixer starts them.
    pub fn new(mixer_patches: usize, device_outputs: usize) -> Self {
        let count = mixer_patches.min(MAX_PATCHES);
        Self {
            docs: BTreeMap::new(),
            free: (0..count).filter_map(|p| u16::try_from(p).ok()).collect(),
            freed: false,
            sent: HashMap::new(),
            slot_out: vec![0; count],
            bases: vec![vec![0.0; device_outputs * device_outputs]; count],
            device_outputs,
            quarantined: BTreeMap::new(),
            count,
        }
    }

    fn identity(&self) -> Vec<f32> {
        let n = self.device_outputs;
        (0..n * n)
            .map(|i| if i / n == i % n { 1.0 } else { 0.0 })
            .collect()
    }

    /// The output slot `slot`'s gains are on.
    pub fn slot_output(&self, slot: u16) -> Option<OutputKey> {
        self.slot_out.get(usize::from(slot)).copied()
    }

    /// Follows doc `doc`'s patches and the devices that play (`playable`: each with its
    /// output). `busy`: the slots a voice of that doc in a mixer may still play through
    /// (`Voices::slots_in_use`; no other doc's voice can be on its slots). Returns the mixer
    /// commands, each for its output, that bring the matrices up to date, and the patches whose
    /// resolution changed (for `Input::PatchStatus`).
    pub fn sync(
        &mut self,
        doc: DocId,
        ws: &Workspace,
        settings: &MachineSettings,
        playable: &[(DeviceInfo, OutputKey)],
        busy: &BTreeSet<u16>,
    ) -> (PatchCommands, Vec<(PatchId, PatchResolution)>) {
        let devices: Vec<DeviceInfo> = playable.iter().map(|(d, _)| d.clone()).collect();
        let mut d = self.docs.remove(&doc).unwrap_or_default();
        // Resolved first: a slot follows its patch's device.
        let mut resolved: HashMap<PatchId, (PatchResolution, Option<OutputKey>)> = HashMap::new();
        for patch in &ws.patches {
            let r = self.resolve(settings, ws, patch, &devices);
            let out = match &r {
                PatchResolution::Mapped { device, .. } => playable
                    .iter()
                    .find(|(i, _)| &i.name == device)
                    .map(|&(_, k)| k),
                _ => None,
            };
            resolved.insert(patch.id, (r, out));
        }
        let mut commands = Vec::new();
        let mut changed = Vec::new();
        let mut given = self.allocate(&mut d, ws, &resolved, busy, &mut commands);
        d.outputs.clear();
        let mut seen = HashMap::new();
        for patch in &ws.patches {
            let resolution = match (d.slots.get(&patch.id), resolved.remove(&patch.id)) {
                (Some(&(slot, _)), Some((r, _))) => {
                    let base = self.matrix(patch, &r);
                    self.apply(slot, base, &mut commands);
                    r
                }
                // Every mixer slot is taken: the patch cannot play.
                _ => PatchResolution::Unmapped,
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
                Some(patch) => {
                    // The patch's channels are for this slot's device only while the patch
                    // plays there.
                    let here = d
                        .slots
                        .get(&patch.id)
                        .and_then(|&(s, _)| self.slot_output(s))
                        == self.slot_output(slot);
                    self.retired_base(&d, slot, &holder, patch, here)
                }
                None => self.bases.get(usize::from(slot)).cloned(),
            };
            if let Some(base) = base {
                self.apply(slot, base, &mut commands);
            }
        }
        // Slots just handed out (every slot of a doc's first sync, or one freed by another
        // patch), which nothing plays through yet, take their gains at once instead of ramping
        // from what they held before: a voice started or moved onto one on this sample is heard
        // on its own patch's outputs only.
        given.sort_unstable();
        for patch in given {
            let out = self.slot_output(patch).unwrap_or(0);
            commands.push((out, CommandKind::SnapPatch { patch }));
        }
        self.docs.insert(doc, d);
        (commands, changed)
    }

    /// A doc closed (its voices were stopped first): every slot it had, in use or retired, is
    /// zeroed and freed. Returns the mixer commands.
    pub fn release_doc(&mut self, doc: DocId) -> Vec<(OutputKey, CommandKind)> {
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
            self.diff(slot, &zero, &mut commands);
            if let Some(b) = self.bases.get_mut(usize::from(slot)) {
                b.fill(0.0);
            }
            self.free.push_back(slot);
            self.freed = true;
        }
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
        here: bool,
    ) -> Option<Vec<f32>> {
        let n = self.device_outputs;
        let mut m = self.bases.get(usize::from(slot))?.clone();
        let channels: &[u32] = match d.resolutions.get(&patch.id) {
            Some(PatchResolution::Mapped { channels, .. }) if here => channels,
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

    /// Sends slot `slot` the gains `base`.
    fn apply(&mut self, slot: u16, base: Vec<f32>, commands: &mut Vec<(OutputKey, CommandKind)>) {
        self.diff(slot, &base, commands);
        if let Some(b) = self.bases.get_mut(usize::from(slot)) {
            *b = base;
        }
    }

    /// Gives every patch of the doc a slot, keeping the one it has; retires and frees the
    /// others. Returns the slots just handed out (from the free list): no voice plays through
    /// them yet.
    fn allocate(
        &mut self,
        d: &mut DocSlots,
        ws: &Workspace,
        resolved: &HashMap<PatchId, (PatchResolution, Option<OutputKey>)>,
        busy: &BTreeSet<u16>,
        commands: &mut Vec<(OutputKey, CommandKind)>,
    ) -> Vec<u16> {
        let live: HashMap<PatchId, usize> =
            ws.patches.iter().map(|p| (p.id, p.outputs.len())).collect();
        let target = |patch: &PatchId| resolved.get(patch).and_then(|(_, o)| *o);
        // A slot leaves its patch when the patch is gone, or resized or moved to another device
        // while a voice plays through it (the voice keeps the layout and device it started
        // with; later starts get the new ones). With no voice on the slot the change is made in
        // place: a slot moving to another device is silenced on the one it leaves.
        let mut leaving = Vec::new();
        let mut moving = Vec::new();
        for (&patch, (slot, outputs)) in &mut d.slots {
            let here = self.slot_out.get(usize::from(*slot)).copied();
            let moved = target(&patch).is_some_and(|t| Some(t) != here);
            match live.get(&patch) {
                Some(&n) if n == *outputs && !moved => {}
                Some(&n) if !busy.contains(slot) => {
                    *outputs = n;
                    if let Some(t) = target(&patch).filter(|_| moved) {
                        moving.push((*slot, t));
                    }
                }
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
        let zero = vec![0.0; self.device_outputs * self.device_outputs];
        for (slot, to) in moving {
            self.diff(slot, &zero, commands);
            if let Some(o) = self.slot_out.get_mut(usize::from(slot)) {
                *o = to;
            }
        }
        // A retired slot goes back to its patch when that has the same id, size and device again
        // (an undo) and no slot; else it is zeroed and freed once no voice uses it.
        let retired: Vec<(u16, PatchId, usize)> = d
            .retired
            .iter()
            .map(|(&s, h)| (s, h.patch, h.outputs))
            .collect();
        for (slot, patch, outputs) in retired {
            let same_device = target(&patch).is_none_or(|t| self.slot_output(slot) == Some(t));
            if live.get(&patch) == Some(&outputs) && same_device && !d.slots.contains_key(&patch) {
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
        let mut given = Vec::new();
        for patch in &ws.patches {
            if d.slots.contains_key(&patch.id) {
                continue;
            }
            match self.free.pop_front() {
                Some(slot) => {
                    // A free slot is silent on the device it was last on; it starts on its
                    // patch's device (the primary for a patch that plays nowhere).
                    if let Some(t) = target(&patch.id)
                        && let Some(o) = self.slot_out.get_mut(usize::from(slot))
                    {
                        *o = t;
                    }
                    d.slots.insert(patch.id, (slot, patch.outputs.len()));
                    given.push(slot);
                }
                None => d.exhausted.push(patch.id),
            }
        }
        // Free slots are silent (the mixer starts them as identity; a freed one is zeroed).
        let free: Vec<u16> = self.free.iter().copied().collect();
        for slot in free {
            self.diff(slot, &zero, commands);
        }
        given
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

    /// Mixer slots the patches can have.
    pub fn capacity(&self) -> usize {
        self.count
    }

    /// Slots no doc has (in use or retired).
    pub fn free_slots(&self) -> usize {
        self.free.len()
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

    /// The matrix a patch should have: cue output `o` → its mapped device channel at the patch's
    /// master × the output's level; silent when unmapped.
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

    /// Sends slot `p`, on its output, the entries of `want` that differ from what it has.
    fn diff(&mut self, p: u16, want: &[f32], commands: &mut Vec<(OutputKey, CommandKind)>) {
        let n = self.device_outputs;
        let Some(out) = self.slot_output(p) else {
            return;
        };
        let identity = self.identity();
        let sent = self.sent.entry((out, p)).or_insert(identity);
        for (i, (s, &w)) in sent.iter_mut().zip(want).enumerate() {
            if s.to_bits() == w.to_bits() {
                continue;
            }
            let (Ok(cue_output), Ok(device_output)) = (u16::try_from(i / n), u16::try_from(i % n))
            else {
                continue;
            };
            *s = w;
            commands.push((
                out,
                CommandKind::SetPatchRouting {
                    patch: p,
                    cue_output,
                    device_output,
                    gain: w,
                },
            ));
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
    /// last sent, for the output the slot is on.
    pub fn resend(&self, slot: u16) -> Vec<(OutputKey, CommandKind)> {
        let mut commands = Vec::new();
        if let Some(out) = self.slot_output(slot) {
            self.resend_into(out, slot, &mut commands);
        }
        commands
    }

    /// Slot `slot`'s gains as last sent to output `out` (a slot never sent to there is
    /// identity, as the mixer starts it).
    fn resend_into(&self, out: OutputKey, slot: u16, commands: &mut Vec<(OutputKey, CommandKind)>) {
        let n = self.device_outputs;
        let identity;
        let sent = match self.sent.get(&(out, slot)) {
            Some(s) => s,
            None => {
                identity = self.identity();
                &identity
            }
        };
        for (i, &gain) in sent.iter().enumerate() {
            if gain == 0.0 {
                continue;
            }
            let (Ok(cue_output), Ok(device_output)) = (u16::try_from(i / n), u16::try_from(i % n))
            else {
                continue;
            };
            commands.push((
                out,
                CommandKind::SetPatchRouting {
                    patch: slot,
                    cue_output,
                    device_output,
                    gain,
                },
            ));
        }
    }

    /// Output `out`'s mixer disabled slot `slot` after repeated faults (it is silent and ignores
    /// routing there): it is never handed out again until that mixer is rebuilt. The patch that had it gets
    /// another at the doc's next sync (or none is free: it cannot play, as when the slots run
    /// out).
    pub fn quarantine(&mut self, slot: u16, out: OutputKey) {
        for d in self.docs.values_mut() {
            d.slots.retain(|_, &mut (s, _)| s != slot);
            d.retired.remove(&slot);
        }
        self.free.retain(|&s| s != slot);
        let i = usize::from(slot);
        if let Some(base) = self.bases.get_mut(i) {
            base.fill(0.0);
            let n = self.device_outputs;
            self.sent.insert((out, slot), vec![0.0; n * n]);
            self.quarantined.insert(slot, out);
        }
    }

    /// Whether slot `slot` is quarantined ([`Self::quarantine`]).
    pub fn is_quarantined(&self, slot: u16) -> bool {
        self.quarantined.contains_key(&slot)
    }

    /// Output `out`'s mixer rebuilt its non-voice state after repeated faults (every slot
    /// silent and enabled, every device output enabled and the master at unity): the commands
    /// that bring all of it back to what was last sent. The slots it had quarantined are free
    /// again.
    pub fn rebuilt(&mut self, out: OutputKey) -> Vec<(OutputKey, CommandKind)> {
        let back: Vec<u16> = self
            .quarantined
            .iter()
            .filter(|&(_, &o)| o == out)
            .map(|(&s, _)| s)
            .collect();
        for slot in &back {
            self.quarantined.remove(slot);
        }
        if !back.is_empty() {
            self.free.extend(back);
            self.freed = true;
        }
        self.resend_all(out)
    }

    /// Every slot's gains on output `out` and its master, as last sent. Sent again to a mixer
    /// that did not reset them changes nothing.
    pub fn resend_all(&self, out: OutputKey) -> Vec<(OutputKey, CommandKind)> {
        let mut commands = Vec::new();
        for slot in (0..self.count).filter_map(|s| u16::try_from(s).ok()) {
            self.resend_into(out, slot, &mut commands);
        }
        // The mixer's master stays at unity.
        commands.push((out, CommandKind::SetMaster { gain: 1.0 }));
        commands
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

#[cfg(test)]
mod tests;
