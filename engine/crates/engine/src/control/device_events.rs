// SPDX-License-Identifier: GPL-3.0-or-later

//! The audio device as the control thread follows it: its worker's answers and events, an
//! outage and its recovery in every doc, the device list, and the device warnings.

use openlab_audio::{CommandKind, VoiceId};
use openlab_core::protocol::{
    DeviceEntry, ErrorBody, ErrorCode, LogLevel, RuntimeChange, WireWarning,
};
use openlab_core::{
    BrokenReason, CueId, CueKind, CueState, DeviceInfo, Input, MAX_BUFFER_FRAMES,
    MIN_BUFFER_FRAMES, PatchResolution,
};

use super::devices::{Device, DeviceAnswer, DeviceEvent, OutputKey};
use super::outputs::Output;
use super::{Backend, Control, DocCx, Engine, notice};

impl Control {
    pub(super) fn poll_device(&mut self) {
        for key in 0..self.eng.outputs.len() {
            let events = match self.eng.outputs.get_mut(key).map(|o| &mut o.backend) {
                Some(Backend::Device(d)) => d.poll(),
                _ => continue,
            };
            self.device_events(key, events);
        }
    }

    /// A device thread's answer for output `key`. The device list, read by the primary's list
    /// thread, is every device's: each follows it (hot-plug, loss by leaving the list).
    pub(super) fn device_answer(&mut self, key: OutputKey, answer: DeviceAnswer) {
        let list = match &answer {
            DeviceAnswer::Listed(list) if key == 0 => Some(list.clone()),
            _ => None,
        };
        let events = match self.eng.outputs.get_mut(key).map(|o| &mut o.backend) {
            Some(Backend::Device(d)) => d.answer(answer),
            _ => return,
        };
        self.device_events(key, events);
        let Some(list) = list else { return };
        for other in 1..self.eng.outputs.len() {
            let events = match self.eng.outputs.get_mut(other).map(|o| &mut o.backend) {
                Some(Backend::Device(d)) => d.answer(DeviceAnswer::Listed(list.clone())),
                _ => continue,
            };
            self.device_events(other, events);
        }
    }

    /// The device policy for output `key`: a loss stops the cues on that device in every doc,
    /// and a return makes them playable again; the other devices play on.
    fn device_events(&mut self, key: OutputKey, events: Vec<DeviceEvent>) {
        if events.is_empty() {
            return;
        }
        let name = self
            .eng
            .outputs
            .get(key)
            .map(|o| o.name.clone())
            .unwrap_or_default();
        for event in events {
            // The master clock follows the devices before anything is dated.
            self.eng.outputs.follow_clock();
            match event {
                DeviceEvent::Lost { why } => self.device_lost(key, &name, &why),
                DeviceEvent::Restored => {
                    self.device_restored(&name, "is back");
                    self.eng.check_buffer(key);
                }
                DeviceEvent::Opened => {
                    self.device_restored(&name, "opened");
                    self.eng.check_buffer(key);
                }
                DeviceEvent::Reconfigured => {
                    self.follow_block();
                    let frames = self.eng.outputs.get(key).and_then(Output::block);
                    self.eng.log_device(
                        LogLevel::Info,
                        match frames {
                            Some(b) => {
                                format!("audio device \"{name}\" reopened with {b}-frame buffers")
                            }
                            None => format!("audio device \"{name}\" reopened"),
                        },
                    );
                    self.eng.check_buffer(key);
                }
                // Patches resolve against the devices that play, so a change in the list changes
                // them only through a loss or a reopen; resolving again is cheap. A device a
                // patch maps to may have come.
                DeviceEvent::ListChanged => {
                    self.eng.outputs_dirty = true;
                    self.each_doc(|cx| {
                        let now = cx.now();
                        for input in cx.sync_patches(false) {
                            cx.feed(input, now);
                        }
                    });
                }
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
        self.eng.outputs.follow_clock();
    }

    /// Output `key`'s device went away (as QLab 5): every doc's cues on it stop at once
    /// ([`DocCx::device_lost`]); the loss is logged once, and each doc's clients get a
    /// `device_lost` notice with its own count. The other devices play on; only when no device
    /// plays any more is everything scheduled before the loss cancelled too.
    fn device_lost(&mut self, key: OutputKey, name: &str, why: &str) {
        self.eng.refresh_playable();
        let last = self.eng.playable.is_empty();
        let mut failed = Vec::with_capacity(self.docs.len());
        self.each_doc(|cx| failed.push(cx.device_lost(key, name, last)));
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

    /// A device opened, or its stream was reopened: patches resolve against it again (cues
    /// that failed with `device_lost`, and idle cues on it, become playable) in every doc.
    fn device_restored(&mut self, name: &str, what: &str) {
        self.eng.refresh_playable();
        self.follow_block();
        self.each_doc(|cx| {
            let now = cx.now();
            for input in cx.sync_patches(false) {
                cx.feed(input, now);
            }
        });
        self.eng
            .log_device(LogLevel::Info, format!("audio device \"{name}\" {what}"));
        self.mark_warnings_dirty();
    }

    /// Opens, as outputs of their own, the listed devices patch mappings name that are not
    /// open yet (after a mapping or list change). A device opens on its own thread without
    /// holding up the loop; its patches resolve `device_missing` until it plays. It runs at the
    /// primary's rate: a device that cannot is not opened (warned once), and its patches stay
    /// `device_missing`. Only a device-backed engine with an output mixer configured opens
    /// further devices.
    pub(super) fn open_outputs(&mut self) {
        if !std::mem::take(&mut self.eng.outputs_dirty) {
            return;
        }
        let Some(mixer_config) = self.eng.config.output_mixer else {
            return;
        };
        let Backend::Device(primary) = &self.eng.outputs.primary().backend else {
            return;
        };
        let listed = primary.listed().to_vec();
        let mut wanted: Vec<String> = Vec::new();
        for doc in &self.docs {
            let ws = doc.model.ws();
            for patch in &ws.patches {
                if let Some(m) = self.eng.config.machine.patch_entry(ws.id, patch.id)
                    && !m.device.is_empty()
                    && !wanted.contains(&m.device)
                {
                    wanted.push(m.device.clone());
                }
            }
        }
        self.park_outputs(&wanted);
        wanted.retain(|name| self.eng.outputs.by_name(name).is_none());
        for name in wanted {
            let Some(device) = listed.iter().find(|d| d.name == name) else {
                // Not here: its patches are `device_missing`; a list change tries again.
                continue;
            };
            let rate = mixer_config.sample_rate;
            if !device
                .configs
                .iter()
                .any(|c| (c.min_rate..=c.max_rate).contains(&rate))
            {
                self.output_warning(
                    &name,
                    format!(
                        "audio device \"{name}\" cannot play at {rate} Hz, the rate of the \
                         primary device: the patches mapped to it cannot play"
                    ),
                );
                continue;
            }
            if self.eng.outputs.len() >= super::outputs::MAX_OUTPUTS {
                self.output_warning(
                    &name,
                    format!(
                        "audio device \"{name}\" was not opened: {} devices are open, the most \
                         openLab plays on at once",
                        super::outputs::MAX_OUTPUTS
                    ),
                );
                continue;
            }
            let (ctl, mixer) = match openlab_audio::Mixer::new(mixer_config) {
                Ok(m) => m,
                Err(e) => {
                    self.output_warning(
                        &name,
                        format!("audio device \"{name}\" was not opened: {e}"),
                    );
                    continue;
                }
            };
            let request = openlab_audio::OpenRequest {
                device_id: Some(device.id.clone()),
                buffer_frames: Some(self.eng.config.machine.buffer_frames_for(&name)),
            };
            let key = self.eng.outputs.next_key();
            let inbox = self.eng.inbox.clone();
            let Some(spawned) = self
                .eng
                .outputs
                .primary()
                .backend
                .device()
                .and_then(|p| p.start_another(mixer, request, inbox, key))
            else {
                return;
            };
            match spawned {
                Ok(d) => {
                    let output = Output {
                        name: name.clone(),
                        mixer: ctl,
                        backend: Backend::Device(Box::new(d)),
                    };
                    if self.eng.outputs.add(output).is_some() {
                        self.eng.size_meters();
                        self.eng.log_device(
                            LogLevel::Info,
                            format!("opening audio device \"{name}\" for the patches mapped to it"),
                        );
                    }
                }
                Err(e) => self.output_warning(
                    &name,
                    format!("audio device \"{name}\" was not opened: {e}"),
                ),
            }
        }
    }

    /// `set_buffer_size` (AUD-21): device `device`'s buffer size on this machine, 16 to 1024
    /// frames. Validated first (nothing changes on a refusal), stored and saved; a device the
    /// engine plays on reopens at once at that size, the others untouched.
    pub(super) fn set_buffer_size(&mut self, device: &str, frames: u32) -> Result<(), ErrorBody> {
        if !(MIN_BUFFER_FRAMES..=MAX_BUFFER_FRAMES).contains(&frames) {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!(
                    "a buffer size of {frames} frames is outside {MIN_BUFFER_FRAMES} to \
                     {MAX_BUFFER_FRAMES}"
                ),
            ));
        }
        if !self.eng.device_list().iter().any(|d| d.name == device) {
            return Err(ErrorBody::new(
                ErrorCode::NotFound,
                format!("no output device named \"{device}\""),
            ));
        }
        self.eng.require_saveable()?;
        self.eng
            .config
            .machine
            .set_buffer_frames(device, frames)
            .map_err(|why| ErrorBody::new(ErrorCode::BadRequest, why))?;
        self.eng.save_machine();
        self.eng.log_device(
            LogLevel::Info,
            format!("audio device \"{device}\": buffer size set to {frames} frames"),
        );
        if let Some(key) = self.eng.outputs.by_name(device)
            && let Some(Backend::Device(d)) = self.eng.outputs.get_mut(key).map(|o| &mut o.backend)
        {
            let events = d.set_buffer(frames);
            // It does not play until it is back: the master clock follows.
            self.eng.outputs.follow_clock();
            self.device_events(key, events);
        }
        Ok(())
    }

    /// An open device no patch of any open doc maps to any more is parked (closed: not
    /// watched, not reported lost) once no voice plays on it; one a patch maps to again
    /// opens again. A device still holding a voice is looked at again next tick.
    fn park_outputs(&mut self, wanted: &[String]) {
        let mut changed = false;
        for key in 1..self.eng.outputs.len() {
            let busy = self.eng.outputs.has_voices(key);
            let Some(o) = self.eng.outputs.get_mut(key) else {
                continue;
            };
            let Backend::Device(d) = &mut o.backend else {
                continue;
            };
            let want = wanted.contains(&o.name);
            if want && d.is_parked() {
                d.unpark();
                changed = true;
            } else if !want && !d.is_parked() {
                if busy {
                    self.eng.outputs_dirty = true;
                } else {
                    d.park();
                    changed = true;
                    let name = o.name.clone();
                    self.eng.log_device(
                        LogLevel::Info,
                        format!("audio device \"{name}\" closed: no patch plays on it"),
                    );
                }
            }
        }
        if changed {
            self.eng.outputs.follow_clock();
            self.eng.refresh_playable();
            self.mark_warnings_dirty();
        }
    }

    /// A device that could not be opened as an output: logged once per device.
    fn output_warning(&mut self, name: &str, message: String) {
        if self.eng.output_warned.insert(name.to_owned()) {
            self.eng.log_device(LogLevel::Warn, message);
        }
    }

    /// The lookahead and `now` follow the largest buffer of the devices that play, so every
    /// deadline is still committed before the callback that renders it on each of them
    /// (commands already sent keep their samples). `now` never goes back. Every doc's cue
    /// engine takes the new lookahead before the engine's block changes (seams note (b)); if
    /// one cannot, they all keep the old one.
    fn follow_block(&mut self) {
        let Some(b) = self
            .eng
            .outputs
            .iter()
            .filter(|(key, o)| *key == 0 || o.is_up())
            .filter_map(|(_, o)| o.block())
            .max()
            .filter(|&b| b > 0 && b != self.eng.block)
        else {
            return;
        };
        let old = self.eng.block;
        let mut failed = None;
        for doc in &mut self.docs {
            if let Err(e) = doc.model.set_lookahead(b) {
                failed = Some(e);
                break;
            }
        }
        match failed {
            None => {
                self.eng.now_floor = self.eng.now();
                self.eng.log_device(
                    LogLevel::Info,
                    format!("audio devices: scheduling {b} frames ahead (was {old})"),
                );
                self.eng.block = b;
            }
            Some(e) => {
                for doc in &mut self.docs {
                    // The old lookahead was accepted before: this cannot fail.
                    let _ = doc.model.set_lookahead(old);
                }
                self.eng.log_device(
                    LogLevel::Warn,
                    format!("audio devices: {e}; commands may land late"),
                );
            }
        }
    }
}

impl DocCx<'_> {
    /// This doc's part of the loss of output `key`'s device `name` (as QLab 5;
    /// [`Control::device_lost`]): the patches on it are reported missing (loaded cues on them
    /// unload, starts are refused as broken, idle cues show broken), every audio run with a
    /// voice on it, or whose patch is on it, ends at once with `ActionFailed { device_lost }`
    /// (hard stop, no fade; the voice and its stream are released, auto-follow never fires),
    /// and every voice the doc has in that device's mixer is stopped, so nothing sounds, or
    /// resumes, when the device returns. When it was the `last` device that played, every
    /// audio run fails and every other active run stops too (`Input::DeviceLost`): nothing
    /// scheduled before the loss may fire after it. Returns the runs stopped.
    fn device_lost(&mut self, key: OutputKey, name: &str, last: bool) -> usize {
        let now = self.now();
        for input in self.sync_patches(false) {
            self.feed(input, now);
        }
        let mut runs = {
            let outputs = &self.eng.outputs;
            let on_device = |cue: CueId| last || self.cue_on_device(cue, name);
            self.doc.voices.on_device_where(|voice, cue, playing| {
                if playing {
                    outputs.output_of(voice) == key
                } else {
                    on_device(cue)
                }
            })
        };
        // Audio runs with no voice yet or any more (pre-wait, post-wait, paused) on the device
        // too: nothing scheduled before the outage may start by itself after it. They end as
        // device_lost breaks.
        for (cue, state) in self.doc.model.engine().cue_states() {
            if matches!(
                state,
                CueState::PreWaiting
                    | CueState::Running
                    | CueState::Paused
                    | CueState::PostWaiting
                    | CueState::Fading
                    | CueState::Stopping
            ) && let Some(run) = self.doc.model.engine().run_of(cue)
                && matches!(
                    self.doc.model.ws().find_cue(cue).map(|c| &c.kind),
                    Some(CueKind::Audio { .. })
                )
                && (last || self.cue_on_device(cue, name))
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
        if last {
            // Nothing else scheduled before the loss may fire after the return: every other
            // active run (memo, Wait, control, Fade, group; broken runs with a pending
            // continue) stops.
            self.feed(Input::DeviceLost, now);
        }
        // Orphaned tails and anything the engine no longer owns on that device. On the
        // primary they are dated on the first sample its mixer renders after the outage
        // (`stop_floor`); another device's commands are dated so while it is away.
        let floor = if key == 0 { self.stop_floor() } else { None };
        let voices: Vec<VoiceId> = self
            .eng
            .outputs
            .on_output(key, self.doc.voices.playing_ids());
        let commands = voices
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

    /// Whether audio cue `cue`'s patch is mapped to device `name` on this machine.
    fn cue_on_device(&self, cue: CueId, name: &str) -> bool {
        let ws = self.doc.model.ws();
        match ws.find_cue(cue).map(|c| &c.kind) {
            Some(CueKind::Audio { patch, .. }) => self
                .eng
                .config
                .machine
                .patch_entry(ws.id, *patch)
                .is_some_and(|m| m.device == name),
            _ => false,
        }
    }

    /// The device's notices: an outage, a mixer that could not be recovered, patches that map
    /// more device channels than the device has (`outputs_dropped`), patches with no free mixer
    /// slot (`mixer_patches_exhausted`).
    pub(super) fn device_warnings(&self, list: &mut Vec<WireWarning>) {
        let eng = &*self.eng;
        eng.stage_faults
            .warnings(list, |key| eng.device_prefix(key));
        for (_, output) in self.eng.outputs.iter() {
            let Backend::Device(d) = &output.backend else {
                continue;
            };
            let name = &output.name;
            if let Some(why) = d.unrecoverable() {
                list.push(notice(
                    "device_unrecoverable",
                    format!(
                        "audio device \"{name}\": {why}; sound cannot come back: restart \
                         openLab"
                    ),
                    None,
                ));
            } else if d.is_down() && d.has_opened() && !d.is_resuming() {
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
            if d.info().outputs_dropped && !d.is_down() && d.has_opened() {
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
        for patch in &self.doc.model.ws().patches {
            if let Some(PatchResolution::ChannelsMissing {
                device,
                needed,
                available,
            }) = self.eng.patches.resolution(self.doc.id, patch.id)
            {
                // The device itself may have the channels: every device's mixer has only as
                // many outputs as the primary's (#654), so channels beyond those cannot be used.
                let has = self
                    .eng
                    .device_list()
                    .into_iter()
                    .find(|d| &d.name == device)
                    .map_or(*available, |d| d.output_channels);
                let message = if has >= *needed {
                    format!(
                        "patch \"{}\" maps channel {needed} of \"{device}\", but openLab uses \
                         only its first {available} outputs (as many as the primary device \
                         has): its cues cannot play",
                        patch.name
                    )
                } else {
                    format!(
                        "patch \"{}\" maps {needed} channel(s) of \"{device}\", which has \
                         {has}: its cues cannot play",
                        patch.name
                    )
                };
                list.push(notice("outputs_dropped", message, None));
            }
        }
        for &id in self.eng.patches.exhausted(self.doc.id) {
            let name = self
                .doc
                .model
                .ws()
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
    }
}

impl Engine {
    /// What starts a message about output `key`'s device: its name while several devices are
    /// open, nothing with one (whose messages need not say which).
    pub(super) fn device_prefix(&self, key: OutputKey) -> String {
        match self.outputs.get(key) {
            Some(o) if self.outputs.len() > 1 => format!("audio device \"{}\": ", o.name),
            _ => String::new(),
        }
    }

    /// Output `key`'s stream runs at another buffer size than it asked for (the device could
    /// not do it and negotiation took the nearest it could): a `warn` log says so.
    pub(super) fn check_buffer(&mut self, key: OutputKey) {
        let Some(o) = self.outputs.get(key) else {
            return;
        };
        let Backend::Device(d) = &o.backend else {
            return;
        };
        if let (Some(asked), Some(got)) = (d.requested_buffer(), d.info().block_frames)
            && asked != got
        {
            let message = format!(
                "audio device \"{}\" cannot use {asked}-frame buffers; it plays with {got}",
                o.name
            );
            self.log_device(LogLevel::Warn, message);
        }
    }

    /// The output devices for a `list_devices` request.
    pub(super) fn device_list(&self) -> Vec<DeviceEntry> {
        match self.outputs.primary().backend.device() {
            Some(d) => d
                .listed()
                .iter()
                .map(|l| DeviceEntry {
                    name: l.name.clone(),
                    output_channels: u32::from(l.max_channels()),
                    is_default: l.is_default,
                    buffer_frames: self.config.machine.buffer_frames_for(&l.name),
                })
                .collect(),
            None => self
                .config
                .devices
                .iter()
                .map(|d| DeviceEntry {
                    name: d.name.clone(),
                    output_channels: d.output_channels,
                    is_default: false,
                    buffer_frames: self.config.machine.buffer_frames_for(&d.name),
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
