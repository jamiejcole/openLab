# openLab show file format

Status: format version **9** (openLab v0.0.1). Requirements: NFR-40, NFR-32, DOC-03.

An openLab workspace is one UTF-8 JSON text file with the extension **`.openlab`**, meant to be readable and diffable. The Rust model in `engine/crates/core` (`model.rs`, `cue.rs`, `show_file.rs`, `validate.rs`) is the source of truth; `engine/fixtures/show/v9-example.openlab` is the loadable current-version example (every cue kind and group mode).

## 1. Conventions

- **Layout.** openLab writes pretty-printed JSON with two-space indentation, `\n` line endings and a
  trailing newline. Fields are always written, always in the order listed below, so saving the same
  workspace twice produces identical bytes and diffs stay small.
- **Names.** All keys and string enum values are `snake_case`.
- **Enums.** An enum with no data is a plain string (`"color": "red"`). An enum whose variants carry
  data is an object with a `"type"` key naming the variant, plus that variant's fields
  (`{"type": "playlist", "loop": true, "shuffle": false}`).
- **Optional values** are written as `null` when absent.
- **Ids** are UUIDs in lowercase hyphenated form (`"7d1c…-…"`). openLab never derives behaviour from
  an id's value; it only compares them. Ids are stable for the life of the object.
- **Times** are seconds as JSON numbers (`f64`), from 0 to 24 hours (86 400 s) unless a field says
  otherwise. Integers are accepted (`2` = `2.0`).
- **Levels** are decibels (`f64`). `-120.0` or lower means silence; openLab writes `-120.0`. The highest level is `+12.0`.
- **Indices** (cue outputs, file channels) are 0-based integers. A patch or cue has at most 128 cue outputs and a file at most 64
  channels.
- **Mute** flags sit beside a level and keep it, so unmuting restores the level.

### What the reader accepts

- **Unknown keys are ignored** (dropped on the next save). A file with a higher `format_version` is still refused.
- **Missing keys are an error**: every field below is required unless it says otherwise. The
  exception is a key inside `settings.hotkeys`: a missing one reads as no key (`null`).
- A leading UTF-8 byte-order mark and `\r\n` line endings are accepted (files edited on Windows).
- Windows absolute media paths are converted to forward slashes (see §4.3).
- Files larger than `MAX_SHOW_FILE_BYTES` (64 MiB), or nested deeper than the JSON parser's limit of 128 levels, are refused. Loading never panics or hangs on any input.

## 2. Versioning and migration

The top level of every file is:

```json
{
  "format_version": 9,
  "workspace": { … }
}
```

- `format_version` is an integer, 0 or greater, and stays at its current value until v1.0.0 (CLAUDE.md "Contracts").
- An autosave (a file in the engine's recovery folder, never a show file) also has a top-level `show_path`, the absolute path of the show file it belongs to, after `format_version`. A reader of the workspace ignores it like any unknown key, so it needs no version of its own.
- **Newer than supported:** refused with a message naming both the file's version and the newest supported. openLab never guesses at a higher version.
- **Older than current:** upgraded in memory one version at a time through the existing chain (`vN_to_vN+1` in `show_file.rs`, each a pure function over the JSON document with a fixture of the old version in `engine/fixtures/show/`). The file on disk is rewritten, in the current version, only when the user saves. A new required key also goes into the last step so older test shows still load.
- Migration notices: some steps warn once instead of guessing (see §3, "Migration notices"). Two non-obvious rules: the removed global master is dropped, never folded into patch levels (QLab 5 has no global master), and a Fade aimed at a Group changed meaning (absolute to relative, v3) in a way no upgrade can preserve, so such files warn.

## 3. Loading, validation and refusal

After parsing and migrating, openLab checks the workspace. Problems that make the show ambiguous
**refuse the file** with a reason; everything else **loads with warnings** that name the affected
cue, and the cue shows as broken where it cannot run.

| Problem | Result |
|---|---|
| Malformed JSON, wrong types, missing keys, unknown cue type | Refused (message includes line and column where possible) |
| Two cues, cue lists, patches or network patches with the same id | Refused |
| Cues nested more than 32 levels deep (a top-level cue is level 1) | Refused |
| A Start, or GoTo with `then_go`, that would start itself again: through a chain of such cues of any length, or by starting a group that contains it | Warning; the cue still runs (a timed loop is legitimate) and the cue engine bounds such cycles at run time (skill `cue-engine`) |
| Two cues with the same `number` | Warning; the later cue (list order, then depth-first through groups) loses its number |
| `second_trigger` is `"play_next"` or `"play_previous"` on a cue that is not a Playlist group | Warning; kept, but a second trigger does nothing until the cue is a Playlist group |
| An empty `number` (`""`) | Treated as `null`, no warning |
| A target that does not exist, targets the cue itself, or is the wrong kind of cue | Warning; the target is kept and the cue shows as broken |
| An Audio cue's `patch` or a Network cue's `destination` does not exist | Warning; the cue shows as broken |
| A time below its minimum or above its maximum (24 h unless stated) | Warning; clamped |
| A level above `+12` | Warning; set to `+12` |
| A level below `-120` | Set to `-120` silently (same meaning) |
| A level above the workspace's `output_max_db` | Warning; set to it (a Fade cue aimed at a Group holds changes in dB and is not limited; every other Fade cue is) |
| A level between `-120` and the workspace's `output_min_db`, or at it | Set to `-120` (it was silent anyway); one warning for the whole file (`LevelsBelowMinimum`, no cue). Not for a Fade cue aimed at a Group |
| `output_max_db` or `output_min_db` outside its range | Warning; clamped into it |
| An OSC `float` argument too large for 32 bits | Warning; set to `0` |
| A Network cue's `address` is not a valid OSC address, or its arguments are too many or too large (skill `osc`) | Kept; the cue shows as broken until it is edited |
| An Audio cue's `end_time` not after its `start_time` | Warning; `end_time` cleared (play to end) |
| An Audio cue's `end_time` less than 50 ms after its `start_time` (`MIN_SLICE_SECONDS`) | Warning; `end_time` moved to `start_time` + 50 ms (QLab 5's shortest trim) |
| Slice markers out of order | Warning; sorted |
| A slice marker at or before `start_time`, or at or after `end_time` | Warning; removed |
| Two slice markers at the same time | Warning; the second is removed |
| A slice marker less than 50 ms (`MIN_SLICE_SECONDS`) after `start_time` or the marker before it, or before `end_time` | Warning; every slice, and so every loop, is at least 50 ms long. The marker moves to make the short slice exactly 50 ms (keeping its play count, so a stutter or vamp stays one); only when the neighbouring slice cannot give the room is the marker removed and the two slices become one, playing as the longer did. Without an `end_time` the last slice meets only the audio engine's loop floor (`MIN_LOOP_MS`) at voice planning |
| More than 254 slice markers (`MAX_SLICE_MARKERS`; the audio engine plays at most 256 slice entries, and a start offset inside a slice splits it) | Warning (`TooManySlices`); the markers after the 254th are removed. An edit, paste or duplicate that would exceed it is refused |
| A patch with no outputs, or more than 128 | Warning; one output added, or the extras removed |
| More Audio cue outputs than its patch has | Warning; kept, but the extra outputs are silent until the patch has them again |
| More than 128 cue outputs; a crosspoint naming channel 64 or more or output 128 or more; a gang naming output 128 or more; Fade levels naming output 128 or more or channel 64 or more | Warning; those entries are removed |
| The same crosspoint (input and output) listed twice, or a cue output in more than one gang or twice in one | Warning; the first is kept, the repeats removed |
| A hotkey key name that is empty or has upper-case letters, spaces or control characters | Warning; the key will not fire |
| Migration notices: a pre-v9 `master_db` other than 0 (`GlobalMasterRemoved`), a pre-v3 Fade aimed at a Group (`GroupFadeNowRelative`), a pre-v6 playhead key the user changed (`PlayheadKeysRemoved`), a pre-v7 output delay other than 0 (`OutputDelaysRemoved`) | Warning, once (the Fade one per cue); the migration removes the field or leaves the cue as it is |
| `show_mode` is not a boolean (and not `null`) | Warning (`ShowModeUnreadable`, once, no cue); the workspace opens in Edit Mode |
| A cue hotkey that is one of the workspace's keys (`settings.hotkeys`, transport or selected-cue, e.g. Space = GO) | Warning (`HotkeyUsedByWorkspace`); kept, but the workspace action wins and the key never starts the cue |
| OSC rights with Edit or Control but no View | Warning (`OscRightsNeedView`); Edit and Control are cleared |
| An empty, over-long (256 bytes), repeated (the first stays) or 17th and later OSC passcode row | Warning (`OscPasscodeDropped`); the row is removed. Warnings never contain the passcode |
| An OSC passcode that is not made of digits only | Warning (`OscPasscodeNotDigits`); kept, since dropping it would change who can connect (a v3 passcode may be any text) |
| A cue name over 1024 bytes, cue notes over 64 KiB, or a Network cue address over 64 KiB, more than 1024 arguments or a string argument over 64 KiB | Warning (`TooLarge`, per cue and field); kept as it is, so the show opens; commands refuse such a new value, and undo restores a loaded one |
| A workspace, cue list, patch or network patch name over 1024 characters or with a control character other than a newline, or a network host over 253 bytes or with one | Warning (`InvalidName`, one per kind of field); kept as it is, so the show opens; commands refuse such a new value, and undo restores a loaded one |
| Settings outside their ranges (§4.2), or an auto-number increment that is not greater than 0 | Warning; clamped (an invalid increment becomes 1) |

Valid targets: a Fade cue targets an Audio or Group cue; a Devamp cue targets an Audio cue or a
Playlist group; every other targeting cue may target any cue except itself.

## 4. The workspace

### 4.1 `workspace`

| Field | Type | Meaning |
|---|---|---|
| `id` | UUID | Workspace id. Names its recovery and backup files. |
| `name` | string | Display name. |
| `settings` | object | §4.2. |
| `patches` | array of output patches | §4.4. |
| `network_patches` | array of network patches | §4.5. |
| `stages` | array of stages, optional | §4.7. Absent when the workspace has none. |
| `show_mode` | boolean, optional | `true`: saved in Show Mode and opens in Show Mode. Absent or `false`: Edit Mode. Written only when `true`. It is the mode in force at the last Save, Save As or autosave; changing mode does not mark the workspace unsaved. A value that is neither boolean nor `null` is damage: opens in Edit Mode with a warning (§3), and the next save writes it right. |
| `cue_lists` | array of cue lists | §4.6. Written last so the short sections come first. |

### 4.2 `settings`

Settings that travel with the show (WS-06). Machine-specific settings, such as which audio device a
patch plays through, are stored on each machine, not in the show file (WS-07).

| Field | Type | Meaning |
|---|---|---|
| `min_go_interval` | seconds, 0–10 | GOs closer together than this are ignored (double-GO protection). `0` = off. |
| `panic_duration` | seconds, 0–60 | Fade-out time of Panic. |
| `autosave_interval` | seconds, 5–3600 | How often unsaved changes are written to the recovery file. |
| `osc_access` | object | The OSC passcode table (NET-02; semantics in skill `osc`): `no_passcode` (the rights of a peer that has not connected or connects without a passcode) and `passcodes` (rows). Rights are `{"view": bool, "edit": bool, "control": bool}`; a row is `{"passcode": string, "rights": rights}`. Edit and Control need View. A new workspace: `no_passcode` all false and one random 4-digit passcode with all rights. |
| `output_max_db` | number, -30 to +12 | The loudest level any fader may hold (default +12). A level above it is put at it, whatever sets it (command, paste, file). Lowering it is one undo step that puts every Audio cue level, patch main and output level, and Fade cue level above it at the limit; the cue engine does the same for a level a fade holds on a playing cue. A Fade aimed at a Group holds changes in dB, which are not limited. There is no global master. Edit semantics: skill `edit-commands`. |
| `output_min_db` | number, -180 to -40 | Levels at or below it are silent (`-120`, shown as -inf); default -60. A level at or below it is put at silence, whatever sets it (a file: one warning, `LevelsBelowMinimum`), so the model never holds a level beyond the limits. A minimum below `-120` makes every level from it up to `-120` silence. |
| `hotkeys` | object | The workspace key map (TRG-01, TRG-07). Transport keys `go`, `stop_all`, `pause_all`, `resume_all`; selected-cue keys `load_selected`, `pause_resume_selected`, `start_selected`, `stop_selected`, `reset_selected`, `preview_selected`, `panic_selected`; cue-editing keys `edit_number`, `edit_name`, `edit_notes`, `edit_target`, `edit_pre_wait`, `edit_duration`, `edit_post_wait`, `cycle_continue`, `toggle_flag` (they act on the cues selected in the app; the engine only keeps them from cue hotkeys). Each is `null` or a hotkey object (§5.1). New workspaces get Space, none, `[`, `]`, `l`, `p`, none, none, none, `v`, `s`, then `n`, `q`, `o`, `t`, `e`, `d`, `w`, `c`, `f`. A file older than v9 gets each key the v9 step added (`preview_selected` onward) unless a cue hotkey or another key of its map is that chord, which then keeps it and the action gets none. Panic All (Esc) is outside the map; there is no Next / Previous Cue key. **Reserved** (refused by the edit command): Esc with any modifiers, Up / Down with any modifiers. A cue hotkey equal to any key of the map never starts the cue (`HotkeyUsedByWorkspace`, §3). The loader keeps a reserved or repeated key a hand edit put here; the app binds no reserved key and only the first of a repeated one. |
| `auto_number_start` | number, 0–1 000 000 | Number given to the first auto-numbered cue. |
| `auto_number_increment` | number, above 0 up to 1 000 000 | Step between auto-numbered cues. |
| `new_cues_auto_load` | bool | Optional (absent: `false`). A cue made while it is on (`AddCue`, not a paste or an undo) gets `auto_load`, a group's children too; cues already there keep theirs. |

### 4.3 Media paths

A media path uses **forward slashes** as separators. It is relative to the folder containing the
show file when the media is inside that folder (`"media/thunder.wav"`), otherwise absolute
(`"/Volumes/Show/thunder.wav"`, `"C:/Show/thunder.wav"`, `"//server/share/thunder.wav"`). "Inside" is
literal: the same components byte for byte after NFC, no case folding, no `.` or `..`. A show never
saved holds absolute paths; its first Save As stores the ones literally inside the new folder
relative and leaves every other absolute path as it was. Cues pasted from a show in another folder
have their relative paths read against that folder first.
Windows absolute paths written with backslashes (`C:\Show\a.wav`, `\\server\share\a.wav`) are
converted to forward slashes on load. Any other backslash is kept as part of a file name, because
macOS allows it (`"sfx/Door\\Slam.wav"` in JSON is the file `Door\Slam.wav`).

openLab writes the paths it creates in Unicode NFC (macOS reports names decomposed). Storing a path relative is literal: a path whose folders differ from the show folder's only in case or Unicode form stays absolute. The cost is only an absolute path where a relative one would have worked, never a relative path naming another file. Matching two spellings of one file (Relink, telling whether two show paths are one file) ignores NFC/NFD, case and `.`/`..`. A native Windows relative path (`media\a.wav`) becomes forward slashes when created; paths already in a file are read as written. Algorithms: skill `media-files`, `persistence`.

### 4.4 Output patches

A patch is a named set of logical outputs that Audio cues play to. Which device channels those
outputs reach is configured per machine.

| Field | Type | Meaning |
|---|---|---|
| `id` | UUID | |
| `name` | string | |
| `master_db` | level | Patch master. |
| `outputs` | array of outputs | 1 to 128 entries (QLab 5's patch range); the patch's channel count is the length. Each is `{"level_db": level, "mute": bool, "polarity_inverted": bool}`. |

### 4.5 Network patches

A destination for Network cues. A new workspace starts with one, "Network 1" (`127.0.0.1`, port 53000, UDP); a file loads with the patches it has, none included.

| Field | Type | Meaning |
|---|---|---|
| `id` | UUID | |
| `name` | string | |
| `host` | string | Host name or IP address (IPv6 may be in brackets). Not checked when the file loads; a cue that sends to a patch whose host is empty or malformed, or whose port is 0, loads as it is and shows as broken. A host name is looked up when the workspace opens (not stored). |
| `port` | integer 0–65535 | 1–65535 can be sent to. |
| `protocol` | `"udp"` \| `"tcp"` | TCP messages are SLIP-framed (OSC 1.1), one connection per message. |

### 4.6 Cue lists

| Field | Type | Meaning |
|---|---|---|
| `id` | UUID | |
| `name` | string | |
| `cues` | array of cues | Top-level cues in list order. Each list has its own playhead. |

### 4.7 Stages

A stage is a named raster Video cues are drawn on (VID-01; QLab 5 manual pp. 207, 225-227). Which display shows it is chosen per machine (`settings.json`, under the workspace id and stage id: the display's id, name and pixel size when it was chosen), not stored in the show file, so a show moves between machines as an audio patch does. A stage with no display on this machine still loads; it carries a warning.

| Field | Type | Meaning |
|---|---|---|
| `id` | UUID | Two stages with one id refuse the file. |
| `name` | string | |
| `width`, `height` | integer 1–16384 | Pixel size. A size outside the range is clamped with a warning (`StageSize`). |
| `keep_black` | boolean, optional | `true`: the stage's display shows black while no cue plays on it (QLab's "keep rendering between cues", p. 227). `false` (the default when absent, as in QLab): its window is hidden then and the desktop shows, so a stage on the operator's own display never covers it between cues. |
| `layer` | integer 0–1000, optional | Stacking order among stages on the same display (QLab 5 manual p. 227): a higher layer is drawn on top; on the same layer the stage whose name sorts first is on top. Default 1. A value above 1000 is clamped with a warning (`StageLayer`). |

At most 64 stages; a file with more keeps the first 64 with a warning (`TooManyStages`).

## 5. Cues

### 5.1 Fields every cue has

| Field | Type | Meaning |
|---|---|---|
| `id` | UUID | Stable id. Targets, OSC and scripts refer to cues by id, never by number. |
| `number` | string or `null` | Free text, unique within the workspace when set. |
| `name` | string | The name the user typed. Empty means "named after the file": the UI shows `display_name` (the Audio cue's file name with its extension).  Assigning or following a file never writes `name`. |
| `notes` | string | Operator notes. |
| `color` | colour | The row's highlight. A colour is `"none"`, one of the eight basic `"red"`, `"orange"`, `"yellow"`, `"green"`, `"cyan"`, `"blue"`, `"purple"`, `"magenta"`, or one of the twelve more `"dark_red"`, `"apricot"`, `"olive"`, `"dark_green"`, `"light_blue"`, `"navy"`, `"indigo"`, `"lilac"`, `"plum"`, `"raspberry"`, `"pink"`, `"grey"`. |
| `second_color` | colour | Optional (absent: `"none"`). The colour the row shows once the cue has been started, until Reset All, a reset of the cue (or of a group around it) or the show is opened again; `"none"` = no second colour. Whether it has been started is not saved. |
| `flagged` | bool | Marked for attention. |
| `armed` | bool | A disarmed cue is skipped when triggered but stays visible. |
| `auto_load` | bool | Optional (absent: `false`). The cue is loaded when the cue before it at its level (passing over skipped cues) starts. Semantics: skill `cue-engine`. |
| `skip_if_disarmed` | bool | Optional (absent: `false`). While the cue is disarmed it is passed over as if it were not in the list: GO, continues and groups go to the cue after it and its own continue never fires. No effect on an armed cue. |
| `pre_wait` | seconds | Delay between the cue being triggered and its action starting. |
| `post_wait` | seconds | Delay used by `auto_continue` before the next cue, counted from the start of the action. Ignored otherwise. |
| `continue_mode` | `"do_not_continue"` \| `"auto_continue"` \| `"auto_follow"` | `auto_continue`: start the next cue `post_wait` after this one's action starts (after its pre-wait). `auto_follow`: start the next cue when this one's action finishes. Semantics: skill `cue-engine`. |
| `triggers` | object | `hotkey`: `null` or `{"key", "modifiers"}`; `hotkey_enabled` (bool, optional, absent: `true`): whether the hotkey starts the cue; off keeps the key. A cue has no OSC address of its own (OSC starts it by number or id); an `osc` key from an older file is ignored. |
| `second_trigger` | `"nothing"` \| `"panic"` \| `"stop"` \| `"hard_stop"` \| `"hard_stop_and_restart"` \| `"devamp"` \| `"play_next"` \| `"play_previous"` | What a trigger does while the cue is already active; semantics in skill `cue-engine`. `play_next`/`play_previous` are for Playlist groups only. |
| `second_trigger_on_release` | bool | Releasing the hotkey or MIDI key that triggers the cue counts as a second trigger. |
| `fade_and_stop_others` | object | Optional (absent: off, `peers`, 1 s). `{"enabled": bool, "scope": "peers" \| "list" \| "all", "duration": seconds}`: when the cue's action starts, the other active cues of its level (each with what is inside it), of its list, or of the workspace fade out over `duration` and stop. Kept while off. A bad `duration` is put in range with `InvalidTime`. Semantics: skill `cue-engine`. |
| `kind` | object | What the cue does: `{"type": …, …}`, §5.3. |

`triggers.hotkey.key` is a lower-case key name (`"a"`, `"space"`, `"f5"`).
`triggers.hotkey.modifiers` is `{"shift", "control", "alt", "command"}`, each a bool (`alt` is
Option on macOS).

### 5.2 Shared value objects

- **Fade spec** `{"duration": seconds, "curve": curve}`. A duration of `0` means no fade.
- **Curve**: `"linear"`, `"equal_power"`, `"s_curve"`, `"logarithmic"`, `"exponential"`.
- **Crosspoint** `{"input": file channel, "output": cue output, "db": level, "mute": bool}`. In a
  Fade cue's levels a crosspoint has no `mute`.
- A **target** is a cue id or `null` (not yet chosen; the cue shows as broken until set).

### 5.3 Cue kinds

**`audio`** — plays a media file. Its duration is not stored; it comes from the file.

| Field | Type | Meaning |
|---|---|---|
| `media` | path or `null` | §4.3. `null` until a file is assigned. |
| `patch` | patch id | Output patch the cue plays to. |
| `start_time` | seconds | Trim in point, from the start of the file. |
| `end_time` | seconds or `null` | Trim out point; `null` = end of file. Must be at least 50 ms after `start_time`. |
| `fade_in`, `fade_out` | fade spec | Fades built into the cue. |
| `slices` | array of `{"time", "play_count"}` | Slice markers in increasing time order, seconds from the start of the file, strictly between the trim points, each slice at least 50 ms long. Each marker ends a slice; that slice plays `play_count` times. `0` = loop until devamped. |
| `last_slice_play_count` | integer | Play count of the slice after the last marker (or of the whole file when there are none). `0` = loop until devamped. |
| `levels.master_db` | level | Cue master. |
| `levels.outputs` | array of `{"db": level, "mute": bool}` | One per cue output, at most 128. Cue output `i` feeds output `i` of the patch; outputs beyond the patch's are silent but kept. |
| `levels.crosspoints` | array of crosspoints | Routing matrix from file channels to cue outputs. Sparse: a crosspoint not listed is silent. Each input/output pair appears at most once. |
| `levels.gangs` | array of arrays of cue output indices | Outputs in the same set move together when edited. An output is in at most one gang. |
| `preload` | bool | Decode the whole file into memory when the cue loads. |

**`fade`** — moves a target's levels: an Audio cue's **to** its values (absolute), each audio cue in a Group **by** them (relative, capped at +12 dB).

| Field | Type | Meaning |
|---|---|---|
| `target` | target | An Audio or Group cue (fading a group moves each audio cue in it, at any depth, relative to its own level). |
| `duration` | seconds | |
| `curve` | curve | |
| `levels.master_db` | level or `null` | `null` = leave the master alone. |
| `levels.output_db` | array of level or `null` | Indexed by cue output; `null` entries are left alone. |
| `levels.crosspoints` | array of `{"input", "output", "db"}` | Listed crosspoints fade to their `db` (by it, for a Group target; a silent crosspoint stays silent); others are left alone. Mutes are never changed by a fade. |
| `stop_target_when_done` | bool | Stop the target when the fade completes. |
| `mode` | `"absolute"` \| `"relative"` | For an Audio target: `absolute` ends the target's levels at the Fade's; `relative` moves each by the Fade's dB from where it is when the Fade starts (a silent level stays silent), within the workspace limits. A Group target always moves by them. Absent: `absolute`. |

**`group`** — contains cues.

| Field | Type | Meaning |
|---|---|---|
| `mode` | object | `{"type": "timeline"}`: start all children together (each after its own pre-wait). `{"type": "start_first_and_enter"}`: start the first child and move the playhead into the group. `{"type": "start_first_and_go_to_next"}`: start the first child and move the playhead past the group. `{"type": "playlist", "loop": bool, "shuffle": bool}`: play children one after another. `{"type": "start_random"}`: start one child at random. |
| `children` | array of cues | Nested cues, at most 32 levels deep in total. |

**Control cues** — act on another cue.

| Type | Extra fields | Action |
|---|---|---|
| `start` | `target` | Starts the target. |
| `stop` | `target`, `fade_out` (fade spec) | Stops the target, fading it out first when `fade_out.duration` > 0. |
| `pause` | `target` | Pauses the target. |
| `load` | `target`, `load_to_time` (seconds) | Loads the target, cued to `load_to_time`. |
| `reset` | `target` | Returns the target to its saved state. |
| `devamp` | `target`, `start_next_cue` (bool), `stop_target_when_slice_ends` (bool), `devamp_mode` (`"current_slice"` \| `"looping_cue"`) | Leaves the target's loop at the end of the pass; semantics in skill `cue-engine`. Target must be an Audio cue or a Playlist group. |
| `go_to` | `target`, `then_go` (bool) | Moves the playhead to the target; `then_go` also fires GO. |
| `arm` | `target` | Arms the target. |
| `disarm` | `target` | Disarms the target. |

**`wait`** — `duration` (seconds). Does nothing for that long.

**`memo`** — no fields. Does nothing; holds notes for the operator.

**`network`** — sends one OSC message.

| Field | Type | Meaning |
|---|---|---|
| `destination` | network patch id | |
| `address` | string | OSC address, e.g. `"/lx/go"`: starts with `/`, printable ASCII, no spaces, at most 512 bytes. |
| `args` | array | Typed arguments: `{"type": "int", "value": i32}`, `{"type": "float", "value": f32}`, `{"type": "string", "value": string}`, `{"type": "bool", "value": bool}`. |
