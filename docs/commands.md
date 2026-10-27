# Editing commands

Every change to a workspace is a `Command` (`openlab-core`, `command.rs`, and `command_model.rs` for everything but the cue tree): cues, workspace settings, output and network patches, cue lists (ED-07). The UI sends them, OSC edits become them, and mirroring (RED-01) will replay them. Commands are serialisable (`serde`, internally tagged by `type`) and travel inside the UI protocol's `command` message ([protocol.md](protocol.md)).

```rust
let inverse = workspace.apply(command)?;   // Result<Inverse, CommandError>
```

`Inverse` is a `Command`: applying it undoes the original, and applying *that* result redoes it. `History` keeps the undo and redo stacks of inverses and is the only thing that should change a workspace in the engine (`History::execute`, `execute_coalescing`, `undo`, `redo`). Undo is unlimited (ED-07). Call `History::clear` when the workspace is replaced (reload, revert).

`History::execute` returns `Ok(true)` if the command changed the workspace and was recorded, `Ok(false)` if it changed nothing.

## Guarantees

- **Atomic.** A refused command leaves the workspace exactly as it was.
- **Exact.** Undo then redo restores ids, order and parent/child structure.
- **Valid.** A command never produces something `repair` would have to fix, so `save_show` accepts the result and the saved file loads back identical (NFR-03):
  - cue ids unique across the workspace; numbers unique and not empty;
  - nesting at most `MAX_GROUP_DEPTH`; checked before recursing, so a hostile tree cannot overflow the stack;
  - every time and level finite and in range (the same checks as load).
- **References between cues are not checked.** A missing target, a wrong-kind target, a self-target, a missing patch and trigger cycles are the *broken-cue state* of ARCHITECTURE §10: `validate` warns, the cue shows as broken. Removing a cue legitimately leaves them behind, and undo must be able to put back states that contain them. The exception is removing a patch or cue list that something still uses: see "Settings, patches and cue lists".
- **A change to what is already there is not an edit.** `SetCueProperty` to the value the cue has, a `MoveCue` in place, `SetCueNumbers` that changes no number, a setting or patch property set to its current value and the like return an empty `Transaction` as their inverse, which `History` does not record: no undo step, no redo cleared, and the engine sends no `model_changed`, makes no revision and leaves the document clean. (Values are validated first: an invalid value is refused even if the field "already has" something equal-looking.)

## Commands

| Command | Notes |
|---|---|
| `AddCue { at, cue, auto_number }` | A Group brings its children. `auto_number` replaces the cue's number with `next_auto_number`. |
| `Paste { at, cues }` | One undo step. Ids must be fresh: build `cues` with `prepare_paste`. |
| `RemoveCue { id }` | Removes the subtree. Its inverse re-adds it at the same place. |
| `MoveCue { id, index }` | Reorder among current siblings. |
| `ReparentCue { id, to }` | Into another group or cue list, children with it. Not into itself or its descendants. |
| `SetCueProperty { id, property }` | `CueProperty`: name, notes, colour, flagged, armed, pre/post wait, continue mode, triggers, second trigger, second trigger on release, target, group mode, or the whole kind (never to or from Group). |
| `BatchSet { ids, property }` | One property on several cues, all or none. Not `Kind`. |
| `SetCueNumbers { numbers }` | Set, clear or swap numbers; uniqueness is judged on the result. |
| `Renumber { ids, start, increment }` | Numbers `ids` in cue-list order (a group before its children), whatever order `ids` is in. |
| `Transaction { commands }` | One undo step; if any command is refused, none applies and the error names its index. No nesting; the inverse is flat. Commands that changed nothing add nothing to the inverse. |
| `SetWorkspaceSetting { setting }` | `WorkspaceSetting`: `min_go_interval`, `panic_duration`, `autosave_interval`, `osc_passcode` (`null` clears), `master_db`, `hotkey { key, value }` (`key`: `go`, `stop_all`, `pause_all`, `resume_all`, `playhead_next`, `playhead_previous`; `value` null = no key), `auto_number_start`, `auto_number_increment`. Each is the whole new value. |
| `AddPatch { index, patch }` | Output patch at `index` (0 to the current count). Id unique, values valid (1 to 64 outputs, levels, delays). |
| `RemovePatch { id, even_if_used? }` | Refused (the error names a cue) while an Audio cue routes to it. `even_if_used` (default false) is only for the inverse of an add; see below. |
| `MovePatch { id, index }` | `index` counts the patches after taking this one out. |
| `SetPatchProperty { id, property }` | `PatchProperty`: `name`, `master_db`, `output_level`, `output_mute`, `output_delay`, `output_polarity` (each `{ output, value }`), `channel_count { value }` (1 to 64; new outputs 0 dB, unmuted, no delay), `outputs { value }` (replaces all). Shrinking keeps cue routing to the dropped outputs (silent, a warning), and its inverse is `outputs`, so undo brings the dropped outputs back with their settings. |
| `AddNetworkPatch { index, patch }`, `RemoveNetworkPatch { id, even_if_used? }`, `MoveNetworkPatch { id, index }`, `SetNetworkPatchProperty { id, property }` | As for patches. `property`: `name`, `host`, `port`, `protocol`. Removal is refused while a Network cue uses the patch. |
| `AddCueList { index, id, name }`, `RemoveCueList { id }`, `MoveCueList { id, index }`, `RenameCueList { id, name }` | A new list is empty. Removing is refused for a list that has cues (move or remove them first, in one `Transaction` if it should be one step) and for the only list. |

`Position { list, parent, index }`: `parent` is `None` for the top level of the list. `index` counts the cues already there (after taking out the moved cue, when moving).

## Settings, patches and cue lists

- **Valid.** A new or changed value must pass what load checks (`validate`/`repair`), including values `repair` would silently normalise (a level below silence): it is refused with the issues listed. An OSC passcode is 1 to `MAX_OSC_PASSCODE_BYTES` (256) bytes; clear it with `null` (NET-02).
- **In use.** Removing a patch, network patch or cue list that something uses is refused, so a UI cannot orphan cues by accident. A cue that already points at a patch the workspace lacks is the usual broken-cue state. Two things follow: `RemovePatch` / `RemoveNetworkPatch` take `even_if_used`, which `AddPatch` / `AddNetworkPatch` put on their inverse (the patch may have been added to mend a cue that already pointed at it, and undo must restore exactly, not be blocked by that cue); and a client may send it too, to remove a patch on purpose and break its cues.
- **Playing.** Patch positions are mixer matrix slots until the stable-slot work (PROGRESS M4.2f): adding a patch before another, removing one or moving one shifts later patches' slots. The engine therefore refuses such an edit, an undo or a redo (`command_refused`) while an Audio cue routed to an affected patch is running, or its voice is still on the device (`Command::patch_slot_floor` says which patch index is the first affected; appends and property edits are always allowed). Network patches, cue lists and settings have no such rule. Remove the guard with M4.2f.
- **Footprints.** `Footprint` also names the cue lists, patches and network patches an edit touched (`Touched`: changed, removed, reordered) and whether settings changed; `model_changes` turns them into `cue_list`, `patch`, `network_patch` and `settings` upserts and deletes. A move sends every member of the collection, each at its final index, in index order, which sorts the client's copy whatever the old order. An edit that is `is_model_wide()` makes the control loop call `CueEngine::model_changed` (patches decide which cues are broken, lists have playheads) instead of the cue-scoped `model_changed_cues`.

## Coalescing (fader drags)

`History::execute_coalescing(workspace, command, key)`; on the wire, `coalesce` on the `command` message ([protocol.md](protocol.md)). An edit with a key joins the previous edit's undo step if that edit had the same key and nothing but such edits happened since: no other command without or with another key, no undo or redo, no `History::clear`. A client sends a fresh key for each gesture (for example `fader:<cue>:<n>`) and none for a one-off edit. A no-op edit and a refused one leave the gesture open.

Every edit is still applied and sent to clients at once; only the undo step is shared. Undoing a gesture restores the state before its first edit, redo the state after its last. When the new step and the gesture's step each set the same single value (the same cue property, setting, patch property, name, or the same item's position), the merged step is the older one alone, so a thousand-event drag is one small command. Otherwise the steps are chained in one transaction, newest undone first. Core has no clock, so a gesture ends only by the events above; a client that sends the same key forever merges forever.

## Helpers

- `next_auto_number(&Workspace)`: one `auto_number_increment` above the highest numeric cue number in the workspace, never below `auto_number_start`; plain decimal text (`0.3`, not `0.30000000000000004`).
- `prepare_paste(&Workspace, &[Cue], new_id)`: copies a clipboard for pasting into any workspace. Fresh ids from `new_id` (core cannot make ids); targets inside the copied set follow to the copies, others are kept if they exist in the destination and cleared otherwise; numbers already in use are cleared; a missing patch or network destination becomes the destination's first.
- Cut is copy + `RemoveCue`; duplicate is `prepare_paste` + `Paste` — both in a `Transaction` when they must be one step.
