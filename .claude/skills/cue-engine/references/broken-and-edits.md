# Broken cues and following model edits

Problems are found before a cue is triggered (NFR-07), so the operator sees them in the list, inspector and pre-show check. Core does no I/O: the binary reports media and device facts as inputs; target and reference problems come from the model. Sources: `broken.rs`, `cue_index.rs`, `edits.rs`, `shape.rs`, `cross_check.rs`.

## Problems (`CueProblem`, in this order)
- `no_target`, `target_missing`, `target_self`: targeting cues (Fade, Start, Stop, Pause, Load, Reset, Devamp, GoTo, Arm, Disarm). Model.
- `target_wrong_kind`: Fade needs Audio or Group; Devamp needs Audio or a playlist group. Same rule as load-time validation.
- `nothing_to_fade`: Fade with no level set (`fade-levels.md`).
- `no_media`: Audio with no file. `media_missing`, `media_unsupported`: from `Input::MediaStatus`.
- `slices_unplayable { reason }`: from `Input::SliceProblems` (trims, markers or play counts cannot be played with the probed file: a looping slice under 50 ms, a start trim past the end). Not shown while the cue has no file or a media problem.
- `patch_missing`: model. `device_unavailable { patch, resolution }`: `Input::PatchStatus` with any resolution but `mapped`.
- `network_patch_missing`, `network_host_invalid`, `network_message_invalid`: Network cues, from the model via `check_endpoint` / `check_message` (limits: skill `osc`).
- `started_broken`: shown alone while a cue triggered with problems, fixed since, still has that run pending.

- Not problems: damaged media that plays (the binary reports it `available`); a trigger cycle (validation warns; the engine bounds it at run time because a timed loop of Start cues is legitimate); a disarmed cue (still broken if it has problems); an unresolved network host.
- `CueProblem` implements `Display` (English, for logs); the UI localises from the variant.
- A group has no problems of its own but shows as broken while any descendant (any depth) is shown with problems (`started_broken` included). With no run it reports `broken` (`StateChanged` dated `now`), `has_broken_descendant(group)` is true, and `GroupWarningChanged` reports every change (also `false` once for a deleted group that had one).
- Such a group is not refused: GO, Start or a continue starts it, broken children are skipped, the others play. It is not in `broken_cues()`. Run-time breaks (`broken_reason`) do not count.

## Inputs
- `MediaStatus { media, status }`: `available`, `missing`, `unsupported { reason }`, `unknown` (forgets the report: assumed playable). Keyed by `MediaRef` as stored, so every Audio cue on a reference shares it. `PatchStatus { patch, resolution }` uses `resolve_patch`'s `PatchResolution`. `Availability` carries many of both in one call.
- `SliceProblems { cues: [(cue, reason?)] }`: the binary planned the slices against the probe (core cannot plan audio); `reason` set = unplayable, absent = playable or no longer planned.
- A slice report is kept with the cue's plan inputs at that moment (file, trims, markers, play counts) and counts only while the cue still has them: an edit of any makes the slices unknown (assumed playable) until the binary reports the new plan, so a GO right after a fix is never refused for the old report.
- Nothing reported = no problem: the engine cannot know a file is missing until told. When to send each: skill `engine-control`. A binary playing an unmapped patch on a default device sends the `mapped` resolution it uses.

## State, triggering, pre-show
- A cue with problems and no runtime reports `broken`. With a runtime it keeps its runtime state (a playing cue whose file is reported missing plays on; the audio side's response arrives as feedback) and reports `broken` when the run ends. A loaded cue that gets problems unloads (`UnloadVoice`) and reports `broken`. One that gets problems in its pre-wait skips its action at the pre-wait's end.
- Triggering (Start, GO, continue, Start/GoTo cue): a cue with problems does not start (`CueBroken`) but keeps its timing exactly like a disarmed cue: pre-wait, action skipped, post-wait. Its continue fires on schedule, an auto-follow at its skipped action start; it reports `broken` throughout (no `pre-waiting`). GO on a broken auto-follow cue starts the cue it follows into and moves the playhead past that too.
- A group with such a child runs the child's timing and still auto-follows. A broken Devamp cue sends no devamp.
- Fixing the problems while that run is pending does not start the skipped action; the run ends as it was, meanwhile `started_broken` shows (a broken cue always has a reason), then `[]`. Stop or Reset cancels its continue. Load warns `CueBroken` and loads nothing. Reset does not clear static problems.
- Pre-show: `broken_cues()` lists every cue with problems in workspace order. Run-time breaks are in `broken_reason(cue)` / `cue_states()`.
- A cue whose problems changed gets `ProblemsChanged { cue, problems }` (full list; empty = fixed) and, if its state changed, `StateChanged` (`idle` <-> `broken`) dated `now`. The first call reports every broken cue. A deleted broken cue reports `idle` once.

## Model-change contract (core side)
- The model-derived part (targets, patches, media references, indexes from reference and patch to cues) is refreshed only on the first call after `model_changed()` or `model_changed_cues(&footprint)`, so a show with thousands of cues pays only what an edit touched. The binary calls one after every workspace change (command, undo, redo, relink, applied `SetArmed`, newly opened workspace) before the next `handle` (mechanics: skill `engine-control`).
- `model_changed()` rebuilds everything. `model_changed_cues` recomputes the footprint's cues, cues the edit added (group descendants included) and every cue whose problems depend on one of them.
- Dependency rule: a cue's problems come from its own content, the existence and kind of its target (`can_target` reads the kind and a group's mode) and the patch and network-patch id sets. So a touched, added or removed cue refreshes the cues that target it (reverse index `by_target`); group warnings are recounted for groups a cue was added to, removed from or moved between, and around any cue that started or stopped showing problems.
- If the engine cannot follow from the footprint (patch or list id set differs, duplicate cue id, a cue the index cannot place) it rebuilds everything. A footprint that misses a changed cue breaks the contract like a missed signal.
- `CueIndex` records each cue's container and order, followed from the footprint and checked step by step so a stale record never yields a wrong cue. An added subtree is new as a whole: a cue in it that the index already had (an id removed and added again in one edit, as a transaction or its undo does) is reported gone and fresh, with all its old and new descendants.
- When anything was added, `reconcile` walks every added, removed or moved cue as an added top, because a cue moved out of an added subtree later in the same transaction no longer sits under the added top yet carries new content. The media catalog's `sync_cues` follows the same rule.
- Property test `commands_prop::incremental_problems_equal_a_full_rebuild` holds an engine told footprints equal to one told `model_changed()` (same actions, broken cues and group warnings). Extend it when adding a dependency.
- Guard: a cue about to be refused for its problems (Start, GO, continue, Load, action start) first has them recomputed from the current workspace; a mismatch rebuilds the problems and counts `unsignalled_model_changes()`. So a cue fixed by an unsignalled edit is never refused for the old problem.

## Following edits
- Each call compares the workspace with what the engine knows; only the first call after `model_changed()` runs the whole-workspace passes (stop deleted cues, move playheads, recount the chain limit, rebuild problems; counted by `model_syncs()`).
- Every build compares the workspace outline on every call (`shape.rs`: cue list ids, each list's top-level count, patch and network patch counts; no allocation). A difference is counted in `unsignalled_model_changes()` (the binary logs it at `error`) and the call follows the edits as if signalled.
- An edit inside a cue or group escapes the outline, so more guards make a missed signal cost a late edit, never a lost start: a refused cue is rechecked; a spent chain budget is recounted once and raised if the real limit is higher; a GO whose playhead cue is gone resyncs that list's playhead. A deleted cue still running keeps its runtime until the next signalled call.
- Debug and release behave alike. The edit cross-check (`set_edit_cross_check`, on in debug, off in release) only counts: it compares workspace and problem cache with a full rebuild (`verify_problem_cache`) and counts a missed signal. It repairs nothing, so actions are identical with and without it; it is off in release only for cost.
- An unsignalled edit that moves cues is the caller's breach: the playhead catches up at the next signalled structural edit or the next GO, as a full rebuild would. `whole_show_passes()` counts passes that still walk everything.

### Rules the passes apply
- A cue with a runtime no longer in the workspace is hard-stopped and forgotten.
- Changed times apply to timers not yet started: pre-wait is fixed at start; post-wait and continue mode at action start; the next cue is looked up when the trigger fires.
- Control and Fade cues read target and settings when their action starts (a Fade's levels, curve and stop-target-when-done; a Load cue's `load_to_time`). Playheads follow edits (`groups-playhead.md`).
- A running Wait or Fade takes an edited `duration` (`follow_duration_edits`): the action now ends at the new duration from its start (load offset and pauses counted), or on that call's `now` if already past it. A paused one stays frozen and, if past the new length, ends on the resume sample. Auto-follow fires and a Fade's stop-target-when-done stops the target on the new end. A broken, disarmed or stopping Wait/Fade is not re-timed.
- A re-timed Fade keeps its rate: the mixer's ramps go on unchanged; at the new end each level still on its way is held where the original curve has got to (`Ramp { gain, frames: 0 }`) and the Fade lets go of it. A lengthened Fade's targets report `fading` until it completes.
- A Fade paused when its duration is edited, or edited then paused, resumes to where its original curve is at the new length (`FadeRamp::gain_at`); each resumed ramp ends at the earlier of the new end and its planned end moved on by the pause.

### Running groups under edits
- A deleted child is stopped like any deleted cue (if it was the last busy child, the group stops). A deleted group stops with all descendants.
- A child added to a running timeline is not started. A playlist plays it when it comes up (unshuffled) or from the next pass (shuffled). Reordering applies to children started later.
- A group waits on the runs that started in it (its children, or children started on their own while in it), wherever those cues are moved while they run.
- A busy cue moved into a running group neither keeps it running nor, if it breaks, cancels its auto-follow; its end starts nothing there.
- A busy child moved out keeps running and the group goes on waiting for it (a drag is not a completion, so nothing starts on the drag). When it ends the group carries on as if it were inside: a running playlist moves on to the child that took its place; a group with no other busy run completes and auto-follows (a moved child that broke while playing: no follow).
- An idle playlist's follow-through from a moved child continues only while it is still that playlist's child (assumed).
- A stop or panic on the group reaches only its own tree: it lets go of children dragged out (they play on, the group neither waits for them nor hears their end), and so do the start and end of the group's run.
- A group's mode is read when its action starts and whenever it acts: a playlist changed to another mode stops moving on and completes with its current child; loop and shuffle are read when it moves on or starts a pass; a child's continue is checked against the mode when it fires. A cue whose kind changes to or from Group while it runs is stopped.
