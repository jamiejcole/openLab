# OSC updates, replies and queries

## `/udpReplyPort {port}`
One int, 1 to 65535, from a UDP peer: replies **and updates** to that peer go to its IP on `port` instead of 53001 (QLab-confirmed over UDP, manual pp. 111-113).
- Accepted: no reply. Applies to the sender's IP only.
- Lives in the UDP session: 60 s of silence forgets it (any message renews), as do a passcode change that ends the session and a different workspace opening. `/updates 0` keeps it. A new `/udpReplyPort` replaces it.
- Bad argument (none, several, not an int, 0, negative, over 65535): one `error` without `workspace_id`, counted `rejected`; the old port is kept.
- Takes a session slot like `/updates`: with the table full (1024) a newcomer gets `error` and nothing is set; a peer already holding a slot can still set it.
- Needs View in some open doc; with View nowhere (or no doc open) one `denied` without `workspace_id`. It is an application message: kept in the session of every doc where the peer has View, and given to a doc opened later where it has View while the peer's entry lives. Closing the doc it was sent to does not lose it. Reason: a peer that may not read must not redirect where we send.
- Not available under `/workspace/{id}` (`error`). TCP: ignored, no reply, no state.
- Byte budgets are per source IP and untouched: changing the port refills nothing.
- `/workspaces` still reports the default 53001 in `udpReplyPort`.

## `/updates`
- `/updates 1` (int or true) subscribes, `/updates 0` unsubscribes. Works at any access level; data goes only to subscribers with View. It is an application message sent to **every open doc**, and to every doc opened later while the peer's entry lives; each doc sends its updates only to subscribers with View **in it**.
- Allowed: no reply. Bad argument, a full peer table in some doc, or no doc open: one `error` without `workspace_id`. `/workspace/{ID}/updates` is `error`.
- Subscriptions lapse after 60 s of UDP silence.

## Update messages
Sent to a subscriber's IP on 53001 (or its `/udpReplyPort`) for UDP, one message per datagram; SLIP-framed on its own connection for TCP and nowhere else. `{WS}`, `{ID}`, `{LIST}` are upper-case UUIDs; `__root__` is lower case. QLab sends each burst twice; we send once.

| Address | Argument | Sent when |
|---|---|---|
| `/update/workspace/{WS}/cue_id/{ID}` | none | the cue changed. Property edit of a cue that stays put: that cue only. State change (start, pause, resume, stop, finish, break): the cue, each enclosing group nearest first, its cue list (`{ID}` = list id), then `__root__`. Cue added, removed or moved: the cue, the groups and list it was in and is in, then `__root__` |
| `/update/workspace/{WS}/cue_id/__root__` | none | with state and shape changes; alone for a change to the cue lists themselves; instead of the per-cue messages when one change touched more than 32 cues (client asks again for everything) |
| `/update/workspace/{WS}/cueList/{LIST}/playbackPosition` | id of the cue under the playhead as a string; **no argument** when past the end or the list is empty | a playhead moved |
| `/update/workspace/{WS}/dashboard` | none | last in the change, when a playhead moved, a cue **started** (idle/loaded to running, pre-waiting or paused) or the tree changed shape. Not for pause, resume, finish |

- No workspace-level message: patch and settings edits send nothing. A copy rebuilt by a differing snapshot sends `__root__`, the dashboard and a `playbackPosition` per list whose playhead moved. Opening or closing a doc sends nothing (clients see `/workspaces`). Moving our active list without moving a playhead sends nothing.
- Order within one change: cue messages, `__root__`, playhead messages, dashboard. A message is built once and sent to every subscriber. Messages carry no data beyond this; the peer queries what it needs.
- A UDP subscriber shares its reply byte budget unless it connected with a passcode. A TCP peer is paced by its connection. A peer that fell behind loses updates and should re-query when it sees `__root__`; there is no catch-up.

## Replies
An OSC message to the sender at `/reply` + the address sent (prefix included) with one string argument of JSON:
`{"status":"ok","address":"/workspace/<ID>/cue/1/name","workspace_id":"<ID>","data":"Opening"}`
- `status` is `ok`, `error` or `denied`. `address` is always the full `/workspace/{ID}/...` form even when sent un-prefixed (exception: wildcard, `selected`, `playhead`, `active` answer with the matched cue's own address). `workspace_id` is on every workspace-level reply. `data` only when there is a value (errors and refusals have none).
- Application-level replies (`/version`, `/workspaces`, the `error` of `/updates`, the `denied` of `/udpReplyPort`) carry `address` as sent and no `workspace_id`. A reply for another workspace has only `address` and `status`.
- Replies go for: `/connect`, reads, queries, and anything failed or refused. Successful actions, writes and `/updates` get none. Key order is not fixed.

## Queries
| Address | Reply `data` |
|---|---|
| `/version` | the engine's version string (QLab answers its own) |
| `/workspaces` | `[{"version","displayName","udpReplyPort","uniqueID","port"}]`, one per open doc in open order (not front order), `[]` with none; ports are the bound ones. One reply, no `workspace_id`. A peer that may not view anywhere gets at most 1400 bytes; a longer list gets `error` |
| `/cue/selected/{prop}`, `/cue/playhead/{prop}` | property of the cue at the active list's playhead; `error` when none |
| `/cue/active/{prop}` | one reply per running cue; one `error` when none |
| `/runningCues` | array of cue objects: running (waiting, playing, fading, stopping; not paused, loaded or broken); `[]` when none |
| `/runningOrPausedCues` | running plus paused |
| `/selectedCues` | the playhead cue in an array; `[]` when none |
| `/cueLists` | cue-list objects (`"type":"Cue List"`, `"number":""`) each with `cues`, groups nested to 16 levels (deeper groups are written without children) |
| `/playheadID` | id of the cue at the active playhead, or `"none"` |
| `/playheadId` | same, replied at `/playheadID` with `"deprecated":true` and a `warning` |

Cue object keys (QLab's): `number`, `uniqueID`, `cues` (group children), `colorName/live`, `flagged`, `listName`, `type`, `colorName`, `name`, `armed`. `listName` is the name; `colorName/live` the colour. `type` is `Audio`, `Group`, `Cue List`; names for other kinds (`Fade`, `Start`, `Stop`, `Pause`, `Load`, `Reset`, `Devamp`, `GoTo`, `Arm`, `Disarm`, `Wait`, `Memo`, `Network`) are ours. State properties (`isRunning`...) use the same states as the queries.
