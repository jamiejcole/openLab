// SPDX-License-Identifier: GPL-3.0-or-later

//! Preview / audition (CUE-05, PB-09): the operator plays a cue to listen to it, outside the
//! show. A preview has no cue state, no pre-wait, no post-wait, no continue and no effect on any
//! playhead, on double-GO protection or on the groups around the cue; its start and stop carry
//! their own variants so the binary can route them to the audition output (M4.2), and Pause All /
//! Resume All pause and resume it with the show's `PauseVoice` / `ResumeVoice` for its run.
//! Rules: `docs/cue-engine.md`, "Preview".

use std::collections::BTreeSet;

use crate::cue::{Cue, CueKind, FadeCurve};
use crate::model::{CueId, Workspace};

use super::time::{SampleTime, seconds_to_samples};
use super::transport::PANIC_CURVE;
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
    /// The sample its voice (this segment of it, after a resume) starts on; nothing for it is
    /// dated earlier.
    pub started: SampleTime,
    /// Where in the cue's action the voice is on `started` (0, or where a pause left it).
    pub offset: SampleTime,
    /// Paused by Pause All on this sample (QLab check V4, M2.28).
    pub paused_at: Option<SampleTime>,
}

/// The voice of a preview fading out in a Panic: no longer a preview (it is reported ended), but
/// a second Panic during the fade still cuts it at once (QLab check V6, M2.28).
#[derive(Clone, Copy, Debug)]
pub(super) struct PanickedPreview {
    pub cue: CueId,
    pub run: RunId,
    pub started: SampleTime,
}

impl CueEngine {
    /// Whether the cue is being previewed now (a group: while any audio cue its preview started
    /// is still being previewed).
    pub fn is_previewing(&self, cue: CueId) -> bool {
        self.previews.contains_key(&cue) || self.group_previews.contains_key(&cue)
    }

    /// Every cue being previewed, groups included, in id order.
    pub fn previewing(&self) -> impl Iterator<Item = CueId> + '_ {
        let mut all: Vec<CueId> = (self.previews.keys())
            .chain(self.group_previews.keys())
            .copied()
            .collect();
        all.sort();
        all.into_iter()
    }

    /// The voice id of the cue's preview, if it is being previewed (an audio cue).
    pub fn preview_run(&self, cue: CueId) -> Option<RunId> {
        self.previews.get(&cue).map(|p| p.run)
    }

    /// Plays `cue` from its start, now. An audio cue plays itself; a group plays every armed
    /// audio cue inside it at once (QLab check V1, M2.28). A disarmed cue is refused (check V2),
    /// a cue with problems is refused as for any start, and one that is already being previewed
    /// is left alone.
    pub(super) fn preview(&mut self, cue: CueId, cx: &mut Ctx) {
        let Some(model) = cx.ws.find_cue(cue) else {
            return cx.warn(EngineWarning::UnknownCue { cue });
        };
        let ignored = |reason| EngineWarning::PreviewIgnored { cue, reason };
        let group = match model.kind {
            CueKind::Audio { .. } => false,
            CueKind::Group { .. } => true,
            _ => return cx.warn(ignored(PreviewIgnoredReason::NotAudio)),
        };
        if self.is_previewing(cue) {
            return cx.warn(ignored(PreviewIgnoredReason::AlreadyPreviewing));
        }
        if !self.is_armed(model) {
            return cx.warn(ignored(PreviewIgnoredReason::Disarmed));
        }
        if !group {
            if self.refuses(cue, cx) {
                return cx.warn(EngineWarning::CueBroken { cue });
            }
            return self.start_preview(cue, cx);
        }
        let mut started = BTreeSet::new();
        for child in self.previewable_children(model, cx.ws) {
            // Being previewed on its own already: it stays one preview. A child with the
            // group's own id (duplicate ids) is left out above; this holds even if the cue
            // index has not yet noticed the duplicate (an unsignalled edit).
            if child == cue || self.is_previewing(child) {
                continue;
            }
            if self.refuses(child, cx) {
                cx.warn(EngineWarning::CueBroken { cue: child });
                continue;
            }
            self.start_preview(child, cx);
            started.insert(child);
        }
        if started.is_empty() {
            return cx.warn(ignored(PreviewIgnoredReason::NothingToPreview));
        }
        self.group_previews.insert(cue, started);
        cx.out.push(Action::PreviewChanged {
            cue,
            active: true,
            at: cx.now,
        });
    }

    /// The audio cues a preview of `group` plays: every armed one inside it, at any depth,
    /// leaving out what is inside a disarmed group (the show would not start it either).
    /// Iterative, so nesting depth cannot overflow the stack.
    ///
    /// A preview is named by its id from here on (`PreviewStart`, the host's media lookup,
    /// `sync_previews`), and an id names the first cue that has it. With duplicate ids (a load
    /// error no command can make, M2.32) an audio cue whose id names another cue first (the
    /// group itself, a Start cue, another audio cue) is left out, so the engine never previews
    /// an id as audio that the workspace resolves to something else.
    fn previewable_children(&self, group: &Cue, ws: &Workspace) -> Vec<CueId> {
        let duplicates = self.problems.tree().has_duplicates();
        let named = |child: &Cue| {
            !duplicates
                || ws
                    .find_cue(child.id)
                    .is_some_and(|c| std::ptr::eq(c, child))
        };
        let mut out = Vec::new();
        let mut stack: Vec<std::slice::Iter<'_, Cue>> = vec![group.children().iter()];
        while let Some(level) = stack.last_mut() {
            let Some(child) = level.next() else {
                stack.pop();
                continue;
            };
            if !self.is_armed(child) {
                continue;
            }
            match child.kind {
                CueKind::Audio { .. } if named(child) => out.push(child.id),
                CueKind::Group { .. } => stack.push(child.children().iter()),
                _ => {}
            }
        }
        out
    }

    fn start_preview(&mut self, cue: CueId, cx: &mut Ctx) {
        let run = self.new_preview_run();
        let at = cx.now;
        self.previews.insert(
            cue,
            Preview {
                run,
                started: at,
                offset: 0,
                paused_at: None,
            },
        );
        cx.out.push(Action::PreviewStart { cue, run, at });
        cx.out.push(Action::PreviewChanged {
            cue,
            active: true,
            at,
        });
    }

    fn new_preview_run(&mut self) -> RunId {
        let run = RunId(PREVIEW_RUN_BASE.saturating_add(self.next_preview));
        self.next_preview = self.next_preview.saturating_add(1);
        run
    }

    /// Ends the cue's preview (all of them for `None`; for a group, every audio cue its preview
    /// plays): fading out briefly, or at once when `hard` or paused (a paused voice is silent).
    /// The preview is over as of now; the fade is the mixer's.
    pub(super) fn stop_previews(&mut self, cue: Option<CueId>, hard: bool, cx: &mut Ctx) {
        let cues: Vec<CueId> = match cue {
            Some(cue) => match self.group_previews.get(&cue) {
                Some(children) => children.iter().copied().collect(),
                None => vec![cue],
            },
            None => self.previews.keys().copied().collect(),
        };
        let frames = seconds_to_samples(PREVIEW_STOP_FADE_SECONDS, self.config.sample_rate());
        for cue in cues {
            let Some(preview) = self.previews.remove(&cue) else {
                continue;
            };
            let fade = (!hard && frames > 0 && preview.paused_at.is_none()).then_some(StopFade {
                frames,
                curve: FadeCurve::Linear,
            });
            self.end_preview(cue, preview, fade, cx);
        }
    }

    /// Stops the voice of a preview already taken out of `previews` and reports it ended.
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
        self.preview_gone(cue, at, cx);
    }

    /// `cue`'s preview has ended: a group preview it was part of ends with its last audio cue.
    fn preview_gone(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        let mut ended = Vec::new();
        for (group, children) in &mut self.group_previews {
            if children.remove(&cue) && children.is_empty() {
                ended.push(*group);
            }
        }
        for group in ended {
            self.group_previews.remove(&group);
            cx.out.push(Action::PreviewChanged {
                cue: group,
                active: false,
                at,
            });
        }
    }

    /// Pause All: every playing preview pauses where it is (QLab check V4, M2.28).
    pub(super) fn pause_previews(&mut self, cx: &mut Ctx) {
        let now = cx.now;
        for (&cue, preview) in &mut self.previews {
            if preview.paused_at.is_some() {
                continue;
            }
            let at = now.max(preview.started);
            preview.paused_at = Some(at);
            cx.out.push(Action::PauseVoice {
                cue,
                run: preview.run,
                at,
                offset: preview
                    .offset
                    .saturating_add(at.saturating_sub(preview.started)),
            });
        }
    }

    /// Resume All: every paused preview plays on from where it paused, with a fresh voice id
    /// (as a show cue's resume), so a late end report of the paused segment is stale.
    pub(super) fn resume_previews(&mut self, cx: &mut Ctx) {
        let now = cx.now;
        let paused: Vec<CueId> = (self.previews.iter())
            .filter(|(_, p)| p.paused_at.is_some())
            .map(|(cue, _)| *cue)
            .collect();
        for cue in paused {
            let run = self.new_preview_run();
            let Some(preview) = self.previews.get_mut(&cue) else {
                continue;
            };
            let Some(paused_at) = preview.paused_at.take() else {
                continue;
            };
            let offset = preview
                .offset
                .saturating_add(paused_at.saturating_sub(preview.started));
            let paused = preview.run;
            let at = now.max(paused_at);
            *preview = Preview {
                run,
                started: at,
                offset,
                paused_at: None,
            };
            cx.out.push(Action::ResumeVoice {
                cue,
                run,
                paused,
                at,
                offset,
            });
        }
    }

    /// Panic (QLab checks V4 and V6, M2.28): a playing preview fades out with the show, ending on
    /// `end`, and is over as of now; a paused one stops at once. With `end` `None` (a second
    /// Panic during the fade, or a panic time of 0) every preview stops at once, and so do the
    /// voices still fading out from the first Panic.
    pub(super) fn panic_previews(&mut self, end: Option<SampleTime>, cx: &mut Ctx) {
        let now = cx.now;
        for p in std::mem::take(&mut self.panicked_previews) {
            cx.out.push(Action::PreviewStop {
                cue: p.cue,
                run: p.run,
                at: now.max(p.started),
                fade: None,
            });
        }
        let Some(end) = end else {
            return self.stop_previews(None, true, cx);
        };
        let cues: Vec<CueId> = self.previews.keys().copied().collect();
        for cue in cues {
            let Some(preview) = self.previews.remove(&cue) else {
                continue;
            };
            let at = now.max(preview.started);
            if preview.paused_at.is_some() || at >= end {
                self.end_preview(cue, preview, None, cx);
                continue;
            }
            let fade = StopFade {
                frames: end - at,
                curve: PANIC_CURVE,
            };
            self.end_preview(cue, preview, Some(fade), cx);
            self.panicked_previews.push(PanickedPreview {
                cue,
                run: preview.run,
                started: preview.started,
            });
        }
    }

    /// Feedback: the voice of a preview ended by itself. Returns whether `run` was a preview's.
    /// An end dated on or after a pause comes from carrying out the pause and is ignored, as for
    /// a show cue.
    pub(super) fn preview_ended(
        &mut self,
        cue: CueId,
        run: RunId,
        at: SampleTime,
        cx: &mut Ctx,
    ) -> bool {
        if self.forget_panicked_preview(cue, run) {
            return true;
        }
        let Some(preview) = self.previews.get(&cue).copied() else {
            return false;
        };
        if preview.run != run {
            return false;
        }
        if preview.paused_at.is_some_and(|p| at >= p) {
            return true;
        }
        self.previews.remove(&cue);
        let at = at.max(preview.started);
        cx.out.push(Action::PreviewChanged {
            cue,
            active: false,
            at,
        });
        self.preview_gone(cue, at, cx);
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
        if self.forget_panicked_preview(cue, run) {
            return true;
        }
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

    /// Whether `run` was the voice of a preview fading out in a Panic (now forgotten: its voice
    /// is gone).
    fn forget_panicked_preview(&mut self, cue: CueId, run: RunId) -> bool {
        let found = (self.panicked_previews.iter()).position(|p| p.cue == cue && p.run == run);
        if let Some(i) = found {
            self.panicked_previews.swap_remove(i);
        }
        found.is_some()
    }

    /// Stops previews of cues that are gone from the workspace or are no longer audio cues, and
    /// group previews whose group is gone or no longer a group (with the audio cues they play).
    /// Returns whether any did.
    pub(super) fn sync_previews(&mut self, cx: &mut Ctx) -> bool {
        let ws = cx.ws;
        let groups: Vec<CueId> = (self.group_previews.keys())
            .copied()
            .filter(|g| {
                !ws.find_cue(*g)
                    .is_some_and(|c| matches!(c.kind, CueKind::Group { .. }))
            })
            .collect();
        let gone: Vec<CueId> = (self.previews.keys())
            .copied()
            .filter(|cue| {
                !ws.find_cue(*cue)
                    .is_some_and(|c| matches!(c.kind, CueKind::Audio { .. }))
            })
            .collect();
        let any = !gone.is_empty() || !groups.is_empty();
        for group in groups {
            self.stop_previews(Some(group), true, cx);
            // A group whose audio cues had all ended or moved on still goes.
            if self.group_previews.remove(&group).is_some() {
                cx.out.push(Action::PreviewChanged {
                    cue: group,
                    active: false,
                    at: cx.now,
                });
            }
        }
        for cue in gone {
            self.stop_previews(Some(cue), true, cx);
        }
        any
    }
}
