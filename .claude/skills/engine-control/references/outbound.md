# Outbound messages

Wire shapes, notice names and warning names are in `docs/protocol.md`; this lists when the loop sends what.
- `runtime { seq, changes }`: one message per engine call with changes (from `RuntimeChange::from_action` read right after the call; a broken state carries its reason), plus binary notices as `warning { notice }`.
- `warnings`: the workspace-level list, replaced when it changes, rebuilt at most once per tick (including device, media, mixer-patch and fault entries). Broken cues are not in it: per-cue rows come from `problems` (snapshot and `runtime` `problem` changes) carrying the cue id, so an edit pays nothing for them.
- **Slice-plan checks** run with the list: each audio cue's plan is checked against its file's probe, keeping each cue's result and rebuilding a plan only when its trims/markers or its file's probe change (an edit of one cue builds at most one plan, `plan_builds()`).
  - `PlanChecks::take_changes` gives the engine `Input::SliceProblems`: cues whose plan cannot be built, and those whose plan builds again or has gone, plus every rebuilt unplayable plan (the engine forgets a report on edit of its plan inputs). A GO in the same burst as a fix plays the fixed cue.
  - The same plans give each audio cue's nominal length (`total_frames` from the top; `None` if it loops for ever or has no plan) as `Input::CueLengths` for the changed ones, used by a halted playlist to time its auto-follow.
  - And each cue's media lengths (file, trimmed range, duration with play counts): changed ones go as one `runtime` message of `media_duration` changes (fields absent for a cue no longer playable or gone); the snapshot's `media_durations` is what was last sent. One edit or one missing file sends one change, never a snapshot.
- `progress`: show cues with a playing voice (elapsed/remaining; remaining `null` while looping for ever;) plus `CueEngine::timers` at the rendered sample (actions of cues without a voice join `items`, pre/post-waits go in `waits`); at most 30 Hz of sample time, only while something plays or counts; read on the doc's report pass, adding no wake.
  - `position` (file time the voice is at) comes from the voice record's slice table and the cuts of devamps the mixer accepted (`DevampScheduled.at`), computed on the control thread with no audio-thread change.
- `meters`: per mapped patch the device outputs it reaches, per cue its voices' peaks per output, read into preallocated buffers; at most 30 Hz, while audio plays plus one message after.
- `log`: engine faults and warnings at their levels (blocks replaced with silence, voices dropped after repeated faults, each stage fault at most once a second per stage, audio-thread allocations, lost mixer events, unsignalled model changes, device loss with the number of cues stopped, failed reopen attempts, device back). All also go to the show log (skill `engine-lifecycle`).
- `status` (when `ControlConfig::status` is set): system health about once a second of wall-clock time and early on a change; lossy like meters.

## Full channel policy
`OUTBOUND_CAPACITY` bounded channel, `try_send`: the loop never blocks.
- Meters and progress are dropped first, once half full (the next replaces them).
- A sequenced message (`runtime`, `model_changed`) or log that does not fit is dropped; clients recover from the `seq`/`revision` gap with a snapshot.
- A `document` or `warnings` message that does not fit is re-sent, newest, once there is room.
- Drops are logged at `warn` once there is room.
