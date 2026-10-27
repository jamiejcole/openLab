# openLab UI protocol (engine <-> UI)

Types: `openlab-core::protocol` (`engine/crates/core/src/protocol/`). They are the source of truth; this page is the spec the Swift types follow. One golden JSON file per message is in `engine/fixtures/protocol/{client,server}/` (the Swift tests decode every one). Architecture: ARCHITECTURE section 6. Model edits are core's `Command` ([commands.md](commands.md)); cue states and warnings are the cue engine's ([cue-engine.md](cue-engine.md)).

## 1. Conventions

- **Transport**: WebSocket on `127.0.0.1`, random port; one JSON object per text frame, UTF-8. The server is not part of the types crate: see [network.md](network.md) (limits, ordering per connection, slow clients).
- **Auth**: the engine writes `{port, token, pid}` to `<app-support>/openLab/engine.json` (mode 0600). The client's first frame must be `hello` with the token.
- **Envelope**: `{"type": "<snake_case name>", ...fields}`. Nested variants (transport actions, requests, commands, runtime changes, upserts, problems, warnings, result data) are also internally tagged with `"type"`.
- **Units**: times are **seconds** (f64), as in the show file, never samples. Levels in the model are dB (as the show file). Meter peaks are **linear amplitude** (f32): 0.0 is silence, 1.0 full scale, above 1.0 a clip. Ids are the model's UUID strings. `RequestId` is a non-negative integer (u64) the client chooses, unique on its connection; `u64::MAX` is reserved for the server's own requests (`bad_request` if a client uses it). `revision` and `seq` are u64.
- **Numbers**: JSON has no NaN or infinity. For **client** input the codec refuses negative or out-of-range values where they are meaningless (a load time, a fade duration) and empty paths: `decode_client` returns `ProtocolError`, never panics, whatever the input (non-JSON, wrong types, missing fields, huge numbers, deep nesting). Numeric payloads inside a `command` are not checked by the codec: `Workspace::apply` validates every time and level (finite, in range; [commands.md](commands.md) "Valid") and refuses with `command_refused`. For **engine** output the encoder sanitises `progress` and `meters` instead of failing: a time or peak that is negative, NaN or infinite is sent as 0 (an overrun of a loop end must not cost the UI a whole tick).
- **Frame limits** (checked on the byte length before any parsing): the first frame of a connection, `hello`, at most `MAX_HELLO_FRAME_BYTES` (4 KiB; the server reads it with `decode_hello`, before the token is checked); later client frames at most `MAX_CLIENT_FRAME_BYTES` (1 MiB); engine frames at most `MAX_SERVER_FRAME_BYTES` (32 MiB, for snapshots). Over the limit: `ProtocolError::TooLarge`.
- **Errors**: an envelope whose `type` is missing is `MissingType`, one that is not a message type of the direction is `UnknownType`; everything else wrong (bad JSON, wrong field types, missing fields, an unknown variant *inside* a known message such as a transport action or request kind) is `Malformed`. All are rejections; the engine answers `error` with `bad_request` or `unknown_message`.
- **Compatibility**: unknown **fields** are ignored (no `deny_unknown_fields`), so a newer peer can add optional fields without a version bump; the receiver must treat absent optional fields as documented. Unknown message **types** and unknown variants of nested enums are errors. Removing, renaming or re-typing a field, or changing its meaning, bumps `PROTOCOL_VERSION` (now **1**). Any change to a message updates the fixtures, the Swift `Codable` types and this page in the same commit.
- **Fixtures**: `UPDATE_PROTOCOL_FIXTURES=1 cargo test -p openlab-core --test protocol` rewrites them (pretty-printed, keys in declaration order, trailing newline); without it the test compares byte for byte and fails on drift. Files are named after the message type (`client/transport.json`); further variants add `_<variant>` (`client/transport_stop.json`, `server/result_error.json`). Every variant of the nested enums the client must decode has its own file: `server/warnings_<warning>`, `server/runtime_problem_<problem>`, `server/runtime_problem_device_unavailable_<resolution>`, `server/runtime_state_<state>`, `server/runtime_state_broken_<reason>`, `server/error_<code>`, `server/log_<level>`; tests fail if a variant has no sample.

## 2. Connection

1. Client connects and sends `hello { protocol_version, token, client_name }`.
2. The engine answers `welcome { protocol_version, snapshot }`, or sends `error` and closes: `protocol_version_mismatch` (the version differs from the engine's; `error.protocol_version` says what the engine speaks) or `bad_token` (wrong token, or any other message came first). `hello` decodes for any `protocol_version`, so a client from another version can be told why.
3. After `welcome` the client may send everything below. Every `command`, `transport`, `undo`, `redo` and `request` gets exactly one `result` with its `id` (a frame the engine cannot read gets an `error` with `bad_request` or `unknown_message`; the connection stays open).
4. `goodbye { shutdown }` ends the session. `shutdown: true` also asks the engine to quit; the default leaves it running for a reconnect.

A client that sees a gap in `revision` or in `seq` (section 4) sends `request: snapshot` and replaces its state.

## 3. Client -> engine

| `type` | Fields | Notes |
|---|---|---|
| `hello` | `protocol_version: u32`, `token`, `client_name` | first frame |
| `command` | `id`, `command`, `coalesce?` | `command` is core's `Command` ([commands.md](commands.md)): one undo step. The engine replies `model_changed`, then `result { ok: { revision } }`; a refused command changes nothing and replies `error.code = command_refused`. A command that changes nothing (a value set to what it already is) replies `ok` with the current revision, sends no `model_changed` and is no undo step. `coalesce`: a string of at most 128 bytes (longer: `bad_request`, nothing applied); consecutive commands with the same key are one undo step, each still sent as its own `model_changed` ([commands.md](commands.md) "Coalescing"). Absent = no coalescing |
| `transport` | `id`, `action` | see below |
| `undo`, `redo` | `id` | like a command; `ok.revision` after, `ok` without a revision if there was nothing to undo or redo |
| `request` | `id`, `request` | see below |
| `goodbye` | `shutdown: bool` (default false) | |

### Transport actions

`action.type` and the cue-engine input each becomes (`Input::...`):

| `action.type` | Fields | Input |
|---|---|---|
| `go` | `list` | `Go` |
| `panic` | - | `Panic` |
| `pause_all`, `resume_all` | - | `PauseAll`, `ResumeAll` |
| `set_playhead` | `list`, `cue` (null: past the end) | `SetPlayhead` |
| `set_playhead_by_number` | `list`, `number` | `SetPlayheadByNumber` |
| `playhead_next`, `playhead_previous` | `list` | `PlayheadNext`, `PlayheadPrevious` |
| `start` | `cue` | `Start` (no playhead move) |
| `stop` | `cue`, `fade_out` (`{duration, curve}`, optional) | `Stop { fade_out }` |
| `hard_stop` | `cue` | `Stop { fade_out: None }` |
| `pause`, `resume` | `cue` | `Pause`, `Resume` |
| `load` | `cue`, `at` (seconds, >= 0) | `Load { time: at }` |
| `reset` | `cue` | `Reset` |
| `trigger_released` | `cue` | `TriggerReleased` (second trigger on release) |
| `preview` | `cue` | `Preview` (M4.13) |
| `stop_preview` | `cue` (optional; absent or null: every preview) | `StopPreview` |

The `result` of a transport action means "accepted and handed to the cue engine"; what happened shows in `runtime` (states, playhead, warnings such as `go_ignored` or `trigger_ignored`).

### Requests

| `request.type` | Fields | Effect and reply |
|---|---|---|
| `new` | `discard_changes?` | new empty workspace (one cue list, one stereo patch); `snapshot` message, then `result` |
| `open` | `path`, `discard_changes?`, `recovery?` | load a show; `snapshot`, `warnings`, then `result`; errors `not_found` (no such file), `io` (unreadable, not a regular file, or the read stalled past the engine's timeout), `bad_show_file`, `unsaved_changes`, `command_refused`. When an autosave is newer than the show file and `recovery` is absent, nothing is opened and the `result` carries `recovery_offer` (below) |
| `open_recovery` | `workspace_id`, `path_hash?`, `modified_ms?`, `discard_changes?` | open an autosave found by `scan_recovery` as an **untitled, unsaved** workspace (`document.path` null, dirty); `snapshot`, `result`. `path_hash` / `modified_ms` pick one of several autosaves of the same workspace id (the listed entry's values); absent: the newest. Errors `not_found`, `io`, `bad_show_file`, `unsaved_changes`, `command_refused` |
| `scan_recovery` | - | list **every** recovery file in the engine's recovery folder, including the autosave of a never-saved workspace and files moved aside as stale (`stale: true`; they may hold the newest work): the launch offer after an unclean exit (NFR-05), and what to show when a show cannot be opened: `result { ok: { data: { type: "recovery_entries", entries: [{workspace_id, path_hash?, show_name?, modified_ms?, stale}] } } }`, newest first; `modified_ms` is milliseconds since the Unix epoch (wall clock, UTC); errors `command_refused` |
| `save` | - | save to the current path; `document` update, `result`; `no_workspace` if never saved (use `save_as`); `io` if the write failed |
| `save_as` | `path` | save to `path` and make it the workspace's path; `document` update, `result`; `io` if it could not be written; `bad_request` (on decode) if `path` is not absolute |
| `revert` | `discard_changes?` | reload the saved file, dropping edits (clears undo); `snapshot`, `result`; `no_workspace` if never saved, `not_found` / `io` / `bad_show_file` if the saved file cannot be read, `unsaved_changes` while dirty without `discard_changes`, `command_refused` |
| `relink` | `folder` | relink missing media (WS-03) as one undo step; `result { ok: { revision, data: { type: "relink", relinked, ambiguous: [{cue, candidates}], unmatched } } }`; nothing is guessed between duplicates |
| `list_devices` | - | `result { ok: { data: { type: "devices", devices: [{name, output_channels}] } } }` |
| `log_tail` | `max?` | the newest show-log entries the engine holds in memory (at most 1000), oldest first, for a log window opened after the fact (DX-02): `result { ok: { data: { type: "log_tail", entries: [log_entry], dropped } } }`; `max` absent: all. `dropped` counts records the disk writer lost (they are still in `entries` while in memory). Entry fields below |
| `snapshot` | - | `snapshot`, then `result` |

Paths are never empty, and the paths of `open` and `save_as` must be absolute (`bad_request` on decode): a relative one would resolve against the engine's own working directory. `new` can answer `unsaved_changes` or `command_refused`.

#### Replacing the workspace (M4.4)

`new`, `open`, `open_recovery` and `revert` replace the workspace the engine holds:

- **They stop everything first**: every running cue is hard-stopped (no fade), as if Panic were pressed twice, then the workspace, undo history, cue engine state and media catalog are rebuilt. This is the engine's own choice (QLab closes a workspace by stopping it); the UI should confirm with the operator when cues are running (SM-05) before sending the request.
- **The engine never prompts.** If the workspace has unsaved edits (`document.dirty`) the request is refused with `unsaved_changes` and nothing changes, unless it says `discard_changes: true`. Discarding also removes the autosave of the dropped edits (ordered after any autosave write in flight, so it is not offered again; not when the same file is reopened, whose autosave may be the one just restored). The UI asks the operator, then repeats the request with the flag.
- **`recovery`** (`open` only): `restore` opens the autosave (dirty until saved, bound to the show path); `ignore` opens the show file as saved and leaves the autosave in place (it is offered again next time until a save, or the next autosave, replaces it). A `recovery_offer` result (`{ type: "recovery_offer", path, workspace_name, modified_ms }`; `path` is the `path` of the `open` it answers, `modified_ms` is milliseconds since the Unix epoch, wall clock UTC) means the autosave is newer than the show file and nothing was opened or stopped; the UI asks and repeats the `open` with a choice. `discard_changes` is checked before the offer.
- **A show that cannot be opened** is `bad_show_file` (or `io` when the file cannot be read). The UI then sends `scan_recovery` and may offer `open_recovery` for an entry.
- **Numbering never restarts.** `revision` and `runtime_seq` keep rising across these requests (a replacement is a new model version: `revision` rises by at least 1). On success the new `snapshot` goes to the requester (before its `result`) **and to every other connected client**; a client treats it as any `snapshot` (replace everything). Workspace-level `warnings` arrive as before, and cue state changes from the stop arrive as `runtime` before the snapshot (clients ignore cue ids they do not hold).
- Only one such request is handled at a time (the file is read off the control thread); another arriving meanwhile is refused with `command_refused`. A read that has not finished after 20 s (a stalled network volume) answers `io` and later requests are served normally; the requester's connection does not hold broadcasts for it for more than 60 s either (docs/network.md).

Saving: `save` and `save_as` write on a separate thread. The `result` comes when the file is durably written; the new `document` (path, `dirty` false when no edit happened meanwhile) is broadcast before it. A failed write is `io`, the workspace stays dirty and the autosave keeps running. The engine autosaves a dirty workspace every `settings.autosave_interval` seconds to the recovery folder (docs/persistence.md); that is not visible on the protocol.

`PROTOCOL_VERSION` stays **1**: every addition is optional or new. `discard_changes` and `recovery` default to `false` / absent and are not written at those values; `new` and `revert` are now objects with optional fields, but the bare `{"type":"new"}` / `{"type":"revert"}` frames still decode and mean the same. The new request, result and error-code names are additions (an old engine answers an unknown request `type` with `bad_request`). The Swift client (M5) must implement these messages and the `unsaved_changes` / recovery flows.

## 4. Engine -> client

| `type` | Fields | Notes |
|---|---|---|
| `welcome` | `protocol_version`, `snapshot` | answer to `hello` |
| `snapshot` | `snapshot` | the whole state again; the client replaces everything |
| `model_changed` | `revision`, `upserts`, `deletes` | below |
| `document` | `document {path, dirty, can_undo, can_redo}` | save state and undo availability changed |
| `runtime` | `seq`, `changes` | cue state changes, event-driven; applied in order |
| `progress` | `items [{cue, elapsed, remaining}]` | seconds; `remaining` null when there is no end; at most 30 Hz, only while a cue runs |
| `meters` | `outputs [{patch, peaks}]`, `cues [{cue, peaks}]` | linear peak per channel since the previous message; at most 30 Hz, only while audio plays |
| `log` | `level` (`error`, `warn`, `info`, `debug`), `message` | engine notices as they happen, with no time stamp; the timed, structured record of everything (GO, cue starts and stops, triggers, document events) is the show log, fetched with `log_tail` (below) |
| `status` | `status {cpu_percent, engine_load, xruns, disk, device, log_dropped}` | system health (DX-03), about once a second and early (not faster than every 250 ms) when `xruns`, the disk's health or the device's state change; droppable like `meters`. Below |
| `warnings` | `warnings [warning]` | workspace-level notices in force (load repairs, damaged media); **replaces** the previous list; empty clears it |
| `result` | `id`, then `ok {revision?, data?}` or `error {code, message}` | exactly one of `ok` / `error` |
| `error` | `code`, `message`, `protocol_version` | not tied to a request (see section 2) |

### Show log entries and status (M4.7)

`log_entry` (in `log_tail`): `time_ms` (wall clock, milliseconds since the Unix epoch, UTC), `show_time?` (seconds of audio rendered; absent for engine records that have none), `level`, `kind`, `message` (English, for people), `cue?`, `source?`. `kind` is `go` (a GO, with the cue it starts), `transport` (any other transport action, with the cue it names), `trigger` (a transport action arrived: `source` is `ui` or `osc <ip>`; rate-limited per source, with an `N more input(s) not logged` record), `cue_start`, `cue_stop`, `cue_fail` (a cue broke at run time; the message carries the reason), `device`, `document` (opened, saved, save failed) and `engine` (everything else: warnings, errors, health). The same records, one JSON object per line, are written to the log file ([logging.md](logging.md)); a client may parse them with the same types.

`status`: `cpu_percent` (the engine process's CPU since the previous status, in percent of one core; `null` without a figure), `engine_load` (audio callback time over block time, worst case since the previous status; `null` until the audio thread measures it, which v0.0.1 does not yet), `xruns` (since start), `disk {streams, underrun_frames, healthy}` (`healthy`: no underrun for ten seconds), `device {name?, state, sample_rate, block_frames?, channels, latency_ms?}` (`state`: `ok`, `lost` while the engine tries to reopen it, `failed` when only a restart helps, `absent` for offline rendering) and `log_dropped` (show-log records the disk writer lost). A client that connects gets its first `status` with the next periodic one (within a second). Non-finite numbers are sent as 0.

These are additions: `PROTOCOL_VERSION` stays 1 (no released client). The Swift client (M5) must decode `status` and the `log_tail` result.

### Snapshot

`revision` (the model revision it is at), `runtime_seq` (the last `runtime` `seq` folded in), `document`, `workspace` (the show-file workspace object, [file-format.md](file-format.md) section 4), `cue_states` (cues not idle: `{cue, state, reason?}`), `problems` (`{cue, problems}` for cues with static problems), `group_warnings` (group ids with a broken descendant; such a group with no run of its own is in `cue_states` as `broken`, with no `reason` and no `problems` entry), `playheads` (`{list, cue}`), `previewing` (cue ids), `warnings`.

### Model changes

`model_changed.revision` rises by exactly 1 per message, starting from `snapshot.revision`; any other step is a gap. Apply `deletes` first, then `upserts` in order. A `delete` of an id the client does not hold is ignored; an `upsert` of a cue the client already holds replaces it wherever it is.

| Upsert `type` | Fields | Meaning |
|---|---|---|
| `cue` | `list`, `parent` (null: top level), `index`, `cue` | put the cue (new, changed or moved) at `index` among the cues of `parent`. A group's upsert carries its whole subtree and replaces it; its children are not upserted separately |
| `cue_list` | `id`, `name`, `index` | name and place among the lists; its cues come as `cue` upserts |
| `patch`, `network_patch` | `index`, `patch` | place among the patches |
| `settings` | `name`, `settings` | the workspace name and settings |

Deletes: `cue {id}` (with its children), `cue_list {id}`, `patch {id}`, `network_patch {id}`. Deleting a cue list deletes its cues with it; the engine only removes empty lists, so the cues were deleted or moved out in the same or an earlier message.

Edits to settings, patches and cue lists: one `settings` upsert (name and all settings), a `patch` / `network_patch` / `cue_list` upsert per item added or changed and a delete per item removed, and for a move (or an undo of one) an upsert of every item in that collection at its final index, ascending. A `cue_list` upsert for a list the client holds keeps its cues. They come before the cue upserts of the same message.

What the engine sends for a cue edit: a `delete` for each cue removed or moved, then an `upsert` for each cue added, moved or changed (its final place; in tree order, parents first, ascending index within a parent), so a move is a delete and an upsert in the same message and several moves in one undo step cannot collide. A cue inside a group that is also upserted is not sent separately. Only the cues the edit names are sent (an undo or redo names what the step changed), and exactly one `model_changed` goes out per edit, undo, redo or runtime model change (`SetArmed`); a client that sees a revision gap requests `snapshot`. Edits to a group itself send its whole subtree. See [control.md](control.md) "Model changes".

The `model_changed` for revision N is always sent before the `result` that reports `ok.revision = N`, so a client that has seen the result has the change.

### Runtime changes

`runtime.seq` rises by 1 per message (gap: request a snapshot). `changes[].type`:

| Type | Fields | Source |
|---|---|---|
| `cue_state` | `cue`, `state`, `reason?` | `Action::StateChanged`. `state`: `idle`, `loaded`, `pre_waiting`, `running`, `paused`, `post_waiting`, `fading`, `stopping`, `broken`. `reason` (`voice_rejected`, `stream_failed`, `device_lost`) is set when it broke while running. A `broken` state with no `reason` is a cue with static problems (listed in `problems`) or, with neither, a group with a broken descendant (`group_warning: true` is the reason; it has no problems of its own and still starts) |
| `problems` | `cue`, `problems` | `ProblemsChanged`: the full list of static problems, empty when none |
| `group_warning` | `group`, `warning` | `GroupWarningChanged`: the group has a descendant shown with problems, which is why it shows as broken (its `cue_state` is `broken` while it has no run) |
| `playhead` | `list`, `cue` | `PlayheadChanged` |
| `preview` | `cue`, `active` | `PreviewChanged` |
| `warning` | `warning` | `Warning`, and `GoIgnored` as a `go_ignored` warning |

`RuntimeChange::from_action(action, &engine)` does this mapping (the mixer-side actions map to nothing); it reads the engine after `handle` to fill `reason` for a `broken` state, which is absent for a cue broken only by static problems. Problems: `no_target`, `target_missing {target}`, `target_self`, `target_wrong_kind {target}`, `no_media`, `media_missing {media}`, `media_unsupported {media, reason}`, `patch_missing {patch}`, `device_unavailable {patch, resolution}`, `network_patch_missing {patch}`, `started_broken`; `resolution` is `mapped`, `unmapped`, `device_missing`, `channels_missing`, `output_count_mismatch`. Warnings mirror the engine's (`unknown_list`, `unknown_cue`, `not_in_list`, `number_not_found`, `cue_broken`, `trigger_ignored`, `load_ignored`, `unsupported`, `late_start {late_ms}`, `chain_limit`, `cue_removed`, `no_target`, `target_missing`, `target_wrong_kind`, `target_not_running`, `preview_ignored`, `preview_failed`, `devamp_ignored`) plus `go_ignored {list, reason}` and `notice {code, message, cue?}` for what only the binary knows. The UI localises from the variant or `code`; `message` is English.

### Error codes

`protocol_version_mismatch`, `bad_token`, `bad_request` (unreadable frame), `unknown_message`, `command_refused`, `not_found`, `no_workspace`, `unsaved_changes`, `io`, `bad_show_file`, `unsupported`, `internal`.
