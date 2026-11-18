# Timing, states, transitions, second trigger, feedback

Rule 1, rule 2, the chain bound and the paused hold are in SKILL.md; not repeated here.

## Time
- `SampleTime` is a `u64` frame count at the device rate (the mixer's `samples_rendered` clock). The show file stores seconds.
- Every conversion goes through `seconds_to_samples`: round to nearest, halves away from zero; negative and NaN give 0; beyond `u64` saturate. Convert when used. All time arithmetic saturates.
- `now` is the earliest sample a command can still hit (control loop passes `samples_rendered + one block`). It never goes backwards inside the engine (one mixer clock across a device reopen). If a caller sends it back: no panic, pending deadlines wait, double-GO does not block, each run's remembered command samples come down to it.

## Deadlines, lookahead, late ticks
- Every timer is an absolute sample deadline, computed at its deadline sample `T`, not at call time: a successor triggered by a timer that expired at `T` starts its pre-wait at `T`. Late calls never make later cues drift.
- A call carries out every deadline in `..now + lookahead` (end excluded; lookahead 0: up to and including `now`), earliest first; ties: the cue started first, so causes precede effects.
- The control loop sets lookahead to one block and ticks when `next_deadline() <= now + max(lookahead,1) - 1`: the window is exactly the next block, so no voice start lands beyond it.
- Deadlines inside the window are committed: an operator input in the same call cannot withdraw them. A stop at `now` for a voice committed to start at `T > now` is sent for `T`; a pause freezes timers at `now`.
- `set_lookahead` between calls (device came back with another block size): committed commands keep their samples. Over one second of samples: error, nothing changes.
- Commands for one voice never go backwards. Each run remembers the latest sample a command for its voice was sent for (start, stop, pause, resume, ramp, devamp); any later command, also from a later call or an operator input after a control/Fade cue committed something, is dated no earlier. A resume is never dated before its pause. A stop or pause is never before the voice's start (`at = max(now, voice start)`).
- Late rule: a voice whose deadline `T < now` starts at `now` from its start offset (head not cut) with `LateStart { late_by }`; its state change still reports `at: T` and its timers count from `T`.

## States (CUE-04)
Reported state derives in order: broken > paused > stopping > fading > pre-waiting > running > post-waiting.
- `idle` (no runtime), `loaded` (ready, optionally to a time), `pre-waiting` (a start always reports this first, even for zero pre-wait), `running` (post-wait may also count), `paused` (everything frozen; also a cue its lost device stopped, which owns nothing: Broken state below), `post-waiting` (action complete, post-wait pending), `stopping` (fading out before stop).
- `fading`: an audio cue in its action whose levels a running, unpaused Fade with a non-zero ramp is moving.
- Post-wait is a timer owned by the cue, not a phase: `running` while it elapses, `post-waiting` only after the action completed.
- A started cue is a run: `{pre-wait deadline, action progress, post-wait deadline, paused-at, fade-out end}`. Each voice segment has a `RunId`, unique for the engine's lifetime (the binary uses it as the mixer `VoiceId`). Loading or starting creates it; starting a loaded cue keeps it. Every resume gives a fresh id (`ResumeVoice { run: fresh, paused: previous }`), so feedback still on its way for the paused segment is stale. `run_of(cue)` is `None` for a cue owning no voice.

### Broken state
- Run-time break (feedback; `voice_rejected`, `stream_failed`, `device_lost`): owns no voice, cannot start until reset; its pending auto-continue still fires on schedule, except after `device_lost`, which cancels it.
- `device_lost` ends the run for good (pre-wait, auto-continue, auto-follow cancelled). What it shows (QLab 5, skill `qlab-behaviour` C-X1, Q391-Q396):
  - In its action and not on its way out (playing, fading, waiting for its probe), or counting its pre-wait or post-wait: `Slot::Lost`, reported `paused` (wire reason `device_lost`, `state_reason`, `is_lost`). It owns no run, voice or deadline (a post-waiting cue whose known end was carried out ahead in the lookahead window had no voice left for the engine to stop: the binary stops what still sounds); `timers` shows the timer that was counting frozen where the loss left it.
  - A playing one keeps the mark after the device returns; Resume / Resume All clear it (to idle, or broken while the device is away; nothing plays), a start or load with the device back clears it and starts from the top, one while the device is away is refused `CueBroken` and the mark stays; Stop, Panic, Reset, Stop All, Reset All and fade-and-stop-others clear it.
  - A group in its action, not paused, any of whose children left this way from playing (`Exit::Lost` sets `GroupRun::lost`; a waiting child does not) gets a `Slot::Lost` of its own when its action ends by a stop while such a child keeps the mark, whichever child left last and also under `Input::DeviceLost` (`end_group_as_lost`, Q397): paused, its time frozen, kept after the return.
    Resume / Resume All, Stop, Reset clear it; a start clears it at once (no device: `lost_may_clear`). Its own group is marked too.
  - A waiting one (pre-wait, post-wait) also clears by itself when its patch is reported `mapped` again (in `refresh`, reported once the problems are current, so it never flashes broken).
  - Paused by the operator, being stopped or panicked, or a `StartFailed` (never played): `broken`, clearing once the cue's patch is reported `mapped` again (`PatchStatus`, or when the run settles with the patch mapped; a patch never reported does not count).
- A run that broke with a pending continue keeps showing `broken` until it fires, even if the device is back: clearing early would show a voiceless run as running.
- `Input::DeviceLost` (sent when the last device that plays is lost, after the binary failed the audio runs) hard-stops every other active run, so nothing scheduled before the loss fires after it. The loss of one of several devices fails only the runs on it.
- Static problems: `broken-and-edits.md`.

## Timer progress
`timers(at)` lists every counting timer at rendered sample `at` for the `progress` message (wire shape: `docs/protocol.md`). Pure, allocation-free.
- Pre-wait: `remaining` to action start. Action of a voiceless cue: elapsed = load offset + samples since start; `remaining` `None` while unknown (group with a busy child); a broken run shows none. Post-wait: counted from action start, also a halted playlist's wait; shown while the action runs, so one cue can have two timers.
- A paused run's timers stand on the pause sample. A timer starting ahead of `at` shows its whole length, elapsed 0. A due, un-ticked deadline shows remaining 0. Idle, loaded and settled-broken cues have none.

## Timing and continue modes (CUE-02, CUE-03)
Started at `S`, pre-wait `P`, post-wait `W`: action starts at `A = S + P`.
- Action duration: Audio unknown to core (ends on `ActionEnded`; a forever loop never completes until stopped or devamped); Wait and Fade their `duration`; Group until no run that started in it is busy; Memo, Network and Start/Stop/Pause/Load/Reset/Devamp/GoTo/Arm/Disarm 0. A disarmed cue is 0: action skipped (no voice, control action or fade) but pre-wait, post-wait and continue still apply.
- Do not continue: nothing triggered. Auto-continue: next cue at `A + W` (armed at action start). Auto-follow: next cue when the action completes: the `at` of `ActionEnded` (clamped to no earlier than `A`), or `A + duration` if known; post-wait ignored.
- "Next cue" is the cue after this one in its list or group when the trigger fires; the last cue triggers nothing. Timeline and playlist groups ignore their children's continue modes; random and start-first apply them.
- Triggering is the Start operation. A cue is idle when its action is complete and no post-wait or fade-out is pending. Stop or Reset cancels the pending post-wait and auto-follow.

### Auto-load
- When a GO moves the playhead onto a cue (`go_on`, also a GO on a skipped cue that moves it on; `auto_load_at_playhead`), that cue is loaded as `Load { time: 0 }` (`load_tree`: a group loads the children its action starts first; its sequence loads with it, at their tops, Q576) if it has `auto_load` (QLab 5, manual p. 128). Not when the playhead is set by hand or by navigation, nor when a cue is started without a GO (measured in QLab 5.6.5, Q296).
- What auto-load loaded (`auto_loaded`, per list) is unloaded when the playhead leaves that cue before it starts (`release_auto_loads`, end of each call and before the next auto-load; Q577); once it started, stopped or was reset the load is forgotten (its run starts the rest). A cue loaded on purpose (Load to Time, a Load cue) leaves the record (`forget_auto_loaded`) and stays.
- Only an idle cue, armed, without static problems: one loaded (to any time), active or broken at run time is left alone, and a disarmed or broken one is not loaded (it would play nothing). No warning either way. A load is no start: no chain budget.

### Load to time
`Load { time }` loads the cue `t` seconds into its timeline (pre-wait, then action). The pre-wait/action split is made at load with the pre-wait the cue has then (so `LoadVoice.offset` and `StartVoice.offset` agree); later pre-wait edits do not change it. Armedness and kind are read only at start.
- `t < P`: pre-wait has `P - t` left; action from the beginning.
- `t >= P`: action starts at `S`, `a = t - P` into it. Post-wait has `max(W - a, 0)` left; known duration `max(D - a, 0)`.
- Past the end: only the action is clamped to its length; the post-wait counts from the unclamped `t`. A cue loaded past its length sits `loaded` at its end: no completion, continue or warning. Starting it ends its action on its start sample. Same for every kind. (Refusing the load was rejected: a Load cue with a fixed time must survive its target being trimmed shorter.)
- Sequence (QLab 5, manual pp. 149, 681): `load_tree` walks `sequence()`, the auto-continue/auto-follow chain a run follows (skipped cues passed over; continue at pre-wait + effective post-wait, follow at the nominal span; a Devamp that starts the next cue or a follow after an unknown length ends it).
- Each later cue loads at `t - its start`, or at its top when that is below 0 (one already loaded at its top, `stands_at_top`, is kept: a preload survives auto-load). A later one disarmed or with static problems is not loaded but passed; one active or broken at run time stops the walk. Only the cue asked for warns.
- Negative time (`load_time`): counts back from `sequence().end`, the latest nominal end in the chain (a forever-looping Audio cue counts one pass); unknown end loads at 0. `sequence_span` answers it for the Load to Time slider. `loaded_offsets` (each loaded cue's action offset) feeds the `progress` items of loaded cues.
- Wait, Fade, Memo, controls, other groups: no clamp at load; at action start the end is `start + (D - a)` saturating at 0.
- Playlist (not looping) and random: nominal span = sum of children's spans (pre-wait + action, audio from `CueLengths`) or the chosen child's span. Loaded at or past it, the action ends at once on GO (no child plays); below it the child reached loads at the remainder. Unknown or endless span clamps nothing. Random's pick counts as played in its cycle even if nothing starts.
- Audio: offsets are passed as asked and the binary clamps. An offset at or past the plan's end (trims, slices, finite loops counted) is an empty plan: the binary reports `ActionLength { frames: offset }` (plus the delay of a held-up start) so core dates the end and a chained successor on that sample, not on a late `ActionEnded` (`late_start` warning otherwise). An infinite loop wraps (offset modulo the loop), so it is never past the end.

### Seek (load to a point of the file)
`Seek { cue, position }` (a click in an Audio cue's waveform, `seek.rs`) names a point of the media file, not of the timeline: core cannot plan slices, so it passes `position` (seconds to device frames; NaN and negative are 0, huge saturates) and the binary keeps it inside the trims and plans from there (`cue_slices_from`), past loops included.
- A point at or past the out point is the end of a pass of the last slice: its next pass follows if it has one (a forever loop loops on from its start), else nothing (a playing cue's action ends).
- Stopping (`stop_end`): nothing, and no warning (the stop ends it where it is; a warning row would blame a healthy cue). Not Audio, post-waiting with its sound over, or a run with no voice: `LoadIgnored`. Broken (static, run time, or a lost mark while the device is away): `CueBroken`, nothing sent.
- A preview of the cue jumps first: `SeekVoice` with a fresh preview id; it stays a preview (no `PreviewChanged`).
- Idle or loaded: a load there (`LoadVoiceAt`, `Loaded.seek`, no pre-wait left, offset 0); starting it plays from the point with no pre-wait (a cue loaded disarmed gets its `LoadVoiceAt` at the start). A lost mark clears as for Load.
- Pre-waiting: the pre-wait ends now (`until = now`; paused: on the resume) and the action starts there in the same call (`Run::seek` taken at action start: `LoadVoiceAt`, then `StartVoice` offset 0); a stream a load to time prepared is unloaded first.
- In its action (running, fading, paused): `SeekVoice { run: fresh, from, at: now }` re-keys the run like a resume (feedback for `from` is stale), the voice counts from 0 at the move (`Voice::Started { from: 0 }`, `action_offset` 0) and its end is unknown until the binary's `ActionLength` for the fresh run, counted from the move: an auto-follow fires once, at the moved sound's end. Timers, post-wait and continue are untouched.
- The moved voice is given every level Fade cues hold or move (`Ramp` 0 frames at the value, then the rest of a moving ramp) and its duck (`DuckVoice`), and a pending devamp is sent again (a paused one gets it on its resume). Paused stays paused; the resume plays from the point (offset 0).
- A burst costs core one fresh id per seek and no chain budget; the binary coalesces.

### Moving to another device (`DeviceMoved`, AUD-07)
`Input::DeviceMoved { cue, run }` (from the binary: the run's patch now plays on another output device; `seek.rs` `device_moved`) moves the sound there in step, as QLab 5 does (skill `qlab-behaviour` Q450).
- Only a run in its action with a started voice, not broken and not stopping with a fade (`stop_end`: it ends where it is); a stale run id, a pre-wait (its start plays on the patch's device then) or a post-wait after the sound changes nothing.
- `RelocateVoice { run: fresh, from, at: commit(now), position }` re-keys the run like a seek (feedback for `from` is stale). Playing: `position = Run::position(at)` and the voice counts on from there (`Voice::Started { at, from: position }`), its end unknown until the binary's `ActionLength` for the fresh run, still counted from the action's start; an auto-follow fires once. Paused: `position` is where it froze, `Voice::Started` is kept and the resume plays on from there.
- As for a seek, the moved voice is given the Fade cues' levels and its duck (`regive_levels`). Timers, post-wait, continues and the group are untouched.
- A run whose devamp is sent and not answered (`DevampWait.at` is `None`, about one block) is not moved yet: only the audio side knows where its voice is in the loop, and a voice moved then could play the loop again. `Run::move_pending` holds the move; `DevampScheduled` or `DevampIgnored` makes it, and it goes with the run (a stop, a failure).
- A run whose devamp is scheduled moves at once; its leave deadline (and the Devamp cue's next cue) stays where the answer put it and no devamp is sent again: the binary plans the moved voice to leave on the same sample (its table ends the loop there).

## Operations (CUE-05)
Voice actions apply to armed Audio cues only. Group operations also reach descendants.

| From \ op | Start | Stop / with fade | Pause | Resume | Load | Reset |
|---|---|---|---|---|---|---|
| idle | pre-waiting (new run) | idle | idle | idle | loaded, `LoadVoice` | idle |
| loaded | pre-waiting (same run, load offset) | idle, `UnloadVoice` | loaded | loaded | reload: `UnloadVoice`, `LoadVoice`, new run | idle, `UnloadVoice` |
| pre-waiting | `TriggerIgnored` | idle (action and continue cancelled) | paused | pre-waiting | `LoadIgnored` | idle |
| running | `TriggerIgnored` | idle, `StopVoice`; with fade: stopping (silent cues: idle) | paused, `PauseVoice` | running | `LoadIgnored` | idle, `StopVoice` |
| paused | resumes | idle, `StopVoice` (with fade: hard) | paused | `ResumeVoice` | `LoadIgnored` | idle, `StopVoice` |
| post-waiting | `TriggerIgnored` | idle (next cue cancelled) | paused | post-waiting | `LoadIgnored` | idle |
| stopping | `TriggerIgnored` | idle, hard `StopVoice` | stopping | stopping | `LoadIgnored` | idle, hard `StopVoice` |
| broken | `CueBroken` (static problems: timing runs); a broken-and-paused cue resumes | pending auto-continue cancelled | timers freeze | timers continue | `CueBroken` | idle, continue cancelled; static problems stay |

The Start column is for option `Nothing`; other options replace `TriggerIgnored`. Seek is the Load column except where a running, paused or pre-waiting cue moves and a stopping cue says nothing ("Seek" above).

| From | Event | To |
|---|---|---|
| pre-waiting | pre-wait elapses at `T` | running; `StartVoice { at: max(T, now) }` |
| running | post-wait elapses at `T` | next cue triggered at `T` |
| running | action completes at `T` | post-waiting if post-wait pending, else idle; auto-follow at `T` |
| post-waiting | post-wait elapses | idle; next cue triggered |
| stopping | fade-out ends (stop at + fade frames, or `ActionEnded` first) | idle |
| loaded or active | `ActionFailed` for the current run | broken; voice released at once (`StopVoice`, or `UnloadVoice` if never started); continues per rule 2; a paused one stays held until resumed |
| any non-idle | cue deleted (command, list edit, undo/redo that removes it) or id now names the other sort (group vs not) | idle; `StopVoice`/`UnloadVoice`; `CueRemoved` if it was active |
| group pre-waiting | pre-wait elapses | running; children started per mode (none busy: completes at `T`) |
| group running | last busy child completes or settles broken | post-waiting or idle; auto-follow unless a child broke |
| group running | last busy child stopped, reset or deleted | idle, continue cancelled |
| playlist child (group has no run) | action completes or breaks | next startable child starts at `T` |

- Pause freezes every timer at `now`. Resume at `R` moves every pending deadline `R - now_paused` later.
- `PauseVoice`/`ResumeVoice` carry `offset`, the position reached in the action counted from the voice's real start sample, so a binary can pause in the mixer or stop and restart from the offset.
- Stop with fade ends the cue when the fade ends, without waiting for feedback (events can be lost on queue overflow). A second stop is hard. An operation on an id not in the workspace warns `UnknownCue`.

## Second trigger
Each cue has `second_trigger` and `second_trigger_on_release` (names: `docs/file-format.md`). The option applies to every trigger that would start an active cue: Start, GO, Start cue, GoTo-and-GO, any continue, Devamp's start-next-cue, a group starting its children, and any binary start (hotkey, OSC, protocol). Double-GO protection filters GOs first.

- `Nothing` (default): ignored (`TriggerIgnored`); a paused cue resumes.
- `Panic`: the cue's own panic (fade over `panic_duration`, 0 = at once). Not a workspace panic: other cues and `panic_end()` untouched. Counts as fading out in a panic.
- `Stop`, `HardStop`: stop it and everything in it at once. (QLab's two differ only for audio effect tails, which we lack.)
- `HardStopAndRestart`: hard-stop, then start afresh (new run and `RunId`, pre-wait from the top). At most once per sample: a run triggered on the trigger's own sample (also the run a restart just made) ignores it, so a continue chain back to the cue restarts it once, not until the chain limit.
- Restart is charged to the chain budget (spent: nothing, not even the stop). A restarted playlist child stays current. Static problems: stopped, new run keeps timing without starting.
- `Devamp`: as a Devamp cue on itself (mode `current_slice`, no start-next-cue or stop-when-slice-ends): playing audio gets `DevampVoice`; a looping playlist finishes its child and completes; a cue paused in its action (only via release) keeps it for the resume. Cannot devamp: `DevampIgnored`.
- `PlayNext`, `PlayPrevious`: playlist groups playing a child (paused too): hard-stop that child, start the next or previous on `at`, wrapping if looping. Not playing a child: `TriggerIgnored`. Other kinds act as `Nothing` (validation warns `SecondTriggerNeedsPlaylist`; value kept).
- Without loop `PlayNext` past the last child completes the group as if that child ended; `PlayPrevious` on the first child stops the group (as `Stop`, continue cancelled; on a later child it goes back one). With loop `PlayPrevious` on the first wraps to the last (assumed). Shuffled: follow the pass order. Going back passes over children a start would refuse.
- At most once per sample per playlist (a further one is `TriggerIgnored`). Not made when only `PLAYLIST_RESERVE` starts are left (`ChainLimit`).
- Broken cue (run time or started broken): option not applied; `CueBroken` (a paused one resumes).
- Paused: any trigger resumes it whatever its option; nothing warned. Only a release applies the option to a paused cue.
- Fading out in a panic (workspace or own): any trigger hard-stops and restarts it whatever its option (`panic-transport.md`).
- Groups: the triggered cue's option applies; a group's option acts on the group as a whole; a child's applies when the child itself is triggered by any source, including its timeline group starting an already busy child.
- On release: `Input::TriggerReleased { cue }` (binary sends on hotkey or MIDI key-up). A second trigger only if `second_trigger_on_release`, the cue is active (not broken, not fading out in a panic) and the option is not `Nothing`; then the option applies on `now`, also to a paused cue (devamp held for the resume; next/previous resume the playlist). Otherwise nothing, silently: a release never starts or resumes. With `Panic`/`Stop` this is sampler behaviour. Unknown cue: `UnknownCue`.
- Stop, panic and devamp start nothing and cost no chain budget.

### Hotkeys (core side)
- `Workspace::cues_with_hotkey(&hotkey)` returns every cue with that key and modifiers in workspace order; cues may share a key and all start, no warning.
- A cue hotkey equal to any key of the workspace map (transport, selected-cue or cue-editing, `TransportHotkeys::action_of`) never starts the cue: the workspace action wins and `cues_with_hotkey` returns nothing. Validation warns `HotkeyUsedByWorkspace`; editing a cue to such a key is allowed.
- A press on a paused cue resumes it; the release follows the on-release option. Dispatch: skill `engine-control`.

## Feedback
- `ActionEnded`, `ActionFailed`, `StartFailed`, `ActionLength`, `DevampScheduled`, `DevampIgnored` are matched on cue and run. Feedback for an idle cue, unknown cue or old run (stopped, reset, resumed or restarted since) is ignored. Feedback naming a Group or a running Fade is ignored (no voice).
- `StartFailed` is `ActionFailed` for a voice that produced no audio yet: stream could not open or prime (file gone, read error before the first block) or the mixer refused its first start (not a resume). The cue breaks the same way but was only skipped (rule 2). Any later failure is `ActionFailed`.
- `ActionLength { cue, run, frames }`: the binary sends a finite show voice's action length in samples from the action's start (load offset included; a start held past its deadline adds the delay), so the engine owns the end as a deadline: follow, playlist step and group completion land on the end sample inside the lookahead window, not a block late.
- After a `DevampScheduled` it sends the new length (leave sample plus the rest of the table; the leave sample alone for a devamp stop); nothing while what follows loops forever. Previews send none.
- `ActionLength` is honoured only for a run playing its voice in its action, not stopping or broken. The end counts from where the voice (re)started, so a pause moves it by exactly the pause and a length sent while paused counts from the resume. Stop (its fade end applies), hard stop, failure or reset cancels it; a later length replaces it.
- `ActionEnded` before the planned deadline is an earlier end and completes at its `at` (clamped to no earlier than start). On or after the planned end it is stale: the deadline already completed the run. After a starved voice's underrun the real end is later than planned; the engine keeps the planned end (successor on time, tail overlaps) and the binary reports the underrun.
- Pause tie: while paused, an `ActionEnded` dated after the pause sample is ignored (a paused voice cannot end; the report comes from carrying out the pause). Dated before or on the pause sample it still completes the action: the mixer dates an end on the first sample after the voice's last, and an end known ahead (`ActionLength`) on that sample runs before the pause. A binary that pauses by stop and restart must not report the stop as `ActionEnded`.
- A held end (SKILL.md paused hold): `ActionFailed`/`StartFailed`/`DevampIgnored` for the ended voice are ignored meanwhile. Reports for a paused segment carry its old id and are stale after the resume, so a late end report can never complete the resumed cue. A resumed voice that had already ended reports with the fresh id. A hard stop forgets the run, so its later `Stopped` report is stale.
- `LoopReached`/`SliceEnded` are not inputs.
- `CueLengths { lengths }`: each Audio cue's nominal action length (plan from the top at device rate; `None` = unknown or forever loop; also for a gone cue or unplayable file), re-sent on change. A halted playlist, a load to time into a playlist and a Load cue's negative time use them; a cue never told is unknown.
- `CuePassLengths { lengths }`: one pass of each Audio cue's trimmed range (every slice once), sent like `CueLengths`. Only a Load cue's negative time uses it, for a target that loops forever (one pass is its end, QLab 5.6.5 measured).

## Contract the binary must meet
- `SetArmed { cue, armed }` is applied to the model (runtime change, not an undo step) before the next `handle`; the engine honours its override only for the call that made it.
- Signal every workspace change with `model_changed()` / `model_changed_cues(&footprint)` before the next `handle` (`broken-and-edits.md`).
- A voice paused by stop-and-restart must restart with the Fade levels the engine reports (`fade_ramps(cue, at)` on the resume sample): start with each `value`, then `Ramp { gain: to, frames: end - at, curve }` for each still moving.
- After `ResumeVoice` the engine re-sends a pending devamp for the fresh run. A binary with a native pause that kept the first devamp answers with the `DevampScheduled` it has, tagged with the fresh run, not `DevampIgnored`.
- `Ramp` actions with one voice, `at`, `frames` and `curve` may go as one mixer `RampSet`; the binary must not apply gangs (values are already per output).
