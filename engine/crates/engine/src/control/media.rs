// SPDX-License-Identifier: GPL-3.0-or-later

//! What the control thread knows about each media file the workspace uses, and the probe workers
//! that find it out. Probing decodes a whole compressed file (1–2 s for 30 minutes, docs/audio.md),
//! and preloading decodes it into memory, so both run on these workers and never on the control
//! thread (ARCHITECTURE §3.1): the control thread sends a job and gets the answer back through
//! its inbox.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{Receiver, Sender};
use openlab_audio::{Media, MediaError, Preloaded, Probed, preload_within, probe};
use openlab_core::{MediaRef, MediaStatus, Workspace, audio_media};

use crate::persist::media_path;

use super::inbox::{Inbox, ProbeAnswer};

/// Probe workers started per control loop. Probes of different files run side by side so one
/// long file does not hold up the rest of a workspace.
pub const PROBE_WORKERS: usize = 2;

/// A job for a probe worker.
#[derive(Debug)]
enum Job {
    Probe {
        key: String,
        ticket: u64,
        path: PathBuf,
    },
    Preload {
        key: String,
        ticket: u64,
        path: PathBuf,
        max_bytes: u64,
    },
}

/// One media file's state on the control thread.
#[derive(Debug)]
pub enum Probe {
    /// Asked; no answer yet.
    Pending,
    /// Playable (possibly damaged: see `probed.warnings`).
    Ready(Arc<Ready>),
    /// Missing or unplayable.
    Failed,
}

#[derive(Debug)]
pub struct Ready {
    pub path: PathBuf,
    pub probed: Probed,
}

#[derive(Debug)]
struct Entry {
    probe: Probe,
    /// The answer that counts: earlier jobs for this key are stale.
    ticket: u64,
    /// Decoded into memory (AUD-05), once the preload job has answered.
    preloaded: Option<Arc<Preloaded>>,
    /// Bytes taken from the workspace preload budget for this file.
    reserved: u64,
    /// The file is too big for what is left of the budget: it streams.
    over_budget: bool,
}

/// Probe workers and the catalog of media the workspace uses.
pub struct MediaCatalog {
    jobs: Sender<Job>,
    entries: HashMap<String, Entry>,
    next_ticket: u64,
    show_dir: Option<PathBuf>,
    budget: u64,
    reserved: u64,
}

impl MediaCatalog {
    /// Starts the probe workers; they answer through `inbox`.
    pub fn new(
        inbox: Sender<Inbox>,
        show_dir: Option<PathBuf>,
        preload_budget: u64,
    ) -> std::io::Result<Self> {
        let (jobs, rx) = crossbeam_channel::unbounded::<Job>();
        for n in 0..PROBE_WORKERS {
            let rx = rx.clone();
            let inbox = inbox.clone();
            // Never joined: a worker stuck in a read on a stalled disk must not hang shutdown.
            // It ends once the catalog (the jobs sender) is gone and its current job is done.
            thread::Builder::new()
                .name(format!("openlab-probe-{n}"))
                .spawn(move || worker(&rx, &inbox))?;
        }
        Ok(Self {
            jobs,
            entries: HashMap::new(),
            next_ticket: 0,
            show_dir,
            budget: preload_budget,
            reserved: 0,
        })
    }

    /// Follows the workspace: probes media references it has not seen and forgets the ones no
    /// cue uses any more (releasing their preload). Returns whether anything was forgotten.
    pub fn sync(&mut self, ws: &Workspace) -> bool {
        let used: HashSet<String> = audio_media(ws)
            .into_iter()
            .map(|(_, m)| m.as_str().to_owned())
            .collect();
        let before = self.entries.len();
        let mut freed = 0;
        self.entries.retain(|k, e| {
            let keep = used.contains(k);
            if !keep {
                freed += e.reserved;
            }
            keep
        });
        self.reserved = self.reserved.saturating_sub(freed);
        let forgot = self.entries.len() != before;
        for key in used {
            if !self.entries.contains_key(&key) {
                self.request(key);
            }
        }
        forgot
    }

    /// Probes `key` again (a stream of it failed: the file may have gone or changed).
    pub fn reprobe(&mut self, key: &str) {
        if let Some(e) = self.entries.get(key)
            && matches!(e.probe, Probe::Pending)
        {
            return;
        }
        self.request(key.to_owned());
    }

    fn request(&mut self, key: String) {
        self.next_ticket += 1;
        let ticket = self.next_ticket;
        let path = media_path(&MediaRef::new(key.clone()), self.show_dir.as_deref());
        let probe = match path {
            Some(path) => {
                let job = Job::Probe {
                    key: key.clone(),
                    ticket,
                    path,
                };
                match self.jobs.send(job) {
                    Ok(()) => Probe::Pending,
                    Err(_) => Probe::Failed,
                }
            }
            // A relative reference in a show that was never saved has no folder to be in.
            None => Probe::Failed,
        };
        let old = self.entries.insert(
            key,
            Entry {
                probe,
                ticket,
                preloaded: None,
                reserved: 0,
                over_budget: false,
            },
        );
        if let Some(old) = old {
            self.reserved = self.reserved.saturating_sub(old.reserved);
        }
    }

    /// Takes a worker's answer. Returns the status to report to the cue engine, or `None` for a
    /// stale answer or a preload. `wants_preload(key, probed)` says whether a cue asks for it.
    pub fn answer(
        &mut self,
        answer: ProbeAnswer,
        wants_preload: impl Fn(&str, &Probed) -> bool,
    ) -> Option<(MediaRef, MediaStatus)> {
        match answer {
            ProbeAnswer::Probed {
                key,
                ticket,
                result,
            } => {
                let entry = self.entries.get_mut(&key).filter(|e| e.ticket == ticket)?;
                let (probe, status, path) = match result {
                    Ok((path, probed)) => {
                        let ready = Ready { path, probed };
                        let path = ready.path.clone();
                        (
                            Probe::Ready(Arc::new(ready)),
                            MediaStatus::Available,
                            Some(path),
                        )
                    }
                    Err(e) => {
                        let status = status_of(&e);
                        (Probe::Failed, status, None)
                    }
                };
                entry.probe = probe;
                entry.preloaded = None;
                self.reserved = self.reserved.saturating_sub(entry.reserved);
                entry.reserved = 0;
                entry.over_budget = false;
                if let (Some(path), Probe::Ready(ready)) = (path, &entry.probe)
                    && wants_preload(&key, &ready.probed)
                {
                    let bytes = preload_bytes(&ready.probed);
                    if self.reserved.saturating_add(bytes) <= self.budget {
                        self.reserved += bytes;
                        entry.reserved = bytes;
                        let job = Job::Preload {
                            key: key.clone(),
                            ticket,
                            path,
                            max_bytes: bytes,
                        };
                        if self.jobs.send(job).is_err() {
                            self.reserved -= bytes;
                            entry.reserved = 0;
                        }
                    } else {
                        entry.over_budget = true;
                    }
                }
                Some((MediaRef::new(key), status))
            }
            ProbeAnswer::Preloaded {
                key,
                ticket,
                result,
            } => {
                let entry = self.entries.get_mut(&key).filter(|e| e.ticket == ticket)?;
                match result {
                    Ok(p) => entry.preloaded = Some(p),
                    Err(_) => {
                        // It streams instead; the budget it held goes back.
                        self.reserved = self.reserved.saturating_sub(entry.reserved);
                        entry.reserved = 0;
                        entry.over_budget = true;
                    }
                }
                None
            }
        }
    }

    pub fn get(&self, key: &str) -> Option<&Probe> {
        self.entries.get(key).map(|e| &e.probe)
    }

    /// What a stream of `key` reads: its preloaded samples if it has them, else the file.
    pub fn media(&self, key: &str) -> Option<(Arc<Ready>, Media)> {
        let entry = self.entries.get(key)?;
        let Probe::Ready(ready) = &entry.probe else {
            return None;
        };
        let media = match &entry.preloaded {
            Some(p) => Media::Preloaded(Arc::clone(p)),
            None => Media::Path(ready.path.clone()),
        };
        Some((Arc::clone(ready), media))
    }

    /// Probes still running.
    pub fn pending(&self) -> usize {
        self.entries
            .values()
            .filter(|e| matches!(e.probe, Probe::Pending))
            .count()
    }

    /// Bytes of the workspace preload budget in use.
    pub fn preload_reserved(&self) -> u64 {
        self.reserved
    }

    /// Files that asked for a preload and did not get one (over budget): they stream.
    pub fn over_budget(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter(|(_, e)| e.over_budget)
            .map(|(k, _)| k.as_str())
    }

    /// Damaged-but-playable files, with how many problems the probe found.
    pub fn damaged(&self) -> impl Iterator<Item = (&str, usize)> {
        self.entries.iter().filter_map(|(k, e)| match &e.probe {
            Probe::Ready(r) if !r.probed.warnings.is_empty() => {
                Some((k.as_str(), r.probed.warnings.len()))
            }
            _ => None,
        })
    }
}

/// Memory a preload of the file takes: every frame at its own rate, f32.
fn preload_bytes(p: &Probed) -> u64 {
    p.info
        .frames
        .saturating_mul(u64::from(p.info.channels))
        .saturating_mul(4)
}

/// How the cue engine sees a failed probe (docs/cue-engine.md, Broken cues).
pub fn status_of(e: &MediaError) -> MediaStatus {
    match e {
        MediaError::NotFound(_) => MediaStatus::Missing,
        other => MediaStatus::Unsupported {
            reason: other.to_string(),
        },
    }
}

fn worker(jobs: &Receiver<Job>, inbox: &Sender<Inbox>) {
    for job in jobs {
        let answer = match job {
            Job::Probe { key, ticket, path } => ProbeAnswer::Probed {
                result: probe(&path).map(|p| (path, p)),
                key,
                ticket,
            },
            Job::Preload {
                key,
                ticket,
                path,
                max_bytes,
            } => ProbeAnswer::Preloaded {
                result: preload_within(Media::Path(path), max_bytes),
                key,
                ticket,
            },
        };
        if inbox.send(Inbox::Probe(answer)).is_err() {
            return;
        }
    }
}
