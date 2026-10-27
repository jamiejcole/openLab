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

ARCHITECTURE section 6 says a hotkey trigger is app-scope (fires in every open workspace), but the wire has
no such message. Until it exists the app does the fan-out itself: on a matching key-down it sends
`transport: start { cue }` to each open workspace's connection for every cue whose `triggers.hotkey`
matches, and on key-up `transport: trigger_released { cue }` for the cues with `second_trigger_on_release`.
Wanted: an app-scope message (proposed name `hotkey { key, modifiers, phase: "down" | "up" }`, sendable on
any connection) that the engine matches against every doc's cue hotkeys, so the engine, not the app, owns
the matching (the engine also runs OSC triggers) and a held key's release reaches the same cues. The
Swift name goes in `ProvisionalProtocol.swift` when it is agreed.

Also: the app refuses (at record time) a cue hotkey that equals a transport key, a menu key or another
cue's hotkey; the engine's `SetCueProperty::Triggers` accepts any of them, so a second client can still
create the duplicates. Core could warn in `validate`.
