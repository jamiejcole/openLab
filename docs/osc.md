# Controlling openLab with OSC

openLab can be driven by OSC (Open Sound Control) from a lighting desk, a tablet remote, a
Stream Deck plug-in, a script or another show computer. It answers the same addresses QLab 5
uses for the features openLab has, so most remotes set up for QLab work without changes. This
page lists everything openLab answers, how to connect, and where it behaves differently from
QLab 5.

Anything not listed here is not implemented: openLab answers it with an `error` reply (see
[Replies](#replies)) and does nothing.

## Connecting

### Ports

| What | Where |
|---|---|
| Messages to openLab | UDP or TCP port **53000**, on every IPv4 network interface of the show computer |
| Replies and updates over UDP | the sender's IP address, port **53001** (whatever port it sent from), unless it asked for another with `/udpReplyPort` |
| Replies and updates over TCP | back on the same connection |

- **UDP**: one OSC message or bundle per datagram, at most 64 KiB.
- **TCP**: OSC 1.1 framing (SLIP: each packet starts and ends with the byte `0xC0`). The older
  OSC 1.0 framing with a length in front of each packet is not supported; choose "SLIP" or
  "OSC 1.1" in your remote's settings.
- Bundles are taken apart and their messages run at once, in order. A bundle's time tag is
  ignored.
- If port 53000 is taken by another program when openLab starts, OSC is off for that session and
  the show log says why. Quit the other program and restart openLab.

### Passcodes and rights

Each workspace has its own passcode table in **Workspace Settings > OSC**. A row is a four-digit
passcode and the rights it gives:

| Right | Lets a remote |
|---|---|
| View | read cue properties and the cue lists, receive updates, edit cue notes, set its reply port |
| Edit | change cue names, numbers, waits, durations, load times, armed and flagged, and move the selection |
| Control | GO, stop, pause, panic, reset, move the playhead, and start, stop or load single cues |

Edit and Control need View. The **No Passcode** row says what a remote may do before it sends a
passcode. A new workspace has one random passcode with every right and nothing without a
passcode, so a remote must connect first.

To connect, send `/connect` with the passcode **as a string**:

```
/connect "4821"
```

The reply says what you may do, for example `ok:view|edit|control`, or `ok:` alone for no
rights. A wrong passcode answers `badpass`. A passcode sent as a number is never accepted (a
leading zero would be lost), so check your remote sends a string. `/connect` with no argument
uses the No Passcode rights.

A remote without the right for a message gets a `denied` reply and nothing happens.

**A connection lasts while the remote keeps talking.** Over UDP it ends after **60 seconds** in
which openLab heard nothing from that IP address; any message renews it. A remote that only
listens for updates should send something (for example `/updates 1`) at least once a minute, or
use TCP, where the connection lasts until it is closed. When it has ended, the remote gets
`denied` until it sends `/connect` again.

Connecting to one open workspace leaves the others locked: a passcode belongs to its workspace.
An `/connect` without a `/workspace/{id}` prefix is tried in every open workspace.

### Wrong passcodes

- After **5** wrong passcodes from one IP address, `/connect` from that address answers `denied`
  for **30 seconds** without checking the passcode. Wait 30 seconds without guessing to try again.
  Reconnecting over TCP does not reset the count.
- If **50** wrong passcodes arrive from anywhere within a minute, every new `/connect` is refused
  until the minute is over. A remote that already connected from that IP address before can
  still connect with the right passcode.
- A wrong `/connect` from a remote that is already connected never disconnects it or lowers its
  rights.

Passcodes travel unencrypted. On a network you do not control, give the No Passcode row no
rights and keep each passcode's rights as narrow as the remote needs.

## Which workspace a message reaches

- Without a prefix, a message goes to **every open workspace**: `/go` fires GO in each one.
  Questions get one reply per workspace.
- With `/workspace/{id}/` in front (for example `/workspace/{id}/go`) it goes to one workspace.
  `{id}` is the workspace's id in upper case, as `/workspaces` and every reply give it, or its
  name exactly as written. A name two open workspaces share is refused (use the id).
- The application messages (`/connect` aside) work only without a prefix: `/version`,
  `/workspaces`, `/updates`, `/udpReplyPort`.

`/go`, `/playhead/...` and `/select/...` act on the **active cue list**: the list whose playhead
moved last. It starts as the first list.

### Naming a cue

| Form | Means |
|---|---|
| `/cue/{number}/...` | the cue with that number, in any list, inside groups too. Numbers are case-sensitive |
| `/cue_id/{id}/...` | the cue with that unique id, in upper case (`/cue/{number}/uniqueID` reads it) |
| `/cue/selected/...` | the cue at the active list's playhead (openLab's selection follows the playhead). Reads only |
| `/cue/playhead/...` | the same |
| `/cue/active/...` | every running cue, one reply each. Reads only |

A cue number may be a **pattern**: `*` any run of characters, `?` one character, `[abc]` or
`[a-c]` one of these, `[!a-c]` none of these, `{1,5,9}` one of the alternatives. The message then
acts on, or answers for, every cue whose number matches: `/cue/1*/stop` stops cues 1, 10, 1.5 and
so on, and `/cue/*/name` returns every cue's name. At most 1024 cues are matched by one message.

Addresses are case-sensitive: `/GO` and `/cue/1/Name` are not answered.

## Replies

A message that works gets **no reply** (actions, edits, `/updates`). A read, a query, `/connect`,
and anything that fails or is refused gets one reply, sent to `/reply` followed by the address you
sent, with one string argument holding JSON:

```
/reply/cue/5/name  {"status":"ok","address":"/workspace/{id}/cue/5/name","workspace_id":"{id}","data":"Thunder"}
```

- `status` is `ok`, `error` (no such cue, a wrong argument, an address openLab does not answer, a
  value out of range) or `denied` (no right, or locked out).
- `address` is the full address with the workspace prefix; for a pattern or `selected`,
  `playhead` and `active` it is the matched cue's own address.
- `workspace_id` names the workspace that answered. `data` is there only when there is a value.
- A reply too big for one UDP datagram is replaced by an `error`.

## Addresses

In the tables, `{number}` is a cue number (or pattern) and arguments are OSC types: string, int,
float. "Right" is what a remote needs (see [Passcodes and rights](#passcodes-and-rights)).

### Application

| Address | Arguments | Right | Does |
|---|---|---|---|
| `/connect` | passcode string, or none | none | connects with that passcode's rights; replies `ok:...`, `badpass` or `denied` |
| `/workspace/{id}/connect` | passcode string, or none | none | the same for one workspace |
| `/version` | none | none | replies openLab's version |
| `/workspaces` | none | none | replies a list of the open workspaces: `displayName`, `uniqueID`, `port`, `udpReplyPort`, `version` |
| `/updates` | 1 or true to subscribe, 0 or false to stop | none | starts or stops [updates](#updates) to this remote. No reply |
| `/udpReplyPort` | port, int 1 to 65535 | View | sends this remote's UDP replies and updates to that port instead of 53001. Ignored over TCP |

### The whole workspace

| Address | Arguments | Right | Does |
|---|---|---|---|
| `/go` | none, or a cue number string | Control | GO on the active list. With a number: moves the playhead to that cue, then GO |
| `/stop` | none | Control | stops every cue at once |
| `/hardStop` | none | Control | the same as `/stop` |
| `/panic` | none | Control | fades every cue out over the workspace's panic time; a second panic stops at once |
| `/pause` | none | Control | pauses every running cue |
| `/resume` | none | Control | resumes every paused cue |
| `/reset` | none | Control | stops everything, resets every cue and puts every list's playhead on its first cue |
| `/playhead/next` | none | Control | moves the active list's playhead to the next cue |
| `/playhead/previous` | none | Control | moves it to the previous cue |
| `/playhead/{number}` | none | Control | puts the playhead on that cue (a cue at the top of a list, not inside a group); its list becomes active |
| `/select/next` | none | Edit | the same as `/playhead/next` |
| `/select/previous` | none | Edit | the same as `/playhead/previous` |
| `/select/{number}` | none | Edit | the same as `/playhead/{number}` |

### Questions about the workspace

All need View. Each replies with `data` as shown.

| Address | Replies |
|---|---|
| `/runningCues` | a list of the cues running now (waiting, playing, fading; not paused) |
| `/runningOrPausedCues` | the same, with the paused cues |
| `/selectedCues` | the cue at the active playhead, in a list (empty when there is none) |
| `/cueLists` | every cue list with its cues, groups nested |
| `/playheadID` | the unique id of the cue at the active playhead, or `"none"` |
| `/playheadId` | the same, with a note that the spelling is old: use `/playheadID` |

A cue in these lists has `number`, `uniqueID`, `name`, `listName`, `type`, `colorName`,
`colorName/live`, `flagged`, `armed` and, for a group, its `cues`.

### Acting on a cue

All need Control and take no argument unless shown.

| Address | Does |
|---|---|
| `/cue/{number}/start` | starts the cue (a paused cue resumes) |
| `/cue/{number}/stop` | stops it at once |
| `/cue/{number}/hardStop` | the same |
| `/cue/{number}/panic` | fades the cue (and a group's cues) out over the panic time |
| `/cue/{number}/pause` | pauses it |
| `/cue/{number}/resume` | resumes it |
| `/cue/{number}/load` | loads it ready to start; with a number of seconds (0 or more), loads it that far in |
| `/cue/{number}/reset` | stops it and puts it back to how it was before it ran |
| `/cue/{number}/preview` | plays it on its own without moving the playhead (a Network cue sends its message) |

### Cue properties

Send the address with **no argument to read** the value; with one argument to change it. Reading
needs View; changing needs Edit (notes: View).

| Address | Value | Notes |
|---|---|---|
| `/cue/{number}/name` | string | |
| `/cue/{number}/notes` | string | |
| `/cue/{number}/number` | string, int or float | an empty string clears the number; a number in use is refused |
| `/cue/{number}/preWait` | seconds, 0 to 86400 | |
| `/cue/{number}/postWait` | seconds, 0 to 86400 | a Wait cue's post-wait is its duration: reading or changing one is the other |
| `/cue/{number}/duration` | seconds, 0 to 86400 | changes only a Wait or Fade cue's duration. Reads a Wait or Fade cue's duration, 0 for a cue that acts at once; Audio and Group cues answer `error` |
| `/cue/{number}/loadTime` | seconds, -86400 to 86400 | a Load cue's time (openLab's own address). A negative time loads the target that long before its end |
| `/cue/{number}/armed` | 1 or 0, or true or false | |
| `/cue/{number}/flagged` | 1 or 0, or true or false | |
| `/cue/{number}/uniqueID` | read only | the cue's unique id, for `/cue_id/` |
| `/cue/{number}/isLoaded` | read only | true or false |
| `/cue/{number}/isRunning` | read only | true while waiting, playing, fading or stopping |
| `/cue/{number}/isPaused` | read only | |
| `/cue/{number}/isBroken` | read only | |

A value of the wrong type, out of range, or a property the cue does not have (a duration on an
Audio cue, a load time on anything but a Load cue) gets an `error` reply and changes nothing.
Every change is one step in the workspace's undo history, as if made in the app.

Examples:

```
/cue/12/duration 4.5          sets Wait cue 12 to 4.5 seconds (no reply)
/cue/12/duration              replies {"status":"ok", ..., "data":4.5}
/cue/20/loadTime -30          Load cue 20 now loads its target 30 seconds before its end
/cue/12/duration -1           replies {"status":"error", ...} and leaves the cue alone
```

## Updates

After `/updates 1`, openLab tells the remote when something changed, so it can ask again for
what it shows. Updates carry no values themselves (`{ws}` is the workspace id):

| Message | Sent when |
|---|---|
| `/update/workspace/{ws}/cue_id/{id}` | that cue changed: a property, its state (started, paused, stopped, finished), or it was added, moved or removed. Its groups and list are named too |
| `/update/workspace/{ws}/cue_id/__root__` | the cue lists changed shape, or too many cues changed at once to name them: ask for everything again |
| `/update/workspace/{ws}/cueList/{list}/playbackPosition` | a playhead moved; the argument is the id of the cue it is on (none when it is past the end) |
| `/update/workspace/{ws}/dashboard` | a cue started or a playhead moved |

Updates go only to a remote with View. Over UDP they stop after 60 seconds of silence from the
remote (see [Connecting](#passcodes-and-rights)); a remote that falls behind loses updates and
should ask again when it sees `__root__`.

## Things to know

- **A repeat within 100 ms is ignored.** The same address with the same arguments from the same
  remote, less than a tenth of a second after the last, is dropped: a double-pressed GO fires
  once. A remote polling one value faster than that sees some polls unanswered.
- **Late input is dropped.** A message that waited more than 2 seconds (for example while a
  workspace was opening) is thrown away rather than run late.
- Network cues in a show send OSC too; their settings are in the cue's inspector and in
  **Workspace Settings > Network**.

## Where openLab differs from QLab 5

- **Only what is listed here is answered.** Levels, slices, `/new`, `/save`, most workspace
  settings and many queries of QLab 5 are not there yet.
- `/cue/{number}/loadTime` is openLab's own: QLab 5 has no address for a Load cue's time.
- A value out of range is refused whole with an `error`. QLab 5 sets a negative duration to 0.
- `/cue/{number}/duration` answers `error` for Audio and Group cues; QLab 5 answers their
  length.
- Wrong passcodes are counted per IP address and across all workspaces, and over TCP as over
  UDP; QLab 5 counts them per connection.
- Updates are sent once (QLab 5 sends each twice). `/updates 1` after a lapse subscribes again.
- A reply too big for a datagram is an `error`; QLab 5 sends nothing.
- `/version` is openLab's version. Cue `type` names other than Audio, Group and Cue List are
  openLab's.
- A cue has no OSC trigger address of its own; start it by number or id.
