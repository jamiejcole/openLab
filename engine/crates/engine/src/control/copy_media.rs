// SPDX-License-Identifier: GPL-3.0-or-later

//! Copying a show's media into its show folder (WS-04): as files are set on cues with the
//! workspace's `copy_media_on_add` on, and on demand (`copy_media`). One copy runs at a time,
//! engine-wide, on a thread of its own; the control thread only plans it from the model, reads
//! its progress counters and, when every file is done, points the cues at their copies as one
//! undo step. Nothing here touches a file on the control thread.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use openlab_core::protocol::{
    ErrorCode, LogLevel, MAX_COPY_FAILURES_LISTED, MediaCopyFailure, MediaCopyReason,
    MediaCopyReport, RequestId, ServerMessage,
};
use openlab_core::{
    CopyItem, CopyMode, CueId, MediaRef, PathStyle, copy_items, portable_ref, retarget_command,
};

use super::inbox::reply;
use super::{Control, DocCx, DocId, Inbox, ReplyTx, fail, ok};
use crate::persist::{
    Copied, CopyError, CopyFailure, CopyHooks, CopyPacing, CopyProgress, CopyWork, FolderIndexes,
    copy_into, media_path,
};

/// Least time between two `media_copy_progress` messages of a copy.
pub const COPY_PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// How the copies run (the engine's defaults; tests pace them as they need).
#[derive(Clone, Debug, Default)]
pub struct CopyConfig {
    pub pacing: CopyPacing,
    pub hooks: CopyHooks,
}

/// A copy asked for and not started yet.
struct Job {
    doc: DocId,
    mode: CopyMode,
    /// `Added`: the cues whose files were set. Unused on demand.
    cues: Vec<CueId>,
}

/// One file the copy's thread works on: the plan's item `index`.
struct WorkItem {
    index: usize,
    source: PathBuf,
    folder: &'static str,
    always_new: bool,
}

/// What a copy's thread found, file by file (the plan's item index and its outcome).
pub struct CopyAnswer {
    ticket: u64,
    results: Vec<(usize, Result<Done, CopyError>)>,
}

/// A file done with on the copy's thread: where it went, and that place as the show file would
/// store it from the job's show folder (worked out there, not on the control thread).
struct Done {
    copied: Copied,
    stored: Option<MediaRef>,
}

impl std::fmt::Debug for CopyAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "CopyAnswer({}, {} files)",
            self.ticket,
            self.results.len()
        )
    }
}

/// How an item of the plan ended.
enum Outcome {
    Copied(PathBuf, Option<MediaRef>),
    Reused(PathBuf, Option<MediaRef>),
    /// Already inside the show folder (on demand): left as it is.
    InFolder,
    Failed(MediaCopyReason, String),
}

/// Whether a copy's thread or the stall check had the last word: exactly one of them does, so
/// the count of stuck threads is right whichever comes first.
#[derive(Debug, Default)]
struct Handover(AtomicU8);

const WORKING: u8 = 0;
const DONE: u8 = 1;
const ABANDONED: u8 = 2;

impl Handover {
    /// The thread is done: `false` if the stall check gave up on it first (it then takes its
    /// count of stuck threads back and sends nothing).
    fn finish(&self, stuck: &AtomicUsize) -> bool {
        let done = self
            .0
            .compare_exchange(WORKING, DONE, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        if !done {
            stuck.fetch_sub(1, Ordering::SeqCst);
        }
        done
    }

    /// The stall check gives up on the thread: `false` when the thread has just finished (its
    /// answer is on its way) and nothing is counted. The count goes up before the swap, so the
    /// thread's take-back can never come first.
    fn abandon(&self, stuck: &AtomicUsize) -> bool {
        stuck.fetch_add(1, Ordering::SeqCst);
        let abandoned = self
            .0
            .compare_exchange(WORKING, ABANDONED, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        if !abandoned {
            stuck.fetch_sub(1, Ordering::SeqCst);
        }
        abandoned
    }
}

/// The copy under way.
struct Running {
    ticket: u64,
    doc: DocId,
    mode: CopyMode,
    /// The show folder the copies go into.
    show_dir: PathBuf,
    items: Vec<CopyItem>,
    /// Outcomes known before the thread started (a file in the folder, one that cannot be
    /// reached), by item index.
    settled: Vec<(usize, Outcome)>,
    /// The cues whose files the thread copies (the copying state the clients show).
    cues: Vec<CueId>,
    files_total: u64,
    progress: Arc<CopyProgress>,
    /// Who the copy's end belongs to ([`Handover`]).
    handover: Arc<Handover>,
    last_beats: u64,
    last_beat_at: Instant,
    last_sent: Option<Instant>,
}

/// The copies of every doc: the one running and those waiting.
pub(super) struct Copies {
    inbox: Sender<Inbox>,
    running: Option<Running>,
    queue: VecDeque<Job>,
    ticket: u64,
    /// Copy threads given up on (stuck on a volume) that have not returned.
    stuck: Arc<AtomicUsize>,
    /// `media_copied` reports that did not fit the outbound channel: sent at a later wake.
    unsent: Vec<(DocId, ServerMessage)>,
}

impl Copies {
    pub(super) fn new(inbox: Sender<Inbox>) -> Self {
        Self {
            inbox,
            running: None,
            queue: VecDeque::new(),
            ticket: 0,
            stuck: Arc::new(AtomicUsize::new(0)),
            unsent: Vec::new(),
        }
    }

    /// Queues the copy of the files just set on `cues` of `doc`: joined to a copy of added
    /// files already waiting for that doc, so a drag of a hundred files is one copy.
    pub(super) fn add(&mut self, doc: DocId, cues: Vec<CueId>) {
        if cues.is_empty() {
            return;
        }
        if let Some(job) = self
            .queue
            .iter_mut()
            .find(|j| j.doc == doc && j.mode == CopyMode::Added)
        {
            for cue in cues {
                if !job.cues.contains(&cue) {
                    job.cues.push(cue);
                }
            }
            return;
        }
        self.queue.push_back(Job {
            doc,
            mode: CopyMode::Added,
            cues,
        });
    }

    /// Whether `doc` has the on-demand copy running or waiting.
    fn on_demand_of(&self, doc: DocId) -> bool {
        self.running
            .as_ref()
            .is_some_and(|r| r.doc == doc && r.mode == CopyMode::OnDemand)
            || self
                .queue
                .iter()
                .any(|j| j.doc == doc && j.mode == CopyMode::OnDemand)
    }

    /// Stops `doc`'s copy and drops its waiting ones (the doc closed or went into Show Mode).
    /// The running copy's thread stops at its next chunk and removes its partial file; its
    /// answer then reports the copy stopped and retargets nothing.
    pub(super) fn stop_doc(&mut self, doc: DocId) {
        self.queue.retain(|j| j.doc != doc);
        if let Some(r) = self.running.as_ref().filter(|r| r.doc == doc) {
            r.progress.cancel();
        }
    }

    /// Copies running or waiting (tests wait until it is zero).
    pub(super) fn busy(&self) -> usize {
        usize::from(self.running.is_some()) + self.queue.len()
    }

    /// Copy threads given up on and still stuck.
    pub(super) fn stuck(&self) -> usize {
        self.stuck.load(Ordering::SeqCst)
    }

    /// Shutdown: the running copy stops at its next chunk; nobody waits for it.
    pub(super) fn stop_all(&mut self) {
        self.queue.clear();
        if let Some(r) = &self.running {
            r.progress.cancel();
        }
    }
}

impl Control {
    /// `copy_media` of doc `doc`: queued, and answered at once.
    pub(super) fn copy_media(&mut self, doc: DocId, id: RequestId, to: &ReplyTx) {
        let Some(d) = self.docs.iter().find(|d| d.id == doc) else {
            fail(to, id, ErrorCode::NoWorkspace, "no workspace is open");
            return;
        };
        let refusal = if d.show_path.is_none() {
            Some("the workspace has never been saved: it has no show folder yet")
        } else if d.show_mode {
            Some("media cannot be copied in Show Mode")
        } else if self.eng.copies.on_demand_of(doc) {
            Some("media is already being copied into the show folder")
        } else {
            None
        };
        if let Some(message) = refusal {
            fail(to, id, ErrorCode::CommandRefused, message);
            return;
        }
        self.eng.copies.queue.push_back(Job {
            doc,
            mode: CopyMode::OnDemand,
            cues: Vec::new(),
        });
        reply(to, ok(id, None));
        self.copy_tick();
    }

    /// Each wake: reports sent late, the running copy's progress and stall check, and the next
    /// copy started once none runs.
    pub(super) fn copy_tick(&mut self) {
        let unsent = std::mem::take(&mut self.eng.copies.unsent);
        for (doc, message) in unsent {
            if !self.eng.out.send_state(Some(doc), message.clone()) {
                self.eng.copies.unsent.push((doc, message));
            }
        }
        if self.eng.copies.running.is_some() {
            self.watch_copy();
        }
        while self.eng.copies.running.is_none() {
            let Some(job) = self.eng.copies.queue.pop_front() else {
                break;
            };
            self.start_copy(job);
        }
    }

    /// Sends the running copy's progress when due, and gives up on a thread that has made no
    /// progress for the document timeout (a volume that stopped answering): its copy is
    /// reported stopped, its files not yet done as not responding, and nothing is retargeted.
    fn watch_copy(&mut self) {
        let now = self.eng.config.stall_clock.now();
        let timeout = self.eng.config.doc_timeout;
        let Some(r) = self.eng.copies.running.as_mut() else {
            return;
        };
        let beats = r.progress.beats();
        if beats != r.last_beats {
            r.last_beats = beats;
            r.last_beat_at = now;
        } else if now.saturating_duration_since(r.last_beat_at) >= timeout {
            if !r.handover.abandon(&self.eng.copies.stuck) {
                // Its answer is in the inbox: the copy ends with it.
                return;
            }
            r.progress.cancel();
            if let Some(r) = self.eng.copies.running.take() {
                let done = r.progress.files_done();
                self.eng.log(
                    LogLevel::Error,
                    format!(
                        "copying media into the show folder made no progress for {} s; its thread was abandoned",
                        timeout.as_secs()
                    ),
                );
                let results = Vec::new();
                self.finish_copy(r, results, Some(done));
            }
            return;
        }
        let due = r
            .last_sent
            .is_none_or(|at| now.saturating_duration_since(at) >= COPY_PROGRESS_EVERY);
        if due {
            r.last_sent = Some(now);
            let message = progress_message(r);
            let doc = r.doc;
            self.eng.out.send_lossy(Some(doc), message);
        }
    }

    /// Plans `job` from its doc's workspace as it is now and starts its thread. A copy with
    /// nothing to write is reported at once.
    fn start_copy(&mut self, job: Job) {
        let Some(d) = self.docs.iter().find(|d| d.id == job.doc) else {
            return;
        };
        let (Some(show_dir), false) = (d.show_path.as_deref().and_then(Path::parent), d.show_mode)
        else {
            return;
        };
        let items = copy_items(d.model.ws(), job.mode, &job.cues);
        let mut settled = Vec::new();
        let mut work = Vec::new();
        let mut cues = Vec::new();
        for (index, item) in items.iter().enumerate() {
            if job.mode == CopyMode::OnDemand && item.in_folder {
                settled.push((index, Outcome::InFolder));
                continue;
            }
            let Some(source) = media_path(&item.media, Some(show_dir)) else {
                settled.push((
                    index,
                    Outcome::Failed(MediaCopyReason::Missing, "the file cannot be found".into()),
                ));
                continue;
            };
            if d.media.volume_stalled(&source) {
                settled.push((
                    index,
                    Outcome::Failed(
                        MediaCopyReason::NotResponding,
                        "the file's volume is not responding".into(),
                    ),
                ));
                continue;
            }
            if !cues.contains(&item.cue) {
                cues.push(item.cue);
            }
            work.push(WorkItem {
                index,
                source,
                folder: item.folder,
                always_new: job.mode == CopyMode::Added && item.in_folder,
            });
        }
        let show_dir = show_dir.to_path_buf();
        self.eng.copies.ticket += 1;
        let now = self.eng.config.stall_clock.now();
        let running = Running {
            ticket: self.eng.copies.ticket,
            doc: job.doc,
            mode: job.mode,
            show_dir: show_dir.clone(),
            items,
            settled,
            cues,
            files_total: work.len() as u64,
            progress: Arc::new(CopyProgress::default()),
            handover: Arc::new(Handover::default()),
            last_beats: 0,
            last_beat_at: now,
            last_sent: None,
        };
        if work.is_empty() {
            self.finish_copy(running, Vec::new(), None);
            return;
        }
        let spawned = spawn_copy(
            running.ticket,
            work,
            show_dir,
            &running.progress,
            &running.handover,
            &self.eng.copies,
            &self.eng.config.copy,
        );
        match spawned {
            Ok(()) => {
                let message = progress_message(&running);
                self.eng.out.send_lossy(Some(running.doc), message);
                self.eng.copies.running = Some(Running {
                    last_sent: Some(now),
                    ..running
                });
            }
            Err(e) => {
                self.eng.log(
                    LogLevel::Error,
                    format!("could not start copying media into the show folder: {e}"),
                );
                let doc = running.doc;
                let report = MediaCopyReport {
                    on_demand: running.mode == CopyMode::OnDemand,
                    copied: 0,
                    reused: 0,
                    in_folder: 0,
                    failed: Vec::new(),
                    failed_count: 0,
                    cancelled: true,
                };
                self.send_report(doc, report);
            }
        }
    }

    /// The copy's thread is done: its doc's cues are pointed at their copies.
    pub(super) fn copy_answer(&mut self, answer: CopyAnswer) {
        let Some(running) = self
            .eng
            .copies
            .running
            .take_if(|r| r.ticket == answer.ticket)
        else {
            // A copy given up on: its late answer changes nothing.
            return;
        };
        self.finish_copy(running, answer.results, None);
        self.copy_tick();
    }

    /// Reports `running` and, unless it was stopped, retargets its cues as one undo step.
    /// `abandoned_after`: the thread was given up on after this many files.
    fn finish_copy(
        &mut self,
        running: Running,
        results: Vec<(usize, Result<Done, CopyError>)>,
        abandoned_after: Option<u64>,
    ) {
        let Running {
            doc,
            mode,
            show_dir,
            items,
            mut settled,
            ..
        } = running;
        let mut cancelled = abandoned_after.is_some();
        for (index, result) in results {
            settled.push((
                index,
                match result {
                    Ok(Done {
                        copied: Copied::New(path),
                        stored,
                    }) => Outcome::Copied(path, stored),
                    Ok(Done {
                        copied: Copied::Reused(path),
                        stored,
                    }) => Outcome::Reused(path, stored),
                    Err(e) => {
                        if e.failure == CopyFailure::Cancelled {
                            cancelled = true;
                        }
                        Outcome::Failed(reason_of(e.failure), e.message)
                    }
                },
            ));
        }
        if abandoned_after.is_some() {
            // Files the thread never reported: their volume stopped answering.
            let mut known = vec![false; items.len()];
            for (i, _) in &settled {
                if let Some(k) = known.get_mut(*i) {
                    *k = true;
                }
            }
            for (index, known) in known.into_iter().enumerate() {
                if !known {
                    settled.push((
                        index,
                        Outcome::Failed(
                            MediaCopyReason::NotResponding,
                            "a volume stopped answering while the files were copied".into(),
                        ),
                    ));
                }
            }
        }
        settled.sort_by_key(|(i, _)| *i);
        let Some(mut cx) = self.cx_of(doc) else {
            return;
        };
        // Show Mode began (or the doc moved) since: nothing is retargeted.
        if cx.doc.show_mode {
            cancelled = true;
        }
        let report = cx.apply_copies(mode, &show_dir, &items, &settled, cancelled);
        self.send_report(doc, report);
    }

    fn send_report(&mut self, doc: DocId, report: MediaCopyReport) {
        let message = ServerMessage::MediaCopied {
            report: Box::new(report),
        };
        if !self.eng.out.send_state(Some(doc), message.clone()) {
            self.eng.copies.unsent.push((doc, message));
        }
    }
}

impl DocCx<'_> {
    /// After a client's command that changed the workspace: queues the copy of the files it
    /// set on cues, when the workspace copies on add, has a show folder and is not in Show Mode.
    pub(super) fn copy_on_add(&mut self, cues: Vec<CueId>) {
        if cues.is_empty()
            || !self.doc.model.ws().settings.copy_media_on_add
            || self.doc.show_path.is_none()
            || self.doc.show_mode
        {
            return;
        }
        self.eng.copies.add(self.doc.id, cues);
    }

    /// Points the cues whose file was copied or found at their copies, as one undo step, and
    /// counts the files. A cue whose file changed since the copy was planned keeps the new one
    /// (the edit wins). A stopped copy retargets nothing.
    fn apply_copies(
        &mut self,
        mode: CopyMode,
        job_dir: &Path,
        items: &[CopyItem],
        settled: &[(usize, Outcome)],
        cancelled: bool,
    ) -> MediaCopyReport {
        let show_dir = self.doc.show_path.as_deref().and_then(Path::parent);
        // A Save As into another folder since the copy began: the stored forms the thread worked
        // out are for the old folder, and are worked out again here.
        let moved = show_dir != Some(job_dir);
        let show_dir = show_dir.and_then(Path::to_str).map(str::to_owned);
        // Each cue's file now, from one walk of the show (a copy may name thousands of cues).
        let current: std::collections::HashMap<CueId, MediaRef> =
            openlab_core::media_of_cues(self.doc.model.ws())
                .into_iter()
                .collect();
        let mut retargets: Vec<(CueId, MediaRef)> = Vec::new();
        let mut copied = 0u64;
        let mut reused: std::collections::HashSet<&Path> = std::collections::HashSet::new();
        let mut in_folder: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut failed = Vec::new();
        let mut failed_count = 0u64;
        for (index, outcome) in settled {
            let Some(item) = items.get(*index) else {
                continue;
            };
            let target = match outcome {
                Outcome::Copied(path, stored) => {
                    copied += 1;
                    Some((path, stored))
                }
                Outcome::Reused(path, stored) => {
                    reused.insert(path.as_path());
                    Some((path, stored))
                }
                Outcome::InFolder => {
                    in_folder.insert(item.media.as_str());
                    None
                }
                Outcome::Failed(reason, message) => {
                    failed_count += 1;
                    if failed.len() < MAX_COPY_FAILURES_LISTED {
                        failed.push(MediaCopyFailure {
                            cue: item.cue,
                            file: item.media.as_str().to_owned(),
                            reason: *reason,
                            message: message.clone(),
                        });
                    }
                    None
                }
            };
            let Some((path, stored)) = target else {
                continue;
            };
            if current.get(&item.cue) != Some(&item.media) {
                continue;
            }
            let stored = match stored {
                Some(stored) if !moved => Some(stored.clone()),
                _ => path
                    .to_str()
                    .map(|p| portable_ref(p, show_dir.as_deref(), PathStyle::host())),
            };
            if let Some(stored) = stored {
                retargets.push((item.cue, stored));
            }
        }
        if !cancelled && !retargets.is_empty() {
            let now = self.now();
            let command = retarget_command(self.doc.model.ws(), &retargets);
            let footprint = command.footprint();
            let mut edit = self.doc.model.edit();
            let result = self.doc.history.execute(&mut edit, command);
            edit.ended(&footprint, &result);
            match result {
                Ok(true) => self.edited(now, &footprint),
                Ok(false) => {}
                Err(e) => {
                    self.partly_changed(now, &e);
                    self.log(
                        LogLevel::Error,
                        format!("pointing cues at their copies failed: {e}"),
                    );
                }
            }
        }
        let report = MediaCopyReport {
            on_demand: mode == CopyMode::OnDemand,
            copied,
            reused: reused.len() as u64,
            in_folder: in_folder.len() as u64,
            failed,
            failed_count,
            cancelled,
        };
        self.log(
            if failed_count > 0 {
                LogLevel::Warn
            } else {
                LogLevel::Info
            },
            format!(
                "copied media into the show folder: {} new, {} already there, {} in the folder, {} failed{}",
                report.copied,
                report.reused,
                report.in_folder,
                report.failed_count,
                if cancelled { " (stopped)" } else { "" }
            ),
        );
        report
    }
}

fn progress_message(r: &Running) -> ServerMessage {
    ServerMessage::MediaCopyProgress {
        cues: r.cues.clone(),
        files_done: r.progress.files_done().min(r.files_total),
        files_total: r.files_total,
        bytes_done: r.progress.bytes_done(),
        bytes_total: r.progress.bytes_total(),
        on_demand: r.mode == CopyMode::OnDemand,
    }
}

fn reason_of(failure: CopyFailure) -> MediaCopyReason {
    match failure {
        CopyFailure::Missing => MediaCopyReason::Missing,
        CopyFailure::Unreadable => MediaCopyReason::Unreadable,
        CopyFailure::DiskFull => MediaCopyReason::DiskFull,
        CopyFailure::ReadOnly => MediaCopyReason::ReadOnly,
        CopyFailure::BadName => MediaCopyReason::BadName,
        CopyFailure::Cancelled | CopyFailure::Other => MediaCopyReason::Other,
    }
}

/// Starts the copy's thread. Never joined: one stuck on a volume that stopped answering ends
/// when its call returns, and its answer (a ticket given up on by then) is ignored.
fn spawn_copy(
    ticket: u64,
    work: Vec<WorkItem>,
    show_dir: PathBuf,
    progress: &Arc<CopyProgress>,
    handover: &Arc<Handover>,
    copies: &Copies,
    config: &CopyConfig,
) -> std::io::Result<()> {
    let (progress, handover, inbox, stuck) = (
        Arc::clone(progress),
        Arc::clone(handover),
        copies.inbox.clone(),
        Arc::clone(&copies.stuck),
    );
    let (pacing, hooks) = (config.pacing, config.hooks.clone());
    thread::Builder::new()
        .name("openlab-copy-media".into())
        .spawn(move || {
            let total = work
                .iter()
                .filter_map(|w| std::fs::metadata(&w.source).ok())
                .map(|m| m.len())
                .fold(0u64, u64::saturating_add);
            progress.set_bytes_total(total);
            let mut buf = Vec::new();
            let mut indexes = FolderIndexes::default();
            let mut results = Vec::with_capacity(work.len());
            for item in work {
                let mut ctx = CopyWork {
                    progress: &progress,
                    pacing,
                    hooks: &hooks,
                    buf: &mut buf,
                    indexes: &mut indexes,
                };
                let result = copy_into(
                    &item.source,
                    &show_dir,
                    item.folder,
                    item.always_new,
                    &mut ctx,
                )
                .map(|copied| {
                    let stored = match &copied {
                        Copied::New(p) | Copied::Reused(p) => p
                            .to_str()
                            .map(|p| portable_ref(p, show_dir.to_str(), PathStyle::host())),
                    };
                    Done { copied, stored }
                });
                results.push((item.index, result));
                progress.file_done();
            }
            if !handover.finish(&stuck) {
                return;
            }
            let _ = inbox.send(Inbox::Copy(CopyAnswer { ticket, results }));
        })
        .map(|_| ())
}
