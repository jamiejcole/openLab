// SPDX-License-Identifier: GPL-3.0-or-later

//! What a cue does to the other cues when its action starts (QLab 5 Triggers tab, manual
//! pp. 129-130): "fade and stop others".

use std::collections::HashSet;

use crate::cue::{FadeSpec, OthersScope};
use crate::model::CueId;
use crate::tree;

use super::group::{ancestors, descendants};
use super::run::Slot;
use super::transport::PANIC_CURVE;
use super::{Ctx, CueEngine};

impl CueEngine {
    /// `cue`'s action has started with "fade and stop others" on: every other cue in its scope
    /// that is active fades out over the set time and stops, children before their groups, with
    /// the Stop operation (a playing voice fades; a paused, pre-waiting or post-waiting cue, or
    /// one without a voice, stops at once; a group waits for its children). The cue itself, the
    /// groups around it and the cues inside it are not "others". Loaded cues stay loaded; a cue
    /// already fading out (a stop with a fade, a panic) keeps its own fade.
    pub(super) fn fade_and_stop_others(&mut self, cue: CueId, cx: &mut Ctx) {
        let ws = cx.ws;
        let Some(model) = ws.find_cue(cue) else {
            return;
        };
        let settings = model.fade_and_stop_others;
        let Some(path) = tree::find_path(ws, cue) else {
            return;
        };
        let around = ancestors(ws, cue);
        let mut mine: HashSet<CueId> = descendants(model).into_iter().collect();
        mine.insert(cue);
        mine.extend(around.iter().copied());
        let parent = around.first().copied();
        // (depth, cue) of every active run in scope; only active runs can be stopped, so the
        // scan costs the cues that run, not the show.
        let mut targets: Vec<(usize, CueId)> = Vec::new();
        for (id, slot) in &self.slots {
            if mine.contains(id) {
                continue;
            }
            let Slot::Active(run) = slot else { continue };
            if run.stop_end.is_some() || run.is_group_stopping() || run.panicked {
                continue;
            }
            let Some(other) = tree::find_path(ws, *id) else {
                continue;
            };
            let groups = ancestors(ws, *id);
            let inside = match settings.scope {
                OthersScope::All => true,
                OthersScope::List => other.list == path.list,
                // A peer is another cue at the cue's own level, or anything inside one.
                OthersScope::Peers => {
                    other.list == path.list
                        && match parent {
                            Some(p) => groups.contains(&p),
                            None => true,
                        }
                }
            };
            if inside {
                targets.push((groups.len(), *id));
            }
        }
        // Deepest first: children before the groups around them.
        targets.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let fade = FadeSpec {
            duration: settings.duration,
            curve: PANIC_CURVE,
        };
        for (_, id) in targets {
            self.stop_one(id, Some(fade), cx);
        }
    }
}
