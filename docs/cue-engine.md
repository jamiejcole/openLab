# openLab cue engine (`openlab-core::cue_engine`)

How cues run: lifecycle states, timing, continue modes, groups, GO and the playhead. Architecture context: [ARCHITECTURE §3.2–3.3](ARCHITECTURE.md#32-clock-and-scheduling-nfr-202122). Requirement IDs refer to [`REQUIREMENTS.md`](../REQUIREMENTS.md).

Status: M2.1–M2.4, M2.11 (CUE-02, CUE-03, CUE-04, CUE-05 without preview/audition, CT-22, PB-01, PB-02, PB-05, CL-06 engine side). Control cues (M2.5), Fade cues (M2.6), panic and pause/resume all (M2.7) and static broken-cue detection (M2.8) come later; see [Not yet](#not-yet).

Two rules decided by the user (M2.13):

1. **Second trigger is ignored.** Start, GO or a continue on a cue that is already active (pre-waiting, running, post-waiting, fading or stopping) does nothing: no restart, no voice action, timers untouched; it reports a `TriggerIgnored { cue, state }` warning. GO still counts as a GO and moves the playhead as usual. A trigger on a **paused** cue resumes it. Per-cue second-trigger options are a later feature (M2.14).
2. **Auto-continue does not depend on success; auto-follow does.** A cue that breaks keeps its auto-continue, which fires the next cue on its original schedule (action start + post-wait), also when it breaks during the pre-wait (then the action is skipped, as for a disarmed cue, and the timing kept). A broken cue never fires its auto-follow. It reports `broken` while the continue is pending; Stop or Reset cancels the pending continue as on any cue. Its voice is released the moment it breaks.

## Shape

```rust
let config = EngineConfig::new(sample_rate, lookahead, seed)?; // checked: 1..=384000 Hz, lookahead ≤ 1 s
let mut engine = CueEngine::new(config);
let actions: Vec<Action> = engine.handle(&workspace, input, now);
let wake: Option<SampleTime> = engine.next_deadline();
```

- Pure and deterministic: no threads, clocks or I/O. The same workspace, inputs, times and seed always give the same actions in the same order (all internal maps are ordered).
- **Randomness** (random groups, shuffled playlists) comes from a SplitMix64 generator in core, seeded with `EngineConfig::seed`. Tests pass a fixed seed and are reproducible; the engine binary seeds it from OS entropy at startup (M4.2), so shows differ between runs.
- `handle` gets the **current** workspace every call. It first applies edits made since the last call (see [Model edits](#model-edits)), then carries out due deadlines, then the input, then deadlines again.
- `Action`s are core types; the engine binary turns voice actions into mixer and disk-stream commands (M4.2). Core does not depend on `openlab-audio`.
- Queries: `state(cue)`, `cue_states()` (every non-idle cue), `broken_reason(cue)`, `run_of(cue)`, `playhead(list)`, `standing_by(ws, list)` (CL-06), `playhead_positions(ws, list)`, `chain_limit(ws)`.
- One engine per open workspace. A newly opened workspace gets a new engine; call `handle(ws, Tick, now)` once so the playheads are reported.

## Time

- `SampleTime` is a `u64` frame count at the device rate, the same clock as the mixer's `samples_rendered` (ARCHITECTURE §3.2). `EngineConfig::sample_rate` is the device rate.
- The show file stores seconds. Every conversion goes through `seconds_to_samples`: round to the nearest sample, halves away from zero; negative and NaN give 0; values beyond `u64` (and +∞) saturate. Times are converted when they are used (see Model edits).
- All time arithmetic saturates; nothing panics on extreme values.
- **`now`** is the scheduling time: the earliest sample a command can still hit. The control loop passes `samples_rendered + one block`. `now` should not go backwards; if it does (a device restart), nothing panics: pending deadlines simply wait, and double-GO protection does not block (see GO).

### Deadlines, lookahead and late ticks

- Every timer is an absolute sample deadline. Each transition is computed **at its deadline sample `T`**, never at the time of the call: a successor triggered by a timer that expired at `T` starts its pre-wait at `T`, whenever the call happens. Late calls never make later cues drift.
- A call carries out every deadline in `..now + lookahead` (end excluded; with lookahead 0, every deadline up to and including `now`), earliest first (ties: the cue started first goes first, so causes come before effects).
- `EngineConfig::lookahead` (samples) lets a deadline that falls inside the next block be committed on its exact sample even though the control loop wakes with jitter. The control loop sets it to one block: with `now = samples_rendered + block`, the window `now .. now + block` is exactly the next block, so no voice start lands beyond it (docs/audio.md). It calls `Tick` when `is_due(now)`: `next_deadline() <= now + max(lookahead, 1) − 1`, so with lookahead 0 a deadline at `now` is ticked at `now`. `EngineConfig::new` refuses a lookahead longer than one second of samples and rates outside 1 to 384000 Hz. Deadlines inside the window are committed: an operator input in the same call cannot withdraw them (exactly like commands already in the mixer's queue), so a stop issued at `now` for a voice committed to start at `T > now` is sent for `T`, and a pause freezes timers at `now`.
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
| `broken` | Failed; owns no voice and cannot start until reset. Its pending auto-continue still fires on schedule. Reasons: `voice_rejected`, `stream_failed`, `device_lost` (feedback). Static detection is M2.8. |

Internally a started cue is a **run**: `{pre-wait deadline, action progress (start, end if known), post-wait deadline, paused-at, fade-out end}`. Post-wait is a timer owned by the cue, not a phase: a cue is `running` while its post-wait elapses, and `post-waiting` only once its action has completed and the post-wait has not. The reported state is derived: paused › stopping › pre-waiting › running › post-waiting.

A start always reports `pre-waiting` first, even for a zero pre-wait (followed by `running` in the same call).

Each voice segment has a `RunId`, unique for the engine's lifetime (the binary can use it as the mixer `VoiceId`). Loading or starting creates it; starting a loaded cue keeps it; **every resume gives the run a fresh id** (`ResumeVoice { run: fresh, paused: previous }`), so feedback still on its way for the paused segment is stale whenever it arrives. `run_of(cue)` is the current id.

## Timing and continue modes (CUE-02, CUE-03)

Started at `S` with pre-wait `P`, post-wait `W`:

- The action starts at `A = S + P`.
- **Action duration**: Audio — unknown to core; the action completes on `ActionEnded` feedback (`VoiceFinished`), and a cue that loops forever never completes until it is stopped (or devamped, M2.5). Wait — its `duration`. Memo — 0. Group — until no child is busy (see [Groups](#groups-ct-22)). Disarmed cues (CUE-07) — 0: the action is skipped (no voice), but pre-wait, post-wait and continue mode still apply. Kinds not implemented yet — 0, with an `Unsupported` warning (see Not yet).
- **Do not continue**: nothing else is triggered; the post-wait is not used.
- **Auto-continue**: the next cue is triggered at `A + W` (post-wait counted from the action start; `W = 0` triggers it at `A`). The post-wait timer is armed when the action starts.
- **Auto-follow**: the next cue is triggered when the action completes: at the `at` of `ActionEnded` (clamped to no earlier than `A`), or at `A + duration` for known durations. The post-wait is ignored.
- "The next cue" is the cue after this one in its list (or group) **when the trigger fires**. The last cue triggers nothing. Inside timeline, playlist and random groups the group runs its children and their continue modes are ignored (see [Groups](#groups-ct-22)).
- Triggering a cue is the Start operation (so an active successor ignores it and a paused one resumes).
- A cue is finished (idle) when its action is complete and no post-wait or fade-out is pending.
- Stopping or resetting a cue cancels its pending post-wait and its auto-follow. A cue that breaks cancels its auto-follow but keeps its auto-continue (rule 2 above).

### Load to time

`Load { time }` loads the cue `t = time` seconds into its timeline (pre-wait, then action). When it starts at `S`:

- `t < P`: the pre-wait has `P − t` left; the action starts from its beginning.
- `t ≥ P`: the action starts at `S`, `a = t − P` into the action (`StartVoice.offset`, also `LoadVoice.offset`). The post-wait has `W − a` left and a known duration `D − a` (both at least 0, so an elapsed post-wait triggers the next cue at `S`).

### Chains and the chain bound

Zero-wait chains run inside one call: an auto-continue chain of zero waits starts every cue on the same sample. Commands do not check references (M1-B) and edits can create trigger cycles, so **each call starts at most `chain_limit(ws)` = number of cues in the workspace (all depths) + 64 cues**. Every start counts, including children started by their group (a looping playlist of zero-length children is such a cycle: it is cut at the limit and the group then completes). Without a cycle a call starts each cue at most once, so only a cycle reaches it. A start beyond the limit is dropped with a `ChainLimit` warning; the chain is cut, not deferred, so a cycle can never hang a call or keep re-firing on later ticks at the same sample. A cycle with time in it (non-zero waits) is not a hang and keeps running, one deadline per call. As a second line of defence the deadline loop has its own step guard.

## Groups (CT-22)

A Group cue is a cue like any other: its own pre-wait, post-wait and continue mode, the [operations](#operations-cue-05) and the second-trigger rule. Its **action** is running its children; behaviour follows QLab 5's group modes.

### Its action

- The action starts when the group's pre-wait elapses, at `A`, and starts children at `A` according to the mode (each child then runs its own pre-wait).
- A child is **busy** while it has a runtime that is not idle, loaded or settled broken: pre-waiting, running, post-waiting, paused, stopping, or broken with an auto-continue still pending.
- The action **completes** on the sample its last busy child stops being busy (completes, settles broken, is stopped, reset or deleted), and then the group's continue mode applies as for any cue: auto-follow triggers the cue after the group at that sample; auto-continue fires `A + post-wait` (a post-wait longer than the children makes the group `post-waiting`).
- **Success**: a group whose run saw a child break (feedback, or a broken child it could not start) does not auto-follow; its auto-continue still fires (rule 2 above, applied to the group). Nested groups pass this on: an inner group that saw a break counts as a broken child of the outer one.
- **Stopped children**: if the last busy child left by being stopped, reset or deleted, the group stops too (idle, its post-wait and auto-follow cancelled) instead of completing. Stopping a child while another is still busy only stops that child.
- An empty or disarmed group's action completes at once (a disarmed group starts no children).
- Feedback (`ActionEnded`/`ActionFailed`) never concerns a group; any naming one is ignored.

### Modes

| Mode | On action start | Children's continue modes | Playhead after GO on the group |
|---|---|---|---|
| Timeline (start all simultaneously) | every child, at `A` | ignored | after the group's continue chain |
| Start first and go to next | the first child | apply: children chain within the group | after the group's continue chain |
| Start first and enter | the first child | apply | into the group: after the first child's chain |
| Playlist | the first child (shuffled: the first of a shuffled pass) | ignored | after the group's continue chain |
| Start random | one random child | ignored | after the group's continue chain |

- **Playlist**: each next child starts on the sample the previous child's action completes (its pre-wait then counts). A child that breaks (feedback) moves the playlist on at the moment it breaks; a broken child that cannot start is skipped at once; both mark the run as not successful. Without loop the group completes after the last child. With loop it wraps to the first and never completes on its own (stop it; Devamp is M2.5). With shuffle each pass plays every child exactly once in a fresh random order. Unshuffled, the next child is the one after the current one as the group stands when the trigger fires (a reorder applies to the next child; a removed current child is followed by the child that took its place). Shuffled, a pass's order is fixed when the pass starts; children removed meanwhile are skipped and children added play from the next pass. A pass in which no child could start ends the playlist; so does the chain limit.
- **Random**: one child is picked uniformly from the seeded generator, never the child this group picked last time when it has another (QLab does not repeat a random child immediately); one child is always picked.

### A child started on its own

Starting a cue inside a group that is idle or loaded (`Start`, or GO on a playhead inside an enter group) makes every idle group around it **run as adopted**: it reports `running` from that sample and completes (goes idle) when no child is busy, but starts no other children and fires no continue of its own. A child started while its group is post-waiting puts the group back to `running`, its post-wait still counting; it completes again without auto-follow. A child started while its group pre-waits runs on its own; when the group's action starts, that child ignores the trigger (it is already active).

### State of a group

The group's own run gives its state, so it is exactly one state: `pre-waiting` during its pre-wait, `running` while its action is under way (any child busy), `post-waiting` after it with the post-wait pending, `paused` when the group was paused, `stopping` while a stop with fade waits for its children, `idle` otherwise. A paused child alone does not pause its group.

### Operations on a group

Each applies to the group and to its descendants (every depth):

- **Stop** stops every descendant with a runtime, children before their groups, then the group. Hard stop: everything idle at once. Stop with fade: each playing voice fades out (silent actions stop at once); a group with a child still fading reports `stopping` and goes idle when its last child has stopped. A second stop is a hard stop.
- **Pause** pauses the group and every active descendant (voices get `PauseVoice`); every timer keeps the time it had left. **Resume** (and Start/GO on a paused group, by the second-trigger rule) resumes the group and every paused descendant; a group whose last child left while it was paused ends then.
- **Reset** resets every descendant (clearing broken children) and the group.
- **Load** loads the group and the children its action starts first: timeline — all children; start-first modes and an unshuffled playlist — the first child; shuffled playlist and random — none. Load to time `t` loads them `t − group pre-wait` seconds (at least 0) into their own timelines, so a loaded timeline group starts its children at that point.
- **Start** on an active group is ignored (`TriggerIgnored`); its children are untouched.

### Nesting and bounds

Groups nest to `MAX_GROUP_DEPTH` (validation and commands enforce it). The engine's walks over descendants and ancestors are iterative, so deeper trees built directly do not overflow the stack. Every start inside a group counts towards the call's chain limit.

## GO and double-GO (PB-01, PB-02)

- `Go { list }` starts the cue at the list's playhead at `now` and moves the playhead to the cue after the last cue of its continue chain: from the playhead cue, every following cue reached through auto-continue/auto-follow (as the model stands) is skipped. If the chain runs to the end of the list, the playhead goes past the end. Groups: GO on a "start first and enter" group moves the playhead **into** it, to the child after the first child's continue chain; GO on a cue inside such a group starts that cue only and moves to the next cue after its chain among its siblings; past the group's last child the playhead leaves the group to the cue after the **group's** continue chain (and so on outwards; the group's own auto-continue/follow targets are skipped as at the top level).
- GO on a cue that is already active is ignored for that cue (`TriggerIgnored`), but the GO counts and the playhead moves on. GO on a paused cue resumes it. GO on a broken cue does not start it (`CueBroken` warning) but still counts as a GO and moves the playhead on, so the show can continue with the next GO.
- Ignored GOs report `GoIgnored { reason }`: `no_cue` (playhead past the end, or empty list), `unknown_list`, or `double_go`.
- **Double-GO**: a GO less than `min_go_interval` after the previous **accepted** GO (in samples, any list) is ignored; one exactly at the interval is accepted. Ignored GOs do not restart the interval. 0 = off. If `now` is earlier than the last accepted GO (clock restart), the GO is accepted.

## Playhead (PB-05, CL-06)

- Each cue list has a playhead: the standing-by cue, or `None` past the end of the list or for an empty list. A new list stands on its first cue. `PlayheadChanged` reports every move, including the initial one.
- **Positions** (M2.11): the playhead stands on a top-level cue or on a cue inside "start first and enter" groups (every group around it in that mode), the cues a GO can reach. In order, the positions are the list's cues depth-first, entering only those groups: `1, 2, 2.1, 2.2, 3` for an enter group `2`; a timeline group `4` is one position and its children none. `playhead_positions(ws, list)` lists them.
- `PlayheadNext`: one position down (into an enter group to its first child; from its last child out to the cue after the group); from the last position to past the end, which stays. `PlayheadPrevious`: one position up (from the cue after an enter group to the group's last child); from past the end to the last position; the first stays. `SetPlayhead { cue }` (by id; `None` = past the end) and `SetPlayheadByNumber` (exact number match, first in position order) take positions of that list only; a cue of the list that is not a position (inside a timeline, go-to-next, playlist or random group) warns `NotInList`, an unknown number `NumberNotFound`; both leave the playhead. Navigation never fires anything.
- Edits: a reordered or moved playhead cue that is still a position keeps the playhead. One that is no longer a position (deleted, its group deleted, moved into a group of another mode, or its group's mode changed) gives way to the first position that came after it and still exists, or past the end. An empty list that gains cues stands on its first cue. A deleted list loses its playhead.

## Operations (CUE-05)

`now` is when the operation comes. Voice actions apply to armed Audio cues only.

| From \ op | Start | Stop | Stop with fade | Pause | Resume | Load (to time) | Reset |
|---|---|---|---|---|---|---|---|
| idle | pre-waiting (new run) | idle | idle | idle | idle | loaded, `LoadVoice` | idle |
| loaded | pre-waiting (same run, loaded offset) | idle, `UnloadVoice` | idle, `UnloadVoice` | loaded | loaded | loaded (reload: `UnloadVoice`, `LoadVoice`, new run) | idle, `UnloadVoice` |
| pre-waiting | pre-waiting, `TriggerIgnored` | idle (action and continue cancelled) | idle | paused | pre-waiting | pre-waiting, `LoadIgnored` | idle |
| running | running, `TriggerIgnored` | idle, `StopVoice` | stopping, `StopVoice` with fade (silent actions: idle at once) | paused, `PauseVoice` | running | running, `LoadIgnored` | idle, `StopVoice` |
| paused | resumes (as Resume) | idle, `StopVoice` | idle, `StopVoice` (hard) | paused | the frozen state, `ResumeVoice` | paused, `LoadIgnored` | idle, `StopVoice` |
| post-waiting | post-waiting, `TriggerIgnored` | idle (next cue cancelled) | idle | paused | post-waiting | post-waiting, `LoadIgnored` | idle |
| stopping | stopping, `TriggerIgnored` | idle, `StopVoice` (hard) | idle, `StopVoice` (hard) | stopping | stopping | stopping, `LoadIgnored` | idle, `StopVoice` (hard) |
| broken | broken, `CueBroken` | broken (pending auto-continue cancelled) | broken (same) | broken (its timers freeze) | broken (its timers continue) | broken, `CueBroken` | idle (pending auto-continue cancelled) |

Events:

| From | Event | To |
|---|---|---|
| pre-waiting | pre-wait elapses at `T` | running; `StartVoice { at: max(T, now) }` |
| running | post-wait elapses at `T` (auto-continue) | running; next cue triggered at `T` |
| running | action completes at `T` (feedback, or known duration) | post-waiting if a post-wait is pending, else idle; auto-follow triggers the next cue at `T` |
| post-waiting | post-wait elapses at `T` | idle; next cue triggered at `T` |
| stopping | fade-out ends (`stop at + fade frames`, or `ActionEnded` first) | idle |
| loaded, any active | `ActionFailed` for the current run | broken: the run's voice is released at once (`StopVoice`, or `UnloadVoice` if it never started). Auto-follow is cancelled; a pending or still-to-be-armed auto-continue fires on its original schedule (a pre-waiting cue skips its action at the action start and arms the post-wait as usual; a paused one stays held until resumed). A broken cue owns no voice, so Stop and Reset have nothing left to silence |
| paused | time passes | paused (nothing runs) |
| any non-idle | cue deleted from the workspace | idle; `StopVoice`/`UnloadVoice`, `CueRemoved` warning if it was active |
| any non-idle | its id now names a cue of the other sort (group vs. not; duplicate ids or a replaced cue) | idle, as if deleted |
| group pre-waiting | pre-wait elapses at `T` | running; children started at `T` per mode (none busy: the action completes at `T`) |
| group running | last busy child completes or settles broken at `T` | post-waiting if a post-wait is pending, else idle; auto-follow at `T` unless a child broke |
| group running | last busy child stopped, reset or deleted at `T` | idle (continue cancelled) |
| group running (playlist) | current child's action completes, or it breaks, at `T` | running; next child started at `T` (or the group completes, above) |
| group stopping | last busy child has stopped | idle |
| group idle / loaded | a child starts on its own at `T` | running (adopted) |
| group post-waiting | a child starts on its own at `T` | running, post-wait still counting |

- **Pause** freezes every timer at `now`; **resume** at `R` moves every pending deadline `R − now_paused` later, so pre-wait, action and post-wait keep exactly the time they had left. `PauseVoice`/`ResumeVoice` carry the position the voice has actually reached in the action (`offset`, counted from the voice's real start sample, so a late start or a pause before a start committed in the lookahead window is exact), so the binary can pause in the mixer or stop and restart from the offset.
- **Stop with fade** fades a playing voice out over the fade's length (`StopVoice.fade`) and ends the cue when the fade ends — without waiting for feedback, which can be lost if the event queue overflows. A second stop is a hard stop.
- A stop or pause never lands before the voice's own start sample (`at` = `max(now, voice start)`), so in the mixer it cannot be overtaken by a start already committed.
- Operations on a cue id that is not in the workspace warn `UnknownCue` and do nothing.
- On a Group, every operation also reaches its descendants; see [Operations on a group](#operations-on-a-group).

## Feedback

- `ActionEnded { cue, run, at }` (from `VoiceFinished`) and `ActionFailed { cue, run, reason }` are matched on **cue and run**. Feedback for an idle cue, an unknown cue, or an old run (the cue has been stopped, reset, resumed or started again since) is ignored without any action.
- `ActionEnded` completes an audio action at `at` (clamped to no earlier than its start), or ends a fade-out stop. While the cue is paused, an `ActionEnded` dated at or after the pause sample is ignored: a paused voice cannot end by itself, so such a report comes from carrying out the pause (e.g. as a stop). An end dated before the pause (the voice ended just before the pause applied) still completes the action and fires auto-follow. Once the cue has resumed, reports for the paused segment carry its old id and are ignored like any stale id, so a pause's late end report can never complete the resumed cue. A genuine end of the resumed voice carries the fresh id and completes the cue as usual (a binary that resumes a voice which had already ended reports that end with the fresh id). A hard stop forgets the run immediately, so the voice's `Stopped` report that follows is stale and ignored.
- `LoopReached`/`DevampScheduled` are not inputs yet (Devamp is M2.5).

## Model edits

Each call compares the workspace with what the engine knows:

- A cue with a runtime that is no longer in the workspace is hard-stopped and forgotten.
- Changed times apply to **timers not yet started**: the pre-wait is fixed when the cue starts; the post-wait, a Wait duration and the continue mode (auto-follow) are fixed when the action starts. The next cue is looked up when the trigger fires.
- Playheads follow edits as described above; double-GO reads `min_go_interval` at each GO.
- **Running groups**: a deleted child is stopped like any deleted cue (if it was the group's last busy child, the group stops). A deleted group stops with all its descendants. A child added to a running timeline is not started (children start only when the action starts); a playlist plays it when it comes up (unshuffled) or from the next pass (shuffled). Reordering applies to children started later (see Playlist). A busy child moved out of its group keeps running on its own; a group left with no busy child by an edit (child moved out, mode changed) stops at once. A group's mode is read when its action starts and again whenever it acts (a playlist changed to another mode stops moving on and completes with its current child; loop and shuffle are read when it moves on or starts a pass; a child's continue is checked against the mode when it fires). A cue whose kind changes to or from Group while it runs is stopped.

## Warnings

`Action::Warning(EngineWarning)` never stops anything: `UnknownList`, `UnknownCue`, `NotInList` (also: a cue of the list the playhead cannot stand on), `NumberNotFound`, `CueBroken`, `TriggerIgnored`, `LoadIgnored`, `Unsupported`, `LateStart`, `ChainLimit`, `CueRemoved`. They are structured so the UI can localise them.

## Not yet

- **Groups**: playlist crossfades and per-group options beyond loop/shuffle; Devamp of a looping playlist (M2.5).
- **Control cues** (M2.5: Start, Stop, Pause, Load, Reset, Devamp, GoTo, Arm, Disarm), **Fade** (M2.6, which will enter `fading`) and **Network** cues (M4.6): their action completes at once with an `Unsupported` warning and does nothing; pre-wait, post-wait and continue mode apply. Wait and Memo are complete.
- **Panic / Pause All / Resume All** (M2.7), **static broken detection** (M2.8), **preview/audition** (CUE-05/PB-09).
- **Priming**: `Loaded` means "loaded by an operation"; keeping cues near the playhead primed (ARCHITECTURE §3.5) is the binary's job (M4.2).
