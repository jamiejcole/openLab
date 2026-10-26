# openLab show file format

Status: format version **2** (openLab v0.0.1). Requirements: NFR-40, NFR-32, DOC-03.

An openLab workspace is one UTF-8 JSON text file with the extension **`.openlab`**. It is meant to be
readable, diffable and safe to keep under version control. The source of truth for the format is the
Rust model in `engine/crates/core` (`model.rs`, `cue.rs`, `show_file.rs`); this document describes
it, and `engine/fixtures/show/` holds a sample file for every format version.

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
- **Levels** are decibels (`f64`). `-120.0` or lower means silence (−∞); openLab writes `-120.0` for
  silence. The highest level is `+12.0`.
- **Indices** (cue outputs, file channels) are 0-based integers. A patch, cue or file has at most 64
  channels.
- **Mute** flags sit beside a level and keep it, so unmuting restores the level.

### What the reader accepts

- **Unknown keys are ignored**, so stray keys added by hand do not stop a file opening; they are
  dropped on the next save. (A file from a newer openLab is still refused by its `format_version`.)
- **Missing keys are an error**: every field below is required unless it says otherwise.
- A leading UTF-8 byte-order mark and `\r\n` line endings are accepted (files edited on Windows).
- Windows absolute media paths are converted to forward slashes (see §4.3).
- Files larger than 64 MiB, or nested deeper than the JSON parser's limit of 128 levels, are refused.
  A hostile file near the size limit made of tiny array elements can briefly need over 1 GB of
  memory while loading; realistic show files need a small fraction of their size.

## 2. Versioning and migration

The top level of every file is:

```json
{
  "format_version": 2,
  "workspace": { … }
}
```

- `format_version` is an integer, 0 or greater. It changes whenever the meaning or shape of the file
  changes in a way an older reader would get wrong. Every change bumps it by exactly one.
- **Newer than supported:** openLab refuses the file with a message naming both the file's version
  and the newest version it supports. It never guesses at a newer format.
- **Older than current:** openLab upgrades the document in memory one version at a time
  (`v0 → v1 → … → current`). Each step is a pure function from one JSON document to the next, with a
  fixture of the old version in `engine/fixtures/show/` and a test that it upgrades correctly. The
  file on disk is only rewritten (in the current version) when the user saves.
- **Policy for contributors:** a change to the model that affects the file bumps `FORMAT_VERSION`,
  adds a migration step and an old-version fixture, and updates this document, all in one commit.

| Version | openLab | Notes |
|---|---|---|
| 0 | pre-release | Development format. Identical to v1 except the workspace's cue lists were stored under `"lists"`. Exists so the migration chain is exercised from day one. |
| 1 | v0.0.1 (development) | Identical to v2 except that cues had no `second_trigger` and `second_trigger_on_release`. Upgrading gives every cue, at every depth, `"nothing"` and `false` (a v1 reader ignored those keys, so a hand-added value is replaced by the default). |
| 2 | v0.0.1 | Current. Adds the per-cue second-trigger settings (§5.1). |

## 3. Loading, validation and refusal

After parsing and migrating, openLab checks the workspace. Problems that make the show ambiguous
**refuse the file** with a reason; everything else **loads with warnings** that name the affected
cue, and the cue shows as broken where it cannot run.

| Problem | Result |
|---|---|
| Malformed JSON, wrong types, missing keys, unknown cue type | Refused (message includes line and column where possible) |
| Two cues, cue lists, patches or network patches with the same id | Refused |
| Cues nested more than 32 levels deep (a top-level cue is level 1) | Refused |
| A Start, or GoTo with `then_go`, that would start itself again: through a chain of such cues of any length, or by starting a group that contains it | Warning; the cue still runs (a timed loop is legitimate) and the cue engine bounds such cycles at run time (docs/cue-engine.md, "Chains") |
| Two cues with the same `number` | Warning; the later cue (list order, then depth-first through groups) loses its number |
| `second_trigger` is `"play_next"` or `"play_previous"` on a cue that is not a Playlist group | Warning; kept, but a second trigger does nothing until the cue is a Playlist group |
| An empty `number` (`""`) | Treated as `null`, no warning |
| A target that does not exist, targets the cue itself, or is the wrong kind of cue | Warning; the target is kept and the cue shows as broken |
| An Audio cue's `patch` or a Network cue's `destination` does not exist | Warning; the cue shows as broken |
| A time below its minimum or above its maximum (24 h unless stated) | Warning; clamped |
| A level above `+12` | Warning; set to `+12` |
| A level below `-120` | Set to `-120` silently (same meaning) |
| An OSC `float` argument too large for 32 bits | Warning; set to `0` |
| An Audio cue's `end_time` not after its `start_time` | Warning; `end_time` cleared (play to end) |
| Slice markers out of order | Warning; sorted |
| A slice marker at or before `start_time`, or at or after `end_time` | Warning; removed |
| Two slice markers at the same time | Warning; the second is removed |
| A patch with no outputs, or more than 64 | Warning; one output added, or the extras removed |
| A patch output delay outside 0–1000 ms | Warning; clamped |
| More Audio cue outputs than its patch has | Warning; kept, but the extra outputs are silent until the patch has them again |
| More than 64 cue outputs; a crosspoint or gang naming channel or output 64 or more; Fade levels naming output or channel 64 or more | Warning; those entries are removed |
| The same crosspoint (input and output) listed twice, or a cue output in more than one gang or twice in one | Warning; the first is kept, the repeats removed |
| A hotkey key name that is empty or has upper-case letters, spaces or control characters | Warning; the key will not fire |
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
| `cue_lists` | array of cue lists | §4.6. Written last so the short sections come first. |

### 4.2 `settings`

Settings that travel with the show (WS-06). Machine-specific settings, such as which audio device a
patch plays through, are stored on each machine, not in the show file (WS-07).

| Field | Type | Meaning |
|---|---|---|
| `min_go_interval` | seconds, 0–10 | GOs closer together than this are ignored (double-GO protection). `0` = off. |
| `panic_duration` | seconds, 0–60 | Fade-out time of Panic. |
| `autosave_interval` | seconds, 5–3600 | How often unsaved changes are written to the recovery file. |
| `osc_passcode` | string or `null` | Passcode OSC clients must send with `/connect`. `null` = no passcode. |
| `master_db` | level | Global master. |
| `hotkeys` | object | Transport keys: `go`, `stop_all` (Stop All / Panic), `pause_all`, `resume_all`, `playhead_next`, `playhead_previous`. Each is `null` (no key) or a hotkey object (§5.1). New workspaces get space, escape, `[`, `]`, down and up. |
| `auto_number_start` | number, 0–1 000 000 | Number given to the first auto-numbered cue. |
| `auto_number_increment` | number, above 0 up to 1 000 000 | Step between auto-numbered cues. |

### 4.3 Media paths

A media path uses **forward slashes** as separators. It is relative to the folder containing the
show file when the media is inside that folder (`"media/thunder.wav"`), otherwise absolute
(`"/Volumes/Show/thunder.wav"`, `"C:/Show/thunder.wav"`, `"//server/share/thunder.wav"`).
Windows absolute paths written with backslashes (`C:\Show\a.wav`, `\\server\share\a.wav`) are
converted to forward slashes on load. Any other backslash is kept as part of a file name, because
macOS allows it (`"sfx/Door\\Slam.wav"` in JSON is the file `Door\Slam.wav`).

openLab writes the paths it creates in Unicode NFC (macOS file systems report names decomposed, NFD)
and compares media paths ignoring NFC/NFD form and letter case, so a file written on macOS and opened
on Windows (or the reverse) still matches. A native Windows relative path (`media\a.wav`) is
converted to forward slashes when the path is created. Paths already in a file are read as written.
See `docs/persistence.md`.

### 4.4 Output patches

A patch is a named set of logical outputs that Audio cues play to. Which device channels those
outputs reach is configured per machine.

| Field | Type | Meaning |
|---|---|---|
| `id` | UUID | |
| `name` | string | |
| `master_db` | level | Patch master. |
| `outputs` | array of outputs | 1 to 64 entries; the patch's channel count is the length. Each is `{"level_db": level, "mute": bool, "delay_ms": 0–1000, "polarity_inverted": bool}`. |

### 4.5 Network patches

A destination for Network cues.

| Field | Type | Meaning |
|---|---|---|
| `id` | UUID | |
| `name` | string | |
| `host` | string | Host name or IP address. |
| `port` | integer 0–65535 | |
| `protocol` | `"udp"` \| `"tcp"` | TCP messages are SLIP-framed. |

### 4.6 Cue lists

| Field | Type | Meaning |
|---|---|---|
| `id` | UUID | |
| `name` | string | |
| `cues` | array of cues | Top-level cues in list order. Each list has its own playhead. |

## 5. Cues

### 5.1 Fields every cue has

| Field | Type | Meaning |
|---|---|---|
| `id` | UUID | Stable id. Targets, OSC and scripts refer to cues by id, never by number. |
| `number` | string or `null` | Free text, unique within the workspace when set. |
| `name` | string | |
| `notes` | string | Operator notes. |
| `color` | `"none"` \| `"red"` \| `"orange"` \| `"green"` \| `"blue"` \| `"purple"` | |
| `flagged` | bool | Marked for attention. |
| `armed` | bool | A disarmed cue is skipped when triggered but stays visible. |
| `pre_wait` | seconds | Delay between the cue being triggered and its action starting. |
| `post_wait` | seconds | Delay used by `auto_continue` before the next cue, counted from the start of the action. Ignored otherwise. |
| `continue_mode` | `"do_not_continue"` \| `"auto_continue"` \| `"auto_follow"` | `auto_continue`: start the next cue `post_wait` after this one's action starts (after its pre-wait). `auto_follow`: start the next cue when this one's action finishes. See [cue-engine.md](cue-engine.md). |
| `triggers` | object | `hotkey`: `null` or `{"key", "modifiers"}`; `osc`: `null` or an OSC address that starts the cue. |
| `second_trigger` | `"nothing"` \| `"panic"` \| `"stop"` \| `"hard_stop"` \| `"hard_stop_and_restart"` \| `"devamp"` \| `"play_next"` \| `"play_previous"` | What a trigger (GO, a Start cue, a continue, a hotkey, OSC, …) does while the cue is already active. `nothing`: ignored (a paused cue resumes). `panic`: fade out over `settings.panic_duration`, then stop. `stop`, `hard_stop`: stop at once. `hard_stop_and_restart`: stop at once and start again from the top. `devamp`: leave the loop after the current pass. `play_next`, `play_previous`: Playlist groups only, play the child after or before the one playing. See [cue-engine.md](cue-engine.md#second-trigger-m214). Since version 2. |
| `second_trigger_on_release` | bool | Releasing the hotkey or MIDI key that triggers the cue counts as a second trigger. Since version 2. |
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
| `end_time` | seconds or `null` | Trim out point; `null` = end of file. Must be after `start_time`. |
| `fade_in`, `fade_out` | fade spec | Fades built into the cue. |
| `slices` | array of `{"time", "play_count"}` | Slice markers in increasing time order, seconds from the start of the file, strictly between the trim points. Each marker ends a slice; that slice plays `play_count` times. `0` = loop until devamped. |
| `last_slice_play_count` | integer | Play count of the slice after the last marker (or of the whole file when there are none). `0` = loop until devamped. |
| `levels.master_db` | level | Cue master. |
| `levels.outputs` | array of `{"db": level, "mute": bool}` | One per cue output, at most 64. Cue output `i` feeds output `i` of the patch; outputs beyond the patch's are silent but kept. |
| `levels.crosspoints` | array of crosspoints | Routing matrix from file channels to cue outputs. Sparse: a crosspoint not listed is silent. Each input/output pair appears at most once. |
| `levels.gangs` | array of arrays of cue output indices | Outputs in the same set move together when edited. An output is in at most one gang. |
| `preload` | bool | Decode the whole file into memory when the cue loads. |

**`fade`** — moves a target's levels to new values.

| Field | Type | Meaning |
|---|---|---|
| `target` | target | An Audio or Group cue (fading a group fades its children). |
| `duration` | seconds | |
| `curve` | curve | |
| `levels.master_db` | level or `null` | `null` = leave the master alone. |
| `levels.output_db` | array of level or `null` | Indexed by cue output; `null` entries are left alone. |
| `levels.crosspoints` | array of `{"input", "output", "db"}` | Listed crosspoints fade to their `db`; others are left alone. Mutes are never changed by a fade. |
| `stop_target_when_done` | bool | Stop the target when the fade completes. |

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
| `devamp` | `target`, `start_next_cue` (bool), `stop_target_when_slice_ends` (bool) | Leaves the target's current loop at the end of the loop; optionally starts the next cue, or stops the target instead of continuing. Target must be an Audio cue. |
| `go_to` | `target`, `then_go` (bool) | Moves the playhead to the target; `then_go` also fires GO. |
| `arm` | `target` | Arms the target. |
| `disarm` | `target` | Disarms the target. |

**`wait`** — `duration` (seconds). Does nothing for that long.

**`memo`** — no fields. Does nothing; holds notes for the operator.

**`network`** — sends one OSC message.

| Field | Type | Meaning |
|---|---|---|
| `destination` | network patch id | |
| `address` | string | OSC address, e.g. `"/lx/go"`. (v0.0.1 sends OSC only; raw UDP/TCP strings come later.) |
| `args` | array | Typed arguments: `{"type": "int", "value": i32}`, `{"type": "float", "value": f32}`, `{"type": "string", "value": string}`, `{"type": "bool", "value": bool}`. |

## 6. Annotated example

An excerpt of `engine/fixtures/show/v2-example.openlab`, which contains every cue kind and group
mode. The notes after the listing refer to the numbered lines.

```json
{
  "format_version": 2,                                   // (1)
  "workspace": {
    "id": "0b5e0000-0000-4000-8000-000000000001",
    "name": "Example Show",
    "settings": {
      "min_go_interval": 0.5,                            // (2)
      "panic_duration": 3.0,
      "autosave_interval": 30.0,
      "osc_passcode": "1234",
      "master_db": -2.0,
      "hotkeys": {
        "go": { "key": "space", "modifiers": { "shift": false, "control": false, "alt": false, "command": false } },
        "stop_all": { … },
        "pause_all": null,                               // (2a)
        …
      },
      "auto_number_start": 1.0,
      "auto_number_increment": 0.5
    },
    "patches": [
      {
        "id": "00000000-0000-0000-0000-000000000001",
        "name": "Main PA",
        "master_db": 0.0,
        "outputs": [
          { "level_db": 0.0, "mute": false, "delay_ms": 0.0, "polarity_inverted": false },
          { "level_db": -1.5, "mute": false, "delay_ms": 2.5, "polarity_inverted": true }
        ]
      }
    ],
    "network_patches": [ … ],
    "cue_lists": [
      {
        "id": "00000000-0000-0000-0000-000000000001",
        "name": "Main",
        "cues": [
          {
            "id": "00000000-0000-0000-0000-000000000002",
            "number": "2",
            "name": "Preshow music",
            "notes": "Loops until the house is called.",
            "color": "green",
            "flagged": false,
            "armed": true,
            "pre_wait": 0.0,
            "post_wait": 0.0,
            "continue_mode": "auto_continue",            // (3)
            "triggers": {
              "hotkey": {
                "key": "p",
                "modifiers": { "shift": true, "control": false, "alt": false, "command": true }
              },
              "osc": "/preshow"
            },
            "second_trigger": "panic",                   // (3a)
            "second_trigger_on_release": false,
            "kind": {
              "type": "audio",                           // (4)
              "media": "media/preshow.wav",              // (5)
              "patch": "00000000-0000-0000-0000-000000000001",
              "start_time": 1.5,
              "end_time": 182.25,
              "fade_in": { "duration": 2.0, "curve": "s_curve" },
              "fade_out": { "duration": 4.0, "curve": "equal_power" },
              "slices": [
                { "time": 30.0, "play_count": 1 },
                { "time": 60.0, "play_count": 0 }        // (6)
              ],
              "last_slice_play_count": 1,
              "levels": {
                "master_db": -6.0,
                "outputs": [
                  { "db": 0.0, "mute": false },
                  { "db": -3.0, "mute": true }           // (7a)
                ],
                "crosspoints": [                         // (7)
                  { "input": 0, "output": 0, "db": 0.0, "mute": false },
                  { "input": 1, "output": 1, "db": 0.0, "mute": false }
                ],
                "gangs": [[0, 1]]
              },
              "preload": true
            }
          },
          {
            "id": "00000000-0000-0000-0000-000000000003",
            "number": "3",
            "name": "Fade preshow",
            …
            "kind": {
              "type": "fade",
              "target": "00000000-0000-0000-0000-000000000002",   // (8)
              "duration": 5.0,
              "curve": "logarithmic",
              "levels": {
                "master_db": -120.0,                     // (9)
                "output_db": [null, -10.0],              // (10)
                "crosspoints": [ { "input": 0, "output": 1, "db": -20.0 } ]
              },
              "stop_target_when_done": true
            }
          }
        ]
      }
    ]
  }
}
```

(The real file has no comments and no `…`; JSON does not allow them. Short objects are shown on one
line here for space; openLab writes one field per line.)

1. Read first. A file with a higher number is refused rather than misread.
2. Times are seconds: GOs less than half a second apart are ignored.
   (2a) No key is assigned to Pause All in this show.
3. When this cue starts, the next cue starts too (after this cue's post-wait of 0 s).
   (3a) Triggering the music again while it plays fades it out over the panic duration (3 s).
4. The `type` key selects the cue kind; the other keys in `kind` belong to that kind.
5. Relative to the show file's folder, forward slashes on every platform.
6. The slice from 30 s to 60 s loops until a Devamp cue releases it; then the rest of the file
   (60 s to the 182.25 s out point) plays once.
7. File channel 1 → cue output 1 and channel 2 → cue output 2 at 0 dB; no other routing.
   (7a) Cue output 2 is muted; unmuting brings it back at −3 dB.
8. Targets are cue ids, so renumbering cues never breaks them.
9. `-120` is silence: this fade takes the preshow music out.
10. Output 1 is left alone; output 2 fades to −10 dB.
