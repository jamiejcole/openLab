# Editing commands

Every change to a workspace's cue tree is a `Command` (`openlab-core`, `command.rs`). The UI sends them, OSC edits become them, and mirroring (RED-01) will replay them. Commands are serialisable (`serde`, internally tagged by `type`).

```rust
let inverse = workspace.apply(command)?;   // Result<Inverse, CommandError>
```

`Inverse` is a `Command`: applying it undoes the original, and applying *that* result redoes it. `History` keeps the undo and redo stacks of inverses and is the only thing that should change a workspace in the engine (`History::execute`, `undo`, `redo`). Undo is unlimited (ED-07). Call `History::clear` when the workspace is replaced (reload, revert).

## Guarantees

- **Atomic.** A refused command leaves the workspace exactly as it was.
- **Exact.** Undo then redo restores ids, order and parent/child structure.
- **Valid.** A command never produces something `repair` would have to fix, so `save_show` accepts the result and the saved file loads back identical (NFR-03):
  - cue ids unique across the workspace; numbers unique and not empty;
  - nesting at most `MAX_GROUP_DEPTH`; checked before recursing, so a hostile tree cannot overflow the stack;
  - every time and level finite and in range (the same checks as load).
- **References are not checked.** A missing target, a wrong-kind target, a self-target, a missing patch and trigger cycles are the *broken-cue state* of ARCHITECTURE §10: `validate` warns, the cue shows as broken. Removing a cue legitimately leaves them behind, and undo must be able to put back states that contain them.

## Commands

| Command | Notes |
|---|---|
| `AddCue { at, cue, auto_number }` | A Group brings its children. `auto_number` replaces the cue's number with `next_auto_number`. |
| `Paste { at, cues }` | One undo step. Ids must be fresh: build `cues` with `prepare_paste`. |
| `RemoveCue { id }` | Removes the subtree. Its inverse re-adds it at the same place. |
| `MoveCue { id, index }` | Reorder among current siblings. |
| `ReparentCue { id, to }` | Into another group or cue list, children with it. Not into itself or its descendants. |
| `SetCueProperty { id, property }` | `CueProperty`: name, notes, colour, flagged, armed, pre/post wait, continue mode, triggers, target, group mode, or the whole kind (never to or from Group). |
| `BatchSet { ids, property }` | One property on several cues, all or none. Not `Kind`. |
| `SetCueNumbers { numbers }` | Set, clear or swap numbers; uniqueness is judged on the result. |
| `Renumber { ids, start, increment }` | Numbers `ids` in cue-list order (a group before its children), whatever order `ids` is in. |
| `Transaction { commands }` | One undo step; if any command is refused, none applies and the error names its index. No nesting; the inverse is flat. |

`Position { list, parent, index }`: `parent` is `None` for the top level of the list. `index` counts the cues already there (after taking out the moved cue, when moving).

## Helpers

- `next_auto_number(&Workspace)`: one `auto_number_increment` above the highest numeric cue number in the workspace, never below `auto_number_start`; plain decimal text (`0.3`, not `0.30000000000000004`).
- `prepare_paste(&Workspace, &[Cue], new_id)`: copies a clipboard for pasting into any workspace. Fresh ids from `new_id` (core cannot make ids); targets inside the copied set follow to the copies, others are kept if they exist in the destination and cleared otherwise; numbers already in use are cleared; a missing patch or network destination becomes the destination's first.
- Cut is copy + `RemoveCue`; duplicate is `prepare_paste` + `Paste` — both in a `Transaction` when they must be one step.
