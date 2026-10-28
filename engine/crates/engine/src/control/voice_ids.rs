// SPDX-License-Identifier: GPL-3.0-or-later

//! Mixer voice ids (docs/control.md "Voices"). One counter for the whole engine, outside the
//! voice table, so an id is never reused whatever workspace or cue engine its voice belongs to:
//! a voice of a replaced workspace (and, with several workspaces open, M4.15, another
//! workspace's) can never be taken for this one's.
//!
//! Each id is recorded with the doc it was taken for (ARCHITECTURE §3.5: `owner: VoiceId ->
//! DocId`), so the mixer's events, which name only voices, go to the doc that owns them; an
//! event of a voice no open doc owns (a closed doc's) is ignored. The map lives on the control
//! thread only; the audio thread knows nothing of docs.

use std::collections::HashMap;

use openlab_audio::VoiceId;

use super::DocId;

pub struct VoiceIds {
    next: u64,
    owners: HashMap<VoiceId, DocId>,
}

impl VoiceIds {
    /// Ids start at 1 (0 is the meter buffers' filler).
    pub fn new() -> Self {
        Self {
            next: 1,
            owners: HashMap::new(),
        }
    }

    /// A fresh id for a voice of `owner`. The counter would need 2^64 starts to wrap; it
    /// saturates rather than panicking, and the mixer refuses a duplicate id as
    /// `VoiceRejected` (a broken start).
    pub fn next(&mut self, owner: DocId) -> VoiceId {
        let id = VoiceId(self.next);
        self.next = self.next.saturating_add(1);
        self.owners.insert(id, owner);
        id
    }

    /// The doc a voice id was taken for, while it is recorded.
    pub fn owner(&self, id: VoiceId) -> Option<DocId> {
        self.owners.get(&id).copied()
    }

    /// Keeps only the ids `keep` holds on to (the owning doc still knows the voice): the map
    /// stays the size of the voices alive, however long the show runs.
    pub fn retain(&mut self, mut keep: impl FnMut(VoiceId, DocId) -> bool) {
        self.owners.retain(|id, doc| keep(*id, *doc));
    }

    /// Ids recorded (a leak measure).
    pub fn recorded(&self) -> usize {
        self.owners.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: DocId = DocId(0);
    const B: DocId = DocId(1);

    #[test]
    fn ids_start_at_one_and_never_repeat() {
        let mut ids = VoiceIds::new();
        let taken: Vec<u64> = (0..1000).map(|_| ids.next(A).0).collect();
        assert_eq!(taken.first(), Some(&1));
        assert!(taken.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn the_counter_saturates_instead_of_panicking() {
        let mut ids = VoiceIds {
            next: u64::MAX - 1,
            owners: HashMap::new(),
        };
        assert_eq!(ids.next(A).0, u64::MAX - 1);
        assert_eq!(ids.next(A).0, u64::MAX);
        assert_eq!(ids.next(A).0, u64::MAX);
    }

    #[test]
    fn ids_of_two_docs_never_meet_and_each_knows_its_owner() {
        let mut ids = VoiceIds::new();
        let a: Vec<VoiceId> = (0..50).map(|_| ids.next(A)).collect();
        let b: Vec<VoiceId> = (0..50).map(|_| ids.next(B)).collect();
        assert!(a.iter().all(|id| !b.contains(id)));
        assert!(a.iter().all(|&id| ids.owner(id) == Some(A)));
        assert!(b.iter().all(|&id| ids.owner(id) == Some(B)));
        // A closed doc's ids are forgotten: their late events find no owner.
        ids.retain(|_, doc| doc != A);
        assert!(a.iter().all(|&id| ids.owner(id).is_none()));
        assert_eq!(ids.recorded(), 50);
    }
}
