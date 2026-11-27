// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::atomic::AtomicU64;

use openlab_audio::WaveformPartial;

use super::*;

/// A read that hangs until it is cancelled (a disk that never answers).
fn hangs(
    _: &Path,
    _: Media,
    _: &MediaInfo,
    cancel: &AtomicBool,
    _: PartialSink<'_>,
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

/// The next message from the reader's thread, yielding until it comes (no clock: the thread's
/// messages are all this waits on).
fn next(rx: &crossbeam_channel::Receiver<Inbox>) -> Result<Inbox, ()> {
    for _ in 0..50_000_000 {
        if let Ok(m) = rx.try_recv() {
            return Ok(m);
        }
        std::thread::yield_now();
    }
    Err(())
}

/// A read that gives an overview, waits for its cancel and gives another.
fn partial_then_cancelled(
    _: &Path,
    _: Media,
    _: &MediaInfo,
    cancel: &AtomicBool,
    sink: PartialSink<'_>,
) -> Result<WaveformResult, WaveformError> {
    let partial = WaveformPartial {
        wave: openlab_audio::Waveform {
            sample_rate: 48_000,
            frames: 256,
            channels: 1,
            levels: Vec::new(),
        },
        total_frames: 1_000_000,
    };
    sink(partial.clone());
    while !cancel.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(1));
    }
    sink(partial);
    Err(WaveformError::Cancelled)
}

/// A read's overviews reach the control loop while it runs; none do once it is cancelled.
#[test]
fn a_cancelled_read_sends_no_more_partials() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut r = WaveformReader::new(
        tx,
        partial_then_cancelled,
        Duration::from_secs(60),
        ProbeClock::system(),
    );
    let doc = DocId(0);
    let Ok((ticket, _)) = r.submit(doc, "/a".into(), media(), info()) else {
        panic!("refused");
    };
    let first = next(&rx);
    assert!(
        matches!(first, Ok(Inbox::WaveformPartial { ticket: t, .. }) if t == ticket),
        "the first overview comes while the read runs"
    );
    r.forget(doc);
    // The read's end follows at once: the second overview it tried was dropped.
    let after = next(&rx);
    assert!(
        matches!(after, Ok(Inbox::Waveform { .. })),
        "the read's end, no overview, comes next"
    );
    assert!(rx.try_recv().is_err());
}
