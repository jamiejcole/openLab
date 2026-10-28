<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Open QLab 5 behaviour questions

Behaviour questions that the official QLab 5 documentation (https://qlab.app/docs/v5/) does not settle. The documentation is always searched first; a row exists only when it gave no answer. For each one, openLab has
implemented the assumption shown, so work is not blocked; the user answers here when they can,
and the orchestrator changes the code if an answer differs.

Status: **assumed** (implemented on the assumption, awaiting the user), **answered** (the user
confirmed or corrected; the change, if any, is linked).

| # | Question | Assumed (implemented) | Where | QLab 5 docs checked (qlab.app/docs/v5/) | Status |
| --- | --- | --- | --- | --- | --- |
| 1 | A cue whose file fails (stream error) while it fades out in a Panic or a stop fade: does it show **broken** or just **stopped**? | Broken (the failure stays visible); nothing follows it, as for a stop | M2.31, `docs/cue-engine.md` Panic | audio/audio-cues, audio/fading-audio, scripting/osc-dictionary-v5: broken is described only for configuration problems | assumed |
| 2 | Under Pause All, a paused playlist child's file fails: does the playlist hold its next child until resume, or move on? | Hold until resume (a paused show starts nothing) | M2.31, `docs/cue-engine.md` Pause All | fundamentals/group-cues, fundamentals/workspace, fundamentals/cue-lists, other-cues/transport-cues | assumed |
| 3 | A cue paused **on its own** (not Pause All) whose sound had in fact just ended: does its auto-follow / playlist step wait for that cue's resume? | Wait for the resume (the engine can't tell a single-cue pause from Pause All; the safer direction) | M2.33 | fundamentals/inspector, fundamentals/cue-sequences, other-cues/transport-cues: follows documented only for unpaused cues | assumed |
| 4 | When a follow held over a pause fires on resume, does the next cue start fresh at the resume moment, or part-way in "as if never paused"? | Fresh, on the resume sample, no late-start warning | M2.33 | as row 3; transport-cues: a Start cue un-pauses its target, nothing on timers | assumed |
| 5 | A halted playlist (a child broke while playing) with unknown or endless lengths, or looping: does its auto-follow ever fire? | No follow | M2.27, `docs/cue-engine.md` | fundamentals/group-cues: only that looping needs a non-zero duration | assumed |
| 6 | Shuffle switched on while a looping playlist plays: does the current pass reshuffle? | Keeps list order until the next pass | M2.27 | fundamentals/group-cues: shuffles "each time the Group is loaded or started" (consistent, not conclusive) | assumed |
| 7 | A trimmed cue shorter than 51 ms is refused (QLab refuses the trim); should the audio engine's own loop floor (`MIN_LOOP_MS`, 10 ms) be raised to match? | Leave the loop floor at 10 ms | M2.27 | audio/audio-cues, fundamentals/inspector: no trim or loop minimum (slice markers at least 0.05 s apart) | assumed |
| 8 | A **loaded** show whose cue hotkey equals a transport key (e.g. Space): which wins? (Assigning one is refused, per the docs; openLab's editor refuses too.) | The transport key; the cue hotkey does not fire and one warning is logged | M2.27, M5.7/M5.11 | fundamentals/inspector: a hotkey can't use keys assigned in Workspace Settings > Keyboard; nothing on a saved conflict | assumed (partly settled by the docs) |
| 9 | A child started on its own while its playlist post-waits: does it carry the playlist on if it ends **after** the post-wait? | Yes (it does not if it ends during the post-wait, which QLab confirmed) | M2.27 (review L1) | fundamentals/group-cues: children auto-continue, post-wait = duration; nothing on this | assumed |
| 10 | Opening a workspace whose id duplicates an open one: the engine gives it a fresh id and marks it dirty. Does QLab persist the new id on save? | Yes, saved with the new id | M4.15f | fundamentals/managing-workspaces, workspace-settings, osc-dictionary-v5: uniqueID exists, nothing on duplicates | assumed |
| 11 | Selecting several cues by hand: which one does the playhead move to? | The topmost selected cue | M5.11 | general/keyboard-shortcuts (Load Last Selected: the most recently selected); osc-dictionary-v5 `/playhead/selected`: the first selected. Conflicting signals | assumed |
| 12 | Does QLab 5 refuse a malformed OSC address or argument list typed into a Network cue, or accept it and fail at send? | Refuse at the edit (a new value must be valid by itself; a cue loaded invalid can still be repaired step by step); destination must be an existing network patch | M4.6a, `CueProperty::{Destination, Address, Args}` | networking/network-cues (message format only; nothing on invalid input) | assumed |
| 13 | Do workspace, cue list, patch and network patch names have a maximum length or forbidden characters (newlines, control characters)? | A new name is at most 256 Unicode scalar values with no control character (including newline and tab); a network host at most 253 bytes (DNS) with none either. Empty is allowed. A loaded show keeps odd values with a warning. Only control characters (Unicode Cc) are refused: bidi overrides/isolates, U+2028/2029, zero-width spaces and a BOM are allowed | M1.20, `command_model.rs` `check_name`, `validate.rs` `name_problem`/`host_problem` | fundamentals/workspace-settings (patch names "optional and free-form", only OSC-by-name patches avoid spaces and reserved characters), fundamentals/managing-workspaces: no limit or character rule found | assumed |

## Settled by the documentation

- OSC over TCP uses double-ended SLIP (RFC 1055, OSC 1.1), no length prefix: scripting/osc-dictionary-v5. Matches `docs/osc.md` (was M4.6b's open point).
