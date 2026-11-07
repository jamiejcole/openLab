---
name: cue-engine
description: "Load when changing or reviewing the cue state machine in engine/crates/core/src/cue_engine/: timing, states, chains, groups, GO, playhead, second trigger, level edits and takeover, fades, panic, Stop All/Reset All, preview, broken cues, display_name (cue.rs), cues_with_hotkey (model.rs)."
---

# Cue engine (`openlab-core::cue_engine`)

Pure, deterministic core: `CueEngine::handle(&workspace, input, now) -> Vec<Action>` plus `next_deadline()`. It decides what runs when; the binary turns `Action`s into mixer/disk/OSC commands (skill `engine-control`) and feeds `Input`s back. QLab decisions and user answers: skill `qlab-behaviour`. Requirement IDs: `REQUIREMENTS.md`.

## Must not break
- A second trigger on an active cue starts nothing twice; a paused cue resumes.
- A paused show starts nothing. Every end or failure that would start a cue while paused is held until the resume.
- A zero-wait cycle can never hang a call or re-fire on later ticks at one sample (chain bound).
- A caught fault never reaches other cues: a cue breaks, its group goes on (rule 2).
- Commands for one voice never go back in time; committed deadlines cannot be withdrawn.
- A level edit or a nudged fader never cuts sound (takeover completes a Fade but does not stop its target).
- Panic ends every voice on one sample `E`; nothing scheduled fires after it.
- Core stays pure: no threads, clocks, files, sockets. Time and randomness (seeded SplitMix64) come in.
- A missed model-change signal costs a late edit, never a lost start or a lost GO.

## Shape
- `EngineConfig::new(sample_rate, lookahead, seed)`: checked, rate 1..=384000 Hz, lookahead at most one second of samples.
- Same workspace, inputs, times and seed always give the same actions in the same order (all maps ordered). Tests pass a fixed seed; the binary seeds from OS entropy.
- `handle` gets the current workspace every call. Order inside a call: follow edits since the last call, carry out due deadlines, apply the input, carry out deadlines again.
- Whole-workspace work (stop deleted cues, move playheads, recount chain limit, rebuild problems) happens only on the first call after `model_changed()` (see `references/broken-and-edits.md`).
- One engine per open workspace. A new engine needs one `handle(ws, Tick, now)` so the playheads are reported.
- Queries are `pub fn`s in `cue_engine/mod.rs`: read them there.
- `Action::Warning(EngineWarning)` never stops anything; variants are structured for UI localisation.

## Rule 1: second trigger
- Start, GO or a continue on an active cue (pre-waiting, running, post-waiting, fading, stopping) does nothing by default: no restart, no voice action, timers untouched, `TriggerIgnored { cue, state }`. GO still counts and moves the playhead.
- A trigger on a paused cue resumes it, whatever its `second_trigger` option. Options and release: `references/timing.md` "Second trigger".

## Rule 2: break vs auto-continue vs auto-follow (single home)
- Auto-continue does not depend on success. A cue that breaks keeps its auto-continue on the original schedule (action start + post-wait), also if it breaks in its pre-wait (action skipped, timing kept).
- Auto-follow needs a clean end. A cue that breaks at run time after its voice played audio (`ActionFailed`) never fires its auto-follow.
- A cue with static problems is skipped like a disarmed cue, so its auto-follow fires at its action start.
- A cue whose voice fails before any audio (`StartFailed`) was only skipped: its auto-follow fires on the failure sample. It reports `broken` while the continue is pending; Stop or Reset cancels it. Its voice is released at once.
- Applies to single cues and to children of timeline, random and start-first groups. A group whose run saw a child break at run time does not auto-follow; its auto-continue still fires.
- A playlist whose current child breaks while playing halts and keeps the group's auto-follow instead (`references/groups-playhead.md`). A broken child met by a playlist is skipped and the group still follows out.
- `device_lost` ends the run as a stop, with no continues at all (`references/panic-transport.md`).

## Paused show starts nothing (single home)
- Failure while paused (`ActionFailed`/`StartFailed`, not `device_lost`): the voice is lost at once; a skipped cue's auto-follow and a playlist's next child are held to the resume. A Devamp's next cue is cancelled, not held.
- Clean end dated at or before the pause: a cue's auto-follow (when it would start a sibling), a Devamp's next cue and a runless playlist's next child are held; the cue shows `paused`, never broken.
- A held end is carried out on the resume sample (Resume, Resume All, or a trigger that resumes it) against the workspace as it is then.
- Stop, Reset, Panic and Stop All cancel the hold. Other feedback while paused only sets lengths or is stale.
- A group whose last child leaves while paused ends on the resume; a running playlist moves on at the resume.
- A failed playlist child shows `paused` until that resume, then `broken`; any other cue that failed while paused shows `broken` at once, a stopped one too.
- Matching rules and the end-vs-pause tie: `references/timing.md` "Feedback".

## Chain bound (single home)
- Zero-wait chains run inside one call (an auto-continue chain of zero waits starts every cue on one sample). Trigger cycles are possible (edits create them; commands do not check references).
- The calls at one `now` start at most `chain_limit(ws)` = cue count in the workspace (all depths) + 64 cues between them. A call at another `now` has the whole limit.
- Every start that creates or resumes a runtime counts: children started by their group, Start cues, GoTo-and-GO, Devamp's start-next-cue, second-trigger restarts and playlist next/previous, follow-through children.
- A trigger that starts nothing (`TriggerIgnored`, `CueBroken`, `UnknownCue`) costs nothing, so a stuck controller key cannot spend the budget a GO needs. A cycle still ends because each lap creates or resumes a runtime.
- Beyond the limit a start is dropped with `ChainLimit`. An operator GO that finds the budget spent is `GoIgnored { reason: chain_limit }`: no start, no playhead move, double-GO interval not restarted.
- A playlist stops starting children when `PLAYLIST_RESERVE` (8, `mod.rs`) starts are left, so a looping playlist of zero-length children cannot starve the cue after its group.
- Cut, not deferred: a cycle never re-fires on later ticks at the same sample. The deadline loop also has its own step guard.
- Why per sample, not per call: feedback can be dated on the very sample that started the lap (a voice that ends on its start sample), and a binary may hand it over at the same `now`. Such a lap repeats call after call only until the sample's budget is spent. In the real binary `now` advances a block, so it runs once per block like a timed loop.
- A cycle that moves time (non-zero wait, Wait or Fade duration) is a timed loop, not a hang; Stop, Reset and Panic end it.
- A cue acting while its own action is under way ignores a trigger back to itself (`TriggerIgnored`).

## Reviewer checklist
- Does a new start path charge the chain budget and respect rule 1?
- Does a new "ends" path respect rule 2 and the paused hold?
- Is every new command for a voice dated no earlier than the run's last committed command?
- Does anything new read the clock or I/O in core, or iterate a hash map?
- Does a new input name a cue/run pair and ignore stale runs?
- Does an edit path signal `model_changed` / `model_changed_cues`?
- Tests use the virtual clock (`now` passed in) and a fixed seed.

## References (read when)
- `references/timing.md`: deadlines, lookahead, late starts, per-voice monotonic commands, states, run ids, timer progress, timing equations, load-to-time, the operation and event tables, second-trigger options, feedback matching. Read for any timing, state or trigger change.
- `references/groups-playhead.md`: group action, modes, playlist, halted playlist, random, child started on its own, group operations, GO, double-GO, playhead, stepping. Read for group, GO or playhead work.
- `references/kinds.md`: control cues, GoTo, Arm/Disarm, Devamp, Wait/Memo, Network cue semantics, `display_name`. Read when touching a cue kind's action.
- `references/fade-levels.md`: Fade cues and live level edits on playing cues, volume limits, edit takeover. Read for Fade or level code.
- `references/panic-transport.md`: Panic, Stop All, a cue's own panic, Reset All, Pause/Resume All, previews. Read for any stop-everything or preview change.
- `references/broken-and-edits.md`: static problems, availability inputs, incremental cache contract, model-edit following, running groups under edits. Read for broken-cue or edit work.
