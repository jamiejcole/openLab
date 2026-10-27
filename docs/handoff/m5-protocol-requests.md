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
