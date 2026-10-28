// SPDX-License-Identifier: GPL-3.0-or-later

//! The control thread's loop against a running device (ARCHITECTURE §3.2): wake on an inbox
//! message, when the next deadline enters the lookahead window, or after [`MAX_WAIT`]
//! (5 ms), whichever comes first. Wake jitter never shifts audio: every transition is computed
//! from sample time, and the lookahead commits each start on its exact sample. Each wake ticks
//! every open doc ([`Control::tick`]) and the next wait is the shortest any doc asks for
//! ([`Control::wait_hint`]), so a doc with a deadline near is never held up by an idle one.
//!
//! [`MAX_WAIT`]: super::MAX_WAIT

use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError};

use super::{Control, Flow, Inbox};

/// One wake of the loop, as [`run_observed`] reports it.
#[derive(Clone, Copy, Debug)]
pub struct Wake {
    /// How long the loop asked to sleep ([`Control::wait_hint`]: the minimum over the docs).
    pub wanted: Duration,
    /// Time from asking to wake; meaningful only when `timed_out` (after a message it includes
    /// handling the whole burst).
    pub waited: Duration,
    /// The wait ended by timing out rather than by a message: `waited - wanted` is then the
    /// timer's overshoot, the wake jitter.
    pub timed_out: bool,
}

/// Runs the loop until a shutdown message (or every inbox sender is gone), then stops the show.
pub fn run(control: Control, inbox: &Receiver<Inbox>) {
    run_observed(control, inbox, |_, _| {});
}

/// As [`run`], calling `observe` after every tick with the loop's state and how it woke. For
/// the real-time soak test (wake jitter, counters sampled on the control thread); it runs on the
/// control thread, so it must be quick.
pub fn run_observed(
    mut control: Control,
    inbox: &Receiver<Inbox>,
    mut observe: impl FnMut(&mut Control, Wake),
) {
    loop {
        let wanted = control.wait_hint();
        let began = Instant::now();
        let timed_out = match inbox.recv_timeout(wanted) {
            Ok(msg) => {
                if control.handle(msg) == Flow::Shutdown {
                    break;
                }
                // Take what else has arrived before ticking, so a burst costs one tick.
                let mut stop = false;
                while let Ok(msg) = inbox.try_recv() {
                    if control.handle(msg) == Flow::Shutdown {
                        stop = true;
                        break;
                    }
                }
                if stop {
                    break;
                }
                false
            }
            Err(RecvTimeoutError::Timeout) => true,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let wake = Wake {
            wanted,
            waited: began.elapsed(),
            timed_out,
        };
        control.tick();
        observe(&mut control, wake);
    }
    control.shutdown();
}
