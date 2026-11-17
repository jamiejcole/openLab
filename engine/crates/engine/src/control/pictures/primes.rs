// SPDX-License-Identifier: GPL-3.0-or-later

//! Video cues standing by (VID-03): the Video cues near each playhead (the cues whose sound
//! is primed, their targets and armed hotkeys, in that order) have their pictures loaded on the
//! stage host ahead, so the file is open and its first frame decoded before their GO. A prime
//! is never drawn: a run's load from the same point (file, stage and start) takes it over
//! instead of opening the file again; one that differs lets it go.
//!
//! Primes open one at a time, the most urgent first: a slow disk serves the cue standing by
//! first, and a stage host that crashes while a prime opens names that file alone. Such a file,
//! or one whose open ahead fails, is not opened ahead again (a file that crashes the decoder
//! would otherwise crash every stage host before any GO): its cue opens it at its GO, under the
//! checks any running picture gets.

use std::collections::BTreeSet;

use super::{DocCx, DocId, Engine, ToStage};
use openlab_core::CueId;

/// The most pictures one doc keeps open ahead: each holds a decoder and a few frames on the
/// stage host.
const MAX_PICTURE_PRIMES: usize = 4;
/// The most files kept from priming after crashes; past it they are all forgotten.
const MAX_UNPRIMED: usize = 256;

/// A picture opened ahead for a cue, not yet any run's.
struct Primed {
    doc: DocId,
    cue: CueId,
    load: ToStage,
    opened: bool,
}

#[derive(Default)]
pub(in crate::control) struct Primes {
    by_id: std::collections::BTreeMap<u64, Primed>,
    /// Files that crashed the stage host or failed while they were opening ahead.
    unprimed: BTreeSet<String>,
}

/// Whether two loads open the same file on the same stage at the same point (geometry, which
/// follows edits, apart).
fn same_point(a: &ToStage, b: &ToStage) -> bool {
    match (a, b) {
        (
            ToStage::Load {
                stage,
                file,
                still,
                offset,
                ..
            },
            ToStage::Load {
                stage: s,
                file: f,
                still: st,
                offset: o,
                ..
            },
        ) => stage == s && file == f && still == st && offset.to_bits() == o.to_bits(),
        _ => false,
    }
}

fn file_of(load: &ToStage) -> Option<&str> {
    match load {
        ToStage::Load { file, .. } => Some(file),
        _ => None,
    }
}

fn with_id(load: &mut ToStage, id: u64) {
    if let ToStage::Load { picture, .. } = load {
        *picture = id;
    }
}

impl Primes {
    /// Keeps `load`'s file from being opened ahead again.
    fn bar(&mut self, load: &ToStage) {
        if self.unprimed.len() >= MAX_UNPRIMED {
            self.unprimed.clear();
        }
        if let Some(file) = file_of(load) {
            self.unprimed.insert(file.to_owned());
        }
    }

    /// Whether a prime is still opening (the next waits for it).
    fn opening(&self) -> bool {
        self.by_id.values().any(|p| !p.opened)
    }
}

impl Engine {
    /// The stage host has prime `picture`'s file open: its doc, which opens its next prime
    /// (`None` if it is no prime).
    pub(super) fn prime_opened(&mut self, picture: u64) -> Option<DocId> {
        let p = self.pictures.primes.by_id.get_mut(&picture)?;
        p.opened = true;
        Some(p.doc)
    }

    /// Prime `picture` failed on the stage host: it goes, its file is not opened ahead again
    /// (the cue's GO opens it and reports what fails then), and its doc opens its next prime
    /// (`None` if it is no prime).
    pub(in crate::control) fn prime_failed(&mut self, picture: u64) -> Option<DocId> {
        let p = self.pictures.primes.by_id.remove(&picture)?;
        self.pictures.primes.bar(&p.load);
        Some(p.doc)
    }

    /// The stage host stopped: after a crash, the prime still opening (one at most) is dropped
    /// and its file kept from priming; every other prime is opened again by the next stage
    /// host. The docs whose prime was dropped, to open their next.
    pub(super) fn primes_host_stopped(&mut self, crashed: bool) -> Vec<DocId> {
        let primes = &mut self.pictures.primes;
        let mut docs = Vec::new();
        if crashed {
            let opening: Vec<u64> = (primes.by_id.iter())
                .filter(|(_, p)| !p.opened)
                .map(|(id, _)| *id)
                .collect();
            for id in opening {
                if let Some(p) = primes.by_id.remove(&id) {
                    primes.bar(&p.load);
                    docs.push(p.doc);
                }
            }
        }
        for p in primes.by_id.values_mut() {
            p.opened = false;
        }
        docs
    }

    /// The loads of the primes, for a stage host that started.
    pub(super) fn prime_loads(&self) -> Vec<ToStage> {
        self.pictures
            .primes
            .by_id
            .values()
            .map(|p| p.load.clone())
            .collect()
    }

    /// A doc closed: its primes go.
    pub(super) fn drop_doc_primes(&mut self, doc: DocId) {
        let gone: Vec<u64> = (self.pictures.primes.by_id.iter())
            .filter(|(_, p)| p.doc == doc)
            .map(|(id, _)| *id)
            .collect();
        for picture in gone {
            self.pictures.primes.by_id.remove(&picture);
            self.picture(ToStage::Unload { picture });
        }
    }

    /// Primes up, and of those open (tests).
    pub(in crate::control) fn primes_up(&self) -> (usize, usize) {
        let by_id = &self.pictures.primes.by_id;
        (by_id.len(), by_id.values().filter(|p| p.opened).count())
    }
}

impl DocCx<'_> {
    /// Keeps the pictures of the first `MAX_PICTURE_PRIMES` of `wanted` (the Video cues near the
    /// playheads, most urgent first) open on the stage host from their start, and lets go of
    /// this doc's other primes. A new prime is opened only while no other is opening (the next
    /// when it has: `Control::reprime`). A cue whose picture is up or waits for its probe,
    /// whose file is not known to play, or whose file crashed the stage host or failed while
    /// opening ahead, is not primed. Nothing without a stage host.
    pub(in crate::control) fn prime_pictures(&mut self, wanted: &[CueId]) {
        if self.eng.stages_host().is_none() {
            return;
        }
        let doc = self.doc.id;
        let mut want: Vec<(CueId, ToStage)> = Vec::new();
        for &cue in wanted {
            if want.len() >= MAX_PICTURE_PRIMES {
                break;
            }
            let busy = self.has_picture(cue)
                || (self.eng.pictures.held.iter()).any(|h| h.doc == doc && h.cue == cue);
            if busy {
                continue;
            }
            let Ok((load, ..)) = self.picture_load(cue, 0) else {
                continue;
            };
            let refused =
                file_of(&load).is_some_and(|f| self.eng.pictures.primes.unprimed.contains(f));
            if !refused {
                want.push((cue, load));
            }
        }
        let mine: Vec<u64> = (self.eng.pictures.primes.by_id.iter())
            .filter(|(_, p)| p.doc == doc)
            .map(|(id, _)| *id)
            .collect();
        for id in mine {
            let kept = self.eng.pictures.primes.by_id.get(&id).and_then(|p| {
                want.iter()
                    .position(|(cue, load)| *cue == p.cue && same_point(load, &p.load))
            });
            match kept {
                Some(i) => {
                    want.remove(i);
                }
                None => {
                    self.eng.pictures.primes.by_id.remove(&id);
                    self.eng.picture(ToStage::Unload { picture: id });
                }
            }
        }
        for (cue, mut load) in want {
            if self.eng.pictures.primes.opening() {
                break;
            }
            self.eng.pictures.next += 1;
            let id = self.eng.pictures.next;
            with_id(&mut load, id);
            self.eng.pictures.primes.by_id.insert(
                id,
                Primed {
                    doc,
                    cue,
                    load: load.clone(),
                    opened: false,
                },
            );
            self.eng.picture(load);
        }
    }

    /// The prime of `cue` if it opens what `load` asks (it is taken: its id and whether the
    /// stage host has it open). A prime of `cue` from elsewhere is let go.
    pub(super) fn take_picture_prime(
        &mut self,
        cue: CueId,
        load: &ToStage,
    ) -> Option<(u64, bool, ToStage)> {
        let doc = self.doc.id;
        let id = (self.eng.pictures.primes.by_id.iter())
            .find(|(_, p)| p.doc == doc && p.cue == cue)
            .map(|(id, _)| *id)?;
        let p = self.eng.pictures.primes.by_id.remove(&id)?;
        if same_point(load, &p.load) {
            return Some((id, p.opened, p.load));
        }
        self.eng.picture(ToStage::Unload { picture: id });
        None
    }
}
