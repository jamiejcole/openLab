// SPDX-License-Identifier: GPL-3.0-or-later

//! What the control thread knows about each media file the workspace uses, and the probe workers
//! that find it out. Probing decodes a whole compressed file (1–2 s for 30 minutes, docs/audio.md),
//! and preloading decodes it into memory, so both run on these workers and never on the control
//! thread (ARCHITECTURE §3.1): the control thread sends a job and gets the answer back through
//! its inbox.
//!
//! Probes of files cues near a playhead need come first (a high-priority queue the workers
//! always empty before the other one). A worker stuck on one file (a stalled network volume)
//! longer than `probe_timeout` is abandoned and replaced, so later probes are not queued behind
//! it for ever; its file is reported unplayable meanwhile (a late answer still counts). The
//! abandoned thread is leaked: it ends when its read returns, if it ever does.
//!
//! Probe scheduling per volume (M2.25b). A volume is the mount a path is on (`bucket_of`, as the
//! media watch names them: `/Volumes/<name>`, `/mnt/<name>`, a drive or share; everything else,
//! the system disk included, is `/`, which is never gated: it mixes the local disk with
//! automounts and cloud folders, so probes there run as ever, a stuck worker simply replaced).
//! One slow probe is not a verdict on a mount, so a volume
//! counts as stalled only when the media watch reports it stalled, or when two different files'
//! probes are stuck on it (preloads never count). A stalled volume gets one trial job at a time
//! (the rest wait, still pending, no worker taken; files keep any last answer) and the next
//! trial waits out a backoff after one gets stuck or fails. Any answer from the volume ends the
//! probe-side stall. Jobs on other volumes always run. See docs/control.md.
//!
//! The workers, their watchdog and the volume state are the engine's ([`ProbePool`]: one pool of
//! [`PROBE_WORKERS`] for every open doc, ARCHITECTURE §3.1). Each doc's [`MediaCatalog`] sends
//! its jobs through a [`PoolHandle`] that tags them with the doc, and the answers come back
//! tagged the same way, so the control loop hands each to its doc (an answer for a closed doc
//! is dropped).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use openlab_audio::{MediaError, Probed};

use super::DocId;

mod catalog;
mod gate;
mod pool;

pub use catalog::*;
pub use pool::*;

/// Probe workers started per engine (shared by every doc). Probes of different files run side by side so one
/// long file does not hold up the rest of a workspace.
pub const PROBE_WORKERS: usize = 2;
/// Most workers abandoned (stuck) and replaced over a catalog's life: past it, stuck workers
/// are not replaced any more, so a dead volume cannot make the engine start threads without end.
pub const MAX_ABANDONED_WORKERS: usize = 16;

/// The monotonic clock the probe watchdog reads. Tests inject one that moves only when told to,
/// so whether a worker counts as stuck never depends on how fast the machine runs.
#[derive(Clone)]
pub struct ProbeClock(Arc<dyn Fn() -> Instant + Send + Sync>);

impl ProbeClock {
    pub fn system() -> Self {
        Self(Arc::new(Instant::now))
    }

    pub fn new(now: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        Self(Arc::new(now))
    }

    pub(super) fn now(&self) -> Instant {
        (self.0)()
    }
}

impl std::fmt::Debug for ProbeClock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProbeClock")
    }
}

/// How a file is probed: [`openlab_audio::probe`] in the engine; tests inject a stand-in.
pub type ProbeFn = fn(&Path) -> Result<Probed, MediaError>;

/// A job for a probe worker.
#[derive(Debug)]
enum Job {
    Probe {
        doc: DocId,
        key: String,
        ticket: u64,
        path: PathBuf,
    },
    Preload {
        doc: DocId,
        key: String,
        ticket: u64,
        path: PathBuf,
        max_bytes: u64,
    },
}

impl Job {
    fn key(&self) -> (&str, u64) {
        match self {
            Self::Probe { key, ticket, .. } | Self::Preload { key, ticket, .. } => (key, *ticket),
        }
    }

    fn doc(&self) -> DocId {
        match self {
            Self::Probe { doc, .. } | Self::Preload { doc, .. } => *doc,
        }
    }

    fn path(&self) -> &Path {
        match self {
            Self::Probe { path, .. } | Self::Preload { path, .. } => path,
        }
    }

    fn is_probe(&self) -> bool {
        matches!(self, Self::Probe { .. })
    }
}
