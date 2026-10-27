# OSC API (NET-01, NET-02, NET-05, TRG-03, DOC-02)

Status: implemented. The socket-free layer is `engine/crates/engine/src/osc/` (decoding, address dispatch, passcode and updates state, replies); the UDP and TCP listeners and the update messages are `engine/crates/engine/src/net/osc.rs` and `osc_mirror.rs` (see "Listeners" below and [network.md](network.md)). `host::run_show` starts them; if port 53000 is taken the engine logs a warning and runs without OSC. This file lists exactly what is done; anything not listed is not implemented.

The address space follows QLab 5's OSC dictionary, so existing remotes keep working where openLab has the feature. Where we were unsure what QLab does, we chose the more conservative behaviour and list it under "Open questions".

## Transport

- UDP and TCP on port **53000**, IPv4, on every interface (as QLab does: set a passcode on a network you do not control). Replies and updates to a UDP peer go to its IP address on port **53001**.
- TCP carries OSC 1.1 SLIP frames (END `0xC0` at both ends; `0xDB 0xDC` is a literal END, `0xDB 0xDD` a literal ESC). The length-prefixed framing of OSC 1.0 is not supported.
- A UDP peer is identified by its IP address, a TCP peer by its connection.

## Listeners

Both sockets run on the network runtime (the one that serves the UI over WebSocket; no extra threads). One task owns all OSC state: the sessions, a copy of the workspace and the TCP connection table. The sockets' tasks only pass bytes to it through bounded queues, so one slow or hostile peer never delays another, and the control thread is never waited for or blocked.

- **Input.** Each accepted message becomes the same `command` or `transport` message a WebSocket client sends, on a reply channel of its own. Request ids start at 2^63 and never reach `u64::MAX` (the WebSocket server's own id); the control loop answers each request on the channel it came from, so they cannot clash with a UI client's anyway. At most 256 requests may be unanswered at once; beyond that new inputs are dropped and counted (`inputs_dropped`), so a UDP flood cannot pile up in the control inbox. A request the control loop answers with an error is counted (`control_refused`); it gets no reply to the OSC peer.
- **The workspace copy.** Answering `/cue/1/name`, resolving `/cue/{number}` and `/cue_id/{id}`, a cue's OSC trigger and the passcode all read a copy the OSC task keeps from the engine's own messages: the `snapshot` it asks for at start, then `model_changed` (applied with `Workspace::apply_changes`), `runtime` (playheads, cue states) and `document`. Nothing asks the control thread a question per message. A gap in the revision or seq, an edit that does not apply, a `document` message with another file path, or a broadcast queue that overflowed makes it ask for a new snapshot. While that is out, broadcasts wait (as in the WebSocket server), so the copy never goes back in time. Until the first snapshot arrives OSC input waits in the queues.
- **Active cue list.** `/go` and `/playhead` act on the first cue list of the workspace; the engine has no other notion of an active list.
- **Sessions end** (`/connect` and `/updates` forgotten) when the workspace id or the passcode in a new snapshot differs from the one before, and for a TCP peer when its connection closes. A passcode edit reaches the OSC task through a snapshot: today no command edits workspace settings, so this happens when a snapshot replaces the workspace (PROGRESS M4.4a: open/new/revert must reach every client).
- **Replies.** UDP: to the sender's IP on port 53001 (a datagram is never answered on its source port). TCP: SLIP-framed on the same connection. Sending never waits: a full socket buffer or write queue loses the message (`send_failed`). A TCP peer that does not read for 10 s (one write that makes no progress) is disconnected.
- **TCP connections.** At most 16 at once (the WebSocket server's limit; far above the consoles, Companion and scripts a show has; each costs a task, a 64 KiB frame limit and a 256-message write queue). The next is closed at accept, counted (`tcp_refused`) and logged as a warning at most once per 10 s with a count (`Inbox::Log`, like the WebSocket server). There is no idle timeout: a console that only listens for updates must stay connected. A host that opens 16 silent connections can therefore hold every slot until it disconnects; this is the same trust assumption as the rest of an unauthenticated OSC port.
- **Counters.** `WsServer::osc_stats()` returns the `OscStats` plus `udp_dropped` (the task's queue was full), `tcp_refused`, `inputs_dropped`, `send_failed`, `control_refused` and `tcp_connections`. Malformed, oversize, bad-address, throttled and dropped input is also logged, as one warning with counts, every 10 s while it happens.

## Limits

| Limit | Value | When exceeded |
|---|---|---|
| Datagram or SLIP frame | 64 KiB | datagram: dropped, counted as malformed; stream: the frame is skipped to its next END without being buffered, counted as `oversize_frames` |
| Bundle nesting | 4 levels (the outermost counts as 1) | whole packet dropped, malformed |
| Messages in one packet | 128 | whole packet dropped, malformed |
| Array nesting in type tags | 4 levels | message dropped, malformed |
| Address | 256 bytes, 8 `/`-separated parts, no empty part (one trailing `/` is allowed) | dropped, counted as `bad_address` |
| Peers with a session (connected or subscribed) | 1024 | a full table refuses newcomers (their `/connect` or `/updates` gets an error reply) until idle UDP peers expire or TCP connections close; existing peers are never evicted, so a flood of forged sources cannot push out a real console |
| Replies per datagram or stream read | 16 | the rest are dropped, counted as `replies_dropped` |
| Reply JSON body | 1400 bytes | replaced by an `error` reply with data `"reply too large"` (keeps a reply near one unfragmented UDP datagram) |
| Reply and update bytes to one UDP source IP | 16 KiB burst, refilling 8 KiB/s (headers counted) | further replies dropped, counted as `replies_throttled`; further updates as `updates_throttled` |
| Open TCP connections | 16 | the next is closed at accept, counted as `tcp_refused`, logged (at most every 10 s) |
| Requests to the engine not yet answered | 256 | further inputs dropped, counted as `inputs_dropped` |
| Update messages from one change | 32 cues | one `/update/workspace/{ws}` instead |

Bundle time tags are ignored: every message runs when it arrives, in bundle order. Bundles are flattened.

Malformed or oversize input never reaches the engine. It is dropped and counted (`OscStats`: `packets`, `messages`, `malformed`, `oversize_frames`, `bad_address`, `unknown_address`, `wrong_workspace`, `denied`, `rejected`, `replies_dropped`, `replies_throttled`, `updates_throttled`).

## Addresses

Every address may be prefixed with `/workspace/{id}`, where `{id}` is the open workspace's id (compared case-insensitively). A message for another id is ignored, with no reply (counted as `wrong_workspace`). Address matching is exact and case-sensitive. **Wildcards and alternatives (`* ? [ ] { }`) are not expanded**; such an address gets an error reply, unless it is exactly a cue's OSC trigger address (the trigger is looked up first, so trigger addresses may contain those characters).

Each accepted message becomes the same input the UI sends (`docs/protocol.md`); OSC adds no engine behaviour. "The list" below is the active cue list.

| Address | Arguments | Becomes |
|---|---|---|
| `/go` | none (any are ignored) | `transport go` on the list |
| `/panic` | none | `transport panic` |
| `/pause` | none | `transport pause_all` |
| `/resume` | none | `transport resume_all` |
| `/playhead/next`, `/playhead/previous` | none | `transport playhead_next` / `playhead_previous` |
| `/playhead/{number}` | none | `transport set_playhead_by_number` |
| `/select/next`, `/select/previous`, `/select/{number}` | none | the same as `/playhead/...` (see open questions) |
| `/cue/{number}/start` | none | `transport start` |
| `/cue/{number}/stop` | none | `transport stop` (no fade) |
| `/cue/{number}/hardStop` | none | `transport hard_stop` |
| `/cue/{number}/pause`, `/resume` | none | `transport pause` / `resume` |
| `/cue/{number}/load` | optional time in seconds, at least 0 | `transport load` (`at` 0 if omitted) |
| `/cue/{number}/reset` | none | `transport reset` |
| `/cue/{number}/preview` | none | `transport preview` |
| `/cue/{number}/name`, `/notes` | one string | `command set_cue_property` |
| `/cue/{number}/number` | one string, int or float; an empty string clears the number | `command set_cue_numbers` |
| `/cue/{number}/preWait`, `/postWait` | one number, finite and at least 0 | `command set_cue_property` |
| `/cue/{number}/armed`, `/flagged` | int 0 or 1, or an OSC boolean | `command set_cue_property` |
| `/cue/{number}/{property}` | none | a reply with the current value (see Replies) |
| `/cue_id/{id}/...` | as `/cue/{number}/...` | the cue is found by its id (a UUID) instead of its number |
| _any other address_ | none | if it is exactly some cue's OSC trigger address (TRG-03), `transport start` for that cue; otherwise dropped and counted as `unknown_address` |

A cue is found by its number as written in the address; the words `selected`, `playhead` and `active` have no special meaning and are looked up as numbers. A number or id that matches no cue gets an error reply. Arguments of the wrong type or count get an error reply; nothing is sent to the engine.

`/stop` (stop everything) is **not implemented**: the protocol has no stop-all action yet (PROGRESS M4.1b). It gets an error reply. Use `/panic` or `/cue/{number}/stop`.

Not implemented: `/cue/{number}/panic`, level and time properties beyond pre-wait and post-wait, `/cue/selected/...`, `/cue/active/...`, `/cue/playhead/...`, `/version`, `/workspaces`, `/cueLists`, `/runningCues`, `/new`, `/save`, and the other QLab application and workspace queries.

## Connecting and the passcode (NET-02)

- **No passcode set** (none, or empty): no message needs `/connect`. `/connect` with any argument is accepted.
- **Passcode set**: every message except `/connect` is refused until the peer has sent `/connect` with the right passcode (one string argument; a missing or non-string argument counts as wrong). Refused messages get a `denied` reply over TCP and **no reply over UDP** (the source address of a datagram may be forged); both are counted as `denied`. The refusal comes before any cue lookup, so nothing about the show leaks.
- `/connect` forms: `/connect` and `/workspace/{id}/connect`.
- A wrong `/connect` from a peer that is already connected is answered `badpass` and changes nothing: it neither disconnects the peer nor counts toward a lockout (a UDP peer is only an IP address, so anyone sharing or forging it could otherwise cut a console off mid-show).
- Failures are counted per remote IP address, for TCP too (reconnecting does not reset them). After 5 wrong passcodes from one IP, while not connected, that IP is locked out for 30 s: `/connect` during the lockout is answered `denied` without checking the passcode. Across all IPs together, 50 wrong passcodes in 60 s make every new `/connect` get `denied` until the window ends (so rotating forged source addresses does not give unlimited guesses). Peers already connected are unaffected by either.
- **Known addresses.** The engine remembers up to 64 IP addresses that completed a `/connect` (until the passcode changes or another workspace opens). A **correct** passcode from a known address passes both the per-IP lockout and the global budget, so an attacker's forged wrong guesses cannot keep the operator's console from reconnecting after its session expired or a TCP link dropped. A wrong passcode from a known address still counts and is still refused. Remaining trade-off: a device that has never connected can be blocked by an attacker's wrong guesses (first-time devices during an attack). To recover, wait for the window (60 s) or lockout (30 s) to end, or connect once from an address that is already known; the app can also reset all sessions.
- Limits of the guards: an attacker rotating 1024 forged addresses can evict the oldest reply budget (bounded: still at most 16 KiB per forged address and 16 replies per datagram), and forged traffic from the console's own address can drain its reply budget (its commands still work). Without a passcode a forged `/connect` takes a session slot just as `/updates 1` does; slots free as UDP peers go idle (10 minutes).
- Because a connected UDP peer is only an IP address, any packet from that address (including a forged one) can switch its `/updates` subscription on or off (`/updates 0`). It cannot disconnect the peer.
- A UDP peer's connection and update subscription end after 10 minutes without any packet from it; a TCP peer's end when the connection closes. The runtime clears all of them when the passcode changes or another workspace opens.
- The passcode comparison does not stop at the first differing byte (its time depends on the longer string, not on where they differ; it does not hide whether the lengths are equal). It travels in clear text: this is access control for a trusted show network, not security against an attacker on the wire.
- **Without a passcode, OSC is for trusted networks only.** Any host can then get replies and subscribe to updates, and a forged UDP source address can direct our replies at someone else. The caps above (replies per read, reply size, per-IP byte budget) bound that, but do not remove it. Set a passcode on any network you do not control. The update fan-out applies the same per-IP budget to UDP peers.
- Permission levels (view, edit, control) are not implemented: a connected peer may do everything the API offers.

## Updates (NET-05)

`/updates 1` (int or boolean true) subscribes the peer, `/updates 0` unsubscribes; it needs `/connect` first when a passcode is set. Anything else gets an error reply.

A subscribed peer is sent these messages (to its IP on port 53001 for UDP, SLIP-framed on its connection for TCP). **This is our reading of QLab 5's update scheme, not something we could check** (open question 13); no other formats are sent. `{ws}` is the workspace id, `{cue}` and `{list}` are ids, all as they appear in replies.

| Address | Argument | Sent when |
|---|---|---|
| `/update/workspace/{ws}` | none | the workspace changed beyond one cue's properties: a cue was added, removed or moved, a cue list or patch changed, a new snapshot replaced the copy, or one change touched more than 32 cues (then this one message stands for them) |
| `/update/workspace/{ws}/cue_id/{cue}` | none | a cue changed: it was edited (also for each cue added, removed or moved), or it started, stopped, paused, finished or broke (any cue state change) |
| `/update/workspace/{ws}/cueList/{list}/playbackPosition` | the cue id the playhead is on, as a string; empty string when it is past the end | a playhead moved |

Order within one change: the workspace update, then the cue updates, then playhead updates. A message is built once and sent to every subscriber. Update messages carry no data beyond this: the peer asks (`/cue/{number}/name` and the other reads) for what it wants to know.

Updates to a UDP peer share its reply byte budget (16 KiB burst, 8 KiB/s refill, headers counted: about 70 updates a second sustained), so a forged subscription cannot aim more than that at a victim; the ones over it are dropped and counted (`updates_throttled`). A TCP peer is paced by its connection (see Listeners). A peer that fell behind loses updates and should ask again for what it displays when it sees `/update/workspace/{ws}`; there is no catch-up.

## Replies

A reply is an OSC message to the sender whose address is `/reply` followed by the address that was sent (prefix included), with one string argument holding JSON:

```json
{"workspace_id": "<uuid>", "address": "/cue/1/name", "status": "ok", "data": "Opening"}
```

`status` is one of `ok`, `error`, `denied`. Replies are sent for:

- `/connect`: `ok` with data `"ok"`; `error` with data `"badpass"`; `denied` when locked out; `error` with data `"too many peers"` if the peer table is full.
- `/updates`: `ok` with data `"ok"`, or `error`.
- a property read (`/cue/{number}/{property}` with no argument): `ok` with the value as JSON (string, number or boolean), or `error` if the cue has no such property.
- anything refused: `error` with a short text in `data` (no such cue, bad argument, no cue list, unsupported address, wildcard) or `denied` (TCP only, see above).

Every reply carries the workspace id, as QLab's do, including `denied` ones to peers that have not connected.

Successful actions (`/go`, `/cue/1/start`, property writes, ...) get no reply.

## Open questions (QLab behaviour we could not confirm)

These choices are conservative and may change once checked against QLab 5.

1. Reply status strings: we use `ok`, `error`, `denied` and the `/connect` data `badpass`.
2. Which messages QLab replies to: we reply to `/connect`, `/updates`, property reads and refusals, but not to successful actions.
3. Which addresses need a passcode when none is set: we require none.
4. A wrong `/workspace/{id}` is ignored silently; QLab might reply with an error.
5. Wildcards in addresses: we reject them; QLab may expand them.
6. Case sensitivity of address parts (`hardStop`, `preWait`): we are case-sensitive.
7. `/select/{number}`: openLab keeps no selection in the engine, so we move the playhead. In QLab, selection and playhead are linked by default but not always.
8. `/stop` with no cue: needs the stop-all action (M4.1b).
9. `/cue/selected`, `/cue/active`, `/cue/playhead` and similar names: unsupported until their meaning is confirmed.
10. How long a UDP client stays connected or subscribed: we use 10 minutes of silence.
11. Whether `/go` with an argument targets a cue: we ignore arguments.
12. Level properties and further cue properties: no protocol command sets them by OSC yet.
13. **The update messages are our reading of QLab 5's scheme** (`/update/workspace/{id}`, `.../cue_id/{cue_id}`, `.../cueList/{list_id}/playbackPosition {cue_id}`). To confirm: the exact addresses and capitalisation, whether QLab sends the cue update for every property change or also for running state, whether the playhead message carries the cue id (and what it carries past the end of the list), whether workspace-level updates are sent for added or removed cues, whether ids are upper-case, and whether QLab sends updates to a subscriber on the reply port or on the connection it came from (we use the reply port for UDP, the connection for TCP).
14. The TCP connection limit (16) and the 64 KiB frame limit are ours; QLab's are unknown.
