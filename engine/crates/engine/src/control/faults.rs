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
            .ws
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
