# Feedback, lost events and mixer faults

Mixer events, stream state and probe answers become cue-engine inputs. `StartFailed` means the voice never produced audio: the cue is skipped, its auto-follow fires, a playlist moves on. `ActionFailed` is any later failure.

| From | Input |
|---|---|
| `VoiceFinished` (current id) | `ActionEnded { cue, run, at }`; a failure instead if its stream failed (`StartFailed` if no frame was written, else `ActionFailed`) |
| `VoiceFinished { Faulted }` (dropped after repeated render panics or a panic applying a command) | failure with `stream_failed` (also after a pending pause) plus an `audio_fault` notice; `health` logs `faulted_voices` at `error` |
| `VoiceFinished` dated after a pending pause | forwarded (the engine ignores it while paused); record kept; the resume that follows reports `ActionEnded` with the **fresh** run on the resume sample (the mixer ignores a resume of a voice it no longer has) |
| `VoiceFinished` dated on the pause sample | nothing left to pause: handled as an end |
| `VoiceFinished` under an old id after a resume was sent | the voice ended before the resume applied: `ActionEnded` with the fresh run, dated no earlier than the resume |
| `VoiceRejected` | `StartFailed { voice_rejected }` when it is the id the start was sent with (also if a pause/resume were sent meanwhile); `ActionFailed { voice_rejected }` for a refused resume, which also stops the voice still paused under its old id |
| `StreamHandle::error` (polled each tick) | once per run, and the file is probed again: `StartFailed { stream_failed }` if the voice never played (not yet in the mixer, or no frame written), else `ActionFailed`; a loaded unstarted voice reports `ActionFailed` |
| start whose stream cannot be prepared (file gone/changed, failed probe, unbuildable plan) or the mixer cannot take at once | `StartFailed` |
| `DevampScheduled` / `DevampIgnored` | `Input::DevampScheduled { at }` / `DevampIgnored` for the run the id stands for (an old id is stale); after `DevampScheduled` for the current id send the new `ActionLength` |
| `MoveEnded { moves }` | no feedback; when `moves` equals the `MoveVoice`s sent, the slots the voice moved off are free to reclaim |
| `Underrun` | an `underrun` notice. A starved voice does not skip what it missed, so it ends later than planned; the engine keeps the planned end (next cue on time, late tail overlaps) |
| probe answer | `MediaStatus` in one `Availability` per tick: `available` (damaged-but-playable too; damage is a warning), `missing` (`NotFound`), else `unsupported { reason }` |

- **No feedback before its sample**: a report dated `at` (a re-reported end on a resume sample, the end of an empty action) is held until `now > at`. Mixer events report rendered samples, so qualify at once.
- The id a voice had before its latest resume stays an **alias** until the voice is gone (an end before the resume applied arrives under it); older ids are named for one more meter read only, so many pauses keep one alias.
- **Orphaned tails**: after an underrun a voice can sound past the end the engine completed its cue on. Panic, Stop All, Hard Stop All, Reset All, hard stop (`stop` without fade) and Reset stop every such voice in their scope (whole show, or the cue and descendants): Panic with a fade ending on the panic's end, the others at once. A cue's own `panic_cue` leaves the tail to the cue's own end (a later Stop All or Panic cuts it).
- Resuming a voice that had ended is reachable only for an action whose length the engine was not told (a loop).

## Reconciliation of lost events
The mixer drops events when its queue is full and counts `dropped_events`. A lost `VoiceFinished` would leave a cue running for ever. The voice-meter read (about 30 Hz, once per wake across docs) lists every voice the mixer holds, paused included; each doc checks its own started voices against it (ids are unique engine-wide).
- A started voice whose start sample was rendered before the previous read and that is **missing from two reads in a row while `dropped_events > 0`** is taken as ended: `ActionEnded` on the read's sample (`ActionFailed { stream_failed }` if its stream failed).
- A voice no meter read ever listed was never taken by the mixer; its lost report was a rejection: `StartFailed { voice_rejected }`.
- The first loss is logged at `warn`.

## Mixer stage faults
A mixer panic outside any voice is reported as `Event::StageFaulted { stage, at, disabled }` (mechanism: skill `audio-mixer`). The unit is already reset (patch to silence, master to unity). `Control::stage_faulted` (`control/faults.rs`) logs at `error` with the `faulted_stages` count and re-sends settings from the control thread's copy (`Patches`) at `now`.

| Stage | Reset (`disabled: false`) | Disabled |
|---|---|---|
| `Patch(slot)` | re-send the slot's non-zero gains (`Patches::resend`); `audio_fault` notice in the owning doc | slot **quarantined** (never freed or handed out); every voice on it stops with no fade (`Voices::fail_slot`) and its cue breaks as a faulted voice does; the doc syncs so the patch takes another free slot or is `mixer_patches_exhausted`; notice |
| `Output(o)` | nothing to send; notice in every doc | `audio_fault` warning in force in every doc (output silent) |
| `Master` | `SetMaster { 1.0 }`; notice in every doc | warning in force (bypassed at unity; levels unaffected) |
| `Meters` | logged only | warning in force (meters stop) |
| `Mixer` rebuild | every slot's gains and the master (`Patches::rebuilt`); quarantined slots free again; warnings cleared; notice in every doc | none |

- A stage that keeps faulting is logged and told at most once a second of sample time; settings are re-sent every time, and a disabled unit is always told.
- **Lost fault reports**: `health` compares the mixer's `faulted_stages` as read at the previous check with reports handled; on a shortfall it logs and re-sends every slot and the master (`Patches::resend_all`, harmless). A lost *disabled* report is found from the mixer's published `disabled_units`: disabled at the previous check, still disabled now, and unknown to control, is logged and answered as if reported (`lost_disabled_reports`).
