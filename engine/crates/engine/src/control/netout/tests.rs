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

fn fail(net: &mut NetOut, n: u128, t: u64, now: SampleTime) {
    net.answer(
        NetAnswer::Lookup {
            patch: nid(n),
            ticket: t,
            result: Err("no such host".into()),
        },
        now,
    )
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
fn sync_looks_up_names_only() {
    let (mut net, _rx) = out();
    let patches = [
        patch(1, "lights.local"),
        patch(2, "10.0.0.2"),
        patch(3, "bad host"),
    ];
    net.sync(&patches, 0);
    assert_eq!(net.lookups.len(), 1);
    assert_eq!(net.running.len(), 1);
    let t = ticket(&net, 1);
    fail(&mut net, 1, t, 100);
    // A name that never resolved is only logged, once; no cue is broken for it.
    assert_eq!(net.take_notes().len(), 1);
    assert_eq!(net.running.len(), 0);
    // A stale ticket is ignored.
    let stale = NetAnswer::Lookup {
        patch: nid(1),
        ticket: t + 99,
        result: Ok(vec![]),
    };
    net.answer(stale, 101);
    assert!(net.take_notes().is_empty());
    // Retried after RETRY_AFTER (10 s = 10_000 samples at 1 kHz), not before.
    net.tick(9_000);
    assert_eq!(net.running.len(), 0);
    net.tick(10_100);
    assert_eq!(net.running.len(), 1);
}

/// H1: look-ups that time out and answer late free their slots, so a slow resolver does not
/// end look-ups for the rest of the session.
#[test]
fn a_slow_resolver_times_out_retries_and_recovers() {
    let (mut net, _rx) = out();
    net.sync(&[patch(1, "lights.local")], 0);
    let mut now = 0;
    let mut tickets = Vec::new();
    // Far more tries than the old give-up limit; each thread answers only after its
    // timeout, as a resolver slower than 20 s would.
    for _ in 0..40 {
        tickets.push(ticket_or_none(&net));
        now += 20_000;
        net.tick(now);
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
    net.answer(
        NetAnswer::Lookup {
            patch: nid(1),
            ticket: t,
            result: Ok(vec![a(1)]),
        },
        now,
    );
    assert_eq!(
        net.lookups.get(&nid(1)).map(|l| l.known.clone()),
        Some(vec![a(1)])
    );
}

fn ticket_or_none(net: &NetOut) -> Option<u64> {
    match net.lookups.get(&nid(1)).map(|l| &l.phase) {
        Some(Phase::InFlight { ticket, .. }) => Some(*ticket),
        _ => None,
    }
}

/// H1: when hung look-ups fill every slot the limit is noted once (no cue is affected), and
/// look-ups resume when a thread answers.
#[test]
fn hung_look_ups_filling_every_slot_do_not_break_cues() {
    let (mut net, _rx) = out();
    let names: Vec<NetworkPatch> = (1..=12).map(|n| patch(n, &format!("h{n}.local"))).collect();
    net.sync(&names, 0);
    assert_eq!(net.running.len(), MAX_LOOKUPS);
    // Every started look-up times out and none answers.
    net.tick(20_000);
    // 10 s later they are due again, but all slots are held by the hung threads: the
    // limit is noted once.
    net.tick(30_000);
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
    net.sync(&patches, 0);
    patches.extend((20..=22).map(|n| patch(n, &format!("w{n}.local"))));
    net.sync(&patches, 10);
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

fn resolve_ok(net: &mut NetOut, n: u128, addrs: Vec<SocketAddr>, now: SampleTime) {
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
    crate::spin::spin_until("workers never drained", || {
        net.outstanding.load(Ordering::SeqCst) == 0
    });
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
    net.sync(std::slice::from_ref(&p), 0);
    resolve_ok(&mut net, 1, vec![a(1)], 0);
    send_one(&mut net, &p);
    assert_eq!(sent(&rec), vec![vec![a(1)]]);
    // Not due yet.
    net.tick(59_000);
    assert!(net.running.is_empty());
    net.tick(60_000);
    assert_eq!(net.running.len(), 1, "the refresh started");
    // Sends keep working while it runs and use the new address once it answers.
    resolve_ok(&mut net, 1, vec![a(2)], 60_100);
    send_one(&mut net, &p);
    assert_eq!(sent(&rec)[1], vec![a(2)]);
    // And it keeps refreshing.
    net.tick(120_200);
    assert_eq!(net.running.len(), 1);
}

/// M1(c): a refresh that fails keeps the last-known addresses and does not break cues; one
/// log line per run of failures; a never-resolved name is logged and still sent to.
#[test]
fn a_failed_refresh_keeps_the_old_addresses_and_does_not_break_cues() {
    let (mut net, _rx, rec) = out_rec();
    let p = patch(1, "console.local");
    net.sync(std::slice::from_ref(&p), 0);
    resolve_ok(&mut net, 1, vec![a(1)], 0);
    send_one(&mut net, &p);
    net.tick(60_000);
    let t = ticket(&net, 1);
    fail(&mut net, 1, t, 60_100);
    assert_eq!(net.take_notes().len(), 1);
    send_one(&mut net, &p);
    assert_eq!(sent(&rec)[1], vec![a(1)], "still the last-known address");
    // Retried after RETRY_AFTER; failing again (here by timing out) is not logged again,
    // and the cue is still not broken.
    net.tick(70_200);
    assert_eq!(net.running.len(), 1);
    let slow = ticket(&net, 1);
    net.tick(90_300);
    // The slow thread answers at last (stale: ignored), freeing its name for a new try.
    fail(&mut net, 1, slow, 90_400);
    assert!(net.take_notes().is_empty(), "one line per run of failures");
    assert_eq!(
        net.lookups.get(&nid(1)).map(|l| l.known.clone()),
        Some(vec![a(1)])
    );
    // A success ends the run: the next failure is logged again.
    net.tick(100_400);
    resolve_ok(&mut net, 1, vec![a(3)], 100_500);
    net.tick(160_500);
    let t = ticket(&net, 1);
    fail(&mut net, 1, t, 160_600);
    assert_eq!(net.take_notes().len(), 1);
    // A name that never resolved has no addresses, is logged once, and its sends still go out
    // (the sender resolves the name itself on its worker).
    let q = patch(2, "never.local");
    net.sync(&[p, q.clone()], 160_700);
    let t = ticket(&net, 2);
    fail(&mut net, 2, t, 160_800);
    assert_eq!(net.take_notes().len(), 1);
    assert_eq!(cache_of(&net, 2), None);
    send_one(&mut net, &q);
    assert_eq!(sent(&rec).last(), Some(&Vec::new()));
}

/// M1(b): a failed TCP send forgets the cached addresses and asks for a look-up now; a
/// failed UDP send (which proves nothing about the address) does not.
#[test]
fn a_tcp_send_failure_triggers_a_fresh_look_up() {
    for (protocol, relookup) in [(NetworkProtocol::Tcp, true), (NetworkProtocol::Udp, false)] {
        let (mut net, rx, rec) = out_rec();
        let mut p = patch(1, "console.local");
        p.protocol = protocol;
        net.sync(std::slice::from_ref(&p), 0);
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
        net.answer(failed, 5_000);
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
    net.sync(std::slice::from_ref(&p), 0);
    resolve_ok(&mut net, 1, vec![a(1)], 0);
    send_one(&mut net, &p);
    let old = net.links.get(&nid(1)).map(|l| Arc::clone(&l.cache));
    let Some(old) = old else { panic!("no link") };
    // The patch now names another host; its look-up answers before any message is sent.
    let q = patch(1, "new.local");
    net.sync(std::slice::from_ref(&q), 10);
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
        net.sync(&names, round);
        net.sync(&[], round);
    }
    assert!(net.running.len() <= MAX_LOOKUPS, "{}", net.running.len());
    // The same name restarted while its old thread runs does not get a second one.
    let (mut net, _rx) = out();
    net.sync(&[patch(1, "a.local")], 0);
    net.sync(&[], 1);
    net.sync(&[patch(1, "a.local")], 2);
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
    net.sync(&[patch(1, "lights.local")], 0);
    let t = ticket(&net, 1);
    fail(&mut net, 1, t, 0);
    net.sync(&[patch(1, "10.0.0.1")], 1);
    assert!(net.lookups.is_empty());
}

#[test]
fn a_show_with_very_many_named_patches_follows_a_bounded_number() {
    let (mut net, _rx) = out();
    let patches: Vec<NetworkPatch> = (0..(MAX_TRACKED as u128 + 50))
        .map(|n| patch(n + 1, &format!("a{n}.local")))
        .collect();
    net.sync(&patches, 0);
    assert_eq!(net.lookups.len(), MAX_TRACKED);
    assert!(net.running.len() <= MAX_LOOKUPS);
}

/// Look-ups and sends that wait until released (a resolver or a connect that hangs).
#[derive(Default)]
struct Hang {
    open: Mutex<bool>,
    cv: std::sync::Condvar,
}

impl Hang {
    fn wait(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.cv.wait(open).unwrap();
        }
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.cv.notify_all();
    }
}

impl Transport for Hang {
    fn send(&self, _: &Destination, _: &[SocketAddr], _: &[u8]) -> Result<(), String> {
        self.wait();
        Ok(())
    }
    fn resolve(&self, _: &str, _: u16) -> Result<Vec<SocketAddr>, String> {
        self.wait();
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
    a.sync(&names(10), 0);
    b.sync(&names(20), 0);
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
    c.sync(&[patch(99, "late.local")], 0);
    assert_eq!(alive.alive(), 1);
    drop(c);
    assert_eq!(
        alive.alive(),
        1,
        "a hung thread of a closed doc still counts"
    );
    held.release();
    crate::spin::spin_until("the thread never ended", || alive.alive() == 0);
}

#[test]
fn at_most_max_lookups_run_at_once() {
    let (mut net, _rx) = out();
    let patches: Vec<NetworkPatch> = (0..20)
        .map(|n| patch(n + 1, &format!("a{n}.local")))
        .collect();
    net.sync(&patches, 0);
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
    crate::spin::spin_until("workers never drained", || {
        net.links
            .values()
            .all(|l| l.pending.load(Ordering::SeqCst) == 0)
    });
    for n in limit..limit + 10 {
        let o = net.send(&patch(n + 1, "10.0.0.1"), CueId(Uuid::nil()), vec![1]);
        assert_eq!(o, SendOutcome::Queued, "{n}");
    }
    assert!(net.links.len() <= MAX_LINKS);
}

/// A worker stuck in its own name look-up or connect is not idle: it is never evicted for room,
/// and a sender replaced by a host edit keeps counting against MAX_LINKS until it is done, so
/// stuck workers cannot pile up past the limit.
#[test]
fn stuck_senders_are_neither_evicted_nor_forgotten() {
    let hang = Arc::new(Hang::default());
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut net = NetOut::new(
        NetTransport(Arc::clone(&hang) as Arc<dyn Transport>),
        tx,
        1000,
    );
    let nil = CueId(Uuid::nil());
    for n in 0..MAX_LINKS as u128 {
        assert_eq!(
            net.send(&patch(n + 1, "10.0.0.1"), nil, vec![1]),
            SendOutcome::Queued
        );
    }
    // Every worker is stuck mid-send: none is idle, so a new destination is refused.
    let extra = net.send(&patch(1000, "10.0.0.1"), nil, vec![1]);
    assert!(matches!(extra, SendOutcome::Refused(_)), "{extra:?}");
    assert_eq!(net.links.len(), MAX_LINKS);
    // Editing a stuck destination's host replaces its sender; the old worker still counts.
    let refused = net.send(&patch(1, "10.0.0.9"), nil, vec![1]);
    assert!(matches!(refused, SendOutcome::Refused(_)), "{refused:?}");
    assert_eq!(net.retiring.len(), 1);
    assert!(net.links.len() + net.retiring.len() <= MAX_LINKS);
    hang.release();
    crate::spin::spin_until("workers never finished", || {
        net.outstanding.load(Ordering::SeqCst) == 0
    });
    // Done: the retired worker no longer counts and the edited destination sends.
    assert_eq!(
        net.send(&patch(1, "10.0.0.9"), nil, vec![1]),
        SendOutcome::Queued
    );
    assert!(net.retiring.is_empty());
}

/// A transport that panics while sending unwinds its worker; the link must not stay counted as
/// busy (it could then never be evicted) and the drain count must still reach zero.
#[test]
fn a_panicking_worker_still_counts_its_job_off() {
    struct Boom;
    impl Transport for Boom {
        fn send(&self, _: &Destination, _: &[SocketAddr], _: &[u8]) -> Result<(), String> {
            panic!("transport fault");
        }
        fn resolve(&self, _: &str, _: u16) -> Result<Vec<SocketAddr>, String> {
            Ok(vec![])
        }
    }
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut net = NetOut::new(NetTransport(Arc::new(Boom)), tx, 1000);
    let p = patch(1, "10.0.0.1");
    assert_eq!(
        net.send(&p, CueId(Uuid::nil()), vec![1]),
        SendOutcome::Queued
    );
    crate::spin::spin_until("the job was never counted off", || {
        net.outstanding.load(Ordering::SeqCst) == 0
            && net
                .links
                .values()
                .all(|l| l.pending.load(Ordering::SeqCst) == 0)
    });
}
