// SPDX-License-Identifier: GPL-3.0-or-later

//! Transport actions (GO, stop, pause, panic, hotkeys): what each feeds the cue engine, and the
//! orphaned voices a stop or panic must silence itself.

use openlab_audio::CommandKind;
use openlab_core::protocol::TransportAction;
use openlab_core::{CueId, CueState, Input, SampleTime, descendants};

use super::{Control, DocCx, voices};

impl DocCx<'_> {
    /// A transport action on this doc (from its client, or an app-scope panic or hotkey).
    pub(super) fn transport(&mut self, action: TransportAction, now: SampleTime) {
        // Every item at this one `now`, in order, before anything else is handled: cues started
        // together start on the same sample (NFR-21). The codec refused any item `batch_cue`
        // does not accept (a nested batch among them); a batch from elsewhere is held to it too.
        if let TransportAction::Batch { actions } = action {
            for item in actions {
                if item.batch_cue().is_some() {
                    self.transport(item, now);
                }
            }
            return;
        }
        let scope = orphan_scope(&action);
        let panic = matches!(action, TransportAction::Panic);
        self.observe_transport(&action);
        if let Some(input) = transport_input(action) {
            self.feed(input, now);
        }
        // During an outage a panic fade could only play once the device is back, so it stops
        // everything at once: a second Panic within the fade is a hard stop (unless this one
        // already was).
        if panic && self.eng.backend.is_down() && self.doc.engine.panic_end().is_some() {
            self.feed(Input::Panic, now);
        }
        if let Some(scope) = scope {
            self.silence_orphans(scope, now);
        }
    }

    /// Stops voices still sounding for cues that no longer own them: a tail an underrun made
    /// longer than the action's known length, after the engine completed the action on its
    /// planned end (docs/control.md, Underrun). The engine has nothing left to stop there, so a
    /// Panic, a hard stop or a reset would leave it sounding.
    pub(super) fn silence_orphans(&mut self, scope: Scope, now: SampleTime) {
        let engine = &self.doc.engine;
        let owned = |cue: CueId, run: openlab_core::RunId| {
            engine.run_of(cue) == Some(run)
                && matches!(
                    engine.state(cue),
                    CueState::Running | CueState::Paused | CueState::Fading | CueState::Stopping
                )
        };
        let in_scope = |cue: CueId| match scope {
            Scope::All => true,
            Scope::Cue(root) => {
                root == cue
                    || self
                        .doc
                        .ws
                        .find_cue(root)
                        .is_some_and(|r| descendants(r.children()).any(|c| c.id == cue))
            }
        };
        let orphans = self
            .doc
            .voices
            .orphans(|cue, run| in_scope(cue) && !owned(cue, run));
        if orphans.is_empty() {
            return;
        }
        // A Panic fades them out with everything else, ending on the panic's end; otherwise
        // (or past it) they are cut at once.
        let fade = match (scope, self.doc.engine.panic_end()) {
            (Scope::All, Some(end)) if end > now => {
                openlab_audio::Fade::new(end - now, voices::to_curve(openlab_core::PANIC_CURVE))
            }
            _ => openlab_audio::Fade::NONE,
        };
        let floor = self.stop_floor();
        let commands = orphans
            .into_iter()
            .map(|voice| {
                let at = self.doc.voices.stop_date(voice, now, floor);
                (at, CommandKind::StopVoice { voice, fade })
            })
            .collect();
        self.send_commands(commands);
    }
}

impl Control {
    /// App-scope Panic (Esc; ARCHITECTURE §6): every open doc panics, as a `panic` transport in
    /// each would (twice within the fade is a hard stop in each).
    pub fn panic_all(&mut self) {
        self.each_doc(|cx| {
            let now = cx.now();
            cx.transport(TransportAction::Panic, now);
        });
        self.eng.mixer.pump();
    }

    /// App-scope hotkey (ARCHITECTURE §6; QLab 5 fires a hotkey in every open workspace, in
    /// front or not): every cue of every doc with this hotkey starts, as the app's `start`
    /// for each would. A doc whose transport keys use it starts nothing (the workspace action
    /// wins, M2.28); that action is the front window's, not the engine's.
    pub fn hotkey(&mut self, hotkey: &openlab_core::Hotkey) {
        self.each_doc(|cx| {
            let now = cx.now();
            for cue in cx.doc.ws.cues_with_hotkey(hotkey) {
                cx.transport(TransportAction::Start { cue }, now);
            }
        });
        self.eng.mixer.pump();
    }

    /// App-scope hotkey release: the release (`trigger_released`) of every cue of every doc
    /// with this hotkey (a cue whose second trigger is not on release ignores it).
    pub fn hotkey_released(&mut self, hotkey: &openlab_core::Hotkey) {
        self.each_doc(|cx| {
            let now = cx.now();
            for cue in cx.doc.ws.cues_with_hotkey(hotkey) {
                cx.transport(TransportAction::TriggerReleased { cue }, now);
            }
        });
        self.eng.mixer.pump();
    }
}

/// The cue-engine input for a transport action (docs/protocol.md, Transport actions); `None`
/// for a `batch`, which is one input per item.
pub fn transport_input(action: TransportAction) -> Option<Input> {
    Some(match action {
        TransportAction::Batch { .. } => return None,
        TransportAction::Go { list } => Input::Go { list },
        TransportAction::Panic => Input::Panic,
        TransportAction::GoFrom { list, cue } => Input::GoFrom { list, cue },
        TransportAction::StopAll | TransportAction::HardStopAll => Input::StopAll,
        TransportAction::PanicCue { cue } => Input::PanicCue { cue },
        TransportAction::PauseAll => Input::PauseAll,
        TransportAction::ResumeAll => Input::ResumeAll,
        TransportAction::SetPlayhead { list, cue } => Input::SetPlayhead { list, cue },
        TransportAction::SetPlayheadByNumber { list, number } => {
            Input::SetPlayheadByNumber { list, number }
        }
        TransportAction::PlayheadNext { list } => Input::PlayheadNext { list },
        TransportAction::PlayheadPrevious { list } => Input::PlayheadPrevious { list },
        TransportAction::Start { cue } => Input::Start { cue },
        TransportAction::Stop { cue, fade_out } => Input::Stop { cue, fade_out },
        TransportAction::HardStop { cue } => Input::Stop {
            cue,
            fade_out: None,
        },
        TransportAction::Pause { cue } => Input::Pause { cue },
        TransportAction::Resume { cue } => Input::Resume { cue },
        TransportAction::Load { cue, at } => Input::Load { cue, time: at },
        TransportAction::Reset { cue } => Input::Reset { cue },
        TransportAction::TriggerReleased { cue } => Input::TriggerReleased { cue },
        TransportAction::Preview { cue } => Input::Preview { cue },
        TransportAction::StopPreview { cue } => Input::StopPreview { cue },
    })
}

/// Which voices a transport action must silence besides what the engine stops.
#[derive(Clone, Copy)]
pub(super) enum Scope {
    All,
    Cue(CueId),
}

fn orphan_scope(action: &TransportAction) -> Option<Scope> {
    match action {
        TransportAction::Panic | TransportAction::StopAll | TransportAction::HardStopAll => {
            Some(Scope::All)
        }
        TransportAction::HardStop { cue }
        | TransportAction::Reset { cue }
        | TransportAction::Stop {
            cue,
            fade_out: None,
        } => Some(Scope::Cue(*cue)),
        _ => None,
    }
}
