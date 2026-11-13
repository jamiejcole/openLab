# Engine and cue semantics

Format: `Q<id> | decision | source`. `C-` ids are the user's QLab 5.5.3 check pages (2026-10-27). `m:` after a source lists merged ids.

## Pause, panic, failures
Q1 | Cue whose file fails while fading out in Panic/stop fade: shows broken (red X), nothing follows it | user 2026-10-29
Q2 | Pause All holds a paused playlist's next child until Resume; group and failed child keep the paused icon, child shows broken after Resume. Only playlist children show Paused while held; any other paused cue that fails shows Broken at once (m: Q36) | user 2026-10-29
Q3 | Cue paused on its own: its auto-follow/playlist step waits for its resume. A QLab oddity (pause at 4.98 s never followed) is not copied | user 2026-10-29
Q4 | Follow held over a pause: after Resume the rest of the cue plays out, then the next follows (fresh start at the resume sample, no late-start warning) | user 2026-10-29
Q5 | Halted playlist (child broke) with unknown/endless lengths or loop: its auto-follow never fires; the group keeps counting | user 2026-10-29
Q6 | Shuffle switched on during a looping pass: the group name goes italic with a shuffle marker appended at once, but the current pass keeps its order and reshuffles next pass | user 2026-10-29
Q9 | Child GOed on its own while its playlist post-waits: it carries the playlist on if it ends after the post-wait, not if during. C-Q5's 2026-10-27 "nothing happens" run reverted its own edit; this later test stands | user 2026-10-29
Q19 | Undo/redo that removes a playing, paused or waiting cue (or a group holding one) is performed; the cue stops as for Delete | user 2026-10-29
Q23 | Level edit on a playing Audio cue while a Fade moves that level: the Fade completes at once for that cue, the edit wins; auto-follow fires. A Fade set to stop its target does NOT stop it then (safety). Editing one child while a Fade targets the group completes the group Fade and holds the other children (m: Q33, Q34). QLab: the fade does not move the cue's master level display | user 2026-10-29; edge parts assumed
Q30 | Running Wait takes a new duration. Running Fade takes the new duration but keeps its rate and ends where its curve is at that time (10 s to 4 s at 2 s ends at the 4 s level); targets report fading until the new end. Edit during pre-wait applies (m: Q35) | user 2026-10-29
Q28 | Start/Stop/Pause/Resume/Reset Selected on several cues: one engine step at one sample time in selection order, each cue as if alone, Pause/Resume toggles per cue state, cue gone from the workspace skipped with `unknown_cue` warning | assumed (asked 2026-10-29, unanswered)
Q45 | Cue moved into a group while running, or busy child moved out: a group answers only to runs started in it. Moved-in cue's end starts nothing there and neither keeps the group running nor cancels its auto-follow if it breaks. Busy child moved out keeps its group waiting (a drag is not a completion); when it ends a running playlist moves on to the child that took its place, other groups with no busy run complete and follow | assumed 2026-10-31
Q51 | Reset All: forgets a random group's cycle (as just opened); double-GO timer kept; first cue of each list is not shown as loaded (binary primes it after any playhead move); Arm/Disarm changes kept | manual p.97-98 + assumed
Q55 | Audio cue whose slices/trims cannot play with its known file (last slice loops under 50 ms; start trim past file end) is broken before GO with `slices_unplayable`; cleared by an edit or a fitting file; GO skips it | assumed
Q110 | S, Stop Selected or row Panic on a previewed cue also ends the selection's previews (a group's stops the audio cues it started), also in Show Mode. Goes out apart from the cue's own panic (a batch cannot name a cue twice) and is the engine's short stop fade, not panic length | assumed 2026-11-07

## Groups (check pages and decisions)
C-G1 | Random group: the picked child's own auto-continue is honoured | user 2026-10-27 (QLab differs, engine follows QLab)
C-G2 | Child broken at the start of a playlist run: skipped, group still follows out to the next cue. A child that breaks at run time (file trashed while the group runs): the user's QLab run still followed out after the group's length (C-Q2); openLab cancels the group follow for a run-time break (Q203) | user 2026-10-27
C-G3 | Playlist skips a child broken before the run and plays the next | user 2026-10-27
C-G4 | GO on a child of an idle group: playlist mode plays that child then the following siblings; timeline mode only that child; neither shows the group running | user 2026-10-27
C-G5 | Playhead after GO on a child: timeline, next sibling; playlist, the next cue outside the group | user 2026-10-27
C-G6 | Group with a broken child shows broken like the child but can still be GOed; a broken descendant gives the group a warning | user 2026-10-27
C-Q1 | GO on a child of a playlist with auto-follow: playhead goes to the next cue and it does not start; GO on the group itself: playhead to the cue after and it starts after the group | user 2026-10-27
C-Q2 | Playlist child breaking while playing: plays on briefly then shows broken; group bar runs on; the cue after the group starts after the group's total length | user 2026-10-27
C-Q3 | GO on a child of a stopped Random or Start-first group: only that child plays; playhead to next child in both | user 2026-10-27
C-Q4 | Playlist played on from a child: Loop on wraps to first child; Shuffle on with loop off plays in order; shuffle switched on mid-pass keeps order | user 2026-10-27
C-S3 | Playlist "play next" on the last child: group stops on the third GO | user 2026-10-27
C-S4 | "Play previous" on first child stops it; on second goes back to child 1 | user 2026-10-27
C-S6 | Group starting a child already playing: child restarts from top alongside the others | user 2026-10-27
Q201 | Random group repeat rule: every armed child starts once before any starts a second time (round-robin) | manual: group-cues
Q202 | Playhead next/previous enters only "start first and enter" groups (first child starts, playhead advances inside); timeline groups: playhead moves past the group | manual: group-cues
Q203 | Group auto-follow after a child broke at run time: none | assumed

## Devamp, Fade, Arm
C-D1 | Devamp "start next cue" starts the cue after the Devamp cue at the moment it devamps its target | user 2026-10-27, manual: devamp-cues
C-D2 | Devamp then pause/resume: devamp kept; the next cue plays after the devamp even while the target is paused | user 2026-10-27
C-D3 | Devamp on a non-looping cue: nothing; Devamp cue shows no duration/progress | user 2026-10-27
C-D4 | Devamp during a non-looping slice: no effect, a later loop keeps looping (not carried over) | user 2026-10-27, assumed for carry-over (m: Q212)
C-D5 | Devamp "start next cue": playhead moves to the next cue immediately after the Devamp GO; that cue starts when the target leaves its loop; playhead stays on it | user 2026-10-27
C-D7 | Devamp mode for the currently looping slice goes to slice 2, then the cue plays again and slice 1 loops forever; devamp for the looping cue while slice 1 loops does nothing. One behaviour: leave whatever loop plays | user 2026-10-27
C-F1 | Fade on a stopped target runs its duration, target untouched | user 2026-10-27
C-F2 | Fade targeting a group is relative: each child moves by the same dB, not to one level. Relative to the level at the ramp's first sample, capped at +12 dB | user 2026-10-27
C-F3 | Fade on a paused target applies; level is there on Resume | user 2026-10-27
C-F4 | Fade with no level changes and stop-when-done is broken and does not run (so the case cannot be made) | user 2026-10-27
C-A1 | Reset leaves Arm/Disarm changes (target stays disarmed) | user 2026-10-27
C-A2 | Arm/Disarm counts as an edit (dirty dot; persists after save and reopen) | user 2026-10-27
Q204 | Devamp "start next cue" replaces the Devamp cue's own continue; pending devamp is re-sent after its target pauses | assumed
Q212 | Devamp on a non-looping slice/playlist/audio cue is ignored, not carried to a later loop (a carried-over devamp would surprise an operator who pressed early) | assumed
Q225 | Devamp while a non-looping slice between two loops (a bridge) plays: leaves at that slice's end, i.e. no effect; the next loop keeps looping. Same rule as Q212 applied inside one slice table | assumed (searched: devamp-cues manual page: silent on bridges)
Q213 | Fade on a non-running target warns, runs its duration, no effect | assumed
Q214 | Trigger on a cue fading out in a panic hard-stops it and starts afresh; the hard StopVoice can click, the binary may send a short stop fade | assumed
Q205 | Stop-target-when-done stops the target when the fade ends | manual: transport-cues

## Panic, pause, stop, triggers
C-P1 | Resume All resumes everything paused, including hand-paused cues | user 2026-10-27, manual: workspace
C-P2 | GO during a Panic fade starts normally; the old cue keeps fading | user 2026-10-27
C-P3 | Restarting a cue that is fading from a Panic cuts the old sound almost at once, no click | user 2026-10-27
C-P4 | Audio cue routed to a patch with no device is broken (red X); GO does nothing | user 2026-10-27
C-P5 | GO on a broken auto-follow cue: it does not play, the next cue starts and playhead moves past | user 2026-10-27
C-P6 | One Esc = Panic (fade over panic time); double Esc = hard stop all; also Panic Selected (S); no separate Stop All key | user 2026-10-27, manual: keyboard shortcuts
C-P7 | Panic does not reset double-GO protection; the second GO is ignored | user 2026-10-27
C-Q6 | Panic fade shape is linear | user 2026-10-27 (tail may only sound faster)
C-Q7 | Panic Selected (S): only the selected cue fades over the panic time; a second press stops it at once | user 2026-10-27
C-T1 | Start cues triggering each other are not broken; bounded loop until Panic | user 2026-10-27 (trigger cycle gives a load warning only, Q206)
C-S1 | Second trigger on a paused cue resumes it from where it was paused | user 2026-10-27
C-S2 | Trigger on a cue fading from Panic restarts it (hard stop then fresh start; a short stop fade is the safer default for the binary) | user 2026-10-27 (m: Q214)
C-S5 | "On release" of a held cue: release stops it, never resumes or restarts; start, pause, press resumes, release stops again | user 2026-10-27
C-C1 | Gapless auto-follow has a tiny barely audible click in QLab; openLab has no gap | user 2026-10-27
Q206 | Trigger cycle between cues is not broken (warning at load only) | assumed
Q207 | Unmapped patch makes its cues broken (docs list no causes) | assumed
Q208 | UI must show the playhead is inside a group | assumed

## Preview
C-V1 | Groups can preview (their armed audio cues, all together whatever the mode); Fade cannot | user 2026-10-27
C-V2 | Preview of a disarmed cue is refused (`disarmed`) | user 2026-10-27
C-V3 | Preview again while previewing does nothing | user 2026-10-27
C-V4 | Pause All pauses previews, Resume All resumes them. Panic fades a playing preview over panic time and stops a paused one at once | user 2026-10-27
C-V5 | Preview starts at the cue's own start (QLab measured 10 s in; kept) | user 2026-10-27
C-V6 | Several previews at once allowed; Esc fades them, second Esc stops at once | user 2026-10-27
C-Q8 | Preview uses the cue's own levels and built-in fade-in (linear curve) | user 2026-10-27
Q209 | Preview skips pre-waits, ignores auto-continue/auto-follow; several previews start through normal outputs | manual: auditioning-cues
Q210 | StopPreview fade is 50 ms | assumed
Q211 | Only audio cue kinds preview (no video kind built) | manual: auditioning-cues

## Basics and Triggers fields
Q270 | Auto-load: the cue is loaded (to 0) when the cue before it at its level (skipped cues passed over) starts, not when that cue ends. Only an idle, armed cue without static problems loads; one already loaded, running or broken is left alone, a disarmed one is not loaded. A group auto-loads as Load does | manual p. 128 + assumed (asked 2026-11-13, unanswered: auto-load-when)
Q271 | Skip if disarmed: a skipped cue is as though absent. The playhead may rest on one (navigation, first cue on open or after Reset All, disarmed while standing by), but a GO there starts the next cue that is there (along its level, else after its group) and moves the playhead from that cue; none left: as GO past the end. A trigger of the skipped cue starts nothing; playlists and random groups pass over it | manual p. 128 (resting assumed; skip-playhead asked 2026-11-13, unanswered)
Q272 | Second colour: a cue counts as started from the first trigger that makes it run (a disarmed cue's timing run too; a skipped cue never). A Reset (operator or Reset cue) of a group brings back the first colour of its children too; stop and panic do not | manual p. 127 + assumed (searched: second color, reset)
Q273 | A hotkey trigger switched off keeps its key and starts nothing (also on release); a show from before the switch has its hotkeys on. A cue has no OSC address of its own: OSC starts a cue by number or id | manual pp. 128-129
Q275 | Fade and stop others acts when the cue's action starts (after its pre-wait, as the duck does), with a linear fade. It stops paused and pre-waiting cues too (at once), leaves loaded cues loaded, stops a peer group with everything in it, and leaves a cue already fading out (stop fade, panic) to its own fade. A disarmed cue acts on nothing | manual pp. 129-130 + assumed (asked 2026-11-13, unanswered: fade-stop-states)
Q274 | Colours: eight basic and twelve extended, stored under openLab's own names (none, red, orange, yellow, green, cyan, blue, purple, magenta; dark_red, apricot, olive, dark_green, light_blue, navy, indigo, lilac, plum, raspberry, pink, grey) | manual p. 127, UI map context.cart-tile (order)
