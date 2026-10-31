// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::atomic::AtomicU64;

use super::*;

/// A read that hangs until it is cancelled (a disk that never answers).
fn hangs(
    _: &Path,
    _: Media,
    _: &MediaInfo,
    cancel: &AtomicBool,
) -> Result<WaveformResult, WaveformError> {
    while !cancel.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(1));
    }
    Err(WaveformError::Cancelled)
}

fn media() -> Media {
    Media::Reader(Box::new(std::io::Cursor::new(Vec::new())), None)
}

fn info() -> MediaInfo {
    MediaInfo {
        channels: 1,
        sample_rate: 48_000,
        frames: 1,
        duration: Duration::ZERO,
    }
}

/// One read runs at a time, a bounded few wait; a read past its timeout is given up on and the
/// next starts; after [`MAX_ABANDONED_READS`] no more are taken and the waiting ones are failed.
#[test]
fn one_read_at_a_time_bounded_and_given_up_on() {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let base = Instant::now();
    let ms = Arc::new(AtomicU64::new(0));
    let clock_ms = Arc::clone(&ms);
    let clock =
        ProbeClock::new(move || base + Duration::from_millis(clock_ms.load(Ordering::Relaxed)));
    let timeout = Duration::from_secs(1);
    let mut r = WaveformReader::new(tx, hangs, timeout, clock);
    let doc = DocId(0);
    for _ in 0..=MAX_QUEUED {
        assert!(r.submit(doc, "/a".into(), media(), info()).is_ok());
    }
    assert_eq!(r.pending(), MAX_QUEUED + 1, "one running, the rest waiting");
    assert_eq!(
        r.submit(doc, "/a".into(), media(), info()).err(),
        Some(Refused::Busy)
    );
    assert!(r.watchdog().is_empty(), "not timed out yet");
    for n in 1..MAX_ABANDONED_READS {
        ms.fetch_add(1001, Ordering::Relaxed);
        assert_eq!(r.watchdog(), vec![(doc, n as u64)]);
        assert_eq!(r.abandoned(), n);
    }
    ms.fetch_add(1001, Ordering::Relaxed);
    let last = r.watchdog();
    assert_eq!(
        last.len(),
        1 + MAX_QUEUED + 1 - MAX_ABANDONED_READS,
        "the waiting reads fail with the last"
    );
    assert_eq!(r.pending(), 0);
    assert_eq!(
        r.submit(doc, "/a".into(), media(), info()).err(),
        Some(Refused::Stuck)
    );
}

/// A closed doc's waiting reads go; another doc's stay.
#[test]
fn forget_drops_a_docs_waiting_reads() {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut r = WaveformReader::new(tx, hangs, Duration::from_secs(1), ProbeClock::system());
    let (a, b) = (DocId(0), DocId(1));
    let _ = r.submit(a, "/a".into(), media(), info());
    let _ = r.submit(a, "/a".into(), media(), info());
    let _ = r.submit(b, "/b".into(), media(), info());
    r.forget(a);
    assert_eq!(
        r.pending(),
        2,
        "a's running read stays until it ends, b's waits"
    );
    r.forget(b);
}
