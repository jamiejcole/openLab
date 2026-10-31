// SPDX-License-Identifier: GPL-3.0-or-later

//! After an edit to the model: the revision, the cue engine's and mixer's view of the change,
//! and a transaction whose rollback failed (an edit nobody can undo).

use openlab_core::protocol::{LogLevel, ServerMessage};
use openlab_core::{
    CommandError, Footprint, Input, SampleTime, model_changes, model_changes_indexed,
};

use super::DocCx;

impl DocCx<'_> {
    /// A command, undo or redo changed the workspace.
    pub(super) fn edited(&mut self, now: SampleTime, footprint: &Footprint) {
        let inputs = self.model_changed(footprint);
        self.document();
        for input in inputs {
            self.feed(input, now);
        }
        // Follow the edit at once: a deleted running cue stops now, not at the next deadline.
        self.feed(Input::Tick, now);
    }

    /// A refused command that is not "nothing changed": a transaction's rollback failed
    /// (`CommandError::RollbackFailed`, only a wrong inverse can cause it), so the workspace is
    /// valid but partly changed and history is cleared. It counts as an edit nobody can undo:
    /// logged as an error, revision bumped (dirty), the cue engine and catalog resynced, and
    /// every client sent the whole model, as for a model it cannot patch from a diff. Any other
    /// refusal does nothing here.
    pub(super) fn partly_changed(&mut self, now: SampleTime, error: &CommandError) {
        if !matches!(error, CommandError::RollbackFailed { .. }) {
            return;
        }
        self.log(
            LogLevel::Error,
            format!("workspace partly changed: {error}"),
        );
        self.doc.engine.model_changed();
        self.doc.revision += 1;
        // Not `reset`: the files are the same, so their probe results stay (a cue started
        // meanwhile must not find them pending).
        self.doc.media.sync(&self.doc.ws);
        let inputs = self.sync_workspace(false);
        self.document();
        for input in inputs {
            self.feed(input, now);
        }
        self.resend_snapshot();
        self.feed(Input::Tick, now);
    }

    /// Signals a workspace change to the engine (before its next `handle`, M4.2 contract) and
    /// the clients (one `model_changed` for `footprint`'s cues, M4.1a), and follows it on the
    /// audio side. Returns the inputs that must follow (patch resolutions).
    /// An Arm/Disarm change (`SetArmed`) goes through here too: it is saved with the show, so it
    /// makes the document dirty (as in QLab 5), but it is not an undo step.
    pub(super) fn model_changed(&mut self, footprint: &Footprint) -> Vec<Input> {
        // Lists, patches and settings decide which cues are broken and where the playheads are:
        // the cue-scoped shortcut is only for edits that touched cues alone.
        if footprint.is_model_wide() {
            self.doc.engine.model_changed();
        } else {
            self.doc.engine.model_changed_cues(footprint);
        }
        self.doc.revision += 1;
        // The catalog's cue index follows the edit first; the changes are read through it, at
        // the cost of the edit's cues (M4.12c). The walk is the fallback.
        self.doc.media.sync_cues(&self.doc.ws, footprint);
        let (upserts, deletes) =
            model_changes_indexed(&self.doc.ws, footprint, self.doc.media.index()).unwrap_or_else(
                || {
                    self.doc.model_change_walks += 1;
                    model_changes(&self.doc.ws, footprint)
                },
            );
        self.eng.out.send(
            Some(self.doc.id),
            ServerMessage::ModelChanged {
                revision: self.doc.revision,
                upserts,
                deletes,
            },
        );
        self.sync_workspace(false)
    }

    /// Brings patches and primes in line with the workspace (the media catalog is followed by
    /// the caller: whole for a newly opened workspace, from the footprint for an edit). `all`
    /// reports every patch resolution (a newly opened workspace), else only changes.
    pub(super) fn sync_workspace(&mut self, all: bool) -> Vec<Input> {
        self.doc.primes_dirty = true;
        let inputs = self.sync_patches(all);
        let now = self.now();
        self.doc.net.sync(&self.doc.ws.network_patches, now);
        inputs
    }

    /// Resolves the patches against the playable devices and brings the mixer's matrices (with
    /// the workspace master folded in) up to date. Returns the
    /// `Availability` for the patches whose resolution changed (`all`: every patch).
    pub(super) fn sync_patches(&mut self, all: bool) -> Vec<Input> {
        self.map_unmapped_patches();
        // A slot belongs to one doc's patch at a time, so only this doc's voices can be on its
        // slots: its own slots in use are all that hold a retired one back.
        let busy = self.doc.voices.slots_in_use();
        let (cmds, changed) = self.eng.patches.sync(
            self.doc.id,
            &self.doc.ws,
            &self.eng.config.machine,
            &self.eng.playable,
            &busy,
        );
        let now = self.now();
        // One batch (split only past the queue's size), so a master move changes every patch
        // on the same sample.
        self.send_commands(cmds.into_iter().map(|cmd| (now, cmd)).collect());
        // After the routing: a voice whose cue now names another patch follows it (#293).
        let moves = self.doc.voices.follow_patches(
            self.doc.id,
            &self.doc.ws,
            &self.eng.patches,
            &self.doc.engine,
            now,
        );
        if !moves.is_empty() {
            self.send_commands(moves);
        }
        self.doc.warnings_dirty = true;
        let patches = if all {
            self.eng.patches.resolutions(self.doc.id)
        } else {
            changed
        };
        if patches.is_empty() {
            Vec::new()
        } else {
            vec![Input::Availability {
                media: Vec::new(),
                patches,
            }]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::ControlConfig;
    use crate::control::tests::{memo_show, memo_workspace, uuid_zero};
    use openlab_core::protocol::{ClientMessage, ErrorCode, LogKind, Outcome};
    use openlab_core::{Command, CueId, CueState};

    fn rollback_failed() -> CommandError {
        CommandError::RollbackFailed {
            index: 2,
            source: Box::new(CommandError::Internal),
            rollback: Box::new(CommandError::InvalidValue("inverse")),
        }
    }

    /// A transaction whose rollback failed left the workspace partly changed: that is an edit
    /// (error logged, revision, dirty, cue engine told, clients sent the whole model) that
    /// nobody can undo. The partial change is simulated by editing behind the history.
    #[test]
    fn a_failed_rollback_is_treated_as_an_edit() {
        let mut s = memo_show();
        let cue = CueId(uuid_zero());
        s.transport(openlab_core::protocol::TransportAction::Start { cue });
        assert_eq!(s.control().engine().state(cue), CueState::PreWaiting);
        let revision = s.control().revision();
        s.take_outbound();
        // What the failed transaction did before it stopped, with history cleared as core does.
        let c = s.control_mut();
        c.docs[0].ws.apply(Command::RemoveCue { id: cue }).unwrap();
        c.docs[0].history.clear();
        let now = s.now();
        s.control_mut()
            .first_cx()
            .partly_changed(now, &rollback_failed());

        let c = s.control_mut();
        assert_eq!(c.revision(), revision + 1);
        assert!(c.docs[0].revision != c.docs[0].saved_revision);
        assert!(!c.docs[0].history.can_undo() && !c.docs[0].history.can_redo());
        assert_eq!(c.engine().unsignalled_model_changes(), 0);
        assert_ne!(c.engine().state(cue), CueState::PreWaiting);
        let (entries, ..) = c.eng.log_tail(None);
        assert!(entries.iter().any(|e| e.level == LogLevel::Error
            && e.kind == LogKind::Engine
            && e.message.contains("command 2")
            && e.message.contains("partly changed")
            && e.message.contains("inverse")));
        let out = s.take_outbound();
        let snapshot = out.iter().find_map(|m| match m {
            ServerMessage::Snapshot { snapshot } => Some(snapshot),
            _ => None,
        });
        let snapshot = snapshot.expect("clients get the whole model");
        assert_eq!(snapshot.revision, revision + 1);
        assert_eq!(&snapshot.workspace, s.control().workspace());
        assert!(out.iter().any(|m| matches!(
            m,
            ServerMessage::Document { document } if document.dirty && !document.can_undo
        )));
    }

    /// Every other refusal changed nothing: no revision, no snapshot.
    #[test]
    fn a_plain_refusal_changes_nothing() {
        let mut s = memo_show();
        let revision = s.control().revision();
        s.take_outbound();
        let now = s.now();
        s.control_mut()
            .first_cx()
            .partly_changed(now, &CommandError::Internal);
        assert_eq!(s.control().revision(), revision);
        assert_eq!(
            s.control().docs[0].revision,
            s.control().docs[0].saved_revision
        );
        assert!(s.take_outbound().is_empty());
    }

    fn two_cue_show() -> crate::control::offline::OfflineShow {
        let mut ws = memo_workspace(10.0);
        let mut second = ws.cue_lists[0].cues[0].clone();
        second.id = CueId(uuid::Uuid::from_u128(1));
        ws.cue_lists[0].cues.push(second);
        let mixer = openlab_audio::MixerConfig {
            sample_rate: 8000,
            output_channels: 2,
            max_block_frames: 64,
            max_voices: 8,
            max_voice_channels: 2,
            patches: 1,
            lookahead_ms: 500,
        };
        crate::control::offline::OfflineShow::new(ws, mixer, ControlConfig::new(8000, 64)).unwrap()
    }

    fn rename_both(a: &str, b: &str) -> Command {
        let rename = |n: u128, value: &str| Command::SetCueProperty {
            id: CueId(uuid::Uuid::from_u128(n)),
            property: openlab_core::CueProperty::Name {
                value: value.into(),
            },
        };
        Command::Transaction {
            commands: vec![rename(0, a), rename(1, b)],
        }
    }

    fn send_ok(s: &mut crate::control::offline::OfflineShow, message: ClientMessage) {
        let replies = s.send(message);
        assert!(replies.iter().any(|m| matches!(
            m,
            ServerMessage::Result {
                outcome: Outcome::Ok(_),
                ..
            }
        )));
    }

    /// The reply and the broadcasts of a request whose rollback failed: `command_refused` that
    /// says so, after the whole model.
    fn assert_partly_changed(
        s: &mut crate::control::offline::OfflineShow,
        replies: &[ServerMessage],
        before: u64,
    ) {
        let error = replies.iter().find_map(|m| match m {
            ServerMessage::Result {
                outcome: Outcome::Error(e),
                ..
            } => Some(e),
            _ => None,
        });
        let error = error.unwrap_or_else(|| panic!("a refusal: {replies:?}"));
        assert_eq!(error.code, ErrorCode::CommandRefused);
        assert!(error.message.contains("partly changed"));
        let c = s.control_mut();
        assert_eq!(c.revision(), before + 1);
        assert!(c.docs[0].revision != c.docs[0].saved_revision);
        assert!(!c.docs[0].history.can_undo() && !c.docs[0].history.can_redo());
        assert_eq!(c.engine().unsignalled_model_changes(), 0);
        let (entries, ..) = c.eng.log_tail(None);
        assert!(
            entries
                .iter()
                .any(|e| e.level == LogLevel::Error && e.message.contains("partly changed"))
        );
        let out = s.take_outbound();
        let snapshot = out.iter().find_map(|m| match m {
            ServerMessage::Snapshot { snapshot } => Some(snapshot),
            _ => None,
        });
        let snapshot = snapshot.expect("clients get the whole model");
        assert_eq!(&snapshot.workspace, s.control().workspace());
        assert!(!snapshot.document.can_undo);
    }

    #[test]
    fn a_command_whose_rollback_fails_is_an_unundoable_edit() {
        let mut s = two_cue_show();
        let before = s.control().revision();
        s.take_outbound();
        openlab_core::fault::inject(0, 1, true);
        let id = s.request_id();
        let replies = s.send(ClientMessage::Command {
            id,
            command: Box::new(rename_both("a", "b")),
            coalesce: None,
        });
        assert_partly_changed(&mut s, &replies, before);
        // The first rename stayed; the second never ran.
        let names: Vec<_> = s.control().workspace().cue_lists[0]
            .cues
            .iter()
            .map(|c| c.name.clone())
            .collect();
        assert_eq!(names, ["a", ""]);
    }

    #[test]
    fn an_undo_whose_rollback_fails_is_an_unundoable_edit() {
        let mut s = two_cue_show();
        let id = s.request_id();
        send_ok(
            &mut s,
            ClientMessage::Command {
                id,
                command: Box::new(rename_both("a", "b")),
                coalesce: None,
            },
        );
        // Behind the history's back: the undo step now refuses at its second command, and its
        // rollback is wrong.
        let c = s.control_mut();
        c.docs[0]
            .ws
            .apply(Command::RemoveCue {
                id: CueId(uuid_zero()),
            })
            .unwrap();
        let before = c.revision();
        s.take_outbound();
        openlab_core::fault::inject(0, usize::MAX, true);
        let id = s.request_id();
        let replies = s.send(ClientMessage::Undo { id });
        assert_partly_changed(&mut s, &replies, before);
    }

    #[test]
    fn a_redo_whose_rollback_fails_is_an_unundoable_edit() {
        let mut s = two_cue_show();
        let id = s.request_id();
        send_ok(
            &mut s,
            ClientMessage::Command {
                id,
                command: Box::new(rename_both("a", "b")),
                coalesce: None,
            },
        );
        let id = s.request_id();
        send_ok(&mut s, ClientMessage::Undo { id });
        let c = s.control_mut();
        c.docs[0]
            .ws
            .apply(Command::RemoveCue {
                id: CueId(uuid::Uuid::from_u128(1)),
            })
            .unwrap();
        let before = c.revision();
        s.take_outbound();
        openlab_core::fault::inject(0, usize::MAX, true);
        let id = s.request_id();
        let replies = s.send(ClientMessage::Redo { id });
        assert_partly_changed(&mut s, &replies, before);
    }
}
