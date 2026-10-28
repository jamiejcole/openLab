# Editing commands

Every change to a workspace is a `Command` (`openlab-core`, `command.rs`, and `command_model.rs` for everything but the cue tree): cues, workspace settings, output and network patches, cue lists (ED-07). The UI sends them, OSC edits become them, and mirroring (RED-01) will replay them. Commands are serialisable (`serde`, internally tagged by `type`) and travel inside the UI protocol's `command` message ([protocol.md](protocol.md)).

```rust
let inverse = workspace.apply(command)?;   // Result<Inverse, CommandError>
```

`Inverse` is a `Command`: applying it undoes the original, and applying *that* result redoes it. `History` keeps the undo and redo stacks of inverses and is the only thing that should change a workspace in the engine (`History::execute`, `execute_coalescing`, `undo`, `redo`). Undo is unlimited (ED-07). Call `History::clear` when the workspace is replaced (reload, revert).

`History::execute` returns `Ok(true)` if the command changed the workspace and was recorded, `Ok(false)` if it changed nothing.

## Guarantees

- **Atomic.** A refused command leaves the workspace exactly as it was (the one exception, `RollbackFailed`, means an inverse is wrong: see `Transaction`).
- **Exact.** Undo then redo restores ids, order and parent/child structure.
- **Valid.** A command never produces something `repair` would have to fix, so `save_show` accepts the result and the saved file loads back identical (NFR-03):
  - cue ids unique across the workspace; numbers unique and not empty;
  - nesting at most `MAX_GROUP_DEPTH`; checked before recursing, so a hostile tree cannot overflow the stack;
  - every time and level finite and in range (the same checks as load).
- **References between cues are not checked.** A missing target, a wrong-kind target, a self-target, a missing patch and trigger cycles are the *broken-cue state* of ARCHITECTURE §10: `validate` warns, the cue shows as broken. Removing a cue legitimately leaves them behind, and undo must be able to put back states that contain them. The exception is removing a patch or cue list that something still uses: see "Settings, patches and cue lists".
- **A change to what is already there is not an edit.** `SetCueProperty` to the value the cue has, a `MoveCue` in place, `SetCueNumbers` that changes no number, a setting or patch property set to its current value and the like return an empty `Transaction` as their inverse, which `History` does not record: no undo step, no redo cleared, and the engine sends no `model_changed`, makes no revision and leaves the document clean. (Values are validated first: an invalid value is refused even if the field "already has" something equal-looking.)

## Network cue properties (M4.6a)

`destination`, `address` and `args` of `CueProperty` apply to Network cues only (`PropertyNotApplicable` on any other kind); they undo, redo, coalesce (one gesture key, one slot per property) and batch like the other properties. A new value is refused, changing nothing, unless:

- `destination` names an existing network patch (`NetworkPatchNotFound`). Removing a patch a cue uses stays a broken-cue state; undo, redo and rollback skip this check so they can restore one.
- `address` and `args` each pass `network::check_message` on their own (the other half stands in as empty / `/`), and the pair must not newly exceed the size limit, so a cue loaded with a bad half can be mended one half at a time; the checks are: the address starts with `/`, is at most 512 bytes of printable ASCII without spaces (pattern characters `* ? [ ] { }` are allowed), at most 32 arguments, strings at most 4096 bytes with no NUL, floats finite, the encoded message at most 8192 bytes (`InvalidMessage(reason)`). Ints and bools cannot be invalid.

QLab 5's Network cue page (qlab.app/docs/v5/networking/network-cues/) describes the address and arguments as typed text and does not say what happens to a malformed one; we refuse it at the edit (open-qlab-questions.md), so a client cannot put a cue into a state the cue engine would break on.

## Commands

| Command | Notes |
|---|---|
| `AddCue { at, cue, auto_number }` | A Group brings its children. `auto_number` replaces the cue's number with `next_auto_number`. |
| `Paste { at, cues }` | One undo step. Ids must be fresh: build `cues` with `prepare_paste`. |
| `RemoveCue { id }` | Removes the subtree. Its inverse re-adds it at the same place. |
| `MoveCue { id, index }` | Reorder among current siblings. |
| `ReparentCue { id, to }` | Into another group or cue list, children with it. Not into itself or its descendants. |
| `SetCueProperty { id, property }` | `CueProperty`: name, notes, colour, flagged, armed, pre/post wait, continue mode, triggers, second trigger, second trigger on release, target, group mode, or the whole kind (never to or from Group); for Network cues only (M4.6a, CT-30) `destination`, `address` and `args`, each `value` the whole new setting. |
| `BatchSet { ids, property }` | One property on several cues, all or none. Not `Kind`. Applied in place with each cue's old value kept; a refusal writes the old values back, so the cost follows the cues named, not the show. |
| `SetCueNumbers { numbers }` | Set, clear or swap numbers; uniqueness is judged on the result. |
| `Renumber { ids, start, increment }` | Numbers `ids` in cue-list order (a group before its children), whatever order `ids` is in. |
| `Transaction { commands }` | One undo step; if any command is refused, none applies and the error names its index. No nesting; the inverse is flat. Commands that changed nothing add nothing to the inverse. A refusal is rolled back by applying the inverses of the commands already applied, newest first, as an undo would (restoring); the workspace is not copied, so the cost follows the commands, not the show (debug and test builds also copy it and check the rollback against the copy). A rollback step can only be refused if an inverse is wrong; then the error is `RollbackFailed`, the workspace is valid but partly changed, and `History` drops its undo and redo steps, which no longer fit it. |
| `SetWorkspaceSetting { setting }` | `WorkspaceSetting`: `min_go_interval`, `panic_duration`, `autosave_interval`, `master_db`, `hotkey { key, value }` (`key`: `go`, `stop_all`, `pause_all`, `resume_all`, `playhead_next`, `playhead_previous`; `value` null = no key), `auto_number_start`, `auto_number_increment`. Each is the whole new value. Only issues the edit itself introduces are refused: a warning the show was loaded with (a bad hotkey name) does not block other settings, and any new invalid hotkey name is refused (judged on the new value alone). An undo or redo (`Workspace::apply_restoring`, crate-internal: only `History` calls it, never with client input) puts back a state that existed and is never refused for a value problem, so it cannot get stuck on a load warning; the same holds for cue properties (`triggers.hotkey`) and for a cue an undo adds back (the inverse of `RemoveCue`): its values are not judged, only its id, number, place and nesting. |
| `AddOscPasscode { index, passcode, rights }` | Adds a row to the OSC passcode table (`settings.osc_access`) at `index` (0 to the row count). `passcode`: 1 to 32 ASCII digits, not already in the table; `rights`: `{view, edit, control}`, Edit and Control need View. At most 16 rows. Undo removes it. |
| `RemoveOscPasscode { passcode }` | Removes the row. Any row may go, the last too (nobody can then connect with a passcode; undo brings it back at its place, with its rights, even if it is a legacy non-digit passcode). |
| `SetOscRights { target, rights }` | `target`: `{type:"no_passcode"}` or `{type:"passcode", passcode}`. Replaces that row's rights; the same rights are not an edit. Errors never repeat a passcode. |
| `AddPatch { index, patch }` | Output patch at `index` (0 to the current count). Id unique, values valid (1 to 64 outputs, levels, delays). |
| `RemovePatch { id, even_if_used? }` | Refused (the error names a cue) while an Audio cue routes to it. `even_if_used` (default false) is only for the inverse of an add; see below. |
| `MovePatch { id, index }` | `index` counts the patches after taking this one out. |
| `SetPatchProperty { id, property }` | `PatchProperty`: `name`, `master_db`, `output_level`, `output_mute`, `output_delay`, `output_polarity` (each `{ output, value }`), `channel_count { value }` (1 to 64; new outputs 0 dB, unmuted, no delay), `outputs { value }` (replaces all). Shrinking keeps cue routing to the dropped outputs (silent, a warning), and its inverse is `outputs`, so undo brings the dropped outputs back with their settings. |
| `AddNetworkPatch { index, patch }`, `RemoveNetworkPatch { id, even_if_used? }`, `MoveNetworkPatch { id, index }`, `SetNetworkPatchProperty { id, property }` | As for patches. `property`: `name`, `host`, `port`, `protocol`. Ports are 1 to 65535 for a new or changed value; load accepts port 0, so an undo, redo or rollback may put a patch on port 0 back. Removal is refused while a Network cue uses the patch. |
| `AddCueList { index, id, name }`, `RemoveCueList { id }`, `MoveCueList { id, index }`, `RenameCueList { id, name }`, `RenameWorkspace { name }` | A new list is empty. `RenameWorkspace` sets the show title (the `name` of the `settings` upsert), not the file: Save As names that. Undoable and coalescing, like a list rename. Removing is refused for a list that has cues (move or remove them first, in one `Transaction` if it should be one step) and for the only list. |

`Position { list, parent, index }`: `parent` is `None` for the top level of the list. `index` counts the cues already there (after taking out the moved cue, when moving).

## Settings, patches and cue lists

- **Names.** A new name of the workspace, a cue list, a patch or a network patch (the add commands and the renames) is at most 256 Unicode scalar values (`MAX_NAME_SCALARS`; characters, not bytes, so a name in any script gets the same room) with no control character (NUL, newline, tab, escape: a name is one line). Empty is allowed. A network patch `host` is at most 253 bytes (the DNS limit) with no control character; whether it can be addressed is not checked here (a bad host makes a Network cue broken). A name or host that breaks the rule is refused with `command_refused` (`InvalidValue`). The rule is for the value being set only: load keeps an odd name or host with one `InvalidName` warning per field kind (`name`, `cue_lists.name`, `patches.name`, `network_patches.name`, `network_patches.host`), and undo, redo and a transaction's rollback put such a value back. QLab 5's documentation sets no limit; the numbers are an assumption (`docs/todo/open-qlab-questions.md`, row 13). Cue names are not covered.
- **Valid.** A new or changed value must pass what load checks (`validate`/`repair`), including values `repair` would silently normalise (a level below silence): it is refused with the issues listed. An OSC passcode added by a command is 1 to 32 digits (NET-02); a show file may hold other text (a migrated v3 passcode), which load keeps with a warning.
- **In use.** Removing a patch, network patch or cue list that something uses is refused, so a UI cannot orphan cues by accident. A cue that already points at a patch the workspace lacks is the usual broken-cue state. Two things follow: `RemovePatch` / `RemoveNetworkPatch` take `even_if_used`, which `AddPatch` / `AddNetworkPatch` put on their inverse (the patch may have been added to mend a cue that already pointed at it, and undo must restore exactly, not be blocked by that cue); and a client may send it too, to remove a patch on purpose and break its cues.
- **Playing.** Patch edits of any kind (insert, remove, move, resize) and their undo and redo are allowed while audio plays: mixer slots are keyed by patch id, so a playing voice keeps its routing (docs/control.md, "Patch slots", M4.15b).
- **Footprints.** `Footprint` also names the cue lists, patches and network patches an edit touched (`Touched`: changed, removed, reordered) and whether settings changed; `model_changes` turns them into `cue_list`, `patch`, `network_patch` and `settings` upserts and deletes. A move, or an edit touching two or more items of one collection (a merged undo step), sends every member of the collection, each at its final index, in index order, which sorts the client's copy whatever the old order. An edit that is `is_model_wide()` makes the control loop call `CueEngine::model_changed` (patches decide which cues are broken, lists have playheads) instead of the cue-scoped `model_changed_cues`.

## Coalescing (fader drags)

`History::execute_coalescing(workspace, command, key)`; on the wire, `coalesce` on the `command` message ([protocol.md](protocol.md)). An edit with a key joins the previous edit's undo step if that edit had the same key and nothing but such edits happened since: no other command without or with another key, no undo or redo, no `History::clear`. A client sends a fresh key for each gesture (for example `fader:<cue>:<n>`) and none for a one-off edit. A no-op edit and a refused one leave the gesture open.

Every edit is still applied and sent to clients at once; only the undo step is shared. Undoing a gesture restores the state before its first edit, redo the state after its last. When the new step and the gesture's step each set the same single value (the same cue property, setting, patch property, name, or the same item's position), the merged step is the older one alone, so a thousand-event drag is one small command. A step of the newer edit is left out only where that is provably safe: both steps are one command setting the same value (or moving the same item), or the newer step only sets values (cue properties, settings, patch and list names), no two of them the same value, with no add, remove or move and not both a `Kind` and a `Target` / `group_mode` of one cue, and the older step holds a command for that exact value (a multi-cue level drag stays one command per cue; a `Kind` is kept when the older chain sets that cue's target or group mode). Anything else is chained flat, newest undone first. Core has no clock, so a gesture ends only by the events above; a client that sends the same key forever merges forever.

## Helpers

- `next_auto_number(&Workspace)`: one `auto_number_increment` above the highest numeric cue number in the workspace, never below `auto_number_start`; plain decimal text (`0.3`, not `0.30000000000000004`).
- `prepare_paste(&Workspace, &[Cue], new_id)`: copies a clipboard for pasting into any workspace. Fresh ids from `new_id` (core cannot make ids); targets inside the copied set follow to the copies, others are kept if they exist in the destination and cleared otherwise; numbers already in use are cleared; a missing patch or network destination becomes the destination's first.
- Cut is copy + `RemoveCue`; duplicate is `prepare_paste` + `Paste` — both in a `Transaction` when they must be one step.
