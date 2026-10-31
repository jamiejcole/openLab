// SPDX-License-Identifier: GPL-3.0-or-later

//! Priming: which cues each doc keeps ready to play, and how the engine's prime pool is shared
//! between the open docs.

use openlab_core::{CueEngine, CueId, CueKind, SampleTime, Workspace, descendants};

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
    /// Primed streams the whole engine may hold (ARCHITECTURE §3.5): `max_primes`, and never
    /// more than half the disk streamer's open-stream limit, which is the engine's. A start
    /// that finds no permit fails (`stream_failed`), and primes are only a head start, so the
    /// other half is always left for starts (M4.15e review M3). 64 with the defaults.
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
    /// Keeps the cues near each playhead primed (ARCHITECTURE §3.5).
    pub(super) fn prime(&mut self, now: SampleTime) {
        let wanted = prime_set(
            self.doc.model.ws(),
            self.doc.model.engine(),
            self.eng.config.prime_cues,
        );
        // Their files are probed first (H2: a restart mid-show must not leave the cue at the
        // playhead behind a hundred others).
        let keys: Vec<String> = wanted
            .iter()
            .filter_map(|c| match &self.doc.model.ws().find_cue(*c)?.kind {
                CueKind::Audio { media: Some(m), .. } => Some(m.as_str().to_owned()),
                _ => None,
            })
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

/// The cues to keep primed: from each list's playhead the next `per_list` audio cues, the
/// targets of those cues, then every cue with a hotkey or OSC trigger (ARCHITECTURE §3.5).
/// Disarmed and running cues are left out (a start of a running cue makes a new voice anyway).
fn prime_set(ws: &Workspace, engine: &CueEngine, per_list: usize) -> Vec<CueId> {
    let mut wanted: Vec<CueId> = Vec::new();
    let add = |id: CueId, wanted: &mut Vec<CueId>| {
        let ok = ws
            .find_cue(id)
            .is_some_and(|c| c.armed && matches!(c.kind, CueKind::Audio { media: Some(_), .. }))
            && engine.run_of(id).is_none();
        if ok && !wanted.contains(&id) {
            wanted.push(id);
        }
    };
    let mut targets = Vec::new();
    for list in &ws.cue_lists {
        let Some(head) = engine.playhead(list.id) else {
            continue;
        };
        let positions = CueEngine::playhead_positions(ws, list.id);
        let from = positions.iter().position(|&c| c == head).unwrap_or(0);
        for &id in positions.iter().skip(from).take(per_list) {
            add(id, &mut wanted);
            if let Some(t) = ws.find_cue(id).and_then(|c| c.kind.target()) {
                targets.push(t);
            }
        }
    }
    for t in targets {
        if let Some(c) = ws.find_cue(t) {
            // A group target primes its audio children.
            for c in descendants(std::slice::from_ref(c)) {
                add(c.id, &mut wanted);
            }
        }
    }
    for c in ws.cues() {
        if c.triggers.hotkey.is_some() || c.triggers.osc.is_some() {
            add(c.id, &mut wanted);
        }
    }
    wanted
}
