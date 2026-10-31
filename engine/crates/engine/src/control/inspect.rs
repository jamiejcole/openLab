// SPDX-License-Identifier: GPL-3.0-or-later

//! Read-only views of the control thread's state for the runner, the offline driver and tests:
//! the open docs ([`DocView`]), the first doc's model and engine, and cost and leak counters.

use openlab_audio::MixerController;
use openlab_core::protocol::{DocumentState, Snapshot, WireWarning};
use openlab_core::{CueEngine, CueId, MachineSettings, SampleTime, Workspace};

use super::devices::Device;
use super::watch::WatchBatch;
use super::{Control, Doc, DocId};

impl Control {
    /// The ids of the open docs, in open order.
    pub fn doc_ids(&self) -> Vec<DocId> {
        self.docs.iter().map(|d| d.id).collect()
    }

    /// A read-only view of doc `id`, if it is open.
    pub fn doc(&self, id: DocId) -> Option<DocView<'_>> {
        self.docs
            .iter()
            .find(|d| d.id == id)
            .map(|doc| DocView { doc })
    }

    /// The docs [`Self::open_doc`] installed since the last call, with the request each
    /// answered (the protocol's `opened { doc }` is M4.15f).
    pub fn take_opened(&mut self) -> Vec<(openlab_core::protocol::RequestId, DocId)> {
        std::mem::take(&mut self.eng.opened)
    }

    /// Mixer patch slots no doc holds.
    pub fn free_mixer_patches(&self) -> usize {
        self.eng.patches.free_slots()
    }

    /// Probe workers running: one pool for the engine, whatever the number of docs.
    pub fn probe_workers(&self) -> usize {
        self.eng.pool.workers()
    }

    /// Files the media watch stats (a file several docs use counts once).
    pub fn watched_files(&self) -> usize {
        self.eng.watch.watched_files()
    }

    /// Voice ids whose owner is recorded (a leak measure: it follows the voices alive).
    pub fn voice_owners(&self) -> usize {
        self.eng.voice_ids.recorded()
    }

    /// The first open doc: what the single-workspace accessors below show.
    fn first(&self) -> Option<&Doc> {
        self.docs.first()
    }

    /// Everything a client needs to draw the show (docs/protocol.md, Snapshot), of the first
    /// open doc (an empty workspace when none is open).
    pub fn snapshot(&self) -> Snapshot {
        match self.first() {
            Some(doc) => doc.snapshot(),
            None => Snapshot {
                revision: 0,
                runtime_seq: 0,
                document: DocumentState {
                    path: None,
                    dirty: false,
                    can_undo: false,
                    can_redo: false,
                    show_mode: false,
                },
                workspace: self.none.ws.clone(),
                cue_states: Vec::new(),
                problems: Vec::new(),
                group_warnings: Vec::new(),
                playheads: Vec::new(),
                previewing: Vec::new(),
                warnings: Vec::new(),
                media_durations: Vec::new(),
            },
        }
    }

    /// One media-watch pass on this thread, in manual mode (tests): its batch, if any,
    /// to hand to [`Self::handle`] as `Inbox::Watch`. `None` in thread mode or when the pass
    /// found nothing.
    pub fn watch_pass(&mut self) -> Option<WatchBatch> {
        self.follow_key_changes();
        self.eng.watch.watch_mut().manual_pass()
    }

    /// Watch batches of the current generation taken so far (a cost measure).
    pub fn watch_batches(&self) -> u64 {
        self.eng.watch.watch().batches()
    }

    /// Media watchers abandoned in a stalled stat.
    pub fn abandoned_watchers(&self) -> usize {
        self.eng.watch.watch().abandoned()
    }

    /// Slice plans built for the `bad_plan` warnings so far (a cost measure: an edit rebuilds
    /// only the cues it changed), in the first doc.
    pub fn plan_builds(&self) -> u64 {
        self.first().map_or(0, |d| d.checks.builds())
    }

    /// Cues the `bad_plan` checks have looked at so far (a cost measure: an edit looks at the
    /// cues it touched, M4.12c), in the first doc.
    pub fn plan_cues_examined(&self) -> u64 {
        self.first().map_or(0, |d| d.checks.examined())
    }

    /// Walks of every audio cue the `bad_plan` checks made so far (a newly followed workspace;
    /// an edit or a probe answer makes none, M4.12c), in the first doc.
    pub fn plan_walks(&self) -> u64 {
        self.first().map_or(0, |d| d.checks.walks())
    }

    /// Edits whose `model_changed` walked the workspace instead of reading the cue index (a
    /// cost measure; none for a valid workspace, M4.12c), in the first doc.
    pub fn model_change_walks(&self) -> u64 {
        self.first().map_or(0, |d| d.model_change_walks)
    }

    /// Network look-ups under way plus messages queued or being sent, every doc's (the offline
    /// driver waits for zero).
    pub fn net_busy(&self) -> usize {
        self.docs.iter().map(|d| d.net.busy()).sum()
    }

    /// PB-10: whether the network output override is on (one flag for every doc).
    pub fn network_output_disabled(&self) -> bool {
        self.eng.network_disabled()
    }

    /// The first open doc's workspace.
    pub fn workspace(&self) -> &Workspace {
        self.first().map_or(&self.none.ws, |d| d.model.ws())
    }

    /// The first open doc's cue engine.
    pub fn engine(&self) -> &CueEngine {
        self.first().map_or(&self.none.engine, |d| d.model.engine())
    }

    /// The machine settings in force, including mappings the engine added (M4.4c).
    pub fn machine(&self) -> &MachineSettings {
        &self.eng.config.machine
    }

    pub fn mixer(&self) -> &MixerController {
        &self.eng.mixer
    }

    /// The first open doc's revision.
    pub fn revision(&self) -> u64 {
        self.first().map_or(0, |d| d.revision)
    }

    /// Voice records alive in every doc (none once a show has stopped and every voice has
    /// ended).
    pub fn live_voices(&self) -> usize {
        self.docs.iter().map(|d| d.voices.live()).sum()
    }

    /// Stream handles the loop holds (voices, loads and primes), every doc's.
    pub fn held_streams(&self) -> usize {
        self.docs.iter().map(|d| d.voices.streams()).sum()
    }

    /// Streams the disk workers have open.
    pub fn open_streams(&self) -> usize {
        self.eng.streamer.open_streams()
    }

    /// The volume the disk streamer files a stream's open under (its per-volume opener cap).
    pub fn stream_volume(&self, path: &std::path::Path) -> Option<String> {
        self.eng.streamer.config().volume_of.map(|f| f(path))
    }

    /// Starts that used a primed stream (not a cold start), in the first doc.
    pub fn prime_hits(&self) -> u64 {
        self.first().map_or(0, |d| d.voices.prime_hits())
    }

    /// Whether the first doc's primed stream of the cue reads preloaded samples.
    pub fn primed_from_memory(&self, cue: CueId) -> bool {
        self.first()
            .is_some_and(|d| d.voices.primed_from_memory(cue))
    }

    /// Cues of the first doc with a primed stream.
    pub fn primed(&self) -> Vec<CueId> {
        let mut v: Vec<CueId> = self
            .first()
            .map(|d| d.voices.primed().collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    /// Probes still running, every doc's.
    pub fn probes_pending(&self) -> usize {
        self.docs.iter().map(|d| d.media.pending()).sum()
    }

    /// Preload jobs started and not answered yet, every doc's.
    pub fn preloads_pending(&self) -> usize {
        self.docs.iter().map(|d| d.media.preloads_pending()).sum()
    }

    /// Probe workers replaced because they were stuck (the engine's pool).
    pub fn abandoned_probe_workers(&self) -> usize {
        self.eng.pool.abandoned_workers()
    }

    /// Cues the first doc's media catalog has read from the workspace (see
    /// `MediaCatalog::cues_read`).
    pub fn media_cues_read(&self) -> u64 {
        self.first().map_or(0, |d| d.media.cues_read())
    }

    /// Memory taken from the first doc's preload budget.
    pub fn preload_reserved(&self) -> u64 {
        self.first().map_or(0, |d| d.media.preload_reserved())
    }

    /// Device-thread jobs not answered yet (a test driver waits for them; none offline).
    pub fn device_jobs_pending(&self) -> usize {
        self.eng.backend.device().map_or(0, Device::jobs_in_flight)
    }

    /// The device is down: an outage, sample time standing still.
    pub fn device_down(&self) -> bool {
        self.eng.backend.is_down()
    }

    /// A device list read is under way (a test driver waits for it).
    pub fn device_listing(&self) -> bool {
        self.eng.backend.device().is_some_and(Device::is_listing)
    }

    /// Frames per block the loop schedules with (follows the open stream).
    pub fn block_frames(&self) -> u64 {
        self.eng.block
    }

    /// Feedback waiting for its sample, every doc's.
    pub fn deferred(&self) -> usize {
        self.docs.iter().map(|d| d.deferred.len()).sum()
    }

    /// Voices the mixer is playing or holding paused, from a voice-meter read (diagnostics and
    /// leak tests; the read takes the peaks the next meters message would have had).
    pub fn mixer_voice_count(&mut self) -> usize {
        self.eng
            .mixer
            .read_voice_meters_into(&mut self.eng.meter_ids, &mut self.eng.meter_peaks)
    }

    /// Playing streams of every doc and the frames each must have written before rendered
    /// sample `t` can be rendered without an underrun (offline driver).
    pub fn stream_needs(&self, t: SampleTime) -> Vec<(&openlab_audio::StreamHandle, u64)> {
        self.docs
            .iter()
            .flat_map(|d| d.voices.stream_needs(t))
            .collect()
    }

    /// Streams prepared and not playing yet, every doc's: loads and primes (offline driver).
    pub fn prepared_streams(&self) -> Vec<&openlab_audio::StreamHandle> {
        self.docs
            .iter()
            .flat_map(|d| d.voices.waiting_streams())
            .collect()
    }

    /// No voice, alias or deferred report is left in any doc.
    pub fn is_idle(&self) -> bool {
        self.docs
            .iter()
            .all(|d| d.voices.is_idle() && d.deferred.is_empty())
    }
}

/// What tests (and later the protocol's bound connections) can read of one open doc.
pub struct DocView<'a> {
    doc: &'a Doc,
}

impl<'a> DocView<'a> {
    pub fn id(&self) -> DocId {
        self.doc.id
    }

    pub fn workspace(&self) -> &'a Workspace {
        self.doc.model.ws()
    }

    pub fn engine(&self) -> &'a CueEngine {
        self.doc.model.engine()
    }

    pub fn snapshot(&self) -> Snapshot {
        self.doc.snapshot()
    }

    pub fn revision(&self) -> u64 {
        self.doc.revision
    }

    pub fn runtime_seq(&self) -> u64 {
        self.doc.runtime_seq
    }

    pub fn is_unsaved(&self) -> bool {
        self.doc.revision != self.doc.saved_revision
    }

    pub fn show_path(&self) -> Option<&'a std::path::Path> {
        self.doc.show_path.as_deref()
    }

    /// Voice records alive.
    pub fn live_voices(&self) -> usize {
        self.doc.voices.live()
    }

    /// Mixer events routed to this doc so far.
    pub fn events_routed(&self) -> u64 {
        self.doc.events_routed
    }

    /// No voice, alias or deferred report is left.
    pub fn is_idle(&self) -> bool {
        self.doc.voices.is_idle() && self.doc.deferred.is_empty()
    }

    /// Probes still running.
    pub fn probes_pending(&self) -> usize {
        self.doc.media.pending()
    }

    /// The workspace-level notices in force.
    pub fn warnings(&self) -> &'a [WireWarning] {
        &self.doc.warnings
    }

    /// Cues with a primed stream.
    pub fn primed(&self) -> Vec<CueId> {
        let mut v: Vec<CueId> = self.doc.voices.primed().collect();
        v.sort();
        v
    }
}
