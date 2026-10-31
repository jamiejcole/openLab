// SPDX-License-Identifier: GPL-3.0-or-later

//! Mixer faults outside any voice (NFR-01; docs/control.md "Mixer faults"). The mixer blames a
//! panic on the patch slot, device output, master or meters it hit in, resets that unit to
//! silence and reports it (`Event::StageFaulted`); after repeated faults it disables the unit.
//! Here the unit gets its settings again from the control thread's copy and the operator is
//! told. A disabled slot's cues are stopped as broken and its patch moves to another slot; a
//! disabled device output, master or meters stay a warning until the mixer is rebuilt (after
//! repeated faults no unit could be blamed for), which sends everything again.

use std::collections::{BTreeMap, BTreeSet};

use openlab_audio::{CommandKind, DisabledUnits, FaultStage, SampleTime};
use openlab_core::PatchId;
use openlab_core::protocol::{LogLevel, WireWarning};

use super::voices::Effects;
use super::{Control, DocCx, notice};

/// The engine's view of the mixer's stage faults.
#[derive(Default)]
pub(super) struct StageFaults {
    /// Units the mixer disabled, for the warnings. Cleared by a rebuild.
    disabled: Disabled,
    /// When each stage's fault was last logged and told: a unit that keeps faulting (further
    /// apart than the mixer's window, or a rebuild after rebuild) is told about once a second.
    told: BTreeMap<FaultStage, SampleTime>,
}

#[derive(Default)]
struct Disabled {
    outputs: BTreeSet<u16>,
    master: bool,
    meters: bool,
}

impl StageFaults {
    /// The warnings in force for the units disabled.
    pub(super) fn warnings(&self, list: &mut Vec<WireWarning>) {
        self.disabled.warnings(list);
    }

    /// Whether a fault of `stage` at `now` is to be logged and told (none was in the second
    /// before it, at `rate`); a unit disabled is always told (it happens once).
    fn tell(&mut self, stage: FaultStage, disabled: bool, now: SampleTime, rate: u32) -> bool {
        let quiet = self
            .told
            .get(&stage)
            .is_some_and(|&t| now < t.saturating_add(u64::from(rate)));
        if quiet && !disabled {
            return false;
        }
        self.told.insert(stage, now);
        true
    }
}

impl Disabled {
    /// The warnings in force for the units disabled.
    pub(super) fn warnings(&self, list: &mut Vec<WireWarning>) {
        for o in &self.outputs {
            list.push(notice(
                "audio_fault",
                format!(
                    "device output {} kept failing in the audio engine and is silent: restart \
                     openLab to use it again",
                    u32::from(*o) + 1
                ),
                None,
            ));
        }
        if self.master {
            list.push(notice(
                "audio_fault",
                "the audio engine's master stage kept failing and is bypassed; levels are \
                 unaffected"
                    .to_owned(),
                None,
            ));
        }
        if self.meters {
            list.push(notice(
                "audio_fault",
                "the level meters kept failing in the audio engine and have stopped: restart \
                 openLab to see them again"
                    .to_owned(),
                None,
            ));
        }
    }
}

impl Control {
    /// The mixer reported a fault in `stage` (`disabled`: it gave up on that unit).
    pub(super) fn stage_faulted(&mut self, stage: FaultStage, disabled: bool) {
        self.eng.stage_reports += 1;
        self.answer_stage_fault(stage, disabled);
    }

    fn answer_stage_fault(&mut self, stage: FaultStage, disabled: bool) {
        let now = self.eng.now();
        let tell = self
            .eng
            .stage_faults
            .tell(stage, disabled, now, self.eng.rate.get());
        if tell {
            let what = match stage {
                FaultStage::Patch(slot) => format!("mixer patch slot {slot}"),
                FaultStage::Output(o) => format!("device output {}", u32::from(o) + 1),
                FaultStage::Master => "the mixer master".to_owned(),
                FaultStage::Meters => "the level meters".to_owned(),
                FaultStage::Mixer => "the mixer".to_owned(),
            };
            let how = match (stage, disabled) {
                (FaultStage::Mixer, _) => {
                    "faults with no known cause kept repeating; the mixer's routing was rebuilt"
                        .to_owned()
                }
                (_, true) => format!("{what} kept failing and was disabled"),
                (_, false) => format!("{what} failed and was reset"),
            };
            let count = self.eng.mixer.faulted_stages();
            self.eng.log(
                LogLevel::Error,
                format!("audio engine fault: {how} ({count} mixer stage fault(s) so far)"),
            );
        }
        let mut commands = Vec::new();
        let mut told = None;
        let faults = &mut self.eng.stage_faults.disabled;
        match stage {
            FaultStage::Patch(slot) => self.patch_faulted(slot, disabled, tell),
            FaultStage::Output(o) if disabled => {
                faults.outputs.insert(o);
            }
            FaultStage::Output(o) => {
                commands.extend(self.eng.patches.resend_output(o));
                told = Some(format!(
                    "device output {} failed in the audio engine and was reset: it may have \
                     dropped out briefly",
                    u32::from(o) + 1
                ));
            }
            FaultStage::Master if disabled => faults.master = true,
            FaultStage::Master => {
                commands.push(CommandKind::SetMaster { gain: 1.0 });
                told = Some(
                    "the audio engine's master stage failed and was reset: sound may have \
                     dropped out briefly"
                        .to_owned(),
                );
            }
            FaultStage::Meters => faults.meters |= disabled,
            FaultStage::Mixer => {
                *faults = Disabled::default();
                commands = self.eng.patches.rebuilt();
                told = Some(
                    "the audio engine kept failing and its routing was rebuilt: sound may have \
                     dropped out briefly"
                        .to_owned(),
                );
            }
        }
        self.eng
            .send_commands(commands.into_iter().map(|c| (now, c)).collect());
        if let Some(message) = told.filter(|_| tell) {
            self.each_doc(|cx| cx.tell(message.clone()));
        }
        self.mark_warnings_dirty();
    }

    /// Mixer slot `slot` was reset (sent its gains again) or disabled (quarantined; the cues on
    /// it break and its patch takes another slot). `tell`: the operator is told.
    fn patch_faulted(&mut self, slot: u16, disabled: bool, tell: bool) {
        let owner = self.eng.patches.owner(slot);
        if disabled {
            self.eng.patches.quarantine(slot);
        } else {
            let now = self.eng.now();
            let commands = self.eng.patches.resend(slot);
            self.eng
                .send_commands(commands.into_iter().map(|c| (now, c)).collect());
        }
        if let Some((doc, patch)) = owner
            && let Some(mut cx) = self.cx_of(doc)
        {
            cx.patch_faulted(slot, patch, disabled, tell);
        }
    }

    /// Fault reports the mixer counted by the previous health check but that never arrived (the
    /// event queue was full): the unit they reset would stay silent, so everything is sent
    /// again (harmless for what was not reset).
    pub(super) fn lost_stage_reports(&mut self, counted_before: u64) {
        if counted_before <= self.eng.stage_reports {
            return;
        }
        let lost = counted_before - self.eng.stage_reports;
        self.eng.stage_reports = counted_before;
        self.eng.log(
            LogLevel::Error,
            format!(
                "audio engine fault: {lost} mixer fault report(s) were lost; the mixer's routing \
                 was sent again"
            ),
        );
        let now = self.eng.now();
        let commands = self.eng.patches.resend_all();
        self.eng
            .send_commands(commands.into_iter().map(|c| (now, c)).collect());
    }
}

impl Control {
    /// Units the mixer had disabled at the previous health check (`before`; every report
    /// queued by then has been drained) and still has (`now`: a rebuild since re-enabled the
    /// others) that the control thread was never told of: their report was lost to a full
    /// event queue, so they are answered now as reported.
    pub(super) fn lost_disabled_reports(&mut self, before: DisabledUnits, now: DisabledUnits) {
        let still: Vec<FaultStage> = now.stages().collect();
        let unknown: Vec<FaultStage> = before
            .stages()
            .filter(|stage| still.contains(stage))
            .filter(|&stage| !self.knows_disabled(stage))
            .collect();
        for stage in unknown {
            self.eng.log(
                LogLevel::Error,
                format!("audio engine fault: the report of a disabled {stage:?} was lost"),
            );
            self.answer_stage_fault(stage, true);
        }
    }

    fn knows_disabled(&self, stage: FaultStage) -> bool {
        let d = &self.eng.stage_faults.disabled;
        match stage {
            FaultStage::Patch(slot) => self.eng.patches.is_quarantined(slot),
            FaultStage::Output(o) => d.outputs.contains(&o),
            FaultStage::Master => d.master,
            FaultStage::Meters => d.meters,
            FaultStage::Mixer => true,
        }
    }
}

impl DocCx<'_> {
    fn patch_faulted(&mut self, slot: u16, patch: PatchId, disabled: bool, tell: bool) {
        let now = self.now();
        let name = self
            .doc
            .model
            .ws()
            .patches
            .iter()
            .find(|p| p.id == patch)
            .map_or_else(|| "(removed)".to_owned(), |p| p.name.clone());
        let mut fx = Effects::default();
        let message = if disabled {
            self.doc.voices.fail_slot(slot, now, &mut fx);
            format!(
                "patch \"{name}\" kept failing in the audio engine: the cues playing on it were \
                 stopped, and it plays on another mixer slot from here if one is free"
            )
        } else {
            format!(
                "patch \"{name}\" failed in the audio engine and was reset: its cues may have \
                 dropped out briefly"
            )
        };
        if tell {
            fx.notices.push(("audio_fault", message, None));
        }
        self.follow(fx, now);
        if disabled {
            for input in self.sync_patches(false) {
                self.feed(input, now);
            }
        }
    }

    /// An `audio_fault` notice for this doc's operator.
    fn tell(&mut self, message: String) {
        let now = self.now();
        let mut fx = Effects::default();
        fx.notices.push(("audio_fault", message, None));
        self.follow(fx, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::tests::{memo_show, memo_workspace};
    use crate::control::{ControlConfig, DocId};
    use openlab_audio::VoiceId;
    use openlab_core::protocol::{RuntimeChange, ServerMessage};

    /// `audio_fault` notices sent as runtime changes, and the last warnings list's.
    fn fault_notices(msgs: &[ServerMessage]) -> (Vec<String>, Option<Vec<String>>) {
        let mut runtime = Vec::new();
        let mut list = None;
        let fault = |w: &WireWarning| match w {
            WireWarning::Notice { code, message, .. } if code == "audio_fault" => {
                Some(message.clone())
            }
            _ => None,
        };
        for m in msgs {
            match m {
                ServerMessage::Runtime { changes, .. } => {
                    runtime.extend(changes.iter().filter_map(|c| match c {
                        RuntimeChange::Warning { warning } => fault(warning),
                        _ => None,
                    }));
                }
                ServerMessage::Warnings { warnings } => {
                    list = Some(warnings.iter().filter_map(fault).collect());
                }
                _ => {}
            }
        }
        (runtime, list)
    }

    /// A memo show with one two-output patch "Main", on a mixer (and engine) of two patch
    /// slots, run for a block.
    fn patched_show() -> (crate::control::offline::OfflineShow, openlab_core::PatchId) {
        let mut ws = memo_workspace(10.0);
        let patch = openlab_core::PatchId(uuid::Uuid::from_u128(5));
        let output = openlab_core::PatchOutput {
            level_db: 0.0,
            mute: false,
            delay_ms: 0.0,
            polarity_inverted: false,
        };
        ws.patches.push(openlab_core::OutputPatch {
            id: patch,
            name: "Main".into(),
            master_db: 0.0,
            outputs: vec![output; 2],
        });
        let mixer = openlab_audio::MixerConfig {
            sample_rate: 8000,
            output_channels: 2,
            max_block_frames: 64,
            max_voices: 8,
            max_voice_channels: 2,
            patches: 2,
            lookahead_ms: 500,
        };
        let mut config = ControlConfig::new(8000, 64);
        config.mixer_patches = 2;
        // The patch is mapped onto it by default, so it is heard.
        config.devices = vec![openlab_core::DeviceInfo {
            name: "Dev".into(),
            output_channels: 2,
        }];
        let mut s = crate::control::offline::OfflineShow::new(ws, mixer, config).unwrap();
        s.run(64);
        s.take_outbound();
        (s, patch)
    }

    /// The last rendered sample of each output, after `frames` more frames.
    fn level_after(s: &mut crate::control::offline::OfflineShow, frames: u64) -> [f32; 2] {
        s.run(frames);
        [
            s.channel(0).last().copied().unwrap(),
            s.channel(1).last().copied().unwrap(),
        ]
    }

    /// Sends `kind` straight to the mixer, as the mixer's own reset after a fault would leave it.
    fn mixer_does(s: &mut crate::control::offline::OfflineShow, kind: CommandKind) {
        let eng = &mut s.control_mut().eng;
        let at = eng.now();
        eng.mixer.send(at, kind);
    }

    /// NFR-01 (review of #300, H1): after a fault the mixer has reset a unit, and the control
    /// thread's re-send is what brings the sound back. A voice plays through the patch onto both
    /// outputs; each unit is put in a silent reset state, then the control thread answers.
    #[test]
    fn a_reset_mixer_unit_plays_at_its_level_again_once_the_control_thread_answers() {
        use crate::control::offline::OfflineShow;
        use openlab_audio::FaultStage;
        let (mut s, patch) = patched_show();
        let slot = s
            .control()
            .eng
            .patches
            .mixer_patch(DocId(0), patch)
            .unwrap();
        let (mut tx, rx) = rtrb::RingBuffer::new(1 << 16);
        tx.push_entire_slice(&[0.5; 1 << 16]).unwrap();
        let mut v = openlab_audio::VoiceStart::new(VoiceId(u64::MAX - 1), rx, 1, 2);
        v.patch = slot;
        v.set_crosspoint(0, 0, 1.0);
        v.set_crosspoint(0, 1, 1.0);
        mixer_does(&mut s, CommandKind::StartVoice(Box::new(v)));
        let heard = level_after(&mut s, 640);
        assert!(heard[0] > 0.0 && heard[1] > 0.0, "{heard:?}");
        let zero_slot = |s: &mut OfflineShow| {
            for cue_output in 0..2 {
                for device_output in 0..2 {
                    let kind = CommandKind::SetPatchRouting {
                        patch: slot,
                        cue_output,
                        device_output,
                        gain: 0.0,
                    };
                    mixer_does(s, kind);
                }
            }
        };
        let quiet_output = |s: &mut OfflineShow| {
            let settings = openlab_audio::OutputSettings {
                level: 0.0,
                ..Default::default()
            };
            mixer_does(
                s,
                CommandKind::SetOutput {
                    output: 0,
                    settings,
                },
            );
        };
        let quiet_master =
            |s: &mut OfflineShow| mixer_does(s, CommandKind::SetMaster { gain: 0.0 });

        quiet_master(&mut s);
        assert_eq!(level_after(&mut s, 640), [0.0, 0.0]);
        s.control_mut().stage_faulted(FaultStage::Master, false);
        assert_eq!(level_after(&mut s, 640), heard, "master");

        quiet_output(&mut s);
        assert_eq!(level_after(&mut s, 640), [0.0, heard[1]]);
        s.control_mut().stage_faulted(FaultStage::Output(0), false);
        assert_eq!(level_after(&mut s, 640), heard, "output");

        zero_slot(&mut s);
        assert_eq!(level_after(&mut s, 640), [0.0, 0.0]);
        s.control_mut()
            .stage_faulted(FaultStage::Patch(slot), false);
        assert_eq!(level_after(&mut s, 640), heard, "patch");

        for (k, answer) in ["rebuild", "lost report"].into_iter().enumerate() {
            zero_slot(&mut s);
            quiet_output(&mut s);
            quiet_master(&mut s);
            assert_eq!(level_after(&mut s, 640), [0.0, 0.0]);
            if k == 0 {
                s.control_mut().stage_faulted(FaultStage::Mixer, false);
            } else {
                let counted = s.control().eng.stage_reports + 1;
                s.control_mut().lost_stage_reports(counted);
            }
            assert_eq!(level_after(&mut s, 640), heard, "{answer}");
        }
    }

    /// #316: a disabled unit whose report was lost (seen only in the mixer's published flags)
    /// is answered as if reported: the patch moves off its slot, a disabled output is a warning
    /// in force. One a rebuild re-enabled meanwhile is left alone.
    #[test]
    fn a_disabled_unit_whose_report_was_lost_is_found_from_the_mixers_flags() {
        let (mut s, patch) = patched_show();
        let at = |s: &crate::control::offline::OfflineShow| {
            s.control().eng.patches.mixer_patch(DocId(0), patch)
        };
        let first = at(&s).unwrap();
        let flags = openlab_audio::DisabledUnits {
            patches: 1 << first,
            outputs: 1 << 1,
            ..Default::default()
        };
        s.control_mut()
            .lost_disabled_reports(flags, openlab_audio::DisabledUnits::default());
        assert_eq!(at(&s), Some(first));
        s.control_mut().lost_disabled_reports(flags, flags);
        let moved = at(&s);
        assert!(moved.is_some_and(|m| m != first), "{moved:?}");
        s.run(64);
        let (_, list) = fault_notices(&s.take_outbound());
        assert!(list.unwrap().iter().any(|m| m.contains("device output 2")));
        // Known now: nothing more happens.
        s.control_mut().lost_disabled_reports(flags, flags);
        assert_eq!(at(&s), moved);
    }

    /// NFR-01: a mixer fault outside any voice reaches the workspace, and the unit it hit gets
    /// its settings again; a disabled patch moves to another slot, a disabled output is a
    /// warning until a rebuild, which frees the quarantined slot again.
    #[test]
    fn a_mixer_stage_fault_reaches_the_workspace_and_its_unit_is_set_again() {
        use openlab_audio::FaultStage;
        let (mut s, patch) = patched_show();
        let slot = |s: &crate::control::offline::OfflineShow| {
            s.control().eng.patches.mixer_patch(DocId(0), patch)
        };
        let first = slot(&s).unwrap();

        // As the mixer's event queue hands it over.
        s.control_mut()
            .route_mixer_event(openlab_audio::Event::StageFaulted {
                stage: FaultStage::Patch(first),
                at: 0,
                disabled: false,
            });
        assert_eq!(s.control().eng.stage_reports, 1);
        let (told, _) = fault_notices(&s.take_outbound());
        assert!(told.iter().any(|m| m.contains("\"Main\"")), "{told:?}");
        assert_eq!(slot(&s), Some(first));
        // A unit that keeps faulting is told about once a second, not once a fault.
        s.control_mut()
            .stage_faulted(FaultStage::Patch(first), false);
        assert!(s.take_outbound().is_empty());
        s.run(8000);
        s.take_outbound();
        s.control_mut()
            .stage_faulted(FaultStage::Patch(first), false);
        assert_eq!(fault_notices(&s.take_outbound()).0.len(), 1);

        s.control_mut()
            .stage_faulted(FaultStage::Patch(first), true);
        let moved = slot(&s).unwrap();
        assert_ne!(moved, first);
        assert_eq!(s.control().eng.patches.free_slots(), 0);

        s.control_mut().stage_faulted(FaultStage::Output(1), true);
        s.run(64);
        let (_, list) = fault_notices(&s.take_outbound());
        assert!(
            list.unwrap().iter().any(|m| m.contains("device output 2")),
            "a disabled output is a warning in force"
        );

        s.control_mut().stage_faulted(FaultStage::Mixer, false);
        s.run(64);
        let (told, list) = fault_notices(&s.take_outbound());
        assert!(told.iter().any(|m| m.contains("rebuilt")), "{told:?}");
        assert_eq!(list, Some(Vec::new()));
        assert_eq!(slot(&s), Some(moved));
        assert_eq!(s.control().eng.patches.free_slots(), 1);
    }

    /// A fault report the mixer counted but the control thread never got (a full event queue)
    /// would leave its unit silent: everything is sent again, and it is logged.
    #[test]
    fn a_lost_mixer_fault_report_sends_the_routing_again() {
        let mut s = memo_show();
        s.run(64);
        s.take_outbound();
        s.control_mut().lost_stage_reports(2);
        assert_eq!(s.control().eng.stage_reports, 2);
        let logged = s.take_outbound().iter().any(
            |m| matches!(m, ServerMessage::Log { message, .. } if message.contains("were lost")),
        );
        assert!(logged);
        // Counted again at the next check: nothing more is lost.
        s.control_mut().lost_stage_reports(2);
        assert!(s.take_outbound().is_empty());
    }
}
