// SPDX-License-Identifier: GPL-3.0-or-later

//! Default patch mapping (M4.4c, WS-07; docs/control.md "Default patch mapping"): a workspace
//! with no mapping at all on this machine (a new one, or a show from another computer) plays
//! on the device the mixer uses, each patch on the device's first outputs, as QLab does. An
//! operator's mapping is never touched.

use openlab_core::protocol::{DeviceEntry, ErrorBody, ErrorCode, LogLevel, RequestId};
use openlab_core::{
    CueId, CueKind, CueState, DeviceInfo, Input, PatchId, PatchMapping, SampleTime,
};

use super::inbox::reply;
use super::{Control, DocCx, DocId, Engine, Flow, ReplyTx, failed, ok};

impl Control {
    /// `set_patch_mapping` (M4.4g, WS-07): sets or clears where doc `doc`'s `patch` plays on
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
fn mapping_refusal(
    mapping: &PatchMapping,
    outputs: usize,
    devices: &[DeviceEntry],
    playable: &[DeviceInfo],
) -> Option<ErrorBody> {
    let Some(device) = devices.iter().find(|d| d.name == mapping.device) else {
        return Some(ErrorBody::new(
            ErrorCode::NotFound,
            format!("no output device named \"{}\"", mapping.device),
        ));
    };
    // The engine plays on one device: a patch mapped to another listed one would be silent
    // (`device_missing`) with nothing to say why. With no device playing (an outage) nothing
    // can be compared; the mapping waits for the device to come back.
    if let Some(playing) = playable.first()
        && !playable.iter().any(|p| p.name == mapping.device)
    {
        return Some(ErrorBody::new(
            ErrorCode::BadRequest,
            format!(
                "the engine plays on \"{}\", not \"{}\": a patch can only be mapped to the \
                 device in use",
                playing.name, mapping.device
            ),
        ));
    }
    if mapping.channels.len() != outputs {
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

impl DocCx<'_> {
    /// The operator cleared `patch`'s device: every Audio cue playing through it (running or
    /// fading) pauses, as QLab 5.5.3 does; mapping a device again does not resume them. A device
    /// that disappears by itself is device loss, not this (docs/control.md, Devices).
    fn pause_cues_on(&mut self, patch: PatchId, now: SampleTime) {
        let engine = self.doc.model.engine();
        let playing: Vec<CueId> = self
            .doc
            .model
            .ws()
            .cues()
            .filter(|c| matches!(&c.kind, CueKind::Audio { patch: p, .. } if *p == patch))
            .map(|c| c.id)
            .filter(|id| matches!(engine.state(*id), CueState::Running | CueState::Fading))
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
                if let Some(refusal) =
                    mapping_refusal(&mapping, outputs, devices, &self.eng.playable)
                {
                    return Err(refusal);
                }
                let text = format!(
                    "patch '{name}' mapped to {} {}",
                    mapping.device,
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
    /// first outputs of the playable device. Nothing happens while no device is playable (an
    /// outage, or none): the patch stays unmapped and the next sync with a device maps it. A
    /// device with fewer outputs than the patch needs leaves it unmapped (one warning per
    /// patch and device); a different device is tried again.
    pub(super) fn map_unmapped_patches(&mut self) {
        let Some(device) = self.eng.playable.first().cloned() else {
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
            let wanted = patch.outputs.len();
            if wanted > have {
                if self
                    .eng
                    .mapping_warned
                    .insert((doc, ws_id, patch.id, device.name.clone()))
                {
                    notes.push((
                        LogLevel::Warn,
                        format!(
                            "device {} has {have} outputs, patch '{}' needs {wanted}: left \
                             unmapped",
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

impl Engine {
    /// Refuses an operator's change to the machine settings that could not be kept: they would
    /// work until the app quits and then be gone, and the reply would have said all was well.
    /// With no settings folder configured (tests, the offline driver) there is nothing to keep.
    fn require_saveable(&self) -> Result<(), ErrorBody> {
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
    fn save_machine(&mut self) {
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
