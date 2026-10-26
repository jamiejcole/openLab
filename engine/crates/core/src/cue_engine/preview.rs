// SPDX-License-Identifier: GPL-3.0-or-later

//! Preview / audition (CUE-05, PB-09): the operator plays a cue to listen to it, outside the
//! show. A preview has no cue state, no pre-wait, no post-wait, no continue and no effect on any
//! playhead, on double-GO protection or on the groups around the cue; the voice actions carry
//! their own variants so the binary can route them to the audition output (M4.2). Rules:
//! `docs/cue-engine.md`, "Preview".

use crate::cue::{CueKind, FadeCurve};
use crate::model::CueId;

use super::time::{SampleTime, seconds_to_samples};
use super::types::{Action, BrokenReason, EngineWarning, PreviewIgnoredReason, RunId, StopFade};
use super::{Ctx, CueEngine};

/// Previews are numbered from here, apart from the show's runs: the show's `RunId`s are the same
/// whether or not the operator previews anything (a preview is invisible to the show), and a
/// preview's id can still be used as a voice id because it never collides with a show run's.
pub const PREVIEW_RUN_BASE: u64 = 1 << 63;

/// How long a stopped preview fades out, so the operator's headphones do not click.
pub const PREVIEW_STOP_FADE_SECONDS: f64 = 0.05;

/// A cue being previewed.
#[derive(Clone, Copy, Debug)]
pub(super) struct Preview {
    pub run: RunId,
    /// The sample its voice starts on; nothing for it is dated earlier.
    pub started: SampleTime,
}

impl CueEngine {
    /// Whether the cue is being previewed now.
    pub fn is_previewing(&self, cue: CueId) -> bool {
        self.previews.contains_key(&cue)
    }

    /// Every cue being previewed, in id order.
    pub fn previewing(&self) -> impl Iterator<Item = CueId> + '_ {
        self.previews.keys().copied()
    }

    /// The voice id of the cue's preview, if it is being previewed.
    pub fn preview_run(&self, cue: CueId) -> Option<RunId> {
        self.previews.get(&cue).map(|p| p.run)
    }

    /// Plays `cue` from its start, now. Audio cues only; a cue with problems is refused as for
    /// any start; one that is already being previewed is left alone.
    pub(super) fn preview(&mut self, cue: CueId, cx: &mut Ctx) {
        let Some(model) = cx.ws.find_cue(cue) else {
            return cx.warn(EngineWarning::UnknownCue { cue });
        };
        if !matches!(model.kind, CueKind::Audio { .. }) {
            return cx.warn(EngineWarning::PreviewIgnored {
                cue,
                reason: PreviewIgnoredReason::NotAudio,
            });
        }
        if self.is_previewing(cue) {
            return cx.warn(EngineWarning::PreviewIgnored {
                cue,
                reason: PreviewIgnoredReason::AlreadyPreviewing,
            });
        }
        if self.refuses(cue, cx) {
            return cx.warn(EngineWarning::CueBroken { cue });
        }
        let run = RunId(PREVIEW_RUN_BASE.saturating_add(self.next_preview));
        self.next_preview = self.next_preview.saturating_add(1);
        let at = cx.now;
        self.previews.insert(cue, Preview { run, started: at });
        cx.out.push(Action::PreviewStart { cue, run, at });
        cx.out.push(Action::PreviewChanged {
            cue,
            active: true,
            at,
        });
    }

    /// Ends the cue's preview (all of them for `None`): fading out briefly, or at once when
    /// `hard`. The preview is over as of now; the fade is the mixer's.
    pub(super) fn stop_previews(&mut self, cue: Option<CueId>, hard: bool, cx: &mut Ctx) {
        let cues: Vec<CueId> = match cue {
            Some(cue) => vec![cue],
            None => self.previews.keys().copied().collect(),
        };
        let frames = seconds_to_samples(PREVIEW_STOP_FADE_SECONDS, self.config.sample_rate());
        for cue in cues {
            let Some(preview) = self.previews.remove(&cue) else {
                continue;
            };
            let fade = (!hard && frames > 0).then_some(StopFade {
                frames,
                curve: FadeCurve::Linear,
            });
            self.end_preview(cue, preview, fade, cx);
        }
    }

    fn end_preview(&mut self, cue: CueId, preview: Preview, fade: Option<StopFade>, cx: &mut Ctx) {
        // Never before the voice's own start sample, so a stop cannot overtake its start.
        let at = cx.now.max(preview.started);
        cx.out.push(Action::PreviewStop {
            cue,
            run: preview.run,
            at,
            fade,
        });
        cx.out.push(Action::PreviewChanged {
            cue,
            active: false,
            at,
        });
    }

    /// Feedback: the voice of a preview ended by itself. Returns whether `run` was a preview's.
    pub(super) fn preview_ended(
        &mut self,
        cue: CueId,
        run: RunId,
        at: SampleTime,
        cx: &mut Ctx,
    ) -> bool {
        if !matches!(self.previews.get(&cue), Some(p) if p.run == run) {
            return false;
        }
        if let Some(preview) = self.previews.remove(&cue) {
            cx.out.push(Action::PreviewChanged {
                cue,
                active: false,
                at: at.max(preview.started),
            });
        }
        true
    }

    /// Feedback: the voice of a preview failed. Returns whether `run` was a preview's.
    pub(super) fn preview_failed(
        &mut self,
        cue: CueId,
        run: RunId,
        reason: BrokenReason,
        cx: &mut Ctx,
    ) -> bool {
        if !matches!(self.previews.get(&cue), Some(p) if p.run == run) {
            return false;
        }
        if let Some(preview) = self.previews.remove(&cue) {
            // Freed at once, as a failed show voice is.
            self.end_preview(cue, preview, None, cx);
            cx.warn(EngineWarning::PreviewFailed { cue, reason });
        }
        true
    }

    /// Stops previews of cues that are gone from the workspace or are no longer audio cues.
    /// Returns whether any did.
    pub(super) fn sync_previews(&mut self, cx: &mut Ctx) -> bool {
        let gone: Vec<CueId> = self
            .previews
            .keys()
            .copied()
            .filter(|cue| {
                !cx.ws
                    .find_cue(*cue)
                    .is_some_and(|c| matches!(c.kind, CueKind::Audio { .. }))
            })
            .collect();
        let any = !gone.is_empty();
        for cue in gone {
            self.stop_previews(Some(cue), true, cx);
        }
        any
    }
}
