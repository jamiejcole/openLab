// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

use openlab_core::{MAX_OSC_MESSAGE_BYTES, check_message};
use uuid::Uuid;

fn nid(n: u128) -> NetworkPatchId {
    NetworkPatchId(Uuid::from_u128(n))
}

fn patch(n: u128, host: &str) -> NetworkPatch {
    NetworkPatch {
        id: nid(n),
        name: "p".into(),
        host: host.into(),
        port: 9000,
        protocol: NetworkProtocol::Udp,
    }
}

struct Fake;
impl Transport for Fake {
    fn send(&self, _: &Destination, _: &[SocketAddr], _: &[u8]) -> Result<(), String> {
        Ok(())
    }
    fn resolve(&self, _: &str, _: u16) -> Result<Vec<SocketAddr>, String> {
        Ok(vec![])
    }
}

fn out() -> (NetOut, crossbeam_channel::Receiver<Inbox>) {
    let (tx, rx) = crossbeam_channel::unbounded();
    (NetOut::new(NetTransport(Arc::new(Fake)), tx, 1000), rx)
}

/// The ticket of `patch`'s look-up in flight.
fn ticket(net: &NetOut, n: u128) -> u64 {
    match net.lookups.get(&nid(n)).map(|l| &l.phase) {
        Some(Phase::InFlight { ticket, .. }) => *ticket,
        other => panic!("not in flight: {other:?}"),
    }
}

fn fail(net: &mut NetOut, n: u128, t: u64, now: SampleTime) -> Reports {
    net.answer(
        NetAnswer::Lookup {
            patch: nid(n),
            ticket: t,
            result: Err("no such host".into()),
        },
        now,
    )
}

fn unresolved(reason: &str) -> NetworkReachability {
    NetworkReachability::Unresolved {
        reason: reason.into(),
    }
}

#[test]
fn packets_are_udp_datagrams_or_slip_frames() {
    let args = [
        OscArg::Int { value: 1 },
        OscArg::Float { value: 0.5 },
        OscArg::String { value: "hi".into() },
        OscArg::Bool { value: true },
    ];
    let udp = packet("/a/b", &args, NetworkProtocol::Udp).unwrap_or_default();
    let want: Vec<u8> = [
        &b"/a/b\0\0\0\0"[..],
        b",ifsT\0\0\0",
        &1i32.to_be_bytes(),
        &0.5f32.to_be_bytes(),
        b"hi\0\0",
    ]
    .concat();
    assert_eq!(udp, want);
    let tcp = packet("/a/b", &args, NetworkProtocol::Tcp).unwrap_or_default();
    assert_eq!(tcp, slip_encode(&want));
    assert_eq!(tcp.first(), Some(&0xC0));
    assert_eq!(tcp.last(), Some(&0xC0));
}

/// The core's size check is exact: a message it passes encodes to at most the cap, and one
/// it refuses for size encodes to more.
#[test]
fn the_cores_size_check_matches_the_encoder() {
    for n in [1usize, 3999, 4000, 4001] {
        for count in 1..4 {
            let args = vec![
                OscArg::String {
                    value: "x".repeat(n)
                };
                count
            ];
            let encoded = packet("/a", &args, NetworkProtocol::Udp).unwrap_or_default();
            assert_eq!(
                check_message("/a", &args).is_ok(),
                encoded.len() <= MAX_OSC_MESSAGE_BYTES,
                "{n} x {count}: {}",
                encoded.len()
            );
        }
    }
}

#[test]
fn ipv4_comes_first_when_the_resolver_returns_both() {
    let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, 9000));
    let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, 9000));
    let v4b = SocketAddr::from((Ipv4Addr::new(10, 0, 0, 1), 9000));
    assert_eq!(prefer_ipv4(&[v6, v4]), vec![v4, v6]);
    assert_eq!(prefer_ipv4(&[v6, v4, v4b]), vec![v4, v4b, v6]);
    assert_eq!(prefer_ipv4(&[v6]), vec![v6]);
    assert!(prefer_ipv4(&[]).is_empty());
}

#[test]
fn sync_looks_up_names_only_and_reports_changes() {
    let (mut net, _rx) = out();
    let patches = [
        patch(1, "lights.local"),
        patch(2, "10.0.0.2"),
        patch(3, "bad host"),
    ];
    assert!(
        net.sync(&patches, true, 0)
            .iter()
            .all(|(id, r)| { *id == nid(1) && *r == NetworkReachability::Unknown })
    );
    assert_eq!(net.lookups.len(), 1);
    assert_eq!(net.running.len(), 1);
    let t = ticket(&net, 1);
    assert_eq!(
        fail(&mut net, 1, t, 100),
        vec![(nid(1), unresolved("no such host"))]
    );
    assert_eq!(net.running.len(), 0);
    // A stale ticket is ignored.
    let stale = NetAnswer::Lookup {
        patch: nid(1),
        ticket: t + 99,
        result: Ok(vec![]),
    };
    assert!(net.answer(stale, 101).is_empty());
    // Retried after RETRY_AFTER (10 s = 10_000 samples at 1 kHz), not before.
    assert!(net.tick(9_000).is_empty());
    assert_eq!(net.running.len(), 0);
    net.tick(10_100);
    assert_eq!(net.running.len(), 1);
}

/// H1: look-ups that time out and answer late free their slots, so a slow resolver does not
/// end look-ups for the rest of the session.
#[test]
fn a_slow_resolver_times_out_retries_and_recovers() {
    let (mut net, _rx) = out();
    net.sync(&[patch(1, "lights.local")], false, 0);
    let mut now = 0;
    let mut tickets = Vec::new();
    // Far more tries than the old give-up limit; each thread answers only after its
    // timeout, as a resolver slower than 20 s would.
    for _ in 0..40 {
        tickets.push(ticket_or_none(&net));
        now += 20_000;
        let r = net.tick(now);
        assert!(
            matches!(
                r.as_slice(),
                [] | [(_, NetworkReachability::Unresolved { .. })]
            ),
            "{r:?}"
        );
        // The thread finally answers (failed), after its timeout.
        if let Some(t) = tickets.last().copied().flatten() {
            fail(&mut net, 1, t, now);
        }
        now += 10_000;
        net.tick(now);
    }
    assert!(net.running.len() <= 1, "{}", net.running.len());
    // The resolver recovers: the next try resolves and the name is mended.
    let t = loop {
        if let Some(Phase::InFlight { ticket, .. }) = net.lookups.get(&nid(1)).map(|l| &l.phase) {
            break *ticket;
        }
        now += 1_000;
        net.tick(now);
    };
    let r = net.answer(
        NetAnswer::Lookup {
            patch: nid(1),
            ticket: t,
            result: Ok(vec![]),
        },
        now,
    );
    assert_eq!(r, vec![(nid(1), NetworkReachability::Resolved)]);
}

fn ticket_or_none(net: &NetOut) -> Option<u64> {
    match net.lookups.get(&nid(1)).map(|l| &l.phase) {
        Some(Phase::InFlight { ticket, .. }) => Some(*ticket),
        _ => None,
    }
}

/// H1: when hung look-ups fill every slot, waiting names are `Unknown`, not broken, the
/// limit is noted once, and look-ups resume when a thread answers.
#[test]
fn hung_look_ups_filling_every_slot_do_not_break_cues() {
    let (mut net, _rx) = out();
    let names: Vec<NetworkPatch> = (1..=12).map(|n| patch(n, &format!("h{n}.local"))).collect();
    net.sync(&names, false, 0);
    assert_eq!(net.running.len(), MAX_LOOKUPS);
    // Every started look-up times out and none answers.
    let r = net.tick(20_000);
    assert_eq!(
        r.iter()
            .filter(|(_, v)| matches!(v, NetworkReachability::Unresolved { .. }))
            .count(),
        MAX_LOOKUPS
    );
    // 10 s later they are due again, but all slots are held by the hung threads: the
    // names waiting are reported Unknown (mended), and the limit is noted once.
    let r = net.tick(30_000);
    assert_eq!(
        r.iter()
            .filter(|(_, v)| *v == NetworkReachability::Unknown)
            .count(),
        MAX_LOOKUPS
    );
    assert!(net.take_limit_note());
    net.tick(31_000);
    assert!(!net.take_limit_note(), "noted once");
    assert_eq!(net.running.len(), MAX_LOOKUPS);
    // One hung thread finally answers: its slot serves a waiting name.
    let hung = *net.running.keys().min().unwrap_or(&0);
    net.answer(
        NetAnswer::Lookup {
            patch: nid(999),
            ticket: hung,
            result: Err("late".into()),
        },
        32_000,
    );
    assert_eq!(
        net.running.len(),
        MAX_LOOKUPS,
        "a waiting name took the slot"
    );
    // The slot went to a name that had never been tried, not back to one of the hung.
    let fresh = (9..=12)
        .filter(|n| {
            net.lookups
                .get(&nid(*n))
                .is_some_and(|l| l.last_start.is_some())
        })
        .count();
    assert_eq!(fresh, 1);
}

/// L1: free slots go to never-tried names first, then to the name tried longest ago.
#[test]
fn free_slots_go_to_the_longest_waiting_name_first() {
    let (mut net, _rx) = out();
    let mut patches: Vec<NetworkPatch> =
        (1..=8).map(|n| patch(n, &format!("h{n}.local"))).collect();
    net.sync(&patches, false, 0);
    patches.extend((20..=22).map(|n| patch(n, &format!("w{n}.local"))));
    net.sync(&patches, false, 10);
    assert_eq!(net.running.len(), MAX_LOOKUPS);
    // x (20) was tried at 500, y (21) at 100, z (22) never.
    for (n, at) in [(20, Some(500)), (21, Some(100)), (22, None)] {
        if let Some(l) = net.lookups.get_mut(&nid(n)) {
            l.last_start = at;
        }
    }
    let started = |net: &NetOut, n: u128| {
        matches!(
            net.lookups.get(&nid(n)).map(|l| &l.phase),
            Some(Phase::InFlight { .. })
        )
    };
    let mut order: Vec<u128> = Vec::new();
    for (i, hung) in [1u128, 2, 3].into_iter().enumerate() {
        let t = ticket(&net, hung);
        fail(&mut net, hung, t, 1_000 + i as u64);
        for n in [22u128, 21, 20] {
            if started(&net, n) && !order.contains(&n) {
                order.push(n);
            }
        }
    }
    assert_eq!(order, vec![22, 21, 20]);
}

fn a(n: u8) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::new(10, 0, 0, n), 9000))
}

/// Records what each send was given and can be told to fail.
#[derive(Default)]
struct Rec {
    sent: Mutex<Vec<Vec<SocketAddr>>>,
    fail: AtomicBool,
}
impl Transport for Rec {
    fn send(&self, _: &Destination, resolved: &[SocketAddr], _: &[u8]) -> Result<(), String> {
        crate::lock(&self.sent).push(resolved.to_vec());
        if self.fail.load(Ordering::SeqCst) {
            Err("refused".into())
        } else {
            Ok(())
        }
    }
    fn resolve(&self, _: &str, _: u16) -> Result<Vec<SocketAddr>, String> {
        Ok(vec![])
    }
}

fn out_rec() -> (NetOut, crossbeam_channel::Receiver<Inbox>, Arc<Rec>) {
    let rec = Arc::new(Rec::default());
    let (tx, rx) = crossbeam_channel::unbounded();
    let net = NetOut::new(NetTransport(rec.clone()), tx, 1000);
    (net, rx, rec)
}

fn resolve_ok(net: &mut NetOut, n: u128, addrs: Vec<SocketAddr>, now: SampleTime) -> Reports {
    let t = ticket(net, n);
    net.answer(
        NetAnswer::Lookup {
            patch: nid(n),
            ticket: t,
            result: Ok(addrs),
        },
        now,
    )
}

/// Waits (bounded, no sleeping) until every queued message has been handled.
fn drained(net: &NetOut) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while net.outstanding.load(Ordering::SeqCst) > 0 {
        assert!(Instant::now() < deadline, "workers never drained");
        thread::yield_now();
    }
}

fn send_one(net: &mut NetOut, p: &NetworkPatch) {
    assert_eq!(
        net.send(p, CueId(Uuid::nil()), vec![1]),
        SendOutcome::Queued
    );
    drained(net);
}

fn sent(rec: &Rec) -> Vec<Vec<SocketAddr>> {
    crate::lock(&rec.sent).clone()
}

fn cache_of(net: &NetOut, n: u128) -> Option<Vec<SocketAddr>> {
    net.links
        .get(&nid(n))
        .map(|l| crate::lock(&l.cache).clone())
}

/// M1(a): a host that moves is followed: the refresh every 60 s of sample time updates
/// the addresses a live sender uses.
#[test]
fn a_refresh_sends_to_the_new_address() {
    let (mut net, _rx, rec) = out_rec();
    let p = patch(1, "console.local");
    net.sync(std::slice::from_ref(&p), false, 0);
    assert_eq!(
        resolve_ok(&mut net, 1, vec![a(1)], 0),
        vec![(nid(1), NetworkReachability::Resolved)]
    );
    send_one(&mut net, &p);
    assert_eq!(sent(&rec), vec![vec![a(1)]]);
    // Not due yet.
    net.tick(59_000);
    assert!(net.running.is_empty());
    net.tick(60_000);
    assert_eq!(net.running.len(), 1, "the refresh started");
    // The cue stays reachable while it runs and when it answers.
    assert!(resolve_ok(&mut net, 1, vec![a(2)], 60_100).is_empty());
    send_one(&mut net, &p);
    assert_eq!(sent(&rec)[1], vec![a(2)]);
    // And it keeps refreshing.
    net.tick(120_200);
    assert_eq!(net.running.len(), 1);
}

/// M1(c): a refresh that fails keeps the last-known addresses and does not break cues; one
/// log line per run of failures; a never-resolved name is still broken.
#[test]
fn a_failed_refresh_keeps_the_old_addresses_and_does_not_break_cues() {
    let (mut net, _rx, rec) = out_rec();
    let p = patch(1, "console.local");
    net.sync(std::slice::from_ref(&p), false, 0);
    resolve_ok(&mut net, 1, vec![a(1)], 0);
    send_one(&mut net, &p);
    net.tick(60_000);
    let t = ticket(&net, 1);
    assert!(fail(&mut net, 1, t, 60_100).is_empty(), "not broken");
    assert_eq!(net.take_notes().len(), 1);
    send_one(&mut net, &p);
    assert_eq!(sent(&rec)[1], vec![a(1)], "still the last-known address");
    // Retried after RETRY_AFTER; failing again (here by timing out) is not logged again,
    // and the cue is still not broken.
    assert!(net.tick(70_200).is_empty());
    assert_eq!(net.running.len(), 1);
    let slow = ticket(&net, 1);
    assert!(net.tick(90_300).is_empty());
    // The slow thread answers at last (stale: ignored), freeing its name for a new try.
    assert!(fail(&mut net, 1, slow, 90_400).is_empty());
    assert!(net.take_notes().is_empty(), "one line per run of failures");
    assert_eq!(
        net.lookups.get(&nid(1)).map(|l| l.reported.clone()),
        Some(NetworkReachability::Resolved)
    );
    // A success ends the run: the next failure is logged again.
    net.tick(100_400);
    assert!(resolve_ok(&mut net, 1, vec![a(3)], 100_500).is_empty());
    net.tick(160_500);
    let t = ticket(&net, 1);
    assert!(fail(&mut net, 1, t, 160_600).is_empty());
    assert_eq!(net.take_notes().len(), 1);
    // A name that never resolved is Unresolved.
    net.sync(&[p, patch(2, "never.local")], false, 160_700);
    let t = ticket(&net, 2);
    assert_eq!(
        fail(&mut net, 2, t, 160_800),
        vec![(nid(2), unresolved("no such host"))]
    );
}

/// M1(b): a failed TCP send forgets the cached addresses and asks for a look-up now; a
/// failed UDP send (which proves nothing about the address) does not.
#[test]
fn a_tcp_send_failure_triggers_a_fresh_look_up() {
    for (protocol, relookup) in [(NetworkProtocol::Tcp, true), (NetworkProtocol::Udp, false)] {
        let (mut net, rx, rec) = out_rec();
        let mut p = patch(1, "console.local");
        p.protocol = protocol;
        net.sync(std::slice::from_ref(&p), false, 0);
        resolve_ok(&mut net, 1, vec![a(1)], 0);
        rec.fail.store(true, Ordering::SeqCst);
        send_one(&mut net, &p);
        let failed = loop {
            match rx.recv() {
                Ok(Inbox::Net(_, f @ NetAnswer::SendFailed { .. })) => break f,
                Ok(_) => {}
                Err(e) => panic!("{e}"),
            }
        };
        assert!(net.answer(failed, 5_000).is_empty());
        assert_eq!(net.running.len(), usize::from(relookup));
        let want = if relookup { vec![] } else { vec![a(1)] };
        assert_eq!(cache_of(&net, 1), Some(want));
        if relookup {
            // The fresh look-up finds the host at its new address, and the sender uses it.
            rec.fail.store(false, Ordering::SeqCst);
            resolve_ok(&mut net, 1, vec![a(9)], 5_100);
            send_one(&mut net, &p);
            assert_eq!(sent(&rec).last(), Some(&vec![a(9)]));
        }
    }
}

/// L2: a replaced link has its own cache; the old worker never sees the new
/// destination's addresses, even from a look-up answer that arrives before the first send.
#[test]
fn a_replaced_link_does_not_share_its_cache() {
    let (mut net, _rx, rec) = out_rec();
    let p = patch(1, "old.local");
    net.sync(std::slice::from_ref(&p), false, 0);
    resolve_ok(&mut net, 1, vec![a(1)], 0);
    send_one(&mut net, &p);
    let old = net.links.get(&nid(1)).map(|l| Arc::clone(&l.cache));
    let Some(old) = old else { panic!("no link") };
    // The patch now names another host; its look-up answers before any message is sent.
    let q = patch(1, "new.local");
    net.sync(std::slice::from_ref(&q), false, 10);
    resolve_ok(&mut net, 1, vec![a(2)], 20);
    assert_ne!(*crate::lock(&old), vec![a(2)]);
    send_one(&mut net, &q);
    assert_eq!(sent(&rec).last(), Some(&vec![a(2)]));
    let new = net.links.get(&nid(1)).map(|l| Arc::clone(&l.cache));
    assert!(new.is_some_and(|n| !Arc::ptr_eq(&n, &old)));
    assert_ne!(*crate::lock(&old), vec![a(2)]);
}

/// M2: forgetting an in-flight look-up does not free its thread's slot, and a name has
/// at most one live thread.
#[test]
fn rapid_forget_and_restart_cannot_pile_up_threads() {
    let (mut net, _rx) = out();
    let names: Vec<NetworkPatch> = (1..=8).map(|n| patch(n, &format!("h{n}.local"))).collect();
    for round in 0..50 {
        net.sync(&names, false, round);
        net.sync(&[], false, round);
    }
    assert!(net.running.len() <= MAX_LOOKUPS, "{}", net.running.len());
    // The same name restarted while its old thread runs does not get a second one.
    let (mut net, _rx) = out();
    net.sync(&[patch(1, "a.local")], false, 0);
    net.sync(&[], false, 1);
    net.sync(&[patch(1, "a.local")], false, 2);
    assert_eq!(net.running.len(), 1);
    assert!(
        net.lookups
            .get(&nid(1))
            .is_some_and(|l| l.phase == Phase::Wanted)
    );
    // Its old thread answers: the name is looked up again.
    let old = *net.running.keys().next().unwrap_or(&0);
    net.answer(
        NetAnswer::Lookup {
            patch: nid(1),
            ticket: old,
            result: Ok(vec![]),
        },
        3,
    );
    assert_eq!(net.running.len(), 1);
    assert_ne!(*net.running.keys().next().unwrap_or(&0), old);
}

#[test]
fn removing_or_editing_a_patch_clears_its_state() {
    let (mut net, _rx) = out();
    net.sync(&[patch(1, "lights.local")], false, 0);
    let t = ticket(&net, 1);
    fail(&mut net, 1, t, 0);
    let r = net.sync(&[patch(1, "10.0.0.1")], false, 1);
    assert_eq!(r, vec![(nid(1), NetworkReachability::Unknown)]);
    assert!(net.lookups.is_empty());
}

#[test]
fn a_show_with_very_many_named_patches_follows_a_bounded_number() {
    let (mut net, _rx) = out();
    let patches: Vec<NetworkPatch> = (0..(MAX_TRACKED as u128 + 50))
        .map(|n| patch(n + 1, &format!("a{n}.local")))
        .collect();
    net.sync(&patches, false, 0);
    assert_eq!(net.lookups.len(), MAX_TRACKED);
    assert!(net.running.len() <= MAX_LOOKUPS);
}

/// Look-ups that wait until released.
#[derive(Default)]
struct Hang {
    open: Mutex<bool>,
    cv: std::sync::Condvar,
}

impl Hang {
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.cv.notify_all();
    }
}

impl Transport for Hang {
    fn send(&self, _: &Destination, _: &[SocketAddr], _: &[u8]) -> Result<(), String> {
        Ok(())
    }
    fn resolve(&self, _: &str, _: u16) -> Result<Vec<SocketAddr>, String> {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.cv.wait(open).unwrap();
        }
        Ok(vec![])
    }
}

/// The bound on look-up threads is the engine's: two docs' network outputs share it, and a
/// thread counts itself off when it ends, whichever doc it was for.
#[test]
fn two_docs_share_the_look_up_bound() {
    let hang = Arc::new(Hang::default());
    let alive = LookupThreads::default();
    let disabled = Arc::new(AtomicBool::new(false));
    let (tx, rx) = crossbeam_channel::unbounded();
    let make = |doc| {
        NetOut::shared(
            NetTransport(Arc::clone(&hang) as Arc<dyn Transport>),
            tx.clone(),
            1000,
            DocId(doc),
            Arc::clone(&disabled),
            alive.clone(),
        )
    };
    let (mut a, mut b) = (make(0), make(1));
    let names = |base: u128| -> Vec<NetworkPatch> {
        (0..6)
            .map(|n| patch(base + n, &format!("d{base}-{n}.local")))
            .collect()
    };
    a.sync(&names(10), false, 0);
    b.sync(&names(20), false, 0);
    assert_eq!(a.running.len(), 6);
    assert_eq!(
        b.running.len(),
        MAX_LOOKUPS - 6,
        "only the engine's slots left"
    );
    assert_eq!(alive.alive(), MAX_LOOKUPS);
    // The override is one flag: turned on through one doc, the other sends nothing.
    a.disabled.store(true, Ordering::SeqCst);
    assert!(b.disabled());
    hang.release();
    // Each answer is tagged with the doc that asked; a slot is free once its doc took it,
    // and B's waiting names take the free slots as answers come in.
    let mut tagged = Vec::new();
    while a.running.len() + b.running.len() > 0 {
        match rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap() {
            Inbox::Net(doc, answer) => {
                if let NetAnswer::Lookup { patch, .. } = &answer {
                    tagged.push((doc, *patch));
                }
                let net = if doc == DocId(0) { &mut a } else { &mut b };
                net.answer(answer, 1);
                assert!(alive.alive() <= MAX_LOOKUPS);
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(tagged.iter().filter(|(d, _)| *d == DocId(0)).count(), 6);
    assert!(
        tagged
            .iter()
            .all(|(d, p)| (*d == DocId(0)) == (p.0.as_u128() < 20))
    );
    assert_eq!(alive.alive(), 0);
    // A doc closed with a look-up hung: the slot stays taken until the thread ends.
    let held = Arc::new(Hang::default());
    let mut c = NetOut::shared(
        NetTransport(Arc::clone(&held) as Arc<dyn Transport>),
        tx.clone(),
        1000,
        DocId(2),
        Arc::clone(&disabled),
        alive.clone(),
    );
    c.sync(&[patch(99, "late.local")], false, 0);
    assert_eq!(alive.alive(), 1);
    drop(c);
    assert_eq!(
        alive.alive(),
        1,
        "a hung thread of a closed doc still counts"
    );
    held.release();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while alive.alive() > 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the thread never ended"
        );
        std::thread::yield_now();
    }
}

#[test]
fn at_most_max_lookups_run_at_once() {
    let (mut net, _rx) = out();
    let patches: Vec<NetworkPatch> = (0..20)
        .map(|n| patch(n + 1, &format!("a{n}.local")))
        .collect();
    net.sync(&patches, false, 0);
    assert_eq!(net.running.len(), MAX_LOOKUPS);
}

#[test]
fn disabled_output_sends_nothing() {
    let (mut net, _rx) = out();
    net.set_disabled(true);
    assert!(net.disabled());
    let o = net.send(&patch(1, "10.0.0.1"), CueId(Uuid::nil()), vec![1]);
    assert_eq!(o, SendOutcome::Suppressed);
    assert!(net.links.is_empty());
}

/// L2: an idle sender is dropped to make room once MAX_LINKS is reached.
#[test]
fn idle_senders_are_freed_when_the_limit_is_reached() {
    let (mut net, _rx) = out();
    let limit = MAX_LINKS as u128;
    for n in 0..limit {
        let o = net.send(&patch(n + 1, "10.0.0.1"), CueId(Uuid::nil()), vec![1]);
        assert_eq!(o, SendOutcome::Queued, "{n}");
    }
    // The workers drain on their own threads; wait until every queue is empty, so the
    // senders are idle (bounded: a failure guard, not a timing assumption).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while net.links.values().any(|l| !l.tx.is_empty()) {
        assert!(
            std::time::Instant::now() < deadline,
            "workers never drained"
        );
        std::thread::yield_now();
    }
    for n in limit..limit + 10 {
        let o = net.send(&patch(n + 1, "10.0.0.1"), CueId(Uuid::nil()), vec![1]);
        assert_eq!(o, SendOutcome::Queued, "{n}");
    }
    assert!(net.links.len() <= MAX_LINKS);
}
