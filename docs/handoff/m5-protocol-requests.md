# Protocol requests from the macOS app (M5)

Things the UI needs from the engine side that the protocol does not carry yet. Each is a request, not a
decision: the engine and `docs/protocol.md` own the shape.

## Media duration per audio cue (M5.4)

The cue list's Duration column is blank for audio cues because the model has no media length. Request:
the probed duration (seconds, after start/end trims or both the file length and the trimmed length) for
each audio cue, in the snapshot and in `cue` upserts, updated when a probe finishes or media is
relinked. Until then the column shows the Fade and Wait durations and the running time of a cue.

## Transport key map is per workspace in QLab, app-wide here (M5.11)

QLab keeps its keyboard controls per workspace. The show file already has `settings.hotkeys` (go,
stop_all, pause_all, resume_all, playhead_next, playhead_previous) but no command changes it, and it
has no entries for the selected-cue keys (load, start, pause/resume, stop, reset selected). Until then
the app keeps one map in `UserDefaults` (`transportKeyMap`) for every workspace. Request: a command
(or settings request) that edits `settings.hotkeys` (undoable, validated against duplicates and cue
hotkeys), and fields for the selected-cue keys. Engine defaults to reconcile: `stop_all` is Esc (the
app reserves Esc for panic) and `playhead_next/previous` are bare Down/Up (QLab's defaults are
Shift-Cmd-Down/Up; bare arrows move the cue selection).
