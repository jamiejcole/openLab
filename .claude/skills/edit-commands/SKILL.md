---
name: edit-commands
description: Load when adding, changing or reviewing an editing Command in openlab-core (command.rs, command_model.rs, command/, History, undo/redo, Transaction, Paste, settings, patches, cue lists, OSC passcodes), a size or name limit, or Command::prepare.
---

# Editing commands

Every change to a workspace is a `Command` (`openlab-core`: `command.rs` with `command/`, `command_model.rs` for everything but the cue tree): cues, workspace settings, output and network patches, cue lists (ED-07). The UI sends them and OSC edits become them. Commands are serde types tagged by `type` and travel in the protocol's `command` message (`docs/protocol.md`).

```rust
let inverse = workspace.apply(command)?;   // Result<Inverse, CommandError>
```

`Inverse` is a `Command`: applying it undoes the original, applying that result redoes it. `History` holds the undo and redo stacks of inverses and is the **only** thing that changes a workspace in the engine (`execute`, `execute_coalescing`, `undo`, `redo`). Undo is unlimited. Call `History::clear` when the workspace changes behind its back. `execute` returns `Ok(true)` if it recorded a change and `Ok(false)` if it changed nothing.

## Must not break
- **Atomic.** A refused command leaves the workspace exactly as it was. The one exception, `RollbackFailed`, means an inverse is wrong.
- **Exact.** Undo then redo restores ids, order and parent/child structure.
- **Valid.** A command never produces something `repair` would fix, so `save_show` accepts the result and the saved file loads back identical (NFR-03).
- **Limits hold on every path that creates or replaces the data** (add, paste, clipboard paste, kind replacement, batch, rename), each path tested (CLAUDE.md rule; applied in `references/limits.md`).
- **An undo or redo is never refused because a cue is playing**, and is never refused for a value problem (`apply_restoring`).
- **A no-op is not an edit**: no undo step, no revision, no `model_changed`, document stays clean.
- **A hostile client cannot overflow the stack or fill memory through one command**: depth is checked before recursing; memory bounds are refused.
- **A volume limit can never be exceeded by any command**, fader or peer (see `references/settings-hotkeys-volume.md`).

## Guarantees
- **Valid** means:
  - cue ids unique across the workspace; numbers unique and not empty;
  - nesting at most `MAX_GROUP_DEPTH`, checked before recursing;
  - every time and level finite and in range (the same checks as load).
  A new or changed value must pass what load checks (`validate`/`repair`), including values `repair` would silently normalise (a level below silence), and is refused with the issues listed.
- **Cue-to-cue references are not checked.** A missing target, wrong-kind target, self-target, missing patch and trigger cycles are the broken-cue state: `validate` warns and the cue shows broken. Removing a cue legitimately leaves them, and undo must be able to restore states that contain them. The exception is removing a patch or cue list something still uses (below).
- **A change to what is already there is not an edit.** `SetCueProperty` to the current value, a `MoveCue` in place, `SetCueNumbers` that changes nothing, a setting or patch property set to its current value return an empty `Transaction` as their inverse, which `History` does not record. Values are validated first: an invalid value is refused even if the field "already has" something equal-looking.
- **Undo and redo while cues play.** Performed whatever is playing, as in QLab 5. A step that removes an active cue (or turns it into a cue of the other sort, group or not) stops it as a deliberate `RemoveCue` does; a step that leaves every active cue in place stops nothing. Engine behaviour and the `CueRemoved` warning: skill `cue-engine`.

## Network cue properties
`destination`, `address` and `args` of `CueProperty` apply to Network cues only (`PropertyNotApplicable` otherwise). They undo, redo, coalesce (one gesture key, one slot per property) and batch like other properties.
- As in QLab 5, the editor **accepts a malformed message and a destination naming no network patch**: the cue shows broken (`network_message_invalid` / `network_patch_missing`) and never sends.
- Only memory bounds are refused (`InvalidMessage(reason)`, nothing changes): see `references/limits.md`.
- Every path that creates or replaces a Network cue applies the same bounds: `AddCue`, `Paste`, `PasteClipboard` (each new cue at any depth in a pasted group) and `SetCueProperty { Kind }`, from `subtree_issues`, refused as `InvalidCue` (`TooLarge`). Every refusal reaches the client as `command_refused`. Load keeps an over-limit message and warns.

## Commands
Variants are in `command.rs` / `command_model.rs`; only non-obvious rules here.

| Command | Rules |
|---|---|
| `AddCue { at, cue, auto_number }` | A Group brings its children. `auto_number` replaces the number with `next_auto_number`. |
| `Paste { at, cues }` | One undo step. Ids must be fresh (build with `prepare_paste`). Still accepted on the wire; its media is converted too. |
| `PasteClipboard { at, cues, source_dir? }` | The raw clipboard (cues copied in any workspace). Core's `apply` refuses it (`NotPrepared`); only `Command::prepare` turns it into a `Paste` of `prepare_paste` (one undo step). A clipboard nested deeper than the limit is refused. |
| `source_dir` of `PasteClipboard` | Absolute folder of the show the cues came from (absent for a never-saved show). Relative media paths are read against it and stored for this show by the media-path rule. Missing, relative or same-as-this-show leaves paths as written. |
| `RemoveCue { id }` | Removes the subtree. Its inverse re-adds it at the same place. |
| `MoveCue { id, index }`, `ReparentCue { id, to }` | Reorder among current siblings; reparent into another group or cue list, children with it, never into itself or a descendant. |
| `SetCueProperty { id, property }` | See below. |
| `BatchSet { ids, property }` | One property on several cues, all or none. Not `Kind` or `KindFields`. Applied in place with each cue's old value kept; a refusal writes the old values back, so cost follows the cues named, not the show. |
| `SetCueNumbers { numbers }`, `Renumber { ids, start, increment }` | Set, clear or swap numbers; uniqueness is judged on the result. `Renumber` numbers `ids` in cue-list order (a group before its children) whatever order `ids` is in. |
| `Transaction { commands }` | One undo step. If any command is refused none applies and the error names its index. No nesting; the inverse is flat; commands that changed nothing add nothing. Rollback applies the inverses of the commands already applied, newest first, as an undo would (the workspace is not copied, so cost follows the commands). If a rollback step is refused, an inverse is wrong: `RollbackFailed`, the workspace is valid but partly changed, and `History` drops both stacks. |
| `SetWorkspaceSetting`, `AddOscPasscode`, `RemoveOscPasscode`, `SetOscRights` | `references/settings-hotkeys-volume.md` (settings) and below (passcodes). |
| Patch, network patch, cue list commands | Below. |

`Position { list, parent, index }`: `parent` is `None` for the top level. `index` counts the cues already there (after taking out the moved cue, when moving).

**`SetCueProperty`.** `CueProperty` names the simple fields (name, notes, colour, flagged, armed, waits, continue mode, triggers, target, group mode, ...), the whole `Kind` (never to or from Group), or `KindFields`.
- `KindFields` merges the named fields into the kind the cue has **when the command arrives**, so a change another client made to other fields meanwhile is kept. Two clients changing the same field: last write wins. `type`, `children` and `media` cannot be named. Not allowed in `BatchSet`. Undo restores the whole old kind. A fader or matrix drag sends every move with one `coalesce` key; typed values are one-off edits.
- Audio `media`: the file alone (`value` a path or `null`; levels, routing, fades, slices untouched).
- Wait/Fade `duration` and Load `load_to_time`: seconds, finite, 0 to `MAX_TIME` (24 h); anything else is `InvalidCue` with an `InvalidTime` issue naming the field. A running Wait or Fade takes the new duration (a Fade keeping its rate); a Load cue reads its time when its action starts.

**Passcodes.** `AddOscPasscode { index, passcode, rights }`: `index` 0 to the row count; `passcode` 1 to 32 ASCII digits, not already in the table; `rights {view, edit, control}`, Edit and Control need View; at most 16 rows (`MAX_OSC_PASSCODES`). Undo removes it.
`RemoveOscPasscode` may remove any row, the last too (nobody can then connect with a passcode); undo brings it back at its place with its rights, even a legacy non-digit passcode. `SetOscRights { target, rights }`: `target` is `{type:"no_passcode"}` or `{type:"passcode", passcode}`; the same rights are not an edit. Errors never repeat a passcode. A show file may hold other text (load keeps it with a warning).

**Patches and lists.**
- `AddPatch { index, patch }`: `index` 0 to the count; id unique; values valid (1 to 64 outputs, levels). `MovePatch`: `index` counts patches after taking this one out.
- `RemovePatch { id, even_if_used? }` is refused (the error names a cue) while an Audio cue routes to it. `even_if_used` (default false) exists so the **inverse of an add** can restore exactly: the patch may have been added to mend a cue that already pointed at it, and undo must not be blocked by that cue. A client may send it to remove a patch on purpose and break its cues.
- `SetPatchProperty`: `name`, `master_db`, per-output `output_level` / `output_mute` / `output_polarity`, `channel_count` (1 to 64; new outputs 0 dB, unmuted, normal polarity), `outputs` (replaces all). **Shrinking keeps cue routing to the dropped outputs** (silent, with a warning); its inverse is `outputs`, so undo brings the dropped outputs back with their settings.
- Network patches mirror this (`AddNetworkPatch`, `RemoveNetworkPatch { even_if_used? }`, `MoveNetworkPatch`, `SetNetworkPatchProperty`: `name`, `host`, `port`, `protocol`). Ports are 1 to 65535 for a new or changed value; load accepts port 0, so an undo, redo or rollback may put one back. Removal is refused while a Network cue uses it.
- Cue lists: `AddCueList`, `RemoveCueList`, `MoveCueList`, `RenameCueList`, `RenameWorkspace` (the show title, `name` of the `settings` upsert, not the file; Save As names that). A new list is empty. Removal is refused for a list that has cues (move or remove them first, in one `Transaction` if it should be one step) and for the only list. Renames are undoable and coalescing.
- **In use**: removal of a patch, network patch or cue list that something uses is refused, so a UI cannot orphan cues by accident. A cue already pointing at a missing patch is the usual broken-cue state.
- **Patch edits while audio plays** (insert, remove, move, resize, and their undo/redo) are allowed: mixer slots are keyed by patch id, so a playing voice keeps its routing (skill `engine-control`, patch slots).
- `Footprint` names the cue lists, patches and network patches an edit touched (`Touched`: changed, removed, reordered) and whether settings changed. `model_changes` turns them into `cue_list`, `patch`, `network_patch` and `settings` upserts/deletes. A move, or an edit touching two or more items of a collection, sends every member of the collection at its final index, in index order, which sorts the client's copy.
  An edit that is `is_model_wide()` makes the control loop call `CueEngine::model_changed` (patches decide which cues are broken, lists have playheads), not the cue-scoped `model_changed_cues`. A new command ships with its `Footprint`.

**New Group from selected cues** is no command of its own. The app sends one `Transaction`: `AddCue` (empty group, auto-numbered, where the first selected cue is) and a `ReparentCue` per selected cue into it at index 0, 1, ... in list order (a selected cue inside a selected group goes with its group). One undo step, all or none (a cue that would nest too deep refuses the whole thing).
A cue that is playing, paused or waiting is moved, never stopped or started, and the new group does not count it as its run: skill `cue-engine`, groups.

## Preparing a command
Before applying a client's command the engine calls `Command::prepare(&Workspace, &MediaPaths, new_id)`. Core stays pure: the engine passes the show file's folder (`None` for a never-saved show) and makes the ids. Inside a `Transaction` every command is prepared against the workspace as it is before the transaction.
- **Media paths** in `AddCue`, `Paste`, `PasteClipboard`, `SetCueProperty` and `BatchSet` (a `Kind` that is Audio, or `media`) are stored as `docs/file-format.md` section 4.3 says (rules and why: skill `persistence`). A client sends the absolute path it was given. An empty path is left alone.
- A never-saved show has no folder, so paths stay absolute. Save As rebasing, and when history is kept or cleared: skill `persistence`.

## Helpers
- `next_auto_number(&Workspace)`: one `auto_number_increment` above the highest numeric cue number, never below `auto_number_start`; plain decimal text (`0.3`, not `0.30000000000000004`).
- `prepare_paste(&Workspace, &[Cue], new_id)`: copies a clipboard for any workspace. Fresh ids from `new_id` (core cannot make ids); targets inside the copied set follow to the copies, others are kept if they exist in the destination and cleared otherwise; numbers already in use are cleared; a missing patch or network destination becomes the destination's first. Fails only if the clipboard is nested deeper than `MAX_GROUP_DEPTH`.
- Cut is copy + `RemoveCue`; duplicate is `prepare_paste` + `Paste`: wrap in a `Transaction` when they must be one step.

## Adding or changing a command
- Core `apply` plus its inverse; a test for no-op, refusal-leaves-workspace-unchanged, and undo/redo exactness.
- Limit checked on every creating or replacing path (add, paste, clipboard, kind change, batch).
- `Footprint` and `model_changes`; `Command::prepare` if it carries paths or ids.
- Contracts in the same commit (CLAUDE.md "Contracts"): protocol type, fixture in `engine/fixtures/protocol/`, Swift `Codable`, `docs/protocol.md`; stored fields also `docs/file-format.md`.

## References
- `references/limits.md` ("Size limits"): read when adding or changing a name, notes, host, message or table bound, or when a limit refusal or load warning is in question.
- `references/settings-hotkeys-volume.md`: read when touching `SetWorkspaceSetting`, the hotkey map refusal rules or volume limits (PB-11).
- `references/coalescing.md`: read when touching `execute_coalescing`, the merge in `history.rs`, or a client's `coalesce` key.
