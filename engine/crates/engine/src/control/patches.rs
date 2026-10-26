// SPDX-License-Identifier: GPL-3.0-or-later

//! Output patches (AUD-07) on the mixer: which mixer patch matrix each workspace patch uses, its
//! resolution against the devices present (sent to the cue engine as `PatchStatus`), and the
//! matrix gains (patch master × output level, mute, polarity) on the mapped device outputs.
//!
//! Device lists, hot-plug and the mixer reopen are M4.2b: today the device list is fixed when
//! the control loop starts. Per-output alignment delay is a device-output setting in the mixer
//! (`SetOutput`), not a matrix gain; applying a patch's delays there is also M4.2b.

use std::collections::HashMap;

use openlab_audio::{CommandKind, MAX_PATCHES};
use openlab_core::{
    DeviceInfo, MachineSettings, OutputPatch, PatchId, PatchResolution, Workspace, db_to_gain,
    resolve_patch,
};

pub struct Patches {
    /// Mixer patch index of each workspace patch (its place in `Workspace::patches`).
    index: HashMap<PatchId, u16>,
    /// Patch output count, for meters.
    outputs: HashMap<PatchId, usize>,
    resolutions: HashMap<PatchId, PatchResolution>,
    /// Gains sent to the mixer, per mixer patch, `[cue output][device output]` row-major.
    sent: Vec<Vec<f32>>,
    device_outputs: usize,
    master: Option<f32>,
}

impl Patches {
    /// `mixer_patches` matrices of `device_outputs` square, identity as the mixer starts them.
    pub fn new(mixer_patches: usize, device_outputs: usize) -> Self {
        let identity: Vec<f32> = (0..device_outputs * device_outputs)
            .map(|i| {
                if i / device_outputs == i % device_outputs {
                    1.0
                } else {
                    0.0
                }
            })
            .collect();
        Self {
            index: HashMap::new(),
            outputs: HashMap::new(),
            resolutions: HashMap::new(),
            sent: vec![identity; mixer_patches.min(MAX_PATCHES)],
            device_outputs,
            master: None,
        }
    }

    /// Follows the workspace's patches and the devices present. Returns the mixer commands that
    /// bring the matrices and the master level up to date, and the patches whose resolution
    /// changed (for `Input::PatchStatus`).
    pub fn sync(
        &mut self,
        ws: &Workspace,
        settings: &MachineSettings,
        devices: &[DeviceInfo],
    ) -> (Vec<CommandKind>, Vec<(PatchId, PatchResolution)>) {
        let mut commands = Vec::new();
        let mut changed = Vec::new();
        let master = db_to_gain(ws.settings.master_db);
        if self.master != Some(master) {
            self.master = Some(master);
            commands.push(CommandKind::SetMaster { gain: master });
        }
        self.index.clear();
        self.outputs.clear();
        let mut seen = HashMap::new();
        for (i, patch) in ws.patches.iter().enumerate() {
            let resolution = match u16::try_from(i)
                .ok()
                .filter(|&p| usize::from(p) < self.sent.len())
            {
                Some(p) => {
                    self.index.insert(patch.id, p);
                    let r = self.resolve(settings, ws, patch, devices);
                    let want = self.matrix(patch, &r);
                    self.diff(p, &want, &mut commands);
                    r
                }
                // More patches than the mixer has matrices: it cannot play them.
                None => PatchResolution::Unmapped,
            };
            self.outputs.insert(patch.id, patch.outputs.len());
            if self.resolutions.get(&patch.id) != Some(&resolution) {
                changed.push((patch.id, resolution.clone()));
            }
            seen.insert(patch.id, resolution);
        }
        // Matrices no patch uses any more go silent.
        for p in ws.patches.len()..self.sent.len() {
            if let Ok(p) = u16::try_from(p) {
                let zero = vec![0.0; self.device_outputs * self.device_outputs];
                self.diff(p, &zero, &mut commands);
            }
        }
        self.resolutions = seen;
        (commands, changed)
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
            let mut g = if out.mute {
                0.0
            } else {
                master * db_to_gain(out.level_db)
            };
            if out.polarity_inverted {
                g = -g;
            }
            if let Some(slot) = m.get_mut(o * n + d) {
                *slot = g;
            }
        }
        m
    }

    fn diff(&mut self, p: u16, want: &[f32], commands: &mut Vec<CommandKind>) {
        let n = self.device_outputs;
        let Some(sent) = self.sent.get_mut(usize::from(p)) else {
            return;
        };
        for (i, (s, &w)) in sent.iter_mut().zip(want).enumerate() {
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

    /// The mixer patch a cue on `patch` plays through.
    pub fn mixer_patch(&self, patch: PatchId) -> Option<u16> {
        self.index.get(&patch).copied()
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

#[cfg(test)]
mod tests {
    use super::*;
    use openlab_core::{
        CueList, CueListId, PatchMapping, PatchOutput, TransportHotkeys, WorkspaceId,
        WorkspaceSettings,
    };
    use uuid::Uuid;

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
        let mut p = Patches::new(2, 4);
        let (cmds, changed) = p.sync(&ws, &settings(&ws, vec![3, 0, 1]), &device(4));
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
        assert!(
            cmds.iter()
                .any(|c| matches!(c, CommandKind::SetMaster { .. }))
        );
        // Nothing changed: nothing sent, nothing reported.
        let (again, changed) = p.sync(&ws, &settings(&ws, vec![3, 0, 1]), &device(4));
        assert!(again.is_empty());
        assert!(changed.is_empty());
    }

    #[test]
    fn unmapped_and_short_devices_are_reported_and_silent() {
        let ws = ws_with(vec![out(0.0, false, false); 2]);
        let mut p = Patches::new(1, 2);
        let (cmds, changed) = p.sync(&ws, &MachineSettings::default(), &device(2));
        assert_eq!(changed, vec![(pid(), PatchResolution::Unmapped)]);
        assert!(routes(&cmds).iter().all(|r| r.3 == 0.0));
        // The device has 8 channels but the mixer only 2 outputs.
        let (_, changed) = p.sync(&ws, &settings(&ws, vec![0, 7]), &device(8));
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
        let (_, changed) = p.sync(&ws, &settings(&ws, vec![0]), &device(1));
        assert_eq!(p.mixer_patch(PatchId(Uuid::from_u128(8))), None);
        assert!(changed.contains(&(PatchId(Uuid::from_u128(8)), PatchResolution::Unmapped)));
    }
}
