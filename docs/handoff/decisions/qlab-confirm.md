# QLab 5 confirmations (M2.16, M2.17, M2.18, M2.20, M3.24, M1.14)

Decided 2026-10-27 by the orchestrator from QLab 5's published documentation (https://qlab.app/docs/v5/), for the user to review. Status values: **confirmed by docs**, **settled by user checks** (M2.24, the QLab checks page), **decided unconfirmed** (docs silent: "unconfirmed — decided by orchestrator", current behaviour kept), **changed** (code changed), **filed**. No code changed: the docs showed no difference from openLab.

Pages read (base `https://qlab.app/docs/v5/`): fundamentals/group-cues, fundamentals/workspace, other-cues/devamp-cues, other-cues/transport-cues, other-cues/arm-and-disarm-cues, tools/auditioning-cues, audio/fading-audio, general/keyboard-shortcuts.

## M2.16 Groups

| Point | openLab | QLab docs | Decision | Status |
|---|---|---|---|---|
| Random: repeat rule | round-robin, every playable child once per cycle | "each child cue that is armed will be started once before any of the cues is started a second time" (fundamentals/group-cues/) | keep | confirmed by docs |
| Random: child continue modes | the chosen child keeps its continue mode | silent | keep | settled by user checks (G1) |
| Group auto-follow after a child broke | none for a child broken at run time | silent | keep | decided unconfirmed |
| Playlist and a broken child | skips a child broken before the run, still auto-follows | silent (only: a looping Playlist of zero-duration cues "will report as broken") | keep | settled by user checks (G2); run-time case decided unconfirmed |
| GO on a child of an idle group | child alone; playlist plays on through siblings, timeline only the child | silent | keep | settled by user checks (G4/G5) |
| GO from a child of timeline/playlist/random | moves to the next sibling | silent | keep | settled by user checks (G4/G5) |
| PlayheadNext/Previous entering groups | only "start first and enter" groups | "its first child cue starts and the playhead advances to the next child cue within the Start First And Enter Group cue"; for timeline groups the playhead moves past the group | keep | confirmed by docs |
| UI must show the playhead is inside a group | UI task | silent | belongs to M5 | decided unconfirmed |

## M2.17 Control cues and Fade

| Point | openLab | QLab docs | Decision | Status |
|---|---|---|---|---|
| Devamp "start next cue" | starts the cue after the Devamp cue, at the devamp sample | "the Devamp cue starts the cue which follows it exactly simultaneously with the moment that it devamps its target cue" (other-cues/devamp-cues/) | keep | confirmed by docs |
| "start next cue" replaces the Devamp cue's continue / playhead | yes | silent beyond the line above | keep | playhead part settled by user checks (D5); rest decided unconfirmed |
| Pending devamp re-sent after a pause of its target | yes | silent about pause | keep | decided unconfirmed |
| Devamp of a non-looping playlist / audio cue | ignored / whatever the mixer answers | silent ("If that slice is looping, playback exits the loop and continues on to the next slice") | keep | decided unconfirmed |
| Fade of a non-running target | warns, runs its duration, no effect | silent (only: the live position indicator needs the cue loaded or running; audio/fading-audio/) | keep | decided unconfirmed |
| Fade of a group | absolute, per child audio cue | silent on groups; "absolute fades supersede relative ones" | keep | decided unconfirmed (absolute part confirmed) |
| Fade on a paused target | applies | silent | keep | decided unconfirmed |
| "Stop target when done" | stops the target when the fade ends | "the target cue will stop once the fade is complete" | keep | confirmed by docs |
| Fade with nothing to fade + stop | cannot happen (broken) | n/a | moved to M2.26 earlier | settled by user checks (F4) |
| Reset and Arm/Disarm changes | Reset does not undo them | Reset "will stop its target cue and reset any temporary changes that have been made to the cue since the workspace opened" (other-cues/transport-cues/); arm state is not described as temporary | keep | decided unconfirmed |
| Arm/Disarm marks the workspace edited; `SetArmed` persists on save | persists | arm-and-disarm-cues page silent | keep; M4.2 decides the edited flag | decided unconfirmed |

## M2.18 Panic, Resume All, broken cues

| Point | openLab | QLab docs | Decision | Status |
|---|---|---|---|---|
| Resume All | every paused cue, however paused | "Resume All: resumes all currently paused cues in the workspace" (fundamentals/workspace/) | keep | confirmed by docs |
| Panic = fade then stop over the panic duration; second Panic hard stops | `panic_duration` setting | "panic means to fade and stop all cues using the panic duration", set in Workspace Settings; "Double-tap this keyboard shortcut to hard stop all cues" (general/keyboard-shortcuts/) | keep | confirmed by docs |
| Panic fade curve | linear | silent | keep | decided unconfirmed |
| Panic and double-GO protection | not reset | silent | keep | decided unconfirmed |
| GO during the panic fade | starts cues normally | silent | keep | decided unconfirmed |
| Unmapped patch makes its cues broken | yes | "A red X means the cue is broken and cannot be played"; causes not listed | keep | decided unconfirmed |
| Group with a broken child | shows broken, can be GOed; group warning for a broken descendant | silent | keep | settled by user checks (G6) |
| Trigger cycle is not broken | warning at load only | silent | keep | decided unconfirmed |
| Trigger on a cue fading out in a panic | hard-stops it and starts it afresh | silent | keep | decided unconfirmed |
| GO on a broken auto-follow cue | starts the next cue, playhead moves past | silent | keep | settled by user checks (P5) |
| Hard `StopVoice` at that restart can click | M4.2 may send a short stop fade | silent | keep core as is; the existing M4.2 note stays (a short fade is the safer default for the binary) | decided unconfirmed |

## M2.20 Preview / audition

| Point | openLab | QLab docs | Decision | Status |
|---|---|---|---|---|
| Cue kinds that can be auditioned | audio only (v0.0.1 has no video) | audible output of "Audio, Mic, Video, and Camera cues" can be previewed (tools/auditioning-cues/) | keep | confirmed by docs (scope-limited) |
| Group / Fade preview | not previewable | silent | keep | decided unconfirmed |
| Disarmed cues | allowed | silent | keep | decided unconfirmed |
| Second Preview of a previewing cue | ignored (UI toggles with StopPreview) | silent | keep | decided unconfirmed |
| Several previews at once | any number, one per cue | previewing several cues "will start the selected cue or cues through normal outputs" | keep | confirmed by docs |
| Pre-waits, auto-continue, auto-follow | skipped / ignored | Preview skips pre-waits and ignores auto-continues and auto-follows | keep | confirmed by docs |
| Panic stops previews (hard) | yes | silent | keep | decided unconfirmed |
| Pause All leaves previews | yes | silent | keep | decided unconfirmed |
| StopPreview fade | 50 ms | silent | keep | decided unconfirmed |
| Start position | the cue's own start | silent | keep | decided unconfirmed |
| Show's fade-ins in a preview | the cue's own levels and fades | silent | keep | decided unconfirmed |

## M3.24 Devamp during a non-looping slice

| Point | openLab | QLab docs | Decision | Status |
|---|---|---|---|---|
| Devamp while a non-looping slice (bridge) plays | leaves at the bridge's end: no effect, the next loop keeps looping | the action fires "When the playback of the target cue reaches the end of the currently-playing slice"; "If that slice is looping, playback exits the loop and continues on to the next slice" (other-cues/devamp-cues/). Nothing about a non-looping slice | keep (a devamp carried over to a later loop would surprise an operator who pressed too early) | decided unconfirmed |

## M1.14 UI-owned settings

| Point | openLab | QLab docs | Decision | Status |
|---|---|---|---|---|
| Where window layout and recent files live | not in `MachineSettings` | silent (workspace page: windows belong to a workspace; no storage described) | UI-owned, stored in the app's UserDefaults; engine `settings.json` keeps engine/machine settings only. ARCHITECTURE §5 said "preferences" without naming window layout; clarified there | decided unconfirmed (WS-06 only says window layout is per-machine) |

## Counts

Rows: 49 (confirmed by docs 9, settled by user checks 7 (one row mixed), decided unconfirmed 33). Code changes: 0. Filed tasks: 0.
