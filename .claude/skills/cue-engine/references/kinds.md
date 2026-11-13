# Cue kinds: control cues, GoTo, Arm, Devamp, Wait, Memo, Network, display name

## Control cues (CT-11..CT-19)
A control cue is a cue like any other. At its action start `A` (after its own pre-wait) it acts on its target as if the operator had done the operation at that moment, and its own action then completes at once (so auto-follow fires at `A`). Its continue mode applies as usual.
- The operation is dated `A`: inside the lookahead window it acts on its own sample (`StopVoice { at: A }`); late (`A < now`) it acts at `now`, except a start, which is dated `A` like any trigger so later timing keeps its schedule (the late rule then applies to voices).
- The cue is still in its action while it acts, so a trigger from its target back to it is ignored (`TriggerIgnored`); a Start cue targeting itself is such a case.
- Targets: no target, missing target, itself, or the wrong kind are static problems (`broken-and-edits.md`): broken, action never runs. The run-time checks (`NoTarget`, `TargetMissing`, `TargetWrongKind`: does nothing, timing and continue apply) remain for out-of-date static problems after a model change the binary did not signal.
- Group targets use the group operations (`groups-playhead.md`): Stop/Pause/Load/Reset reach every descendant; Start starts the group in its mode. A target inside a group starts on its own; its groups do not run.

| Cue | Action at `A` |
|---|---|
| Start | Start operation on the target dated `A`: idle/loaded starts; paused resumes (whatever its option); other active: its second-trigger option; broken: `CueBroken`; disarmed target skipped as on GO |
| Stop | Stop with the cue's `fade_out` (duration 0 = hard) |
| Pause | Pause (a paused or idle target is left) |
| Load | Load to `load_to_time` (active target: `LoadIgnored`; broken: `CueBroken`) |
| Reset | Reset (clears broken) of the target and its descendants, which also become unplayed (first colour again, `panic-transport.md` "Played"). Does not undo Arm/Disarm |
| Devamp, GoTo, Arm, Disarm | below |
| Wait | nothing; action lasts its `duration` |
| Memo | nothing; action completes at once |

A running Wait takes an edited duration (`broken-and-edits.md`).

## GoTo
- Moves the playhead of the target's cue list (any list, not only its own) to the target, at any depth.
- With "and GO" it then GOes on that list dated `A`: the target starts and the playhead moves past its continue chain (or into an armed enter group), as an operator GO, except double-GO protection does not block it and it does not restart the interval.

## Arm and Disarm
- The engine cannot change the `&Workspace`. The cue emits `SetArmed { cue, armed }` and records an override honoured for the rest of that call: a Disarm followed by a trigger of the target in the same chain sees it disarmed (action skipped, Load sends no `LoadVoice`, GO does not enter a disarmed enter group).
- The binary applies every `SetArmed` to the model before the next `handle`; each call starts by dropping all overrides, so the model rules, including an operator edit of `armed` made right after.
- Only the target changes (a group's children keep theirs). A running target keeps running; the change applies to its next action. Arming an already armed cue (as the engine sees it) emits nothing.

## Devamp
- "Start next cue" is the Devamp cue's continue. On an armed Devamp cue it replaces its continue mode: the next cue starts once, when the loop is left, never also at the Devamp cue's action start, and its post-wait is unused. For the playhead, GO on it moves the playhead onto the cue it will start (it stands by until the loop is left and stays under the playhead when it starts; a chain reaching the Devamp cue stops there too).
- Without start-next-cue, or on a disarmed or broken Devamp cue, its own continue mode applies as usual, also for the playhead. A broken Devamp cue sends no devamp, so start-next-cue never fires.
- Modes (`devamp_mode`): `current_slice` leaves whatever loop the audio target is playing and goes to the next slice; `looping_cue` leaves the cue's own loop.
  - No whole-cue play count exists: a cue without slice markers loops as a whole through `last_slice_play_count`, and both modes leave that.
  - With slice markers every loop is a slice's, so `looping_cue` does nothing but warn `DevampIgnored { cue: target }` (nothing pending, so start-next-cue never fires and a later devamp can act). A looping playlist is devamped the same in both modes; the `Devamp` second trigger acts as `current_slice`.
- Audio target that is playing (in its action with a started voice; not paused, stopping or broken): emits `DevampVoice { cue, run, at: max(A, now, voice start), stop }` (`stop` = stop target when slice ends). Answers:
  - `DevampScheduled { at: L }`: the voice leaves its loop on `L`. With start-next-cue the cue after the Devamp cue (looked up as for any continue when it fires) starts exactly on `L`: a deadline, inside the window on `L`, late at `now` otherwise. The target plays into its next slice (or ends there with `stop`) and completes on its end report.
  - If the target's end arrives before that deadline ran (ended at `T <= L`), the next cue starts on `min(L, T)`: it never waits past the end of the cue it follows.
  - `DevampIgnored` (nothing to leave, e.g. no slice table): `DevampIgnored` warning, no next cue.
- One devamp at a time per voice: a second while one is pending sends nothing and warns `DevampIgnored` (the audio side would ignore it, and its answer could not be told apart).
- Pause: a pending devamp survives a pause of its target. On resume it is sent again for the fresh run (dated on the resume), any known `L` is forgotten and the next cue starts on the new answer's `L`. Answers for the paused segment's id are stale.
- A Devamp fired at a target paused in its action is kept: neither ignored nor taken for a resume. The cue stays paused, nothing is sent to the paused segment, and the devamp is pending like one sent before the pause.
- The resume then sends one `DevampVoice` (fresh run, dated on the resume, right after `ResumeVoice`), so no command for the voice is dated before an earlier one. The cue finishes the pass it was paused in, then leaves the loop. A second Devamp meanwhile is `DevampIgnored`; a stop or reset of the paused target drops it.
- A stopped, reset, deleted or broken target cancels a pending devamp and its next cue.
- Looping playlist, running (not stopping): finishes its current child and completes instead of moving on or wrapping; its continue applies (auto-follow unless a child broke at run time) and start-next-cue starts the next cue on the same sample. A paused playlist completes when its child completes after the resume. "Stop target when slice ends" makes no difference.
- Group that is not a looping playlist, or already devamped: `DevampIgnored`. Target not running (idle, loaded, pre-waiting or paused in its pre-wait, stopping, broken): `TargetNotRunning`. Other kind: `TargetWrongKind`.

## Network cue (core side)
- Action has no duration: at action start it emits `Action::SendOsc { cue, at, destination, address, args }` and completes at once, so pre-wait, post-wait and continues behave as for a Memo. `at` is the sample the action started on (OSC is not sample accurate).
- Only an armed cue with no problems emits it. Disarmed: skipped like any disarmed cue. Broken: skipped for its problems.
- Delivery is never the cue's business: it is complete when handed over and a failed send is a warning from the binary. With network output disabled `SendOsc` is received and dropped; the cue still runs, completes and continues.
- Send now: `Input::Preview { cue }` on a Network cue emits one `SendOsc { at: now }` and nothing else: no preview state (`is_previewing` false, no `PreviewChanged`), no cue state, playhead or continue; never `already_previewing`. With problems: `CueBroken`, nothing sent. Disarmed: `PreviewIgnored { reason: disarmed }`. Output disabled drops it.
- Sending rules, limits, workers, host look-ups and "never break for an unresolved host": skill `osc`.

## Display name
`Cue::display_name`: the stored `name` if non-empty; else for an Audio cue with media, the file name with its extension (last `/`-separated component of the stored `MediaRef`, as stored: no case or Unicode-form change; a backslash is a file-name character); else empty.
Assigning or relinking a file never writes `name`, so a named cue keeps its name and an unnamed one follows the file. OSC `/cue/{n}/name` returns the stored name. Shared fixture `engine/fixtures/display_name.json` is used by the Rust and Swift tests.
