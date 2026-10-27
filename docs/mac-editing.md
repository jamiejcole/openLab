<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Mac app: cue editing (M5.10 part 1)

All structural edits go through `CueEditor` (one per window) and send core's commands ([commands.md](commands.md)); the cue list updates from the engine's `model_changed` echo. Every path refuses in Show Mode (menu items disabled, toolbar disabled, keys ignored, rows not draggable, editor entry points return at once). Typed inspector edits are committed first (`commitPendingEdits`); if that fails the edit is not made.

| Action | Keys (QLab 5 keyboard-shortcuts page) | Command sent |
|---|---|---|
| New cue | Cmd-0 Audio, Cmd-6 Fade, Cmd-9 Group; the other kinds have a Cues-menu item and a toolbar button, no key | `add_cue { at, cue, auto_number: true }` |
| Delete | Cmd-Delete (Edit menu) | `remove_cue`, or one `transaction` for several cues |
| Renumber | Cmd-R (sheet: first number, increment) | `renumber { ids, start, increment }` |
| Move up / down | Option-Cmd-Up / Down | `move_cue` (one `transaction` for several) |
| Drag in the outline | - | `move_cue` within a parent, `reparent_cue` to another group, one `transaction` for a multi-cue drag |
| Undo / Redo | Cmd-Z / Shift-Cmd-Z | `undo` / `redo` messages |

Undo and Redo are enabled from the engine's `document.can_undo` / `can_redo`; while a text field is being edited, Cmd-Z undoes the text first.

## Decisions where QLab 5's documentation is silent

- **Where a new cue goes**: right after the last selected cue, beside it (same group, same list). A selected group gets the cue after the group, not inside it (nothing is added to a group by accident). Nothing selected: the end of the list. The new cue is selected, the playhead moves to it and the inspector shows it.
- **Auto number**: `auto_number: true`; core numbers it from the workspace's auto-number settings (ED-08).
- **Delete**: no confirmation (it is one undo step). Deleting a group deletes its children. A cue that is running, paused, pre/post-waiting, fading, stopping or being previewed, or a group holding one, is refused with a sheet ("stop it first"): cutting a sound the audience may hear is never done by a stray key. Afterwards the next cue (else the previous, else the parent) is selected.
- **Delete key**: only Cmd-Delete, as QLab lists. Cmd-Delete is left to the text field while one is being edited.
- **Drag**: dropping on a group puts the cue at the end of it; on any other cue, after it. A cue cannot be dropped into itself or its own descendants (no drop indicator). Dragging a selected row drags the whole selection, in list order.
- **Renumber**: core's rules (cue-list order, whatever the selection order). The sheet refuses a first number below 0 or an increment of 0 or less before sending.
- **Audio cue with no patch**: refused with a message (core needs a patch); the new cue has no file yet.
- **Defaults**: Wait 5 s, Fade 3 s (linear), others as core's zero values.
- **Arrow keys** are the outline's own; selecting a cue moves the playhead (M5.11). Return does not edit the name (QLab binds N to the number, not built here).

Left for M5.10b: drag files from Finder, drop-to-target, multi-select batch edit, copy/paste, find, fade-this-out, dragging cues onto another cue list in the sidebar, Network cue creation, and the Target toolbar button (the model has no such cue).
