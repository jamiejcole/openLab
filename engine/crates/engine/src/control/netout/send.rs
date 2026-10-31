// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl NetOut {
    /// Hands a message to its destination's worker.
    pub fn send(&mut self, patch: &NetworkPatch, cue: CueId, packet: Vec<u8>) -> SendOutcome {
        if self.disabled() {
            return SendOutcome::Suppressed;
        }
        let to = Destination::of(patch);
        if self.links.get(&patch.id).is_none_or(|l| l.to != to) {
            self.drop_link(patch.id);
            self.retiring.retain(|p| p.load(Ordering::SeqCst) > 0);
            if self.links.len() + self.retiring.len() >= MAX_LINKS {
                // Free a sender with nothing queued and nothing being sent (a worker stuck in a
                // resolver or a connect is not idle).
                let idle = self
                    .links
                    .iter()
                    .find(|(_, l)| l.pending.load(Ordering::SeqCst) == 0)
                    .map(|(id, _)| *id);
                match idle {
                    Some(id) => self.drop_link(id),
                    None if self.links.len() < MAX_LINKS => {
                        return SendOutcome::Refused(
                            "too many senders are stuck on slow destinations".to_owned(),
                        );
                    }
                    None => {
                        return SendOutcome::Refused("too many network destinations".to_owned());
                    }
                }
            }
            match self.spawn_link(patch.id, to) {
                Ok(link) => {
                    self.links.insert(patch.id, link);
                }
                Err(e) => return SendOutcome::Refused(format!("could not start a sender: {e}")),
            }
        }
        let queued_ms = self.transport.now_ms();
        let Some(link) = self.links.get(&patch.id) else {
            return SendOutcome::Refused("no sender".to_owned());
        };
        self.outstanding.fetch_add(1, Ordering::SeqCst);
        link.pending.fetch_add(1, Ordering::SeqCst);
        match link.tx.try_send(Job {
            cue,
            packet,
            queued_ms,
        }) {
            Ok(()) => SendOutcome::Queued,
            Err(TrySendError::Full(_)) => {
                link.pending.fetch_sub(1, Ordering::SeqCst);
                self.outstanding.fetch_sub(1, Ordering::SeqCst);
                SendOutcome::Refused("too many messages waiting for the destination".to_owned())
            }
            Err(TrySendError::Disconnected(_)) => {
                link.pending.fetch_sub(1, Ordering::SeqCst);
                self.outstanding.fetch_sub(1, Ordering::SeqCst);
                // The worker is gone; the next message starts a new one.
                self.drop_link(patch.id);
                SendOutcome::Refused("the sender stopped".to_owned())
            }
        }
    }

    /// Removes a sender. One with work still pending keeps counting against [`MAX_LINKS`].
    pub(super) fn drop_link(&mut self, id: NetworkPatchId) {
        if let Some(link) = self.links.remove(&id)
            && link.pending.load(Ordering::SeqCst) > 0
        {
            self.retiring.push(link.pending);
        }
    }

    fn spawn_link(&mut self, patch: NetworkPatchId, to: Destination) -> std::io::Result<Link> {
        let known = self
            .lookups
            .get(&patch)
            .filter(|l| l.host == to.host && l.port == to.port)
            .map(|l| l.known.clone())
            .unwrap_or_default();
        let cache: Resolved = Arc::new(Mutex::new(known));
        let (tx, rx) = bounded::<Job>(LINK_QUEUE);
        let transport = Arc::clone(&self.transport);
        let inbox = self.inbox.clone();
        let dest = to.clone();
        let outstanding = Arc::clone(&self.outstanding);
        let disabled = Arc::clone(&self.disabled);
        let resolved = Arc::clone(&cache);
        let pending = Arc::new(AtomicUsize::new(0));
        let busy = Arc::clone(&pending);
        let doc = self.doc;
        thread::Builder::new()
            .name("openlab-net-send".into())
            .spawn(move || {
                // Ends when the control thread drops the link (a destination that changed, or
                // shutdown), after sending what is queued.
                while let Ok(job) = rx.recv() {
                    let outcome = if disabled.load(Ordering::SeqCst) {
                        Ok(())
                    } else if transport.now_ms().saturating_sub(job.queued_ms) > MAX_MESSAGE_AGE_MS
                    {
                        Err(("destination too late".to_owned(), false))
                    } else {
                        let found = crate::lock(&resolved).clone();
                        transport
                            .send(&dest, &found, &job.packet)
                            .map_err(|e| (e, true))
                    };
                    if let Err((reason, from_transport)) = outcome {
                        let _ = inbox.send(Inbox::Net(
                            doc,
                            NetAnswer::SendFailed {
                                cue: job.cue,
                                patch,
                                destination: dest.clone(),
                                reason,
                                from_transport,
                            },
                        ));
                    }
                    busy.fetch_sub(1, Ordering::SeqCst);
                    outstanding.fetch_sub(1, Ordering::SeqCst);
                }
            })?;
        Ok(Link {
            to,
            tx,
            cache,
            pending,
        })
    }
}
