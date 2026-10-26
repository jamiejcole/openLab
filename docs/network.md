# openLab network layer (`openlab-engine::net`)

The WebSocket server for the UI protocol ([protocol.md](protocol.md)); ARCHITECTURE §3.1 and §6. Code: `engine/crates/engine/src/net/` (`ws.rs` server and connections, `hub.rs` fan-out, `token.rs`). OSC is M4.5 and will live beside it.

## Shape

- **Threads**: a tokio runtime with 2 worker threads (`openlab-net`), the **fan-out thread** (`openlab-net-fanout`, std) and one tiny **reply thread** per connection (`openlab-net-reply`: crossbeam cannot wake a tokio task, so it moves replies from the control loop's reply channel into the connection's task). None of them is the control or audio thread, and nothing here blocks them: the control loop only does `try_send` on its inbox/outbound channels.
- **Listening**: `127.0.0.1` only, an OS-chosen port. `WsServer::port()` and `token()` are what M4.9 writes to `engine.json` (this task does not write the file). `host::run_show` starts the server; nothing publishes the port yet, so no client can connect until M4.9.
- **Token**: 32 bytes from the OS CSPRNG (`getrandom`, already in the dependency tree through `tempfile`/`uuid`; ARCHITECTURE §12), 64 hex characters, fresh per start, compared in constant time (`constant_time_eq`, no early exit, walks the longer input). Never in `Debug` output.
- **Limits**: 16 authenticated connections, 8 still upgrading or waiting for `hello` (more are closed at accept). Up to 64 requests in flight per connection: beyond that the server stops reading the socket, so TCP holds a flood back instead of the inbox growing.

## Connection

1. **Upgrade and `hello` within `hello_timeout` (5 s) in total.** Before `hello` is accepted the socket delivers at most 16 KiB (the upgrade request plus the 4 KiB hello); the WebSocket library would otherwise buffer a whole 1 MiB message before anyone looks at it. The first frame is decoded with `decode_hello` (4 KiB).
2. Refusals are an `error` message, then close: wrong token or any other message first -> `bad_token`; version differs -> `protocol_version_mismatch` (token is checked first); unreadable or oversize hello -> `bad_request`. A hello over 16 KiB, a timeout or a closed socket just closes.
3. On success the connection is registered with the fan-out, then the server asks the control loop for a snapshot with a request of its own (id `u64::MAX`, **reserved**: a client using it gets `bad_request`; its `result` is swallowed) and answers `welcome { snapshot }`. Broadcasts made in between are queued and filtered: a `model_changed` at or below the snapshot's revision and a `runtime` at or below its `runtime_seq` are not sent. `document`/`warnings` queued from before the snapshot may arrive after it; the last one before the snapshot equals the snapshot's own state, so the client ends up right.
4. After `welcome`: client frames are decoded with `decode_client` (1 MiB; the WebSocket layer enforces the same limit while reading) and `command`/`transport`/`undo`/`redo`/`request` go to the control inbox with the connection's reply channel. An unreadable frame gets `error` (`bad_request`, or `unknown_message` for an unknown `type`) and the connection stays up; 32 in a row (any good frame resets the count), a frame over the limit, or a socket error close it. Binary frames are `bad_request`. A second `hello` is `bad_request`.
5. `goodbye`: the connection closes. With `shutdown: true` the message is also forwarded to the control inbox, which stops the show and ends the loop (docs/control.md). The server does not exit the process: `host::run_show` returns when the control loop ends (the process exit policy, relaunch and `engine.json` cleanup are M4.9).

## Ordering and gaps

- The server keeps a **per-connection** order: broadcasts are written before replies, and a `result` carrying `ok.revision = N` waits until the `model_changed` for N has been written (it is in the connection's queue or about to be: the control loop puts it on the outbound channel first). If it never arrives (the control loop's outbound channel was full and dropped it, docs/control.md "Outbound") the result goes out after `revision_wait` (2 s); the client sees `result.revision` ahead of its own model and requests a `snapshot`.
- A `snapshot` message (reply to a `snapshot`/`open`/`new`/`revert` request) resets the connection's filter to its revision and seq. Revision/seq gaps in broadcasts are **forwarded as they are**; clients recover with `request: snapshot` (protocol section 2).

## Slow clients

Each client has its own bounded queue (`CLIENT_QUEUE`, 1024 messages). The fan-out thread encodes a message once and `try_send`s it to every queue; it never waits.

- `meters` and `progress`: dropped for a client whose queue is more than half full.
- A state message (`model_changed`, `runtime`, `document`, `warnings`, `log`) that does not fit marks the client **lagged**; the same happens when a message cannot be encoded or exceeds `MAX_SERVER_FRAME_BYTES` (32 MiB). A lagged client is **not closed**: its queue is thrown away, broadcasts for it are skipped, and the server sends it a fresh `snapshot` (asked from the control loop like the first one) as soon as its connection task runs; later broadcasts are queued again before the snapshot is requested and filtered against its revision. A client whose snapshot itself exceeds the cap gets `error internal` and is closed. A `result` may overtake the snapshot during such a resync.
- A client that stops reading entirely stalls its TCP write; a single write that makes no progress for `write_timeout` (10 s) closes the connection. Meanwhile it costs the control loop and the other clients nothing (tested with 1500 x 16 KiB messages).
- Dead peers: a `ping` every `ping_interval` (10 s); a connection that has sent nothing (a `pong` counts) for `dead_after` (30 s) is closed.

## Tests

`tests/net_ws.rs`: real sockets against a stand-in control thread (auth success, wrong token, wrong version, other message first, hello timeout, 4 KiB and pre-auth caps, handshake limit, result ordering over 200 commands, lost `model_changed`, two clients, welcome filtering, gap and snapshot recovery, a 2,000-cue group upsert, over-cap message resync, stalled client, malformed and abusive frames, goodbye, keepalive, dead client, stop). `tests/net_control.rs`: the same server in front of the real `runner::run` loop. Timeouts in the tests are hang guards; the server's own timers are shortened through `ServerConfig`.
