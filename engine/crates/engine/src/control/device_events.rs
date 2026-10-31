// SPDX-License-Identifier: GPL-3.0-or-later

//! The audio device as the control thread follows it: its worker's answers and events, an
//! outage and its recovery in every doc, the device list, and the device warnings.

use openlab_audio::CommandKind;
use openlab_core::protocol::{DeviceEntry, LogLevel, RuntimeChange, WireWarning};
use openlab_core::{BrokenReason, CueKind, CueState, DeviceInfo, Input, PatchResolution};

use super::devices::{Device, DeviceAnswer, DeviceEvent};
use super::{Backend, Control, DocCx, Engine, notice};

impl Control {
    pub(super) fn poll_device(&mut self) {
        let events = match &mut self.eng.backend {
            Backend::Device(d) => d.poll(),
            Backend::Offline => return,
        };
        self.device_events(events);
    }

    pub(super) fn device_answer(&mut self, answer: DeviceAnswer) {
        let events = match &mut self.eng.backend {
            Backend::Device(d) => d.answer(answer),
            Backend::Offline => return,
        };
        self.device_events(events);
    }

    /// The device policy (docs/control.md "Devices"): one device for every
    /// doc, so a loss stops every doc and a return makes every doc playable again.
    fn device_events(&mut self, events: Vec<DeviceEvent>) {
        let name = self
            .eng
            .backend
            .device()
            .map(|d| d.info().name.clone())
            .unwrap_or_default();
        for event in events {
            match event {
                DeviceEvent::Lost { why } => self.device_lost(&name, &why),
                DeviceEvent::Restored => self.device_restored(&name),
                // Patches resolve against the device the mixer plays on, so a change in the
                // list changes them only through a loss or a reopen; resolving again is cheap.
                DeviceEvent::ListChanged => self.each_doc(|cx| {
                    let now = cx.now();
                    for input in cx.sync_patches(false) {
                        cx.feed(input, now);
                    }
                }),
                DeviceEvent::OpenFailed {
                    attempt,
                    error,
                    retry_in,
                } => self.eng.log_device(
                    LogLevel::Warn,
                    format!(
                        "audio device \"{name}\": reopen attempt {attempt} failed ({error}); \
                         next in {:.2} s",
                        retry_in.as_secs_f64()
                    ),
                ),
                DeviceEvent::Warn(message) => self.eng.log_device(LogLevel::Warn, message),
                DeviceEvent::Stalled { ms, log } => {
                    if let Some(earlier) = log {
                        let more = if earlier > 0 {
                            format!(" ({earlier} more stall(s) since the last notice)")
                        } else {
                            String::new()
                        };
                        self.eng.log_device(
                            LogLevel::Warn,
                            format!(
                                "audio device \"{name}\": no audio callback for {ms} ms; the \
                                 device is treated as lost if it stays silent{more}"
                            ),
                        );
                    }
                    self.mark_warnings_dirty();
                }
                DeviceEvent::StallCleared { log } => {
                    if log {
                        self.eng.log_device(
                            LogLevel::Info,
                            format!("audio device \"{name}\": audio callbacks resumed"),
                        );
                    }
                    self.mark_warnings_dirty();
                }
                DeviceEvent::Unrecoverable { why } => {
                    self.eng.log_device(
                        LogLevel::Error,
                        format!("audio device \"{name}\": {why}: restart openLab to play sound"),
                    );
                    self.mark_warnings_dirty();
                }
                DeviceEvent::Unstuck => {
                    self.eng.log_device(
                        LogLevel::Info,
                        format!("audio device \"{name}\": the device thread answers again"),
                    );
                    self.mark_warnings_dirty();
                }
                DeviceEvent::ListStuck => {
                    self.eng.log_device(
                        LogLevel::Error,
                        "reading the output device list does not finish: hot-plug is not \
                         followed until it does"
                            .to_owned(),
                    );
                    self.mark_warnings_dirty();
                }
            }
        }
    }

    /// The device went away (M4.2g, as QLab 5): every doc's cues on it stop at once
    /// ([`DocCx::device_lost`]); the loss is logged once, and each doc's clients get a
    /// `device_lost` notice with its own count.
    fn device_lost(&mut self, name: &str, why: &str) {
        self.eng.playable.clear();
        let mut failed = Vec::with_capacity(self.docs.len());
        self.each_doc(|cx| failed.push(cx.device_lost()));
        let total: usize = failed.iter().sum();
        let text = |n: usize| {
            format!(
                "audio device \"{name}\" lost ({why}): {n} cue(s) playing on it stopped; \
                 they start again from the beginning with GO once the device is back"
            )
        };
        self.eng.log_device(LogLevel::Error, text(total));
        let mut counts = failed.into_iter();
        self.each_doc(|cx| {
            let n = counts.next().unwrap_or(0);
            cx.runtime(vec![RuntimeChange::Warning {
                warning: notice("device_lost", text(n), None),
            }]);
        });
    }

    /// The stream was reopened: patches resolve against the device again (cues that failed
    /// with `device_lost`, and idle cues on it, become playable) in every doc.
    fn device_restored(&mut self, name: &str) {
        let Some(device) = self.eng.backend.device() else {
            return;
        };
        let block = device.info().block_frames;
        self.eng.playable = vec![playable_device(device)];
        // Another buffer size: the lookahead and `now` follow it, so every deadline is still
        // committed before the callback that renders it (commands already sent keep their
        // samples). Every doc's cue engine takes the new lookahead before the engine's block
        // changes (seams note (b)); if one cannot, they all keep the old one.
        if let Some(b) = block
            .map(u64::from)
            .filter(|&b| b > 0 && b != self.eng.block)
        {
            let old = self.eng.block;
            let mut failed = None;
            for doc in &mut self.docs {
                if let Err(e) = doc.engine.set_lookahead(b) {
                    failed = Some(e);
                    break;
                }
            }
            match failed {
                None => {
                    self.eng.now_floor = self.eng.now();
                    self.eng.log_device(
                        LogLevel::Info,
                        format!(
                            "audio device \"{name}\" reopened with {b}-frame buffers (was {old})"
                        ),
                    );
                    self.eng.block = b;
                }
                Some(e) => {
                    for doc in &mut self.docs {
                        // The old lookahead was accepted before: this cannot fail.
                        let _ = doc.engine.set_lookahead(old);
                    }
                    self.eng.log_device(
                        LogLevel::Warn,
                        format!("audio device \"{name}\": {e}; commands may land late"),
                    );
                }
            }
        }
        self.each_doc(|cx| {
            let now = cx.now();
            for input in cx.sync_patches(false) {
                cx.feed(input, now);
            }
        });
        self.eng
            .log_device(LogLevel::Info, format!("audio device \"{name}\" is back"));
        self.mark_warnings_dirty();
    }
}

impl DocCx<'_> {
    /// This doc's part of a device loss (M4.2g, as QLab 5; [`Control::device_lost`]): the
    /// patches on it are reported missing (loaded cues on them unload, starts are refused as
    /// broken, idle cues show broken), every run with a voice on it ends at once with
    /// `ActionFailed { device_lost }` (hard stop, no fade; the voice and its stream are
    /// released, auto-follow never fires), and every voice the doc has in the mixer is stopped,
    /// so nothing sounds, or resumes, when the device returns. Returns the runs stopped.
    fn device_lost(&mut self) -> usize {
        let now = self.now();
        for input in self.sync_patches(false) {
            self.feed(input, now);
        }
        let mut runs = self.doc.voices.on_device();
        // Audio runs with no voice yet or any more (pre-wait, post-wait, paused) are routed to
        // the device too: nothing scheduled before the outage may start by itself after it.
        // They end as device_lost breaks; `Input::DeviceLost` below stops all the others.
        for (cue, state) in self.doc.engine.cue_states() {
            if matches!(
                state,
                CueState::PreWaiting
                    | CueState::Running
                    | CueState::Paused
                    | CueState::PostWaiting
                    | CueState::Fading
                    | CueState::Stopping
            ) && let Some(run) = self.doc.engine.run_of(cue)
                && matches!(
                    self.doc.ws.find_cue(cue).map(|c| &c.kind),
                    Some(CueKind::Audio { .. })
                )
                && !runs.contains(&(cue, run))
            {
                runs.push((cue, run));
            }
        }
        runs.sort();
        let failed = runs.len();
        for (cue, run) in runs {
            self.feed(
                Input::ActionFailed {
                    cue,
                    run,
                    reason: BrokenReason::DeviceLost,
                },
                now,
            );
        }
        // Nothing else scheduled before the loss may fire after the return: every other active
        // run (memo, Wait, control, Fade, group; broken runs with a pending continue) stops.
        self.feed(Input::DeviceLost, now);
        // Orphaned tails and anything the engine no longer owns. Dated on the first sample the
        // mixer renders after the outage (`stop_floor`), so a voice plays nothing more.
        let floor = self.stop_floor();
        let commands = self
            .doc
            .voices
            .playing_ids()
            .into_iter()
            .map(|voice| {
                let at = self.doc.voices.stop_date(voice, now, floor);
                let fade = openlab_audio::Fade::NONE;
                (at, CommandKind::StopVoice { voice, fade })
            })
            .collect();
        self.send_commands(commands);
        self.doc.warnings_dirty = true;
        failed
    }

    /// The device's notices: an outage, a mixer that could not be recovered, patches that map
    /// more device channels than the device has (`outputs_dropped`), patches with no free mixer
    /// slot (`mixer_patches_exhausted`), device outputs that patches give different delays.
    pub(super) fn device_warnings(&self, list: &mut Vec<WireWarning>) {
        self.eng.stage_faults.warnings(list);
        if let Some(d) = self.eng.backend.device() {
            let name = &d.info().name;
            if let Some(why) = d.unrecoverable() {
                list.push(notice(
                    "device_unrecoverable",
                    format!(
                        "audio device \"{name}\": {why}; sound cannot come back: restart \
                         openLab"
                    ),
                    None,
                ));
            } else if d.is_down() {
                list.push(notice(
                    "device_lost",
                    format!(
                        "audio device \"{name}\" is lost: the cues that played on it were \
                         stopped, and cues on it cannot start; reconnecting"
                    ),
                    None,
                ));
            }
            if d.is_stalled() && !d.is_down() {
                list.push(notice(
                    "device_stalled",
                    format!(
                        "audio device \"{name}\" has stopped calling back: it is treated as \
                         lost if it stays silent"
                    ),
                    None,
                ));
            }
            if d.list_stuck() {
                list.push(notice(
                    "device_list_stuck",
                    "reading the output device list does not finish: devices plugged in or out \
                     are not seen"
                        .to_owned(),
                    None,
                ));
            }
            if d.info().outputs_dropped && !d.is_down() {
                list.push(notice(
                    "outputs_dropped",
                    format!(
                        "audio device \"{name}\" has {} output channel(s), fewer than the {} \
                         the show was opened with: the others are not heard",
                        d.info().channels,
                        self.eng.cue_outputs
                    ),
                    None,
                ));
            }
        }
        for patch in &self.doc.ws.patches {
            if let Some(PatchResolution::ChannelsMissing {
                device,
                needed,
                available,
            }) = self.eng.patches.resolution(self.doc.id, patch.id)
            {
                list.push(notice(
                    "outputs_dropped",
                    format!(
                        "patch \"{}\" maps {needed} channel(s) of \"{device}\", which has \
                         {available}: its cues cannot play",
                        patch.name
                    ),
                    None,
                ));
            }
        }
        for &id in self.eng.patches.exhausted(self.doc.id) {
            let name = self
                .doc
                .ws
                .patches
                .iter()
                .find(|p| p.id == id)
                .map_or("", |p| p.name.as_str());
            list.push(notice(
                "mixer_patches_exhausted",
                format!(
                    "patch \"{name}\" cannot play: all {} mixer patches are in use",
                    self.eng.patches.capacity()
                ),
                None,
            ));
        }
        for output in self.eng.patches.delay_conflicts(self.doc.id) {
            list.push(notice(
                "output_delay_conflict",
                format!(
                    "device output {} has different delays in different patches; the longest \
                     is used",
                    u32::from(output) + 1
                ),
                None,
            ));
        }
    }
}

impl Engine {
    /// The output devices for a `list_devices` request.
    pub(super) fn device_list(&self) -> Vec<DeviceEntry> {
        match self.backend.device() {
            Some(d) => d
                .listed()
                .iter()
                .map(|l| DeviceEntry {
                    name: l.name.clone(),
                    output_channels: u32::from(l.max_channels()),
                })
                .collect(),
            None => self
                .config
                .devices
                .iter()
                .map(|d| DeviceEntry {
                    name: d.name.clone(),
                    output_channels: d.output_channels,
                })
                .collect(),
        }
    }
}

/// The device patches resolve against while `d` plays: its name, with the mixer outputs that
/// reach it.
pub(super) fn playable_device(d: &Device) -> DeviceInfo {
    DeviceInfo {
        name: d.info().name.clone(),
        output_channels: u32::from(d.info().channels),
    }
}
