// SPDX-License-Identifier: GPL-3.0-or-later

//! The child each Playlist group plays next (QLab 5.5 marks it in the cue list), kept for the
//! client and reported as it changes (`Action::PlaylistNextChanged`).

use std::collections::{BTreeSet, HashMap};

use crate::cue::{Cue, CueKind, GroupMode, SecondTrigger};
use crate::model::{CueId, Workspace};

use super::operations::StartPath;
use super::run::{Phase, Run, Slot, Voice};
use super::time::SampleTime;
use super::{Action, Ctx, CueEngine};

/// What an edit not yet followed changed, for the marks: every cue, or these.
pub(super) enum MarksEdit {
    All,
    Cues(BTreeSet<CueId>),
}

impl CueEngine {
    /// The child the Playlist group `group` plays next. Not running: the first child that a
    /// start plays, as `start` decides ([`Self::start_path`]): a run of its own is kept as the
    /// one playing (a broken one too) unless its second trigger ends it (a stop, or a panic
    /// that stops it at once rather than fading it out); a cue broken at run time with no run,
    /// one left paused by a lost device still away, a skipped one and a new run of one with
    /// static problems are passed over. A looping one goes round again after every pass that
    /// plays none but started a run, to the children whose runs it ended as new runs. Playing:
    /// the one after its current child as the group stands (wrapping when it loops, the current
    /// one as a new run). None once it is stopping, devamped or halted, past its last child, or
    /// for a shuffled playlist whose order is not drawn yet (not running, or between passes);
    /// none too, before its action, for a disarmed one or one loaded to a time at or past its
    /// span (its start plays no child), and for any other cue.
    /// `Action::PlaylistNextChanged` reports each change.
    pub fn next_in_playlist(&self, group: CueId) -> Option<CueId> {
        self.playlist_marks.get(&group).copied()
    }

    /// Every Playlist group with a next child, in group id order.
    pub fn playlist_marks(&self) -> impl Iterator<Item = (CueId, CueId)> + '_ {
        self.playlist_marks.iter().map(|(g, c)| (*g, *c))
    }

    /// The cues the pending edit touched, with the groups they were in before it (the index is
    /// not yet brought up to date): taken before the edit is followed.
    pub(super) fn marks_edit_before_sync(&self) -> MarksEdit {
        match self.pending_cues() {
            None => MarksEdit::All,
            Some(touched) => {
                let tree = self.problems.tree();
                let parents: Vec<CueId> = touched.iter().filter_map(|c| tree.parent(*c)).collect();
                let mut cues: BTreeSet<CueId> = touched.into_iter().collect();
                cues.extend(parents);
                MarksEdit::Cues(cues)
            }
        }
    }

    /// Brings the marks up to date at the end of a call and reports each that changed, in group id
    /// order. Looked at: every group after an edit with no footprint; else the cues an edit touched,
    /// the groups they were and are in, and the groups inside a touched group (an added subtree);
    /// the groups that had a mark (one may be gone); every group with a run; each lost or broken
    /// cue, with its group; the groups around a run whose voice start or a committed command is
    /// still ahead; and each cue whose state, arming or length changed in this call, with the
    /// groups around it. So a call costs what it touched (and how deep it is nested).
    pub(super) fn sync_playlist_marks(&mut self, edit: Option<MarksEdit>, cx: &mut Ctx) {
        let ws = cx.ws;
        let tree = self.problems.tree();
        let mut look: BTreeSet<CueId> = BTreeSet::new();
        match edit {
            None => {}
            Some(MarksEdit::All) => {
                self.whole_show_passes = self.whole_show_passes.saturating_add(1);
                look.extend(ws.cues().filter(|c| is_playlist(c)).map(|c| c.id));
                look.extend(self.playlist_marks.keys().copied());
            }
            Some(MarksEdit::Cues(cues)) => {
                for id in &cues {
                    look.insert(*id);
                    look.extend(tree.parent(*id));
                    if let Some(cue) = tree.locate(ws, *id)
                        && !cue.children().is_empty()
                    {
                        look.extend(super::group::descendants(cue));
                    }
                }
                look.extend(self.playlist_marks.keys().copied());
            }
        }
        for (id, slot) in &self.slots {
            match slot {
                Slot::Active(Run { group: Some(_), .. }) => {
                    look.insert(*id);
                }
                // Whether a start plays it can change with no state change (its device comes
                // back; a broken run settles into a break, or its frozen timers end): its group
                // (or it, a group) looks again on every call while it is lost or broken.
                Slot::Lost(_) | Slot::Broken(_) => {
                    look.insert(*id);
                    look.extend(tree.parent(*id));
                }
                Slot::Active(r) if r.is_broken() => {
                    look.insert(*id);
                    look.extend(tree.parent(*id));
                }
                // Whether a panic fades a voice out or stops it at once depends on `now` while
                // the voice's start or a command for it is still ahead: the playlists around it
                // look again on every call until then.
                Slot::Active(r) if r.committed > cx.now || voice_starts_after(r, cx.now) => {
                    look.extend(tree.ancestors(*id));
                }
                _ => {}
            }
        }
        // A length that arrived, or a cue that broke or was disarmed, may put every playlist
        // above it loaded to a time past its span (the span counts nested children).
        for cue in std::mem::take(&mut self.lengths_changed) {
            look.extend(tree.ancestors(cue));
        }
        for action in &cx.out {
            let cue = match action {
                Action::StateChanged { cue, .. } | Action::SetArmed { cue, .. } => *cue,
                _ => continue,
            };
            look.insert(cue);
            look.extend(tree.ancestors(cue));
        }
        let mut changed = Vec::new();
        for id in look {
            let next = tree
                .locate(ws, id)
                .filter(|c| is_playlist(c))
                .and_then(|group| self.compute_next(group, cx));
            if self.playlist_marks.get(&id).copied() != next {
                changed.push((id, next));
            }
        }
        for (group, next) in changed {
            match next {
                Some(child) => self.playlist_marks.insert(group, child),
                None => self.playlist_marks.remove(&group),
            };
            cx.out.push(Action::PlaylistNextChanged { group, next });
        }
    }

    /// [`Self::next_in_playlist`] worked out from the group's run and children now.
    fn compute_next(&self, group: &Cue, cx: &Ctx) -> Option<CueId> {
        let (ws, now) = (cx.ws, cx.now);
        let CueKind::Group {
            mode: GroupMode::Playlist { loop_, shuffle },
            children,
        } = &group.kind
        else {
            return None;
        };
        // One pass over `kids` as `playlist_next` makes it: the child that plays, else whether a
        // child started a run in it. The runs it ends join `ended`, idle when it meets them again,
        // as is the child playing now (it has ended by the time the playlist moves on).
        let pass = |kids: &[Cue], ended: &mut Vec<CueId>| {
            let mut started = false;
            for c in kids {
                match self.child_meets(c, ended.contains(&c.id), now, ws) {
                    Meet::Plays => return (Some(c.id), started),
                    Meet::Passed {
                        started: s,
                        ended: e,
                    } => {
                        started |= s;
                        if e {
                            ended.push(c.id);
                        }
                    }
                }
            }
            (None, started)
        };
        // The pass from `from`, then round again while it loops: a running playlist goes round
        // at the end of its pass, and after any pass in which a child started a run (that run's
        // end moves it on again); a start of the playlist opens with a pass, so it goes round only
        // that way. A round that ended no run is the same as the one after it: none plays.
        let walk = |from: usize, idle: Option<CueId>, opens_with_pass: bool| {
            let mut ended: Vec<CueId> = idle.into_iter().collect();
            let (mut next, mut started) =
                pass(children.get(from..).unwrap_or_default(), &mut ended);
            let mut round = *loop_ && (started || !opens_with_pass);
            while next.is_none() && round {
                let before = ended.len();
                (next, started) = pass(children, &mut ended);
                round = started && ended.len() > before;
            }
            next
        };
        // A disarmed group keeps only its timing: its start plays no child.
        let plays_children = !*shuffle && self.is_armed(group);
        let first = || plays_children.then(|| walk(0, None, true)).flatten();
        let run = match self.slots.get(&group.id) {
            // Loaded to a time at or past its span, its start plays no child either.
            Some(Slot::Loaded(l)) if self.playlist_spent(children, *loop_, l.action_offset, cx) => {
                return None;
            }
            Some(Slot::Active(run)) => run,
            _ => return first(),
        };
        let Some(g) = &run.group else {
            return first();
        };
        if g.stopping || g.devamp.is_some() || g.halt.is_some() {
            return None;
        }
        match run.phase {
            Phase::PreWait { .. } => return first(),
            Phase::Done => return None,
            Phase::Action { .. } => {}
        }
        let Some(p) = &g.playlist else {
            return first();
        };
        if !p.order.is_empty() {
            let from = if p.current.is_some() {
                p.pos.saturating_add(1)
            } else {
                p.pos
            };
            // By id, so a long pass of children passed over costs one lookup each.
            let by_id: HashMap<CueId, &Cue> = children.iter().map(|c| (c.id, c)).collect();
            return p
                .order
                .iter()
                .skip(from)
                .filter_map(|id| by_id.get(id).copied())
                .find(|c| self.child_meets(c, false, now, ws) == Meet::Plays)
                .map(|c| c.id);
        }
        // As the group stands now: after the current child, or the one that took its place.
        let from = p
            .current
            .and_then(|(c, _)| children.iter().position(|k| k.id == c))
            .map_or(p.index, |i| i.saturating_add(1));
        walk(from, p.current.map(|(c, _)| c), false)
    }

    /// What a start of the playlist's child `c` does, by what `start` does with its runtime
    /// (`idle`: as if it had none). One left as it is is taken as the child playing if it is a
    /// run (a broken one, or one fading out in a panic); a trigger that ends the run passes it
    /// and leaves it idle; a new run of a cue with static problems only keeps its timing (C-G3),
    /// so it is passed though it started.
    fn child_meets(&self, c: &Cue, idle: bool, now: SampleTime, ws: &Workspace) -> Meet {
        let fresh = || {
            if !self.is_skipped(c) && !self.has_problems(c.id) {
                Meet::Plays
            } else {
                Meet::Passed {
                    started: !self.is_skipped(c),
                    ended: false,
                }
            }
        };
        if idle {
            return fresh();
        }
        match self.start_path(c) {
            StartPath::Refused | StartPath::Skipped
                if matches!(self.slots.get(&c.id), Some(Slot::Active(_))) =>
            {
                Meet::Plays
            }
            StartPath::Refused | StartPath::Skipped => Meet::Passed {
                started: false,
                ended: false,
            },
            StartPath::Trigger(SecondTrigger::HardStopAndRestart) | StartPath::Fresh => fresh(),
            StartPath::Trigger(option) if self.trigger_stops(c, option, now, ws) => Meet::Passed {
                started: false,
                ended: true,
            },
            StartPath::Trigger(_) | StartPath::Resume => Meet::Plays,
        }
    }
}

/// What a playlist does with a child it meets ([`CueEngine::child_meets`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Meet {
    Plays,
    /// Passed over: `started` a run of it (one that ends at once, or keeps only its timing),
    /// `ended` the run it had.
    Passed {
        started: bool,
        ended: bool,
    },
}

fn voice_starts_after(run: &Run, now: SampleTime) -> bool {
    matches!(run.voice, Voice::Started { at, .. } if at > now)
}

fn is_playlist(cue: &Cue) -> bool {
    matches!(
        cue.kind,
        CueKind::Group {
            mode: GroupMode::Playlist { .. },
            ..
        }
    )
}
