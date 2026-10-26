# openLab UI protocol (engine <-> UI)

Types: `openlab-core::protocol` (`engine/crates/core/src/protocol/`). They are the source of truth; this page is the spec the Swift types follow. One golden JSON file per message is in `engine/fixtures/protocol/{client,server}/` (the Swift tests decode every one). Architecture: ARCHITECTURE section 6. Model edits are core's `Command` ([commands.md](commands.md)); cue states and warnings are the cue engine's ([cue-engine.md](cue-engine.md)).

## 1. Conventions

- **Transport**: WebSocket on `127.0.0.1`, random port; one JSON object per text frame, UTF-8. The server (M4.3) is not part of the types crate.
- **Auth**: the engine writes `{port, token, pid}` to `<app-support>/openLab/engine.json` (mode 0600). The client's first frame must be `hello` with the token.
- **Envelope**: `{"type": "<snake_case name>", ...fields}`. Nested variants (transport actions, requests, commands, runtime changes, upserts, problems, warnings, result data) are also internally tagged with `"type"`.
- **Units**: times are **seconds** (f64), as in the show file, never samples. Levels in the model are dB (as the show file). Meter peaks are **linear amplitude** (f32): 0.0 is silence, 1.0 full scale, above 1.0 a clip. Ids are the model's UUID strings. `RequestId` is a non-negative integer (u64) the client chooses, unique on its connection. `revision` and `seq` are u64.
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
| `command` | `id`, `command` | `command` is core's `Command` ([commands.md](commands.md)): one undo step. The engine replies `model_changed`, then `result { ok: { revision } }`; a refused command changes nothing and replies `error.code = command_refused` |
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
| `new` | - | new empty workspace; `snapshot` message, then `result` |
| `open` | `path` | load a show; `snapshot`, `warnings`, then `result`; errors `io`, `bad_show_file` |
| `save` | - | save to the current path; `document` update, `result`; `no_workspace` if never saved (use `save_as`) |
| `save_as` | `path` | |
| `revert` | - | reload the saved file, dropping edits (clears undo); `snapshot`, `result` |
| `relink` | `folder` | relink missing media (WS-03) as one undo step; `result { ok: { revision, data: { type: "relink", relinked, ambiguous: [{cue, candidates}], unmatched } } }`; nothing is guessed between duplicates |
| `list_devices` | - | `result { ok: { data: { type: "devices", devices: [{name, output_channels}] } } }` |
| `snapshot` | - | `snapshot`, then `result` |

Paths are never empty (`bad_request` on decode).

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
| `log` | `level` (`error`, `warn`, `info`, `debug`), `message` | |
| `warnings` | `warnings [warning]` | workspace-level notices in force (load repairs, damaged media); **replaces** the previous list; empty clears it |
| `result` | `id`, then `ok {revision?, data?}` or `error {code, message}` | exactly one of `ok` / `error` |
| `error` | `code`, `message`, `protocol_version` | not tied to a request (see section 2) |

### Snapshot

`revision` (the model revision it is at), `runtime_seq` (the last `runtime` `seq` folded in), `document`, `workspace` (the show-file workspace object, [file-format.md](file-format.md) section 4), `cue_states` (cues not idle: `{cue, state, reason?}`), `problems` (`{cue, problems}` for cues with static problems), `group_warnings` (group ids with a broken descendant), `playheads` (`{list, cue}`), `previewing` (cue ids), `warnings`.

### Model changes

`model_changed.revision` rises by exactly 1 per message, starting from `snapshot.revision`; any other step is a gap. Apply `deletes` first, then `upserts` in order.

| Upsert `type` | Fields | Meaning |
|---|---|---|
| `cue` | `list`, `parent` (null: top level), `index`, `cue` | put the cue (new, changed or moved) at `index` among the cues of `parent`. A group's upsert carries its whole subtree and replaces it; its children are not upserted separately |
| `cue_list` | `id`, `name`, `index` | name and place among the lists; its cues come as `cue` upserts |
| `patch`, `network_patch` | `index`, `patch` | place among the patches |
| `settings` | `name`, `settings` | the workspace name and settings |

Deletes: `cue {id}` (with its children), `cue_list {id}`, `patch {id}`, `network_patch {id}`.

The `model_changed` for revision N is always sent before the `result` that reports `ok.revision = N`, so a client that has seen the result has the change.

### Runtime changes

`runtime.seq` rises by 1 per message (gap: request a snapshot). `changes[].type`:

| Type | Fields | Source |
|---|---|---|
| `cue_state` | `cue`, `state`, `reason?` | `Action::StateChanged`. `state`: `idle`, `loaded`, `pre_waiting`, `running`, `paused`, `post_waiting`, `fading`, `stopping`, `broken`. `reason` (`voice_rejected`, `stream_failed`, `device_lost`) is set when it broke while running |
| `problems` | `cue`, `problems` | `ProblemsChanged`: the full list of static problems, empty when none |
| `group_warning` | `group`, `warning` | `GroupWarningChanged` |
| `playhead` | `list`, `cue` | `PlayheadChanged` |
| `preview` | `cue`, `active` | `PreviewChanged` |
| `warning` | `warning` | `Warning`, and `GoIgnored` as a `go_ignored` warning |

`RuntimeChange::from_action(action, &engine)` does this mapping (the mixer-side actions map to nothing); it reads the engine after `handle` to fill `reason` for a `broken` state, which is absent for a cue broken only by static problems. Problems: `no_target`, `target_missing {target}`, `target_self`, `target_wrong_kind {target}`, `no_media`, `media_missing {media}`, `media_unsupported {media, reason}`, `patch_missing {patch}`, `device_unavailable {patch, resolution}`, `network_patch_missing {patch}`, `started_broken`; `resolution` is `mapped`, `unmapped`, `device_missing`, `channels_missing`, `output_count_mismatch`. Warnings mirror the engine's (`unknown_list`, `unknown_cue`, `not_in_list`, `number_not_found`, `cue_broken`, `trigger_ignored`, `load_ignored`, `unsupported`, `late_start {late_ms}`, `chain_limit`, `cue_removed`, `no_target`, `target_missing`, `target_wrong_kind`, `target_not_running`, `preview_ignored`, `preview_failed`, `devamp_ignored`) plus `go_ignored {list, reason}` and `notice {code, message, cue?}` for what only the binary knows. The UI localises from the variant or `code`; `message` is English.

### Error codes

`protocol_version_mismatch`, `bad_token`, `bad_request` (unreadable frame), `unknown_message`, `command_refused`, `not_found`, `no_workspace`, `io`, `bad_show_file`, `unsupported`, `internal`.
