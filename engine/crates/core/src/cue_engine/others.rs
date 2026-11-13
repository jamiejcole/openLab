// SPDX-License-Identifier: GPL-3.0-or-later

//! What a cue does to the other cues when its action starts (QLab 5 Triggers tab, manual
//! pp. 129-130): "fade and stop others", and "duck or boost others" while it runs.
//!
//! The duck is carried by its own per-voice gain stage in the mixer (`Action::DuckVoice`), so it
//! multiplies whatever the cue's levels, a level edit or a Fade cue do: those land relative to
//! it, and a new voice (a new run, after a Panic too) starts with no duck at all.

use std::collections::{BTreeMap, HashSet};

use crate::cue::{FadeSpec, LevelLimits, MAX_DB, OthersScope, SILENCE_DB};
use crate::model::CueId;
use crate::tree;

use super::group::{ancestors, descendants};
use super::level::{self, LevelParam, db_to_gain};
use super::run::{Phase, Run, Slot, Voice};
use super::time::SampleTime;
use super::transport::PANIC_CURVE;
use super::types::Action;
use super::{Ctx, CueEngine};

/// A duck or boost in force: the cue doing it is in its action.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Duck {
    /// The change in dB, as set when its action started.
    db: f64,
    /// The ramp to it and back, in samples.
    frames: SampleTime,
}

/// The duck stage gain a voice plays at under `db` of ducks and boosts in all: silence at or
/// below [`SILENCE_DB`]; a boost only as far as keeps its master fader at the workspace maximum
/// (`master` is the loudest its master is at or moving to, linear), so no boost takes a fader
/// past what the workspace allows.
fn duck_gain(db: f64, master: f32, limits: LevelLimits) -> f32 {
    if db == 0.0 || db.is_nan() {
        return 1.0;
    }
    if db <= SILENCE_DB {
        return 0.0;
    }
    let db = if db > 0.0 {
        let master_db = if master > 0.0 {
            20.0 * f64::from(master).log10()
        } else {
            SILENCE_DB
        };
        db.min((limits.max_db - master_db).max(0.0)).min(MAX_DB)
    } else {
        db
    };
    db_to_gain(db)
}

/// The loudest the run's master fader is at or moving to (linear): what its voice was given,
/// and both ends of a Fade moving it.
fn master_of(run: &Run) -> f32 {
    let given = run
        .given
        .as_ref()
        .and_then(|g| g.played.get(&LevelParam::Master))
        .map_or(1.0, |p| p.0);
    run.levels
        .get(&LevelParam::Master)
        .map_or(given, |l| given.max(l.from).max(l.to))
}

impl CueEngine {
    /// Whether `cue` is ducking now: its run is in its action and not broken (paused, or fading
    /// out to a stop, it still is).
    fn ducking(&self, cue: CueId) -> bool {
        matches!(self.slots.get(&cue), Some(Slot::Active(run))
            if matches!(run.phase, Phase::Action { .. }) && !run.is_broken())
    }

    /// `cue`'s action started on `at` with "duck or boost others" on.
    pub(super) fn start_duck(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        let Some(model) = cx.ws.find_cue(cue) else {
            return;
        };
        let d = model.duck_others;
        let frames = cx.secs(d.duration, &self.config);
        self.ducks.insert(cue, Duck { db: d.db, frames });
        self.apply_ducks(at, frames, cx);
    }

    /// After a state change of `cue` on `at`: a duck it was doing that has ended (its action
    /// ended, it stopped, broke or was removed) is undone over its own time from `at`.
    pub(super) fn end_duck_if_over(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        if self.ducks.contains_key(&cue)
            && !self.ducking(cue)
            && let Some(d) = self.ducks.remove(&cue)
        {
            self.apply_ducks(at, d.frames, cx);
        }
    }

    /// Once per call, after everything else: a duck whose cue ended without a state report (a
    /// deleted cue) is undone, and a voice whose boost no longer fits under the maximum (an edit
    /// or a Fade raised its master) is brought down within the edit ramp. Nothing to do, and
    /// nothing looked at past two fields, while no cue ducks.
    pub(super) fn sync_ducks(&mut self, cx: &mut Ctx) {
        if self.ducks.is_empty() && !self.duck_live {
            return;
        }
        let over: Vec<CueId> = self
            .ducks
            .keys()
            .copied()
            .filter(|c| !self.ducking(*c))
            .collect();
        let now = cx.now;
        for cue in over {
            if let Some(d) = self.ducks.remove(&cue) {
                self.apply_ducks(now, d.frames, cx);
            }
        }
        let frames = level::edit_frames(self.config.sample_rate().get());
        self.apply_ducks(now, frames, cx);
    }

    /// The duck gain the voice of `cue` should play at: the sum of the ducks of the other cues of
    /// its list, as `duck_gain` puts it.
    fn duck_for(
        &self,
        cue: CueId,
        list: usize,
        ducks: &[(usize, HashSet<CueId>, f64)],
        run: &Run,
        limits: LevelLimits,
    ) -> f32 {
        let db: f64 = ducks
            .iter()
            .filter(|(l, own, _)| *l == list && !own.contains(&cue))
            .map(|(_, _, db)| db)
            .sum();
        duck_gain(db, master_of(run), limits)
    }

    /// The ducks in force, each with its list and the cues it does not duck (itself and what is
    /// inside it).
    fn duck_scopes(&self, cx: &Ctx) -> Vec<(usize, HashSet<CueId>, f64)> {
        let ws = cx.ws;
        self.ducks
            .iter()
            .filter_map(|(cue, d)| {
                let path = tree::find_path(ws, *cue)?;
                let mut own: HashSet<CueId> = ws
                    .find_cue(*cue)
                    .map(descendants)
                    .unwrap_or_default()
                    .into_iter()
                    .collect();
                own.insert(*cue);
                Some((path.list, own, d.db))
            })
            .collect()
    }

    /// Sends every playing audio voice whose duck gain is not what the ducks in force make it a
    /// ramp to it over `frames` from `at` (never before a command already committed for it).
    /// Voices fading out to a stop or in a panic are left as they are, so a duck coming off
    /// never brings up a sound that is going away.
    fn apply_ducks(&mut self, at: SampleTime, frames: SampleTime, cx: &mut Ctx) {
        let scopes = self.duck_scopes(cx);
        let limits = cx.ws.settings.limits();
        let ws = cx.ws;
        let mut wanted: BTreeMap<CueId, f32> = BTreeMap::new();
        let mut live = false;
        for (id, slot) in &self.slots {
            let Slot::Active(run) = slot else { continue };
            if !matches!(run.voice, Voice::Started { .. })
                || run.stop_end.is_some()
                || run.panicked
                || run.is_broken()
            {
                live |= run.duck != 1.0;
                continue;
            }
            let gain = match tree::find_path(ws, *id) {
                Some(path) => self.duck_for(*id, path.list, &scopes, run, limits),
                None => 1.0,
            };
            live |= gain != 1.0;
            if gain != run.duck {
                wanted.insert(*id, gain);
            }
        }
        for (cue, gain) in wanted {
            let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
                continue;
            };
            let Voice::Started { at: started, .. } = run.voice else {
                continue;
            };
            let at = run.commit(at.max(started));
            run.duck = gain;
            cx.out.push(Action::DuckVoice {
                cue,
                run: run.run,
                at,
                gain,
                frames,
            });
        }
        self.duck_live = live;
    }

    /// A voice just started on `at` for `cue`: while a duck is in force it starts at its duck
    /// (on its start sample, so nothing of it is heard unducked).
    pub(super) fn duck_new_voice(&mut self, cue: CueId, cx: &mut Ctx) {
        if self.ducks.is_empty() {
            return;
        }
        let scopes = self.duck_scopes(cx);
        let limits = cx.ws.settings.limits();
        let Some(list) = tree::find_path(cx.ws, cue).map(|p| p.list) else {
            return;
        };
        let Some(Slot::Active(run)) = self.slots.get(&cue) else {
            return;
        };
        let gain = self.duck_for(cue, list, &scopes, run, limits);
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        let Voice::Started { at, .. } = run.voice else {
            return;
        };
        if gain != run.duck {
            run.duck = gain;
            self.duck_live |= gain != 1.0;
            cx.out.push(Action::DuckVoice {
                cue,
                run: run.run,
                at,
                gain,
                frames: 0,
            });
        }
    }
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
