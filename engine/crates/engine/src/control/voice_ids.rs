// SPDX-License-Identifier: GPL-3.0-or-later

//! Mixer voice ids (docs/control.md "Voices"). One counter for the whole engine, outside the
//! voice table, so an id is never reused whatever workspace or cue engine its voice belongs to:
//! a voice of a replaced workspace (and, with several workspaces open, M4.15, another
//! workspace's) can never be taken for this one's.

use openlab_audio::VoiceId;

pub struct VoiceIds {
    next: u64,
}

impl VoiceIds {
    /// Ids start at 1 (0 is the meter buffers' filler).
    pub fn new() -> Self {
        Self { next: 1 }
    }

    /// A fresh id. The counter would need 2^64 starts to wrap; it saturates rather than
    /// panicking, and the mixer refuses a duplicate id as `VoiceRejected` (a broken start).
    pub fn next(&mut self) -> VoiceId {
        let id = VoiceId(self.next);
        self.next = self.next.saturating_add(1);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_start_at_one_and_never_repeat() {
        let mut ids = VoiceIds::new();
        let taken: Vec<u64> = (0..1000).map(|_| ids.next().0).collect();
        assert_eq!(taken.first(), Some(&1));
        assert!(taken.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn the_counter_saturates_instead_of_panicking() {
        let mut ids = VoiceIds { next: u64::MAX - 1 };
        assert_eq!(ids.next().0, u64::MAX - 1);
        assert_eq!(ids.next().0, u64::MAX);
        assert_eq!(ids.next().0, u64::MAX);
    }
}
