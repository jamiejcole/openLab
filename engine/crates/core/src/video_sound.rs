// SPDX-License-Identifier: GPL-3.0-or-later

//! A Video cue's sound (VID-06; QLab 5 manual pp. 211-212, 214): the output patch its movie's
//! audio track plays on, its levels (an Audio cue's, AUD-06) and which of the file's audio
//! tracks plays.
//!
//! Stored in the Video cue's kind (file format §5, the same field names as an Audio cue's). A
//! cue whose file has no audio track keeps them unused: a movie without sound is never broken
//! by its patch, and levels set while a file had sound come back with a file that has it.

use serde::{Deserialize, Serialize};

use crate::cue::CueLevels;
use crate::model::PatchId;

/// A Video cue's sound, stored as fields of its kind; a field missing from a file is its
/// default (no patch, unity master, no outputs or crosspoints, the first track).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoSound {
    /// The output patch its sound plays on; `None` is "none", and a cue whose file has sound
    /// is broken.
    pub patch: Option<PatchId>,
    /// Its levels and routing, as an Audio cue's.
    pub levels: CueLevels,
    /// Which of the file's audio tracks plays, counted from 0. A number the file does not have
    /// plays the first track.
    pub audio_track: u32,
}

impl Default for VideoSound {
    fn default() -> Self {
        Self {
            patch: None,
            levels: CueLevels {
                master_db: 0.0,
                outputs: Vec::new(),
                crosspoints: Vec::new(),
                gangs: Vec::new(),
            },
            audio_track: 0,
        }
    }
}
