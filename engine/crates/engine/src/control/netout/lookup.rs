// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl NetOut {
    /// Follows the workspace's network patches: starts look-ups for new host names, forgets
    /// what is gone. `all` reports every name's state, not only what changed (a newly opened
    /// workspace).
    pub fn sync(&mut self, patches: &[NetworkPatch], all: bool, now: SampleTime) -> Reports {
        let mut out = Reports::new();
        let keep: HashSet<NetworkPatchId> = patches.iter().map(|p| p.id).collect();
        let gone: Vec<NetworkPatchId> = self
            .lookups
            .keys()
            .filter(|id| !keep.contains(id))
            .copied()
            .collect();
        for id in gone {
            self.forget(id, &mut out);
        }
        self.links.retain(|id, _| keep.contains(id));
        for patch in patches {
            let named = matches!(check_endpoint(&patch.host, patch.port), Ok(HostKind::Name));
            match self.lookups.get(&patch.id) {
                Some(l) if named && l.host == patch.host && l.port == patch.port => {
                    if all {
                        out.push((patch.id, l.reported.clone()));
                    }
                }
                _ => {
                    self.forget(patch.id, &mut out);
                    if named && self.lookups.len() < MAX_TRACKED {
                        self.lookups.insert(
                            patch.id,
                            Lookup {
                                host: patch.host.clone(),
                                port: patch.port,
                                phase: Phase::Wanted,
                                reported: NetworkReachability::Unknown,
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
        out.extend(self.relieve());
        out
    }

    /// Drops a patch's look-up state. A look-up thread still running for it stays counted in
    /// `running` until it answers.
    fn forget(&mut self, id: NetworkPatchId, out: &mut Reports) {
        if let Some(link) = self.links.get(&id) {
            set_cache(&link.cache, &[]);
        }
        if let Some(l) = self.lookups.remove(&id)
            && l.reported != NetworkReachability::Unknown
        {
            out.push((id, NetworkReachability::Unknown));
        }
    }

    /// A look-up finished (or the thread gave up), or a send failed. Returns what changed for
    /// the cue engine.
    pub fn answer(&mut self, answer: NetAnswer, now: SampleTime) -> Reports {
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
                return Vec::new();
            }
        };
        // The thread is gone, whatever it was for: its slot is free again.
        self.running.remove(&ticket);
        if let Some(slot) = self.slots.remove(&ticket) {
            slot.give_back();
        }
        let retry = now.saturating_add(self.samples(RETRY_AFTER));
        let refresh = now.saturating_add(self.samples(REFRESH_AFTER));
        let mut out = Reports::new();
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
                    out.extend(Self::changed(l, patch, NetworkReachability::Resolved));
                }
                Err(reason) => {
                    l.phase = Phase::Idle {
                        retry_at: Some(retry),
                    };
                    if l.resolved_once {
                        Self::note_failed_refresh(l, &reason, &mut self.notes);
                    } else {
                        out.extend(Self::changed(
                            l,
                            patch,
                            NetworkReachability::Unresolved { reason },
                        ));
                    }
                }
            }
        }
        self.start_wanted(now);
        out.extend(self.relieve());
        out
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

    fn changed(l: &mut Lookup, patch: NetworkPatchId, now: NetworkReachability) -> Reports {
        if l.reported == now {
            return Vec::new();
        }
        l.reported = now.clone();
        vec![(patch, now)]
    }

    /// Once per tick: look-ups that timed out, retries that are due, look-ups waiting for a slot.
    pub fn tick(&mut self, now: SampleTime) -> Reports {
        let timeout = self.samples(LOOKUP_TIMEOUT);
        let retry = now.saturating_add(self.samples(RETRY_AFTER));
        let mut out = Reports::new();
        for (id, l) in &mut self.lookups {
            match l.phase {
                Phase::InFlight { since, .. } if now.saturating_sub(since) >= timeout => {
                    // The thread is not waited for, but it stays counted in `running`.
                    l.phase = Phase::Idle {
                        retry_at: Some(retry),
                    };
                    if l.resolved_once {
                        Self::note_failed_refresh(l, "the look-up timed out", &mut self.notes);
                    } else {
                        out.extend(Self::changed(
                            l,
                            *id,
                            NetworkReachability::Unresolved {
                                reason: "the look-up timed out".to_owned(),
                            },
                        ));
                    }
                }
                Phase::Idle { retry_at: Some(at) } if now >= at => l.phase = Phase::Wanted,
                _ => {}
            }
        }
        self.start_wanted(now);
        out.extend(self.relieve());
        out
    }

    /// With every slot taken by look-ups that never answered, the names that wait for a slot
    /// are not known to be bad: report them `Unknown` instead of leaving them broken.
    /// Every look-up slot is taken: this doc's, or the engine's together.
    fn slots_full(&self) -> bool {
        self.running.len() >= MAX_LOOKUPS || self.alive.alive() >= MAX_LOOKUPS
    }

    fn relieve(&mut self) -> Reports {
        if !self.slots_full() {
            self.limit_hit = false;
            return Vec::new();
        }
        let mut out = Reports::new();
        for (id, l) in &mut self.lookups {
            // A name that resolved before is already known; waiting does not change that.
            if l.phase == Phase::Wanted && !l.resolved_once {
                out.extend(Self::changed(l, *id, NetworkReachability::Unknown));
            }
        }
        if !self.limit_hit && !out.is_empty() {
            self.limit_hit = true;
            self.limit_note = true;
        }
        out
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
