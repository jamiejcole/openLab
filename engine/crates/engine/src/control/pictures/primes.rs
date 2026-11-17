// SPDX-License-Identifier: GPL-3.0-or-later

//! Video cues standing by (VID-03): the Video cues near each playhead (the cues whose sound
//! is primed, their targets and armed hotkeys, in that order) have their pictures loaded on the
//! stage host ahead, so the file is open and its first frame decoded before their GO. A prime
//! is never drawn: a run's load from the same point (file, stage and start) takes it over
//! instead of opening the file again; one that differs lets it go.
//!
//! One prime open is asked of the stage host at a time, every doc's together, the most urgent
//! first: a slow disk serves the cue standing by first, a stuck volume holds at most one decode
//! thread for priming, and a stage host that crashes while a prime opens names that file. A
//! prime let go before its open answered still counts until it answers (the stage host's own
//! open limit answers for a stuck one), and only then is it unloaded. A file whose open ahead
//! failed or crashed the stage host, or one on a stalled volume, is not opened ahead: its cue
//! opens it at its GO, under the checks any running picture gets.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::{DocCx, DocId, Engine, ToStage};
use openlab_core::CueId;

/// The most pictures one doc keeps open ahead: each holds a decoder and a few frames on the
/// stage host.
const MAX_PICTURE_PRIMES: usize = 4;
/// The most files kept from priming after failures; past it they are all forgotten.
const MAX_UNPRIMED: usize = 256;

/// A picture opened ahead for a cue, not yet any run's.
struct Primed {
    doc: DocId,
    cue: CueId,
    load: ToStage,
    /// The stage host was sent its load (a new stage host is sent them again one at a time).
    sent: bool,
    opened: bool,
    /// Not wanted any more, but its open has not answered: unloaded once it does.
    dropped: bool,
}

impl Primed {
    /// Its open was asked and has not answered.
    fn opening(&self) -> bool {
        self.sent && !self.opened
    }
}

#[derive(Default)]
pub(in crate::control) struct Primes {
    by_id: BTreeMap<u64, Primed>,
    /// Files that crashed the stage host or failed while they were opening ahead.
    unprimed: BTreeSet<String>,
    /// Opens asked ahead so far (tests).
    asked: u64,
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

    /// The stage host has none of them: those let go go, the others are to be sent again.
    fn forget_sent(&mut self) {
        self.by_id.retain(|_, p| !p.dropped);
        for p in self.by_id.values_mut() {
            p.sent = false;
            p.opened = false;
        }
    }

    /// Whether a prime's open is under way (the next waits for it).
    fn opening(&self) -> bool {
        self.by_id.values().any(Primed::opening)
    }
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

impl Engine {
    /// The stage host has prime `picture`'s file open (one let go meanwhile is unloaded now).
    /// `false` if it is no prime.
    pub(in crate::control) fn prime_opened(&mut self, picture: u64) -> bool {
        let Some(p) = self.pictures.primes.by_id.get_mut(&picture) else {
            return false;
        };
        p.opened = true;
        if p.dropped {
            self.pictures.primes.by_id.remove(&picture);
            self.picture(ToStage::Unload { picture });
        }
        true
    }

    /// Prime `picture` failed on the stage host: it goes, and its file is not opened ahead again
    /// (the cue's GO opens it and reports what fails then). `false` if it is no prime.
    pub(in crate::control) fn prime_failed(&mut self, picture: u64) -> bool {
        let Some(p) = self.pictures.primes.by_id.remove(&picture) else {
            return false;
        };
        self.pictures.primes.bar(&p.load);
        true
    }

    /// The stage host stopped. After a crash the prime that was opening (one at most) goes and
    /// its file is not opened ahead again; `true` then: the crash is that file's doing, not a
    /// running picture's. Primes let go go now (the next stage host never had them); the others
    /// are sent again one at a time.
    pub(in crate::control) fn primes_host_stopped(&mut self, crashed: bool) -> bool {
        let primes = &mut self.pictures.primes;
        let opening: Vec<u64> = (primes.by_id.iter())
            .filter(|(_, p)| p.opening())
            .map(|(id, _)| *id)
            .collect();
        let mut blamed = false;
        if crashed {
            for id in opening {
                if let Some(p) = primes.by_id.remove(&id) {
                    primes.bar(&p.load);
                    blamed = true;
                }
            }
        }
        primes.forget_sent();
        blamed
    }

    /// A stage host started: it has no picture opened ahead (one sent before it was ready was
    /// never written to it), so they are sent again one at a time.
    pub(in crate::control) fn primes_host_started(&mut self) {
        self.pictures.primes.forget_sent();
    }

    /// A doc closed: its primes go, one still opening once its open answers.
    pub(super) fn drop_doc_primes(&mut self, doc: DocId) {
        let gone: Vec<u64> = (self.pictures.primes.by_id.iter())
            .filter(|(_, p)| p.doc == doc)
            .map(|(id, _)| *id)
            .collect();
        for picture in gone {
            self.let_go_prime(picture);
        }
    }

    /// Lets prime `picture` go: unloaded now if the stage host has answered its open (or never
    /// had it), else kept, counted as opening, until it answers.
    fn let_go_prime(&mut self, picture: u64) {
        let Some(p) = self.pictures.primes.by_id.get_mut(&picture) else {
            return;
        };
        if p.opening() {
            p.dropped = true;
            return;
        }
        let sent = p.sent;
        self.pictures.primes.by_id.remove(&picture);
        if sent {
            self.picture(ToStage::Unload { picture });
        }
    }

    /// Primes on the stage host's side, let-go ones still opening included, and of those open
    /// (tests).
    pub(in crate::control) fn primes_up(&self) -> (usize, usize) {
        let up = || self.pictures.primes.by_id.values().filter(|p| p.sent);
        (up().count(), up().filter(|p| p.opened).count())
    }

    /// Opens asked ahead so far (tests).
    pub(in crate::control) fn primes_asked(&self) -> u64 {
        self.pictures.primes.asked
    }
}

impl DocCx<'_> {
    /// Keeps the pictures of the first `MAX_PICTURE_PRIMES` of `wanted` (the Video cues near the
    /// playheads, most urgent first) open on the stage host from their start, and lets go of
    /// this doc's other primes. One open is asked at a time across every doc (`Control` primes
    /// every doc again when it answers). A cue whose picture is up or waits for its probe,
    /// whose file is not known to play, is on a stalled volume, or crashed the stage host or
    /// failed while opening ahead, is not primed. Nothing while no stage host is up.
    pub(in crate::control) fn prime_pictures(&mut self, wanted: &[CueId]) {
        // Only a stage host that is up is written to: the next one is sent them when it is.
        if !self.eng.opens_pictures() {
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
            let refused = file_of(&load).is_some_and(|f| {
                self.eng.pictures.primes.unprimed.contains(f)
                    || self.doc.media.volume_stalled(Path::new(f))
            });
            if !refused {
                want.push((cue, load));
            }
        }
        // This doc's primes: one wanted is kept (taken back if it was let go), the others go.
        let mine: Vec<u64> = (self.eng.pictures.primes.by_id.iter())
            .filter(|(_, p)| p.doc == doc)
            .map(|(id, _)| *id)
            .collect();
        let mut kept: Vec<Option<u64>> = vec![None; want.len()];
        for id in mine {
            let matched = self.eng.pictures.primes.by_id.get(&id).and_then(|p| {
                (want.iter().zip(&kept)).position(|((cue, load), k)| {
                    k.is_none() && *cue == p.cue && same_point(load, &p.load)
                })
            });
            match matched.and_then(|i| kept.get_mut(i)) {
                Some(slot) => {
                    *slot = Some(id);
                    if let Some(p) = self.eng.pictures.primes.by_id.get_mut(&id) {
                        p.dropped = false;
                    }
                }
                None => self.eng.let_go_prime(id),
            }
        }
        if self.eng.pictures.primes.opening() {
            return;
        }
        // The most urgent prime the stage host does not have yet is sent.
        for ((cue, load), k) in want.into_iter().zip(kept) {
            match k {
                Some(id) => {
                    let Some(p) = self.eng.pictures.primes.by_id.get_mut(&id) else {
                        continue;
                    };
                    if p.sent {
                        continue;
                    }
                    p.sent = true;
                    let load = p.load.clone();
                    self.eng.pictures.primes.asked += 1;
                    self.eng.picture(load);
                }
                None => {
                    let mut load = load;
                    self.eng.pictures.next += 1;
                    let id = self.eng.pictures.next;
                    with_id(&mut load, id);
                    self.eng.pictures.primes.by_id.insert(
                        id,
                        Primed {
                            doc,
                            cue,
                            load: load.clone(),
                            sent: true,
                            opened: false,
                            dropped: false,
                        },
                    );
                    self.eng.pictures.primes.asked += 1;
                    self.eng.picture(load);
                }
            }
            return;
        }
    }

    /// The prime of `cue` if the stage host has it and it opens what `load` asks (it is taken:
    /// its id and whether it is open). A prime of `cue` from elsewhere is let go.
    pub(super) fn take_picture_prime(
        &mut self,
        cue: CueId,
        load: &ToStage,
    ) -> Option<(u64, bool, ToStage)> {
        let doc = self.doc.id;
        let id = (self.eng.pictures.primes.by_id.iter())
            .find(|(_, p)| p.doc == doc && p.cue == cue && !p.dropped)
            .map(|(id, _)| *id)?;
        let usable = (self.eng.pictures.primes.by_id.get(&id))
            .is_some_and(|p| p.sent && same_point(load, &p.load));
        if !usable {
            self.eng.let_go_prime(id);
            return None;
        }
        let p = self.eng.pictures.primes.by_id.remove(&id)?;
        Some((id, p.opened, p.load))
    }
}
