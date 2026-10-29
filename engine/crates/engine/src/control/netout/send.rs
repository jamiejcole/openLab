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
            self.links.remove(&patch.id);
            if self.links.len() >= MAX_LINKS {
                // Free a sender with nothing queued; its worker ends after any message it is
                // sending now.
                let idle = self
                    .links
                    .iter()
                    .find(|(_, l)| l.tx.is_empty())
                    .map(|(id, _)| *id);
                match idle {
                    Some(id) => {
                        self.links.remove(&id);
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
        match link.tx.try_send(Job {
            cue,
            packet,
            queued_ms,
        }) {
            Ok(()) => SendOutcome::Queued,
            Err(TrySendError::Full(_)) => {
                self.outstanding.fetch_sub(1, Ordering::SeqCst);
                SendOutcome::Refused("too many messages waiting for the destination".to_owned())
            }
            Err(TrySendError::Disconnected(_)) => {
                self.outstanding.fetch_sub(1, Ordering::SeqCst);
                // The worker is gone; the next message starts a new one.
                self.links.remove(&patch.id);
                SendOutcome::Refused("the sender stopped".to_owned())
            }
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
                    outstanding.fetch_sub(1, Ordering::SeqCst);
                }
            })?;
        Ok(Link { to, tx, cache })
    }
}
