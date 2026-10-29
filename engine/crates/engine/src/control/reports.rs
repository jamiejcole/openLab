// SPDX-License-Identifier: GPL-3.0-or-later

//! What the control thread broadcasts to clients as the show runs: progress reports, meters,
//! the warnings list, state sent again after a drop, and the open-workspaces list.

use openlab_audio::VoiceId;
use openlab_core::protocol::{
    CueMeter, OutputMeter, ProgressItem, RuntimeChange, ServerMessage, WaitProgress, WireWait,
    WorkspaceInfo,
};
use openlab_core::{Input, SampleTime, Timer};

use super::voices::Effects;
use super::{Control, DocCx, DocId, Engine, Listed, MeterRound, REPORT_HZ, notice};

impl Control {
    pub(super) fn mark_warnings_dirty(&mut self) {
        for doc in &mut self.docs {
            doc.warnings_dirty = true;
        }
    }

    /// Reads the output and voice meters once when a report is due; each doc takes its part in
    /// its tick ([`DocCx::reports`]).
    pub(super) fn begin_meter_round(&mut self) {
        let rendered = self.eng.mixer.samples_rendered();
        let period = (u64::from(self.eng.rate.get()) / REPORT_HZ).max(1);
        if self.eng.last_meters.is_some_and(|l| rendered < l + period) {
            return;
        }
        self.eng.last_meters = Some(rendered);
        self.eng
            .mixer
            .read_output_meters_into(&mut self.eng.out_peaks);
        let listed = self
            .eng
            .mixer
            .read_voice_meters_into(&mut self.eng.meter_ids, &mut self.eng.meter_peaks);
        let prev = self.eng.last_meter_read.replace(rendered);
        self.eng.meter_round = Some(MeterRound {
            rendered,
            prev,
            listed,
        });
    }

    /// After the docs took their meters: voice ids no doc knows any more are forgotten (their
    /// owners stay recorded only while an event of them can still mean something).
    pub(super) fn end_meter_round(&mut self) {
        if self.eng.meter_round.take().is_none() {
            return;
        }
        let docs = &self.docs;
        self.eng.voice_ids.retain(|id, owner| {
            docs.iter()
                .find(|d| d.id == owner)
                .is_some_and(|d| d.voices.knows(id))
        });
    }

    /// The open docs as the app connections list them, in open order.
    pub fn workspace_list(&self) -> Vec<WorkspaceInfo> {
        self.docs
            .iter()
            .map(|d| WorkspaceInfo {
                doc: d.id,
                workspace_id: d.ws.id,
                name: d.ws.name.clone(),
                path: d
                    .show_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
                dirty: d.is_dirty(),
                path_hash: d.show_path.as_deref().map(crate::persist::path_key),
            })
            .collect()
    }

    /// Sends the `workspaces` list to the app connections when a doc opened or closed, or one's
    /// name, path or unsaved state changed since it was last sent (or that send did not fit).
    pub(super) fn publish_workspaces(&mut self) {
        let same = self.eng.listed.len() == self.docs.len()
            && self
                .eng
                .listed
                .iter()
                .zip(&self.docs)
                .all(|(l, d)| l.lists(d));
        if same && !self.eng.list_unsent {
            return;
        }
        self.eng.listed = self.docs.iter().map(Listed::of).collect();
        let workspaces = self.workspace_list();
        self.eng.list_unsent = !self
            .eng
            .out
            .send_state(None, ServerMessage::Workspaces { workspaces });
    }
}

impl Engine {
    /// Tells doc `doc`'s connections it closed (again on a later tick if that does not fit).
    pub(super) fn announce_closed(&mut self, doc: DocId) {
        if !self
            .out
            .send_state(Some(doc), ServerMessage::Closed { doc })
        {
            self.closed_unsent.push(doc);
        }
    }

    /// The `closed` messages that did not fit, once there is room.
    pub(super) fn resend_closed(&mut self) {
        while let Some(&doc) = self.closed_unsent.first() {
            if !self
                .out
                .send_state(Some(doc), ServerMessage::Closed { doc })
            {
                return;
            }
            self.closed_unsent.remove(0);
        }
    }
}

impl DocCx<'_> {
    /// Progress and meters, each at most [`REPORT_HZ`] in sample time; reconciliation of lost
    /// voice ends rides on the voice-meter read.
    pub(super) fn reports(&mut self) {
        let rendered = self.eng.mixer.samples_rendered();
        let period = (u64::from(self.eng.rate.get()) / REPORT_HZ).max(1);
        let due = |last: Option<SampleTime>| last.is_none_or(|l| rendered >= l + period);
        if due(self.doc.last_progress) {
            let mut items: Vec<ProgressItem> = self
                .doc
                .voices
                .progress(rendered)
                .into_iter()
                .map(|(cue, pos, left)| ProgressItem {
                    cue,
                    elapsed: self.seconds(pos),
                    remaining: left.map(|l| self.seconds(l)),
                })
                .collect();
            // Cues without a voice: their actions join the voices' (an audio cue's action is
            // never among them), their pre-waits and post-waits go in `waits` (M4.2e).
            let mut waits: Vec<WaitProgress> = Vec::new();
            let voiced = items.len();
            for t in self.doc.engine.timers(rendered) {
                let elapsed = self.seconds(t.elapsed);
                let wait = match t.timer {
                    Timer::Action => {
                        items.push(ProgressItem {
                            cue: t.cue,
                            elapsed,
                            remaining: t.remaining.map(|l| self.seconds(l)),
                        });
                        continue;
                    }
                    Timer::PreWait => WireWait::PreWait,
                    Timer::PostWait => WireWait::PostWait,
                };
                waits.push(WaitProgress {
                    cue: t.cue,
                    wait,
                    elapsed,
                    remaining: self.seconds(t.remaining.unwrap_or(0)),
                });
            }
            if items.len() > voiced {
                items.sort_by_key(|i| i.cue);
            }
            if !items.is_empty() || !waits.is_empty() {
                self.doc.last_progress = Some(rendered);
                let mut m = ServerMessage::Progress { items, waits };
                m.sanitise();
                self.eng.out.send_lossy(Some(self.doc.id), m);
            }
        }
        // The engine read the meters once this wake ([`Control::begin_meter_round`]): this
        // doc's voices and patches are taken from it.
        if let Some(round) = self.eng.meter_round {
            self.meters(round);
        }
    }

    fn seconds(&self, frames: u64) -> f64 {
        frames as f64 / f64::from(self.eng.rate.get())
    }

    /// This doc's part of the wake's meter read: its cues' peaks, its patches' outputs, and
    /// the reconciliation of its voices against the voices listed (ids are unique engine-wide,
    /// so another doc's listed voice is never taken for one of this doc's).
    fn meters(&mut self, round: MeterRound) {
        let MeterRound {
            rendered,
            prev,
            listed: n,
        } = round;
        let gone = self.doc.voices.take_gone();
        let outs = self.eng.cue_outputs.max(1);
        let mut cues: Vec<CueMeter> = Vec::new();
        let mut listed = Vec::with_capacity(n);
        for (k, id) in self.eng.meter_ids.iter().take(n).enumerate() {
            listed.push(*id);
            let cue = self
                .doc
                .voices
                .cue_of(*id)
                .or_else(|| gone.iter().find(|(g, _)| g == id).map(|(_, c)| *c));
            let Some(cue) = cue else { continue };
            let peaks = self
                .eng
                .meter_peaks
                .get(k * outs..(k + 1) * outs)
                .unwrap_or(&[]);
            match cues.iter_mut().find(|m| m.cue == cue) {
                Some(m) => {
                    for (a, b) in m.peaks.iter_mut().zip(peaks) {
                        *a = a.max(*b);
                    }
                }
                None => cues.push(CueMeter {
                    cue,
                    peaks: peaks.to_vec(),
                }),
            }
        }
        self.reconcile(&listed, rendered, prev);
        let outputs: Vec<OutputMeter> = self
            .doc
            .ws
            .patches
            .iter()
            .filter_map(|p| {
                let channels = self.eng.patches.channels(self.doc.id, p.id)?;
                let peaks = channels
                    .iter()
                    .map(|&c| {
                        usize::try_from(c)
                            .ok()
                            .and_then(|c| self.eng.out_peaks.get(c))
                            .copied()
                            .unwrap_or(0.0)
                    })
                    .collect();
                Some(OutputMeter { patch: p.id, peaks })
            })
            .collect();
        let live = self.doc.voices.has_playing() || !cues.is_empty();
        // Only while audio plays, plus one message after, so meters fall back to silence.
        if live || self.doc.meters_live {
            self.doc.meters_live = live;
            let mut m = ServerMessage::Meters { outputs, cues };
            m.sanitise();
            self.eng.out.send_lossy(Some(self.doc.id), m);
        }
    }

    /// A started voice the mixer has not listed in two meter reads in a row, while mixer
    /// events have been lost, has ended without its report reaching us: report it ended so its
    /// cue does not run forever (docs/control.md, Reconciliation).
    fn reconcile(&mut self, listed: &[VoiceId], rendered: SampleTime, prev: Option<SampleTime>) {
        let Some(prev) = prev else {
            return;
        };
        let lost_events = self.eng.mixer.dropped_events() > 0;
        let started: Vec<(VoiceId, SampleTime)> = self.doc.voices.started().collect();
        let mut fx = Effects::default();
        for (id, start) in started {
            if start >= prev {
                continue;
            }
            let absent = self.doc.voices.mark_absent(id, !listed.contains(&id));
            if absent >= 2 && lost_events {
                self.doc.voices.lost(id, rendered, &mut fx);
            }
        }
        let now = self.now();
        self.follow(fx, now);
    }

    /// This doc's state its clients missed goes again, newest, once there is room
    /// ([`Control::health`]).
    pub(super) fn resend_state(&mut self) {
        if self.doc.resend_document && self.eng.out.has_room() {
            self.document();
        }
        if self.doc.resend_warnings && self.eng.out.has_room() {
            self.doc.resend_warnings = !self.eng.out.send_state(
                Some(self.doc.id),
                ServerMessage::Warnings {
                    warnings: self.doc.warnings.clone(),
                },
            );
        }
    }

    /// The workspace-level notices in force: damaged media, preloads over budget, cues whose
    /// slices cannot play, audio-thread allocations. Sent when the list changes.
    pub(super) fn refresh_warnings(&mut self) {
        let mut list = self.doc.file.notices.clone();
        let mut damaged: Vec<(&str, usize)> = self.doc.media.damaged().collect();
        damaged.sort_unstable();
        for (path, n) in damaged {
            list.push(notice(
                "damaged_media",
                format!("{path}: {n} damaged part(s); they play as silence"),
                None,
            ));
        }
        let mut over: Vec<&str> = self.doc.media.over_budget().collect();
        over.sort_unstable();
        for path in over {
            list.push(notice(
                "preload_over_budget",
                format!("{path} does not fit the preload memory budget and is streamed"),
                None,
            ));
        }
        let mut failed: Vec<&str> = self.doc.media.preload_failed().collect();
        failed.sort_unstable();
        for path in failed {
            list.push(notice(
                "preload_failed",
                format!("{path} could not be loaded into memory and is streamed"),
                None,
            ));
        }
        for (cue, e) in self
            .doc
            .checks
            .check(&self.doc.ws, &mut self.doc.media, self.eng.rate)
        {
            list.push(notice(
                "bad_plan",
                format!("the cue's slices cannot be played: {e}"),
                Some(cue),
            ));
        }
        self.device_warnings(&mut list);
        self.eng.watch.watch().notices(&mut list);
        if self.eng.seen.alloc_violations > 0 {
            list.push(notice(
                "realtime_allocation",
                format!(
                    "the audio thread allocated memory {} time(s): report this bug",
                    self.eng.seen.alloc_violations
                ),
                None,
            ));
        }
        if list != self.doc.warnings {
            self.doc.warnings = list.clone();
            self.doc.resend_warnings = !self.eng.out.send_state(
                Some(self.doc.id),
                ServerMessage::Warnings { warnings: list },
            );
        }
        // The plans just checked also give the cues' nominal lengths, which a playlist halted by
        // a broken child times its auto-follow by (docs/cue-engine.md, "Playlist"), and the
        // media lengths clients show (M5.4d): only the cues whose plan changed.
        let changes = self.doc.checks.take_changes();
        if !changes.media.is_empty() {
            self.runtime(
                changes
                    .media
                    .into_iter()
                    .map(RuntimeChange::MediaDuration)
                    .collect(),
            );
        }
        if !changes.lengths.is_empty() {
            let now = self.now();
            self.feed(
                Input::CueLengths {
                    lengths: changes.lengths,
                },
                now,
            );
        }
    }
}
