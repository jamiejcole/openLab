// SPDX-License-Identifier: GPL-3.0-or-later

//! Which audio cues' slices cannot be played with their file (`bad_plan` warnings), kept per
//! cue and recomputed only when the cue's trims and markers or its file's probe change: an edit
//! of one cue in a show of thousands builds one plan, not thousands (docs/control.md).

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use openlab_core::{Cue, CueId, CueKind, SliceMarker, Workspace};

use super::media::MediaCatalog;
use super::plan::{PlanFailure, PlanSpec, cue_slices};

struct Entry {
    probe: usize,
    start: u64,
    end: Option<u64>,
    marks: Vec<(u64, u32)>,
    last: u32,
    error: Option<String>,
    /// The pass that last saw the cue.
    pass: u64,
}

impl Entry {
    fn matches(&self, probe: usize, spec: &PlanSpec<'_>) -> bool {
        self.probe == probe
            && self.start == spec.start_time.to_bits()
            && self.end == spec.end_time.map(f64::to_bits)
            && self.last == spec.last_slice_play_count
            && self.marks.len() == spec.slices.len()
            && self
                .marks
                .iter()
                .zip(spec.slices)
                .all(|(&(t, n), m)| t == m.time.to_bits() && n == m.play_count)
    }
}

#[derive(Default)]
pub struct PlanChecks {
    cache: HashMap<CueId, Entry>,
    pass: u64,
    builds: u64,
}

impl PlanChecks {
    /// The cues (in workspace order) whose slices cannot be played, with why.
    pub fn check(
        &mut self,
        ws: &Workspace,
        media: &MediaCatalog,
        rate: NonZeroU32,
    ) -> Vec<(CueId, String)> {
        self.pass += 1;
        let mut out = Vec::new();
        for list in &ws.cue_lists {
            self.walk(&list.cues, media, rate, &mut out);
        }
        let pass = self.pass;
        self.cache.retain(|_, e| e.pass == pass);
        out
    }

    fn walk(
        &mut self,
        cues: &[Cue],
        media: &MediaCatalog,
        rate: NonZeroU32,
        out: &mut Vec<(CueId, String)>,
    ) {
        for c in cues {
            self.walk(c.children(), media, rate, out);
            let CueKind::Audio {
                media: Some(m),
                start_time,
                end_time,
                slices,
                last_slice_play_count,
                ..
            } = &c.kind
            else {
                continue;
            };
            let Some((ready, _)) = media.media(m.as_str()) else {
                continue;
            };
            let probe = Arc::as_ptr(&ready) as usize;
            let spec = PlanSpec {
                start_time: *start_time,
                end_time: *end_time,
                slices,
                last_slice_play_count: *last_slice_play_count,
            };
            let pass = self.pass;
            let error = match self.cache.get_mut(&c.id) {
                Some(e) if e.matches(probe, &spec) => {
                    e.pass = pass;
                    e.error.clone()
                }
                _ => {
                    self.builds += 1;
                    let error = match cue_slices(&spec, &ready.probed.info, rate, 0) {
                        Ok(_) => None,
                        Err(PlanFailure::Plan(e)) => Some(e.to_string()),
                    };
                    self.cache.insert(
                        c.id,
                        Entry {
                            probe,
                            start: start_time.to_bits(),
                            end: end_time.map(f64::to_bits),
                            marks: marks(slices),
                            last: *last_slice_play_count,
                            error: error.clone(),
                            pass,
                        },
                    );
                    error
                }
            };
            if let Some(e) = error {
                out.push((c.id, e));
            }
        }
    }

    /// Plans built so far (a cost measure for tests).
    pub fn builds(&self) -> u64 {
        self.builds
    }
}

fn marks(slices: &[SliceMarker]) -> Vec<(u64, u32)> {
    slices
        .iter()
        .map(|m| (m.time.to_bits(), m.play_count))
        .collect()
}
