// SPDX-License-Identifier: GPL-3.0-or-later

//! Default patch mapping (M4.4c, WS-07; docs/control.md "Default patch mapping"): a workspace
//! with no mapping at all on this machine (a new one, or a show from another computer) plays
//! on the device the mixer uses, each patch on the device's first outputs, as QLab does. An
//! operator's mapping is never touched.

use openlab_core::PatchMapping;
use openlab_core::protocol::LogLevel;

use super::Control;

impl Control {
    /// Maps each patch of the open workspace that has no mapping entry on this machine to the
    /// first outputs of the playable device. Nothing happens while no device is playable (an
    /// outage, or none): the patch stays unmapped and the next sync with a device maps it. A
    /// device with fewer outputs than the patch needs leaves it unmapped (one warning per
    /// patch and device); a different device is tried again.
    pub(super) fn map_unmapped_patches(&mut self) {
        let Some(device) = self.playable.first().cloned() else {
            return;
        };
        let have = usize::try_from(device.output_channels).unwrap_or(usize::MAX);
        let mut notes = Vec::new();
        let mut changed = false;
        for patch in &self.ws.patches {
            if self
                .config
                .machine
                .patch_mapping(self.ws.id, patch.id)
                .is_some()
            {
                continue;
            }
            let wanted = patch.outputs.len();
            if wanted > have {
                if self.mapping_warned.insert((patch.id, device.name.clone())) {
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
                .config
                .machine
                .set_patch_mapping(self.ws.id, patch.id, mapping)
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
                    if self.mapping_warned.insert((patch.id, device.name.clone())) {
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
        if changed && let Some(writer) = &self.settings_writer {
            writer.save(self.config.machine.clone());
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
