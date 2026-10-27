# Protocol requests from the macOS app (M5)

Things the UI needs from the engine side that the protocol does not carry yet. Each is a request, not a
decision: the engine and `docs/protocol.md` own the shape.

## Media duration per audio cue (M5.4)

The cue list's Duration column is blank for audio cues because the model has no media length. Request:
the probed duration (seconds, after start/end trims or both the file length and the trimmed length) for
each audio cue, in the snapshot and in `cue` upserts, updated when a probe finishes or media is
relinked. Until then the column shows the Fade and Wait durations and the running time of a cue.
