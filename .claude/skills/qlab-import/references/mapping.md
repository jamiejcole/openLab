# QLab 5 field to openLab field

Every QLab class and field the reader uses, what it becomes, and the fixture that shows it (`engine/fixtures/qlab/<fixture>`, local only; QLab 5.6.5). "Dropped" means listed in the report's notes, by kind. "Assumed" means no fixture settles the value: the reason is given.

Values: levels are linear gains (1.0 is 0 dB), converted to dB to a thousandth (they are 32-bit in the file); times are rounded to the microsecond. A level QLab does not store is −∞ (silence). Booleans may be stored as booleans or as integers.

## Workspace (root dictionary)
| QLab | openLab | Fixture |
|---|---|---|
| `workspaceName` (`name.qlab5`) | name, extension dropped; report `workspace_name` | all |
| `QLabShortVersionString` | report `qlab_version`; major below 5 refused (`qlab_too_old`); missing with `cueLists` present: too old | all |
| `QLabRequireMinimumVersion` (`QLabVersion`: `versionString`, `buildNumber`) | major above 5 refused (`qlab_too_new`) | all |
| `cueLists` (data: a second keyed archive) | root `GroupCue` mode 3; its `cues` are the lists | all |
| `uniqueID`, `controller`, `selectedCueListID`, `totalCues`, `highlightRelatedCues` | not read | |

## Settings (`settings`, a dictionary of sections)
| QLab | openLab | Fixture |
|---|---|---|
| General `panicDuration` (0-60), `minTimeBetweenGOs` (0-10), `autoNumberIncrement` (> 0), `autoNumber`, `goRequiresKeyUp`, `playbackPositionLockedToSelection`, `autoLoad`, `doAutoCopy` | `panic_duration`, `min_go_interval`, `auto_number_increment`, `auto_number_new_cues`, `go_needs_key_up`, `lock_playhead`, `new_cues_auto_load`, `copy_media_on_add` | groups-triggers |
| General: backups, autosave, start/close cue numbers, row and button sizes | not read | |
| Audio `maxVolume`, `minVolume` (dB) | `output_max_db`, `output_min_db`, clamped to their ranges | all |
| Key Map: a dictionary of action to key (`keyCharacter`, `keyCode`, `shift`, `control`, `option`, `command`, `function`, `numericPad`, `capsLock`) | `settings.hotkeys`; a QLab map with no entry for an action gives it no key | groups-triggers (`go` set to shift-G) |
| Key Map `go`, `stopAll`, `pauseAll`, `resumeAll`, `loadSelected`, `pauseResumeSelected`, `startSelected`, `stopSelected`, `resetSelected`, `preview` | `go`, `stop_all`, `pause_all`, `resume_all`, `load_selected`, `pause_resume_selected`, `start_selected`, `stop_selected`, `reset_selected`, `preview_selected` | default map; `stopAll`, `startSelected`, `stopSelected`, `resetSelected` assumed (not in it) |
| Key Map `panicSelected`, `editNumber`, `editName`, `editNotes`, `editTarget`, `editPreWait`, `editAction`, `editPostWait`, `cycleContinue`, `flag` | `panic_selected`, `edit_number`, `edit_name`, `edit_notes`, `edit_target`, `edit_pre_wait`, `edit_duration`, `edit_post_wait`, `cycle_continue`, `toggle_flag` | default map |
| Key Map `auditionGo`, `auditionPreview`, `cycleTargetType` | dropped without a note (no openLab action; owner 2026-11-26) | all |
| OSC Access (passcodes, ports) | not imported; the workspace gets its own passcode; a note says so | all |
| Audio `audioOutputPatches` | patches, below | cue-types |
| Network `networkPatches` | network patches, below | cue-types |
| Video `routes`, `stages` | routes and stages, below | cue-types |
| Light, MIDI, Mic, Camera, Templates, Audio Maps, Collaboration, Events, Audition | not read | |

## Keys (cue hotkeys and the key map)
`keyCharacter` is the character macOS reports ignoring modifiers: 32 `space`, 13 or 3 `return`, 9 `tab`, 27 `escape`, 0xF700-0xF703 `up` `down` `left` `right`, 0xF704 + n `f(n+1)`, any other printable character lower-cased (`1` for shift-1). 0 is no key.
Modifiers: `shift`, `control`, `option` (alt), `command`; `function`, `numericPad`, `capsLock` are not kept. A key openLab cannot name is dropped with a note. Fixture: groups-triggers H0 (a), H1 (shift-command-1, code 18 char 49), H2 (F5, char 63240), H3 (option-control-space, off).

## Audio patch (`AudioOutputPatch`)
| QLab | openLab | Fixture |
|---|---|---|
| `uniqueID`, `name` | report `audio_patches`; patch name | cue-types |
| `cueOutputChannels` (1-128) | the patch's outputs | cue-types (12; Patch 2: 4) |
| `useSystemDevice`, `deviceDescription` {`deviceUID`, `name`, `outputChannels`} | report only (`system`, `device_uid`, `device_name`, `device_outputs`); the mapping is the operator's | cue-types |
| `levels` (matrix; rows are the patch's inputs, columns the device's outputs: QLab manual pp. 429-430): row 0 column 0 master; row i column 0 cue output i; row 0 column j device output j; row i column j cue output i to device output j | `master_db`, `outputs[i-1].level_db`, `device_outputs` (only where not 0 dB) | cue-types |
| The same matrix's crosspoints | `crosspoints`: none when the matrix is QLab's default (output n to device output n at 0 dB), else every pair listed, one QLab does not store as silence | cue-types (default only; a changed matrix is assumed from the dictionary) |
| `patchOutputEffects`, `cueOutputEffects` with effects | dropped | |
| `cueOutputNames`, `muteChannels`, `soloChannels` | not read | |
| No patches | the new workspace's one stereo patch | |

## Network patch (`networkPatches` entry: `data` dictionary)
| QLab | openLab | Fixture |
|---|---|---|
| `uniqueID`, `name` | patch id map, name | cue-types (Lighting desk) |
| `clientStates[0]` `host`, `port`, `useTcp`, `interface` | `host`, `port`, `protocol`, `interface` (empty: none); more destinations: first only, noted | cue-types (10.0.0.5:9000 TCP) |
| `deviceIdentifier` `com.figure53.oscmessage` | OSC; any other type makes its Network cues unsupported | cue-types |
| `userData` (passcode for the destination), `tcpTimeout` | not read | |

## Video routes and stages (`F53VideoOutputRoute`, stages)
| QLab | openLab | Fixture |
|---|---|---|
| route `uniqueID`, `name`, `naturalSize` "{w, h}" | report `video_outputs`; `OutputRoute` of that size | cue-types (Output 1, 1920 x 1080) |
| `destinationInfo.name` | report `screen` | cue-types (empty) |
| `rotationDegrees` 0/90/180/270 | `rotation`; anything else 0, noted | cue-types (0) |
| `scalingMode` 0 centre, 1 fit, 2 stretch, other fill | `scaling` | assumed: the manual's menu order (p. 233); QLab's default route stores 2 |
| `rear` | dropped | |
| a route's partial screen layout and index (QLab keeps them on the route; none in the fixtures) | not read: the part of a display is this Mac's display choice (skill `video`), so an imported route is on a whole display | assumed 2026-11-27 (#842) |
| `stages` (empty when saved unlicensed) | a stage per entry, name and `size` or `naturalSize`, regions not imported (noted) | assumed: no licensed fixture |
| No stages | "Stage 1", the first route's size, one region covering it on that route; Video cues with an empty `viewportID` play on it | cue-types |

## Cue lists
| QLab | openLab | Fixture |
|---|---|---|
| `GroupCue` mode 0 under the root | cue list: `name`, `cues` | cue-types (Main Cue List, Second List) |
| `GroupCue` mode 5 (cart) | unsupported, reason `cart`; placeholder: a cue list of its cues | assumed: unlicensed QLab refuses `/new cart`; mode 5 is cart in the OSC dictionary |

## Every cue
| QLab | openLab | Fixture |
|---|---|---|
| `uniqueID` | QLab id map (missing: `no-id-<n>`) | all |
| `number` (empty: none), `name`, `notes` (attributed string) | `number`, `name` (cut to 1024 bytes), `notes` (64 KiB) | cue-types 1, 14 |
| `colorName` | `color`: red, orange, yellow, green, cyan, blue, purple, magenta; crimson dark_red, peach apricot, olive, forest dark_green, sky blue light_blue, midnight navy, indigo, lavender lilac, plum, berry raspberry, hot pink pink, gray grey; anything else none | groups-triggers K0-K19 |
| `useSecondColor`, `secondColorName` | `second_color` (none when off) | groups-triggers K20 |
| `flagged`, `armed`, `autoLoad`, `skipIfDisarmed` | same | cue-types 1, 4, 5 |
| `preWait`, `postWait` | same | cue-types 1 |
| `continueMode` 0 / 1 / 2 | do not continue / auto-continue / auto-follow | groups-triggers C0-C2 |
| `cueTargetUniqueID` | the kind's target | cue-types 2-12 |
| `useHotKey`, `hotKey` (`F53HotKey`: `QLActionKeyKeyCharacter`, `...KeyCode`, `...Shift`, `...Control`, `...Option`, `...Command`, `...Function`, `...NumericPad`, `...CapsLock`) | `triggers.hotkey`, `hotkey_enabled` (a key with `useHotKey` off is kept, switched off) | groups-triggers H0-H3 |
| `useWallClock`, `wallHours`, `wallMinutes`, `wallSeconds`, `wallSunday`..`wallSaturday` (missing: on) | `triggers.wall_clock` | groups-triggers W1 (19:30:15, Mon Wed Fri) |
| `wallClockMode` 1 | `twelve_hour` | assumed: only 0 seen |
| `secondTriggerAction` 0-7 | nothing, panic, stop, hard stop, hard stop and restart, devamp, playlist next, playlist previous | groups-triggers T0-T5, G6 (6); 7 from the OSC dictionary |
| `secondTriggerOnRelease` | same | groups-triggers T3 |
| `duckOthers`, `duckLevel` (linear), `duckTime` | `duck_others` (dB) | groups-triggers D1 (−8 dB), D2 (+6 dB) |
| `fadeAndStopOthers` (false when off; 1 peers, 2 list or cart, 3 all), `fadeAndStopOthersTime` | `fade_and_stop_others` | groups-triggers F1-F3 |
| `useMIDITrigger`, `useTimecode` | dropped | |
| `midiTrigger`, `timecode`, `showAsTimecode`, `color` (NSColor), `buildNumber` | not read | |

## Cue kinds
| QLab class | openLab kind | Fixture |
|---|---|---|
| `AudioCue` | Audio, below | cue-types 1, audio-levels |
| `VideoCue` | Video, below | cue-types 16 |
| `FadeCue` (`targetMode` 0) | Fade, below | fades |
| `FadeCue` or `ResetCue` with `targetMode` not 0 (a patch or map) | unsupported, `patch_target` | assumed |
| `GroupCue` `groupMode` 1 / 2 / 3 / 4 / 6 | start first and enter / start first and go to next / timeline / start random / playlist (`playlistLoop`, `playlistShuffle`); 0 nested: timeline | groups-triggers G1-G6 |
| `GroupCue` `playlistCrossfade` | dropped | groups-triggers G6 |
| `StartCue`, `PauseCue`, `ResetCue`, `ArmCue`, `DisarmCue` | same, target only | cue-types 3, 5, 7, 10, 11 |
| `StopCue` | Stop, no fade | cue-types 4 |
| `LoadCue` `loadTime` | Load `load_to_time` | cue-types 6 (0.75) |
| `DevampCue` `devampType` 1 / 2, `fireNextCueAtMomentLoopEnds`, `stopTargetWhenDone` | current slice / looping cue, `start_next_cue`, `stop_target_when_slice_ends` | cue-types 8 |
| `GotoCue` | Go To, `then_go` off (QLab stores no such option) | cue-types 9 |
| `TargetCue` `targetCueNumber` | Target, `new_target` by cue number | cue-types 12 (empty) |
| `WaitCue` `duration` | Wait | cue-types 13 |
| `MemoCue` | Memo | cue-types 14 |
| `OSCCue` (`NetworkCue` accepted) `networkPatchID`, `oscString` | Network: address and arguments split from the text (a quoted string is one argument; a whole number an int, a number with a point a float, else a string) | cue-types 15 (`/cue/1/start`) |
| `OSCCue` with no OSC text, or on a non-OSC patch | unsupported, `network_message` | assumed |
| `OSCCue` `allParameterValues` `com.figure53.oscmessage` `f53.custom.fadeEnabled` | dropped (parameter fades) | |
| `LightCue`, `MIDICue`, `MIDIFileCue`, `TimecodeCue`, `ScriptCue`, `CameraCue`, `MicCue`, `TitlesCue` (Text), any other class | unsupported, `cue_type` | cue-types 20-27, 30 |

## Audio cue
| QLab | openLab | Fixture |
|---|---|---|
| `fileTarget` (`F53Alias`: `relativePath` from the `.qlab5` folder, `lastKnownPath`; `bookmark` ignored) | `media`, found by the engine | cue-types 1, audio-levels 6 (none) |
| `audioOutputPatchID` | `patch` (unknown: the first patch) | cue-types 1 (Patch 2) |
| `startTime`, `endTime` (the file's length when untrimmed; `lastSeenFileDuration`) | `start_time`, `end_time` (none when at or past the file's end) | audio-levels 2 (0.1, 0.9) |
| `slices` (`AudioSlice`: `time`, `playCount`, `infiniteLoop`) | slice markers: `infiniteLoop` endless, `playCount` 0 skip, else that many | audio-levels 2 |
| `lastSlice` | `last_slice_play_count` | audio-levels 2 (3), 5 (endless) |
| `playCount`, `infiniteLoop` (the whole cue) | without slices: the last slice's count (the cue's wins if both are set, noted); with slices: dropped | audio-levels 3 (loop), 4 (2) |
| `levels` (`AudioLevelMatrix` `entries`, keyed by number: `AudioLevelKnobs` `row`, `column`, `initialLevel`, `trimLevel`, `gang`) | row 0 column 0 `master_db`; row 0 column n `outputs[n-1]` (as many as the patch has outputs); row i column n a crosspoint from input i-1 to output n-1; row i column 0 input i's level, folded into its crosspoints (noted); `trimLevel` multiplies `initialLevel` (noted) | audio-levels 1 (−6 master, −3 out 1, −∞ out 2, trim 0.5 on out 4, input 1 at −1.5) |
| `gang` on row 0 knobs (also listed in the matrix's `gangs`) | `gangs` of cue outputs; gangs of the master or crosspoints dropped | audio-levels 1 (outputs 1 and 3) |
| `rate` not 1 | dropped | audio-levels 3 |
| `effects`, `doFade` (integrated fade), `audioMapID`, `audioObjects`, `muteChannels`, `soloChannels` | dropped | |
| `doPitchShift`, `track`, `waveformVisibleChannel`, `lockFadeToCue`, `fade` (the integrated envelope) | not read | |

## Fade cue
| QLab | openLab | Fixture |
|---|---|---|
| `duration` | `duration` | fades F1 (2) |
| `mode` 0 / 1 | absolute / relative | fades F1, F2 |
| `stopTargetWhenDone` | `stop_target_when_done` | fades F3 |
| `fade.entries` (`FadeValueEntry`: `row`, `column`, `endValue` linear, `live`): only `live` ones | row 0 column 0 master; row 0 column n `output_db[n-1]`; row i column n crosspoint i-1 to n-1; row i column 0 (input level) dropped. A level QLab sets to −∞ is stored as no entry | fades F1 (−20), F2 (−6 relative), F3 (none), F4 (out 1 −10, 1→1 −3, 2→2 0) |
| `fade.shapes.upShape` / `downShape` | `curve.rising` / `curve.falling` | fades F5-F11 |
| a shape's `type`: 0 custom, 1 S-curve, 2 parametric, 3 linear (none: S-curve) | `custom`, `s_curve`, `parametric`, `linear`; any other type (4 seen, which QLab's menu does not offer and draws as a straight rising line) is noted and the curve takes the default shapes | fades F5, F6, F7, F9, F8 (4): each seen in QLab's Curve tab menu |
| a shape's `curveParameter` (none: 1) | `parameter` | fades F11 (2 on the rising shape) |
| a shape's `shapeEntries` (`FadeShapeEntry` `t`, `v`, linked `previous`/`next`; 32-bit numbers): `t` the time and `v` the height in the graph, 0 to 1, the fixed ends stored as the first and last entries | `points`, the entries at `t` 0 and 1 dropped. A shape openLab cannot hold (a point outside 0..1, out of order, more than 64, a parameter outside 0.01..100, no `t` or `v`) is noted; the curve takes the default shapes, keeping its domain | fades F11, F9 (below) |
| `audioDomain` 0 slider, 1 decibel, 2 linear (none: slider) | `curve.audio_domain`; another value is noted and becomes slider | fades F9 (0), F12 (1), F10 (2); order from the manual's menu (p. 199) and QLab's derived `shapes.domain.mode` (1, 2, 0) |
| `shapes.domain`, `mirrorShape`, `resolution` | not read (the domain is `audioDomain`'s) | |
| `doOpacity`/`opacity`, `doTranslation`/`translationX`/`translationY`, `doScale`/`scaleX`/`scaleY`, `preserveAspectRatio`, `doRotation`/`rotation` | `geometry` | fades F20 |
| `doRate` | dropped | |

What `v` means was settled in QLab's Curve tab: fades F11 (custom: rising (0.5, 0.8); falling (0.25, 0.3), (0.6, 0.1)) and F9 (linear: rising (0.3, 0.6); falling (0.7, 0.6)) draw each point at that time and height in both graphs, so `v` is the height in the graph, not the progress of the fade.

QLab draws a custom curve as a smooth curve near its points, not through them (a quadratic B-spline); openLab's custom curve is the same curve (skill qlab-behaviour Q839), so a converted custom curve follows QLab's.

## Video cue
| QLab | openLab | Fixture |
|---|---|---|
| `fileTarget` | `media` | cue-types 16 |
| `viewportID` (empty: QLab's single stage) | `stage` | cue-types 16 |
| `duration` (> 0) | `duration` (a still's) | cue-types 16 (4) |
| `fullScreen`, `fillStyle` 0 fit / 1 fill / 2 stretch | `fill_stage`, `fill_style` | cue-types 16 (fill) |
| `translationX/Y`, `scaleX/Y`, `preserveAspectRatio`, `cropTop/Bottom/Left/Right`, `originX/Y`, `opacity`, `layer`, `antialias` | `translation`, `scale`, `scale_locked`, `crop`, `anchor` (assumed pixels from the centre), `opacity`, `layer`, `smooth` | cue-types 16 |
| `startTime`, `endTime`, `slices`, `lastSlice`, `playCount`, `infiniteLoop`, `holdLastFrame` | `timing` as an Audio cue's, `hold_at_end` | cue-types 16 |
| `audioCue` (an embedded `AudioCue`): `audioOutputPatchID`, `levels` | `sound.patch`, `sound.levels` | cue-types 16 |
| `quaternion` (set), `blendMode.index` not 0, `videoEffects.effectEntries`, `rate` not 1 | dropped | |
