# openLab cue engine (`openlab-core::cue_engine`)

How cues run: lifecycle states, timing, continue modes, GO and the playhead. Architecture context: [ARCHITECTURE §3.2–3.3](ARCHITECTURE.md#32-clock-and-scheduling-nfr-202122). Requirement IDs refer to [`REQUIREMENTS.md`](../REQUIREMENTS.md).

Status: M2.1–M2.3 (CUE-02, CUE-03, CUE-04, CUE-05 without preview/audition, PB-01, PB-02, PB-05, CL-06 engine side). Groups (M2.4), control cues (M2.5), Fade cues (M2.6), panic and pause/resume all (M2.7) and static broken-cue detection (M2.8) come later; see [Not yet](#not-yet).

## Shape

```rust
let mut engine = CueEngine::new(EngineConfig { sample_rate, lookahead });
let actions: Vec<Action> = engine.handle(&workspace, input, now);
let wake: Option<SampleTime> = engine.next_deadline();
```

- Pure and deterministic: no threads, clocks or I/O. The same workspace, inputs and times always give the same actions in the same order (all internal maps are ordered).
- `handle` gets the **current** workspace every call. It first applies edits made since the last call (see [Model edits](#model-edits)), then carries out due deadlines, then the input, then deadlines again.
- `Action`s are core types; the engine binary turns voice actions into mixer and disk-stream commands (M4.2). Core does not depend on `openlab-audio`.
- Queries: `state(cue)`, `cue_states()` (every non-idle cue), `broken_reason(cue)`, `run_of(cue)`, `playhead(list)`, `standing_by(ws, list)` (CL-06), `chain_limit(ws)`.
- One engine per open workspace. A newly opened workspace gets a new engine; call `handle(ws, Tick, now)` once so the playheads are reported.

## Time

- `SampleTime` is a `u64` frame count at the device rate, the same clock as the mixer's `samples_rendered` (ARCHITECTURE §3.2). `EngineConfig::sample_rate` is the device rate.
- The show file stores seconds. Every conversion goes through `seconds_to_samples`: round to the nearest sample, halves away from zero; negative and NaN give 0; values beyond `u64` (and +∞) saturate. Times are converted when they are used (see Model edits).
- All time arithmetic saturates; nothing panics on extreme values.
- **`now`** is the scheduling time: the earliest sample a command can still hit. The control loop passes `samples_rendered + one block`. `now` should not go backwards; if it does (a device restart), nothing panics: pending deadlines simply wait, and double-GO protection does not block (see GO).

### Deadlines, lookahead and late ticks

- Every timer is an absolute sample deadline. Each transition is computed **at its deadline sample `T`**, never at the time of the call: a successor triggered by a timer that expired at `T` starts its pre-wait at `T`, whenever the call happens. Late calls never make later cues drift.
- A call carries out every deadline up to its **horizon**, `now + lookahead`, earliest first (ties: the cue started first goes first, so causes come before effects).
- `EngineConfig::lookahead` (samples) lets a deadline that falls inside the next block be committed on its exact sample even though the control loop wakes with jitter. The control loop sets it to at most one block (the mixer must never hold voice starts beyond the next block, docs/audio.md) and calls `Tick` once `now + lookahead >= next_deadline()`. Deadlines inside the window are committed: an operator input in the same call cannot withdraw them (exactly like commands already in the mixer's queue), so a stop issued at `now` for a voice committed to start at `T > now` is sent for `T`, and a pause freezes timers at `now`.
- **Late rule**: a voice whose deadline `T` has already passed (`T < now`, a late tick) starts at `max(T, now) = now`, from its start offset (the head of the sound is not cut), with a `LateStart { late_by }` warning. Its state change still reports `at: T`, and every timer derived from it counts from `T`.

## States (CUE-04)

Each cue reports exactly one state:

| State | Meaning |
|---|---|
| `idle` | Nothing to do. Idle cues have no runtime. |
| `loaded` | Loaded (optionally to a time) and ready to start. |
| `pre-waiting` | Started; the pre-wait is counting down. |
| `running` | The action is under way (post-wait may be counting too). |
| `paused` | Pre-wait, action and post-wait are frozen. |
| `post-waiting` | The action is complete; the post-wait is still counting down to the next cue. |
| `fading` | Defined for Fade cues (M2.6); nothing enters it yet. |
| `stopping` | Fading out before it stops (a stop with a fade). |
| `broken` | Failed; does nothing until reset. Reasons: `voice_rejected`, `stream_failed`, `device_lost` (feedback). Static detection is M2.8. |

Internally a started cue is a **run**: `{pre-wait deadline, action progress (start, end if known), post-wait deadline, paused-at, fade-out end}`. Post-wait is a timer owned by the cue, not a phase: a cue is `running` while its post-wait elapses, and `post-waiting` only once its action has completed and the post-wait has not. The reported state is derived: paused › stopping › pre-waiting › running › post-waiting.

A start always reports `pre-waiting` first, even for a zero pre-wait (followed by `running` in the same call). A restart reports `idle` and then `pre-waiting`.

Each run has a `RunId`, unique for the engine's lifetime (the binary can use it as the mixer `VoiceId`). Loading creates the run; starting a loaded cue keeps it.

## Timing and continue modes (CUE-02, CUE-03)

Started at `S` with pre-wait `P`, post-wait `W`:

- The action starts at `A = S + P`.
- **Action duration**: Audio — unknown to core; the action completes on `ActionEnded` feedback (`VoiceFinished`), and a cue that loops forever never completes until it is stopped (or devamped, M2.5). Wait — its `duration`. Memo — 0. Disarmed cues (CUE-07) — 0: the action is skipped (no voice), but pre-wait, post-wait and continue mode still apply. Kinds not implemented yet — 0, with an `Unsupported` warning (see Not yet).
- **Do not continue**: nothing else is triggered; the post-wait is not used.
- **Auto-continue**: the next cue is triggered at `A + W` (post-wait counted from the action start; `W = 0` triggers it at `A`). The post-wait timer is armed when the action starts.
- **Auto-follow**: the next cue is triggered when the action completes: at the `at` of `ActionEnded` (clamped to no earlier than `A`), or at `A + duration` for known durations. The post-wait is ignored.
- "The next cue" is the cue after this one in its list (or group) **when the trigger fires**. The last cue triggers nothing.
- Triggering a cue is the Start operation (so a running successor restarts and a paused one resumes).
- A cue is finished (idle) when its action is complete and no post-wait or fade-out is pending.
- Stopping or resetting a cue cancels its pending post-wait and its auto-follow. A cue that breaks cancels them too.

### Load to time

`Load { time }` loads the cue `t = time` seconds into its timeline (pre-wait, then action). When it starts at `S`:

- `t < P`: the pre-wait has `P − t` left; the action starts from its beginning.
- `t ≥ P`: the action starts at `S`, `a = t − P` into the action (`StartVoice.offset`, also `LoadVoice.offset`). The post-wait has `W − a` left and a known duration `D − a` (both at least 0, so an elapsed post-wait triggers the next cue at `S`).

### Chains and the chain bound

Zero-wait chains run inside one call: an auto-continue chain of zero waits starts every cue on the same sample. Commands do not check references (M1-B) and edits can create trigger cycles, so **each call starts at most `chain_limit(ws)` = number of cues in the workspace (all depths) + 64 cues**. Without a cycle a call starts each cue at most once, so only a cycle reaches it. A start beyond the limit is dropped with a `ChainLimit` warning; the chain is cut, not deferred, so a cycle can never hang a call or keep re-firing on later ticks at the same sample. A cycle with time in it (non-zero waits) is not a hang and keeps running, one deadline per call. As a second line of defence the deadline loop has its own step guard.

## GO and double-GO (PB-01, PB-02)

- `Go { list }` starts the cue at the list's playhead at `now` and moves the playhead to the cue after the last cue of its continue chain: from the playhead cue, every following cue reached through auto-continue/auto-follow (as the model stands) is skipped. If the chain runs to the end of the list, the playhead goes past the end.
- GO on a cue that is already running restarts it (hard stop of the old run, a new run from the start). GO on a paused cue resumes it. GO on a broken cue does not start it (`CueBroken` warning) but still counts as a GO and moves the playhead on, so the show can continue with the next GO.
- Ignored GOs report `GoIgnored { reason }`: `no_cue` (playhead past the end, or empty list), `unknown_list`, or `double_go`.
- **Double-GO**: a GO less than `min_go_interval` after the previous **accepted** GO (in samples, any list) is ignored; one exactly at the interval is accepted. Ignored GOs do not restart the interval. 0 = off. If `now` is earlier than the last accepted GO (clock restart), the GO is accepted.

## Playhead (PB-05, CL-06)

- Each cue list has a playhead: a top-level cue of that list (the standing-by cue), or `None` past the end of the list or for an empty list. A new list stands on its first cue. `PlayheadChanged` reports every move, including the initial one.
- `PlayheadNext`: one cue down; from the last cue to past the end, which stays. `PlayheadPrevious`: one cue up; from past the end to the last cue; the first cue stays. `SetPlayhead { cue }` (by id; `None` = past the end) and `SetPlayheadByNumber` (exact number match) take top-level cues of that list only (cues inside groups: M2.4); others warn `NotInList` / `NumberNotFound` and leave the playhead. Navigation never fires anything.
- Edits: a reordered playhead cue keeps the playhead. A deleted playhead cue gives way to the first cue that came after it and still exists, or past the end. An empty list that gains cues stands on its first cue. A deleted list loses its playhead.

## Operations (CUE-05)

`now` is when the operation comes. Voice actions apply to armed Audio cues only.

| From \ op | Start | Stop | Stop with fade | Pause | Resume | Load (to time) | Reset |
|---|---|---|---|---|---|---|---|
| idle | pre-waiting (new run) | idle | idle | idle | idle | loaded, `LoadVoice` | idle |
| loaded | pre-waiting (same run, loaded offset) | idle, `UnloadVoice` | idle, `UnloadVoice` | loaded | loaded | loaded (reload: `UnloadVoice`, `LoadVoice`, new run) | idle, `UnloadVoice` |
| pre-waiting | restart: idle → pre-waiting | idle (action and continue cancelled) | idle | paused | pre-waiting | pre-waiting, `LoadIgnored` | idle |
| running | restart, `StopVoice` | idle, `StopVoice` | stopping, `StopVoice` with fade (silent actions: idle at once) | paused, `PauseVoice` | running | running, `LoadIgnored` | idle, `StopVoice` |
| paused | resumes (as Resume) | idle, `StopVoice` | idle, `StopVoice` (hard) | paused | the frozen state, `ResumeVoice` | paused, `LoadIgnored` | idle, `StopVoice` |
| post-waiting | restart | idle (next cue cancelled) | idle | paused | post-waiting | post-waiting, `LoadIgnored` | idle |
| stopping | restart, `StopVoice` (hard) | idle, `StopVoice` (hard) | idle, `StopVoice` (hard) | stopping | stopping | stopping, `LoadIgnored` | idle, `StopVoice` (hard) |
| broken | broken, `CueBroken` | broken | broken | broken | broken | broken, `CueBroken` | idle |

Events:

| From | Event | To |
|---|---|---|
| pre-waiting | pre-wait elapses at `T` | running; `StartVoice { at: max(T, now) }` |
| running | post-wait elapses at `T` (auto-continue) | running; next cue triggered at `T` |
| running | action completes at `T` (feedback, or known duration) | post-waiting if a post-wait is pending, else idle; auto-follow triggers the next cue at `T` |
| post-waiting | post-wait elapses at `T` | idle; next cue triggered at `T` |
| stopping | fade-out ends (`stop at + fade frames`, or `ActionEnded` first) | idle |
| loaded, any active | `ActionFailed` for the current run | broken: the run's voice is released at once (`StopVoice`, or `UnloadVoice` if it never started), timers and continue cancelled. A broken cue owns no voice, so Stop and Reset have nothing left to silence |
| paused | time passes | paused (nothing runs) |
| any non-idle | cue deleted from the workspace | idle; `StopVoice`/`UnloadVoice`, `CueRemoved` warning if it was active |

- **Pause** freezes every timer at `now`; **resume** at `R` moves every pending deadline `R − now_paused` later, so pre-wait, action and post-wait keep exactly the time they had left. `PauseVoice`/`ResumeVoice` carry the position in the action (`offset`), so the binary can pause in the mixer or stop and restart from the offset.
- **Stop with fade** fades a playing voice out over the fade's length (`StopVoice.fade`) and ends the cue when the fade ends — without waiting for feedback, which can be lost if the event queue overflows. A second stop is a hard stop.
- A stop, pause or restart never lands before the voice's own start sample (`at` = `max(now, voice start)`), so in the mixer it cannot be overtaken by a start already committed.
- Operations on a cue id that is not in the workspace warn `UnknownCue` and do nothing.

## Feedback

- `ActionEnded { cue, run, at }` (from `VoiceFinished`) and `ActionFailed { cue, run, reason }` are matched on **cue and run**. Feedback for an idle cue, an unknown cue, or an old run (the cue has been stopped, restarted or reset since) is ignored without any action.
- `ActionEnded` completes an audio action at `at` (clamped to no earlier than its start), or ends a fade-out stop. A hard stop forgets the run immediately, so the voice's `Stopped` report that follows is stale and ignored.
- `LoopReached`/`DevampScheduled` are not inputs yet (Devamp is M2.5).

## Model edits

Each call compares the workspace with what the engine knows:

- A cue with a runtime that is no longer in the workspace is hard-stopped and forgotten.
- Changed times apply to **timers not yet started**: the pre-wait is fixed when the cue starts; the post-wait, a Wait duration and the continue mode (auto-follow) are fixed when the action starts. The next cue is looked up when the trigger fires.
- Playheads follow edits as described above; double-GO reads `min_go_interval` at each GO.

## Warnings

`Action::Warning(EngineWarning)` never stops anything: `UnknownList`, `UnknownCue`, `NotInList`, `NumberNotFound`, `CueBroken`, `LoadIgnored`, `Unsupported`, `LateStart`, `ChainLimit`, `CueRemoved`. They are structured so the UI can localise them.

## Not yet

- **Groups** (M2.4): a Group cue's action completes at once with an `Unsupported` warning; its children are not started. The playhead stays on top-level cues.
- **Control cues** (M2.5: Start, Stop, Pause, Load, Reset, Devamp, GoTo, Arm, Disarm), **Fade** (M2.6, which will enter `fading`) and **Network** cues (M4.6): their action completes at once with an `Unsupported` warning and does nothing; pre-wait, post-wait and continue mode apply. Wait and Memo are complete.
- **Panic / Pause All / Resume All** (M2.7), **static broken detection** (M2.8), **preview/audition** (CUE-05/PB-09).
- **Priming**: `Loaded` means "loaded by an operation"; keeping cues near the playhead primed (ARCHITECTURE §3.5) is the binary's job (M4.2).
