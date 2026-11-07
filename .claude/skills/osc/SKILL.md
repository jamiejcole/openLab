---
name: osc
description: Load when changing or reviewing OSC input or output: engine/crates/engine/src/osc/, net/osc.rs, net/osc_mirror.rs, addresses, patterns, replies, /connect passcodes and rights, updates, listener limits, Network cue sending (control/netout.rs, OSC override).
---

# OSC (NET-01, NET-02, NET-05, TRG-03, DOC-02, CT-30, PB-10)

The address space follows QLab 5's OSC dictionary so existing remotes work. Anything not listed here is not implemented. Socket-free layer: `engine/crates/engine/src/osc/` (decode, dispatch, patterns, sessions, replies). Listeners and update messages: `net/osc.rs`, `net/osc_mirror.rs`. If port 53000 is taken the engine logs a warning and runs without OSC (the WebSocket server starts normally).

## Must not break
- The control thread is never waited for or blocked by OSC. One OSC task owns all OSC state; sockets only pass bytes through bounded queues.
- OSC adds no engine behaviour: each accepted message becomes the same `command`/`transport` input a UI client sends (`docs/protocol.md`).
- Everything is bounded (datagrams, frames, matches, matching work, replies, inputs, peers, connections, per-IP bytes). Hostile or malformed input is dropped and counted, never reaches the engine, never panics or hangs. A limit applies on every path that creates the data.
- We must never become a reflection amplifier: a spoofable UDP source is held to a per-IP byte budget (see `listener-limits`).
- Rights are judged on address and argument count before any cue lookup, so a refusal reveals nothing about which cues or triggers exist.
- A GO must not fire the wrong show: a workspace name shared by two open docs is refused, never guessed.
- A wrong `/connect` never disconnects a connected peer or lowers its rights.
- Identical messages within 100 ms from one peer are dropped, `/go` included (a double-tapped GO fires once).
- Stale input is dropped, not replayed: input held more than `max_input_age` (2 s) is discarded so a GO mash during startup is not replayed.
- OSC answers from its own copy of each doc; it never asks the control thread a question per message.
- A Network cue is never broken for an unresolved host name or an unreachable destination (see `network-cue` reference).

## Transport
- UDP and TCP on port **53000**, IPv4, all interfaces (as QLab; set a passcode on a network you do not control).
- UDP replies and updates go to the peer's IP on port **53001** whatever port it sent from, unless it set another with `/udpReplyPort`. A datagram is never answered on its source port.
- TCP carries OSC 1.1 SLIP frames (END `0xC0` at both ends; `0xDB 0xDC` is a literal END, `0xDB 0xDD` a literal ESC). OSC 1.0 length-prefixed framing is not supported.
- A UDP peer is identified by its IP, a TCP peer by its connection.
- Bundle time tags are ignored: every message runs on arrival, in bundle order. Bundles are flattened (a bundle of two starts starts both).
- Without a passcode, OSC is for trusted networks only: any host can read, subscribe, and direct our replies at a forged source (bounded by the per-IP budget). The passcode travels in clear and is compared without early exit (time depends on the longer string).

## Which doc a message reaches
- Workspace addresses may be prefixed `/workspace/{id}`. `{id}` is an open doc's id in **upper case** as replies write it, or its name exactly as written. An id is tried first, then the one doc with that name. A name containing `/` cannot be addressed.
- A name shared by several open docs is refused with the same `error` as an unknown workspace, plus one show-log warning per peer and name (a Finder copy keeps its name).
- A message for no open workspace (other id, lower-case id, name in another case) gets one `error` with only `address` and `status` (counted `wrong_workspace`).
- **Un-prefixed workspace messages go to every open doc**, whichever window is front: `/go` fires every doc's GO. Each doc judges the message with its own passcode table, the peer's session in it, and its cues. A query gets one reply per doc, in open order, each with that doc's `workspace_id` and its address rewritten to `/workspace/{ID}/...`; a doc where it fails answers with that status.
- With no doc open, a workspace message gets the same one `error`; `/workspaces` answers `[]` and `/version` its version.
- Application-level addresses (`/version`, `/workspaces`, `/updates`, `/udpReplyPort`) are not available under the prefix (`error`).
- Matching is exact and case-sensitive: `/GO`, `/Cue/1/start`, `/cue/1/NAME`, `/cue/1/prewait` and a lower-case `/cue_id/` UUID are unknown addresses (`error`).
- Reply, input and matching-work caps hold for all docs of one datagram/read together; the fan-out never multiplies them.

## The OSC copy and the per-doc gate
- OSC keeps one workspace copy per open doc, built from a snapshot then `model_changed`, `runtime`, `document` and snapshot broadcasts, applied like a WebSocket client. Gap, failed apply, `document` with another path, or tap overflow means it asks for a new snapshot.
- A doc's copy is **not ready** while it has none yet or a snapshot it asked for is out; while the doc list is out, no copy is ready. Broadcasts for a not-ready doc wait, so no copy goes back in time.
- Input that may reach a not-ready doc waits too, so no `/cue/5` or trigger resolves against a copy known wrong. Input reaching only ready docs goes at once (a console on show A is not held while show B opens).
- Reach is read from the addresses before handling. Un-prefixed messages, `/version`, `/workspaces` and undecodable packets reach every doc. `/workspace/{id}` reaches docs whose copy or list entry matches, else every not-ready doc (its name may be unknown yet). A bundle or TCP read waits whole if any message in it must.
- Order is kept: input never overtakes earlier waiting input from the same peer, nor earlier waiting input reaching a doc it reaches. A TCP close waits behind its input. A TCP read from a peer that may ask in no ready doc waits undecoded so its frame limit is decided with the right copies.
- Held input is bounded (`MAX_HELD_INPUTS`, 16 MiB); past it input stays in the queues (datagrams beyond the UDP queue are dropped, `udp_dropped`).
- A lost `model_changed` is noticed only at the next broadcast; the copy can be stale until then. For this the engine refuses `go_from` for a cue no longer on that list (`not_in_list` warning) instead of firing the old playhead cue.

## Active cue list
- `/go` and `/playhead/next|previous` act on each doc's **active** list: the one whose playhead moved last (QLab: the list on display). It starts as the first list.
- Every playhead move the copy hears of updates it (a cross-list GoTo makes the target list active: assumed). The OSC task sets it at once, ahead of the broadcast, when it forwards a playhead-moving message, so a `/go` right after `/playhead/20` acts on the list holding cue 20. A copy replacement keeps it while the list still exists in the same workspace.
- Cue numbers are looked up in **all** lists at any depth (unique across the workspace).
- No separate selection exists: selected is the playhead (QLab's default lock). `/select/...` equals `/playhead/...`; `/cue/selected/...` equals `/cue/playhead/...`.

## Addresses
A message that succeeds gets **no reply** (actions, writes, `/updates`). A failure gets an `error` reply; an address nothing answers to (not a cue trigger) gets one too. Wrong argument type or count is an `error`; nothing goes to the engine. "The list" is the active list.

| Address | Arguments | Becomes |
|---|---|---|
| `/go` | none; a non-string argument is ignored | `transport go` on the list |
| `/go "3"` | string: a cue number | `transport go_from`: playhead moves to the cue, then GO. `error` and nothing sent if no such cue, empty string, or cue inside a group |
| `/stop` | none | `transport stop_all`: every cue and preview at once |
| `/hardStop` | none | `transport hard_stop_all`; the engine maps it to the same input as `stop_all` (stop-all has no fade-out). The variants stay apart so a future fading stop can differ |
| `/panic` | none | `transport panic`: all cues fade out over the panic duration, a second panic stops everything |
| `/pause`, `/resume` | none | `transport pause_all` / `resume_all` |
| `/reset` | none | `transport reset_all`: stop all, reset every cue, every list's playhead to its first cue (OSC dictionary pp. 409-412; all lists, pp. 97-98) |
| `/playhead/next`, `/playhead/previous` | none | `playhead_next` / `playhead_previous` on the list |
| `/playhead/{number}`, `/select/{number}` | none | `set_playhead_by_number` on the list holding the cue (it becomes active). `error` if no such cue or inside a group |
| `/select/next`, `/select/previous` | none | same as `/playhead/...` |
| `/cue/{number}/start` | none | `transport start` |
| `/cue/{number}/stop` | none | `transport stop` at once, ignoring the cue's own fade-out (QLab-confirmed) |
| `/cue/{number}/hardStop` | none | `transport hard_stop` |
| `/cue/{number}/panic` | none | `transport panic_cue`: the cue and its contents fade over the panic duration; nothing else |
| `/cue/{number}/pause`, `/resume` | none | `transport pause` / `resume` |
| `/cue/{number}/load` | optional seconds >= 0 | `transport load` (`at` 0 if omitted) |
| `/cue/{number}/reset`, `/preview` | none | `transport reset` / `preview` |
| `/cue/{number}/name`, `/notes` | one string | `command set_cue_property` |
| `/cue/{number}/number` | string, int or float; empty string clears | `command set_cue_numbers` |
| `/cue/{number}/preWait`, `/postWait` | one number, finite, >= 0 | `command set_cue_property` |
| `/cue/{number}/armed`, `/flagged` | int 0/1 or OSC boolean | `command set_cue_property` |
| `/cue/{number}/{property}` | none | reply with current value. Also readable, read-only: `uniqueID`, `isLoaded`, `isRunning`, `isPaused`, `isBroken` (a write is `error`) |
| `/cue_id/{ID}/...` | as `/cue/{number}/...` | cue found by id (UUID, upper case) |
| `/cue/selected/{property}`, `/cue/playhead/{property}` | as the property | cue at the active list's playhead. Reads only |
| `/cue/active/{property}` | as the property | every running cue. Reads only |
| any other address | none | if exactly some cue's OSC trigger (TRG-03): `transport start` for that cue; else `error` (`unknown_address`) |

Not implemented: level and time properties beyond pre-wait and post-wait, `/new`, `/save`, other QLab application and workspace queries.

### Patterns
The cue-number part of `/cue/{number}/{verb}` is a pattern when it has `*`, `?`, `[...]` or `{a,b}`: `*` any run (also none), `?` one char, `[abc]`/`[a-c]` one of, `[!a-c]` none of, `{a,b}` one alternative (not nested). It is matched against every cue number in every list and inside groups, in workspace order (list, then cues, a group before its children), and acts on or answers for **every match**.
- A read answers once per match, with the matched cue's own address in the JSON (`/workspace/{ID}/cue/3/name`, or `/cue_id/{ID}/name` for a numberless cue); the OSC reply address is `/reply/` plus the address asked. `*` also matches cue lists (empty number): a list answers `name` and `number` (`""`) at its `cue_id` address, and is skipped by other properties and by actions. A match lacking the property gets its own `error`.
- An action is sent per matched cue, in order. A write reaches every match. A wrong argument is an `error` per match.
- No match, or an invalid pattern (unclosed `[`/`{`, nested `{`, stray `]`/`}`, over 64 alternatives): one `error`.
- Pattern characters elsewhere are not expanded. The whole address may still equal a cue's trigger (looked up first), so triggers can contain them.
- Work is bounded: 1024 matches (`matches_capped`), 64 alternatives, 1M matching steps per read for all its messages, cue numbers over 256 characters never match by pattern.

## Connecting and the passcode (NET-02)
- The doc keeps a passcode table (`settings.osc_access`; stored shape in `docs/file-format.md`): rows of passcode + View/Edit/Control (Edit and Control need View) and a **No Passcode** row.
- A peer that has not connected has the No Passcode rights; `/connect` with a table passcode gives that row's rights.
- A new doc: No Passcode none, one random 4-digit passcode (1000-9999) with all rights.
- With no rows, any `/connect` argument is accepted and gives the No Passcode rights. Editing commands: `AddOscPasscode`, `RemoveOscPasscode`, `SetOscRights` (see skill `edit-commands`).
- `/connect` forms: `/connect`, `/workspace/{ID}/connect`. Reply `{"status":"ok","data":"ok:view|edit"}`: `ok:` plus the rights joined by `|` in order view, edit, control (`ok:` alone for none). A wrong passcode is `{"status":"ok","data":"badpass"}` (status `ok`). A lockout is `denied`; a full peer table is `error`.
- **Send the passcode as a string.** An int/long argument never matches (leading zeros are ambiguous) and counts as wrong. Another type or several arguments are wrong when the table has rows.
- `/connect` with no argument connects with the No Passcode rights: stores nothing, takes no slot, counts no failure, is not a known address.
- Session, rights and known addresses are **per peer and per doc**; connecting to one doc leaves another locked. An un-prefixed `/connect` is answered by every doc against its own table. If some open doc accepts the passcode it is not a guess in the others (they answer `badpass` uncounted), so a console sending each doc's passcode un-prefixed cannot lock itself out. A passcode no doc accepts is one failure.
- A table edit ends sessions when a row was removed or changed or No Passcode changed; adding a row keeps them (adding a passcode mid-show must not disconnect consoles). It keeps known addresses.
- Sessions also end when the doc closes or its copy holds another workspace id (known addresses forgotten too), when a TCP connection closes, and for UDP after **60 s of silence** (any message to any doc renews all; as QLab). A silent UDP console must `/connect` again and meanwhile gets `denied`; a display that only listens must send something each minute or use TCP. A bare `/updates 1` after expiry resubscribes (unlike QLab).
- A connected UDP peer is only an IP, so any packet from that IP, forged included, can switch its `/updates` on or off; it cannot disconnect it.

## Rights
Every message except `/connect`, `/version`, `/workspaces` and `/updates` needs a right in the doc it reaches. A peer lacking it gets a `denied` reply over **both** UDP and TCP: `address`, `status`, and `workspace_id` (application messages such as `/udpReplyPort`: no `workspace_id`). A UDP `denied` to a peer without View is held to the strict byte budget and 1400 bytes.

| Right | Allows |
|---|---|
| View | every read (`/cue/{n}/{prop}` without argument, `selected|playhead|active`, `/cueLists`, `/runningCues`, `/runningOrPausedCues`, `/selectedCues`, `/playheadID`), writing `notes`, `/udpReplyPort`. A peer without View has the strict reply budget and the 4 KiB frame limit |
| Edit | writing `name`, `number`, `preWait`, `postWait`, `armed`, `flagged`; `/select/...`. (A read-only property write is refused as `error` once allowed) |
| Control | `/go`, `/stop`, `/hardStop`, `/panic`, `/pause`, `/resume`, `/reset`, `/playhead/...`, cue verbs (start, stop, hardStop, panic, pause, resume, load, reset, preview; by number, id or pattern), and a cue's trigger address |

- An address nothing answers to counts as a possible trigger, so it needs Control: a peer without Control cannot probe which triggers exist.
- A trigger shaped like a read (`/cue/zz*/name`) passes the read check, so Control is checked again once the trigger is found. A peer without Control gets the same `error` as for a missing cue (not `denied`, which would also hide real missing-cue errors from View peers) and nothing starts.
- Edit or Control without View are cleared on load (warning) and ignored by the access check, so a broken table never grants more than it says.
- `/updates` works at any level (no right, no reply). Update data is sent only to subscribers that have View **when it goes out**; a subscriber without View keeps its subscription and starts receiving once it gets View.
- A TCP peer's `/udpReplyPort` is ignored without a reply, whatever its rights.
- Only name, notes and pre-wait writes were measured in QLab; the rest of the table follows QLab's documented rights table by analogy (assumed).

## Lockout (differs from QLab on purpose)
- Wrong passcodes are counted per remote IP across all docs, and for TCP too (reconnecting does not reset). A correct passcode does not reset the count; failures age out. After **5** wrong from one IP it is locked out **30 s**: `/connect` is `denied` without checking the passcode.
- Globally, **50** wrong in **60 s** make every new `/connect` `denied` until the window ends (rotating forged sources gets no free guesses). Counts are shared by all docs, so trying docs in turn gives no fresh budget.
- A wrong `/connect` from a connected peer answers `badpass`, changes nothing, but **counts**; while its IP is locked out its `/connect` is `denied`, even with the right passcode. Without this a View peer could search the table for a Control row without limit.
- **Known addresses**: each doc remembers up to 64 IPs that completed `/connect`. A correct passcode from a known address passes the global budget (so forged wrong guesses cannot stop the operator's console reconnecting) but never that IP's own lockout; wrong guesses during a lockout still count and extend it. A wrong passcode from a known address is refused.
- Trade-off: a device that never connected can be blocked during an attack.
- A multi-address attacker (an IPv6 /64) is bounded only by the global budget, so the default 4-digit passcode falls in about 3 hours at most; use a longer passcode on an open network.
- Passcodes added by command are 1 to 32 ASCII digits, at most 16 rows. If the OS random source fails, the default passcode comes from the clock and a warning is logged.
- Recovery: wait 30 s without a wrong guess, use another IP, or stay connected (a connected peer keeps its rights through lockouts).

## Identical messages
A message with the same address and arguments as the peer's previous one, arriving less than **100 ms** after it, is dropped: no action, no reply (`/go` included; QLab-measured). Time is arrival, not handling time; frames from one TCP read share its arrival time. Only the last message per peer is remembered and a dropped repeat does not renew the time. Memory: one fingerprint for at most 1024 peers. A client polling the same query faster than every 100 ms sees some polls unanswered.

## Differences from QLab (deliberate; do not "fix")
- Lockout is our stricter shared guard; QLab backs off per TCP connection, unshared. Replies are QLab's (`denied`).
- Updates are sent once, not twice. A bare `/updates 1` resubscribes after lapse.
- A reply too large for a datagram becomes an `error` reply; QLab sends nothing.
- `/version` is openLab's version. Cue `type` names other than Audio, Group, Cue List are ours (not measured; assumed).
- Cue lists answer patterns only for `name`, `number`, `uniqueID`.
- No `/new`, `/save` or other application/workspace queries.

## References
- `references/updates-and-queries.md`: read when touching `/updates`, `/udpReplyPort`, update messages, reply JSON, queries or cue objects.
- `references/listener-limits.md`: read when touching sockets, the task, TCP/UDP caps, byte budgets, held input, counters, or reviewing OSC for flood, reflection or hang risk.
- `references/network-cue.md`: read when touching Network cue sending, `control/netout.rs`, network patches, look-ups or the output override.
- Skill `engine-ws-server` for the hub tap the OSC copy is fed from; `docs/protocol.md` for the WebSocket messages OSC reuses.
