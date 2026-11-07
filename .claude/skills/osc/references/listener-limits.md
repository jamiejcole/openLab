# OSC listener limits and hardening

Constants live in code (`osc/codec.rs`, `osc/dispatch.rs`, `osc/session.rs`, `osc/mod.rs`, `net/osc.rs` `OscConfig`); this file gives the design reasons and the bound each protects. Counters are in `OscStats` and `WsServer::osc_stats()`; malformed, oversize, bad-address, throttled, capped and dropped input is logged as one warning with counts every 10 s while it happens.

## Task design
- Both sockets run on the network runtime (no extra threads). Sockets pass bytes to the one OSC task through bounded queues, so a slow or hostile peer never delays another.
- The task serves **UDP datagrams first** (at most 32 in a row, then the queues at random), so a TCP flood cannot starve a console's datagrams.
- The UDP socket asks for 1 MiB buffers (macOS refuses a datagram larger than its send buffer, 9216 by default, so large replies would be lost; a sender needs a raised `net.inet.udp.maxdgram` too). The kernel buffer in front of the socket is finite: a burst of a few hundred datagrams faster than it is read overflows it silently and uncounted.
- Requests to the engine use ids from 2^63 (never `u64::MAX`, the WebSocket server's own). At most `max_inflight` (2048, above the 1024 inputs one datagram can make) may be unanswered; beyond that inputs are dropped (`inputs_dropped`). An engine `error` for a request is counted (`control_refused`); the OSC peer gets no reply.
- Replies: UDP to IP:53001 (or `/udpReplyPort`); TCP SLIP-framed on the same connection, one queue entry per read. Sending never waits; a full buffer or queue loses the message (`send_failed`). A TCP peer whose write makes no progress for 10 s is disconnected. Queued unwritten replies: 4 MiB per connection, 64 MiB total.

## Input limits
| Limit | Value | When exceeded |
|---|---|---|
| Datagram | 64 KiB | dropped, malformed |
| SLIP frame | 1 MiB from a peer that may ask; 4 KiB from one that may not yet (passcode set, not connected) | skipped to the next END unbuffered, `oversize_frames` |
| Bundle nesting / messages per packet / array nesting | 4 / 128 / 4 | packet or message dropped, malformed |
| Address | 256 bytes, 8 parts, no empty part (one trailing `/` ok) | `bad_address` |
| Replies per read, all docs | 1024 | rest dropped, `replies_dropped` |
| Reply bytes per read, all docs | 2 MiB | rest dropped, `replies_dropped` |
| Inputs per read, all docs | 1024 | rest not produced, `inputs_capped` |
| Cues one pattern or `/cue/active` acts on, per doc | 1024 | rest unused, `matches_capped` |
| Pattern alternatives | 64 | `error` reply |
| Matching work per read, all messages and docs | 1,000,000 steps (about 5-10 ms) | rest of the read's pattern messages get `error`, `matches_capped` |
| Reply body to a peer that may ask | 65507 bytes UDP; 1 MiB TCP | replaced by an `error` with no data |
| Reply body to a peer that may not ask yet | 1400 bytes | replaced by an `error` with no data |
| Peers with a session | 1024 | newcomers refused (`error` on `/connect`, `/updates`) until UDP peers expire or TCP closes; existing peers never evicted, so forged sources cannot push out a real console |
| Rightless sessions (bare `/updates` or `/udpReplyPort`, strangers cannot view) | 64 | the silent-longest is evicted; a session with rights never is |
| Datagrams waiting | 256 | dropped, `udp_dropped` |
| Input age at handling | 2 s | dropped, `inputs_dropped` |
| Cues reported singly after one change | 32 | one `__root__` instead |
| Group nesting in `/cueLists` | 16 | deeper groups written without children |

Matching work is bounded so a hostile bundle of patterns cannot hold the OSC task much past ten milliseconds and a GO behind it is not held up. Each alternative is charged for its tokens even against an empty number; runs of `*` count once.

## Why 1024 replies per read, and the reflection budget
- A wildcard answers once per matched cue (as QLab), so a show of several hundred cues must answer `/cue/*/name` fully from one datagram; 1024 equals the per-message match cap. A reply is about 250 bytes unless notes or `/cueLists` are in it, so the 2 MiB byte cap bounds the odd case.
- A spoofable UDP source must not make us an amplifier (without budgets one 40-byte datagram could draw 2 MiB). Per **source IP**, whatever doc: replies **and updates** of a UDP source that has not connected with a passcode are budgeted.
  - Passcode set: 16 KiB burst, 8 KiB/s (it is only sent small refusals). Dropped replies: `replies_throttled`; updates: `updates_dropped`.
  - No passcode: 256 KiB burst (four full datagrams), 64 KiB/s, so a 60 KB notes read works. With no passcode `/connect` proves nothing about the source, so **every** UDP source stays budgeted, connected or not.
  - A UDP peer connected with a real passcode, and any TCP peer, has no budget. Accepted cost: forged traffic from a console's own address can make us send it replies up to the per-read caps.
- The kind of budget for a doc's reply follows that doc's table. An application reply (`/version`, `/workspaces`, unknown-workspace error, `/updates` and `/udpReplyPort` errors) is unbudgeted only for a peer connected with a passcode to some doc; otherwise it is held to the open budget if some open doc has no passcode, else the strict one.
- At most 1024 source IPs have a budget. A bucket is dropped only once refilled; a **depleted bucket is never evicted** (that would give a cycling forged address a fresh burst). When the table is full of depleted buckets a source with no bucket is sent nothing until one refills (2 s strict, 4 s open). A tracked console keeps its bucket.
- Rotating 1024 forged addresses can evict the oldest budget (bounded: at most 16 KiB per forged address). Without a passcode a forged `/connect` takes a session slot like `/updates 1`; slots free as UDP peers idle (60 s).

## TCP connections
- At most **256** (each costs a task, decoder and write queue; worst case 256 half-sent frames of 1 MiB). Beyond: the new connection is closed at accept, `tcp_refused`, one rate-limited warning per 10 s with a count.
- Keepalive on accepted sockets (30 s idle, 10 s probes, best effort) frees slots of peers that vanished without closing (a laptop off Wi-Fi), after about 2 minutes.
- Full table: the connection silent longest is closed for the newcomer if it has been silent `tcp_idle_evict` (60 s), is not subscribed, and (with a passcode set) has not completed `/connect`. The newcomer waits up to 0.5 s for the slot. Why: a stranger's idle connections must not push a console out; subscribed or connected consoles are never evicted this way, so with the table full of consoles the newcomer is refused.
- One host (IP; IPv4-mapped IPv6 counts as its IPv4) holds at most **16** (`max_tcp_connections_per_host`). At its cap its next connection may close **that host's own** longest-silent evictable connection (never another host's), waiting up to 0.5 s in a task of its own (one per host at a time; a second connection meanwhile is refused at once, so a host cannot slow the accept loop). Otherwise refused and logged with the limit and host. It takes 16 hosts to fill the table.
