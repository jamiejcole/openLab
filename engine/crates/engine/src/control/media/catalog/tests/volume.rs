// SPDX-License-Identifier: GPL-3.0-or-later

use super::super::super::gate::tests::DEAD;
use super::*;

#[cfg(unix)]
struct Hang(Mutex<(usize, bool)>, std::sync::Condvar);

#[cfg(unix)]
impl Hang {
    const fn new() -> Self {
        Self(Mutex::new((0, false)), std::sync::Condvar::new())
    }

    fn block(&self) {
        let mut g = lock(&self.0);
        g.0 += 1;
        self.1.notify_all();
        while !g.1 {
            g = self
                .1
                .wait(g)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn entered(&self) -> usize {
        lock(&self.0).0
    }

    fn open(&self) {
        lock(&self.0).1 = true;
        self.1.notify_all();
    }

    fn wait_entered(&self, n: usize) {
        let mut g = lock(&self.0);
        let deadline = Instant::now() + Duration::from_secs(30);
        while g.0 < n {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "probes never came");
            g = self.1.wait_timeout(g, left).unwrap().0;
        }
    }
}

#[cfg(unix)]
static DEAD_VOLUME: Hang = Hang::new();
#[cfg(unix)]
static SLOW_FILE: Hang = Hang::new();
#[cfg(unix)]
static FIFO_VOLUME: Hang = Hang::new();

#[cfg(unix)]
fn dead_probe(path: &Path) -> Result<Probed, MediaError> {
    if path.starts_with(DEAD) {
        DEAD_VOLUME.block();
    }
    fake(path)
}

#[cfg(unix)]
fn slow_probe(path: &Path) -> Result<Probed, MediaError> {
    if path == Path::new("/slow/hang.wav") {
        SLOW_FILE.block();
    }
    fake(path)
}

#[cfg(unix)]
fn fifo_probe(path: &Path) -> Result<Probed, MediaError> {
    if path.starts_with("/mnt/fifo") {
        FIFO_VOLUME.block();
    }
    fake(path)
}

#[cfg(unix)]
fn timed_catalog(
    probe: ProbeFn,
) -> (
    MediaCatalog,
    Receiver<Inbox>,
    Arc<std::sync::atomic::AtomicU64>,
) {
    let (tx, rx) = crossbeam_channel::unbounded();
    let base = Instant::now();
    let ms = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let tick = Arc::clone(&ms);
    let clock = ProbeClock::new(move || base + Duration::from_millis(tick.load(Ordering::Acquire)));
    let cat =
        MediaCatalog::with_own_pool(tx, None, 0, probe, Duration::from_secs(30), clock).unwrap();
    (cat, rx, ms)
}

#[cfg(unix)]
fn next_answer(rx: &Receiver<Inbox>) -> ProbeAnswer {
    match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
        Inbox::Probe(_, a) => a,
        other => panic!("{other:?}"),
    }
}

#[cfg(unix)]
fn advance(ms: &std::sync::atomic::AtomicU64, secs: u64) {
    ms.fetch_add(secs * 1000, Ordering::AcqRel);
}

#[cfg(unix)]
#[test]
fn a_hung_volume_with_many_files_cannot_delay_a_healthy_one() {
    let (mut cat, rx, ms) = timed_catalog(dead_probe);
    let dead: Vec<String> = (0..6).map(|i| format!("{DEAD}/{i}.wav")).collect();
    for k in &dead {
        cat.request(k.clone());
    }
    cat.request("/healthy/h.wav".to_owned());
    DEAD_VOLUME.wait_entered(2);
    // Priming asks for the dead files again, urgently: copies of jobs a worker holds are
    // not sent, the others wait like the rest.
    cat.prioritize(dead.iter().map(String::as_str));
    assert!(cat.watchdog().is_empty(), "not stuck before the timeout");
    advance(&ms, 31);
    // Two files stuck: only they are given up on; the rest stay pending, not failed.
    let mut reported: Vec<String> = cat
        .watchdog()
        .into_iter()
        .map(|(m, _)| m.as_str().to_owned())
        .collect();
    assert_eq!(reported.len(), 2);
    assert_eq!(cat.take_volume_events(), vec![(DEAD.to_owned(), true)]);
    // The healthy file is probed by a fresh worker, whatever sits ahead of it.
    let healthy = next_answer(&rx);
    assert!(matches!(&healthy, ProbeAnswer::Probed { key, .. } if key == "/healthy/h.wav"));
    assert!(cat.answer(healthy).is_some());
    assert!(matches!(cat.get("/healthy/h.wav"), Some(Probe::Ready(_))));
    // One trial of the dead volume runs; the three others wait (no worker taken).
    DEAD_VOLUME.wait_entered(3);
    let deadline = Instant::now() + Duration::from_secs(30);
    while lock(&cat.pool.gate).deferred.len() < 3 {
        assert!(Instant::now() < deadline, "files never deferred");
        std::thread::yield_now();
    }
    assert_eq!(DEAD_VOLUME.entered(), 3);
    assert_eq!(cat.abandoned_workers(), 2);
    for k in &dead {
        if !reported.contains(k) {
            assert!(matches!(cat.get(k), Some(Probe::Pending)), "{k}");
        }
    }
    // The trial is stuck too: given up on, replaced, and the next trial waits 30 s.
    advance(&ms, 31);
    reported.extend(
        cat.watchdog()
            .into_iter()
            .map(|(m, _)| m.as_str().to_owned()),
    );
    assert_eq!(reported.len(), 3);
    assert_eq!(cat.abandoned_workers(), 3);
    advance(&ms, 29);
    cat.watchdog();
    assert_eq!(lock(&cat.pool.gate).deferred.len(), 3);
    advance(&ms, 2);
    cat.watchdog();
    DEAD_VOLUME.wait_entered(4);
    assert_eq!(cat.abandoned_workers(), 3, "waiting files cost no worker");
    // The volume comes back: stuck probes answer (late answers count) and the waiting
    // files are probed.
    DEAD_VOLUME.open();
    let mut answered = 0;
    while answered < 6 {
        cat.answer(next_answer(&rx));
        answered += 1;
        cat.watchdog();
    }
    for k in &dead {
        assert!(matches!(cat.get(k), Some(Probe::Ready(_))), "{k}");
    }
    assert_eq!(cat.take_volume_events(), vec![(DEAD.to_owned(), false)]);
}

#[cfg(unix)]
static RESPAWN_FILE: Hang = Hang::new();

#[cfg(unix)]
fn respawn_probe(path: &Path) -> Result<Probed, MediaError> {
    if path == Path::new("/respawn/hang.wav") {
        RESPAWN_FILE.block();
    }
    fake(path)
}

/// M9: a stuck worker whose replacement cannot start (out of threads) is reported once,
/// and the replacement is started on a later tick, not left missing for good.
#[cfg(unix)]
#[test]
fn a_replacement_worker_that_cannot_start_is_reported_and_retried() {
    let (mut cat, rx, ms) = timed_catalog(respawn_probe);
    cat.request("/respawn/hang.wav".to_owned());
    RESPAWN_FILE.wait_entered(1);
    advance(&ms, 31);
    cat.own_pool.as_mut().unwrap().fail_spawns = 2;
    assert_eq!(cat.watchdog().len(), 1);
    let pool = cat.own_pool.as_mut().unwrap();
    assert_eq!(pool.workers(), PROBE_WORKERS - 1);
    assert!(
        matches!(pool.take_worker_note(), Some(WorkerNote::SpawnFailed(e)) if e.contains("injected")),
    );
    // Still failing: tried again, not reported again.
    cat.watchdog();
    let pool = cat.own_pool.as_mut().unwrap();
    assert_eq!(pool.workers(), PROBE_WORKERS - 1);
    assert_eq!(pool.take_worker_note(), None);
    // The next tick starts it.
    cat.watchdog();
    let pool = cat.own_pool.as_mut().unwrap();
    assert_eq!(pool.workers(), PROBE_WORKERS);
    assert_eq!(pool.take_worker_note(), Some(WorkerNote::Restored));
    cat.request("/respawn/fine.wav".to_owned());
    assert!(cat.answer(next_answer(&rx)).is_some());
    RESPAWN_FILE.open();
}

#[cfg(unix)]
#[test]
fn one_slow_file_leaves_the_other_files_of_its_volume_alone() {
    let (mut cat, rx, ms) = timed_catalog(slow_probe);
    cat.request("/slow/hang.wav".to_owned());
    SLOW_FILE.wait_entered(1);
    cat.request("/slow/fine1.wav".to_owned());
    cat.request("/other/fine2.wav".to_owned());
    for _ in 0..2 {
        assert!(cat.answer(next_answer(&rx)).is_some());
    }
    advance(&ms, 31);
    // Only the slow file is given up on (as before); its mount is not stalled.
    let out = cat.watchdog();
    assert_eq!(out.len(), 1);
    assert!(cat.take_volume_events().is_empty());
    cat.request("/slow/fine3.wav".to_owned());
    assert!(cat.answer(next_answer(&rx)).is_some());
    assert!(lock(&cat.pool.gate).deferred.is_empty());
    for k in ["/slow/fine1.wav", "/other/fine2.wav", "/slow/fine3.wav"] {
        assert!(matches!(cat.get(k), Some(Probe::Ready(_))), "{k}");
    }
    assert_eq!(cat.abandoned_workers(), 1);
    SLOW_FILE.open();
}

#[cfg(unix)]
#[test]
fn jobs_that_never_return_do_not_stall_a_volume_for_good() {
    let (mut cat, rx, ms) = timed_catalog(fifo_probe);
    cat.request("/mnt/fifo/a.wav".to_owned());
    cat.request("/mnt/fifo/b.wav".to_owned());
    FIFO_VOLUME.wait_entered(2);
    advance(&ms, 31);
    assert_eq!(cat.watchdog().len(), 2);
    assert_eq!(
        cat.take_volume_events(),
        vec![("/mnt/fifo".to_owned(), true)]
    );
    assert!(lock(&cat.pool.gate).stalled("/mnt/fifo"));
    // Another workspace is opened: nothing of the stall carries over.
    cat.reset(None, &template().0);
    assert!(!lock(&cat.pool.gate).stalled("/mnt/fifo"));
    cat.request("/mnt/fifo/c.wav".to_owned());
    FIFO_VOLUME.wait_entered(3);
    assert!(lock(&cat.pool.gate).deferred.is_empty());
    // The old jobs' reports never reach the new state.
    FIFO_VOLUME.open();
    for _ in 0..3 {
        next_answer(&rx);
    }
    assert!(!lock(&cat.pool.gate).stalled("/mnt/fifo"));
}

#[cfg(unix)]
static WEDGED_SHARE: Hang = Hang::new();
#[cfg(unix)]
static BURNED: Hang = Hang::new();

/// The first three probes of the share (two stuck, then the trial) hang until it is opened.
#[cfg(unix)]
fn wedged_probe(path: &Path) -> Result<Probed, MediaError> {
    if path.starts_with("/burn") {
        BURNED.block();
    }
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    if path.starts_with("/Volumes/wedged") && CALLS.fetch_add(1, Ordering::AcqRel) < 3 {
        WEDGED_SHARE.block();
    }
    fake(path)
}

#[cfg(unix)]
#[test]
fn files_waiting_on_a_share_that_recovers_are_all_probed_though_stuck_jobs_never_return() {
    let (mut cat, rx, ms) = timed_catalog(wedged_probe);
    // Over a tech week many workers were abandoned (slow files on the system disk): the
    // stalled share must still get its trials and its waiting files must still be probed.
    for round in 0..MAX_ABANDONED_WORKERS / 4 {
        cat.request(format!("/burn/{round}a.wav"));
        cat.request(format!("/burn/{round}b.wav"));
        BURNED.wait_entered(2 * (round + 1));
        advance(&ms, 31);
        assert_eq!(cat.watchdog().len(), 2);
    }
    assert_eq!(cat.abandoned_workers(), MAX_ABANDONED_WORKERS / 2);
    let share = "/Volumes/wedged";
    cat.request(format!("{share}/a.wav"));
    cat.request(format!("{share}/b.wav"));
    WEDGED_SHARE.wait_entered(2);
    advance(&ms, 31);
    assert_eq!(cat.watchdog().len(), 2);
    assert!(lock(&cat.pool.gate).stalled(share));
    // Cues keep asking for files on it: one goes out as the trial and hangs, the rest wait.
    let rest: Vec<String> = (0..4).map(|i| format!("{share}/c{i}.wav")).collect();
    for k in &rest {
        cat.request(k.clone());
    }
    cat.prioritize(rest.iter().map(String::as_str));
    WEDGED_SHARE.wait_entered(3);
    let deadline = Instant::now() + Duration::from_secs(30);
    while lock(&cat.pool.gate).deferred.len() < 3 {
        assert!(Instant::now() < deadline, "files never deferred");
        std::thread::yield_now();
    }
    // The share answers: the hung reads return, the stall ends, and every waiting file is
    // probed, however many workers were abandoned before.
    WEDGED_SHARE.open();
    let mut answered = 0;
    while answered < 2 + rest.len() {
        cat.answer(next_answer(&rx));
        answered += 1;
        cat.watchdog();
    }
    for k in &rest {
        assert!(matches!(cat.get(k), Some(Probe::Ready(_))), "{k}");
    }
    assert!(
        !lock(&cat.pool.gate).stalled(share),
        "an answer ends the stall"
    );
    assert_eq!(
        cat.take_volume_events(),
        vec![(share.to_owned(), true), (share.to_owned(), false)]
    );
    BURNED.open();
}

#[test]
fn a_show_at_the_root_of_a_volume_exempts_that_volume() {
    // `bucket_of` calls a file directly in a mount folder part of `/`; the show's own volume
    // is still found.
    for dir in ["/Volumes/SHOW", "/mnt/x", "/media/u/x", "/run/media/u/x"] {
        let gate = Gate {
            show_dirs: BTreeMap::from([(DocId(0), PathBuf::from(dir))]),
            ..Gate::default()
        };
        assert!(
            gate.exempt(&bucket_of(Path::new(&format!("{dir}/a.wav")))),
            "{dir}"
        );
        assert_ne!(
            bucket_of(Path::new(dir)),
            bucket_of(&Path::new(dir).join("a")),
            "{dir}"
        );
    }
}

#[cfg(unix)]
#[test]
fn the_catalog_exempts_the_volume_of_the_show_it_was_made_or_reset_with() {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mk = |dir: Option<&str>| {
        MediaCatalog::with_own_pool(
            tx.clone(),
            dir.map(PathBuf::from),
            0,
            fake,
            Duration::from_secs(30),
            ProbeClock::system(),
        )
        .unwrap()
    };
    let mut cat = mk(Some("/Volumes/ShowDrive"));
    let exempt = |cat: &MediaCatalog, v: &str| lock(&cat.pool.gate).exempt(v);
    assert!(exempt(&cat, "/Volumes/ShowDrive"));
    assert!(!exempt(&cat, "/Volumes/Other"));
    // Another show, on another volume.
    cat.reset(Some(PathBuf::from("/Volumes/Other")), &template().0);
    assert!(exempt(&cat, "/Volumes/Other"));
    assert!(!exempt(&cat, "/Volumes/ShowDrive"));
    cat.reset(None, &template().0);
    assert!(!exempt(&cat, "/Volumes/Other"));
}
