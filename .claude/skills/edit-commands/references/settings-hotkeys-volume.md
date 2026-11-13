# `SetWorkspaceSetting`: hotkey map and volume limits

`WorkspaceSetting` variants (see `command_model.rs`): `min_go_interval`, `panic_duration`, `autosave_interval`, `output_max_db` (-30 to +12 dB), `output_min_db` (-180 to -40 dB; levels at or below it are silent), `hotkey { key, value }` (`key` is a `TransportKey`; `value` null = no key), `hotkeys { value }`, `auto_number_start`, `auto_number_increment`. Each carries the whole new value. A limit outside its range is refused.

## Hotkey map
- The map (`TransportHotkeys`) holds the transport keys, the selected-cue keys and the cue-editing keys (`edit_number` ... `toggle_flag`, TRG-07). The engine does nothing on an editing key except keep it from starting a cue; the app edits the selected cues. A file older than v9 gets each editing key unless a cue hotkey or another key of its map is that chord.
- `hotkeys { value }` replaces the whole key map (`settings.hotkeys`), for swapping two keys or a Reset, which one key at a time would refuse halfway. **Every key must be present** (`null` for none); an unknown field is refused as `bad_request`, so a client bug cannot silently clear keys it left out.
- A key the edit **changes to a value** is refused (`HotkeyRefused { field, reason }`, as `command_refused`) when it:
  - is reserved: Esc with any modifiers (always panic), Up or Down with any modifiers (`is_reserved_key`);
  - equals another key of the new map;
  - is the hotkey of any cue (the first in workspace order is named; the cue's id when it has no number).
- Keys the edit does not change are not judged, so a clash a hand-edited show was loaded with blocks nothing else. Clearing a key (`null`) is always allowed.
- The other direction stays allowed: a cue hotkey may be set to a key of the map, and then the workspace key wins and the cue warns.
- A key-map key equal to a menu command's key is accepted; the app binds it and warns that the menu command lost its key.
- QLab 5 refuses only Esc and the arrows. The duplicate and cue-clash rules are openLab's own, so the map never silently takes a key from a cue or another action.
- Only issues the edit itself introduces are refused: a warning the show was loaded with (a bad hotkey name) does not block other settings, and a new invalid hotkey name is refused (judged on the new value alone).
- **Undo and redo** (`Workspace::apply_restoring`, crate-internal: only `History` calls it, never with client input) put back a state that existed and are never refused for a value problem, so history cannot get stuck on a load warning. The same holds for cue properties (`triggers.hotkey`) and for a cue an undo adds back (the inverse of `RemoveCue`): its values are not judged, only its id, number, place and nesting.
- Cue-side hotkey semantics (what fires): skill `cue-engine`.

## Volume limits (PB-11)
- A level set by any command is put within the limits: above the maximum is set at it; at or below the minimum, or from a minimum below -120 dB up to -120 dB, is silence. A client may send a level and read back the limited one.
- Changing a limit while levels lie outside the new one is expanded by the engine (`Command::prepare`) into a **transaction** that first puts those Audio cue, Fade cue (not aimed at a Group) and patch levels within it: one undo step. A Fade with no target counts as not aimed at a Group, here and on add and paste, so it cannot end up louder.
- The same setting applied without that expansion is refused.
- Result: no fader, Fade or peer can play louder than the maximum, 10 ms after it is lowered (the mixer side: skill `audio-mixer`). The stored range is `docs/file-format.md` section 4.2.
