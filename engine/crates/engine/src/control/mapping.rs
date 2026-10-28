// SPDX-License-Identifier: GPL-3.0-or-later

//! Default patch mapping (M4.4c, WS-07; docs/control.md "Default patch mapping"): a workspace
//! with no mapping at all on this machine (a new one, or a show from another computer) plays
//! on the device the mixer uses, each patch on the device's first outputs, as QLab does. An
//! operator's mapping is never touched.

use openlab_core::PatchMapping;
use openlab_core::protocol::LogLevel;

use super::{DocCx, Engine};

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
        let (doc, ws_id, patches) = (self.doc.id, self.doc.ws.id, &self.doc.ws.patches);
        self.eng.mapping_warned.retain(|(d, w, p, _)| {
            *d != doc || (*w == ws_id && patches.iter().any(|x| x.id == *p))
        });
        let have = usize::try_from(device.output_channels).unwrap_or(usize::MAX);
        let mut notes = Vec::new();
        let mut changed = false;
        for patch in &self.doc.ws.patches {
            if self
                .eng
                .config
                .machine
                .patch_mapping(self.doc.ws.id, patch.id)
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
            match self
                .eng
                .config
                .machine
                .set_patch_mapping(self.doc.ws.id, patch.id, mapping)
            {
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
