// SPDX-License-Identifier: GPL-3.0-or-later

//! A cue list's playhead (PB-05, CL-06): the top-level cue standing by, or `None` past the end.

use std::collections::HashSet;

use crate::cue::Cue;
use crate::model::CueId;

#[derive(Clone, Debug)]
pub(super) struct Playhead {
    pub cue: Option<CueId>,
    /// The list's top-level cue ids when last synced, to find where a deleted cue stood.
    order: Vec<CueId>,
}

impl Playhead {
    /// A new list's playhead stands on its first cue.
    pub fn new(cues: &[Cue]) -> Self {
        Self {
            cue: cues.first().map(|c| c.id),
            order: ids(cues),
        }
    }

    /// Follows edits to the list. A deleted playhead cue gives way to the first cue that came
    /// after it and still exists (none: past the end); a list that was empty and gains cues
    /// stands on its first cue. Returns whether the playhead moved.
    pub fn sync(&mut self, cues: &[Cue]) -> bool {
        let unchanged =
            self.order.len() == cues.len() && self.order.iter().zip(cues).all(|(a, c)| *a == c.id);
        if unchanged {
            return false;
        }
        let before = self.cue;
        self.cue = match self.cue {
            Some(id) if cues.iter().any(|c| c.id == id) => Some(id),
            Some(id) => {
                let present: HashSet<CueId> = cues.iter().map(|c| c.id).collect();
                self.order
                    .iter()
                    .skip_while(|o| **o != id)
                    .skip(1)
                    .find(|o| present.contains(o))
                    .copied()
            }
            None if self.order.is_empty() => cues.first().map(|c| c.id),
            None => None,
        };
        self.order = ids(cues);
        before != self.cue
    }

    /// One cue down; from the last cue to past the end, where it stays.
    pub fn next(&mut self, cues: &[Cue]) -> bool {
        let next = self
            .index(cues)
            .and_then(|i| cues.get(i.saturating_add(1)))
            .map(|c| c.id);
        self.set(next)
    }

    /// One cue up; from past the end to the last cue; the first cue stays.
    pub fn previous(&mut self, cues: &[Cue]) -> bool {
        let previous = match self.index(cues) {
            Some(i) => cues.get(i.saturating_sub(1)),
            None => cues.last(),
        };
        self.set(previous.map(|c| c.id))
    }

    pub fn set(&mut self, cue: Option<CueId>) -> bool {
        let moved = self.cue != cue;
        self.cue = cue;
        moved
    }

    fn index(&self, cues: &[Cue]) -> Option<usize> {
        let id = self.cue?;
        cues.iter().position(|c| c.id == id)
    }
}

fn ids(cues: &[Cue]) -> Vec<CueId> {
    cues.iter().map(|c| c.id).collect()
}
