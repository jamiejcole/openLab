// SPDX-License-Identifier: GPL-3.0-or-later

//! One wake of the control loop ([`Control::tick`]): the engine-wide steps (mixer events, the
//! media watch, the probe watchdog, slot hand-outs, health) and each doc's own tick.

use openlab_audio::{Event, VoiceId};
use openlab_core::protocol::LogLevel;
use openlab_core::{Input, SampleTime, seconds_to_samples};

use super::voices::Effects;
use super::watch::WatchEvent;
use super::{Control, DocCx, DocId, PROBE_HOLD_LIMIT, STREAM_START_LIMIT, media};

impl Control {
    /// One wake of the loop (ARCHITECTURE §3.1, §3.2; docs/audio.md: every tick): the device,
    /// the mixer's events (each to the doc owning the voice), the media watch and this wake's
    /// meter read, once; then each doc ([`DocCx::tick`]: its feedback, deadlines, priming,
    /// reports, saves); then the engine's probe watchdog, slot hand-outs, document worker,
    /// settings, health and status, and `pump`. With no doc open only the engine's steps run.
    pub fn tick(&mut self) {
        let rendered = self.eng.mixer.samples_rendered();
        self.eng.last_tick_stalled = rendered == self.eng.last_tick_rendered;
        self.eng.last_tick_rendered = rendered;
        self.poll_device();
        self.route_mixer_events();
        self.watch_tick();
        self.begin_meter_round();
        self.each_doc(|cx| cx.tick());
        self.end_meter_round();
        self.balance_primes();
        self.follow_key_changes();
        self.probe_watchdog();
        self.waveform_watchdog();
        self.hand_out_freed_slots();
        self.eng.check_stalled_read();
        self.eng.check_stalled_save();
        self.eng.poll_clears();
        self.poll_orphan_saves();
        self.eng.resend_closed();
        self.publish_workspaces();
        let awake = self
            .docs
            .iter()
            .any(|d| d.show_mode || d.model.engine().has_active_cues());
        self.eng.keep_awake(awake);
        self.eng.settings_tick();
        self.health();
        let paths = self.docs.iter().map(|d| d.show_path.as_deref());
        self.eng.observe_tick(paths);
        self.eng.mixer.pump();
    }

    /// The mixer's events, drained once and handed to the doc that owns each voice. An event of
    /// a voice no open doc owns (a closed doc's) is dropped; `PendingFull` means nothing to any
    /// doc; a stage fault is the engine's ([`Self::stage_faulted`]).
    fn route_mixer_events(&mut self) {
        while let Some(e) = self.eng.mixer.poll_event() {
            self.route_mixer_event(e);
        }
    }

    pub(super) fn route_mixer_event(&mut self, e: Event) {
        if let Event::StageFaulted {
            stage, disabled, ..
        } = e
        {
            self.stage_faulted(stage, disabled);
            return;
        }
        let Some(voice) = event_voice(&e) else {
            return;
        };
        let Some(owner) = self.eng.voice_ids.owner(voice) else {
            return;
        };
        if let Some(doc) = self.docs.iter_mut().find(|d| d.id == owner) {
            doc.events.push(e);
            doc.events_routed += 1;
        }
    }

    /// Each catalog's key changes to the shared watch, then the watch's batches to the docs
    /// that watch their files, its watchdog, log entries and notices.
    fn watch_tick(&mut self) {
        self.follow_key_changes();
        for batch in std::mem::take(&mut self.eng.watch_batches) {
            let events = self.eng.watch.accept(batch);
            let mut touched: Vec<DocId> = Vec::new();
            for (doc, event) in events {
                if let Some(mut cx) = self.cx_of(doc) {
                    cx.watch_event(event);
                    if !touched.contains(&doc) {
                        touched.push(doc);
                    }
                }
            }
            for doc in &mut self.docs {
                if touched.contains(&doc.id) {
                    // Primes of a file gone are dropped; a file probed again near a playhead
                    // is probed first.
                    doc.primes_dirty = true;
                    doc.warnings_dirty = true;
                }
            }
        }
        self.follow_key_changes();
        let watch = self.eng.watch.watch_mut();
        watch.supervise();
        let logs = watch.take_logs();
        let changed = watch.take_notices_changed();
        let stalled: Vec<String> = watch.stalled_volumes().map(str::to_owned).collect();
        self.eng
            .pool
            .set_watch_stalled(stalled.iter().map(String::as_str));
        for (level, message) in logs {
            self.eng.log(level, message);
        }
        if changed {
            self.mark_warnings_dirty();
        }
    }

    /// Hands every catalog's new key changes to the shared watch.
    pub(super) fn follow_key_changes(&mut self) {
        for doc in &mut self.docs {
            let changes = doc.media.take_key_changes();
            self.eng.watch.apply(doc.id, changes);
        }
    }

    /// The waveform reader's watchdog: a read past its timeout is given up on and its requests
    /// fail with `io`. Apart from the probe pool's: a slow read never counts as a stuck probe.
    fn waveform_watchdog(&mut self) {
        let given_up = self.eng.waveform_reader.watchdog();
        if given_up.is_empty() {
            return;
        }
        let message = format!(
            "a waveform read did not finish in time and was given up on ({} so far)",
            self.eng.waveform_reader.abandoned()
        );
        self.eng.log(LogLevel::Error, message);
        for (doc, ticket) in given_up {
            if let Some(d) = self.docs.iter_mut().find(|d| d.id == doc) {
                d.waveforms.give_up(ticket);
            }
        }
    }

    /// The probe pool's watchdog: stuck workers replaced, their files reported unplayable in
    /// the doc that asked (from the next tick's `Availability`), volume notes logged once.
    fn probe_watchdog(&mut self) {
        let stuck = self.eng.pool.watchdog();
        if !stuck.is_empty() {
            let message = format!(
                "{} media probe(s) stuck; workers replaced ({} so far)",
                stuck.len(),
                self.eng.pool.abandoned_workers()
            );
            self.eng.log(LogLevel::Error, message);
            for s in stuck {
                if let Some(doc) = self.docs.iter_mut().find(|d| d.id == s.doc) {
                    let mut out = Vec::new();
                    doc.media.give_up(&s.key, s.ticket, &mut out);
                    doc.pending_media.extend(out);
                }
            }
        }
        for (volume, stalled) in self.eng.pool.take_volume_events() {
            // Not a "does not answer" warning: that notice is the media watch's alone.
            let message = if stalled {
                format!(
                    "probes of volume \"{volume}\" are held back: two files did not answer in \
                     time; one is tried at a time"
                )
            } else {
                format!("probes of volume \"{volume}\" run again: it answered")
            };
            self.eng.log(LogLevel::Info, message);
        }
        match self.eng.pool.take_worker_note() {
            Some(media::WorkerNote::SpawnFailed(e)) => self.eng.log(
                LogLevel::Error,
                format!(
                    "could not start a media probe worker ({e}); {} of {} running, retrying",
                    self.eng.pool.workers(),
                    media::PROBE_WORKERS
                ),
            ),
            Some(media::WorkerNote::Restored) => self.eng.log(
                LogLevel::Info,
                "media probe workers running again".to_owned(),
            ),
            None => {}
        }
        if self.eng.pool.take_limit_reached() {
            self.eng.log(
                LogLevel::Error,
                format!(
                    "{} probe workers were stuck and replaced; no more are replaced, so further \
                     stuck probes wait (their starts fail after the hold limit)",
                    media::MAX_ABANDONED_WORKERS
                ),
            );
        }
    }

    /// A mixer slot went back to the free list (a doc's voice ended on a retired slot, or a doc
    /// closed): docs with patches waiting for one sync again.
    fn hand_out_freed_slots(&mut self) {
        if !self.eng.patches.take_freed() {
            return;
        }
        self.each_doc(|cx| {
            if !cx.eng.patches.exhausted(cx.doc.id).is_empty() {
                let now = cx.now();
                for input in cx.sync_patches(false) {
                    cx.feed(input, now);
                }
            }
        });
    }

    /// Reports counters that must never rise (lost events, mixer faults, allocations on the
    /// audio thread (M3.21), dropped outbound messages) once for the engine, and lets each doc
    /// send again what its clients missed.
    fn health(&mut self) {
        let eng = &mut self.eng;
        let dropped = eng.mixer.dropped_events();
        if dropped > eng.seen.dropped_events {
            eng.seen.dropped_events = dropped;
            eng.log(
                LogLevel::Warn,
                format!("{dropped} mixer event(s) lost; voice ends are reconciled from meters"),
            );
        }
        let faults = eng.mixer.faults();
        if faults > eng.seen.faults {
            eng.seen.faults = faults;
            eng.log(
                LogLevel::Error,
                format!("audio engine fault: {faults} block(s) replaced with silence"),
            );
        }
        let faulted = eng.mixer.faulted_voices();
        if faulted > eng.seen.faulted_voices {
            eng.seen.faulted_voices = faulted;
            eng.log(
                LogLevel::Error,
                format!("audio engine fault: {faulted} voice(s) dropped after repeated faults"),
            );
        }
        // Reports counted at the previous check have been drained by now (the mixer counts a
        // fault just before it queues the report). Each report is logged as it is handled.
        let counted_before =
            std::mem::replace(&mut eng.seen.faulted_stages, eng.mixer.faulted_stages());
        let disabled_now = eng.mixer.disabled_units();
        let disabled_before = std::mem::replace(&mut eng.seen.disabled_units, disabled_now);
        self.lost_stage_reports(counted_before);
        self.lost_disabled_reports(disabled_before, disabled_now);
        let eng = &mut self.eng;
        let allocs = eng.backend.alloc_violations();
        if allocs > eng.seen.alloc_violations {
            eng.seen.alloc_violations = allocs;
            eng.log(
                LogLevel::Error,
                format!("real-time violation: {allocs} allocation(s) on the audio thread"),
            );
            self.mark_warnings_dirty();
        }
        self.each_doc(|cx| cx.resend_state());
        let eng = &mut self.eng;
        let out = eng.out.dropped() + eng.out.lossy_dropped();
        if out > eng.seen.outbound_dropped && eng.out.has_room() {
            let (seq, lossy) = (eng.out.dropped(), eng.out.lossy_dropped());
            eng.seen.outbound_dropped = out;
            eng.log(
                LogLevel::Warn,
                format!(
                    "clients were not keeping up: {seq} message(s) and {lossy} meter/progress \
                     update(s) dropped so far"
                ),
            );
        }
    }
}

/// The voice a mixer event is about (none for `PendingFull`).
fn event_voice(e: &Event) -> Option<VoiceId> {
    match e {
        Event::VoiceFinished { voice, .. }
        | Event::Underrun { voice, .. }
        | Event::VoiceRejected { voice, .. }
        | Event::LoopReached { voice, .. }
        | Event::SliceEnded { voice, .. }
        | Event::DevampScheduled { voice, .. }
        | Event::DevampIgnored { voice, .. }
        | Event::MoveEnded { voice, .. } => Some(*voice),
        Event::PendingFull { .. } | Event::StageFaulted { .. } => None,
    }
}

impl DocCx<'_> {
    /// This doc's part of a wake ([`Control::tick`]).
    fn tick(&mut self) {
        let now = self.now();
        self.flush_media(now);
        let mut fx = Effects::default();
        // The mixer's events of this doc's voices, routed here by the engine this wake.
        let mut events = std::mem::take(&mut self.doc.events);
        for e in events.drain(..) {
            self.doc.voices.event(e, &mut fx);
        }
        self.doc.events = events;
        let start_limit = seconds_to_samples(STREAM_START_LIMIT.as_secs_f64(), self.eng.rate);
        self.doc
            .voices
            .poll_streams(now, start_limit, &mut fx, self.doc.model.ws());
        let hold = seconds_to_samples(PROBE_HOLD_LIMIT.as_secs_f64(), self.eng.rate);
        self.doc.voices.expire_held(now, hold, &mut fx);
        self.follow(fx, now);
        self.reclaim_slots(now);
        self.doc.net.tick(now);
        self.note_lookup_limit();
        self.deliver_deferred(now);
        if self.doc.model.engine().is_due(now) {
            self.feed(Input::Tick, now);
        }
        // The playhead may stand on a cue whose stopped voice was still there at the playhead
        // move's priming pass (a Reset All, a stop then Set Playhead): now it is gone, the cue
        // is primed again before its GO.
        if self.doc.voices.take_freed() {
            self.doc.primes_dirty = true;
        }
        if self.doc.primes_dirty {
            self.doc.primes_dirty = false;
            self.prime(now);
        }
        self.reports();
        if self.doc.warnings_dirty {
            self.doc.warnings_dirty = false;
            self.refresh_warnings();
        }
        self.docs_tick();
    }

    /// A retired patch slot whose last voice has left is freed (zeroed), and a patch waiting
    /// for a slot gets it (docs/control.md "Patch slots").
    fn reclaim_slots(&mut self, now: SampleTime) {
        let doc = self.doc.id;
        if !self.eng.patches.has_retired(doc)
            || !self
                .eng
                .patches
                .reclaimable(doc, &self.doc.voices.slots_in_use())
        {
            return;
        }
        for input in self.sync_patches(false) {
            self.feed(input, now);
        }
    }

    /// One event of the shared media watch about a file of this doc ([`Control::watch_tick`]):
    /// a file gone is `Missing` in this tick's `Availability` (its cues are broken within the
    /// tick); a file back, changed or unreadable is probed again.
    fn watch_event(&mut self, event: WatchEvent) {
        match event {
            WatchEvent::Gone { key, id, .. } => {
                if let Some(status) = self.doc.media.mark_missing(&key, id) {
                    self.doc.pending_media.push(status);
                }
            }
            WatchEvent::Back { key, id, .. }
            | WatchEvent::Changed { key, id, .. }
            | WatchEvent::Unreadable { key, id, .. } => {
                self.doc.media.recheck(&key, id);
            }
        }
    }
}
