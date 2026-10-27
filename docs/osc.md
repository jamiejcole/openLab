# OSC API (NET-01, NET-02, NET-05, TRG-03, DOC-02)

Status: the **socket-free layer is implemented** (`engine/crates/engine/src/osc/`): decoding, address dispatch, passcode and updates state, replies. **The UDP and TCP listeners are not yet running (M4.5a)**, so nothing answers on port 53000 today, and update messages (NET-05) are not yet sent. This file lists exactly what the layer does; anything not listed is not implemented.

The address space follows QLab 5's OSC dictionary, so existing remotes keep working where openLab has the feature. Where we were unsure what QLab does, we chose the more conservative behaviour and list it under "Open questions".

## Transport (planned listeners, M4.5a)

- UDP and TCP on port **53000**. Replies and updates to a UDP peer go to its IP address on port **53001**.
- TCP carries OSC 1.1 SLIP frames (END `0xC0` at both ends; `0xDB 0xDC` is a literal END, `0xDB 0xDD` a literal ESC). The length-prefixed framing of OSC 1.0 is not supported.
- A UDP peer is identified by its IP address, a TCP peer by its connection.

## Limits

| Limit | Value | When exceeded |
|---|---|---|
| Datagram or SLIP frame | 64 KiB | datagram: dropped, counted as malformed; stream: the frame is skipped to its next END without being buffered, counted as `oversize_frames` |
| Bundle nesting | 4 levels (the outermost counts as 1) | whole packet dropped, malformed |
| Messages in one packet | 128 | whole packet dropped, malformed |
| Array nesting in type tags | 4 levels | message dropped, malformed |
| Address | 256 bytes, 8 `/`-separated parts, no empty part (one trailing `/` is allowed) | dropped, counted as `bad_address` |
| Peers with state | 1024 | the longest-silent UDP peer is dropped to make room; if only TCP peers are left, the newcomer's `/connect` or `/updates` gets an error reply |

Bundle time tags are ignored: every message runs when it arrives, in bundle order. Bundles are flattened.

Malformed or oversize input never reaches the engine. It is dropped and counted (`OscStats`: `packets`, `messages`, `malformed`, `oversize_frames`, `bad_address`, `unknown_address`, `wrong_workspace`, `denied`, `rejected`).

## Addresses

Every address may be prefixed with `/workspace/{id}`, where `{id}` is the open workspace's id (compared case-insensitively). A message for another id is ignored, with no reply (counted as `wrong_workspace`). Address matching is exact and case-sensitive. **Wildcards and alternatives (`* ? [ ] { }`) are not expanded**; such an address gets an error reply.

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
- A wrong passcode on a peer that was connected disconnects it. After 5 wrong passcodes in a row a peer is locked out for 30 s: `/connect` during the lockout is answered `denied` without checking the passcode.
- A UDP peer's connection and update subscription end after 10 minutes without any packet from it; a TCP peer's end when the connection closes. The runtime clears all of them when the passcode changes or another workspace opens.
- The passcode is compared without stopping at the first differing byte. It travels in clear text: this is access control for a trusted show network, not security against an attacker on the wire.
- Permission levels (view, edit, control) are not implemented: a connected peer may do everything the API offers.

## Updates (NET-05)

`/updates 1` (int or boolean true) subscribes the peer, `/updates 0` unsubscribes; it needs `/connect` first when a passcode is set. Anything else gets an error reply. **Sending the updates is part of M4.5a**; today only the subscription is recorded.

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
13. The `/updates` message formats (what is sent to subscribers) are decided in M4.5a.
