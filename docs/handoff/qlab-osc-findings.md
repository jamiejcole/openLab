# QLab 5 OSC behaviour — captured findings

Captured on 2026-10-27 against **QLab 5.5.3** (reported by `/version`) on this Mac (macOS, Darwin 25.6), talking to `127.0.0.1:53000`. Everything below was observed in a QLab workspace created for this test; no QLab application preferences were changed. Workspace settings that were changed are listed under "Setup".

## How to read the evidence

- Lines are produced by a Python 3 stdlib-only probe (`osc.py`, kept in the shared session notes folder together with the full raw log `qlab-osc-log.jsonl`). Format: `time  send|recv  transport  address  typetags  args`.
- `send udp` = sent from one bound UDP socket (an ephemeral port). Replies were listened for on that **same sending port** (`udp@sendport`) and on a socket bound to **UDP 53001** (`udp@53001`). `from=` is QLab's source address of each datagram.
- `tcp0`…`tcp9` are separate TCP connections to port 53000 using SLIP double-END framing (RFC 1055). Their `recv` lines are frames read back on the same connection.
- `# time …` lines are my own markers written into the log just before an action; they say what I did (UI actions are described there).
- JSON reply bodies are reproduced exactly as received (QLab escapes `/` as `\/`). Only the strings *we* sent of 10 KB or more are abbreviated (`<str len=…>`), and in Q12 the long `x` runs in replies are abbreviated; their lengths are given.

## Setup

- New workspace, saved as `~/Documents/openlab-osc-probe/openlab-osc-probe.qlab5`. **Workspace unique ID: `243ACE2D-31F6-4615-87B6-BC26ADAA7BF6`.** Only this workspace was open during the tests.
- Main Cue List: Audio cues 1–5 (names `Audio1`…`Audio5`), Group cue 10 (`Group10`) containing Audio cues 6 and 7; a second cue list `Cue List` holding Audio cue 20 (`List2Audio20`). Every audio cue targets a 10-second silent WAV, so cues really run for 10 s without making sound. Cue numbers in QLab are unique across the whole workspace, which is why the second list's cue is 20, not 1.
- Workspace settings changed: OSC Access rows (see the phases below; the saved workspace ends with passcodes `1234` = all rights, `2677` = view, `6510` = view+edit, `2375` = view+control, and No Passcode = none), and "Lock playhead to selection" was switched off for part of Q6 and back on afterwards.
- Later UI edits during Q11 renamed cue 1 to `RenamedOne` and cue 2 to `RenamedTwo`, and reordered the main list; the final order is 1, 2, 4, 3, 10, 5.
- UUIDs of interest: Main Cue List `9AA46B16-4874-42E9-8E1E-F62CBB8CA139`, second list `EB163B29-7249-4603-8D1D-03FCE040A292`, cue 1 `12533BF2-…`, cue 2 `A285E785-…`, cue 3 `3E63219F-…`, cue 4 `C2A8C4FA-…`, cue 5 `BE801827-…`, group 10 `D18C05C9-…`, cue 6 `4C601614-…`, cue 7 `A253A8BE-…`, cue 20 `F90D7E43-…`.

### Important default discovered during setup: a new workspace denies everything without a passcode

QLab 5's per-workspace **OSC Access** settings (File › Workspace Settings › Network › OSC Access) hold a table of passcodes, each with **View / Edit / Control** checkboxes, plus a "No Passcode" row. A brand-new workspace came with "No Passcode" having *no* permissions and one auto-generated passcode (`6997` here) with all three. So out of the box, every workspace-level message without `/connect` is `denied`; only application-level queries (`/version`, `/workspaces`) answer. The same page shows the listening port (53000, TCP and UDP), a note that UDP replies go to port 53001 unless a client asks for another with `/udpReplyPort`, and a separate plain-text UDP port (53535).

```
# 16:36:37.990 DEFAULT-ACCESS: NoPasscode=none, passcode 6997=V/E/C
16:36:37.990 send udp /cue/1/name ,
16:36:37.990 send udp /go ,
16:36:37.990 send udp /connect ,
16:36:37.990 send tcp0 /cue/1/name ,
16:36:37.990 send tcp0 /go ,
16:36:37.990 send tcp0 /version ,
16:36:37.990 send tcp0 /workspaces ,
16:36:37.990 send tcp0 /connect ,
16:36:37.991 recv udp@53001 from=127.0.0.1:51588 /reply/cue/1/name ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:36:37.991 recv udp@53001 from=127.0.0.1:51373 /reply/go ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/go","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:36:37.991 recv udp@53001 from=127.0.0.1:50512 /reply/connect ,s {"status":"ok","data":"ok:","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
16:36:37.992 recv tcp0 /reply/cue/1/name ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:36:37.992 recv tcp0 /reply/go ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/go","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:36:37.992 recv tcp0 /reply/version ,s {"address":"\/version","status":"ok","data":"5.5.3"}
16:36:37.992 recv tcp0 /reply/workspaces ,s {"address":"\/workspaces","status":"ok","data":[{"version":"5.5.3","displayName":"Untitled Workspace","udpReplyPort":53001,"uniqueID":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","port":53000}]}
16:36:37.992 recv tcp0 /reply/connect ,s {"status":"ok","data":"ok:","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
16:36:44.481 send udp /connect ,s 0000
16:36:44.481 send udp /connect ,s 6997
16:36:44.481 send udp /cue/1/name ,
16:36:44.482 recv udp@53001 from=127.0.0.1:49472 /reply/connect ,s {"status":"ok","data":"badpass","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
…
```

So the test phases were:

1. **Phase A (default):** No Passcode = none; passcode 6997 = view/edit/control. (Above; setup was done after `/connect "6997"`.)
2. **"No passcode" phase (Q1, Q2, Q4–Q8, Q11–Q14):** passcode rows deleted, No Passcode = view + edit + control.
3. **Passcode phase (Q3, Q9):** passcode `1234` = view/edit/control, No Passcode = none.
4. **View-only (Q10):** No Passcode = view only (passcode 6997 still present with full rights).

---

## 1. Reply format

**CONFIRMED.** Every reply is an OSC message to `/reply` + *the address exactly as sent*, with one string argument holding JSON. Keys: `status` always; `address` always (for workspace messages it is the full `/workspace/{id}/…` form, even if the request had no prefix); `workspace_id` on every workspace-level reply (absent on `/version`, `/workspaces`, `/updates` and on wrong-workspace errors); `data` only when there is a value. `status` values seen: `ok`, `error`, `denied` (nothing else). `/go` that succeeds gives **no reply**. Key order varies between replies. Replies to UDP arrive on port **53001**, from a *different, changing* QLab source port each time (never from 53000), and never on the sender's own port.

Observed key sets (across the whole log): `{address,status,data}` for `/version` and `/workspaces`; `{status,data,workspace_id,address}` for successful reads and `/connect`; `{address,status,workspace_id}` for `error`/`denied` workspace replies; `{address,status}` for `/updates` denials and wrong-workspace errors; `/playheadId` additionally carries `deprecated: true` and a `warning` string.

UDP, never connected, No Passcode = full rights:
```
# 16:40:30.729 PHASE-B NoPasscode=V/E/C, no passcodes; fresh daemon, never connected
16:40:30.729 send udp /version ,
16:40:30.729 send udp /workspaces ,
16:40:30.729 send udp /cue/1/name ,
16:40:30.729 send udp /go ,
16:40:30.729 send udp /cue/1/start ,
16:40:30.729 send udp /cue/999/start ,
16:40:30.729 send udp /cue/999/name ,
16:40:30.729 send udp /cue/1/name ,s Audio1
16:40:30.729 send udp /bogus/address ,
16:40:30.730 recv udp@53001 from=127.0.0.1:52654 /reply/version ,s {"address":"\/version","status":"ok","data":"5.5.3"}
16:40:30.730 recv udp@53001 from=127.0.0.1:54827 /reply/workspaces ,s {"address":"\/workspaces","status":"ok","data":[{"version":"5.5.3","displayName":"openlab-osc-probe","udpReplyPort":53001,"uniqueID":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","port":53000}]}
16:40:30.731 recv udp@53001 from=127.0.0.1:64381 /reply/cue/1/name ,s {"status":"ok","data":"Audio1","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
16:40:30.739 recv udp@53001 from=127.0.0.1:58929 /reply/cue/999/start ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/999\/start","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:40:30.740 recv udp@53001 from=127.0.0.1:64812 /reply/cue/999/name ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/999\/name","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:40:30.740 recv udp@53001 from=127.0.0.1:49238 /reply/bogus/address ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/bogus\/address","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:40:30.790 recv udp@53001 from=127.0.0.1:64852 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
…
```

The same over TCP (replies come back on the same connection):
```
16:40:32.779 send tcp0 /version ,
16:40:32.779 send tcp0 /workspaces ,
16:40:32.779 send tcp0 /cue/1/name ,
16:40:32.779 send tcp0 /go ,
16:40:32.779 send tcp0 /cue/1/start ,
16:40:32.779 send tcp0 /cue/999/start ,
16:40:32.779 send tcp0 /cue/999/name ,
16:40:32.779 send tcp0 /cue/1/name ,s Audio1
16:40:32.779 send tcp0 /bogus/address ,
16:40:32.786 recv tcp0 /reply/version ,s {"address":"\/version","status":"ok","data":"5.5.3"}
16:40:32.786 recv tcp0 /reply/workspaces ,s {"address":"\/workspaces","status":"ok","data":[{"version":"5.5.3","displayName":"openlab-osc-probe","udpReplyPort":53001,"uniqueID":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","port":53000}]}
16:40:32.787 recv tcp0 /reply/cue/1/name ,s {"status":"ok","data":"Audio1","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
16:40:32.787 recv tcp0 /reply/cue/999/start ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/999\/start","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:40:32.787 recv tcp0 /reply/cue/999/name ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/999\/name","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:40:32.788 recv tcp0 /reply/bogus/address ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/bogus\/address","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
```

Notes:
- `workspace_id` in the JSON is the uppercase UUID. The OSC reply *path* echoes what was sent (`/reply/cue/1/name`), while the JSON `address` is normalised to `/workspace/{id}/cue/1/name`. One exception seen: `/playhead/2` replied with JSON address `/workspace/{id}/playhead` (see Q10 evidence).
- `/connect` replies: `data` is `"ok:"` followed by the granted rights joined by `|` (`ok:view|edit|control`, `ok:view`, or `ok:` with nothing). A wrong passcode is **`"status":"ok"` with `"data":"badpass"`** — not `error`.
- A successful property write (`/cue/1/name "Audio1"`) gets no reply. Unknown addresses (`/bogus/address`) get `error`.
- Replies to wildcards are one reply per matched cue (Q5).

## 2. Do actions reply?

**CONFIRMED.** Successful actions (`/go`, `/cue/1/start`, `/cue/1/stop`, `/stop`, property writes) send **no reply**. A failing action on a missing cue (`/cue/999/start`) replies `status: "error"` with no `data` field. Unknown or wrongly-cased addresses also reply `error`. Without permission, every workspace message (including `/go`) replies `denied` (Q3, Q10). Evidence: the Q1 blocks above (`/go`, `/cue/1/start` and the name write have no reply line; `/cue/999/start` and `/cue/999/name` reply `error`). That the start really happened was checked separately:

```
# 16:40:48.519 Q5 wildcards
16:40:48.519 send udp /cue/*/name ,
16:40:48.519 send udp /cue/?/name ,
16:40:48.519 send udp /cue/[12]/name ,
…
```
```
16:40:57.409 send udp /cue/{1,2}/start ,
16:40:57.955 recv udp@53001 … /reply/cue/1/isRunning ,s {"status":"ok","data":true,…}
```
(full lines in Q5)

## 3. Passcode 1234

**CONFIRMED** (one sub-point UNCLEAR, see below).

- **Before `/connect`:** `/go` is answered `denied` over **both UDP and TCP** — not silence. Every workspace-level message is `denied`, including reads and `/cueLists`. `/version` still works. `/updates 1` is answered `{"address":"\/updates","status":"denied"}` (no `workspace_id`).
- **`/connect "1234"`** → `{"status":"ok","data":"ok:view|edit|control",…}`. After that, everything works on that UDP peer / TCP connection.
- **`/connect "wrong"`** → `{"status":"ok","data":"badpass",…}`.
- `/connect` with **no argument** → `ok:` (connected with no rights). `/connect 1234` sent as an **int32** is accepted like the string.
- **Lockout:** yes, but short and growing. After a wrong passcode, further `/connect` attempts on the *same TCP connection* are answered `denied` (without checking the passcode, even if correct) for a back-off that roughly doubles with each counted failure: about 0.2–0.3 s after the first, ~0.8–0.9 s after the second, ~1.7–1.9 s after the third (bounds from the probes below). Attempts made during the back-off are answered `denied` and do not seem to extend it or count as failures. A correct passcode right after the back-off ends works. The back-off is **not shared** between TCP connections or with UDP from the same IP: a correct `/connect` on another connection 1 ms after a wrong one succeeded.
- **With no passcode at all** (No Passcode row has all rights), `/connect` is **not needed**: Q1 shows reads and actions working from a client that never sent `/connect`. But note the default for a new workspace (Setup) is that No Passcode has *no* rights, so in practice a client must `/connect` unless the operator opened up the No Passcode row.

Before `/connect` (fresh process, fresh TCP connection):
```
# 17:07:03.665 Q3 before connect
17:07:03.665 send udp /go ,
17:07:03.665 send tcp0 /go ,
17:07:03.665 send udp /cue/1/name ,
17:07:03.665 send tcp0 /cue/1/name ,
17:07:03.665 send udp /version ,
17:07:03.665 send udp /updates ,i 1
17:07:03.665 send tcp0 /updates ,i 1
17:07:03.665 send udp /cueLists ,
17:07:03.666 recv udp@53001 from=127.0.0.1:60371 /reply/go ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/go","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:07:03.666 recv tcp0 /reply/go ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/go","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:07:03.667 recv udp@53001 from=127.0.0.1:54998 /reply/cue/1/name ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:07:03.667 recv udp@53001 from=127.0.0.1:60283 /reply/version ,s {"address":"\/version","status":"ok","data":"5.5.3"}
17:07:03.667 recv udp@53001 from=127.0.0.1:63593 /reply/updates ,s {"address":"\/updates","status":"denied"}
17:07:03.667 recv udp@53001 from=127.0.0.1:62717 /reply/cueLists ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cueLists","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:07:03.667 recv tcp0 /reply/cue/1/name ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:07:03.667 recv tcp0 /reply/updates ,s {"address":"\/updates","status":"denied"}
```

Connect replies, then working access, then wrong / missing / int passcodes on fresh connections:
```
# 17:07:11.439 Q3 connect replies
17:07:11.439 send udp /connect ,s 1234
17:07:11.440 recv udp@53001 from=127.0.0.1:55189 /reply/connect ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
17:07:11.987 send tcp0 /connect ,s 1234
17:07:11.987 recv tcp0 /reply/connect ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
17:07:12.532 send udp /go ,
17:07:12.532 send udp /cue/1/name ,
17:07:12.532 send tcp0 /cue/1/name ,
17:07:12.559 recv udp@53001 from=127.0.0.1:51887 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
17:07:12.559 recv tcp0 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
17:07:13.066 send udp /stop ,
```
```
# 17:07:13.370 wrong passcode on a fresh TCP connection
17:07:13.891 send tcp2 /connect ,s wrong
17:07:13.891 recv tcp2 /reply/connect ,s {"status":"ok","data":"badpass","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
17:07:14.433 send tcp2 /connect ,
17:07:14.433 recv tcp2 /reply/connect ,s {"status":"ok","data":"ok:","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
17:07:14.979 send tcp2 /connect ,i 1234
17:07:14.979 recv tcp2 /reply/connect ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
```

One wrong then correct immediately, then every 0.5 s:
```
# 17:07:24.047 Q3 lockout: tcp3 one wrong then correct immediately, then correct every 0.5s
17:07:24.047 send tcp3 /connect ,s wrong
17:07:24.047 send tcp3 /connect ,s 1234
17:07:24.047 recv tcp3 /reply/connect ,s {"status":"ok","data":"badpass","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
17:07:24.048 recv tcp3 /reply/connect ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:07:24.557 send tcp3 /connect ,s 1234
17:07:24.558 recv tcp3 /reply/connect ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
17:07:25.063 send tcp3 /connect ,s 1234
17:07:25.064 recv tcp3 /reply/connect ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
17:07:25.568 send tcp3 /connect ,s 1234
17:07:25.569 recv tcp3 /reply/connect ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
17:07:26.077 send tcp3 /connect ,s 1234
17:07:26.077 recv tcp3 /reply/connect ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
…
```

One wrong then correct every ~0.05 s (missing replies are the duplicate-drop in "Extra finding" below, not lockout):
```
# 17:07:40.338 Q3 lockout fine: tcp4 wrong then correct every 0.05s
17:07:40.338 send tcp4 /connect ,s wrong
17:07:40.339 recv tcp4 /reply/connect ,s {"status":"ok","data":"badpass","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
17:07:40.395 send tcp4 /connect ,s 1234
17:07:40.396 recv tcp4 /reply/connect ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:07:40.451 send tcp4 /connect ,s 1234
17:07:40.507 send tcp4 /connect ,s 1234
17:07:40.507 recv tcp4 /reply/connect ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:07:40.561 send tcp4 /connect ,s 1234
17:07:40.619 send tcp4 /connect ,s 1234
17:07:40.619 recv tcp4 /reply/connect ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
17:07:40.676 send tcp4 /connect ,s 1234
17:07:40.734 send tcp4 /connect ,s 1234
17:07:40.735 recv tcp4 /reply/connect ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
17:07:40.793 send tcp4 /connect ,s 1234
```

Lockout scope (wrong on tcp4, correct on tcp5 and UDP 1 ms later):
```
# 17:07:55.512 Q3 lockout scope: tcp4 wrong then immediately correct on tcp5 and udp
17:07:55.512 send tcp4 /connect ,s wrong
17:07:55.512 send tcp5 /connect ,s 1234
17:07:55.512 send udp /connect ,s 1234
17:07:55.513 recv tcp4 /reply/connect ,s {"status":"ok","data":"badpass","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
17:07:55.513 recv tcp5 /reply/connect ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
17:07:55.513 recv udp@53001 from=127.0.0.1:64267 /reply/connect ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
```

Twenty distinct wrong passcodes 0.15 s apart, then the correct one every 0.5 s (relative times):
```
+ 0.00s  /connect 'w1'     -> ,s {"status":"ok","data":"badpass","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
+ 0.16s  /connect 'w2'     -> ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
+ 0.32s  /connect 'w3'     -> ,s {"status":"ok","data":"badpass","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
+ 0.48s  /connect 'w4'     -> ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
+ 0.63s  /connect 'w5'     -> ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
+ 0.79s  /connect 'w6'     -> ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
+ 0.95s  /connect 'w7'     -> ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
+ 1.11s  /connect 'w8'     -> ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
+ 1.26s  /connect 'w9'     -> ,s {"status":"ok","data":"badpass","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
+ 1.42s  /connect 'w10'    -> ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
+ 1.57s  /connect 'w11'    -> ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
+ 1.73s  /connect 'w12'    -> ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
+ 1.89s  /connect 'w13'    -> ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
+ 2.05s  /connect 'w14'    -> ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
+ 2.21s  /connect 'w15'    -> ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
+ 2.37s  /connect 'w16'    -> ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
+ 2.52s  /connect 'w17'    -> ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
+ 2.68s  /connect 'w18'    -> ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
+ 2.84s  /connect 'w19'    -> ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
+ 2.99s  /connect 'w20'    -> ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
+ 3.15s  /connect '1234'   -> ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
+ 3.66s  /connect '1234'   -> ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
+ 4.17s  /connect '1234'   -> ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
+ 4.68s  /connect '1234'   -> ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
+ 5.18s  /connect '1234'   -> ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
+ 5.69s  /connect '1234'   -> ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
```

**UNCLEAR:** how long the failure counter persists (I did not measure whether it resets after a correct passcode or after some quiet time), and whether UDP clients share one back-off per IP.

## 4. `/workspace/{id}/…` prefix

**CONFIRMED.** The right id (uppercase, as QLab reports it) works exactly like no prefix. A wrong id — a well-formed but unknown UUID, a non-UUID string, or **the right UUID in lowercase** — is not silent: it gets an `error` reply whose JSON has only `address` and `status` (no `workspace_id`). Application-level addresses are not available under the prefix (`/workspace/{id}/version` → `error`, and `/workspace/{id}/updates` → `error`, see Q9).

```
# 16:40:43.347 Q4
16:40:43.347 send udp /workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue/1/name ,
16:40:43.347 send udp /workspace/243ace2d-31f6-4615-87b6-bc26adaa7bf6/cue/1/name ,
16:40:43.348 send udp /workspace/00000000-0000-0000-0000-000000000000/cue/1/name ,
16:40:43.348 send udp /workspace/nonsense/cue/1/name ,
16:40:43.348 send udp /workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/version ,
16:40:43.348 send udp /workspace/00000000-0000-0000-0000-000000000000/go ,
16:40:43.348 send tcp0 /workspace/00000000-0000-0000-0000-000000000000/cue/1/name ,
16:40:43.348 send tcp0 /workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue/1/name ,
16:40:43.349 recv udp@53001 from=127.0.0.1:56354 /reply/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue/1/name ,s {"status":"ok","data":"Audio1","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
16:40:43.349 recv udp@53001 from=127.0.0.1:50881 /reply/workspace/243ace2d-31f6-4615-87b6-bc26adaa7bf6/cue/1/name ,s {"address":"\/workspace\/243ace2d-31f6-4615-87b6-bc26adaa7bf6\/cue\/1\/name","status":"error"}
16:40:43.349 recv udp@53001 from=127.0.0.1:53857 /reply/workspace/00000000-0000-0000-0000-000000000000/cue/1/name ,s {"address":"\/workspace\/00000000-0000-0000-0000-000000000000\/cue\/1\/name","status":"error"}
16:40:43.349 recv udp@53001 from=127.0.0.1:50704 /reply/workspace/nonsense/cue/1/name ,s {"address":"\/workspace\/nonsense\/cue\/1\/name","status":"error"}
16:40:43.350 recv udp@53001 from=127.0.0.1:56825 /reply/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/version ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/version","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:40:43.350 recv udp@53001 from=127.0.0.1:60150 /reply/workspace/00000000-0000-0000-0000-000000000000/go ,s {"address":"\/workspace\/00000000-0000-0000-0000-000000000000\/go","status":"error"}
16:40:43.350 recv tcp0 /reply/workspace/00000000-0000-0000-0000-000000000000/cue/1/name ,s {"address":"\/workspace\/00000000-0000-0000-0000-000000000000\/cue\/1\/name","status":"error"}
16:40:43.350 recv tcp0 /reply/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue/1/name ,s {"status":"ok","data":"Audio1","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
```

## 5. Wildcards and case

**CONFIRMED.** OSC pattern matching works in the cue-number part: `*`, `?`, `[12]` and `{1,2}` all expand, and QLab sends **one reply per matched cue**. `*` also matches cue lists (whose number is empty); their replies use a `/cue_id/{uuid}` JSON address. Matching includes cues in all cue lists and inside groups. `/cue/{1,2}/start` started both cues. Addresses are **case-sensitive**: `/GO`, `/Cue/1/start`, `/cue/1/NAME`, `/cue/1/Name`, `/cue/1/prewait`, `/VERSION` all reply `error`, while `/cue/1/preWait` works. Cue UUIDs in `/cue_id/…` are also case-sensitive (lowercase → `error`, see Q14).

```
# 16:40:48.519 Q5 wildcards
16:40:48.519 send udp /cue/*/name ,
16:40:48.519 send udp /cue/?/name ,
16:40:48.519 send udp /cue/[12]/name ,
16:40:48.520 recv udp@53001 from=127.0.0.1:61296 /reply/cue/*/name ,s {"status":"ok","data":"Main Cue List","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue_id\/9AA46B16-4874-42E9-8E1E-F62CBB8CA139\/name"}
16:40:48.520 recv udp@53001 from=127.0.0.1:49679 /reply/cue/*/name ,s {"status":"ok","data":"Audio1","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
16:40:48.520 recv udp@53001 from=127.0.0.1:55103 /reply/cue/*/name ,s {"status":"ok","data":"Audio2","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/2\/name"}
16:40:48.520 recv udp@53001 from=127.0.0.1:52153 /reply/cue/*/name ,s {"status":"ok","data":"Audio3","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/name"}
16:40:48.520 recv udp@53001 from=127.0.0.1:52153 /reply/cue/*/name ,s {"status":"ok","data":"Audio4","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/4\/name"}
16:40:48.520 recv udp@53001 from=127.0.0.1:61647 /reply/cue/*/name ,s {"status":"ok","data":"Audio5","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/5\/name"}
16:40:48.520 recv udp@53001 from=127.0.0.1:56960 /reply/cue/*/name ,s {"status":"ok","data":"Group10","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/10\/name"}
16:40:48.520 recv udp@53001 from=127.0.0.1:55643 /reply/cue/*/name ,s {"status":"ok","data":"GroupChildA","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/6\/name"}
16:40:48.521 recv udp@53001 from=127.0.0.1:57321 /reply/cue/*/name ,s {"status":"ok","data":"GroupChildB","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/7\/name"}
16:40:48.521 recv udp@53001 from=127.0.0.1:62902 /reply/cue/*/name ,s {"status":"ok","data":"Cue List","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue_id\/EB163B29-7249-4603-8D1D-03FCE040A292\/name"}
16:40:48.521 recv udp@53001 from=127.0.0.1:58242 /reply/cue/*/name ,s {"status":"ok","data":"List2Audio20","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/20\/name"}
16:40:48.521 recv udp@53001 from=127.0.0.1:53428 /reply/cue/?/name ,s {"status":"ok","data":"Audio1","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
16:40:48.521 recv udp@53001 from=127.0.0.1:53428 /reply/cue/?/name ,s {"status":"ok","data":"Audio2","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/2\/name"}
16:40:48.521 recv udp@53001 from=127.0.0.1:60664 /reply/cue/?/name ,s {"status":"ok","data":"Audio3","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/name"}
16:40:48.521 recv udp@53001 from=127.0.0.1:65093 /reply/cue/?/name ,s {"status":"ok","data":"Audio4","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/4\/name"}
16:40:48.522 recv udp@53001 from=127.0.0.1:59270 /reply/cue/?/name ,s {"status":"ok","data":"Audio5","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/5\/name"}
16:40:48.522 recv udp@53001 from=127.0.0.1:51562 /reply/cue/?/name ,s {"status":"ok","data":"GroupChildA","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/6\/name"}
16:40:48.522 recv udp@53001 from=127.0.0.1:57072 /reply/cue/?/name ,s {"status":"ok","data":"GroupChildB","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/7\/name"}
16:40:48.522 recv udp@53001 from=127.0.0.1:63111 /reply/cue/[12]/name ,s {"status":"ok","data":"Audio1","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
16:40:48.522 recv udp@53001 from=127.0.0.1:64469 /reply/cue/[12]/name ,s {"status":"ok","data":"Audio2","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/2\/name"}
16:40:49.567 send udp /cue/{1,2}/start ,
16:40:51.116 send udp /cue/*/stop ,
16:40:57.409 send udp /cue/{1,2}/start ,
16:40:57.954 send udp /cue/1/isRunning ,
16:40:57.954 send udp /cue/2/isRunning ,
16:40:57.954 send udp /cue/3/isRunning ,
16:40:57.954 send udp /runningCues ,
…
```

Proof that `/cue/{1,2}/start` and `/cue/*/stop` act:
```
16:40:57.409 send udp /cue/{1,2}/start ,
16:40:57.954 send udp /cue/1/isRunning ,
16:40:57.954 send udp /cue/2/isRunning ,
16:40:57.954 send udp /cue/3/isRunning ,
16:40:57.954 send udp /runningCues ,
16:40:57.955 recv udp@53001 from=127.0.0.1:52174 /reply/cue/1/isRunning ,s {"status":"ok","data":true,"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/isRunning"}
16:40:57.955 recv udp@53001 from=127.0.0.1:50115 /reply/cue/2/isRunning ,s {"status":"ok","data":true,"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/2\/isRunning"}
16:40:57.955 recv udp@53001 from=127.0.0.1:50824 /reply/cue/3/isRunning ,s {"status":"ok","data":false,"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/isRunning"}
16:40:57.955 recv udp@53001 from=127.0.0.1:51070 /reply/runningCues ,s {"status":"ok","data":[{"number":"1","uniqueID":"12533BF2-CD0C-4BC7-87B5-4AB68AEEE226","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio1","type":"Audio","colorName":"none","name":"Audio1","armed":true},{"number":"2","uniqueID":"A285E785-2AB9-4525-AF40-1091E9C40990","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio2","type":"Audio","colorName":"none","name":"Audio2","armed":true}],"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/runningCues"}
16:40:58.696 send udp /cue/*/stop ,
16:40:59.241 send udp /cue/1/isRunning ,
16:40:59.241 send udp /cue/2/isRunning ,
16:40:59.242 recv udp@53001 from=127.0.0.1:53156 /reply/cue/1/isRunning ,s {"status":"ok","data":false,"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/isRunning"}
16:40:59.243 recv udp@53001 from=127.0.0.1:57331 /reply/cue/2/isRunning ,s {"status":"ok","data":false,"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/2\/isRunning"}
```

Case:
```
# 16:41:03.952 Q5 case
16:41:03.952 send udp /GO ,
16:41:03.952 send udp /Cue/1/start ,
16:41:03.952 send udp /cue/1/NAME ,
16:41:03.952 send udp /cue/1/Name ,
16:41:03.952 send udp /cue/1/prewait ,
16:41:03.952 send udp /cue/1/preWait ,
16:41:03.952 send udp /VERSION ,
16:41:03.954 recv udp@53001 from=127.0.0.1:58307 /reply/GO ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/GO","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:41:03.954 recv udp@53001 from=127.0.0.1:49430 /reply/Cue/1/start ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/Cue\/1\/start","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:41:03.955 recv udp@53001 from=127.0.0.1:61050 /reply/cue/1/NAME ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/NAME","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:41:03.956 recv udp@53001 from=127.0.0.1:60859 /reply/cue/1/Name ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/Name","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:41:03.956 recv udp@53001 from=127.0.0.1:49998 /reply/cue/1/prewait ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/prewait","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:41:03.957 recv udp@53001 from=127.0.0.1:58982 /reply/cue/1/preWait ,s {"status":"ok","data":0,"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/preWait"}
16:41:03.957 recv udp@53001 from=127.0.0.1:52007 /reply/VERSION ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/VERSION","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:41:04.700 send udp /cue/1/isRunning ,
16:41:04.700 send udp /runningCues ,
16:41:04.701 recv udp@53001 from=127.0.0.1:61557 /reply/cue/1/isRunning ,s {"status":"ok","data":false,"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/isRunning"}
16:41:04.701 recv udp@53001 from=127.0.0.1:59445 /reply/runningCues ,s {"status":"ok","data":[],"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/runningCues"}
16:41:05.447 send udp /stop ,
```

## 6. `/select/{number}` vs `/playhead/{number}`

**CONFIRMED.** `/select/{n}` changes the selection; `/playhead/{n}` moves the playhead. With the workspace setting **"Lock playhead to selection"** on (the default, Workspace Settings › General), each drags the other along, so both look identical. With it off, `/select/{n}` moves only the selection and `/playhead/{n}` moves only the playhead. Selecting or setting the playhead to a cue in **another cue list switches the displayed list** to it; `/cue/selected` and `/cue/playhead` then refer to that list.

Lock on (default):
```
# 16:41:12.687 Q6 baseline
16:41:12.687 send udp /cue/selected/number ,
16:41:12.687 send udp /cue/playhead/number ,
16:41:12.687 send udp /selectedCues ,
16:41:12.688 send udp /playheadId ,
16:41:12.688 recv udp@53001 from=127.0.0.1:58769 /reply/cue/selected/number ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/selected\/number","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:41:12.689 recv udp@53001 from=127.0.0.1:53562 /reply/cue/playhead/number ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/playhead\/number","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:41:12.689 recv udp@53001 from=127.0.0.1:64576 /reply/selectedCues ,s {"status":"ok","data":[],"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/selectedCues"}
16:41:12.689 recv udp@53001 from=127.0.0.1:49903 /reply/playheadId ,s {"status":"ok","data":"none","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","deprecated":true,"warning":"DEPRECATED: OSC command \/playheadId is deprecated as of v5.0. It will be removed in a future version. Use \/playheadID instead (uppercase 'D').","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/playheadID"}
```
```
# 16:41:13.331 Q6 after /select/3
16:41:13.331 send udp /select/3 ,
16:41:13.331 send udp /cue/selected/number ,
16:41:13.331 send udp /cue/playhead/number ,
16:41:13.331 send udp /selectedCues ,
16:41:13.331 send udp /playheadId ,
16:41:13.360 recv udp@53001 from=127.0.0.1:57112 /reply/cue/selected/number ,s {"status":"ok","data":"3","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/number"}
16:41:13.360 recv udp@53001 from=127.0.0.1:64556 /reply/cue/playhead/number ,s {"status":"ok","data":"3","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/number"}
16:41:13.360 recv udp@53001 from=127.0.0.1:53703 /reply/selectedCues ,s {"status":"ok","data":[{"number":"3","uniqueID":"3E63219F-C5E8-4312-9FEC-78EAD04D8A28","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio3","type":"Audio","colorName":"none","name":"Audio3","armed":true}],"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/selectedCues"}
16:41:13.360 recv udp@53001 from=127.0.0.1:50499 /reply/playheadId ,s {"status":"ok","data":"3E63219F-C5E8-4312-9FEC-78EAD04D8A28","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","deprecated":true,"warning":"DEPRECATED: OSC command \/playheadId is deprecated as of v5.0. It will be removed in a future version. Use \/playheadID instead (uppercase 'D').","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/playheadID"}
```
```
# 16:41:13.975 Q6 after /playhead/5
16:41:13.975 send udp /playhead/5 ,
16:41:13.976 send udp /cue/selected/number ,
16:41:13.976 send udp /cue/playhead/number ,
16:41:13.976 send udp /selectedCues ,
16:41:13.976 send udp /playheadId ,
16:41:13.988 recv udp@53001 from=127.0.0.1:50293 /reply/cue/selected/number ,s {"status":"ok","data":"5","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/5\/number"}
16:41:13.988 recv udp@53001 from=127.0.0.1:61751 /reply/cue/playhead/number ,s {"status":"ok","data":"5","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/5\/number"}
16:41:13.988 recv udp@53001 from=127.0.0.1:59851 /reply/selectedCues ,s {"status":"ok","data":[{"number":"5","uniqueID":"BE801827-C647-45B5-A076-90B7EBDCD7FC","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio5","type":"Audio","colorName":"none","name":"Audio5","armed":true}],"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/selectedCues"}
16:41:13.988 recv udp@53001 from=127.0.0.1:50633 /reply/playheadId ,s {"status":"ok","data":"BE801827-C647-45B5-A076-90B7EBDCD7FC","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","deprecated":true,"warning":"DEPRECATED: OSC command \/playheadId is deprecated as of v5.0. It will be removed in a future version. Use \/playheadID instead (uppercase 'D').","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/playheadID"}
```

Lock off (`/cue/selected/number` then `/cue/playhead/number` after each command; `workspace_id`/`address` trimmed here for width, full lines in the raw log):
```
# Q6 lock-off: /playhead/2
16:42:45.961 recv /reply/cue/selected/number -> status ok, data '3'
16:42:45.961 recv /reply/cue/playhead/number -> status ok, data '2'
# Q6 lock-off: /select/4
16:42:46.606 recv /reply/cue/selected/number -> status ok, data '4'
16:42:46.606 recv /reply/cue/playhead/number -> status ok, data '2'
# Q6 lock-off: /playhead/1
16:42:47.244 recv /reply/cue/selected/number -> status ok, data '4'
16:42:47.244 recv /reply/cue/playhead/number -> status ok, data '1'
# Q6 lock-off: /select/10
16:42:47.901 recv /reply/cue/selected/number -> status ok, data '10'
16:42:47.901 recv /reply/cue/playhead/number -> status ok, data '1'
# Q6 lock-off: /select/20
16:42:48.562 recv /reply/cue/selected/number -> status ok, data '20'
16:42:48.562 recv /reply/cue/playhead/number -> status ok, data '20'
# Q6 lock-off: /playhead/20
16:42:49.172 recv /reply/cue/selected/number -> status ok, data '20'
16:42:49.172 recv /reply/cue/playhead/number -> status ok, data '20'
```

## 7. `/stop`, `/panic`, `/hardStop`, `/cue/{n}/panic`

**CONFIRMED** (for this workspace's default settings: panic duration 1 s). Cue 1 was started, then the command sent, then `/cue/1/isRunning` polled every 0.1 s:

| Command | Result |
|---|---|
| `/stop` (no cue) | stops at once (not running at the first poll, +0.01 s) |
| `/panic` | fades: still running until ~+1.07 s, i.e. the 1 s panic duration |
| `/hardStop` | exists; stops at once |
| `/cue/1/stop` | stops within ~0.1 s (still running at +0.01 s, stopped at +0.11 s) — practically instant, no audible fade with default settings |
| `/cue/1/panic` | fades like `/panic`: ~+1.07 s |
| `/cue/1/hardStop` | stops at once |
| `/hardPanic` | does not exist: `error` reply, cue kept running |

None of the existing ones reply.

```
16:41:53.625 send udp /stop ,
    /cue/1/isRunning polled every 0.1 s: false at +0.01s
16:41:58.686 send udp /panic ,
    /cue/1/isRunning polled every 0.1 s: true at +0.01s, false at +1.07s
16:42:03.744 send udp /hardStop ,
    /cue/1/isRunning polled every 0.1 s: false at +0.01s
16:42:08.811 send udp /cue/1/stop ,
    /cue/1/isRunning polled every 0.1 s: true at +0.01s, false at +0.11s
16:42:13.861 send udp /cue/1/panic ,
    /cue/1/isRunning polled every 0.1 s: true at +0.01s, false at +1.07s
16:42:18.926 send udp /cue/1/hardStop ,
    /cue/1/isRunning polled every 0.1 s: false at +0.01s
16:42:24.007 send udp /hardPanic ,
16:42:24.008 recv udp@53001 from=127.0.0.1:64019 /reply/hardPanic ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/hardPanic","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
    /cue/1/isRunning polled every 0.1 s: true at +0.00s
```

## 8. `/go "3"` vs `/go 3`

**CONFIRMED.** A **string** argument is used as a cue number: `/go "3"` (playhead on 1) started cue 3 and moved the playhead to the cue after it (4). An **int** or **float** argument is ignored: `/go 3` and `/go 3.0` behaved like a plain `/go` (cue 1 ran, playhead moved to 2). `/go "999"` (no such cue) → `error`. (`/go ""` produced no reply; I did not check what it did.)

```
# Q8 /go s:3 with playhead on 1
16:41:29.941 send udp /playhead/1 ,
16:41:30.284 send udp /go ,s 3
16:41:30.928 recv /reply/runningCues -> running cue numbers ['3']
16:41:30.929 recv /reply/cue/playhead/number -> '4'
# Q8 /go i:3 with playhead on 1
16:41:32.713 send udp /playhead/1 ,
16:41:33.059 send udp /go ,i 3
16:41:33.707 recv /reply/runningCues -> running cue numbers ['1']
16:41:33.708 recv /reply/cue/playhead/number -> '2'
# Q8 /go f:3.0 with playhead on 1
16:41:35.497 send udp /playhead/1 ,
16:41:35.843 send udp /go ,f 3.0
16:41:36.489 recv /reply/runningCues -> running cue numbers ['1']
16:41:36.490 recv /reply/cue/playhead/number -> '2'
```
```
# 16:41:51.532 Q8b
16:41:51.532 send udp /go ,s 999
16:41:51.532 send udp /go ,s
16:41:51.533 recv udp@53001 from=127.0.0.1:55084 /reply/go ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/go","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:41:52.076 send udp /stop ,
16:41:52.613 send udp /cue/1/start ,
```

## 9. How long does QLab remember a UDP client?

**CONFIRMED for update subscriptions: about 60 s of silence (lost between 60 s and 65 s).** CONFIRMED for passcode sessions: a UDP client's `/connect` is forgotten after about 60 s of silence (still connected after 58 s, `denied` after 62 s, 70 s and 300 s). Tested 30, 50, 58, 62, 70 and 300 s; I stopped before the 15-minute probe because expiry was already established at ~60 s.

Update subscription (No Passcode = full rights). After `/updates 1` over UDP and no further UDP traffic, the UDP client stopped receiving updates between +60.09 s and +65.09 s. A second TCP connection (`tcp1`, not subscribed) moved the playhead every 5 s to generate updates; it received none itself.
```
+  0.00s send udp /updates ,i 1
+  5.02s send tcp1 /playhead/2 ,
+  5.02s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s A285E785-2AB9-4525-AF40-1091E9C40990
+ 10.02s send tcp1 /playhead/3 ,
+ 10.03s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s 3E63219F-C5E8-4312-9FEC-78EAD04D8A28
+ 15.03s send tcp1 /playhead/4 ,
+ 15.03s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s C2A8C4FA-FE42-484F-A071-6FCB16A8FC65
+ 20.03s send tcp1 /playhead/1 ,
+ 20.04s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s 12533BF2-CD0C-4BC7-87B5-4AB68AEEE226
+ 25.04s send tcp1 /playhead/2 ,
+ 25.04s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s A285E785-2AB9-4525-AF40-1091E9C40990
+ 30.04s send tcp1 /playhead/3 ,
+ 30.04s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s 3E63219F-C5E8-4312-9FEC-78EAD04D8A28
+ 35.05s send tcp1 /playhead/4 ,
+ 35.05s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s C2A8C4FA-FE42-484F-A071-6FCB16A8FC65
+ 40.05s send tcp1 /playhead/1 ,
+ 40.06s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s 12533BF2-CD0C-4BC7-87B5-4AB68AEEE226
+ 45.06s send tcp1 /playhead/2 ,
+ 45.07s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s A285E785-2AB9-4525-AF40-1091E9C40990
+ 50.07s send tcp1 /playhead/3 ,
+ 50.07s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s 3E63219F-C5E8-4312-9FEC-78EAD04D8A28
+ 55.08s send tcp1 /playhead/4 ,
+ 55.08s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s C2A8C4FA-FE42-484F-A071-6FCB16A8FC65
+ 60.09s send tcp1 /playhead/1 ,
+ 60.09s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s 12533BF2-CD0C-4BC7-87B5-4AB68AEEE226
+ 65.09s send tcp1 /playhead/2 ,
+ 70.10s send tcp1 /playhead/3 ,
+ 75.11s send tcp1 /playhead/4 ,
+ 80.12s send tcp1 /playhead/1 ,
+ 85.13s send tcp1 /playhead/2 ,
+ 90.13s send tcp1 /playhead/3 ,
+ 95.14s send tcp1 /playhead/4 ,
+100.15s send tcp1 /playhead/1 ,
```

Any workspace-level UDP message keeps the client alive: sending `/cue/1/name` every 30 s kept updates flowing; they stopped 60–70 s after the last one (+150 s received, +160 s not).
```
+  0.00s send udp /updates ,i 1
+ 10.01s send tcp1 /playhead/2 ,
+ 20.01s send tcp1 /playhead/3 ,
+ 30.02s send tcp1 /playhead/4 ,
+ 30.02s send udp /cue/1/name ,
+ 30.05s recv udp@53001 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
+ 40.02s send tcp1 /playhead/1 ,
+ 40.03s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s 12533BF2-CD0C-4BC7-87B5-4AB68AEEE226
+ 50.03s send tcp1 /playhead/2 ,
+ 50.04s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s A285E785-2AB9-4525-AF40-1091E9C40990
+ 60.04s send tcp1 /playhead/3 ,
+ 60.04s send udp /cue/1/name ,
+ 60.05s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s 3E63219F-C5E8-4312-9FEC-78EAD04D8A28
+ 60.08s recv udp@53001 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
+ 70.05s send tcp1 /playhead/4 ,
+ 70.05s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s C2A8C4FA-FE42-484F-A071-6FCB16A8FC65
+ 80.06s send tcp1 /playhead/1 ,
+ 80.06s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s 12533BF2-CD0C-4BC7-87B5-4AB68AEEE226
+ 90.07s send tcp1 /playhead/2 ,
+ 90.07s send udp /cue/1/name ,
+ 90.07s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s A285E785-2AB9-4525-AF40-1091E9C40990
+ 90.11s recv udp@53001 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
+100.07s send tcp1 /playhead/3 ,
+100.08s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s 3E63219F-C5E8-4312-9FEC-78EAD04D8A28
+110.08s send tcp1 /playhead/4 ,
+110.09s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s C2A8C4FA-FE42-484F-A071-6FCB16A8FC65
+120.09s send tcp1 /playhead/1 ,
+120.09s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s 12533BF2-CD0C-4BC7-87B5-4AB68AEEE226
+130.10s send tcp1 /playhead/2 ,
+130.10s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s A285E785-2AB9-4525-AF40-1091E9C40990
+140.11s send tcp1 /playhead/3 ,
+140.11s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s 3E63219F-C5E8-4312-9FEC-78EAD04D8A28
+150.12s send tcp1 /playhead/4 ,
+150.12s recv udp@53001 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s C2A8C4FA-FE42-484F-A071-6FCB16A8FC65
+160.12s send tcp1 /playhead/1 ,
```

After the client has expired, re-subscribing has a catch: `/updates 1` on its own did **not** bring updates back (it got no reply at all), nor did a following `/version`; updates only resumed after a **workspace-level** message (`/cue/1/name`). A workspace-level message alone, without a fresh `/updates 1`, did not resume them either. `/workspace/{id}/updates 1` is not a valid address (`error`). My reading of this (inference, not proven): `/updates` is an application-level message that sets a flag on the peer, and the per-workspace UDP client is only (re)created by a workspace-level message; both pieces are needed after an expiry.
```
# 16:52:15.707 Q9c after >65s UDP silence: /updates 1 alone, then /version, then /cue/1/name; tcp1 moves playhead between
16:52:15.707 send udp /updates ,i 1
16:52:19.715 send tcp1 /playhead/1 ,
16:52:22.720 send tcp1 /playhead/2 ,
16:52:24.728 send udp /updates ,i 1
16:52:26.736 send tcp1 /playhead/3 ,
16:52:28.743 send udp /version ,
16:52:28.744 recv udp@53001 from=127.0.0.1:61516 /reply/version ,s {"address":"\/version","status":"ok","data":"5.5.3"}
16:52:30.750 send tcp1 /playhead/4 ,
16:52:32.757 send udp /cue/1/name ,
16:52:32.759 recv udp@53001 from=127.0.0.1:59919 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
16:52:34.764 send tcp1 /playhead/1 ,
16:52:34.769 recv udp@53001 from=127.0.0.1:56011 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s 12533BF2-CD0C-4BC7-87B5-4AB68AEEE226
16:52:34.820 recv udp@53001 from=127.0.0.1:59171 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/dashboard ,
16:52:36.772 send tcp1 /playhead/2 ,
16:52:36.777 recv udp@53001 from=127.0.0.1:64415 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s A285E785-2AB9-4525-AF40-1091E9C40990
16:52:36.828 recv udp@53001 from=127.0.0.1:64833 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/dashboard ,
16:53:57.489 send tcp1 /playhead/3 ,
```
```
# 16:53:58.497 Q9d after >65s UDP silence: only /cue/1/name (no /updates)
16:53:58.497 send udp /cue/1/name ,
16:53:58.498 recv udp@53001 from=127.0.0.1:50568 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
16:53:59.506 send tcp1 /playhead/4 ,
```
```
# 16:54:00.513 Q9d /updates 0 while live
16:54:00.513 send udp /updates ,i 0
16:54:01.518 send tcp1 /playhead/1 ,
```
```
# 16:55:07.526 Q9e after >65s silence, unsubscribed: /workspace/{id}/updates 1 alone
16:55:07.526 send udp /workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/updates ,i 1
16:55:07.527 recv udp@53001 from=127.0.0.1:62264 /reply/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/updates ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/updates","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:55:08.534 send tcp1 /playhead/2 ,
16:55:09.542 send tcp1 /playhead/3 ,
```

Passcode sessions over UDP (passcode 1234; each probe follows a fresh `/connect "1234"` and N seconds of UDP silence; `denied` means QLab had forgotten the connection):
```
# 17:09:10.521 Q9P connect then 30s UDP silence
17:09:10.521 send udp /connect ,s 1234
17:09:10.523 recv udp@53001 from=127.0.0.1:64809 /reply/connect ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
# 17:09:40.525 Q9P probe after 30s
17:09:40.525 send udp /cue/1/name ,
17:09:40.526 recv udp@53001 from=127.0.0.1:63977 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
17:09:41.533 send udp /cue/2/name ,
17:09:41.535 recv udp@53001 from=127.0.0.1:53778 /reply/cue/2/name ,s {"status":"ok","data":"RenamedTwo","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/2\/name"}
# 17:09:42.541 Q9P connect then 50s UDP silence
17:09:42.542 send udp /connect ,s 1234
17:09:42.543 recv udp@53001 from=127.0.0.1:55163 /reply/connect ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
# 17:10:32.550 Q9P probe after 50s
17:10:32.550 send udp /cue/1/name ,
17:10:32.552 recv udp@53001 from=127.0.0.1:57286 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
17:10:33.557 send udp /cue/2/name ,
17:10:33.559 recv udp@53001 from=127.0.0.1:63995 /reply/cue/2/name ,s {"status":"ok","data":"RenamedTwo","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/2\/name"}
# 17:10:34.566 Q9P connect then 58s UDP silence
17:10:34.567 send udp /connect ,s 1234
17:10:34.567 recv udp@53001 from=127.0.0.1:64166 /reply/connect ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
# 17:11:32.575 Q9P probe after 58s
17:11:32.575 send udp /cue/1/name ,
17:11:32.577 recv udp@53001 from=127.0.0.1:60352 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
17:11:33.583 send udp /cue/2/name ,
17:11:33.585 recv udp@53001 from=127.0.0.1:64499 /reply/cue/2/name ,s {"status":"ok","data":"RenamedTwo","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/2\/name"}
# 17:11:34.589 Q9P connect then 62s UDP silence
17:11:34.589 send udp /connect ,s 1234
17:11:34.590 recv udp@53001 from=127.0.0.1:61195 /reply/connect ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
# 17:12:36.597 Q9P probe after 62s
17:12:36.598 send udp /cue/1/name ,
17:12:36.599 recv udp@53001 from=127.0.0.1:61142 /reply/cue/1/name ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:12:37.612 send udp /cue/2/name ,
17:12:37.614 recv udp@53001 from=127.0.0.1:52327 /reply/cue/2/name ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/2\/name","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
# 17:12:38.626 Q9P connect then 70s UDP silence
17:12:38.626 send udp /connect ,s 1234
17:12:38.628 recv udp@53001 from=127.0.0.1:54664 /reply/connect ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
# 17:13:48.640 Q9P probe after 70s
17:13:48.640 send udp /cue/1/name ,
17:13:48.641 recv udp@53001 from=127.0.0.1:64340 /reply/cue/1/name ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:13:49.647 send udp /cue/2/name ,
17:13:49.648 recv udp@53001 from=127.0.0.1:57476 /reply/cue/2/name ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/2\/name","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
# 17:13:50.655 Q9P connect then 300s UDP silence
17:13:50.655 send udp /connect ,s 1234
17:13:50.656 recv udp@53001 from=127.0.0.1:49175 /reply/connect ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
# 17:18:50.665 Q9P probe after 300s
17:18:50.665 send udp /cue/1/name ,
17:18:50.666 recv udp@53001 from=127.0.0.1:60926 /reply/cue/1/name ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:18:51.669 send udp /cue/2/name ,
17:18:51.670 recv udp@53001 from=127.0.0.1:58647 /reply/cue/2/name ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/2\/name","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
# 17:18:52.677 Q9P connect then 900s UDP silence
17:18:52.677 send udp /connect ,s 1234
17:18:52.678 recv udp@53001 from=127.0.0.1:60831 /reply/connect ,s {"status":"ok","data":"ok:view|edit|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
# 17:18:58.224 Q9P stopped by hand before the 900s wait (expiry already shown at ~60s)
```

## 10. Permission levels

**CONFIRMED.** Each passcode, and the "No Passcode" row, has three checkboxes: **View, Edit, Control**. Edit and Control can only be ticked when View is ticked (they are greyed out otherwise). `/connect` reports the granted set in its `data`: `ok:view|edit|control`, `ok:view|edit`, `ok:view|control`, `ok:view`, or `ok:`.

What each right allowed (each tested on its own TCP connection after `/connect` with a passcode holding exactly those rights; "allowed" means no `denied` reply, and for writes the value was read back):

| Message | View only | View + Edit | View + Control |
|---|---|---|---|
| reads (`/cue/1/name`, `/cue/3/preWait`, `/cueLists`) | allowed | allowed | allowed |
| `/cue/{n}/notes "…"` (write) | **allowed** (value changed) | allowed | allowed |
| `/cue/{n}/name "…"`, `/cue/{n}/preWait x` (writes) | `denied` | allowed | `denied` |
| `/select/2` | `denied` | allowed | allowed |
| `/playhead/3` | `denied` | `denied` | allowed |
| `/go`, `/stop`, `/cue/1/start`, `/cue/1/stop` | `denied` | `denied` | allowed |
| `/updates 1` / `/updates 0` | allowed (no reply) | allowed | allowed |

So a **view-only** client can read everything, subscribe to updates and (surprisingly) edit cue **notes**, but cannot rename cues, change timings, move the selection or playhead, or run anything. The same view-only result was seen earlier with the "No Passcode" row set to view only (no `/connect` needed there); in that test a name write was `denied` and a UDP `/updates 1` was accepted (updates later arrived on 53001, visible at the start of the Q1 capture). Note one oddity in the replies: `/playhead/3` is denied with JSON address `/workspace/{id}/playhead` (the number is dropped).

Passcodes with exactly one right set each (`2677` = view, `6510` = view+edit, `2375` = view+control):
```
# 17:19:39.152 Q10b passcode 2677 (view) on tcp7
17:19:39.152 send tcp7 /connect ,s 2677
17:19:39.153 recv tcp7 /reply/connect ,s {"status":"ok","data":"ok:view","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
17:19:39.410 send tcp7 /cue/1/name ,
17:19:39.412 recv tcp7 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
17:19:39.670 send tcp7 /cue/1/notes ,s note7
17:19:39.927 send tcp7 /select/2 ,
17:19:39.927 recv tcp7 /reply/select/2 ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/select\/2","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:19:40.185 send tcp7 /playhead/3 ,
17:19:40.186 recv tcp7 /reply/playhead/3 ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/playhead","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:19:40.442 send tcp7 /cue/1/start ,
17:19:40.443 recv tcp7 /reply/cue/1/start ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/start","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:19:40.699 send tcp7 /cue/1/stop ,
17:19:40.699 recv tcp7 /reply/cue/1/stop ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/stop","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:19:40.957 send tcp7 /go ,
17:19:40.958 recv tcp7 /reply/go ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/go","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:19:41.216 send tcp7 /stop ,
17:19:41.216 recv tcp7 /reply/stop ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/stop","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:19:41.473 send tcp7 /updates ,i 1
17:19:41.731 send tcp7 /updates ,i 0
17:19:41.990 send tcp7 /cueLists ,
17:19:41.991 recv tcp7 /reply/cueLists ,s {"status":"ok","data":[{"number":"","uniqueID":"9AA46B16-4874-42E9-8E1E-F62CBB8CA139","cues":[{"number":"1","uniqueID":"12533BF2-CD0C-4BC7-87B5-4AB68AEEE226","cues":[],"colorName\/live":"none","flagged":false,"listName":"RenamedOne","type":"Audio","colorName":"none","name":"RenamedOne","armed":true},{"number":"2","uniqueID":"A285E785-2AB9-4525-AF40-1091E9C40990","cues":[],"colorName\/live":"none","flagged":false,"listName":"RenamedTwo","type":"Audio","colorName":"none","name":"RenamedTwo","armed":true},{"number":"4","uniqueID":"C2A8C4FA-FE42-484F-A071-6FCB16A8FC65","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio4","type":"Audio","colorName":"none","name":"Audio4","armed":true},{"number":"3","uniqueID":"3E63219F-C5E8-4312-9FEC-78EAD04D8A28","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio3","type":"Audio","colorName":"none","name":"Audio3","armed":true},{"number":"10","uniqueID":"D18C05C9-6D75-4794-B573-2DA09868A4A2","cues":[{"number":"6","uniqueID":"4C601614-38C0-416F-A835-E7D6262F7925","cues":[],"colorName\/live":"none","flagged":false,"listName":"GroupChildA","type":"Audio","colorName":"none","name":"GroupChildA","armed":true},{"number":"7","uniqueID":"A253A8BE-07FC-46FC-BDE8-39832A981784","cues":[],"colorName\/live":"none","flagged":false,"listName":"GroupChildB","type":"Audio","colorName":"none","name":"GroupChildB","armed":true}],"colorName\/live":"none","flagged":false,"listName":"Group10","type":"Group","colorName":"none","name":"Group10","armed":true},{"number":"5","uniqueID":"BE801827-C647-45B5-A076-90B7EBDCD7FC","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio5","type":"Audio","colorName":"none","name":"Audio5","armed":true}],"colorName\/live":"none","flagged":false,"listName":"Main Cue List","type":"Cue List","colorName":"none","name":"Main Cue List","armed":true},{"number":"","uniqueID":"EB163B29-7249-4603-8D1D-03FCE040A292","cues":[{"number":"20","uniqueID":"F90D7E43-CA39-4DEE-B2A1-6925F69E3C12","cues":[],"colorName\/live":"none","flagged":false,"listName":"List2Audio20","type":"Audio","colorName":"none","name":"List2Audio20","armed":true}],"colorName\/live":"none","flagged":false,"listName":"Cue List","type":"Cue List","colorName":"none","name":"Cue List","armed":true}],"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cueLists"}
```
```
# 17:19:42.246 Q10b passcode 6510 (view+edit) on tcp8
17:19:42.246 send tcp8 /connect ,s 6510
17:19:42.247 recv tcp8 /reply/connect ,s {"status":"ok","data":"ok:view|edit","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
17:19:42.505 send tcp8 /cue/1/name ,
17:19:42.505 recv tcp8 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
17:19:42.763 send tcp8 /cue/1/notes ,s note8
17:19:43.022 send tcp8 /select/2 ,
17:19:43.280 send tcp8 /playhead/3 ,
17:19:43.281 recv tcp8 /reply/playhead/3 ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/playhead","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:19:43.539 send tcp8 /cue/1/start ,
17:19:43.540 recv tcp8 /reply/cue/1/start ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/start","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:19:43.797 send tcp8 /cue/1/stop ,
17:19:43.798 recv tcp8 /reply/cue/1/stop ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/stop","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:19:44.056 send tcp8 /go ,
17:19:44.057 recv tcp8 /reply/go ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/go","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:19:44.314 send tcp8 /stop ,
17:19:44.314 recv tcp8 /reply/stop ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/stop","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:19:44.572 send tcp8 /updates ,i 1
17:19:44.832 send tcp8 /updates ,i 0
17:19:45.090 send tcp8 /cueLists ,
17:19:45.090 recv tcp8 /reply/cueLists ,s {"status":"ok","data":[{"number":"","uniqueID":"9AA46B16-4874-42E9-8E1E-F62CBB8CA139","cues":[{"number":"1","uniqueID":"12533BF2-CD0C-4BC7-87B5-4AB68AEEE226","cues":[],"colorName\/live":"none","flagged":false,"listName":"RenamedOne","type":"Audio","colorName":"none","name":"RenamedOne","armed":true},{"number":"2","uniqueID":"A285E785-2AB9-4525-AF40-1091E9C40990","cues":[],"colorName\/live":"none","flagged":false,"listName":"RenamedTwo","type":"Audio","colorName":"none","name":"RenamedTwo","armed":true},{"number":"4","uniqueID":"C2A8C4FA-FE42-484F-A071-6FCB16A8FC65","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio4","type":"Audio","colorName":"none","name":"Audio4","armed":true},{"number":"3","uniqueID":"3E63219F-C5E8-4312-9FEC-78EAD04D8A28","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio3","type":"Audio","colorName":"none","name":"Audio3","armed":true},{"number":"10","uniqueID":"D18C05C9-6D75-4794-B573-2DA09868A4A2","cues":[{"number":"6","uniqueID":"4C601614-38C0-416F-A835-E7D6262F7925","cues":[],"colorName\/live":"none","flagged":false,"listName":"GroupChildA","type":"Audio","colorName":"none","name":"GroupChildA","armed":true},{"number":"7","uniqueID":"A253A8BE-07FC-46FC-BDE8-39832A981784","cues":[],"colorName\/live":"none","flagged":false,"listName":"GroupChildB","type":"Audio","colorName":"none","name":"GroupChildB","armed":true}],"colorName\/live":"none","flagged":false,"listName":"Group10","type":"Group","colorName":"none","name":"Group10","armed":true},{"number":"5","uniqueID":"BE801827-C647-45B5-A076-90B7EBDCD7FC","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio5","type":"Audio","colorName":"none","name":"Audio5","armed":true}],"colorName\/live":"none","flagged":false,"listName":"Main Cue List","type":"Cue List","colorName":"none","name":"Main Cue List","armed":true},{"number":"","uniqueID":"EB163B29-7249-4603-8D1D-03FCE040A292","cues":[{"number":"20","uniqueID":"F90D7E43-CA39-4DEE-B2A1-6925F69E3C12","cues":[],"colorName\/live":"none","flagged":false,"listName":"List2Audio20","type":"Audio","colorName":"none","name":"List2Audio20","armed":true}],"colorName\/live":"none","flagged":false,"listName":"Cue List","type":"Cue List","colorName":"none","name":"Cue List","armed":true}],"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cueLists"}
```
```
# 17:19:45.349 Q10b passcode 2375 (view+control) on tcp9
17:19:45.349 send tcp9 /connect ,s 2375
17:19:45.349 recv tcp9 /reply/connect ,s {"status":"ok","data":"ok:view|control","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
17:19:45.607 send tcp9 /cue/1/name ,
17:19:45.608 recv tcp9 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
17:19:45.865 send tcp9 /cue/1/notes ,s note9
17:19:46.123 send tcp9 /select/2 ,
17:19:46.381 send tcp9 /playhead/3 ,
17:19:46.639 send tcp9 /cue/1/start ,
17:19:46.898 send tcp9 /cue/1/stop ,
17:19:47.157 send tcp9 /go ,
17:19:47.414 send tcp9 /stop ,
17:19:47.673 send tcp9 /updates ,i 1
17:19:47.931 send tcp9 /updates ,i 0
17:19:48.190 send tcp9 /cueLists ,
17:19:48.191 recv tcp9 /reply/cueLists ,s {"status":"ok","data":[{"number":"","uniqueID":"9AA46B16-4874-42E9-8E1E-F62CBB8CA139","cues":[{"number":"1","uniqueID":"12533BF2-CD0C-4BC7-87B5-4AB68AEEE226","cues":[],"colorName\/live":"none","flagged":false,"listName":"RenamedOne","type":"Audio","colorName":"none","name":"RenamedOne","armed":true},{"number":"2","uniqueID":"A285E785-2AB9-4525-AF40-1091E9C40990","cues":[],"colorName\/live":"none","flagged":false,"listName":"RenamedTwo","type":"Audio","colorName":"none","name":"RenamedTwo","armed":true},{"number":"4","uniqueID":"C2A8C4FA-FE42-484F-A071-6FCB16A8FC65","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio4","type":"Audio","colorName":"none","name":"Audio4","armed":true},{"number":"3","uniqueID":"3E63219F-C5E8-4312-9FEC-78EAD04D8A28","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio3","type":"Audio","colorName":"none","name":"Audio3","armed":true},{"number":"10","uniqueID":"D18C05C9-6D75-4794-B573-2DA09868A4A2","cues":[{"number":"6","uniqueID":"4C601614-38C0-416F-A835-E7D6262F7925","cues":[],"colorName\/live":"none","flagged":false,"listName":"GroupChildA","type":"Audio","colorName":"none","name":"GroupChildA","armed":true},{"number":"7","uniqueID":"A253A8BE-07FC-46FC-BDE8-39832A981784","cues":[],"colorName\/live":"none","flagged":false,"listName":"GroupChildB","type":"Audio","colorName":"none","name":"GroupChildB","armed":true}],"colorName\/live":"none","flagged":false,"listName":"Group10","type":"Group","colorName":"none","name":"Group10","armed":true},{"number":"5","uniqueID":"BE801827-C647-45B5-A076-90B7EBDCD7FC","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio5","type":"Audio","colorName":"none","name":"Audio5","armed":true}],"colorName\/live":"none","flagged":false,"listName":"Main Cue List","type":"Cue List","colorName":"none","name":"Main Cue List","armed":true},{"number":"","uniqueID":"EB163B29-7249-4603-8D1D-03FCE040A292","cues":[{"number":"20","uniqueID":"F90D7E43-CA39-4DEE-B2A1-6925F69E3C12","cues":[],"colorName\/live":"none","flagged":false,"listName":"List2Audio20","type":"Audio","colorName":"none","name":"List2Audio20","armed":true}],"colorName\/live":"none","flagged":false,"listName":"Cue List","type":"Cue List","colorName":"none","name":"Cue List","armed":true}],"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cueLists"}
```

Which writes need which right (write, then read back, on the same three connections):
```
# 17:19:57.911 Q10c which rights allow writes: per connection write notes+name then read back
17:19:57.911 send tcp7 /cue/3/notes ,s N7
17:19:58.115 send tcp7 /cue/3/notes ,
17:19:58.116 recv tcp7 /reply/cue/3/notes ,s {"status":"ok","data":"N7","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/notes"}
17:19:58.322 send tcp7 /cue/3/name ,s Name7
17:19:58.323 recv tcp7 /reply/cue/3/name ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/name","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:19:58.526 send tcp7 /cue/3/name ,
17:19:58.528 recv tcp7 /reply/cue/3/name ,s {"status":"ok","data":"Audio3","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/name"}
17:19:58.731 send tcp7 /cue/3/preWait ,f 0.7
17:19:58.733 recv tcp7 /reply/cue/3/preWait ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/preWait","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:19:58.939 send tcp7 /cue/3/preWait ,
17:19:58.940 recv tcp7 /reply/cue/3/preWait ,s {"status":"ok","data":0,"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/preWait"}
17:19:59.144 send tcp8 /cue/3/notes ,s N8
17:19:59.350 send tcp8 /cue/3/notes ,
17:19:59.351 recv tcp8 /reply/cue/3/notes ,s {"status":"ok","data":"N8","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/notes"}
17:19:59.558 send tcp8 /cue/3/name ,s Name8
17:19:59.763 send tcp8 /cue/3/name ,
17:19:59.764 recv tcp8 /reply/cue/3/name ,s {"status":"ok","data":"Name8","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/name"}
17:19:59.971 send tcp8 /cue/3/preWait ,f 0.8
17:20:00.179 send tcp8 /cue/3/preWait ,
17:20:00.180 recv tcp8 /reply/cue/3/preWait ,s {"status":"ok","data":0.80000001192092896,"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/preWait"}
17:20:00.387 send tcp9 /cue/3/notes ,s N9
17:20:00.595 send tcp9 /cue/3/notes ,
17:20:00.596 recv tcp9 /reply/cue/3/notes ,s {"status":"ok","data":"N9","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/notes"}
17:20:00.804 send tcp9 /cue/3/name ,s Name9
17:20:00.805 recv tcp9 /reply/cue/3/name ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/name","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:20:01.013 send tcp9 /cue/3/name ,
17:20:01.014 recv tcp9 /reply/cue/3/name ,s {"status":"ok","data":"Name8","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/name"}
17:20:01.222 send tcp9 /cue/3/preWait ,f 0.9
17:20:01.223 recv tcp9 /reply/cue/3/preWait ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/preWait","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:20:01.431 send tcp9 /cue/3/preWait ,
17:20:01.432 recv tcp9 /reply/cue/3/preWait ,s {"status":"ok","data":0.80000001192092896,"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/preWait"}
17:20:02.079 send tcp9 /cue/3/name ,s Audio3
17:20:02.079 send tcp9 /cue/3/notes ,s
17:20:02.079 send tcp9 /cue/3/preWait ,f 0.0
17:20:02.079 send tcp9 /cue/1/notes ,s
17:20:02.080 recv tcp9 /reply/cue/3/name ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/name","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
17:20:02.081 recv tcp9 /reply/cue/3/preWait ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/preWait","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
```

"No Passcode" row set to view only (no `/connect` sent first):
```
# 16:40:06.292 Q10 NoPasscode=VIEW only (6997 full exists); fresh daemon, no connect
16:40:06.292 send udp /cue/1/name ,
16:40:06.292 send udp /cue/1/name ,s ViewRename
16:40:06.292 send udp /go ,
16:40:06.292 send udp /cue/1/start ,
16:40:06.292 send udp /updates ,i 1
16:40:06.292 send udp /cueLists ,
16:40:06.292 send udp /connect ,
16:40:06.292 send tcp0 /cue/1/name ,
16:40:06.292 send tcp0 /cue/1/name ,s ViewRename
16:40:06.292 send tcp0 /go ,
16:40:06.292 send tcp0 /connect ,
16:40:06.292 send tcp0 /cue/1/duration ,
16:40:06.292 send tcp0 /cue/1/preWait ,f 1.0
16:40:06.292 send tcp0 /select/2 ,
16:40:06.292 send tcp0 /playhead/2 ,
16:40:06.292 send tcp0 /stop ,
16:40:06.293 recv udp@53001 from=127.0.0.1:54949 /reply/cue/1/name ,s {"status":"ok","data":"Audio1","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
16:40:06.294 recv udp@53001 from=127.0.0.1:59842 /reply/cue/1/name ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:40:06.294 recv udp@53001 from=127.0.0.1:54402 /reply/go ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/go","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:40:06.294 recv udp@53001 from=127.0.0.1:59515 /reply/cue/1/start ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/start","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:40:06.295 recv udp@53001 from=127.0.0.1:50769 /reply/cueLists ,s {"status":"ok","data":[{"number":"","uniqueID":"9AA46B16-4874-42E9-8E1E-F62CBB8CA139","cues":[{"number":"1","uniqueID":"12533BF2-CD0C-4BC7-87B5-4AB68AEEE226","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio1","type":"Audio","colorName":"none","name":"Audio1","armed":true},{"number":"2","uniqueID":"A285E785-2AB9-4525-AF40-1091E9C40990","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio2","type":"Audio","colorName":"none","name":"Audio2","armed":true},{"number":"3","uniqueID":"3E63219F-C5E8-4312-9FEC-78EAD04D8A28","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio3","type":"Audio","colorName":"none","name":"Audio3","armed":true},{"number":"4","uniqueID":"C2A8C4FA-FE42-484F-A071-6FCB16A8FC65","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio4","type":"Audio","colorName":"none","name":"Audio4","armed":true},{"number":"5","uniqueID":"BE801827-C647-45B5-A076-90B7EBDCD7FC","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio5","type":"Audio","colorName":"none","name":"Audio5","armed":true},{"number":"10","uniqueID":"D18C05C9-6D75-4794-B573-2DA09868A4A2","cues":[{"number":"6","uniqueID":"4C601614-38C0-416F-A835-E7D6262F7925","cues":[],"colorName\/live":"none","flagged":false,"listName":"GroupChildA","type":"Audio","colorName":"none","name":"GroupChildA","armed":true},{"number":"7","uniqueID":"A253A8BE-07FC-46FC-BDE8-39832A981784","cues":[],"colorName\/live":"none","flagged":false,"listName":"GroupChildB","type":"Audio","colorName":"none","name":"GroupChildB","armed":true}],"colorName\/live":"none","flagged":false,"listName":"Group10","type":"Group","colorName":"none","name":"Group10","armed":true}],"colorName\/live":"none","flagged":false,"listName":"Main Cue List","type":"Cue List","colorName":"none","name":"Main Cue List","armed":true},{"number":"","uniqueID":"EB163B29-7249-4603-8D1D-03FCE040A292","cues":[{"number":"20","uniqueID":"F90D7E43-CA39-4DEE-B2A1-6925F69E3C12","cues":[],"colorName\/live":"none","flagged":false,"listName":"List2Audio20","type":"Audio","colorName":"none","name":"List2Audio20","armed":true}],"colorName\/live":"none","flagged":false,"listName":"Cue List","type":"Cue List","colorName":"none","name":"Cue List","armed":true}],"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cueLists"}
16:40:06.295 recv udp@53001 from=127.0.0.1:64545 /reply/connect ,s {"status":"ok","data":"ok:view","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
16:40:06.295 recv tcp0 /reply/cue/1/name ,s {"status":"ok","data":"Audio1","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
16:40:06.295 recv tcp0 /reply/cue/1/name ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:40:06.295 recv tcp0 /reply/go ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/go","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:40:06.295 recv tcp0 /reply/connect ,s {"status":"ok","data":"ok:view","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/connect"}
16:40:06.296 recv tcp0 /reply/cue/1/duration ,s {"status":"ok","data":10,"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/duration"}
16:40:06.296 recv tcp0 /reply/cue/1/preWait ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/preWait","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:40:06.296 recv tcp0 /reply/select/2 ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/select\/2","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:40:06.296 recv tcp0 /reply/playhead/2 ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/playhead","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:40:06.296 recv tcp0 /reply/stop ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/stop","status":"denied","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:40:11.673 recv udp@53001 from=127.0.0.1:55087 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/settings/network.access.osc ,
16:40:12.856 recv udp@53001 from=127.0.0.1:49443 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/settings/network.access.osc ,
16:40:18.349 recv udp@53001 from=127.0.0.1:53014 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/settings/network.access.osc ,
```

## 11. Updates

**CONFIRMED.** Summary:

- **Addresses** (exact capitalisation; `{ws}` = workspace UUID):
  - `/update/workspace/{ws}/cue_id/{cue uuid}` — **no arguments** (type tag string `,`). "This cue changed; ask for it." Sent for the cue itself, its parent cue list, and the parent group(s).
  - `/update/workspace/{ws}/cue_id/__root__` — no arguments. Sent with most cue changes (the top level / list of lists).
  - `/update/workspace/{ws}/cueList/{list uuid}/playbackPosition` — **one string argument: the UUID of the cue now at the playhead**, or **no argument at all** (`,`) when there is no cue on standby.
  - `/update/workspace/{ws}/dashboard` — no arguments; accompanies playhead moves and running-state changes.
  - No `/update/workspace/{ws}` (workspace-level) message and no `disconnect` message were seen.
- **UUID case:** always **uppercase** (`12533BF2-CD0C-4BC7-87B5-4AB68AEEE226`), in both addresses and arguments. `__root__` is literal lowercase.
- **One message per datagram** over UDP (no bundles). Each burst is usually sent twice, ~100 ms apart.
- **Where they arrive:** for a UDP subscriber, on **UDP port 53001** of the subscriber's IP, from a changing QLab source port. Never on the sender's port (0 messages on `udp@sendport` in the whole session). For a TCP subscriber, **on the same TCP connection** only; another TCP connection from the same host gets nothing, and UDP 53001 gets nothing.
- **State changes alone send `cue_id` updates:** yes — starting, pausing, resuming and finishing/stopping a cue each send `cue_id/{cue}`, `cue_id/{list}` and `cue_id/__root__` (and `dashboard` at start), with no property having changed.
- **Playhead past the last cue:** `playbackPosition` with **no arguments** (type tags `,`). `/playheadID` then answers `"none"` and `/cue/playhead/number` answers `error`.
- **Switching the displayed cue list** (clicking the other list in the sidebar and back) sent **nothing** (tested on both transports).
- **`/updates 0` stops them** (UDP and TCP). Neither `/updates 1` nor `/updates 0` replies when allowed; `/updates` replies only when `denied`.
- A UDP subscription lapses after ~60 s without UDP traffic from the subscriber (Q9). A TCP subscription lasted more than two minutes with no traffic on that connection.
- The subscription is keyed on the client **IP** (replies go to IP:53001): a subscription made by an earlier probe process (a different source port) was still delivering to port 53001 after the probe restarted (start of the Q1 capture).

### Over UDP (`/updates 1`, keep-alive `/cue/1/name` every 20 s from 16:55; keep-alive replies removed from the blocks below)

```
# 16:44:26.662 Q11-UDP subscribe
16:44:26.662 send udp /updates ,i 1
```

a) Rename cue 1 in the inspector (one message, no argument):
```
# 16:44:33.431 Q11-UDP a) rename cue 1 in inspector
16:44:33.668 recv udp@53001 from=127.0.0.1:57755 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/12533BF2-CD0C-4BC7-87B5-4AB68AEEE226 ,
```

b) GO on cue 1 and let it finish (start burst ×2, then finish burst ×2 at +10 s):
```
# 16:44:45.516 Q11-UDP b) click GO (playhead on 1), wait for finish
16:44:45.565 recv udp@53001 from=127.0.0.1:49952 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s A285E785-2AB9-4525-AF40-1091E9C40990
16:44:45.615 recv udp@53001 from=127.0.0.1:51393 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:44:45.615 recv udp@53001 from=127.0.0.1:51393 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/12533BF2-CD0C-4BC7-87B5-4AB68AEEE226 ,
16:44:45.615 recv udp@53001 from=127.0.0.1:56484 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/dashboard ,
16:44:45.615 recv udp@53001 from=127.0.0.1:51103 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
16:44:45.717 recv udp@53001 from=127.0.0.1:50172 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:44:45.717 recv udp@53001 from=127.0.0.1:59144 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/12533BF2-CD0C-4BC7-87B5-4AB68AEEE226 ,
16:44:45.717 recv udp@53001 from=127.0.0.1:57441 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
16:44:45.770 recv udp@53001 from=127.0.0.1:58860 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/A285E785-2AB9-4525-AF40-1091E9C40990 ,
16:44:45.770 recv udp@53001 from=127.0.0.1:58860 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:44:45.770 recv udp@53001 from=127.0.0.1:61403 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
16:44:55.630 recv udp@53001 from=127.0.0.1:54168 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:44:55.630 recv udp@53001 from=127.0.0.1:54168 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/12533BF2-CD0C-4BC7-87B5-4AB68AEEE226 ,
16:44:55.630 recv udp@53001 from=127.0.0.1:59552 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
16:44:55.733 recv udp@53001 from=127.0.0.1:50145 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:44:55.733 recv udp@53001 from=127.0.0.1:50145 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/12533BF2-CD0C-4BC7-87B5-4AB68AEEE226 ,
16:44:55.733 recv udp@53001 from=127.0.0.1:59142 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
```

c) GO cue 4, pause (Pause-all button) ~2 s later, resume ~2 s later, let it finish:
```
# 16:55:34.931 Q11-UDP c2) GO (cue 4), Pause-all at ~2s, Resume-all at ~4s, let finish
16:55:35.089 recv udp@53001 from=127.0.0.1:59506 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s BE801827-C647-45B5-A076-90B7EBDCD7FC
16:55:35.138 recv udp@53001 from=127.0.0.1:64557 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:55:35.138 recv udp@53001 from=127.0.0.1:64557 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
16:55:35.138 recv udp@53001 from=127.0.0.1:60896 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/dashboard ,
16:55:35.138 recv udp@53001 from=127.0.0.1:62767 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/C2A8C4FA-FE42-484F-A071-6FCB16A8FC65 ,
16:55:35.241 recv udp@53001 from=127.0.0.1:51282 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:55:35.241 recv udp@53001 from=127.0.0.1:51282 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
16:55:35.241 recv udp@53001 from=127.0.0.1:56182 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/C2A8C4FA-FE42-484F-A071-6FCB16A8FC65 ,
16:55:35.292 recv udp@53001 from=127.0.0.1:59598 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/BE801827-C647-45B5-A076-90B7EBDCD7FC ,
16:55:35.292 recv udp@53001 from=127.0.0.1:59598 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:55:35.292 recv udp@53001 from=127.0.0.1:54031 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
16:55:37.334 recv udp@53001 from=127.0.0.1:55616 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/C2A8C4FA-FE42-484F-A071-6FCB16A8FC65 ,
16:55:37.423 recv udp@53001 from=127.0.0.1:52330 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:55:37.423 recv udp@53001 from=127.0.0.1:52330 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
16:55:37.423 recv udp@53001 from=127.0.0.1:54503 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/C2A8C4FA-FE42-484F-A071-6FCB16A8FC65 ,
16:55:39.608 recv udp@53001 from=127.0.0.1:63837 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:55:39.608 recv udp@53001 from=127.0.0.1:63837 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
16:55:39.608 recv udp@53001 from=127.0.0.1:61384 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/C2A8C4FA-FE42-484F-A071-6FCB16A8FC65 ,
16:55:47.351 recv udp@53001 from=127.0.0.1:62823 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:55:47.351 recv udp@53001 from=127.0.0.1:62823 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
16:55:47.351 recv udp@53001 from=127.0.0.1:62539 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/C2A8C4FA-FE42-484F-A071-6FCB16A8FC65 ,
16:55:47.451 recv udp@53001 from=127.0.0.1:61557 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:55:47.451 recv udp@53001 from=127.0.0.1:61557 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
16:55:47.451 recv udp@53001 from=127.0.0.1:57909 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/C2A8C4FA-FE42-484F-A071-6FCB16A8FC65 ,
```

d) Move the playhead (click cue 2's row; lock on):
```
# 16:55:57.273 Q11-UDP d2) click row of cue 2 (playhead 5 -> 2, lock on)
16:55:57.323 recv udp@53001 from=127.0.0.1:53578 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s A285E785-2AB9-4525-AF40-1091E9C40990
16:55:57.374 recv udp@53001 from=127.0.0.1:55577 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/dashboard ,
```

e) Add a cue (Cues › Audio; new cue 2.5 uuid `1727ECB4-…`, inserted after the selection and taking the playhead):
```
# 16:56:04.499 Q11-UDP e) menu Cues > Audio (adds cue after selected cue 2)
16:56:04.713 recv udp@53001 from=127.0.0.1:54167 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,
16:56:04.716 recv udp@53001 from=127.0.0.1:55412 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s 1727ECB4-AEE4-438C-B245-4E7985F1C25C
16:56:04.762 recv udp@53001 from=127.0.0.1:54055 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:56:04.762 recv udp@53001 from=127.0.0.1:54055 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/dashboard ,
16:56:04.762 recv udp@53001 from=127.0.0.1:58754 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/1727ECB4-AEE4-438C-B245-4E7985F1C25C ,
16:56:04.862 recv udp@53001 from=127.0.0.1:64005 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:56:04.862 recv udp@53001 from=127.0.0.1:64005 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
16:56:04.862 recv udp@53001 from=127.0.0.1:60473 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/1727ECB4-AEE4-438C-B245-4E7985F1C25C ,
```

f) Delete that cue (Edit › Delete): the deleted cue's id is still sent:
```
# 16:56:13.883 Q11-UDP f) menu Edit > Delete (selected new cue 2.5)
16:56:13.977 recv udp@53001 from=127.0.0.1:57811 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,
16:56:13.987 recv udp@53001 from=127.0.0.1:56195 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s 3E63219F-C5E8-4312-9FEC-78EAD04D8A28
16:56:14.021 recv udp@53001 from=127.0.0.1:63954 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:56:14.021 recv udp@53001 from=127.0.0.1:55012 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
16:56:14.021 recv udp@53001 from=127.0.0.1:49404 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/dashboard ,
16:56:14.021 recv udp@53001 from=127.0.0.1:61589 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/1727ECB4-AEE4-438C-B245-4E7985F1C25C ,
16:56:14.123 recv udp@53001 from=127.0.0.1:52582 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:56:14.123 recv udp@53001 from=127.0.0.1:52582 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
```

g) Move a cue (View › Move selected cues down):
```
# 16:56:23.076 Q11-UDP g) menu View > Move selected cues down (cue 3 moves below cue 4)
16:56:23.178 recv udp@53001 from=127.0.0.1:52780 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,
16:56:23.183 recv udp@53001 from=127.0.0.1:53845 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s 3E63219F-C5E8-4312-9FEC-78EAD04D8A28
16:56:23.229 recv udp@53001 from=127.0.0.1:53250 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/dashboard ,
16:56:23.229 recv udp@53001 from=127.0.0.1:53250 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
```

h) Switch cue lists (mouse click on "Cue List", ~3 s later on "Main Cue List") — nothing arrives; a probe right after proves the subscription was alive:
```
# 17:02:54.235 (Select cue... showed nothing; Escape pressed). Q11-UDP h2) mouse click 'Cue List' in sidebar, ~3s later click 'Main Cue List'
```
```
# 17:03:13.921 probe subscription alive
17:03:13.921 send tcp1 /playhead/2 ,
17:03:13.932 recv udp@53001 from=127.0.0.1:55306 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s A285E785-2AB9-4525-AF40-1091E9C40990
17:03:13.983 recv udp@53001 from=127.0.0.1:52522 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/dashboard ,
```

Playhead past the last cue (select Group10, the last cue, then GO; group and both children run; `playbackPosition` with no argument):
```
# 17:03:20.767 Q11-UDP i) click Group10 row (last cue in Main list), then GO; wait for finish
17:03:20.823 recv udp@53001 from=127.0.0.1:53250 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s D18C05C9-6D75-4794-B573-2DA09868A4A2
17:03:20.874 recv udp@53001 from=127.0.0.1:51817 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/dashboard ,
17:03:23.099 recv udp@53001 from=127.0.0.1:50974 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,
17:03:23.144 recv udp@53001 from=127.0.0.1:62716 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/4C601614-38C0-416F-A835-E7D6262F7925 ,
17:03:23.144 recv udp@53001 from=127.0.0.1:62716 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:03:23.144 recv udp@53001 from=127.0.0.1:62514 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/D18C05C9-6D75-4794-B573-2DA09868A4A2 ,
17:03:23.144 recv udp@53001 from=127.0.0.1:56754 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
17:03:23.144 recv udp@53001 from=127.0.0.1:52218 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/A253A8BE-07FC-46FC-BDE8-39832A981784 ,
17:03:23.144 recv udp@53001 from=127.0.0.1:55184 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/dashboard ,
17:03:23.246 recv udp@53001 from=127.0.0.1:57876 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:03:23.246 recv udp@53001 from=127.0.0.1:57876 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
17:03:23.246 recv udp@53001 from=127.0.0.1:62349 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/A253A8BE-07FC-46FC-BDE8-39832A981784 ,
17:03:23.246 recv udp@53001 from=127.0.0.1:58578 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/D18C05C9-6D75-4794-B573-2DA09868A4A2 ,
17:03:23.246 recv udp@53001 from=127.0.0.1:63853 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/4C601614-38C0-416F-A835-E7D6262F7925 ,
17:03:33.171 recv udp@53001 from=127.0.0.1:65004 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/4C601614-38C0-416F-A835-E7D6262F7925 ,
17:03:33.172 recv udp@53001 from=127.0.0.1:65004 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:03:33.172 recv udp@53001 from=127.0.0.1:53356 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/D18C05C9-6D75-4794-B573-2DA09868A4A2 ,
17:03:33.172 recv udp@53001 from=127.0.0.1:59243 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
17:03:33.172 recv udp@53001 from=127.0.0.1:55195 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/A253A8BE-07FC-46FC-BDE8-39832A981784 ,
17:03:33.274 recv udp@53001 from=127.0.0.1:63716 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:03:33.274 recv udp@53001 from=127.0.0.1:50775 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
17:03:33.274 recv udp@53001 from=127.0.0.1:59707 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/A253A8BE-07FC-46FC-BDE8-39832A981784 ,
17:03:33.274 recv udp@53001 from=127.0.0.1:50803 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/D18C05C9-6D75-4794-B573-2DA09868A4A2 ,
17:03:40.443 send udp /playheadID ,
17:03:40.443 send udp /cue/playhead/number ,
17:03:40.444 recv udp@53001 from=127.0.0.1:62270 /reply/playheadID ,s {"status":"ok","data":"none","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/playheadID"}
17:03:40.444 recv udp@53001 from=127.0.0.1:57929 /reply/cue/playhead/number ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/playhead\/number","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
```

`/updates 0`: nothing more arrives:
```
# 17:03:46.852 Q11-UDP /updates 0 then tcp1 playhead moves
17:03:46.852 send udp /updates ,i 0
17:03:47.397 send tcp1 /playhead/2 ,
17:03:48.043 send tcp1 /playhead/4 ,
```

An earlier attempt at (c) (16:45) also shows pause/resume, but its finish update is missing because the UDP subscription had expired (that is how the ~60 s expiry was found):
```
# 16:45:15.972 Q11-UDP c) GO (cue 2) then ~2s Pause-all button then ~2s Resume-all button
16:45:16.129 recv udp@53001 from=127.0.0.1:53734 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s 3E63219F-C5E8-4312-9FEC-78EAD04D8A28
16:45:16.176 recv udp@53001 from=127.0.0.1:65419 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/A285E785-2AB9-4525-AF40-1091E9C40990 ,
16:45:16.176 recv udp@53001 from=127.0.0.1:65438 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:45:16.176 recv udp@53001 from=127.0.0.1:54819 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/dashboard ,
16:45:16.176 recv udp@53001 from=127.0.0.1:58114 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
16:45:16.279 recv udp@53001 from=127.0.0.1:54499 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/A285E785-2AB9-4525-AF40-1091E9C40990 ,
16:45:16.279 recv udp@53001 from=127.0.0.1:54499 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:45:16.279 recv udp@53001 from=127.0.0.1:61337 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
16:45:16.335 recv udp@53001 from=127.0.0.1:52211 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/3E63219F-C5E8-4312-9FEC-78EAD04D8A28 ,
16:45:16.335 recv udp@53001 from=127.0.0.1:52211 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:45:16.335 recv udp@53001 from=127.0.0.1:50612 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
16:45:18.383 recv udp@53001 from=127.0.0.1:61289 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/A285E785-2AB9-4525-AF40-1091E9C40990 ,
16:45:18.475 recv udp@53001 from=127.0.0.1:51846 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/A285E785-2AB9-4525-AF40-1091E9C40990 ,
16:45:18.475 recv udp@53001 from=127.0.0.1:51846 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:45:18.475 recv udp@53001 from=127.0.0.1:50867 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
16:45:20.725 recv udp@53001 from=127.0.0.1:61624 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/A285E785-2AB9-4525-AF40-1091E9C40990 ,
16:45:20.725 recv udp@53001 from=127.0.0.1:61624 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
16:45:20.725 recv udp@53001 from=127.0.0.1:61624 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
```

### Over TCP (`/updates 1` on `tcp0`, no keep-alive)

```
# 17:04:15.043 Q11-TCP subscribe on tcp0 (no keepalive)
17:04:15.043 send tcp0 /updates ,i 1
```
```
# 17:04:16.624 Q11-TCP a) rename cue 2 in inspector (selected via click)
17:04:20.140 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/A285E785-2AB9-4525-AF40-1091E9C40990 ,
```
```
# 17:04:31.228 Q11-TCP b) GO (playhead on 2), let finish
17:04:31.281 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s C2A8C4FA-FE42-484F-A071-6FCB16A8FC65
17:04:31.332 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/A285E785-2AB9-4525-AF40-1091E9C40990 ,
17:04:31.332 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:04:31.332 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
17:04:31.332 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/dashboard ,
17:04:31.432 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/A285E785-2AB9-4525-AF40-1091E9C40990 ,
17:04:31.432 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:04:31.432 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
17:04:31.485 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:04:31.485 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
17:04:31.485 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/C2A8C4FA-FE42-484F-A071-6FCB16A8FC65 ,
17:04:41.356 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/A285E785-2AB9-4525-AF40-1091E9C40990 ,
17:04:41.356 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:04:41.356 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
17:04:41.457 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/A285E785-2AB9-4525-AF40-1091E9C40990 ,
17:04:41.457 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:04:41.457 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
```
```
# 17:04:43.335 Q11-TCP c) GO (cue 4), Pause-all at ~2s, Resume-all at ~4s, let finish
17:04:43.383 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s 3E63219F-C5E8-4312-9FEC-78EAD04D8A28
17:04:43.432 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:04:43.432 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
17:04:43.432 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/dashboard ,
17:04:43.432 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/C2A8C4FA-FE42-484F-A071-6FCB16A8FC65 ,
17:04:43.534 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:04:43.534 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
17:04:43.534 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/C2A8C4FA-FE42-484F-A071-6FCB16A8FC65 ,
17:04:43.585 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:04:43.585 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/3E63219F-C5E8-4312-9FEC-78EAD04D8A28 ,
17:04:43.585 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
17:04:45.623 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/C2A8C4FA-FE42-484F-A071-6FCB16A8FC65 ,
17:04:45.719 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:04:45.719 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
17:04:45.719 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/C2A8C4FA-FE42-484F-A071-6FCB16A8FC65 ,
17:04:47.909 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:04:47.909 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
17:04:47.909 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/C2A8C4FA-FE42-484F-A071-6FCB16A8FC65 ,
17:04:55.642 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:04:55.642 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
17:04:55.643 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/C2A8C4FA-FE42-484F-A071-6FCB16A8FC65 ,
17:04:55.744 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:04:55.744 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
17:04:55.744 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/C2A8C4FA-FE42-484F-A071-6FCB16A8FC65 ,
```
```
# 17:05:04.147 Q11-TCP d) click row of cue 5 (playhead 3 -> 5)
17:05:04.194 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s BE801827-C647-45B5-A076-90B7EBDCD7FC
17:05:04.245 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/dashboard ,
```
```
# 17:05:12.054 Q11-TCP e) menu Cues > Audio (adds after selected cue 5)
17:05:12.463 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s EF44E901-9B29-4CC9-9ADF-4474F6358383
17:05:12.506 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:05:12.506 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/dashboard ,
17:05:12.506 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/EF44E901-9B29-4CC9-9ADF-4474F6358383 ,
17:05:12.605 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:05:12.605 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
17:05:12.605 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/EF44E901-9B29-4CC9-9ADF-4474F6358383 ,
```
```
# 17:05:25.773 Q11-TCP f) menu Edit > Delete (new cue 8)
17:05:26.162 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,
17:05:26.166 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s D18C05C9-6D75-4794-B573-2DA09868A4A2
17:05:26.210 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:05:26.210 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
17:05:26.210 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/dashboard ,
17:05:26.210 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/EF44E901-9B29-4CC9-9ADF-4474F6358383 ,
17:05:26.311 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:05:26.311 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
```
```
# 17:05:34.547 Q11-TCP g) menu View > Move selected cues up (Group10 moves above cue 5)
17:05:34.937 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,
17:05:34.941 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s D18C05C9-6D75-4794-B573-2DA09868A4A2
17:05:34.989 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/dashboard ,
17:05:34.989 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
```
```
# 17:05:43.229 Q11-TCP h) click 'Cue List' in sidebar, ~3s later click 'Main Cue List'
```
```
# 17:05:56.699 Q11-TCP i) tcp1 /playhead/5 (last cue) then tcp1 /go, wait finish
17:05:56.699 send tcp1 /playhead/5 ,
17:05:56.712 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,s BE801827-C647-45B5-A076-90B7EBDCD7FC
17:05:56.764 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/dashboard ,
17:05:57.703 send tcp1 /go ,
17:05:57.716 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cueList/9AA46B16-4874-42E9-8E1E-F62CBB8CA139/playbackPosition ,
17:05:57.764 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/BE801827-C647-45B5-A076-90B7EBDCD7FC ,
17:05:57.764 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:05:57.764 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
17:05:57.764 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/dashboard ,
17:05:57.865 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/BE801827-C647-45B5-A076-90B7EBDCD7FC ,
17:05:57.865 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:05:57.865 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
17:06:07.777 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/BE801827-C647-45B5-A076-90B7EBDCD7FC ,
17:06:07.778 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:06:07.778 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
17:06:07.881 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/BE801827-C647-45B5-A076-90B7EBDCD7FC ,
17:06:07.881 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/9AA46B16-4874-42E9-8E1E-F62CBB8CA139 ,
17:06:07.881 recv tcp0 /update/workspace/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6/cue_id/__root__ ,
```
```
# 17:06:09.210 Q11-TCP j) /updates 0 on tcp0, then tcp1 moves playhead
17:06:09.211 send tcp0 /updates ,i 0
17:06:09.719 send tcp1 /playhead/2 ,
```

## 12. Limits

**CONFIRMED.**

- **TCP connections:** QLab accepted **1000 simultaneous TCP connections** and answered `/version` on every one (10, 100, 500 and 1000 tried; 1000 in 0.17 s). I stopped there rather than risk destabilising QLab, so the real ceiling is above 1000 (or there is none).
- **Long strings:** a 10 KB (10 240-char) cue name was accepted over UDP and TCP; a 60 000-char name over UDP; a 100 KB (102 400-char) name over TCP. No error reply; reading the name back returned the whole string, **including over UDP** (QLab sent a 60 180-byte reply datagram to port 53001). A 100 KB string cannot be sent in one UDP datagram at all (IPv4 limit ~65 507 bytes), so that write was TCP-only; and reading that 100 KB name back over UDP got **no reply at all** (the reply could not fit in a datagram) — silence, not an error. Note for clients on macOS: outgoing datagrams above 9 216 bytes fail with `EMSGSIZE` unless the socket's send buffer is raised (`net.inet.udp.maxdgram`); my first 10 KB UDP attempt failed locally for that reason and never reached QLab.

```
$ python3 tcpmany.py 10 / 100 / 500 / 1000   (ulimit -n 4096; opens N connections, then sends /version on each and reads one reply per connection)
opened 10 connections; first failure: None
/version replies ok=10 failed=0 in 0.00s
opened 100 connections; first failure: None
/version replies ok=100 failed=0 in 0.00s
opened 500 connections; first failure: None
/version replies ok=500 failed=0 in 0.04s
opened 1000 connections; first failure: None
/version replies ok=1000 failed=0 in 0.17s
```
```
# 16:43:25.302 Q12 10KB tcp
16:43:25.302 send tcp0 /cue/2/name ,s <str len=10240 head='xxxx…(40 chars)'>
16:43:26.366 send tcp0 /cue/2/name ,
16:43:26.367 recv tcp0 /reply/cue/2/name ,s {"status":"ok","data":"xxxx…(10240 chars)","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/2\/name"}
```
```
# 16:43:27.434 Q12 100KB tcp
16:43:27.435 send tcp0 /cue/3/name ,s <str len=102400 head='xxxx…(40 chars)'>
16:43:28.496 send tcp0 /cue/3/name ,
16:43:28.501 recv tcp0 /reply/cue/3/name ,s {"status":"ok","data":"xxxx…(102400 chars)","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/name"}
16:43:29.567 send udp /cue/3/name ,
```
```
# 16:43:51.528 Q12 10KB udp (SO_SNDBUF raised)
16:43:51.528 send udp /cue/1/name ,s <str len=10240 head='xxxx…(40 chars)'>
16:43:52.574 send udp /cue/1/name ,
16:43:52.575 recv udp@53001 from=127.0.0.1:60111 /reply/cue/1/name ,s {"status":"ok","data":"xxxx…(10240 chars)","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
16:43:53.622 send tcp0 /cue/1/name ,
16:43:53.623 recv tcp0 /reply/cue/1/name ,s {"status":"ok","data":"xxxx…(10240 chars)","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
```
```
# 16:43:54.664 Q12 60000B udp
16:43:54.665 send udp /cue/4/name ,s <str len=60000 head='xxxx…(40 chars)'>
16:43:55.712 send tcp0 /cue/4/name ,
16:43:55.716 recv tcp0 /reply/cue/4/name ,s {"status":"ok","data":"xxxx…(60000 chars)","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/4\/name"}
16:43:56.756 send udp /cue/4/name ,
16:43:56.758 recv udp@53001 from=127.0.0.1:57969 /reply/cue/4/name ,s {"status":"ok","data":"xxxx…(60000 chars)","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/4\/name"}
16:43:57.800 send udp /cue/2/name ,
16:43:57.801 recv udp@53001 from=127.0.0.1:58608 /reply/cue/2/name ,s {"status":"ok","data":"xxxx…(10240 chars)","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/2\/name"}
```

## 13. Which cue list does `/cue/{number}` use?

**CONFIRMED: all lists.** Cue numbers are unique in the workspace, and `/cue/{number}` finds a cue in any list, whichever list is displayed: with list 2 displayed, `/cue/1/name` and `/cue/1/start` work on the main list's cue 1; with the main list displayed, `/cue/20/…` reaches list 2's cue. (Wildcards also cover all lists, Q5.) By contrast **`/go` acts on the displayed list's playhead**: with list 2 displayed, `/go` started cue 20.

```
# Q13 list 2 displayed
16:42:59.130 send udp /cue/1/name ,
16:42:59.130 send udp /cue/20/name ,
16:42:59.130 send udp /cue/1/start ,
16:42:59.132 recv udp@53001 from=127.0.0.1:57052 /reply/cue/1/name ,s {"status":"ok","data":"Audio1","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
16:42:59.132 recv udp@53001 from=127.0.0.1:61086 /reply/cue/20/name ,s {"status":"ok","data":"List2Audio20","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/20\/name"}
16:42:59.775 send udp /runningCues ,
16:42:59.776 recv /reply/runningCues -> running cue numbers ['1']
16:43:00.319 send udp /stop ,
# Q13 /go while list 2 displayed, list2 playhead on 20
16:43:00.319 send udp /go ,
16:43:00.863 send udp /runningCues ,
16:43:00.864 recv /reply/runningCues -> running cue numbers ['20']
16:43:01.403 send udp /stop ,
16:43:01.403 send udp /select/1 ,
# Q13 list 1 displayed
16:43:01.403 send udp /cue/20/name ,
16:43:01.403 send udp /cue/20/start ,
16:43:01.416 recv udp@53001 from=127.0.0.1:58058 /reply/cue/20/name ,s {"status":"ok","data":"List2Audio20","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/20\/name"}
16:43:01.946 send udp /runningCues ,
16:43:01.947 recv /reply/runningCues -> running cue numbers ['20']
16:43:02.491 send udp /stop ,
```

## 14. `/cue/active`, `/cue/selected`, `/cue/playhead`, `/runningCues`, `/cueLists`

**CONFIRMED.**

- `/cue/selected/{property}` and `/cue/playhead/{property}` exist: they act on the selected cue / the cue at the playhead **of the displayed list**, and reply with the JSON `address` rewritten to the real cue number (`/workspace/{id}/cue/3/number`). With nothing selected / no playhead → `error`.
- `/cue/active/{property}` exists: one reply **per running cue**, each with its real address; none running → `error`.
- The bare forms `/cue/active`, `/cue/selected`, `/cue/playhead` (no property) → `error`.
- `/runningCues` → `ok` with `data` an array of cue objects (`number`, `uniqueID`, `cues`, `colorName/live`, `flagged`, `listName`, `type`, `colorName`, `name`, `armed`); `[]` when nothing runs. `/runningOrPausedCues` and `/selectedCues` have the same shape.
- `/cueLists` → `ok` with an array of cue-list objects of the same shape (`type: "Cue List"`, `number: ""`), each with its cues nested under `cues` (groups nest their children).
- `/playheadID` returns the playhead cue's UUID (or `"none"`); `/playheadId` (lowercase d) still works but adds `"deprecated": true` and a `warning`.
- `/cue_id/{UUID}/{property}` works with the uppercase UUID; lowercase → `error`.

```
# 16:41:21.531 Q14 nothing running
16:41:21.531 send udp /cue/active/name ,
16:41:21.531 send udp /cue/active ,
16:41:21.531 send udp /cue/selected ,
16:41:21.531 send udp /cue/playhead ,
16:41:21.531 send udp /playheadID ,
16:41:21.531 send udp /runningOrPausedCues ,
16:41:21.532 recv udp@53001 from=127.0.0.1:59042 /reply/cue/active/name ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/active\/name","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:41:21.532 recv udp@53001 from=127.0.0.1:49338 /reply/cue/active ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/active","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:41:21.532 recv udp@53001 from=127.0.0.1:51404 /reply/cue/selected ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/selected","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:41:21.532 recv udp@53001 from=127.0.0.1:58570 /reply/cue/playhead ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/playhead","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:41:21.532 recv udp@53001 from=127.0.0.1:57554 /reply/playheadID ,s {"status":"ok","data":"BE801827-C647-45B5-A076-90B7EBDCD7FC","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/playheadID"}
16:41:21.532 recv udp@53001 from=127.0.0.1:62908 /reply/runningOrPausedCues ,s {"status":"ok","data":[],"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/runningOrPausedCues"}
```
```
# 16:41:22.178 Q14 cue 1 and 2 running
16:41:22.178 send udp /cue/1/start ,
16:41:22.178 send udp /cue/2/start ,
16:41:22.519 send udp /cue/active/name ,
16:41:22.520 send udp /cue/active/number ,
16:41:22.520 send udp /cue/active ,
16:41:22.520 send udp /runningCues ,
16:41:22.520 send udp /cue_id/12533BF2-CD0C-4BC7-87B5-4AB68AEEE226/name ,
16:41:22.520 send udp /cue_id/12533bf2-cd0c-4bc7-87b5-4ab68aeee226/name ,
16:41:22.520 recv udp@53001 from=127.0.0.1:61670 /reply/cue/active/name ,s {"status":"ok","data":"Audio1","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
16:41:22.520 recv udp@53001 from=127.0.0.1:61670 /reply/cue/active/name ,s {"status":"ok","data":"Audio2","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/2\/name"}
16:41:22.521 recv udp@53001 from=127.0.0.1:52082 /reply/cue/active/number ,s {"status":"ok","data":"1","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/number"}
16:41:22.521 recv udp@53001 from=127.0.0.1:52082 /reply/cue/active/number ,s {"status":"ok","data":"2","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/2\/number"}
16:41:22.521 recv udp@53001 from=127.0.0.1:49712 /reply/cue/active ,s {"address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/active","status":"error","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6"}
16:41:22.521 recv udp@53001 from=127.0.0.1:57681 /reply/runningCues ,s {"status":"ok","data":[{"number":"1","uniqueID":"12533BF2-CD0C-4BC7-87B5-4AB68AEEE226","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio1","type":"Audio","colorName":"none","name":"Audio1","armed":true},{"number":"2","uniqueID":"A285E785-2AB9-4525-AF40-1091E9C40990","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio2","type":"Audio","colorName":"none","name":"Audio2","armed":true}],"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/runningCues"}
…
```

```
16:38:10.759 send udp /cueLists ,
16:38:10.760 recv udp@53001 from=127.0.0.1:60990 /reply/cueLists ,s {"status":"ok","data":[{"number":"","uniqueID":"9AA46B16-4874-42E9-8E1E-F62CBB8CA139","cues":[{"number":"1","uniqueID":"12533BF2-CD0C-4BC7-87B5-4AB68AEEE226","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio1","type":"Audio","colorName":"none","name":"Audio1","armed":true},{"number":"2","uniqueID":"A285E785-2AB9-4525-AF40-1091E9C40990","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio2","type":"Audio","colorName":"none","name":"Audio2","armed":true},{"number":"3","uniqueID":"3E63219F-C5E8-4312-9FEC-78EAD04D8A28","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio3","type":"Audio","colorName":"none","name":"Audio3","armed":true},{"number":"4","uniqueID":"C2A8C4FA-FE42-484F-A071-6FCB16A8FC65","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio4","type":"Audio","colorName":"none","name":"Audio4","armed":true},{"number":"5","uniqueID":"BE801827-C647-45B5-A076-90B7EBDCD7FC","cues":[],"colorName\/live":"none","flagged":false,"listName":"Audio5","type":"Audio","colorName":"none","name":"Audio5","armed":true},{"number":"10","uniqueID":"D18C05C9-6D75-4794-B573-2DA09868A4A2","cues":[{"number":"6","uniqueID":"4C601614-38C0-416F-A835-E7D6262F7925","cues":[],"colorName\/live":"none","flagged":false,"listName":"GroupChildA","type":"Audio","colorName":"none","name":"GroupChildA","armed":true},{"number":"7","uniqueID":"A253A8BE-07FC-46FC-BDE8-39832A981784","cues":[],"colorName\/live":"none","flagged":false,"listName":"GroupChildB","type":"Audio","colorName":"none","name":"GroupChildB","armed":true}],"colorName\/live":"none","flagged":false,"listName":"Group10","type":"Group","colorName":"none","name":"Group10","armed":true}],"colorName\/live":"none","flagged":false,"listName":"Main Cue List","type":"Cue List","colorName":"none","name":"Main Cue List","armed":true},{"number":"","uniqueID":"EB163B29-7249-4603-8D1D-03FCE040A292","cues":[{"number":"20","uniqueID":"F90D7E43-CA39-4DEE-B2A1-6925F69E3C12","cues":[],"colorName\/live":"none","flagged":false,"listName":"List2Audio20","type":"Audio","colorName":"none","name":"List2Audio20","armed":true}],"colorName\/live":"none","flagged":false,"listName":"Cue List","type":"Cue List","colorName":"none","name":"Cue List","armed":true}],"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cueLists"}
```

---

## Extra finding: identical messages within ~0.1 s are dropped

**CONFIRMED.** QLab silently ignores a message that is identical (same address and arguments) to one it received less than about 100 ms earlier, over UDP and TCP — no action and no reply. Different messages at the same rate are all handled, and identical ones 120 ms apart are all handled. This also applies to `/go`: two `/go` 50 ms apart started only one cue. (The workspace's "minimum time between GOs" was 0, so this is not the double-GO setting.)

```
# 17:08:17.731 dedupe test: different queries on tcp5 every 0.05s
17:08:17.783 send tcp5 /cue/1/name ,
17:08:17.784 recv tcp5 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
17:08:17.841 send tcp5 /cue/2/name ,
17:08:17.842 recv tcp5 /reply/cue/2/name ,s {"status":"ok","data":"RenamedTwo","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/2\/name"}
17:08:17.899 send tcp5 /cue/3/name ,
17:08:17.900 recv tcp5 /reply/cue/3/name ,s {"status":"ok","data":"Audio3","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/3\/name"}
17:08:17.958 send tcp5 /cue/4/name ,
17:08:17.958 recv tcp5 /reply/cue/4/name ,s {"status":"ok","data":"Audio4","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/4\/name"}
17:08:18.012 send tcp5 /cue/5/name ,
17:08:18.013 recv tcp5 /reply/cue/5/name ,s {"status":"ok","data":"Audio5","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/5\/name"}
17:08:18.070 send tcp5 /cue/1/name ,
17:08:18.071 recv tcp5 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
```
```
# 17:08:18.513 same test over UDP
17:08:18.567 send udp /cue/1/name ,
17:08:18.568 recv udp@53001 from=127.0.0.1:55208 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
17:08:18.624 send udp /cue/1/name ,
17:08:18.682 send udp /cue/1/name ,
17:08:18.683 recv udp@53001 from=127.0.0.1:59722 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
17:08:18.740 send udp /cue/1/name ,
```
```
# 17:08:19.186 same /cue/1/name on tcp5 every 0.12s
17:08:19.314 send tcp5 /cue/1/name ,
17:08:19.315 recv tcp5 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
17:08:19.441 send tcp5 /cue/1/name ,
17:08:19.443 recv tcp5 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
17:08:19.565 send tcp5 /cue/1/name ,
17:08:19.566 recv tcp5 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
17:08:19.693 send tcp5 /cue/1/name ,
17:08:19.694 recv tcp5 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
```
```
# 17:08:30.030 dedupe: 3 identical /cue/1/name in one burst (0ms)
17:08:30.030 send tcp5 /cue/1/name ,
17:08:30.030 send tcp5 /cue/1/name ,
17:08:30.030 send tcp5 /cue/1/name ,
17:08:30.031 recv tcp5 /reply/cue/1/name ,s {"status":"ok","data":"RenamedOne","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/cue\/1\/name"}
```
```
# 17:08:31.338 dedupe: /playhead/1 then /go twice 0.05s apart
17:08:31.338 send tcp5 /playhead/1 ,
17:08:31.642 send tcp5 /go ,
17:08:31.697 send tcp5 /go ,
17:08:32.102 send tcp5 /runningCues ,
17:08:32.102 recv tcp5 /reply/runningCues ,s {"status":"ok","data":[{"number":"1","uniqueID":"12533BF2-CD0C-4BC7-87B5-4AB68AEEE226","cues":[],"colorName\/live":"none","flagged":false,"listName":"A","type":"Audio","colorName":"none","name":"A","armed":true}],"workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/runningCues"}
17:08:32.155 send tcp5 /playheadID ,
17:08:32.156 recv tcp5 /reply/playheadID ,s {"status":"ok","data":"A285E785-2AB9-4525-AF40-1091E9C40990","workspace_id":"243ACE2D-31F6-4615-87B6-BC26ADAA7BF6","address":"\/workspace\/243ACE2D-31F6-4615-87B6-BC26ADAA7BF6\/playheadID"}
17:08:32.693 send tcp5 /hardStop ,
17:08:32.693 send tcp5 /cue/1/name ,s RenamedOne
```

## Could not test, or only partly

- **Q3:** how long the wrong-passcode counter persists, and whether the UDP back-off is per IP (only TCP connections were tested in detail).
- **Q9:** the 15-minute silence probe was not run (stopped by hand): expiry was already established between 58 s and 62 s, and 70 s and 5 min both showed it. TCP connections were not probed for an idle timeout beyond ~2 minutes.
- **Q10:** properties other than name, notes and pre-wait were not checked against the Edit/Control split; nor were application-level messages such as `/new`, `/save` or `/udpReplyPort`.
- **Q12:** more than 1000 TCP connections (stopped to avoid destabilising QLab); 100 KB over UDP is impossible by IPv4 datagram size.
- **Q11 (h):** switching lists was done with the sidebar only. Background (no-focus) clicks on the sidebar did not switch lists; real mouse clicks did. Other ways of switching (cue-list tabs, `/select` into another list) were not captured for updates.
- **`/udpReplyPort`** (mentioned on the OSC Access page) was not tested; all UDP replies went to 53001.
- Behaviour with **two workspaces open** (which one an un-prefixed address reaches) was not tested; only one workspace was open.
- **Bundles sent to QLab** were not tested (the helper can encode them); QLab itself never sent a bundle.
- `/cue/{n}/stop` "fade": with default settings it took ~0.1 s; I did not check whether a cue's own fade-out settings change that.
