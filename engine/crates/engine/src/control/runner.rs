// SPDX-License-Identifier: GPL-3.0-or-later

//! The control thread's loop against a running device (ARCHITECTURE §3.2): wake on an inbox
//! message, when the next deadline enters the lookahead window, or after [`MAX_WAIT`]
//! (5 ms), whichever comes first. Wake jitter never shifts audio: every transition is computed
//! from sample time, and the lookahead commits each start on its exact sample.
//!
//! [`MAX_WAIT`]: super::MAX_WAIT

use crossbeam_channel::{Receiver, RecvTimeoutError};

use super::{Control, Flow, Inbox};

/// Runs the loop until a shutdown message (or every inbox sender is gone), then stops the show.
pub fn run(mut control: Control, inbox: &Receiver<Inbox>) {
    loop {
        match inbox.recv_timeout(control.wait_hint()) {
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
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        control.tick();
    }
    control.shutdown();
}
