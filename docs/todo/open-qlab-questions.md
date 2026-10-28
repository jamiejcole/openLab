<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Open QLab 5 behaviour questions

Behaviour questions that QLab's documentation does not settle. For each one, openLab has
implemented the assumption shown, so work is not blocked; the user answers here when they can,
and the orchestrator changes the code if an answer differs.

Status: **assumed** (implemented on the assumption, awaiting the user), **answered** (the user
confirmed or corrected; the change, if any, is linked).

| # | Question | Assumed (implemented) | Where | Status |
| --- | --- | --- | --- | --- |
| 1 | A cue whose file fails (stream error) while it fades out in a Panic or a stop fade: does it show **broken** or just **stopped**? | Broken (the failure stays visible); nothing follows it, as for a stop | M2.31, `docs/cue-engine.md` Panic | assumed |
| 2 | Under Pause All, a paused playlist child's file fails: does the playlist hold its next child until resume, or move on? | Hold until resume (a paused show starts nothing) | M2.31, `docs/cue-engine.md` Pause All | assumed |
| 3 | A cue paused **on its own** (not Pause All) whose sound had in fact just ended: does its auto-follow / playlist step wait for that cue's resume? | Wait for the resume (the engine can't tell a single-cue pause from Pause All; the safer direction) | M2.33 | assumed |
| 4 | When a follow held over a pause fires on resume, does the next cue start fresh at the resume moment, or part-way in "as if never paused"? | Fresh, on the resume sample, no late-start warning | M2.33 | assumed |
| 5 | A halted playlist (a child broke while playing) with unknown or endless lengths, or looping: does its auto-follow ever fire? | No follow | M2.27, `docs/cue-engine.md` | assumed |
| 6 | Shuffle switched on while a looping playlist plays: does the current pass reshuffle? | Keeps list order until the next pass | M2.27 | assumed |
| 7 | A trimmed cue shorter than 51 ms is refused (QLab refuses the trim); should the audio engine's own loop floor (`MIN_LOOP_MS`, 10 ms) be raised to match? | Leave the loop floor at 10 ms | M2.27 | assumed |
| 8 | A cue hotkey that is also a transport key (e.g. Space): which wins? | The transport key; the cue hotkey does not fire and one warning is logged | M2.27, M5.7/M5.11 | assumed |
| 9 | A child started on its own while its playlist post-waits: does it carry the playlist on if it ends **after** the post-wait? | Yes (it does not if it ends during the post-wait, which QLab confirmed) | M2.27 (review L1) | assumed |
| 10 | Opening a workspace whose id duplicates an open one: the engine gives it a fresh id and marks it dirty. Does QLab persist the new id on save? | Yes, saved with the new id | M4.15f | assumed |
| 11 | Selecting several cues by hand: which one does the playhead move to? | The topmost selected cue | M5.11 | assumed |
| 12 | OSC over TCP: SLIP framing only, or also the OSC 1.0 length prefix? | See `docs/osc.md` (M4.6b) | M4.6b | assumed |
