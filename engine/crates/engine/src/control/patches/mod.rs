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
        let mut d = self.docs.remove(&doc).unwrap_or_default();
        let mut commands = Vec::new();
        let mut changed = Vec::new();
        let mut given = self.allocate(&mut d, ws, busy, &mut commands);
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
        // Slots just handed out (every slot of a doc's first sync, or one freed by another
        // patch), which nothing plays through yet, take their gains at once instead of ramping
        // from what they held before: a voice started or moved onto one on this sample is heard
        // on its own patch's outputs only (M4.15e review L1).
        given.sort_unstable();
        commands.extend(
            given
                .into_iter()
                .map(|patch| CommandKind::SnapPatch { patch }),
        );
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
    /// others. Returns the slots just handed out (from the free list): no voice plays through
    /// them yet.
    fn allocate(
        &mut self,
        d: &mut DocSlots,
        ws: &Workspace,
        busy: &BTreeSet<u16>,
        commands: &mut Vec<CommandKind>,
    ) -> Vec<u16> {
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
        let mut given = Vec::new();
        for patch in &ws.patches {
            if d.slots.contains_key(&patch.id) {
                continue;
            }
            match self.free.pop_front() {
                Some(slot) => {
                    d.slots.insert(patch.id, (slot, patch.outputs.len()));
                    given.push(slot);
                }
                None => d.exhausted.push(patch.id),
            }
        }
        // Free slots are silent (the mixer starts them as identity; a freed one is zeroed).
        let free: Vec<u16> = self.free.iter().copied().collect();
        for slot in free {
            self.diff(slot, &zero, 1.0, commands);
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

    /// Whether slot `slot` is quarantined ([`Self::quarantine`]).
    pub fn is_quarantined(&self, slot: u16) -> bool {
        self.quarantined.contains(&slot)
    }

    /// The mixer rebuilt its non-voice state after repeated faults (every slot silent and
    /// enabled, every device output and the master at unity, each output keeping its delay):
    /// the commands that bring all of it back to what was last sent. Quarantined slots are free again.
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

    /// The mixer reset device output `output` (to unity, keeping its delay) after a fault: its
    /// settings as last sent.
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
mod tests;
