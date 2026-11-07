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
- `idle` (no runtime), `loaded` (ready, optionally to a time), `pre-waiting` (a start always reports this first, even for zero pre-wait), `running` (post-wait may also count), `paused` (everything frozen), `post-waiting` (action complete, post-wait pending), `stopping` (fading out before stop).
- `fading`: an audio cue in its action whose levels a running, unpaused Fade with a non-zero ramp is moving.
- Post-wait is a timer owned by the cue, not a phase: `running` while it elapses, `post-waiting` only after the action completed.
- A started cue is a run: `{pre-wait deadline, action progress, post-wait deadline, paused-at, fade-out end}`. Each voice segment has a `RunId`, unique for the engine's lifetime (the binary uses it as the mixer `VoiceId`). Loading or starting creates it; starting a loaded cue keeps it. Every resume gives a fresh id (`ResumeVoice { run: fresh, paused: previous }`), so feedback still on its way for the paused segment is stale. `run_of(cue)` is `None` for a cue owning no voice.

### Broken state
- Run-time break (feedback; `voice_rejected`, `stream_failed`, `device_lost`): owns no voice, cannot start until reset; its pending auto-continue still fires on schedule, except after `device_lost`, which cancels it.
- `device_lost` ends the run for good (pre-wait, auto-continue, auto-follow cancelled) and clears once the cue's patch is reported `mapped` again (`PatchStatus`, or when the run settles with the patch mapped; a patch never reported does not count).
- A run that broke with a pending continue keeps showing `broken` until it fires, even if the device is back: clearing early would show a voiceless run as running.
- `Input::DeviceLost` (sent after the binary failed the audio runs) hard-stops every other active run, so nothing scheduled before the loss fires after it.
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

### Load to time
`Load { time }` loads the cue `t` seconds into its timeline (pre-wait, then action). The pre-wait/action split is made at load with the pre-wait the cue has then (so `LoadVoice.offset` and `StartVoice.offset` agree); later pre-wait edits do not change it. Armedness and kind are read only at start.
- `t < P`: pre-wait has `P - t` left; action from the beginning.
- `t >= P`: action starts at `S`, `a = t - P` into it. Post-wait has `max(W - a, 0)` left; known duration `max(D - a, 0)`.
- Past the end: only the action is clamped to its length; the post-wait counts from the unclamped `t`. A cue loaded past its length sits `loaded` at its end: no completion, continue or warning. Starting it ends its action on its start sample. Same for every kind. (Refusing the load was rejected: a Load cue with a fixed time must survive its target being trimmed shorter.)
- Wait, Fade, Memo, controls, other groups: no clamp at load; at action start the end is `start + (D - a)` saturating at 0.
- Playlist (not looping) and random: nominal span = sum of children's spans (pre-wait + action, audio from `CueLengths`) or the chosen child's span. Loaded at or past it, the action ends at once on GO (no child plays); below it the child reached loads at the remainder. Unknown or endless span clamps nothing. Random's pick counts as played in its cycle even if nothing starts.
- Audio: offsets are passed as asked and the binary clamps. An offset at or past the plan's end (trims, slices, finite loops counted) is an empty plan: the binary reports `ActionLength { frames: offset }` (plus the delay of a held-up start) so core dates the end and a chained successor on that sample, not on a late `ActionEnded` (`late_start` warning otherwise). An infinite loop wraps (offset modulo the loop), so it is never past the end.

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

The Start column is for option `Nothing`; other options replace `TriggerIgnored`.

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
- A cue hotkey equal to a workspace key (transport or selected-cue key, `TransportHotkeys::action_of`) never starts the cue: the workspace action wins and `cues_with_hotkey` returns nothing. Validation warns `HotkeyUsedByWorkspace`; editing a cue to such a key is allowed.
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
- `CueLengths { lengths }`: each Audio cue's nominal action length (plan from the top at device rate; `None` = unknown or forever loop; also for a gone cue or unplayable file), re-sent on change. Only a halted playlist uses them; a cue never told is unknown.

## Contract the binary must meet
- `SetArmed { cue, armed }` is applied to the model (runtime change, not an undo step) before the next `handle`; the engine honours its override only for the call that made it.
- Signal every workspace change with `model_changed()` / `model_changed_cues(&footprint)` before the next `handle` (`broken-and-edits.md`).
- A voice paused by stop-and-restart must restart with the Fade levels the engine reports (`fade_ramps(cue, at)` on the resume sample): start with each `value`, then `Ramp { gain: to, frames: end - at, curve }` for each still moving.
- After `ResumeVoice` the engine re-sends a pending devamp for the fresh run. A binary with a native pause that kept the first devamp answers with the `DevampScheduled` it has, tagged with the fresh run, not `DevampIgnored`.
- `Ramp` actions with one voice, `at`, `frames` and `curve` may go as one mixer `RampSet`; the binary must not apply gangs (values are already per output).
