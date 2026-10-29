// SPDX-License-Identifier: GPL-3.0-or-later

//! GO and the playheads: GO, double-GO protection, moving a list's playhead, and which groups a
//! playhead is visiting (docs/cue-engine.md, "GO and double-GO", "Playhead").

use crate::cue::Cue;
use crate::model::{CueId, CueListId, Workspace};

use super::playhead::{self, Playhead};
use super::types::{Action, EngineWarning, GoIgnoredReason};
use super::{Ctx, CueEngine};

impl CueEngine {
    // ----- GO and playheads -----

    pub(super) fn go(&mut self, list: CueListId, cx: &mut Ctx) {
        let ignored = |reason, cx: &mut Ctx| cx.out.push(Action::GoIgnored { list, reason });
        if !cx.ws.cue_lists.iter().any(|l| l.id == list) {
            return ignored(GoIgnoredReason::UnknownList, cx);
        }
        if self.double_go(cx) {
            return ignored(GoIgnoredReason::DoubleGo, cx);
        }
        if let Err(reason) = self.go_on(list, cx.now, cx) {
            return ignored(reason, cx);
        }
        self.last_go = Some(cx.now);
    }

    /// Whether a GO now is within `min_go_interval` of the last one. A clock that went backwards
    /// (device restart) never blocks GO.
    fn double_go(&self, cx: &Ctx) -> bool {
        let interval = cx.secs(cx.ws.settings.min_go_interval, &self.config);
        self.last_go
            .is_some_and(|last| cx.now >= last && cx.now - last < interval)
    }

    /// GO from `cue`: only if the playhead can rest there.
    pub(super) fn go_from(&mut self, list: CueListId, cue: CueId, cx: &mut Ctx) {
        let Some(cues) = cx
            .ws
            .cue_lists
            .iter()
            .find(|l| l.id == list)
            .map(|l| &l.cues)
        else {
            return cx.warn(EngineWarning::UnknownList { list });
        };
        if !playhead::positions(cues).contains(&cue) {
            return cx.warn(EngineWarning::NotInList { list, cue });
        }
        // A GO that double-GO protection would refuse leaves the playhead where it was.
        if self.double_go(cx) {
            return cx.out.push(Action::GoIgnored {
                list,
                reason: GoIgnoredReason::DoubleGo,
            });
        }
        self.move_playhead(list, Some(cue), cx);
        self.go(list, cx);
    }

    pub(super) fn navigate(
        &mut self,
        list: CueListId,
        cx: &mut Ctx,
        step: fn(&mut Playhead, &[Cue]) -> bool,
    ) {
        let Some(cues) = cx
            .ws
            .cue_lists
            .iter()
            .find(|l| l.id == list)
            .map(|l| &l.cues)
        else {
            return cx.warn(EngineWarning::UnknownList { list });
        };
        if let Some(playhead) = self.playheads.get_mut(&list)
            && step(playhead, cues)
        {
            cx.out.push(Action::PlayheadChanged {
                list,
                cue: playhead.cue,
            });
            self.forget_continues(cx.ws, list, None);
        }
    }

    pub(super) fn set_playhead(&mut self, list: CueListId, cue: Option<CueId>, cx: &mut Ctx) {
        let Some(cues) = cx
            .ws
            .cue_lists
            .iter()
            .find(|l| l.id == list)
            .map(|l| &l.cues)
        else {
            return cx.warn(EngineWarning::UnknownList { list });
        };
        if let Some(id) = cue
            && !playhead::positions(cues).contains(&id)
        {
            return cx.warn(EngineWarning::NotInList { list, cue: id });
        }
        self.move_playhead(list, cue, cx);
    }

    pub(super) fn set_playhead_by_number(&mut self, list: CueListId, number: String, cx: &mut Ctx) {
        let Some(cues) = cx
            .ws
            .cue_lists
            .iter()
            .find(|l| l.id == list)
            .map(|l| &l.cues)
        else {
            return cx.warn(EngineWarning::UnknownList { list });
        };
        let numbered = |c: &&Cue| c.number.as_deref() == Some(number.as_str());
        match playhead::position_cues(cues).into_iter().find(numbered) {
            Some(cue) => self.move_playhead(list, Some(cue.id), cx),
            None => cx.warn(EngineWarning::NumberNotFound { list, number }),
        }
    }

    pub(super) fn move_playhead(&mut self, list: CueListId, cue: Option<CueId>, cx: &mut Ctx) {
        self.move_playhead_after(list, cue, None, cx);
    }

    /// Moves the playhead; `gone` is the cue a GO started, whose own enter step (into an
    /// armed "start first and enter" group) is the one way into a group that keeps what the
    /// group's run fired (see `forget_continues`).
    pub(super) fn move_playhead_after(
        &mut self,
        list: CueListId,
        cue: Option<CueId>,
        gone: Option<CueId>,
        cx: &mut Ctx,
    ) {
        if let Some(playhead) = self.playheads.get_mut(&list)
            && playhead.set(cue)
        {
            cx.out.push(Action::PlayheadChanged { list, cue });
            self.forget_continues(cx.ws, list, gone);
        }
    }

    /// The list's playhead moved, or the list was edited: every group the playhead is in now
    /// and was not in when the engine last looked (`Playhead::around`) has been entered, and,
    /// unless it was entered by the GO on that group itself (`gone`), forgets that its run fired
    /// its continue. That memory belongs to the visit it was made in, so leaving a group
    /// entered afresh (a jump, stepping, a GoTo, an edit that moves the playhead or the cue into
    /// it) stands on the cue right after it, whatever the group did earlier in the show; an edit
    /// that leaves the playhead in the same groups keeps the visit (see `playhead::after_go`).
    pub(super) fn forget_continues(
        &mut self,
        ws: &Workspace,
        list: CueListId,
        gone: Option<CueId>,
    ) {
        let Some(cues) = ws.cue_lists.iter().find(|l| l.id == list).map(|l| &l.cues) else {
            return;
        };
        let Some(id) = self.playheads.get(&list).map(|p| p.cue) else {
            return;
        };
        let now = id
            .map(|id| playhead::groups_around(cues, id))
            .unwrap_or_default();
        self.enter_groups(list, now, gone);
    }

    /// [`Self::forget_continues`] with the groups around the playhead now (`now`, outermost
    /// first) already found.
    pub(super) fn enter_groups(&mut self, list: CueListId, now: Vec<CueId>, gone: Option<CueId>) {
        let Some(playhead) = self.playheads.get_mut(&list) else {
            return;
        };
        let before = std::mem::replace(&mut playhead.around, now);
        for group in &playhead.around {
            if !before.contains(group) && Some(*group) != gone {
                self.continued.remove(group);
            }
        }
    }
}
