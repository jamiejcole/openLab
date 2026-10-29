// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

fn watch(core: &mut WatchCore, key: &str, id: u64) {
    core.apply(WatchJob::Watch {
        key: key.to_owned(),
        path: PathBuf::from(key),
        id,
    });
}

/// A core watching `keys` (ids 1..), with every file present and the first pass made.
fn setup(keys: &[&str]) -> (WatchCore, FakeFs) {
    let fs = FakeFs::new();
    let mut core = WatchCore::new();
    for (i, k) in keys.iter().enumerate() {
        fs.put(k, 1);
        watch(&mut core, k, i as u64 + 1);
    }
    assert!(core.pass(&fs).is_empty(), "the first pass is silent");
    (core, fs)
}

const A: &str = "/show/a.wav";
const B: &str = "/show/b.wav";

#[test]
fn an_unchanged_pass_finds_nothing() {
    let (mut core, fs) = setup(&[A, B]);
    for _ in 0..3 {
        assert!(core.pass(&fs).is_empty());
    }
    assert_eq!(core.finish(), None);
}

#[test]
fn a_file_gone_is_reported_on_the_first_pass_once() {
    let (mut core, fs) = setup(&[A, B]);
    fs.trash(A);
    assert_eq!(
        core.pass(&fs),
        vec![WatchEvent::Gone {
            key: A.into(),
            id: 1,
            trashed: false
        }]
    );
    assert!(core.pass(&fs).is_empty());
}

#[test]
fn a_file_missing_from_the_start_is_gone_on_the_first_pass() {
    let fs = FakeFs::new();
    let mut core = WatchCore::new();
    watch(&mut core, A, 7);
    assert_eq!(
        core.pass(&fs),
        vec![WatchEvent::Gone {
            key: A.into(),
            id: 7,
            trashed: false
        }]
    );
}

#[test]
fn a_file_back_is_reported_after_two_identical_passes() {
    let (mut core, fs) = setup(&[A]);
    fs.trash(A);
    assert_eq!(core.pass(&fs).len(), 1);
    fs.put_back(A);
    assert!(core.pass(&fs).is_empty(), "once is not enough");
    let back = core.pass(&fs);
    assert!(
        matches!(&back[..], [WatchEvent::Back { key, id: 1, .. }] if key == A),
        "{back:?}"
    );
    assert!(core.pass(&fs).is_empty());
}

#[test]
fn a_file_still_being_copied_is_not_back() {
    let (mut core, fs) = setup(&[A]);
    fs.remove(A);
    assert_eq!(core.pass(&fs).len(), 1);
    fs.put(A, 1);
    for size in [10, 20, 30, 40] {
        fs.set_size(A, size);
        assert!(core.pass(&fs).is_empty(), "size {size} still growing");
    }
    assert!(matches!(&core.pass(&fs)[..], [WatchEvent::Back { .. }]));
}

#[test]
fn a_changed_file_is_reported_after_two_identical_passes() {
    let (mut core, fs) = setup(&[A]);
    fs.replace(A);
    assert!(core.pass(&fs).is_empty());
    let changed = core.pass(&fs);
    assert!(
        matches!(&changed[..], [WatchEvent::Changed { key, .. }] if key == A),
        "{changed:?}"
    );
    assert!(core.pass(&fs).is_empty());
    fs.touch(A);
    assert!(core.pass(&fs).is_empty());
    assert!(matches!(&core.pass(&fs)[..], [WatchEvent::Changed { .. }]));
}

#[test]
fn a_change_undone_before_the_second_pass_is_not_reported() {
    let (mut core, fs) = setup(&[A]);
    fs.set_size(A, 5);
    assert!(core.pass(&fs).is_empty());
    fs.set_size(A, 1000);
    assert!(core.pass(&fs).is_empty());
    assert!(core.pass(&fs).is_empty());
}

#[test]
fn unreadable_is_reported_once_per_change() {
    let (mut core, fs) = setup(&[A]);
    fs.deny(A);
    let ev = core.pass(&fs);
    assert!(
        matches!(&ev[..], [WatchEvent::Unreadable { key, id: 1, .. }] if key == A),
        "{ev:?}"
    );
    assert!(core.pass(&fs).is_empty());
    assert!(core.pass(&fs).is_empty());
    // Readable again, unchanged: the re-probe on Unreadable already read it (one blip,
    // one re-probe).
    fs.allow(A);
    assert!(core.pass(&fs).is_empty());
    assert!(core.pass(&fs).is_empty());
    // A different file when it comes back: Back.
    fs.deny(A);
    assert!(matches!(
        &core.pass(&fs)[..],
        [WatchEvent::Unreadable { .. }]
    ));
    fs.replace(A);
    fs.allow(A);
    assert!(core.pass(&fs).is_empty());
    assert!(matches!(&core.pass(&fs)[..], [WatchEvent::Back { .. }]));
    fs.deny(A);
    assert!(matches!(
        &core.pass(&fs)[..],
        [WatchEvent::Unreadable { .. }]
    ));
    fs.allow(A);
    fs.remove(A);
    assert!(matches!(&core.pass(&fs)[..], [WatchEvent::Gone { .. }]));
}

#[test]
fn a_return_after_a_failed_reprobe_is_reported_even_unchanged() {
    let (mut core, fs) = setup(&[A]);
    fs.deny(A);
    assert_eq!(core.pass(&fs).len(), 1);
    // Its re-probe could not read it either.
    core.apply(WatchJob::ProbeFailed {
        key: A.into(),
        id: 1,
        missing: false,
    });
    fs.allow(A);
    assert!(core.pass(&fs).is_empty());
    assert!(matches!(&core.pass(&fs)[..], [WatchEvent::Back { .. }]));
    // A failed probe of a readable file changes nothing (no re-probe loop on a file that
    // stats but cannot be decoded).
    core.apply(WatchJob::ProbeFailed {
        key: A.into(),
        id: 1,
        missing: false,
    });
    for _ in 0..3 {
        assert!(core.pass(&fs).is_empty());
    }
}

#[test]
fn a_file_whose_probe_found_nothing_is_reported_back_however_soon_it_appears() {
    // Its first stat after the probe failed: still reported (a late-mounting drive).
    let fs = FakeFs::new();
    let mut core = WatchCore::new();
    watch(&mut core, A, 1);
    core.apply(WatchJob::ProbeFailed {
        key: A.into(),
        id: 1,
        missing: true,
    });
    fs.put(A, 1);
    assert!(core.pass(&fs).is_empty());
    assert!(matches!(
        &core.pass(&fs)[..],
        [WatchEvent::Back { id: 1, .. }]
    ));
    // Seen silently on the first pass, then the probe said missing: the same.
    let (mut core, fs) = setup(&[B]);
    core.apply(WatchJob::ProbeFailed {
        key: B.into(),
        id: 1,
        missing: true,
    });
    assert!(core.pass(&fs).is_empty());
    assert!(matches!(&core.pass(&fs)[..], [WatchEvent::Back { .. }]));
    // A failure of an earlier watch of the key changes nothing; still gone: no event.
    core.apply(WatchJob::ProbeFailed {
        key: B.into(),
        id: 99,
        missing: true,
    });
    assert!(core.pass(&fs).is_empty());
    let fs2 = FakeFs::new();
    let mut core = WatchCore::new();
    watch(&mut core, A, 1);
    core.apply(WatchJob::ProbeFailed {
        key: A.into(),
        id: 1,
        missing: true,
    });
    assert!(core.pass(&fs2).is_empty(), "control knows it is missing");
}

#[test]
fn a_volume_on_trial_reports_its_first_stat() {
    let a = "/Volumes/Show/a.wav";
    let b = "/Volumes/Show/b.wav";
    let (mut core, fs) = setup(&[a, b]);
    core.apply(WatchJob::Skip {
        bucket: "/Volumes/Show".into(),
    });
    assert_eq!(core.finish(), None);
    core.apply(WatchJob::Unskip {
        bucket: "/Volumes/Show".into(),
        trial: true,
    });
    fs.deny(a);
    while let Some(chunk) = core.next_chunk(CHUNK) {
        for t in chunk {
            let stat = fs.stat(&t.path);
            core.record(&t, stat);
        }
    }
    let batch = core.finish().unwrap();
    assert_eq!(batch.volumes, vec![("/Volumes/Show".to_owned(), false)]);
    assert!(core.pass(&fs).is_empty());
}

#[test]
fn abandonment_and_a_returning_stat_never_both_miss() {
    // The stat returns before the watchdog acts on it: not abandoned.
    let w = WatcherState::default();
    assert!(w.begin("/Volumes/A"));
    let seen = w.current().unwrap();
    assert!(!w.end());
    assert!(!w.try_abandon(&seen));
    // Another stat of the same volume since: not the one judged stalled either.
    assert!(w.begin("/Volumes/A"));
    assert!(!w.try_abandon(&seen));
    // The watchdog first: the returning stat sees it and reports its own volume.
    let seen = w.current().unwrap();
    assert!(w.try_abandon(&seen));
    assert!(w.end());
    assert!(!w.begin("/Volumes/B"), "an abandoned watcher stats nothing");
    assert!(w.abandoned());
}

#[test]
fn a_symlink_is_recorded_and_watched_through() {
    let (mut core, fs) = setup(&[A]);
    fs.set_symlink(A, true);
    assert!(core.pass(&fs).is_empty());
    assert!(core.items.get(A).is_some_and(|i| i.symlink));
}

#[test]
fn locate_is_not_used_yet() {
    assert_eq!(
        RealFs.locate(&Fingerprint {
            dev: 1,
            ino: 2,
            size: 3,
            modified_ms: 4
        }),
        None
    );
}

#[test]
fn an_unwatch_mid_pass_drops_its_event() {
    let (mut core, fs) = setup(&[A, B]);
    fs.remove(A);
    fs.remove(B);
    // The first chunk holds both files: record A, then A is unwatched before the pass ends.
    let chunk = core.next_chunk(1).unwrap();
    for t in &chunk {
        core.record(t, fs.stat(&t.path));
    }
    core.apply(WatchJob::Unwatch { key: A.into() });
    let rest = core.next_chunk(CHUNK).unwrap();
    for t in &rest {
        core.record(t, fs.stat(&t.path));
    }
    assert!(core.next_chunk(CHUNK).is_none());
    let batch = core.finish().unwrap();
    assert_eq!(batch.events.len(), 1);
    assert_eq!(batch.events[0].key(), B);
}

#[test]
fn a_stat_of_an_earlier_watch_of_the_key_is_ignored() {
    let (mut core, fs) = setup(&[A]);
    let chunk = core.next_chunk(CHUNK).unwrap();
    fs.remove(A);
    // Unwatched and watched again (a new id) while the stat was under way.
    core.apply(WatchJob::Unwatch { key: A.into() });
    watch(&mut core, A, 9);
    for t in &chunk {
        core.record(t, fs.stat(&t.path));
    }
    assert!(core.next_chunk(CHUNK).is_none());
    assert_eq!(core.finish(), None);
    assert_eq!(
        core.pass(&fs),
        vec![WatchEvent::Gone {
            key: A.into(),
            id: 9,
            trashed: false
        }]
    );
}

#[test]
fn a_reset_forgets_everything_and_carries_the_generation() {
    let (mut core, fs) = setup(&[A]);
    fs.remove(A);
    core.apply(WatchJob::Reset { generation: 3 });
    assert!(core.is_empty());
    watch(&mut core, A, 2);
    let chunk = core.next_chunk(CHUNK).unwrap();
    for t in &chunk {
        core.record(t, fs.stat(&t.path));
    }
    assert!(core.next_chunk(CHUNK).is_none());
    assert_eq!(core.finish().map(|b| b.generation), Some(3));
}

#[test]
fn a_skipped_volume_keeps_its_state() {
    let a = "/Volumes/Show/a.wav";
    let (mut core, fs) = setup(&[a, B]);
    core.apply(WatchJob::Skip {
        bucket: "/Volumes/Show".into(),
    });
    fs.remove(a);
    fs.remove(B);
    let ev = core.pass(&fs);
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].key(), B);
    core.apply(WatchJob::Unskip {
        bucket: "/Volumes/Show".into(),
        trial: false,
    });
    let ev = core.pass(&fs);
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].key(), a);
}

#[test]
fn chunks_cover_every_file_once_per_pass() {
    let fs = FakeFs::new();
    let mut core = WatchCore::new();
    for i in 0..200 {
        let k = format!("/show/{i:03}.wav");
        fs.put(&k, 1);
        watch(&mut core, &k, i + 1);
    }
    let mut seen = 0;
    let mut chunks = 0;
    while let Some(c) = core.next_chunk(CHUNK) {
        assert!(c.len() <= CHUNK);
        seen += c.len();
        chunks += 1;
    }
    assert_eq!((seen, chunks), (200, 4));
    // The next call starts the next pass.
    assert_eq!(core.next_chunk(CHUNK).map(|c| c.len()), Some(CHUNK));
}

#[test]
fn pacing_spreads_a_pass_over_its_period() {
    let min = MIN_PERIOD;
    assert_eq!(pass_period(10, min), min);
    assert_eq!(pass_period(1_500, min), Duration::from_secs(3));
    assert_eq!(pass_period(3_000, min), Duration::from_secs(6));
    assert_eq!(pass_period(10_000, min), Duration::from_secs(20));
    for n in [0usize, 1, 64, 65, 2_000, 3_000, 10_000] {
        let chunks = n.div_ceil(CHUNK).max(1) as u32;
        let total = chunk_gap(n, min) * chunks;
        // Integer division may round the gap down by under a nanosecond per chunk.
        assert!(
            total + Duration::from_nanos(u64::from(chunks)) >= pass_period(n, min),
            "{n}: {total:?}"
        );
        assert!(total <= pass_period(n, min));
    }
    assert!(chunk_gap(3_000, min) * 47 >= Duration::from_millis(5_999));
}

#[test]
fn buckets_name_the_volume() {
    for (path, bucket) in [
        ("/Volumes/Show Drive/a/b.wav", "/Volumes/Show Drive"),
        ("/Volumes/x.wav", "/"),
        ("/Users/me/a.wav", "/"),
        ("/media/me/USB/a.wav", "/media/me/USB"),
        ("/run/media/me/USB/a.wav", "/run/media/me/USB"),
        ("/mnt/nas/a.wav", "/mnt/nas"),
        ("relative.wav", "/"),
    ] {
        assert_eq!(bucket_of(Path::new(path)), bucket, "{path}");
    }
    #[cfg(windows)]
    {
        assert_eq!(bucket_of(Path::new(r"D:\show\a.wav")), "D:");
        assert_eq!(bucket_of(Path::new(r"\\nas\share\a.wav")), r"\\nas\share");
    }
}

#[test]
fn real_fs_stats_a_file_and_reports_missing_and_folders() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("a.wav");
    std::fs::write(&f, b"1234").unwrap();
    let st = RealFs.stat(&f).unwrap();
    assert!(st.regular && !st.symlink);
    assert_eq!(st.fp.size, 4);
    assert!(!RealFs.stat(dir.path()).unwrap().regular);
    let e = RealFs.stat(&dir.path().join("gone.wav")).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::NotFound);
    #[cfg(unix)]
    {
        let link = dir.path().join("link.wav");
        std::os::unix::fs::symlink(&f, &link).unwrap();
        let st = RealFs.stat(&link).unwrap();
        assert!(st.regular && st.symlink);
        // A path through a file is not a folder: the file is gone, not unreadable.
        let e = RealFs.stat(&f.join("x.wav")).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotADirectory);
    }
}

// ----- the control side: generations, the watchdog -----

fn manual() -> (MediaWatch, Receiver<Inbox>, FakeFs) {
    let fs = FakeFs::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    let config = WatchConfig {
        fs: Arc::new(fs.clone()),
        mode: WatchMode::Manual,
        clock: EpochClock::new(|| 0),
        ..WatchConfig::default()
    };
    let w = MediaWatch::new(&config, tx).unwrap();
    (w, rx, fs)
}

#[test]
fn a_failing_volume_waits_30_s_doubling_to_5_min_and_its_notice_stays() {
    let (mut w, _rx, _fs) = manual();
    w.skip_stalled("/Volumes/NAS");
    assert!(w.take_notices_changed());
    let mut waits = Vec::new();
    for _ in 0..6 {
        w.volume_answered("/Volumes/NAS", false);
        waits.push(w.volumes["/Volumes/NAS"].retry_at.unwrap());
        // Its retry was sent and failed again.
        w.volumes.get_mut("/Volumes/NAS").unwrap().retry_at = None;
    }
    assert_eq!(
        waits,
        vec![30_000, 60_000, 120_000, 240_000, 300_000, 300_000]
    );
    assert!(!w.take_notices_changed(), "no flapping while it fails");
    let mut list = Vec::new();
    w.notices(&mut list);
    assert_eq!(list.len(), 1);
    // An answer from another volume changes nothing; its own clears it.
    w.volume_answered("/Volumes/Other", true);
    w.volume_answered("/Volumes/NAS", true);
    assert!(w.take_notices_changed());
    list.clear();
    w.notices(&mut list);
    assert!(list.is_empty());
}

#[test]
fn a_batch_of_an_older_generation_is_dropped() {
    let (mut w, _rx, fs) = manual();
    w.apply(vec![
        KeyChange::Reset,
        KeyChange::Watch {
            key: A.into(),
            path: A.into(),
            id: 1,
        },
    ]);
    let batch = w.manual_pass().unwrap();
    assert_eq!(batch.generation, 1);
    fs.put(A, 1);
    w.apply(vec![KeyChange::Reset]);
    assert_eq!(w.accept(batch), None);
    assert_eq!(w.batches(), 0);
    assert_eq!(w.manual_pass(), None);
}

struct Panicky;
impl WatchFs for Panicky {
    fn stat(&self, _: &Path) -> io::Result<Stat> {
        panic!("a bug in the watcher")
    }
}

fn wait_finished(w: &MediaWatch) {
    let Runner::Thread(t) = &w.runner else {
        panic!("not threaded")
    };
    let h = t.handle.as_ref().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !h.is_finished() {
        assert!(Instant::now() < deadline, "the watcher did not end");
        thread::yield_now();
    }
}

#[test]
fn a_watcher_that_ends_is_restarted_once_then_the_watch_stops() {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let config = WatchConfig {
        fs: Arc::new(Panicky),
        period: Duration::from_millis(1),
        clock: EpochClock::new(|| 0),
        ..WatchConfig::default()
    };
    let mut w = MediaWatch::new(&config, tx).unwrap();
    w.apply(vec![KeyChange::Watch {
        key: A.into(),
        path: A.into(),
        id: 1,
    }]);
    wait_finished(&w);
    w.supervise();
    assert!(!w.dead);
    let logs = w.take_logs();
    assert!(
        logs.iter()
            .any(|(l, m)| *l == LogLevel::Error && m.contains("restarted"))
    );
    let mut list = Vec::new();
    w.notices(&mut list);
    assert!(list.is_empty());
    wait_finished(&w);
    w.supervise();
    assert!(w.take_notices_changed());
    w.notices(&mut list);
    assert!(
        matches!(&list[..], [WireWarning::Notice { code, .. }] if code == "media_watch_stopped")
    );
    // Nothing more happens.
    w.supervise();
    assert!(w.take_logs().iter().any(|(l, _)| *l == LogLevel::Error));
    assert!(w.take_logs().is_empty());
}
