// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl NetOut {
    /// Follows the workspace's network patches: starts look-ups for new host names, forgets
    /// what is gone. The answers only fill the senders' address caches: whether a name resolves
    /// is never a cue problem (QLab 5 does not flag it).
    pub fn sync(&mut self, patches: &[NetworkPatch], now: SampleTime) {
        let keep: HashSet<NetworkPatchId> = patches.iter().map(|p| p.id).collect();
        let gone: Vec<NetworkPatchId> = self
            .lookups
            .keys()
            .filter(|id| !keep.contains(id))
            .copied()
            .collect();
        for id in gone {
            self.forget(id);
        }
        let dropped: Vec<NetworkPatchId> = self
            .links
            .keys()
            .filter(|id| !keep.contains(id))
            .copied()
            .collect();
        for id in dropped {
            self.drop_link(id);
        }
        for patch in patches {
            let named = matches!(check_endpoint(&patch.host, patch.port), Ok(HostKind::Name));
            match self.lookups.get(&patch.id) {
                Some(l) if named && l.host == patch.host && l.port == patch.port => {}
                _ => {
                    self.forget(patch.id);
                    if named && self.lookups.len() < MAX_TRACKED {
                        self.lookups.insert(
                            patch.id,
                            Lookup {
                                host: patch.host.clone(),
                                port: patch.port,
                                phase: Phase::Wanted,
                                last_start: None,
                                known: Vec::new(),
                                resolved_once: false,
                                failing: false,
                            },
                        );
                    }
                }
            }
        }
        self.start_wanted(now);
        self.note_if_stuck();
    }

    /// Drops a patch's look-up state. A look-up thread still running for it stays counted in
    /// `running` until it answers.
    fn forget(&mut self, id: NetworkPatchId) {
        if let Some(link) = self.links.get(&id) {
            set_cache(&link.cache, &[]);
        }
        self.lookups.remove(&id);
    }

    /// A look-up finished (or the thread gave up), or a send failed.
    pub fn answer(&mut self, answer: NetAnswer, now: SampleTime) {
        let (patch, ticket, result) = match answer {
            NetAnswer::Lookup {
                patch,
                ticket,
                result,
            } => (patch, ticket, result),
            NetAnswer::SendFailed {
                patch,
                destination,
                from_transport,
                ..
            } => {
                if from_transport && destination.protocol == NetworkProtocol::Tcp {
                    self.stale(patch, &destination, now);
                }
                return;
            }
        };
        // The thread is gone, whatever it was for: its slot is free again.
        self.running.remove(&ticket);
        if let Some(slot) = self.slots.remove(&ticket) {
            slot.give_back();
        }
        let retry = now.saturating_add(self.samples(RETRY_AFTER));
        let refresh = now.saturating_add(self.samples(REFRESH_AFTER));
        // An answer of a look-up that was replaced or given up on is not about this one.
        if let Some(l) = self.lookups.get_mut(&patch)
            && matches!(l.phase, Phase::InFlight { ticket: t, .. } if t == ticket)
        {
            match result {
                Ok(addrs) => {
                    l.known.clone_from(&addrs);
                    l.resolved_once = true;
                    l.failing = false;
                    l.phase = Phase::Idle {
                        retry_at: Some(refresh),
                    };
                    if let Some(link) = self.links.get(&patch)
                        && link.to.host == l.host
                        && link.to.port == l.port
                    {
                        set_cache(&link.cache, &addrs);
                    }
                }
                Err(reason) => {
                    l.phase = Phase::Idle {
                        retry_at: Some(retry),
                    };
                    if l.resolved_once {
                        Self::note_failed_refresh(l, &reason, &mut self.notes);
                    } else {
                        Self::note_unresolved(l, &reason, &mut self.notes);
                    }
                }
            }
        }
        self.start_wanted(now);
        self.note_if_stuck();
    }

    /// A name that has never resolved is logged once per run of failures; the cue is not
    /// broken for it (QLab 5 does not flag an unreachable destination) and a send resolves
    /// the name itself on its worker.
    fn note_unresolved(l: &mut Lookup, reason: &str, notes: &mut Vec<String>) {
        if !l.failing {
            l.failing = true;
            notes.push(format!(
                "could not look up {} ({reason}); sends to it resolve the name themselves",
                l.host
            ));
        }
    }

    /// A refresh that fails keeps the last-known addresses; the first failure of a run is logged.
    fn note_failed_refresh(l: &mut Lookup, reason: &str, notes: &mut Vec<String>) {
        if !l.failing {
            l.failing = true;
            notes.push(format!(
                "could not look up {} again ({reason}); keeping its last known addresses",
                l.host
            ));
        }
    }

    /// A TCP send to `to` failed: the addresses it used may be stale, so forget them (the
    /// sender then resolves the name itself) and look the name up again now.
    fn stale(&mut self, patch: NetworkPatchId, to: &Destination, now: SampleTime) {
        let Some(l) = self.lookups.get_mut(&patch) else {
            return;
        };
        if l.host != to.host || l.port != to.port {
            return;
        }
        l.known.clear();
        if let Some(link) = self.links.get(&patch)
            && link.to.host == l.host
            && link.to.port == l.port
        {
            set_cache(&link.cache, &[]);
        }
        if matches!(l.phase, Phase::Idle { .. }) {
            l.phase = Phase::Wanted;
        }
        self.start_wanted(now);
    }

    /// Once per tick: look-ups that timed out, retries that are due, look-ups waiting for a slot.
    pub fn tick(&mut self, now: SampleTime) {
        let timeout = self.samples(LOOKUP_TIMEOUT);
        let retry = now.saturating_add(self.samples(RETRY_AFTER));
        for l in self.lookups.values_mut() {
            match l.phase {
                Phase::InFlight { since, .. } if now.saturating_sub(since) >= timeout => {
                    // The thread is not waited for, but it stays counted in `running`.
                    l.phase = Phase::Idle {
                        retry_at: Some(retry),
                    };
                    if l.resolved_once {
                        Self::note_failed_refresh(l, "the look-up timed out", &mut self.notes);
                    } else {
                        Self::note_unresolved(l, "the look-up timed out", &mut self.notes);
                    }
                }
                Phase::Idle { retry_at: Some(at) } if now >= at => l.phase = Phase::Wanted,
                _ => {}
            }
        }
        self.start_wanted(now);
        self.note_if_stuck();
    }

    /// Every look-up slot is taken: this doc's, or the engine's together.
    fn slots_full(&self) -> bool {
        self.running.len() >= MAX_LOOKUPS || self.alive.alive() >= MAX_LOOKUPS
    }

    /// With every slot taken by look-ups that never answered, names wait for one: logged once
    /// per stretch. Their cues are not affected either way.
    fn note_if_stuck(&mut self) {
        if !self.slots_full() {
            self.limit_hit = false;
            return;
        }
        let waiting = self
            .lookups
            .values()
            .any(|l| l.phase == Phase::Wanted && !l.resolved_once);
        if waiting && !self.limit_hit {
            self.limit_hit = true;
            self.limit_note = true;
        }
    }

    fn start_wanted(&mut self, now: SampleTime) {
        let mut wanted: Vec<(Option<SampleTime>, NetworkPatchId)> = self
            .lookups
            .iter()
            .filter(|(_, l)| l.phase == Phase::Wanted)
            .map(|(id, l)| (l.last_start, *id))
            .collect();
        // Never-tried names first, then the one tried longest ago (id breaks ties), so a few
        // slots serve every name in turn.
        wanted.sort();
        let retry_at = now.saturating_add(self.samples(RETRY_AFTER));
        for (_, id) in wanted {
            if self.slots_full() {
                break;
            }
            let Some(l) = self.lookups.get_mut(&id) else {
                continue;
            };
            let (host, port) = (l.host.clone(), l.port);
            // One live thread per name: a second would only wait on the same resolver.
            if self.running.values().any(|(h, p)| *h == host && *p == port) {
                continue;
            }
            self.next_ticket = self.next_ticket.wrapping_add(1);
            let ticket = self.next_ticket;
            let transport = Arc::clone(&self.transport);
            let inbox = self.inbox.clone();
            let name = host.clone();
            let doc = self.doc;
            self.alive.0.fetch_add(1, Ordering::SeqCst);
            let slot = Arc::new(Slot {
                count: Arc::clone(&self.alive.0),
                given_back: AtomicBool::new(false),
                ended: AtomicBool::new(false),
                orphaned: AtomicBool::new(false),
            });
            let ended = Ended(Arc::clone(&slot));
            let spawned = thread::Builder::new()
                .name("openlab-net-lookup".into())
                .spawn(move || {
                    let _ended = ended;
                    let result = transport.resolve(&name, port);
                    let _ = inbox.send(Inbox::Net(
                        doc,
                        NetAnswer::Lookup {
                            patch: id,
                            ticket,
                            result,
                        },
                    ));
                });
            match spawned {
                Ok(_) => {
                    l.phase = Phase::InFlight { ticket, since: now };
                    l.last_start = Some(now);
                    self.running.insert(ticket, (host, port));
                    self.slots.insert(ticket, slot);
                }
                Err(_) => {
                    slot.give_back();
                    l.phase = Phase::Idle {
                        retry_at: Some(retry_at),
                    };
                }
            }
        }
    }
}
