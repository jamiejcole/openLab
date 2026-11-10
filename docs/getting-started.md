<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Getting started with openLab

openLab plays the sound cues of a live show. You build a list of cues, then press GO to run them one at a time.
It follows QLab 5's way of working, so most of what you know from there carries over.
This guide covers a first show on macOS 12 or later. Every command named here is in the menu bar, and the menus
show the key for each one.

## 1. Make a workspace

1. Open openLab. The launcher window lists recent workspaces; choose **File > New Workspace**.
2. Save it with **File > Save As…** into the folder that will hold the show's audio. Media inside the show's folder
   is stored by relative path, so the folder can be moved to another Mac as one piece.
3. Open **File > Workspace Settings…** and, on the Audio tab, pick the output device and check the patch routes to
   the outputs you expect. A change of device applies the next time openLab starts.

openLab autosaves while you work. After a crash or power loss it offers the autosave when the show is opened, and
**File > Recover Autosaves…** lists them at any time.

## 2. Add cues

- Drag audio files from Finder onto the cue list: one Audio cue per file.
- Or use the cue toolbar or the **Cues** menu to add a cue of any type.

The cue types in this release:

| Type | What it does |
|---|---|
| Audio | Plays a file. Trim, loop, slice and fade it on its Timing & Loops tab; set levels on its Levels tab. |
| Fade | Changes the levels of a target Audio cue over a time, and can stop it when done. |
| Group | Holds other cues: timeline, playlist, start-first-and-enter, start-first-and-go-to-next, or random. |
| Start, Stop, Pause, Load, Reset | Act on a target cue. |
| Devamp | Lets a looping Audio cue leave its loop. |
| GoTo, Arm, Disarm | Move the playhead to a target, or arm and disarm it. |
| Wait | Counts down a time. |
| Memo | A note in the list; does nothing when run. |
| Network | Sends an OSC message to a network patch. |

A new Audio cue routes file channel 1 to output 1 and channel 2 to output 2. A mono file therefore plays on
output 1 only until you raise the 1-to-2 crosspoint on its Levels tab.

## 3. Edit a cue

Select a cue and use the inspector under the list.

- **Basics**: number, name, target, pre-wait, duration, post-wait, continue mode, notes, colour, flagged, armed.
- **Target**: for a cue that acts on another cue, type the target's cue number, or drag one cue onto the other's
  Target cell in the list. The buttons beside the field clear the target and jump to it. For an Audio cue the
  target is its file.
- With the list focused, **N**, **Q** and **T** edit the selected cue's number, name and target.
- A Fade does nothing until one of its levels is switched on, on its Levels tab.

A red triangle in the list marks a cue that cannot run as it stands: a missing file, no target, nothing to fade.
The Warnings tab of the sidebar says why, and **File > Relink Missing Media…** finds files that have moved.

Undo and Redo cover every edit.

## 4. Run the show

- The playhead (the marker at the left of the list) is on the cue that goes next. Arrow keys move it.
- **GO** is the big button and the Space bar. It starts the cue at the playhead and moves the playhead on.
- **Esc** is Panic: everything fades out over the panic time. Press it twice for an immediate stop.
- The **Transport** menu has Stop All, Pause All, Resume All and Reset All, and the same actions for the selected
  cues. The sidebar's Active Cues tab lists what is running, with pause and panic buttons for each cue.
- A cue's continue mode chains it to the next: auto-continue starts the next cue with it, auto-follow starts the
  next cue when it ends.

## 5. Show Mode

**View > Enter Show Mode** locks the workspace for a performance: nothing can be edited, the cue toolbar and
inspector are hidden, and GO, the standing-by cue and its notes are drawn large in high contrast. Problems appear
as a line in the footer and in the log instead of as dialogs that could cover GO. **View > Enter Edit Mode** goes
back.

## Show-day checklist

1. **Window > Pre-Show Check**: lists broken cues, media files that are missing or cannot be played, audio patches
   with no device, network destinations and engine warnings. It should say no problems were found.
2. Check the footer: it shows the audio device state and a dropout count, which should read zero.
3. Run a cue to each output and listen.
4. Save, then enter Show Mode.
5. During the show, **Window > Show Log** has the record of every GO and every warning.

## Controlling openLab from other software

openLab listens for OSC and uses QLab 5's address space where the feature exists, so existing remotes and control
surfaces set up for QLab can drive it. Passcodes and what each may do (view, edit, control) are set in
**Workspace Settings > OSC**.

## Where things are kept

- The show file is a `.openlab` JSON file; its format is in [`file-format.md`](file-format.md).
- **Help > Show Crash Reports in Finder** opens the folder to attach when reporting a problem.
