# Coalescing (fader drags)

`History::execute_coalescing(workspace, command, key)`. On the wire, `coalesce` on the `command` message (the key's 128-byte limit and form: `docs/protocol.md`).

## Contract
- An edit with a key joins the previous edit's undo step if that edit had the **same key** and nothing but such edits happened since: no other command (without a key, or with another), no undo or redo, no `History::clear`.
- A client sends a fresh key per gesture (e.g. `fader:<cue>:<n>`, `levels:<cue>:<n>` with a fresh `n` per mouse gesture) and none for a one-off edit.
- A no-op edit and a refused one leave the gesture open.
- **Core has no clock**: a gesture ends only by the events above. A client that sends the same key for ever merges for ever.
- Every edit is still applied and sent to clients at once; only the undo step is shared.
- Undoing a gesture restores the state before its first edit; redo restores the state after its last.

## Merge shape
- When the new step and the gesture's step each set the same single value (same cue property, setting, patch property, name, or the same item's position), the merged step is the older one alone, so a thousand-event drag is one small command.
- A step of the newer edit is left out only where that is provably safe:
  - both steps are one command setting the same value (or moving the same item); or
  - the newer step only sets values (cue properties, settings, patch and list names), no two of them the same value, with no add, remove or move and not both a `Kind` and a field inside that kind (`target`, `group_mode`, `media`, `destination`, `address`, `args`) of one cue, **and** the older step holds a command for that exact value. A multi-cue level drag stays one command per cue; a `Kind` is kept when the older chain sets one of those fields on that cue.
- Anything else is chained flat, newest undone first.

Why so careful: a wrong merge makes undo restore the wrong state; chaining is always correct, merging only saves space. When unsure, chain.
