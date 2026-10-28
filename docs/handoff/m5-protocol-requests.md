# Protocol requests from the macOS app (M5)

Things the UI needs from the engine side that the protocol does not carry yet. Each is a request, not a
decision: the engine and `docs/protocol.md` own the shape.

## Media duration per audio cue (M5.4)

The cue list's Duration column is blank for audio cues because the model has no media length. Request:
the probed duration (seconds, after start/end trims or both the file length and the trimmed length) for
each audio cue, in the snapshot and in `cue` upserts, updated when a probe finishes or media is
relinked. Until then the column shows the Fade and Wait durations and the running time of a cue.

## Edit the transport keys (M5.11)

The app reads the per-workspace transport keys from `settings.hotkeys` (go, stop_all, pause_all,
resume_all, playhead_next, playhead_previous) but nothing can change them. Requests:

1. A command (or settings request) that edits `settings.hotkeys`: undoable, validated against
   duplicates among the transport keys and against cue hotkeys, Esc and bare arrows refused.
2. Fields for the selected-cue keys, which the app has as fixed defaults today: load selected (`L`),
   pause/resume selected (`P`), start selected, stop selected, reset selected.

Defaults to reconcile: a new workspace has `stop_all` = Esc (the app keeps Esc as panic, so it binds
no Stop All key from it) and `playhead_next/previous` = bare Down/Up (the app treats these as "the
cue selection moves the playhead", as QLab does, and binds no transport key; QLab's own menu keys
are Shift-Cmd-Down/Up).

## Inspector basics gaps (M5.6)

QLab 5's Basics tab has controls the model has no field for; the inspector omits them until it does:

1. Auto-load and "skip if disarmed" per cue (and a workspace default for auto-load).
2. A second colour after first start; QLab offers 20 colours, the model has none plus five.
3. Editing a Wait or Fade cue's duration (the inspector shows it read-only): a duration is inside `kind`, so
   it needs a `CueProperty` of its own or the app must send a whole `Kind` replacement.
4. Cue-number edits send `set_cue_numbers`. A "default name" (QLab shows a content-derived name in grey
   when the name is empty and clearing a name restores it) has no counterpart in the model.

## Cue hotkeys (M5.7)

Done on the engine side (M4.15f): the app-scope `hotkey { id, key, modifiers, phase: "down" | "up" }`
(docs/protocol.md section 3), sendable on any connection, matches every doc's cue hotkeys (a doc whose
transport keys use the key starts nothing) and sends the release to the same cues. The Swift codec has it
(`ProvisionalClientMessage.hotkey`), but the app still does the fan-out itself (per-workspace
`transport: start` / `trigger_released`): M5 should switch to the message, so the engine owns the matching.

Also: the app refuses (at record time) a cue hotkey that equals a transport key, a menu key or another
cue's hotkey; the engine's `SetCueProperty::Triggers` accepts any of them, so a second client can still
create the duplicates. Core could warn in `validate`.

## Show Mode (M5.12)

Show Mode (SM-01) is UI-only state, kept per window in memory and lost on relaunch; the engine does not know it.
SM-07 (no system sleep while a workspace is open in Show Mode, engine M4.9) needs it: wanted is a per-doc
`set_show_mode { on }` command (or a flag on `document`) so the engine can hold the sleep assertion and a
second client can show the same mode. Until then the app does not tell the engine.

## Undo and redo of an active cue (M5.10a)

Wanted: an `undo` / `redo` that would remove an active cue (running, paused, waiting, fading) should be refused, or need a flag, like the app's own delete guard. Today the engine stops the cue's voice and warns `CueRemoved` (docs/cue-engine.md), and the client cannot predict what an undo does, so Cmd-Z can cut a sound the audience hears.

## Media paths from drops and `Kind` replacement (M5.10b)

The app sends the dropped file's absolute path (forward slashes, NFC) in `AddCue` and in `SetCueProperty { kind }`
when a file replaces an audio cue's file. Wanted: the engine stores it relative to the show folder when inside it
(docs/file-format.md §4.3), on `AddCue` and `Kind` alike; the app does not know the show's folder rules.
Also wanted: a `SetCueProperty` variant for an audio cue's `media` alone, so a replace does not send (and so
overwrite) the whole kind with the app's copy of it.
Paste: the wire has only `paste`, which wants cues already prepared (docs/commands.md: `prepare_paste` is a Rust
function). The app re-implements `prepare_paste` in `CueEditor.preparePaste` (fresh ids, targets follow or clear,
used numbers cleared, missing patch / network destination becomes the first) from the model it holds, so it can
drift from core. Wanted: a `paste` form that takes the raw clipboard and runs `prepare_paste` in the engine
(fresh ids made by the engine), then delete the Swift copy.

## Several workspaces (M4.15f landed; M5.2, M5.19)

The engine side of ARCHITECTURE section 6 is in (protocol 2; docs/protocol.md section 2): `hello { workspace }`
binds a connection to a doc, the app connection gets `welcome { workspaces }` and `workspaces` updates,
`new`/`open`/`open_recovery` answer `opened { doc }`, `close` / `close { doc }` with `closed` to the doc's
connections, `already_open`, the app-scope `panic` and `hotkey`, `set_output_overrides` on any connection.
The app's minimal changes (M4.15f) make it work against this engine: protocol version 2, doc ids are JSON
numbers (kept as a `ModelID` of their digits: `ModelID(doc:)`, `docNumber`), no `discard_changes` on
new/open and no `revert`. What the app (M5) must still do:

1. **Rename the provisional layer**: `ProvisionalProtocol.swift` and the `.provisional` cases are the real
   messages now; fold them into `ClientProtocol.swift` / `ServerProtocol.swift`.
2. **Drop the one-workspace fallback** (`EngineSession.Mode.single`, `singleDoc`, `FakeEngine` single mode):
   a protocol 1 engine is refused at `hello`, so single mode is never reached against a real engine.
3. **Order windows and stores by open order, not by the doc id's string** (`AppController` sorts
   `stores.keys` by `raw`: "10" sorts before "9"); the `workspaces` list is in open order.
4. **Esc**: the app sends `transport: panic` on the app connection, which the engine takes as the
   app-scope panic; send `panic { id }` instead (it also works on a bound connection).
5. **Hotkeys**: send `hotkey` (above) instead of fanning out `start` / `trigger_released`.
6. **`closed`**: a bound connection gets `closed { doc }` and the engine closes it; the app reacts to the
   `workspaces` list today (and a reconnect is refused `no_workspace`, which stops the client). Handle
   `closed` on the store's connection too, so a window closed from elsewhere (another client, OSC later)
   goes at once and the client does not try to reconnect.
7. **Reattach after an engine restart (M5.2)**: doc ids restart from 0 in a new engine run (with a new
   token), so a window must rebind by reopening its file (`open`), never by its old doc id.
8. **The engine still starts with one untitled doc** (doc 0) until M4.15g; the app then decides what to
   open (recovery, last workspaces) and closes that untitled doc if it does not want it.
9. **Copies of an open show** open with a fresh workspace id and unsaved (`dirty`); the app's save prompt
   on close covers it.
