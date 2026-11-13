# openLab UI protocol (engine <-> UI)

Types: `openlab-core::protocol` (`engine/crates/core/src/protocol/`) are the source of truth for fields; this page states behaviour, ordering and limits the types cannot. One golden JSON file per message variant is in `engine/fixtures/protocol/{client,server}/`; the Swift tests (`ProtocolFixtureTests`) decode every one. Model edits are core's `Command` (skill `edit-commands`); cue states and warnings are the cue engine's (skill `cue-engine`). Server mechanics (limits, ordering per connection, slow clients): skill `engine-ws-server`.

## 1. Conventions

- **Transport**: WebSocket on `127.0.0.1`, random port; one JSON object per UTF-8 text frame.
- **Auth**: the engine writes `{port, token, pid}` to `<app-support>/openLab/engine.json` (mode 0600). The client's first frame must be `hello` with the token.
- **Envelope**: `{"type": "<snake_case name>", ...fields}`. Nested variants (transport actions, requests, commands, runtime changes, upserts, problems, warnings, result data) are internally tagged with `"type"` too.
- **Units**: times are **seconds** (f64), never samples. Model levels are dB. Meter peaks are **linear amplitude** (f32; 1.0 full scale, above is a clip). Ids are UUID strings. `RequestId` is a client-chosen u64, unique per connection; `u64::MAX` is reserved for the server's own requests (`bad_request` if a client uses it). `revision` and `seq` are u64.
- **Numbers**: JSON has no NaN or infinity.
  - Client input: the codec refuses negative or out-of-range values where meaningless (a load time, a fade duration) and empty paths. `decode_client` returns `ProtocolError` and never panics, whatever the input.
  - Numeric payloads inside a `command` are not checked by the codec: `Workspace::apply` validates every time and level and refuses with `command_refused`.
  - Engine output: the encoder sanitises `progress` and `meters`: a negative, NaN or infinite time or peak is sent as 0 (a loop-end overrun must not cost the UI a tick). Other non-finite numbers in `status` are also sent as 0.
- **Frame limits** are checked on byte length before parsing (`MAX_HELLO_FRAME_BYTES`, decoded before the token is checked; `MAX_CLIENT_FRAME_BYTES`; `MAX_SERVER_FRAME_BYTES`; all in `protocol/mod.rs`). Over: `ProtocolError::TooLarge`.
- **Errors**: a missing `type` is `MissingType`; a `type` that is not a message of the direction is `UnknownType`; everything else wrong (bad JSON, wrong field types, an unknown variant inside a known message) is `Malformed`. All are rejections: the engine answers `bad_request` or `unknown_message`, as a `result` under the frame's id when it can be read (section 3), else as `error`. After `welcome` the connection stays open, except for a frame over the size limit, which ends it. A bad `hello` (malformed, or over `MAX_HELLO_FRAME_BYTES`) answers `bad_request` and ends the connection.
- **Compatibility**: unknown **fields** are ignored, so a newer peer may add optional fields without a version bump; receivers treat absent optional fields as documented. Unknown message **types** and unknown nested variants are errors. Removing, renaming or re-typing a field, or changing its meaning, bumps `PROTOCOL_VERSION` (now **4**: section 2). A client ignores a runtime change `type` it does not know. Any change updates the fixtures, the Swift `Codable` types and this page in the same commit.
- **Fixtures**: see skill `wire-protocol`. The test compares byte for byte and fails if a variant has no sample.

## 2. Connection

The engine holds several open workspaces ("docs"; limit in REQUIREMENTS §21). A doc is named by its `doc` id (`DocId`, u64), handed out in open order from 0 and never reused while the engine runs; a new engine run restarts from 0 with a new token. A connection is **bound** to one doc or is the **app connection**.

1. The client sends `hello { protocol_version, token, client_name, workspace?, pid? }`. `workspace` is a `doc` id and binds the connection; without it the connection is an app connection. `pid` is the client's process id: when an app connection ends without `goodbye`, the engine does not relaunch the app while that process runs, and waits for it to reconnect.
2. The engine answers `welcome`, or sends `error` and closes: `protocol_version_mismatch` (`error.protocol_version` says what the engine speaks), `bad_token` (wrong token, or any other message came first), `no_workspace` (`workspace` names no open doc; the client must not retry). `hello` decodes for any `protocol_version`, so another version can be told why.
   - Bound: `welcome { protocol_version, doc, snapshot }`. Everything the connection sends acts on that doc, and it receives only that doc's broadcasts plus the engine's own `log`, `log_entry`, `status`. `revision` and `runtime_seq` count per doc (a new doc's snapshot is revision 1).
   - App connection: `welcome { protocol_version, workspaces }`. A `workspaces` message follows every change to the list (a doc opened or closed, a name, path or unsaved state changed). It receives no doc's broadcasts. Doc-scoped messages on it (`command`, `undo`, `redo`, `transport` other than `panic`, `save`, `save_as`, `relink`, `preshow_check`, `close` without `doc`) answer `no_workspace`. Its `request: snapshot` answers a `workspaces` message, then `result`.
3. After `welcome` the client may send anything below. Every `command`, `transport`, `undo`, `redo`, `request`, `close`, `panic` and `hotkey` gets exactly one `result` with its `id`. A frame the engine cannot read gets `error` (`bad_request` or `unknown_message`). If the frame is a JSON object with a string `type` and a `u64` `id` (other than the reserved one), the engine answers it as a refused request: one `result` with that `id` and `error { code: bad_request | unknown_message }`, so the waiting call fails. Otherwise (no readable id) it sends the id-less `error`, which belongs to no request: a client publishes it and fails no request because of it (it cannot tell which one the engine meant); its requests end by their own deadline or when the connection drops. Both kinds count toward `MAX_BAD_FRAMES`.
4. **App-scope messages** work on any connection and act on the engine or every doc: `new`, `open`, `open_recovery`, `scan_recovery`, `discard_recovery`, `list_devices`, `set_output_device`, `set_buffer_size`, `log_tail`, `set_output_overrides`, `close { doc }`, `panic`, `hotkey`.
   - `panic` (Esc) panics every doc; twice within the fade is a hard stop. On the app connection `transport { action: panic }` is this app-scope panic.
   - `hotkey`: see section 3.
   - Pause All, GO and other transport actions act on the bound doc only (the front window's: the UI's notion; the engine has no front doc).
5. **Closing a doc**: see "Opening and closing workspaces". `goodbye { shutdown }` ends the session; `shutdown: true` also asks the engine to quit.

A client that sees a gap in `revision` or `seq` (section 4) sends `request: snapshot` and replaces its state.

`PROTOCOL_VERSION` is **4**. An engine refuses any other version at `hello`.

## 3. Client -> engine

Fields: `ClientMessage` in `protocol/client.rs`. Behaviour notes:

- **`command`** (`id`, `command`, `coalesce?`): `command` is core's `Command`, one undo step.
  - Reply order: `model_changed`, then `result { ok: { revision } }`. A refused command changes nothing and replies `command_refused`.
  - Exception: if a transaction's rollback itself fails (`RollbackFailed`, only a wrong inverse can cause it) the workspace is partly changed. The engine logs an error, bumps the revision (dirty), clears undo and redo, sends `document` and a whole `snapshot` instead of `model_changed`, then replies `command_refused` saying the workspace is partly changed.
  - A command that changes nothing replies `ok` with the current revision, sends no `model_changed`, and is no undo step.
  - `coalesce` is at most `MAX_COALESCE_KEY` bytes (longer: `bad_request`, nothing applied). Consecutive commands with the same key are one undo step, each still sent as its own `model_changed`.
- **`undo`, `redo`**: `ok.revision` after; `ok` without a revision when there was nothing to undo. A step that removes an active or previewed cue stops the cue as a delete does, with a `cue_removed` warning.
- **`hotkey`** (`id`, `key` 1 to 64 bytes, `modifiers?`, `phase?` `down` default or `up`): app-scope. `down` starts every cue of every doc whose `triggers.hotkey` is exactly this key and these modifiers; `up` sends each of those cues its release (`trigger_released`). A doc whose workspace key map (`settings.hotkeys`, the current edited map) uses the key starts none: the workspace action wins, and that action is the front window's, sent by the UI as a `transport`. The cue-editing keys of the map (`edit_number` ... `toggle_flag`) count too: the engine starts no cue on them and does nothing else with them; the app edits the selected cues. A key outside the bounds is `bad_request`.

### Transport actions

Each action becomes one cue-engine input (mapping in `control/transport.rs`; semantics in skill `cue-engine`). Notes the types do not carry:

- `go_from { list, cue }`: the playhead moves to `cue`, then GO, in one step; nothing starts if `cue` is not a playhead position of `list` (`not_in_list` warning).
- `stop_all` and `hard_stop_all` both map to `StopAll`: every cue and preview stops at once, no fade, nothing pending fires, no panic window. They stay distinct so a client can say which it means (OSC `/stop`, `/hardStop`).
- `stop { cue, fade_out? }` fades out over `fade_out` (`{duration, curve}`) when given; `hard_stop { cue }` never fades.
- `panic_cue { cue }`: the cue's own panic; it and what is in it fade out over `panic_duration` (0: at once). Other cues and the workspace panic are untouched.
- `reset_all`: every cue and preview stops at once (a panic fade included), every cue is reset (run-time break cleared, loaded cue unloaded), random groups start a fresh cycle, each list's playhead returns to its first cue: the doc as just opened.
- `set_playhead { list, cue }`: null cue means past the end.
- `preview { cue }`: an audio cue, or every armed audio cue in a group. For a Network cue it sends its message once now (no `preview` change follows). `stop_preview { cue? }`: absent or null stops every preview.
- `batch { actions }`: 1 to `MAX_BATCH_ACTIONS` per-cue actions (`start`, `stop`, `hard_stop`, `panic_cue`, `pause`, `resume`, `load`, `reset`, `preview`, `stop_preview`), no cue twice.
  - All items run in order at one scheduling time before anything else is handled, so cues started together start on the same sample and cues stopped together end on the same sample (NFR-21).
  - An empty or longer list, another action (a nested `batch` included) or a repeated cue makes the frame `bad_request`, nothing done. A cue not in the workspace gets `unknown_cue` and the other items still run. One `result` for the batch.
  - `panic_cue` items are one press: a cue fading only because an earlier item panicked its group or children is not pressed again.

The `result` of a transport action means accepted and handed to the cue engine. What happened shows in `runtime` (states, playhead, warnings such as `go_ignored`, `trigger_ignored`).

### Requests

Fields and reply shapes: `Request` and `ResultData` (`client.rs`, `server.rs`). Behaviour:

- **`new`, `open`, `open_recovery`, `scan_recovery`, `discard_recovery`**: "Opening and closing workspaces".
- **`save`**: `document` update, then `result`. `no_workspace` if never saved (use `save_as`); `io` if the write failed.
- **`save_as { path }`** (absolute): saves and makes `path` the doc's path. `io` on write failure. `already_open` (nothing written) if `path` is another open doc's show file or one another save or `open` is touching, including under another spelling (symlink, `/var` for `/private/var`; the writer compares by file).
- **`relink { folder }`** (absolute, at most `MAX_FOLDER_BYTES`): relinks the bound doc's missing media (WS-03, NFR-04).
  - Walked on its own thread, on a copy of the workspace, never on the control thread, within core's scan limits; symlinked folders are not entered. Missing files match by normalised file name; matches apply as **one undo step**; duplicates are `ambiguous`, never guessed; a match is skipped if the cue was edited to another file meanwhile.
  - `truncated` is true when a limit ended the walk early, so `unmatched` may name a cue whose file lies beyond it (the UI says so).
  - Errors: `not_found`, `bad_request` (not a folder), `io` (also a walk that did not return in time), `command_refused` (another relink is walking), `no_workspace` (doc closed meanwhile). `open` and `scan_recovery` are not held up by it.
- **`list_devices`**: each device's `name`, `output_channels`, `is_default` and `buffer_frames` (what it is asked for on this machine); `state` when the engine has opened an output on it since start, for the primary or for a patch (as in `status.outputs`, `closed` included; absent: never), and `primary` on the engine's primary device. `selected` is the device chosen with `set_output_device` (absent: none). A device that left the list (lost) is only in `status.outputs`.
- **`set_buffer_size { device, frames }`** (AUD-21): the frames per callback asked of `device` on this machine, 16 to 1024 (512 when none is set); kept in machine settings. A device the engine plays on reopens at once at that size, its cues playing on after the gap; the other devices are untouched. A size the device cannot do falls back to the nearest it can, with a `warn` log; a reopen that fails is a device loss. `bad_request` (out of range), `not_found` (not a `list_devices` name) or `io` change nothing.
- **`set_output_device { device? }`**: the primary device (whose clock is the show's) the engine opens at its **next start** (it never switches live; the app restarts it). Must be a `list_devices` name; null means system default. Kept in machine settings. `not_found` or `io` change nothing. At start an unlisted chosen device falls back to the default with a `warn` log, so the show still starts.
- **`preshow_check`**: "Pre-show check"; answered at once from current state, no file access.
- **`log_tail { max? }`**: the newest in-memory show-log entries, oldest first, with `dropped` and `seq`.
- **`set_output_overrides { network_disabled }`** (works with no doc open): with `true`, Network cues still run, complete and continue but send nothing. Engine-wide, in memory only, off after an engine restart; shown as `network_output_disabled` in `status`.
- **`set_show_mode { on }`** (SM-01): kept per open doc and saved with it (`show_mode`, file-format §4.1); a file without one starts in Edit Mode. Changing it does not mark the doc unsaved. It survives a UI restart (`document.show_mode`); a `snapshot`'s `workspace` never carries it. While any doc is in Show Mode or any cue runs, the engine prevents idle sleep (SM-07). The UI, not the engine, locks editing. `no_workspace` on the app connection.
- **`list_patch_mappings`** (WS-07): where each of the doc's patches plays on this machine, workspace order; `mapping` absent for a patch never mapped or cleared. `resolution` is where it resolves now: the device and channels it plays on, or why not (`unmapped`, also while it waits for a free mixer slot; `device_missing` while its device is absent, away or not open yet; `channels_missing`; `output_count_mismatch`).
- **`set_patch_mapping { patch, mapping? }`**: sets (`{device, channels}`) or clears (null or absent) where the doc's `patch` plays **on this machine**; kept in machine settings under workspace id and patch id, not in the show file.
  - `device` must be a name `list_devices` gives; `channels` has one 0-based device channel per patch output, each below `output_channels`. Refused with nothing changed: `not_found` (patch or device), `bad_request` (count, range). Patches resolve again at once, so `runtime` problems and `preshow_check` show the new `resolution` and a running cue's slot follows in place. A device the engine does not play on yet opens for the patch (AUD-02), which is `device_missing` until it plays.
  - A clear pauses the cues playing through the patch; a later mapping does not resume them. A clear is stored explicitly.
- **`snapshot`**: a `snapshot` (on the app connection `workspaces`), then `result`.
- **`waveform { cue, start, end, buckets }`** (AUD-14): peaks of an Audio cue's file over `start..end` (finite, `0 <= start < end`) in `buckets` (1 to 8192) parts. `end` is clamped to the file length; fewer buckets may come back (few frames, or a size cap).
  - The file is read whole once by the waveform reader (never a probe worker; one file at a time engine-wide) and cached per workspace under a byte cap, so later answers at any zoom are instant; a re-probe reads again.
  - Refused: `bad_request` (range or count out of bounds; not an Audio cue), `not_found` (no cue or no file), `unsupported` (missing, unplayable, not probed yet, or too long; ask again once `media_duration` is known), `command_refused` (too many waiting), `io` (disk stopped answering past `waveform_timeout`; repeated timeouts stop further reads until restart).

Paths are never empty, and `open` and `save_as` paths must be absolute (`bad_request` on decode): a relative one would resolve against the engine's own working directory.

#### Test support (test-only; not in a release engine)

An engine built with `test-support` and started with `--virtual-device stepped|realtime` plays on a virtual output (skill `devices`). Three more app-scope requests drive it: `advance { frames }` (stepped only), `lose_device`, `restore_device`. A release engine refuses them like any unknown request (`bad_request`) and refuses `--virtual-device` (exit 2; `mac/scripts/verify-bundle.sh` checks the bundled engine). The Swift cases are Debug only.

- `advance` waits for background work, then renders `frames` at 48 kHz block by block with a control tick and the same wait after each, so states, meters, log times and status are as a device would have made them. `frames: 0` only waits. Errors: `unsupported` (no virtual device, or realtime), `bad_request` (another `advance` under way), `internal`.
- `lose_device` removes the virtual device as an unplugged interface would (cues on it break `device_lost`); `restore_device` lists it again and the engine reopens it at the next device-list read. `unsupported` without a virtual device.
- On a stepped engine nothing moves between requests (device time, the epoch clock behind log `time_ms`, autosave, `status`), and worker answers are taken in at the next `advance`, so a script gets the same messages every run. Send `advance { frames: 0 }` after `open` before binding a connection so the welcome snapshot has every media duration.

#### Opening and closing workspaces

- **Another doc opens; the others play on.** `new`, `open` and `open_recovery` add a doc (limit in REQUIREMENTS §21). Nothing running stops. The `result` carries `opened { doc }` and nothing else; app connections get a `workspaces` list with the new doc; a connection bound to it (`hello { workspace: doc }`) gets its snapshot, including load-repair and recovery warnings.
- **The engine never prompts.** Opening needs no `discard_changes`. `close` over unsaved edits is refused `unsaved_changes` unless `discard_changes: true`; discarding also removes the autosave this doc wrote of the dropped edits, ordered after any autosave write in flight so it is not offered again. The UI asks the operator, then repeats `close` with the flag.
- **`open_recovery`** opens an autosave from `scan_recovery`, dirty. It binds to its show file (`document.path` set, as `open` with `restore`) when it is the current autosave of that file, newer than the file, and the file still holds this workspace, unused by another doc. Otherwise it is untitled (`document.path` null), with a `recovery_untitled` notice when it named a file that could not be used.
- **Closing**: the doc's cues stop at once (Panic twice), the requester gets its `result`, then every connection bound to it gets `closed { doc }` and is closed; app connections get `workspaces` without it. A client must not reconnect to a closed doc (`hello` answers `no_workspace`). `close` errors: `unsaved_changes`, `no_workspace` (no such doc, or no `doc` on the app connection).
- **`recovery`** (`open` only): `restore` opens the autosave (dirty, bound to the show path); `ignore` opens the show as saved and leaves the autosave (offered again until a save or later autosave replaces it). Absent, with an autosave newer than the show file: nothing opens and the `result` carries `recovery_offer { path, workspace_name, modified_ms }`; the UI asks and repeats `open` with a choice. `modified_ms`: wall-clock ms since the Unix epoch.
- **`open_recovery { workspace_id, path_hash?, modified_ms? }`** opens an autosave from `scan_recovery` as an **untitled, unsaved** doc. The optional fields pick one of several autosaves of one workspace id; absent: the newest.
- **`scan_recovery`** lists every recovery file, including a never-saved doc's autosave and files moved aside as stale (`stale: true`; they may hold the newest work), newest first. It serves the launch offer after an unclean exit (NFR-05) and what to show when a show cannot be opened (`bad_show_file`, or `io`).
- **One show file, one doc.** `open` of an open show, or of a file another doc's `save_as` is writing, answers `already_open`. Paths compare as media paths do (case, Unicode form, `.`/`..`) and, on the document worker before the read, by the file itself. So neither doc's save can overwrite the other's. A copy of an open show (another file, same workspace id) opens with a fresh workspace id and dirty.
- **`discard_recovery { workspace_id, path_hash?, modified_ms? }`** deletes the one autosave file `open_recovery` would open for that selector (it cannot be undone), and nothing else; the `result` is plain `ok`. `not_found` when no listed autosave matches; `command_refused` for the current autosave of an open workspace; `io` when the file cannot be removed. A save of a doc restored from a set-aside autosave (`open_recovery`) removes that file once the save is done: the operator has taken its contents. Any other save leaves set-aside files.
- **One document read at a time** engine-wide (`open`, `open_recovery`, `scan_recovery`, `discard_recovery`, `new`); another arriving meanwhile is `command_refused`. A read unfinished after the engine's timeout (a stalled network volume) answers `io`, and later requests are served normally.
- **Saving**: `save` and `save_as` write on a separate thread. `result` comes once the file is durably written; the new `document` (path, `dirty` false when no edit happened meanwhile) is broadcast before it. A failed write is `io`; the doc stays dirty and autosave keeps running. Autosave is not visible on the protocol (skill `persistence`).

## 4. Engine -> client

Fields: `ServerMessage` in `protocol/server.rs`. Behaviour:

- **`welcome`**: section 2. **`snapshot`**: the whole state again (after `request: snapshot`, a resync, or a save-as to another folder that re-spelled media references); the client replaces everything.
- **`document { path, dirty, can_undo, can_redo, show_mode? }`**: `show_mode` absent means false.
- **`runtime { seq, changes }`**: event-driven, applied in order.
- **`progress`**, **`meters`**: at most 30 Hz, only while something runs; meters are linear peaks per channel since the previous message.
- **`log`**: untimed engine notices; lossy like `meters` (a client whose queue is half full misses some). **`log_entry`**: each show-log record; lossy and rate-limited. A `log` notice is also a record, so a log window uses `log_entry` and `log_tail` only.
- **`status`**: about once a second, and early (not faster than every 250 ms) when `xruns`, the disk's health, the device's state, an output device's opening or state, or `network_output_disabled` change; droppable like `meters`. A new client gets its first with the next periodic one.
- **`workspaces`** (app connections only): the open docs in open order; **replaces** the previous list. `path` is null for a never-saved doc. `path_hash` names the doc's autosave (matches its `recovery_entries` entry): a copy of a show shares its `workspace_id`, so the app tells the open doc's autosave from another copy's by this hash. Sent when a doc opens or closes and when a name, path or unsaved state changes (checked once per control-loop wake).
- **`closed { doc }`**: to connections bound to `doc`; the engine closes the connection next.
- **`warnings`** (DX-01): workspace-level notices in force (load repairs, damaged media, device problems); **replaces** the previous list; empty clears. Broken cues are not here: their rows, with the cue id, come from `problems` in the snapshot and `runtime`.
- **`result { id, ok { revision?, data? } | error { code, message } }`**: exactly one of `ok`/`error`. **`error`**: not tied to a request (section 2).

### Progress

Sent to connections bound to the doc whose cues it reports, only while something in the doc counts; nothing while every cue is idle or loaded, and no wake is added for it. Full rules: skill `cue-engine` ("Timer progress").

- `items`: an audio cue's from its voice (`position`, when present: seconds into the media file the voice is at, with its slices, loop passes and devamps applied, so it stays inside a looping slice and moves on after a devamp; absent for a cue without a voice, past a finite plan's end, and for a voice a devamp stop has ended); a cue without a voice (Wait, Fade, Group, ...) from its run, `elapsed` since its action start plus any load-to-time offset. `remaining` is null when there is no end (infinite loop, waiting for feedback, group with a busy child). A broken run has no item.
- `waits` (absent when empty): `wait` is `pre_wait` or `post_wait`; `remaining` is never null. A cue can be in both lists. Countdowns keep their schedule for a cue that is broken or skipped.
- A paused cue's entries keep coming at the values it froze at. Stop, Reset and Panic end them.

### Pre-show check

`result.ok.data` of `preshow_check` (SM-06, NFR-07). Media status is what probes and the media watch last reported (skill `media-files`).

- `ok` is true when nothing is a problem: `summary.total` is 0. Not problems: `not_checked` destinations, patches no cue uses, the `preload_over_budget` warning.
- `summary.total` = `broken_cues + system`. Media, patches and destinations are causes of broken cues and are not added again; `patches` counts only used patches with no device and `network` only `missing` destinations.
- `broken_cues` in workspace order, with the same `problem` objects as `runtime`. A disarmed cue is listed only for problems of its own; a group is not listed for a broken child.
- `media`: by path, `missing` or `unsupported`, with the cues using it.
- `patches`: patches with no device to play on, then patches cues name that the workspace lacks. A patch no cue uses is listed but not counted and not a failure.
- `network`: destinations Network cues use: `missing` (a problem) or `not_checked` (exists; **reachability is not probed**: neither problem nor pass, `ok` stays true).
- `system`: the `warnings` list (device lost or stalled, damaged media, preload problems, unplayable slices), sorted by `code`, cue, message. Any entry makes `ok` false except `preload_over_budget` (streamed and plays).

### Show log entries and status

A `LogEntry`: `time_ms` (wall-clock ms, UTC), `show_time?`, `level`, `kind`, `message` (English), `cue?`, `source?`. `trigger` records (a transport action arrived; `source` `ui` or `osc <ip>`) are rate-limited per source with an `N more input(s) not logged` record. The same records go to the log file (skill `engine-lifecycle`).

**Live log resync.** `seq` numbers every record the engine makes from 1, pushed or not; `log_tail` carries the `seq` of the newest record when answered (0: none).
- A live view reads `log_tail` once, then appends each `log_entry` whose `seq` is above the last it has. An entry at or below it is already in the view.
- A jump of more than one means records were not pushed: read `log_tail` again.
- Pushes are lossy (dropped while the client's queue is half full) and rate-limited on the trigger records' 10 s wall-clock window. A push the hub drops for a slow client is not offered again: that client sees `status.log_seq` (about once a second) above the last `seq` it got, and reads `log_tail`.
- The log is the engine's, not a doc's: `log_entry` carries no doc.

**`status`**: `cpu_percent` (engine process, percent of one core; null without a figure), `engine_load` (worst callback time over block time since the previous status; 1.0 means a callback took as long as its block, above 1 is a dropout; null when no callback ran, e.g. device lost, and for offline rendering), `xruns`, `disk { streams, underrun_frames, healthy }` (healthy: no underrun for ten seconds), `device { name?, state, sample_rate, block_frames?, channels, latency_ms? }` (the primary device; `state`: `ok`, `lost` while the engine tries to reopen, `failed` when only a restart helps, `absent` for offline rendering), `outputs` (AUD-02: every output device the engine has opened since start, the primary and each device a patch was mapped to, `closed` ones included; the primary first (`primary: true`), then the others in the order they opened, each `{ name, state, primary?, sample_rate, channels, block_frames?, latency_ms? }`; `channels` is 0 and `block_frames` absent until a device's first open answers; `state`: `up`, `opening` (for a patch mapped to it, until it plays), `lost` (reopening), `reconfiguring` (reopening at once at another buffer size), `closed` (no patch maps to it), `failed` (only a restart helps); every device runs at the primary's rate; absent reads as empty), `log_dropped`, `log_seq` (absent reads as 0), and `network_output_disabled` (absent: false). **The UI must show `network_output_disabled` plainly while true.**

### Media durations

What an audio cue's file gives it to play, for the Duration column (CL-02): derived from the probe plus trims and slices, never saved. Seconds, rounded down to the millisecond.
- `file`: whole file. `trimmed`: start trim to end trim, every slice once. `duration`: the cue's action (`trimmed` with every slice's play count); absent when a slice loops until devamped (QLab 5 excludes pre-wait: manual pp. 94-95 and 124). `channels`: 1 to 64 (AUD-06).
- A field is absent when unknown: all while the file is unprobed, missing or unplayable; `trimmed` and `duration` when trims and slices cannot play (`slices_unplayable`).
- A change with only `cue` means "not known now": the client drops what it held, so a missing file, relink or removed cue never leaves a stale length.
- They ride the runtime channel, not `cue` upserts: a probe answer is not an edit and would move the revision and unsaved state. A change is sent when the probe answered again, the file went missing, trims, slices, counts or file were edited (after that edit's `model_changed`), or the cue was removed; one `runtime` message per tick.

### Snapshot

Fields: `Snapshot` in `server.rs`. `workspace` never carries `show_mode`. `cue_states` lists cues not idle. `group_warnings` lists groups with a broken descendant; one with no run of its own is `broken` in `cue_states` with no `reason` and no `problems` entry. A previewed group is listed in `previewing` with its audio cues.

### Model changes

`model_changed.revision` rises by exactly 1 per message from `snapshot.revision`; any other step is a gap (request `snapshot`). Apply `deletes` first, then `upserts` in order. A `delete` of an unknown id is ignored; an `upsert` of a cue the client holds replaces it wherever it is.

- `cue { list, parent, index, cue }` puts the cue (new, changed or moved) at `index` among the cues of `parent` (null: top level). A group's upsert carries its whole subtree and replaces it; children are not upserted separately.
- `cue_list { id, name, index }`; its cues come as `cue` upserts, and an upsert for a held list keeps its cues. `patch`, `network_patch`: `index`, item. `settings { name, settings }`: the name and all settings (OSC passcodes inside are visible to every client holding a token).
- Deletes: `cue` (with children), `cue_list`, `patch`, `network_patch`. The engine only removes empty cue lists, so their cues were deleted or moved out in the same or an earlier message.
- Collections: a `patch`/`network_patch`/`cue_list` upsert per item added or changed and a delete per item removed; for a move (or its undo), or when two or more items of a collection changed, an upsert of every item of that collection at its final index, ascending. These come before the message's cue upserts.
- Cue edits: a `delete` for each cue removed or moved, then an `upsert` for each cue added, moved or changed, in tree order (parents first, ascending index), so several moves in one undo step cannot collide. A cue inside an also-upserted group is not sent separately. One `model_changed` per edit, undo, redo or runtime model change (`SetArmed`).
- **`model_changed` for revision N is always sent before the `result` reporting `ok.revision = N`.**

### Runtime changes

`runtime.seq` rises by 1 per message (a gap means request a snapshot). Change types, from `RuntimeChange` (`protocol/changes.rs`): `cue_state`, `problems`, `group_warning`, `playhead`, `preview`, `warning`, `media_duration`.

- `cue_state { cue, state, reason? }`: `reason` (`voice_rejected`, `stream_failed`, `device_lost`) is set when the cue broke while running. A `broken` state with no `reason` is a cue with static problems (listed in `problems`) or, with neither, a group with a broken descendant (it has no problems of its own and still starts).
- `problems`: the full list of static problems, empty when none. `group_warning { group, warning }`: why a group shows broken (its `cue_state` is `broken` while it has no run).
- `warning` mirrors the engine's warnings, plus `go_ignored {list, reason}` and `notice {code, message, cue?}` for what only the binary knows.
- `media_duration` does not come from the cue engine: it comes from the control loop's plan checks (above).
- The UI localises from the variant or `code`; `message` is English.

### Ordering and gaps

- Match a `result` to its request by `id`: results may leave in a different order than the requests were sent.
- The server may hold a `result` with `ok.revision = N` until `model_changed` N has been written (at most `revision_wait`, 2 s). If a `result` is ahead of the client's model, request a `snapshot`.
- Never pipeline `snapshot` requests: while one is outstanding, broadcasts are withheld until its `result`.
- On one connection, broadcasts are ordered before replies, and a `snapshot` is never followed by older broadcasts (those at or below its revision or `seq` are filtered). `document` and `warnings` may arrive after the `snapshot`; they are still current.
- Revision and `seq` gaps are forwarded unchanged; the client recovers with `request: snapshot`. A lagged client gets a fresh unsolicited `snapshot` and is not closed; a `result` without a revision may overtake it.
- `closed { doc }` arrives after the requester's `result`. A resync for a closed doc answers `no_workspace`, then `closed`.
- `u64::MAX` is a reserved request id (`bad_request`). A second `hello` and binary frames are `bad_request`; 32 consecutive bad frames close the connection (`MAX_BAD_FRAMES`).
- `meters`, `progress`, `status`, `log_entry` and `log` are dropped for a slow client without lagging it.

### Error codes

`ErrorCode` in `server.rs`; the client acts on the code, never the message:
- `protocol_version_mismatch`, `bad_token`: connection ends at `hello`; do not retry blindly.
- `bad_request`, `unknown_message`: the frame was rejected; after `welcome` the connection stays open unless the frame was over the limit. A bad `hello` or an over-limit frame ends it.
- `command_refused`: the engine refused a valid message (validation, limit, busy). Nothing changed.
- `not_found`, `already_open`, `unsaved_changes`, `io`, `bad_show_file`, `unsupported`, `internal`.
- `no_workspace`: no doc to act on (none open, a closed doc, an app connection, or a save with no path yet).
