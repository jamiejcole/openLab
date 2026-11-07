# Network cue sending and the output override (CT-30, PB-10, NFR-07)

Core decides what the cue does; the binary (`engine/crates/engine/src/control/netout/`) delivers. Wire/stored shape of the cue and patches: `docs/protocol.md`, `docs/file-format.md`. Cue-engine side (action timing, broken-cue state): skill `cue-engine`.

## Must not break
- **A Network cue is never broken for an unresolved host name, an unreachable destination, or a destination nobody listens on** (user decision; matches QLab 5, manual pp. 371-372, which breaks it only for no patch, an incomplete patch and a missing or invalid message). Never add a "host unresolved" problem. A failed send is a warning, never cue state.
- No socket I/O, name look-up or wait on the control thread or audio thread. A slow, dead or hung destination delays only its own messages.
- **No probing**: no ping, no connect check, no periodic test of a destination. UDP cannot tell if anyone listens; a TCP connect would open connections to the show's devices unasked. Only name resolution runs ahead of time, and IP literals are never looked up.
- A cue fired seconds late on stage is worse than none: a message older than 1 s in its queue is dropped.
- The cue is complete once handed over; delivery failure never changes its state.

## Cue semantics
- No duration: when its action starts it emits `Action::SendOsc { cue, at, destination, address, args }` and completes at once; pre-wait, post-wait and continue modes behave as for a Memo.
- `at` is the sample the action started on. OSC is not sample accurate: the binary sends when `at` enters its lookahead window (up to one device block early) or at once if late.
- Only an armed cue without problems emits `SendOsc`. Disarmed: skipped like any disarmed cue. Broken: skipped for its problems.
- **Send now** (inspector Send button, Preview Selected): `Input::Preview` on a Network cue emits one `SendOsc { at: now }` and nothing else: no preview state, no `PreviewChanged`, no cue state, playhead or continue, never `already_previewing`. Problems: `CueBroken` warning, nothing sent. Disarmed: `PreviewIgnored { reason: disarmed }`.
- Per destination, messages leave in the order the cues fired.

## Problems found before GO (from the model, by core)
`network_patch_missing`, `network_host_invalid { reason }` (empty host, neither an IP nor a valid host name, digits-and-dots that are not an IPv4 address, port 0; IPv4, IPv6 with or without brackets, or a name), `network_message_invalid { reason }`.

Message limits, so one message is one datagram on every platform (macOS default limit 9216); invalid is a cue problem, never a panic:
- Address starts with `/`, printable ASCII, no spaces, at most 512 bytes. `* ? [ ] { }` are allowed and sent as written.
- At most 32 arguments: `int` (32 bit), `float` (32 bit, finite), `string` (at most 4096 bytes, no NUL), `bool` (`T`/`F`).
- Encoded message at most 8192 bytes.
Raw UDP/TCP string messages and OSC parameter fades are not implemented.

## Sending
- The control thread encodes (`osc::encode_message`, the encoder the listener uses) and hands the message to a **worker thread per destination** through a bounded queue (`LINK_QUEUE` = 32). Workers start on first use, at most `MAX_LINKS` (64); an idle sender is dropped to make room; a worker ends when its destination changes or the engine stops.
- UDP: one datagram per message, from one socket per address family (a receiver sees one stable source port). TCP: OSC 1.1 SLIP frame (END at both ends), a new connection per message, closed after it, 2 s connect/send timeout. Length-prefixed OSC 1.0 is not offered (our reading of what a QLab TCP patch sends; unconfirmed).
- Age: a message waiting more than `MAX_MESSAGE_AGE_MS` (1 s, transport monotonic clock) is dropped with "destination too late". A full queue refuses the message. A failed send (no route, refused, TCP timeout) returns as `Inbox::Net(NetAnswer::SendFailed)`. Both become a runtime `notice` warning `network_send_failed` for the cue plus a show-log line.
- The sender uses the addresses the last look-up found, **IPv4 first** (on macOS `localhost` can resolve to `::1` first and an IPv4-bound receiver would never see the datagram). No resolver call per message.

## Name look-ups
- Names are resolved ahead so sending is cheap: on workspace open and when patches change, `NetOut::sync` starts a look-up (`getaddrinfo` only, on a short-lived thread) that fills that sender's address cache. Nothing is reported to the cue engine.
- Bounds: at most `MAX_LOOKUPS` (8) threads alive engine-wide across all docs (`LookupThreads`; a look-up holds its slot until its doc takes the answer; timed-out and forgotten ones count until they answer), one per name in a doc, `MAX_TRACKED` (256) names followed. If hung threads fill every slot one log line says so and sends resolve the name themselves.
- A send to a name with an empty cache resolves it itself **on the destination's worker**. A name that resolves at open but not at GO is a send failure (warning), not a stall.
- Failed look-up: logged once per run of failures, retried every `RETRY_AFTER` (10 s); a look-up unanswered after 20 s counts as failed (its thread is not waited for).
- Resolved name: re-looked up every `REFRESH_AFTER` (60 s) so a changed address (DHCP, rebooted console) is followed. A refresh that fails keeps the last-known addresses (one log line per failure streak, retried every 10 s). A failed **TCP** send clears that destination's addresses and looks up at once; a failed UDP send proves nothing about the address.
- These timers run on **sample time**, which stands still during an audio outage; retries resume once audio is back.
- Each sender holds its own address snapshot, so a replaced sender's old worker never sees the new destination's.

## Output override (PB-10)
- `Request::SetOutputOverrides { network_disabled }` sets one engine-wide flag shared by every doc's `NetOut` and send threads, settable through any doc. It is in memory only: not in a show file, machine setting or undo step; a restarted engine starts with output on.
- With it set, `SendOsc` is received and dropped (info log, nothing queued): Network cues still run, complete and continue. Turning it on also drops what is queued (the sender checks before each send); only a message already being sent goes out.
- `network_output_disabled` is in every `status` message and a change sends one early (`docs/protocol.md`). The UI must show it plainly.
