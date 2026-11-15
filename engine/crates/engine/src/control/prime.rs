// SPDX-License-Identifier: GPL-3.0-or-later

//! Priming: which cues each doc keeps ready to play, and how the engine's prime pool is shared
//! between the open docs.

use std::collections::HashSet;

use openlab_core::{
    Cue, CueContainer, CueEngine, CueId, CueIndex, CueKind, SampleTime, Workspace, descendants,
};

use super::voices::Env;
use super::{Control, DocCx, DocId, Engine};

/// New primes one doc makes per pass while the pool is contended: a doc taking over another's
/// share fills its rings a few at a time, not in one disk burst beside a playing show.
const PRIME_BURST: usize = 4;

/// `pool` shared out max-min fairly between `wants` (in the same order): a want that fits its
/// fair share is met, and what it leaves goes to the larger ones. The shares sum to at most
/// `pool`, and to the sum of the wants when they fit.
fn share_out(pool: usize, wants: &[usize]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..wants.len()).collect();
    order.sort_by_key(|&i| wants.get(i).copied().unwrap_or(0));
    let mut shares = vec![0; wants.len()];
    let mut left = pool;
    for (k, &i) in order.iter().enumerate() {
        let fair = left / (wants.len() - k);
        let take = wants.get(i).copied().unwrap_or(0).min(fair);
        if let Some(s) = shares.get_mut(i) {
            *s = take;
        }
        left -= take;
    }
    shares
}

impl Control {
    /// The docs' prime allowances follow each other's wants: a doc holding more than it may
    /// now lets go (least urgent first, at its next pass), and one held short while another
    /// let go takes its share up. A doc's wants change only at its own passes, so an idle
    /// doc opening beside a busy one makes no doc re-prime.
    pub(super) fn balance_primes(&mut self) {
        // Nothing to look at on the (usual) wake where no doc primed and none opened or
        // closed; one doc alone never has another to make room for.
        if !std::mem::take(&mut self.eng.primes_changed) || self.docs.len() < 2 {
            return;
        }
        for doc in &mut self.docs {
            let Some(usage) = self.eng.primes.get(&doc.id).copied() else {
                continue;
            };
            let allowed = self.eng.prime_allowance(doc.id);
            // Not on a mere shortfall of primes (a probe pending, a file missing): only a
            // change of allowance, so an unchanged show costs no priming passes.
            if usage.held > allowed || (usage.cap < usage.want && allowed > usage.cap) {
                doc.primes_dirty = true;
            }
        }
    }
}

impl Engine {
    /// Primed streams the whole engine may hold: `max_primes`, and never
    /// more than half the disk streamer's open-stream limit, which is the engine's. A start
    /// that finds no permit fails (`stream_failed`), and primes are only a head start, so the
    /// other half is always left for starts. 64 with the defaults.
    fn prime_pool(&self) -> usize {
        self.config
            .max_primes
            .min(self.config.streamer.max_streams / 2)
    }

    /// What doc `doc` may hold of the prime pool now, by demand (review M-C): every doc's
    /// wants as of its last priming pass, shared out max-min fairly ([`share_out`]). While the
    /// wants fit the pool each doc gets all it wants, so an idle doc beside a busy show takes
    /// nothing from it; only when they do not fit is each held to its fair share.
    fn prime_allowance(&self, doc: DocId) -> usize {
        let pool = self.prime_pool();
        let wants: Vec<usize> = self.primes.values().map(|u| u.want).collect();
        let shares = share_out(pool, &wants);
        self.primes
            .keys()
            .zip(shares)
            .find(|(d, _)| **d == doc)
            .map_or(pool, |(_, s)| s)
    }

    /// Whether the docs' prime wants together exceed the pool.
    fn primes_contended(&self) -> bool {
        self.primes.values().map(|u| u.want).sum::<usize>() > self.prime_pool()
    }
}

impl DocCx<'_> {
    /// Keeps the cues near each playhead primed.
    pub(super) fn prime(&mut self, now: SampleTime) {
        let wanted = prime_set(
            self.doc.model.ws(),
            self.doc.model.engine(),
            self.doc.media.index(),
            self.eng.config.prime_cues,
        );
        // Their files are probed first (H2: a restart mid-show must not leave the cue at the
        // playhead behind a hundred others).
        let keys: Vec<String> = wanted
            .iter()
            .filter_map(
                |c| match &self.doc.media.index().locate(self.doc.model.ws(), *c)?.kind {
                    CueKind::Audio { media: Some(m), .. } => Some(m.as_str().to_owned()),
                    _ => None,
                },
            )
            .collect();
        self.doc.media.prioritize(keys.iter().map(String::as_str));
        let floor = self.stop_floor();
        let want = wanted.len().min(self.eng.config.max_primes);
        let id = self.doc.id;
        let entry = self.eng.primes.entry(id).or_default();
        let before = (entry.want, entry.held, entry.cap);
        entry.want = want;
        let cap = self.eng.prime_allowance(id);
        // Fills only a few new rings per pass while docs share a short pool.
        let burst = if self.eng.primes_contended() {
            PRIME_BURST
        } else {
            usize::MAX
        };
        let mut env = Env {
            doc: self.doc.id,
            ws: self.doc.model.ws(),
            media: &self.doc.media,
            streamer: &mut self.eng.streamer,
            patches: &self.eng.patches,
            ids: &mut self.eng.voice_ids,
            rate: self.eng.rate,
            cue_outputs: self.eng.cue_outputs,
            now,
            floor,
        };
        let cut = self.doc.voices.prime(&wanted, cap, burst, &mut env);
        let held = self.doc.voices.primed().count();
        let entry = self.eng.primes.entry(id).or_default();
        entry.held = held;
        entry.cap = cap;
        if before != (want, held, cap) {
            self.eng.primes_changed = true;
        }
        if cut {
            // The rest of the burst-limited fill comes on the next passes.
            self.doc.primes_dirty = true;
        }
    }
}

/// The cues to keep primed: from each list's playhead the next `per_list` cues (any kind; the
/// audio ones count), the targets of those cues, then every cue whose hotkey trigger is on. Disarmed and running cues are left out (a start of a running cue makes
/// a new voice anyway).
///
/// Reads the playhead window and the targets through the cue index, so they cost the cues
/// named and their depth, not the length of the lists (#411). The trigger pass still tests
/// every cue's two trigger fields (a walk that allocates a small stack, no per-cue search),
/// which a trigger set kept up from edit footprints would save only at the price of a second
/// index that could go stale. Cue ids are unique (commands and opening refuse duplicates); if
/// they were not, the index places the first cue with an id, as `find_cue` does.
fn prime_set(ws: &Workspace, engine: &CueEngine, index: &CueIndex, per_list: usize) -> Vec<CueId> {
    let mut wanted = Wanted::default();
    let mut targets = Vec::new();
    for list in &ws.cue_lists {
        let Some(head) = engine.playhead(list.id) else {
            continue;
        };
        let in_list = index.contains(head) && index.list_of(head) == Some(list.id);
        let mut at = if in_list {
            Some(head)
        } else {
            index.kids(CueContainer::List(list.id)).first().copied()
        };
        for _ in 0..per_list {
            let Some(id) = at else { break };
            if let Some(cue) = index.locate(ws, id) {
                wanted.add(cue, engine);
                if let Some(t) = cue.kind.target() {
                    targets.push(t);
                }
            }
            at = index.next_in_list(id);
        }
    }
    for t in targets {
        if let Some(c) = index.locate(ws, t) {
            // A group target primes its audio children.
            for c in descendants(std::slice::from_ref(c)) {
                wanted.add(c, engine);
            }
        }
    }
    wanted.add_triggered(ws, engine);
    wanted.list
}

/// [`prime_set`] by walking each list's positions and searching the workspace for every cue:
/// the reference it is tested against.
#[cfg(test)]
fn prime_set_walk(ws: &Workspace, engine: &CueEngine, per_list: usize) -> Vec<CueId> {
    let mut wanted = Wanted::default();
    let mut targets = Vec::new();
    for list in &ws.cue_lists {
        let Some(head) = engine.playhead(list.id) else {
            continue;
        };
        let positions = CueEngine::playhead_positions(ws, list.id);
        let from = positions.iter().position(|&c| c == head).unwrap_or(0);
        for &id in positions.iter().skip(from).take(per_list) {
            if let Some(cue) = ws.find_cue(id) {
                wanted.add(cue, engine);
                if let Some(t) = cue.kind.target() {
                    targets.push(t);
                }
            }
        }
    }
    for t in targets {
        if let Some(c) = ws.find_cue(t) {
            for c in descendants(std::slice::from_ref(c)) {
                wanted.add(c, engine);
            }
        }
    }
    wanted.add_triggered(ws, engine);
    wanted.list
}

/// The cues to prime, in the order they were found, each once.
#[derive(Default)]
struct Wanted {
    list: Vec<CueId>,
    seen: HashSet<CueId>,
}

impl Wanted {
    fn add(&mut self, cue: &Cue, engine: &CueEngine) {
        let ok = cue.armed
            && matches!(cue.kind, CueKind::Audio { media: Some(_), .. })
            && engine.run_of(cue.id).is_none();
        if ok && self.seen.insert(cue.id) {
            self.list.push(cue.id);
        }
    }

    fn add_triggered(&mut self, ws: &Workspace, engine: &CueEngine) {
        for c in ws.cues() {
            if c.triggers.active_hotkey().is_some() {
                self.add(c, engine);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlab_core::{
        CueListId, EngineConfig, GroupMode, Hotkey, Input, MediaRef, Modifiers, load_show,
    };

    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as usize % n.max(1)
        }
    }

    fn cue_id(n: u128) -> CueId {
        CueId(uuid::Uuid::from_u128(n))
    }

    /// A random run of cues: audio with and without a file, groups (nested), starts aimed at
    /// cues of the whole show, some disarmed, some with triggers.
    fn tree(rng: &mut Rng, template: &Cue, next: &mut u128, budget: usize) -> Vec<Cue> {
        let mut out = Vec::new();
        while out.len() < budget {
            *next += 1;
            let mut c = template.clone();
            c.id = cue_id(*next);
            c.number = None;
            c.armed = rng.below(5) != 0;
            c.triggers.hotkey = (rng.below(8) == 0).then(|| Hotkey {
                key: "a".into(),
                modifiers: Modifiers::default(),
            });
            c.triggers.hotkey_enabled = rng.below(4) != 0;
            match rng.below(6) {
                0 => {
                    let inner = rng.below(6);
                    let children = tree(rng, template, next, inner);
                    c.kind = CueKind::Group {
                        mode: GroupMode::Timeline,
                        children,
                    };
                }
                1 => {
                    let target = cue_id(1 + u128::try_from(rng.below(40)).unwrap_or(0));
                    c.kind = CueKind::Start {
                        target: Some(target),
                    };
                }
                2 => {
                    if let CueKind::Audio { media, .. } = &mut c.kind {
                        *media = None;
                    }
                }
                _ => {
                    if let CueKind::Audio { media, .. } = &mut c.kind {
                        *media = Some(MediaRef::new("a.wav"));
                    }
                }
            }
            out.push(c);
        }
        out
    }

    fn example() -> (Workspace, Cue) {
        let text = include_str!("../../../../fixtures/show/v1-example.openlab");
        let mut ws = load_show(text).unwrap().workspace;
        let id = openlab_core::media_of_cues(&ws)[0].0;
        let template = ws.find_cue(id).unwrap().clone();
        for list in &mut ws.cue_lists {
            list.cues.clear();
        }
        (ws, template)
    }

    /// The indexed priming set equals the walk's for random shows (groups, targets, triggers,
    /// disarmed and running cues, two lists, a playhead on a random cue of each list or past the
    /// end).
    #[test]
    fn prime_set_equals_a_full_walk() {
        let (mut base, template) = example();
        let mut second = base.cue_lists[0].clone();
        second.id = CueListId(uuid::Uuid::from_u128(9_999_999));
        base.cue_lists.push(second);
        let (mut nonempty, mut with_targets) = (0, 0);
        for seed in 1..=60u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let mut ws = base.clone();
            let mut next = 0;
            for list in &mut ws.cue_lists {
                let n = 3 + rng.below(25);
                list.cues = tree(&mut rng, &template, &mut next, n);
            }
            let mut engine = CueEngine::new(EngineConfig::new(1000, 5, 1).unwrap());
            let _ = engine.handle(&ws, Input::Tick, 0);
            let all: Vec<CueId> = ws.cues().map(|c| c.id).collect();
            for list in ws.cue_lists.clone() {
                let cue = all.get(rng.below(all.len() + 1)).copied();
                let _ = engine.handle(&ws, Input::SetPlayhead { list: list.id, cue }, 0);
                if rng.below(2) == 0 {
                    let _ = engine.handle(&ws, Input::Go { list: list.id }, 0);
                }
            }
            let index = CueIndex::build(&ws);
            for per_list in [0, 1, 3, 8, 100] {
                let got = prime_set(&ws, &engine, &index, per_list);
                assert_eq!(
                    got,
                    prime_set_walk(&ws, &engine, per_list),
                    "seed {seed} per_list {per_list}"
                );
                nonempty += usize::from(!got.is_empty());
            }
            with_targets += ws
                .cues()
                .filter(|c| c.kind.target().is_some_and(|t| ws.find_cue(t).is_some()))
                .count();
        }
        assert!(nonempty > 200, "sets with cues: {nonempty}");
        assert!(with_targets > 50, "starts with a target: {with_targets}");
    }

    fn audio(template: &Cue, n: u128) -> Cue {
        let mut c = template.clone();
        c.id = cue_id(n);
        c.number = None;
        c.armed = true;
        c.continue_mode = openlab_core::ContinueMode::DoNotContinue;
        c.triggers.hotkey = None;
        c.triggers.hotkey_enabled = true;
        if let CueKind::Audio { media, .. } = &mut c.kind {
            *media = Some(MediaRef::new("a.wav"));
        }
        c
    }

    /// Exact answers for the cases the property test cannot name: a running cue and a disarmed
    /// one in the window are left out, a cue with a hotkey is added after the window but one
    /// whose hotkey trigger is off is not, and a playhead standing on a cue of another list
    /// starts the list at its first cue.
    #[test]
    fn prime_set_leaves_out_running_and_disarmed_and_adds_triggers() {
        let (mut ws, template) = example();
        let mut second = ws.cue_lists[0].clone();
        second.id = CueListId(uuid::Uuid::from_u128(9_999_999));
        ws.cue_lists.push(second);
        let n = |k: u128| audio(&template, k);
        let (mut disarmed, mut off, mut hotkey) = (n(2), n(4), n(6));
        disarmed.armed = false;
        let key = Hotkey {
            key: "a".into(),
            modifiers: Modifiers::default(),
        };
        off.triggers.hotkey = Some(key.clone());
        off.triggers.hotkey_enabled = false;
        hotkey.triggers.hotkey = Some(key);
        ws.cue_lists[0].cues = vec![n(1), disarmed, n(3), off, n(5), hotkey, n(10)];
        ws.cue_lists[1].cues = vec![n(10), n(11), n(12), n(13)];
        let first = ws.cue_lists[0].id;
        let mut engine = CueEngine::new(EngineConfig::new(1000, 5, 1).unwrap());
        let _ = engine.handle(&ws, Input::Tick, 0);
        let at = Some(cue_id(1));
        let _ = engine.handle(
            &ws,
            Input::SetPlayhead {
                list: first,
                cue: at,
            },
            0,
        );
        let _ = engine.handle(&ws, Input::Start { cue: cue_id(1) }, 0);
        assert!(engine.run_of(cue_id(1)).is_some(), "cue 1 runs");
        // Cue 10 moves from the second list to the end of the first: the second list's
        // playhead is on it still.
        let moved = ws.cue_lists[1].cues.remove(0);
        ws.cue_lists[0].cues.pop();
        ws.cue_lists[0].cues.push(moved);
        assert_eq!(engine.playhead(ws.cue_lists[1].id), Some(cue_id(10)));
        let index = CueIndex::build(&ws);
        let got = prime_set(&ws, &engine, &index, 3);
        let ids = |ks: &[u128]| ks.iter().map(|k| cue_id(*k)).collect::<Vec<_>>();
        assert_eq!(got, ids(&[3, 11, 12, 13, 6]));
        assert_eq!(got, prime_set_walk(&ws, &engine, 3));
    }

    /// A pass reads the cues it names, not the length of the list: the index lookups it makes
    /// are the same in a show of 100 cues and one of 2,000, playhead near the far end.
    #[test]
    fn prime_set_lookups_do_not_grow_with_the_show() {
        let (base, template) = example();
        let steps = |cues: u128| {
            let mut ws = base.clone();
            ws.cue_lists[0].cues = (1..=cues).map(|k| audio(&template, k)).collect();
            let mut engine = CueEngine::new(EngineConfig::new(1000, 5, 1).unwrap());
            let _ = engine.handle(&ws, Input::Tick, 0);
            let list = ws.cue_lists[0].id;
            let cue = Some(cue_id(cues - 10));
            let _ = engine.handle(&ws, Input::SetPlayhead { list, cue }, 0);
            let index = CueIndex::build(&ws);
            let got = prime_set(&ws, &engine, &index, 8);
            assert_eq!(got.len(), 8);
            index.lookup_steps()
        };
        let small = steps(100);
        assert!(small > 0, "the pass looks cues up through the index");
        assert_eq!(steps(2000), small);
    }
}
