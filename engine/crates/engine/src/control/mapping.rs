// SPDX-License-Identifier: GPL-3.0-or-later

//! Default patch mapping (M4.4c, WS-07; docs/control.md "Default patch mapping"): a workspace
//! with no mapping at all on this machine (a new one, or a show from another computer) plays
//! on the device the mixer uses, each patch on the device's first outputs, as QLab does. An
//! operator's mapping is never touched.

use openlab_core::PatchMapping;
use openlab_core::protocol::LogLevel;

use super::Control;

impl Control {
    /// Maps every patch of the open workspace to the first outputs of the playable device when
    /// the workspace has no patch mapping in the machine settings. Nothing happens while no
    /// device is playable (an outage, or none): the patches stay unmapped, and the next sync
    /// with a device maps them.
    pub(super) fn map_unmapped_patches(&mut self) {
        let Some(device) = self.playable.first().cloned() else {
            return;
        };
        let machine = &self.config.machine;
        if machine
            .workspaces
            .get(&self.ws.id.to_string())
            .is_some_and(|w| !w.patches.is_empty())
        {
            return;
        }
        // One attempt per workspace and device: a patch that cannot be mapped is not retried
        // (and logged again) on every edit.
        let key = (self.ws.id, device.name.clone());
        if self.mapping_tried.as_ref() == Some(&key) {
            return;
        }
        self.mapping_tried = Some(key);
        let mut notes = Vec::new();
        for patch in &self.ws.patches {
            let wanted = patch.outputs.len();
            let have = usize::try_from(device.output_channels).unwrap_or(usize::MAX);
            let used = wanted.min(have);
            // Device channels are 0-based; `used` is below MAX_CHANNELS, so this cannot wrap.
            let channels: Vec<u32> = (0..used).filter_map(|c| u32::try_from(c).ok()).collect();
            let mapping = PatchMapping {
                device: device.name.clone(),
                channels,
            };
            match self
                .config
                .machine
                .set_patch_mapping(self.ws.id, patch.id, mapping)
            {
                Ok(()) if used == wanted => notes.push((
                    LogLevel::Info,
                    format!(
                        "mapped patch '{}' to {} {}",
                        patch.name,
                        device.name,
                        outputs_text(used)
                    ),
                )),
                Ok(()) => notes.push((
                    LogLevel::Warn,
                    format!(
                        "patch '{}' needs {wanted} outputs but {} has {have}: mapped {}, the \
                         patch stays silent until it is mapped to enough outputs",
                        patch.name,
                        device.name,
                        if used == 0 {
                            "none".to_owned()
                        } else {
                            outputs_text(used)
                        }
                    ),
                )),
                Err(why) => notes.push((
                    LogLevel::Warn,
                    format!("patch '{}' could not be mapped: {why}", patch.name),
                )),
            }
        }
        let notes_made = !notes.is_empty();
        for (level, message) in notes {
            self.log(level, message);
        }
        if notes_made && let Some(writer) = &self.settings_writer {
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
