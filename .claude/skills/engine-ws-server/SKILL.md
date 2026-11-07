---
name: engine-ws-server
description: "Load when changing or reviewing the engine's WebSocket server: engine/crates/engine/src/net/ (ws.rs, hub.rs, token.rs, refusals.rs), handshake, hub fan-out, slow clients, snapshot and ordering rules. OSC is skill osc; app relaunch is skill engine-lifecycle; schemas: docs/protocol.md."
---

# Engine WebSocket server (`net/`)

Serves the UI protocol (`docs/protocol.md`). Code: `ws.rs` (accept loop, connections), `hub.rs` (fan-out, tap), `token.rs`, `refusals.rs` (rate-limited refusal logging). The OSC listeners share this runtime (skill `osc`); app-connection relaunch is `relaunch.rs`/`stopping.rs` (skill `engine-lifecycle`). Tunables are `ServerConfig::default()` in `ws.rs`; tests shorten them, so test timeouts are hang guards only.

## Must not break
- The control thread and audio thread are never blocked by the network: the control side only `send`s or `try_send`s on the reply and outbound channels, which wake net tasks directly.
- A slow, stuck or hostile client costs only itself: bounded queue per client, fan-out never waits, a stalled write times out.
- A client never sees state go back in time: a snapshot is never overtaken by a later broadcast; a stale snapshot is followed by a fresh one, never dropped.
- A lagged client is resynced, not closed.
- A pre-auth peer cannot make the engine buffer large data (16 KiB cap before `hello`).
- A frame that cannot be read does not close the connection (32 in a row do).
- Every refusal is counted and logged at a bounded rate, so a UI that cannot connect can be diagnosed.

## Shape
- A tokio runtime of **2** worker threads (`openlab-net`) runs the accept loop, one task per connection and the **fan-out task**.
- Listens on `127.0.0.1` only, on an OS-chosen port. `host::run` writes `WsServer::port()` and `token()` into `engine.json` (skill `engine-lifecycle`).
- Token: 32 bytes from the OS CSPRNG, 64 hex characters, fresh each start, compared in constant time (walks the longer input, no early exit), never in `Debug` output.
- Limits: **32** authenticated connections (a window plus a spare per open doc, at most 8 docs, plus app connections) and **8** still upgrading or waiting for `hello`; extras are closed at accept. Up to **64** requests in flight per connection (until their `result` is written): beyond that the server stops reading the socket so TCP holds a flood back instead of growing the inbox.
- Refusals are logged (`warn`, via `Inbox::Log`) at most once per `refusal_log_interval` (10 s) with a count: the first at once, later ones when the interval passes (by timer, even if no connection follows).

## Connection
1. Upgrade and `hello` within `hello_timeout` (5 s) **in total**. Before `hello` is accepted the socket delivers at most 16 KiB (upgrade plus the 4 KiB hello); the WebSocket library would otherwise buffer a 1 MiB message before anyone looks at it.
2. Refusal = an `error` message, then close. Codes and frame limits are in `docs/protocol.md`. A hello over 16 KiB, a timeout or a closed socket just closes.
3. Success: the connection is registered with the fan-out, bound to the doc its `hello` names or as an app connection. The server asks the control loop for a snapshot with its own request (id `u64::MAX`, reserved: a client using it gets `bad_request`; its `result` is swallowed; first snapshot waits at most `SNAPSHOT_WAIT`, 10 s) and sends `welcome { doc, snapshot }`; an app connection gets `welcome { workspaces }`. A doc that is not open answers `no_workspace`, sent as `error` before closing.
4. Broadcasts made meanwhile are queued and filtered: `model_changed` at or below the snapshot revision and `runtime` at or below its `runtime_seq` are dropped. `document` and `warnings` queued from before the snapshot may arrive after it; the last one equals the snapshot's state, so the client ends right.
5. After `welcome`: client frames are decoded and `command`/`transport`/`undo`/`redo`/`request`/`close`/`panic`/`hotkey` go to the control inbox as `Inbox::Conn { doc, .. }` (`doc` is `None` for an app connection) with the connection's reply channel. `transport`, `panic` and `hotkey` are first announced as `Inbox::Trigger` (`ui`) for the show log.
6. An unreadable frame gets `error` (`bad_request`, or `unknown_message` for an unknown `type`) and the connection stays up. 32 in a row (any good frame resets), an over-limit frame or a socket error close it. Binary frames and a second `hello` are `bad_request`.
7. `goodbye` closes the connection. With `shutdown: true` it is also forwarded to the control inbox, which stops the show and ends the loop. The server never exits the process; shutdown order is skill `engine-lifecycle`.

## Ordering and gaps
- Order is **per connection**: broadcasts are written before replies. A `result` carrying `ok.revision = N` is **held** until the `model_changed` for N (or a snapshot at N or later) is written; the control loop puts that broadcast on the outbound channel first. Reading and serving other replies continue meanwhile (a GO is never delayed by a held result), so results may leave in a different order than they arrived; clients match by `id`.
- If that `model_changed` never arrives (outbound channel full and it was dropped), the result goes out after `revision_wait` (2 s). The client then sees `result.revision` ahead of its model and requests a `snapshot`.
- **Snapshots never go back in time.** While a snapshot-answering request is out (the server's own, or the client's `snapshot` until its `result`) the connection forwards no broadcasts; they wait in its queue and are filtered against the snapshot's revision and seq once written. Held results wait for the snapshot too (it covers them). The hold is bounded: after `snapshot_wait` (60 s) with no `result` the connection drops the hold and takes a fresh snapshot.
- Second line of defence: a snapshot behind what the client was already sent (revision or seq below the connection's: stale, or the numbering restarted) is followed at once by a fresh one (the connection marks itself lagged). It must not be dropped: after a numbering restart the fresh one would be behind too and never get through.
- A `snapshot` message resets the connection's filter to its revision and seq. Gaps in revision/seq of broadcasts are **forwarded as they are**; clients recover with `request: snapshot`.
- Broadcasts are tagged by doc (`Outgoing { doc, message }`). The hub offers a doc's broadcasts to connections bound to it, the engine's own (`log`, `log_entry`, `status`) to every connection, and `workspaces` to app connections only.
- `revision` and `runtime_seq` are per doc, so one filter sees one numbering. Opening a doc sends no snapshot to anyone (a bound connection takes it in `welcome`). A doc's `snapshot` broadcast goes out only when its model changed in a way no `model_changed` says (e.g. save-as re-spelled media references); a connection takes it unless it already holds that revision and seq.
- **Closing a doc closes its connections.** `closed { doc }` reaches each bound connection: it writes its held results and replies already in its reply channel (the control loop answers `close` before broadcasting `closed`, so the requester has its `result`), then `closed`, then ends. A connection that lagged meanwhile (its queue, with `closed`, thrown away) learns it from resync: the snapshot request of a closed doc answers `no_workspace`, and it writes `closed` and ends.

## Slow clients
Each client has a bounded queue (`CLIENT_QUEUE`, 1024). The fan-out task encodes a message once, outside the lock on the client list, and `try_send`s to every queue; it never waits for a client.
- `meters`, `progress`, `status`, `log_entry`, `log` are dropped for a client whose queue is half full or more, without marking it lagged (a `log` notice is also in the show log, which `log_tail` still has).
- A state message (`model_changed`, `runtime`, `document`, `warnings`) that does not fit, cannot be encoded, or exceeds `MAX_SERVER_FRAME_BYTES` (32 MiB) marks the client **lagged**. A lagged client is **not closed**: its queue is thrown away, broadcasts for it are skipped, and as soon as its task runs and no other snapshot is out the server asks for a fresh `snapshot` (broadcasts queued again before the request and filtered against its revision).
- A client whose snapshot itself exceeds the cap gets `error internal` and is closed. A `result` without a revision may overtake the snapshot during a resync.
- A client that stops reading stalls its TCP write; one write with no progress for `write_timeout` (10 s) closes the connection. Meanwhile it costs the control loop and other clients nothing.
- Dead peers: `ping` every `ping_interval` (10 s); a connection silent (a `pong` counts) for `dead_after` (30 s) is closed.

## Tap for OSC
The fan-out task also offers every doc's `model_changed`, `runtime`, `document`, `snapshot` and `closed`, tagged by doc, plus the `workspaces` list, to the OSC task through a bounded **tap** (`hub::Tap`). A full tap marks the OSC task lagged; it then rebuilds every copy as a lagged client would. A bind failure for the OSC ports leaves OSC off and the WebSocket server starting normally.

## Client contract (lives in `docs/protocol.md`)
The client-visible rules above (held `result`, `result.revision` ahead of the model means request a `snapshot`, never pipeline snapshot requests, results may reorder, gaps are forwarded, reserved id) are client obligations; if you change one, change `docs/protocol.md` and the Swift client (`EngineClient`) in the same commit.
