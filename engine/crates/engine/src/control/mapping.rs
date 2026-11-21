// SPDX-License-Identifier: GPL-3.0-or-later

//! Default patch mapping (WS-07): a workspace
//! with no mapping at all on this machine (a new one, or a show from another computer) plays
//! on the device the mixer uses, each patch on the device's first outputs, as QLab does. An
//! operator's mapping is never touched.

use openlab_core::protocol::{DeviceEntry, ErrorBody, ErrorCode, LogLevel, RequestId};
use openlab_core::{CueId, CueState, Input, PatchId, PatchMapping, SampleTime};

use super::inbox::reply;
use super::{Backend, Control, DocCx, DocId, Engine, Flow, ReplyTx, failed, ok};

impl Control {
    /// `set_patch_mapping` (WS-07): sets or clears where doc `doc`'s `patch` plays on
    /// this machine. The machine settings are engine-wide, so the entry is keyed by the doc's
    /// workspace id and the patch id, as the default mapping keys it (a second open doc of the
    /// same workspace id shares it). Validated first, nothing changes on a refusal; then stored
    /// (written like the default mapping) and every doc's patches resolve again, which reaches
    /// running cues as a device change does: the mixer slot follows in place and the cue
    /// engine gets the new availability.
    pub(super) fn set_patch_mapping(
        &mut self,
        doc: DocId,
        id: RequestId,
        patch: PatchId,
        mapping: Option<PatchMapping>,
        to: &ReplyTx,
    ) -> Flow {
        let devices = self.eng.device_list();
        // Cleared: the cues playing through the patch, in every doc that shares its entry.
        let cleared = mapping.is_none();
        let mut workspace = None;
        let outcome = match self.cx_of(doc) {
            Some(mut cx) => {
                workspace = Some(cx.doc.model.ws().id);
                cx.apply_patch_mapping(patch, mapping, &devices)
            }
            None => Err(ErrorBody::new(ErrorCode::NoWorkspace, "no such workspace")),
        };
        match outcome {
            Err(error) => reply(to, failed(id, error)),
            Ok(()) => {
                self.each_doc(|cx| {
                    let now = cx.now();
                    if cleared && Some(cx.doc.model.ws().id) == workspace {
                        cx.pause_cues_on(patch, now);
                    }
                    for input in cx.sync_patches(false) {
                        cx.feed(input, now);
                    }
                });
                reply(to, ok(id, None));
            }
        }
        Flow::Continue
    }
}

/// Why `mapping` cannot be used for a patch with `outputs` outputs on the listed `devices`.
/// Any listed device will do: one that is not open yet opens for it.
fn mapping_refusal(
    mapping: &PatchMapping,
    outputs: usize,
    devices: &[DeviceEntry],
) -> Option<ErrorBody> {
    let device = if mapping.system {
        // The system output is no device of its own: its channels are checked against the
        // device it is now.
        if !mapping.device.is_empty() {
            return Some(ErrorBody::new(
                ErrorCode::BadRequest,
                "a mapping on the system output names no device",
            ));
        }
        devices.iter().find(|d| d.is_default)
    } else {
        devices.iter().find(|d| d.name == mapping.device)
    };
    let Some(device) = device else {
        return Some(ErrorBody::new(
            ErrorCode::NotFound,
            if mapping.system {
                "the system has no default output device".to_owned()
            } else {
                format!("no output device named \"{}\"", mapping.device)
            },
        ));
    };
    // Its first outputs; the rest have no device output of their own (a device smaller than
    // the patch).
    if mapping.channels.is_empty() || mapping.channels.len() > outputs {
        return Some(ErrorBody::new(
            ErrorCode::BadRequest,
            format!(
                "the patch has {outputs} outputs; the mapping lists {} channels",
                mapping.channels.len()
            ),
        ));
    }
    if let Some(bad) = mapping
        .channels
        .iter()
        .find(|&&c| c >= device.output_channels)
    {
        return Some(ErrorBody::new(
            ErrorCode::BadRequest,
            format!(
                "channel {bad} is out of range: \"{}\" has {} outputs",
                device.name, device.output_channels
            ),
        ));
    }
    None
}

/// How the log names where `mapping` plays.
fn mapping_device_text(mapping: &PatchMapping) -> String {
    if mapping.system {
        "the system output".to_owned()
    } else {
        mapping.device.clone()
    }
}

impl DocCx<'_> {
    /// The operator cleared `patch`'s device: every cue playing through it (running or fading)
    /// pauses, as QLab 5.5.3 does; mapping a device again does not resume them. A Video cue
    /// plays through its patch only when its movie has sound: a still or a silent movie that
    /// names the patch plays on. A device that disappears by itself is device loss, not this.
    fn pause_cues_on(&mut self, patch: PatchId, now: SampleTime) {
        let engine = self.doc.model.engine();
        let playing: Vec<CueId> = self
            .doc
            .model
            .ws()
            .cues()
            .filter(|c| c.kind.sound_patch() == Some(patch))
            .map(|c| c.id)
            .filter(|id| matches!(engine.state(*id), CueState::Running | CueState::Fading))
            .filter(|id| self.cue_patch(*id) == Some(patch))
            .collect();
        for cue in playing {
            self.feed(Input::Pause { cue }, now);
        }
    }

    /// Validates and stores the request's mapping (`None` clears it: an explicit "unmapped"
    /// entry, so the default mapping leaves the patch alone) and saves the machine settings.
    fn apply_patch_mapping(
        &mut self,
        patch: PatchId,
        mapping: Option<PatchMapping>,
        devices: &[DeviceEntry],
    ) -> Result<(), ErrorBody> {
        let Some((name, outputs)) = self
            .doc
            .model
            .ws()
            .patches
            .iter()
            .find(|p| p.id == patch)
            .map(|p| (p.name.clone(), p.outputs.len()))
        else {
            return Err(ErrorBody::new(ErrorCode::NotFound, "no such patch"));
        };
        let workspace = self.doc.model.ws().id;
        self.eng.require_saveable()?;
        let message = match mapping {
            Some(mapping) => {
                if let Some(refusal) = mapping_refusal(&mapping, outputs, devices) {
                    return Err(refusal);
                }
                let text = format!(
                    "patch '{name}' mapped to {} {}",
                    mapping_device_text(&mapping),
                    channels_text(&mapping.channels)
                );
                self.eng
                    .config
                    .machine
                    .set_patch_mapping(workspace, patch, mapping)
                    .map_err(|why| ErrorBody::new(ErrorCode::BadRequest, why))?;
                text
            }
            None => {
                self.eng
                    .config
                    .machine
                    .clear_patch_mapping(workspace, patch);
                format!("patch '{name}' left unmapped on this machine")
            }
        };
        // Whatever the default mapping warned about this patch is settled by the operator now.
        let doc = self.doc.id;
        self.eng
            .mapping_warned
            .retain(|(d, _, p, _)| *d != doc || *p != patch);
        self.eng.save_machine();
        self.log(LogLevel::Info, message);
        Ok(())
    }
}

impl DocCx<'_> {
    /// Maps each patch of the open workspace that has no mapping entry on this machine to the
    /// first outputs of the primary device: cue output n to device output n, as QLab 5 does.
    /// Cue outputs past the device's outputs get none (their crosspoints may still route them).
    /// Nothing happens while it does not play (an outage, or none): the patch stays unmapped and
    /// the next sync with it maps it. A device with no outputs leaves it unmapped (one warning
    /// per patch and device); a different device is tried again.
    pub(super) fn map_unmapped_patches(&mut self) {
        let Some(device) = self
            .eng
            .playable
            .iter()
            .find(|(_, key)| *key == 0)
            .map(|(d, _)| d.clone())
        else {
            return;
        };
        // This doc's warnings for patches that are gone (or of a workspace it no longer has)
        // are forgotten; other docs' stay (a closed doc's go when it closes).
        let (doc, ws_id, patches) = (
            self.doc.id,
            self.doc.model.ws().id,
            &self.doc.model.ws().patches,
        );
        self.eng.mapping_warned.retain(|(d, w, p, _)| {
            *d != doc || (*w == ws_id && patches.iter().any(|x| x.id == *p))
        });
        let have = usize::try_from(device.output_channels).unwrap_or(usize::MAX);
        let mut notes = Vec::new();
        let mut changed = false;
        for patch in &self.doc.model.ws().patches {
            if self
                .eng
                .config
                .machine
                .patch_entry(self.doc.model.ws().id, patch.id)
                .is_some()
            {
                continue;
            }
            let wanted = patch.outputs.len().min(have);
            if wanted == 0 {
                if self
                    .eng
                    .mapping_warned
                    .insert((doc, ws_id, patch.id, device.name.clone()))
                {
                    notes.push((
                        LogLevel::Warn,
                        format!(
                            "device {} has no outputs: patch '{}' left unmapped",
                            device.name, patch.name
                        ),
                    ));
                }
                continue;
            }
            // Device channels are 0-based; `wanted` is at most MAX_CHANNELS, so no wrap.
            let channels: Vec<u32> = (0..wanted).filter_map(|c| u32::try_from(c).ok()).collect();
            let mapping = PatchMapping {
                device: device.name.clone(),
                channels,
                system: false,
            };
            match self.eng.config.machine.set_patch_mapping(
                self.doc.model.ws().id,
                patch.id,
                mapping,
            ) {
                Ok(()) => {
                    changed = true;
                    notes.push((
                        LogLevel::Info,
                        format!(
                            "mapped patch '{}' to {} {}",
                            patch.name,
                            device.name,
                            outputs_text(wanted)
                        ),
                    ));
                }
                Err(why) => {
                    if self
                        .eng
                        .mapping_warned
                        .insert((doc, ws_id, patch.id, device.name.clone()))
                    {
                        notes.push((
                            LogLevel::Warn,
                            format!("patch '{}' could not be mapped: {why}", patch.name),
                        ));
                    }
                }
            }
        }
        for (level, message) in notes {
            self.log(level, message);
        }
        if changed {
            self.eng.save_machine();
        }
    }
}

impl DocCx<'_> {
    /// Keeps this machine's mapping of each patch as long as the patch: after a cue-output count
    /// edit, its undo or redo (or a show whose count differs from the stored mapping), outputs the
    /// patch keeps keep their device channels, a new one goes to the channel of its own number
    /// when openLab can use that output of the device (else it, and those after it, get none, as
    /// QLab 5 routes cue output n only to device output n), and removed ones drop. So one edit
    /// never leaves the patch `output_count_mismatch` (its cues broken). An explicitly cleared
    /// entry stays cleared.
    pub(super) fn fit_mappings_to_patches(&mut self) {
        let ws = self.doc.model.ws();
        let mut fitted: Vec<(PatchId, String, PatchMapping)> = Vec::new();
        let mut devices = None;
        let system = self.eng.system_output();
        for patch in &ws.patches {
            let Some(old) = self.eng.config.machine.patch_entry(ws.id, patch.id) else {
                continue;
            };
            let wanted = patch.outputs.len();
            if old.is_none() || old.channels.len() == wanted {
                continue;
            }
            if old.channels.len() > wanted {
                // Shrunk: the outputs it lost drop.
                let mut fit = old.clone();
                fit.channels.truncate(wanted);
                fitted.push((patch.id, patch.name.clone(), fit));
                continue;
            }
            // Listed only when an entry needs it: the list is built on each call.
            let list = devices.get_or_insert_with(|| self.eng.device_list());
            let on = old.device_for(system.as_deref());
            let usable = list
                .iter()
                .find(|d| Some(d.name.as_str()) == on)
                .map(|d| usize::try_from(d.output_channels).unwrap_or(usize::MAX))
                // Every device opens with as many outputs as it has, up to 64; the offline
                // driver's one mixer has its own count.
                .map(|have| match self.eng.outputs.primary().backend {
                    Backend::Device(_) => have.min(openlab_audio::MAX_OUTPUT_CHANNELS),
                    Backend::Offline => {
                        have.min(self.eng.outputs.primary().mixer.device_outputs().max(1))
                    }
                })
                // Not connected now: round the outputs the mapping already reaches.
                .unwrap_or_else(|| {
                    old.channels
                        .iter()
                        .max()
                        .map_or(1, |&c| usize::try_from(c).unwrap_or(0).saturating_add(1))
                })
                .max(1);
            let mut channels = old.channels.clone();
            channels.extend(
                (old.channels.len()..wanted.min(usable)).filter_map(|i| u32::try_from(i).ok()),
            );
            if channels == old.channels {
                continue;
            }
            fitted.push((
                patch.id,
                patch.name.clone(),
                PatchMapping {
                    device: old.device.clone(),
                    channels,
                    system: old.system,
                },
            ));
        }
        if fitted.is_empty() {
            return;
        }
        let ws_id = ws.id;
        let mut notes = Vec::new();
        for (patch, name, mapping) in fitted {
            let text = format!(
                "patch '{name}' now has {} cue outputs: mapped to {} {}",
                mapping.channels.len(),
                mapping_device_text(&mapping),
                channels_text(&mapping.channels)
            );
            match self
                .eng
                .config
                .machine
                .set_patch_mapping(ws_id, patch, mapping)
            {
                Ok(()) => notes.push((LogLevel::Info, text)),
                Err(why) => notes.push((
                    LogLevel::Warn,
                    format!("patch '{name}' could not be mapped to its new size: {why}"),
                )),
            }
        }
        for (level, message) in notes {
            self.log(level, message);
        }
        self.eng.save_machine();
    }
}

impl Engine {
    /// A new workspace's output patches (QLab 5 manual p. 106): one for each output device
    /// connected now. Its own first patch is the primary device's (the default mapping puts it
    /// there when the doc opens); each other listed device with outputs gets a patch like it,
    /// named "Patch 2", "Patch 3"... in list order, mapped on this machine to that device's first
    /// outputs. At most [`MAX_OUTPUTS`] in all: the engine plays on no more devices at once. With
    /// no device playing the workspace keeps its one patch.
    ///
    /// [`MAX_OUTPUTS`]: super::outputs::MAX_OUTPUTS
    pub(super) fn add_device_patches(&mut self, ws: &mut openlab_core::Workspace) {
        let Some((primary, _)) = self.playable.iter().find(|(_, key)| *key == 0) else {
            return;
        };
        let primary = primary.name.clone();
        let Some(first) = ws.patches.first().cloned() else {
            return;
        };
        let mut named = vec![primary];
        let mut notes = Vec::new();
        for device in self.device_list() {
            if ws.patches.len() >= super::outputs::MAX_OUTPUTS {
                break;
            }
            if device.output_channels == 0 || named.contains(&device.name) {
                continue;
            }
            named.push(device.name.clone());
            let have = usize::try_from(device.output_channels).unwrap_or(usize::MAX);
            let wanted = first.outputs.len().min(have);
            let mut patch = first.clone();
            patch.id = PatchId(super::docs::random_id());
            patch.name = format!("Patch {}", ws.patches.len() + 1);
            let mapping = PatchMapping {
                device: device.name.clone(),
                channels: (0..wanted).filter_map(|c| u32::try_from(c).ok()).collect(),
                system: false,
            };
            // A listed device with outputs is always a valid mapping; were one refused, that
            // device would get no patch rather than a second one on the primary.
            if self
                .config
                .machine
                .set_patch_mapping(ws.id, patch.id, mapping)
                .is_err()
            {
                continue;
            }
            notes.push(format!(
                "mapped patch '{}' to {} {}",
                patch.name,
                device.name,
                outputs_text(wanted)
            ));
            ws.patches.push(patch);
        }
        if notes.is_empty() {
            return;
        }
        self.save_machine();
        for message in notes {
            self.log(LogLevel::Info, message);
        }
    }

    /// `set_output_device`: the device the engine opens at its next start. Validated against the
    /// list, stored like a mapping; the stream in use is not touched (the engine plays on one
    /// device for its whole run).
    pub(super) fn set_output_device(&mut self, device: Option<String>) -> Result<(), ErrorBody> {
        if let Some(name) = &device
            && !self.device_list().iter().any(|d| &d.name == name)
        {
            return Err(ErrorBody::new(
                ErrorCode::NotFound,
                format!("no output device named \"{name}\""),
            ));
        }
        self.require_saveable()?;
        let message = match &device {
            Some(name) => {
                format!("output device \"{name}\" will be used from the next engine start")
            }
            None => "the system default output device will be used from the next engine start"
                .to_owned(),
        };
        self.config
            .machine
            .set_output_device(device)
            .map_err(|why| ErrorBody::new(ErrorCode::BadRequest, why))?;
        self.save_machine();
        self.log(LogLevel::Info, message);
        Ok(())
    }

    /// `set_backup_keep`: how many backups of each workspace later saves keep.
    pub(super) fn set_backup_keep(&mut self, count: u32) -> Result<u32, ErrorBody> {
        self.require_saveable()?;
        self.config
            .machine
            .set_backup_keep(count)
            .map_err(|why| ErrorBody::new(ErrorCode::BadRequest, why))?;
        self.save_machine();
        self.log(
            LogLevel::Info,
            format!("{count} backups of each workspace will be kept"),
        );
        Ok(count)
    }

    /// The OSC server's state, for `osc_state`.
    pub(super) fn osc_state(&self) -> Option<crate::net::OscState> {
        self.config.osc.as_ref().map(|osc| osc.state())
    }

    /// `set_osc_port`: opens the new port first and keeps the old one if that fails; the choice is
    /// kept for the next start.
    pub(super) fn set_osc_port(&mut self, port: u16) -> Result<crate::net::OscState, ErrorBody> {
        let Some(osc) = self.config.osc.clone() else {
            return Err(ErrorBody::new(ErrorCode::Unsupported, "OSC is not running"));
        };
        if port == 0 {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "an OSC port is a number from 1 to 65535",
            ));
        }
        self.require_saveable()?;
        let state = osc.set_port(port).map_err(|why| {
            self.log(LogLevel::Warn, format!("OSC port {port}: {why}"));
            ErrorBody::new(ErrorCode::Io, why)
        })?;
        self.config
            .machine
            .set_osc_port(port)
            .map_err(|why| ErrorBody::new(ErrorCode::BadRequest, why))?;
        self.save_machine();
        self.log(LogLevel::Info, format!("OSC listens on port {port}"));
        Ok(state)
    }

    /// Refuses an operator's change to the machine settings that could not be kept: they would
    /// work until the app quits and then be gone, and the reply would have said all was well.
    /// With no settings folder configured (tests, the offline driver) there is nothing to keep.
    pub(super) fn require_saveable(&self) -> Result<(), ErrorBody> {
        if self.config.app_dirs.is_none() {
            return Ok(());
        }
        let why = if !self.config.machine_writable {
            "settings.json could not be read at start and is left as it is"
        } else if self.settings_writer.is_none() {
            "the settings writer is not running"
        } else {
            return Ok(());
        };
        Err(ErrorBody::new(
            ErrorCode::Io,
            format!("this machine's settings cannot be saved ({why}): nothing was changed"),
        ))
    }

    /// Hands the machine settings to the writer, unless they are defaults standing in for a file
    /// that could not be read (the operator's mappings would be lost) or there is no writer.
    pub(super) fn save_machine(&mut self) {
        if !self.config.machine_writable {
            if !std::mem::replace(&mut self.unwritable_warned, true) {
                self.log(
                    LogLevel::Warn,
                    "settings.json could not be read at start: default mappings are kept in \
                     memory and the file is left as it is"
                        .to_owned(),
                );
            }
            return;
        }
        let Some(writer) = &self.settings_writer else {
            return;
        };
        if !writer.save(self.config.machine.clone()) {
            self.settings_writer = None;
            self.log(
                LogLevel::Warn,
                "the settings writer stopped: this machine's settings are no longer saved"
                    .to_owned(),
            );
        }
    }

    /// Reports a failed settings write (once per failure) in the show log.
    pub(super) fn settings_tick(&mut self) {
        let Some(error) = self.settings_writer.as_ref().and_then(|w| w.take_error()) else {
            return;
        };
        self.log(
            LogLevel::Warn,
            format!("could not save this machine's settings: {error}"),
        );
    }
}

/// "outputs 1–2", "output 1".
fn outputs_text(count: usize) -> String {
    if count == 1 {
        "output 1".to_owned()
    } else {
        format!("outputs 1\u{2013}{count}")
    }
}

/// "channels 1, 2" (device channels are 0-based on the wire, 1-based for people).
fn channels_text(channels: &[u32]) -> String {
    let list: Vec<String> = channels
        .iter()
        .map(|c| c.saturating_add(1).to_string())
        .collect();
    if list.len() == 1 {
        format!("channel {}", list.join(""))
    } else {
        format!("channels {}", list.join(", "))
    }
}
