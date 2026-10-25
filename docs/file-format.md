# openLab show file format

Status: format version **1** (openLab v0.0.1). Requirements: NFR-40, NFR-32, DOC-03.

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
- **Times** are seconds as JSON numbers (`f64`), never negative. Integers are accepted (`2` = `2.0`).
- **Levels** are decibels (`f64`). `-120.0` or lower means silence (−∞); openLab writes `-120.0` for
  silence. The highest level is `+12.0`.
- **Indices** (cue outputs, file channels) are 0-based integers.

### What the reader accepts

- **Unknown keys are ignored**, so a file written by a slightly newer openLab that only added keys
  still opens (the added data is dropped on the next save).
- **Missing keys are an error**: every field below is required unless it says otherwise.
- A leading UTF-8 byte-order mark and `\r\n` line endings are accepted (files edited on Windows).
- Backslashes in media paths are converted to forward slashes (see §4.3).
- Files larger than 64 MiB, or nested deeper than the JSON parser's limit of 128 levels, are refused.

## 2. Versioning and migration

The top level of every file is:

```json
{
  "format_version": 1,
  "workspace": { … }
}
```

- `format_version` is a positive integer. It changes whenever the meaning or shape of the file
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
| 1 | v0.0.1 | Current. |

## 3. Loading, validation and refusal

After parsing and migrating, openLab checks the workspace. Problems that make the show ambiguous
**refuse the file** with a reason; everything else **loads with warnings** that name the affected
cue, and the cue shows as broken where it cannot run.

| Problem | Result |
|---|---|
| Malformed JSON, wrong types, missing keys, unknown cue type | Refused (message includes line and column where possible) |
| Two cues, cue lists, patches or network patches with the same id | Refused |
| Cues nested more than 32 levels deep (a top-level cue is level 1) | Refused |
| Two cues with the same `number` | Warning; the later cue (list order, then depth-first through groups) loses its number |
| An empty `number` (`""`) | Treated as `null`, no warning |
| A target that does not exist, targets the cue itself, or is the wrong kind of cue | Warning; the target is kept and the cue shows as broken |
| An Audio cue's `patch` or a Network cue's `destination` does not exist | Warning; the cue shows as broken |
| A negative time | Warning; set to `0` |
| A level above `+12` | Warning; set to `+12` |
| A level below `-120` | Set to `-120` silently (same meaning) |
| An Audio cue's `end_time` not after its `start_time` | Warning; `end_time` cleared (play to end) |

Valid targets: a Fade cue targets an Audio or Group cue; a Devamp cue targets an Audio cue; every
other targeting cue may target any cue except itself.

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
| `min_go_interval` | seconds | GOs closer together than this are ignored (double-GO protection). `0` = off. |
| `panic_duration` | seconds | Fade-out time of Panic. |
| `autosave_interval` | seconds | How often unsaved changes are written to the recovery file. `0` = off. |
| `osc_passcode` | string or `null` | Passcode OSC clients must send with `/connect`. `null` = no passcode. |

### 4.3 Media paths

A media path is a string with **forward slashes only**. It is relative to the folder containing the
show file when the media is inside that folder (`"media/thunder.wav"`), otherwise absolute
(`"/Volumes/Show/thunder.wav"`, `"C:/Show/thunder.wav"`). Backslashes read from a file are
converted to forward slashes, so a file written by hand on Windows opens on macOS.

### 4.4 Output patches

A patch is a named set of logical outputs that Audio cues play to. Which device channels those
outputs reach is configured per machine.

| Field | Type | Meaning |
|---|---|---|
| `id` | UUID | |
| `name` | string | |
| `channel_count` | integer | Number of outputs in the patch. |

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
| `post_wait` | seconds | Delay used by `auto_continue`/`auto_follow` before the next cue. |
| `continue_mode` | `"do_not_continue"` \| `"auto_continue"` \| `"auto_follow"` | `auto_continue`: start the next cue `post_wait` after this one starts. `auto_follow`: start the next cue `post_wait` after this one's action finishes. |
| `triggers` | object | `hotkey`: `null` or `{"key", "modifiers"}`; `osc`: `null` or an OSC address that starts the cue. |
| `kind` | object | What the cue does: `{"type": …, …}`, §5.3. |

`triggers.hotkey.key` is a lower-case key name (`"a"`, `"space"`, `"f5"`).
`triggers.hotkey.modifiers` is `{"shift", "control", "alt", "command"}`, each a bool (`alt` is
Option on macOS).

### 5.2 Shared value objects

- **Fade spec** `{"duration": seconds, "curve": curve}`. A duration of `0` means no fade.
- **Curve**: `"linear"`, `"equal_power"`, `"s_curve"`, `"logarithmic"`, `"exponential"`.
- **Crosspoint** `{"input": file channel, "output": cue output, "db": level}`.
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
| `slices` | array of `{"time", "play_count"}` | Slice markers, seconds from the start of the file. Each marker ends a slice; that slice plays `play_count` times. `0` = loop until devamped. |
| `last_slice_play_count` | integer | Play count of the slice after the last marker (or of the whole file when there are none). `0` = loop until devamped. |
| `levels.master_db` | level | Cue master. |
| `levels.output_db` | array of levels | One per cue output. Cue output `i` feeds output `i` of the patch. |
| `levels.crosspoints` | array of crosspoints | Routing matrix from file channels to cue outputs. Sparse: a crosspoint not listed is silent. |
| `levels.gangs` | array of arrays of cue output indices | Outputs in the same set move together when edited. |
| `preload` | bool | Decode the whole file into memory when the cue loads. |

**`fade`** — moves a target's levels to new values.

| Field | Type | Meaning |
|---|---|---|
| `target` | target | An Audio or Group cue (fading a group fades its children). |
| `duration` | seconds | |
| `curve` | curve | |
| `levels.master_db` | level or `null` | `null` = leave the master alone. |
| `levels.output_db` | array of level or `null` | Indexed by cue output; `null` entries are left alone. |
| `levels.crosspoints` | array of crosspoints | Listed crosspoints fade to their `db`; others are left alone. |
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
| `address` | string | OSC address, e.g. `"/lx/go"`. |
| `args` | array | Typed arguments: `{"type": "int", "value": i32}`, `{"type": "float", "value": f32}`, `{"type": "string", "value": string}`, `{"type": "bool", "value": bool}`. |

## 6. Annotated example

An excerpt of `engine/fixtures/show/v1-example.openlab`, which contains every cue kind and group
mode. The notes after the listing refer to the numbered lines.

```json
{
  "format_version": 1,                                   // (1)
  "workspace": {
    "id": "0b5e0000-0000-4000-8000-000000000001",
    "name": "Example Show",
    "settings": {
      "min_go_interval": 0.5,                            // (2)
      "panic_duration": 3.0,
      "autosave_interval": 30.0,
      "osc_passcode": "1234"
    },
    "patches": [
      { "id": "00000000-0000-0000-0000-000000000001", "name": "Main PA", "channel_count": 2 }
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
                "output_db": [0.0, -3.0],
                "crosspoints": [                         // (7)
                  { "input": 0, "output": 0, "db": 0.0 },
                  { "input": 1, "output": 1, "db": 0.0 }
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
3. When this cue starts, the next cue starts too (after this cue's post-wait of 0 s).
4. The `type` key selects the cue kind; the other keys in `kind` belong to that kind.
5. Relative to the show file's folder, forward slashes on every platform.
6. The slice from 30 s to 60 s loops until a Devamp cue releases it; then the rest of the file
   (60 s to the 182.25 s out point) plays once.
7. File channel 1 → cue output 1 and channel 2 → cue output 2 at 0 dB; no other routing.
8. Targets are cue ids, so renumbering cues never breaks them.
9. `-120` is silence: this fade takes the preshow music out.
10. Output 1 is left alone; output 2 fades to −10 dB.
