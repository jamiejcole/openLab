// SPDX-License-Identifier: GPL-3.0-or-later

//! How long the control loop may wait before it must tick again (ARCHITECTURE §3.2).

use std::time::Duration;

use openlab_core::SampleTime;

use super::{Control, Doc, Engine, MAX_WAIT, MIN_STALL_WAIT};

impl Doc {
    /// How long this doc lets the loop wait: until its next deadline enters the lookahead
    /// window or a deferred report of it becomes due, at most [`MAX_WAIT`].
    fn wait(&self, eng: &Engine) -> Duration {
        let frames = wake_frames(
            self.engine.next_deadline(),
            self.deferred.iter().map(|(at, _)| *at),
            eng.mixer.samples_rendered(),
            eng.block,
        );
        let secs = frames as f64 / f64::from(eng.rate.get());
        Duration::from_secs_f64(secs.min(MAX_WAIT.as_secs_f64()))
    }
}

impl Control {
    /// How long the loop may wait for the inbox before it must tick again: the shortest wait
    /// any doc asks for ([`Doc::wait`]), at most [`MAX_WAIT`].
    pub fn wait_hint(&self) -> Duration {
        // Nothing becomes due by waiting while sample time stands still.
        if self.eng.backend.is_down() {
            return MAX_WAIT;
        }
        let wait = self
            .open_docs()
            .map(|doc| doc.wait(&self.eng))
            .min()
            .unwrap_or(MAX_WAIT);
        // A deadline enters the window only when sample time moves, and it moves in whole
        // blocks. Whenever the last tick saw it standing still (a stalled device, up to the
        // watchdog limit, but also the wake between two ordinary callbacks), waking sooner
        // would spin. The floor is a quarter block at most, so a small device block cannot
        // slip past the window. The first wake after a tick that saw progress is not floored,
        // and a message ends any wait at once.
        if self.eng.last_tick_stalled {
            wait.max(stall_floor(self.eng.block, self.eng.rate.get()))
        } else {
            wait
        }
    }
}

/// The shortest wait after a tick that saw no progress: [`MIN_STALL_WAIT`], or a quarter of a
/// device block when that is shorter.
pub fn stall_floor(block: u64, rate: u32) -> Duration {
    let quarter = Duration::from_secs_f64(block as f64 / f64::from(rate.max(1)) / 4.0);
    MIN_STALL_WAIT.min(quarter)
}

/// Frames from `rendered` until the loop must tick: when the engine's next deadline enters the
/// lookahead window (`is_due(rendered + block)`: `deadline < rendered + 2 * block`), or a
/// deferred report becomes deliverable (`at < rendered + block`). 0 = now.
pub fn wake_frames(
    deadline: Option<SampleTime>,
    deferred: impl Iterator<Item = SampleTime>,
    rendered: SampleTime,
    block: SampleTime,
) -> u64 {
    let mut frames = u64::MAX;
    if let Some(d) = deadline {
        frames = frames.min(
            d.saturating_add(1)
                .saturating_sub(block.saturating_mul(2))
                .saturating_sub(rendered),
        );
    }
    for at in deferred {
        frames = frames.min(
            at.saturating_add(1)
                .saturating_sub(block)
                .saturating_sub(rendered),
        );
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::tests::{memo_workspace, uuid_zero};
    use openlab_core::{CueEngine, CueId, EngineConfig, Input};

    /// `wake_frames` agrees with the engine's own wake rule (`CueEngine::is_due`, lookahead =
    /// block, `now = rendered + block`) for every rendered position around a deadline: 0
    /// exactly when a tick there is due, otherwise the distance to the first position where it is.
    #[test]
    fn wake_frames_agrees_with_is_due() {
        for block in [1u64, 3, 64] {
            for d in (block..300).step_by(7) {
                let engine = engine_with_deadline(block, d);
                let due = |rendered: u64| engine.is_due(rendered + block);
                for rendered in 0..300u64 {
                    let w =
                        wake_frames(engine.next_deadline(), std::iter::empty(), rendered, block);
                    assert_eq!(w == 0, due(rendered), "d {d} r {rendered} b {block}");
                    if w > 0 {
                        assert!(due(rendered + w));
                        assert!(!due(rendered + w - 1));
                    }
                }
            }
        }
    }

    /// An engine (1 kHz, lookahead `block`) whose only deadline is a Memo's pre-wait ending on
    /// sample `d`.
    fn engine_with_deadline(block: u64, d: u64) -> CueEngine {
        let ws = memo_workspace(d as f64 / 1000.0);
        let mut engine = CueEngine::new(EngineConfig::new(1000, block, 1).unwrap());
        engine.handle(
            &ws,
            Input::Start {
                cue: CueId(uuid_zero()),
            },
            0,
        );
        assert_eq!(engine.next_deadline(), Some(d));
        engine
    }

    #[test]
    fn wake_frames_for_deferred_reports() {
        // Deliverable once `at < now`, now = rendered + block.
        let block = 10;
        for at in 0..100u64 {
            for rendered in 0..100u64 {
                let w = wake_frames(None, std::iter::once(at), rendered, block);
                assert_eq!(w == 0, at < rendered + block, "at {at} r {rendered}");
                if w > 0 {
                    assert!(at < rendered + w + block);
                }
            }
        }
        assert_eq!(wake_frames(None, std::iter::empty(), 5, 10), u64::MAX);
        assert_eq!(
            wake_frames(Some(u64::MAX), std::iter::empty(), 0, 10),
            u64::MAX - 20
        );
    }

    /// The engine's own `is_due` with lookahead = block matches the rule `wake_frames` assumes.
    #[test]
    fn the_wake_rule_matches_the_engine() {
        let block = 64;
        let ws = memo_workspace(0.5);
        let mut engine = CueEngine::new(EngineConfig::new(1000, block, 1).unwrap());
        engine.handle(
            &ws,
            Input::Start {
                cue: CueId(uuid_zero()),
            },
            block,
        );
        let d = engine.next_deadline().unwrap();
        for rendered in 0..d + 10 {
            let w = wake_frames(Some(d), std::iter::empty(), rendered, block);
            assert_eq!(
                w == 0,
                engine.is_due(rendered + block),
                "rendered {rendered}"
            );
        }
    }
}
