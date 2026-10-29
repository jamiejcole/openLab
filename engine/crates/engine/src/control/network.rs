// SPDX-License-Identifier: GPL-3.0-or-later

//! A doc's network output: a Network cue's OSC message handed to [`netout::NetOut`], its
//! answers and reports, and the operator's switch that disables all network output.

use std::sync::atomic::Ordering;

use openlab_core::protocol::{LogKind, LogLevel, RuntimeChange};
use openlab_core::{CueId, Input, NetworkPatchId, OscArg, SampleTime};

use super::netout::{self, NetAnswer, SendOutcome};
use super::{DocCx, Engine, notice};

impl Engine {
    /// PB-10, for every doc.
    pub(super) fn set_network_disabled(&mut self, disabled: bool) {
        if self.network_disabled() != disabled {
            self.net_disabled.store(disabled, Ordering::SeqCst);
            let text = if disabled {
                "network output disabled: Network cues send nothing"
            } else {
                "network output enabled"
            };
            self.log(LogLevel::Warn, text.to_owned());
        }
    }
}

impl DocCx<'_> {
    /// A Network cue's turn (CT-30): encodes the message and hands it to its destination's
    /// worker. Whatever goes wrong is a warning; the cue completes regardless (QLab: a Network
    /// cue does not wait for, or learn of, delivery).
    pub(super) fn send_osc(
        &mut self,
        cue: CueId,
        destination: NetworkPatchId,
        address: &str,
        args: &[OscArg],
    ) {
        let Some(patch) = self
            .doc
            .ws
            .network_patches
            .iter()
            .find(|p| p.id == destination)
            .cloned()
        else {
            // Cannot happen: a cue without its patch has a problem and does not send.
            self.net_warning(cue, "network patch missing".to_owned());
            return;
        };
        if self.doc.net.disabled() {
            let label = self.cue_label(cue);
            self.record(
                LogKind::Engine,
                LogLevel::Info,
                format!(
                    "{label}: network output is disabled, {address} to {} not sent",
                    patch.name
                ),
                Some(cue),
                None,
            );
            return;
        }
        let packet = match netout::packet(address, args, patch.protocol) {
            Ok(p) => p,
            Err(e) => return self.net_warning(cue, format!("message not sent: {e}")),
        };
        match self.doc.net.send(&patch, cue, packet) {
            SendOutcome::Queued | SendOutcome::Suppressed => {}
            SendOutcome::Refused(why) => {
                self.net_warning(cue, format!("message to {} not sent: {why}", patch.name));
            }
        }
    }

    /// A worker's answer: a failed send is a warning; a look-up goes to the cue engine.
    pub(super) fn net_answer(&mut self, answer: NetAnswer) {
        let now = self.now();
        match answer {
            NetAnswer::SendFailed {
                cue,
                ref destination,
                ref reason,
                ..
            } => {
                let message = format!("message to {destination} not sent: {reason}");
                // A TCP failure also asks for a fresh look-up of the host.
                let reports = self.doc.net.answer(answer, now);
                self.network_reports(reports, now);
                self.net_warning(cue, message);
            }
            lookup @ NetAnswer::Lookup { .. } => {
                let reports = self.doc.net.answer(lookup, now);
                self.network_reports(reports, now);
            }
        }
        self.note_lookup_limit();
    }

    pub(super) fn note_lookup_limit(&mut self) {
        for note in self.doc.net.take_notes() {
            self.log(LogLevel::Warn, note);
        }
        if self.doc.net.take_limit_note() {
            self.log(
                LogLevel::Warn,
                "host look-ups are stuck; names waiting for one are taken to be reachable until one answers"
                    .to_owned(),
            );
        }
    }

    pub(super) fn network_reports(&mut self, patches: netout::Reports, now: SampleTime) {
        if !patches.is_empty() {
            self.feed(Input::NetworkAvailability { patches }, now);
        }
    }

    /// A show-time notice (`network_send_failed`) for a Network cue, logged and sent to clients.
    fn net_warning(&mut self, cue: CueId, message: String) {
        let changes = vec![RuntimeChange::Warning {
            warning: notice("network_send_failed", message, Some(cue)),
        }];
        self.observe_changes(&changes);
        self.runtime(changes);
    }
}
