# openLab — Requirements (Complete Product)

The full feature set for a "finished" openLab: an open-source, cross-platform show-control and playback system in the spirit of QLab 5.
This is the master list. Scope for each release (starting with v0.0.1) is chosen from here and tracked in GitHub issues and milestones.

**Conventions**
- Each requirement has a stable ID (e.g. `AUD-07`). IDs are never reused or renumbered; retired items are struck through, not deleted.
- `- [ ]` = not done, `- [x]` = done (implemented **and** tested). Tick only when the tests exist and pass.
- Release tags: `v0.0.1`, `v0.0.2` = targeted release. Untagged = later. Partial scoping for a release is listed in §21.
- Priority tags: `low` = low priority, `nice-to-have` = optional. Untagged = normal priority.
- **MUST / SHOULD / MAY** follow RFC 2119 meaning. The release a requirement lands in is decided separately.

**Roadmap order:** Audio → Video. Lighting deferred.
**Primary use:** theatre playback.

---

## 1. Non-Functional Requirements

### 1.1 Reliability & safety (highest priority — this runs live shows)
- [x] **NFR-01** `v0.0.1` Playback MUST NOT stop, glitch or crash because of a UI fault, an editing action, a slow disk read, or a malformed input. The playback engine is isolated from UI failure.
- [x] **NFR-02** `v0.0.1` The real-time audio path MUST be real-time safe: no memory allocation, locks, blocking I/O, or logging on the audio thread.
- [x] **NFR-03** `v0.0.1` Every external input (workspace files, media files, OSC/MIDI/network messages, plugin output, settings) MUST be validated; bad input produces a warning, never a crash or hang.
- [x] **NFR-04** `v0.0.1` Saves MUST be atomic (write-temp-then-rename). A crash or power loss during save never corrupts the existing file.
- [x] **NFR-05** `v0.0.1` Autosave to a recovery file at a configurable interval; on launch after an unclean exit, offer to restore.
- [x] **NFR-06** `v0.0.1` Keep N rolling timestamped backups of each workspace (configurable, default on).
- [x] **NFR-07** `v0.0.1` Missing/unsupported media, missing audio/MIDI devices and Network cues with no or an unusable network patch or a malformed message MUST be reported as "broken" cue state before the show, not discovered at GO. As in QLab 5, whether the destination is reachable or its host name resolves is not checked: a failed send is a warning at GO.
- [ ] **NFR-08** `v0.0.1` Device hot-unplug/replug (audio interface, MIDI, display) MUST be survived without a crash; reconnect automatically where possible, warn loudly otherwise.
- [x] **NFR-09** `v0.0.1` The app MUST run for 24 hours continuously with no memory growth, timing drift or performance degradation (soak-tested).
- [x] **NFR-10** `v0.0.1` No network connection is ever required. No telemetry by default. No modal dialogs, update prompts or OS notifications from openLab during Show Mode.
- [x] **NFR-11** `v0.0.1` A single misbehaving cue (bad file, failing script, unreachable OSC host) MUST NOT affect other running cues.

### 1.2 Performance & timing
- [x] **NFR-20** `v0.0.1` GO-to-sound latency for a loaded cue ≤ 10 ms beyond the audio device's buffer latency.
- [x] **NFR-21** `v0.0.1` Cues started together (same GO, group, or auto-continue with zero wait) MUST start sample-accurately relative to each other.
- [x] **NFR-22** `v0.0.1` Waits, fades and timecode triggers MUST be scheduled against the audio clock (not UI timers) with ≤ 1 ms jitter.
- [x] **NFR-23** `v0.0.1` Minimum spec target: 2014 Mac mini (Intel dual-core, macOS 12 Monterey) and low-end Windows 10/11 x64 laptops. On minimum spec: ≥ 32 simultaneous stereo audio cues streaming from disk at 48 kHz with fades, without dropouts.
- [x] **NFR-24** `v0.0.1` Workspaces with ≥ 2,000 cues MUST open in < 3 s and edit without UI lag on minimum spec.
- [x] **NFR-25** `v0.0.1` Idle CPU < 2% on minimum spec with a workspace open and no cues running.
- [x] **NFR-26** `v0.0.1` UI rendering MUST NOT compete with the audio engine for priority; meters/waveforms degrade gracefully under load.
- [x] **NFR-27** `v0.0.1` Audio dropout (xrun) counter visible to the operator and logged.

### 1.3 Platform
- [x] **NFR-30** `v0.0.1` macOS 12 Monterey or later, Universal binary (x86_64 + arm64). Priority platform.
- [x] **NFR-31** `v0.0.1` Windows 10 and 11 (x64). All platform-specific code is isolated behind a thin layer from day one so Windows is a port, not a rewrite.
- [x] **NFR-32** `v0.0.1` Workspace files are fully portable between macOS and Windows (path separators, line endings, case sensitivity, file-name encoding).
- [ ] **NFR-33** Linux MAY be supported later; nothing in the design should preclude it.

### 1.4 Maintainability & openness
- [x] **NFR-40** `v0.0.1` Workspace file format is open, documented, versioned, human-readable and diff-friendly (text-based), with automatic forward migration of older versions.
- [x] **NFR-41** `v0.0.1` Core engine logic (cue model, scheduling, state machine, routing, fades) is UI-independent and testable headlessly.
- [x] **NFR-42** `v0.0.1` Licensed GPLv3-or-later. All dependencies must be GPLv3-compatible. LICENSE file and SPDX headers present.
- [x] **NFR-43** `v0.0.1` Reproducible builds via a single documented command per platform; CI builds every commit.

---

## 2. Workspace & Files
- [x] **WS-01** `v0.0.1` A workspace is a single document containing cue lists, carts, patches, settings and references to media.
- [x] **WS-02** `v0.0.1` New / Open / Save / Save As / Close; multiple workspaces open at once (no Revert, as QLab 5).
- [x] **WS-03** `v0.0.1` Media referenced by relative path when inside the workspace folder, absolute otherwise; relink missing media (single and batch "find in folder").
- [ ] **WS-04** "Bundle workspace": copy workspace + all referenced media into one self-contained folder for transport between machines.
- [ ] **WS-05** Workspace templates (start a new show from a saved template).
- [x] **WS-06** `v0.0.1` Per-workspace settings (patches, MIDI/OSC config, hotkeys, defaults) travel with the file; per-machine settings (device names, window layout) do not.
- [x] **WS-07** `v0.0.1` Device patch mapping: a workspace refers to logical outputs; on a different machine the operator maps them to physical devices once.
- [x] **WS-08** `v0.0.1` Recent files, open-on-launch option.
- [ ] **WS-09** Import/export cue list as CSV (number, name, notes, type, timings).
- [ ] **WS-10** Print / export cue sheet to PDF.
- [ ] **WS-11** `nice-to-have` Best-effort import of QLab workspaces (cue structure, names, numbers, timings, audio file references).

---

## 3. Cue Lists & Carts
- [x] **CL-01** `v0.0.1` Multiple cue lists per workspace, each with its own playhead.
- [x] **CL-02** `v0.0.1` Cue list shows, in QLab 5's fixed column order: status, type icon, number, name, target, pre-wait, duration, post-wait, continue mode; cue colour tints the row, a flag shows in the status column; column widths are remembered. *(Column show/hide/reorder dropped 2026-10-29: QLab 5 has none.)*
- [x] **CL-03** `v0.0.1` Hierarchical display of groups (expand/collapse).
- [ ] **CL-04** `v0.0.3` Carts: grid of cues fired directly by click/hotkey/trigger, independent of the playhead. Configurable grid size.
- [x] **CL-05** `v0.0.1` Cue list / cart switching via tabs or sidebar.
- [x] **CL-06** `v0.0.1` Playhead visibly distinct; "standing by" cue shown large with name and notes.
- [ ] **CL-07** `v0.0.2` Open a cue list in a new window (sidebar button and context menu; a second window on the same workspace), as QLab 5.

---

## 4. Cue Model (common to all cues)
- [x] **CUE-01** `v0.0.1` Fields: number (free text, unique within workspace or empty), name, notes, colour, flag, armed/disarmed.
- [x] **CUE-02** `v0.0.1` Timing: pre-wait, action (duration), post-wait.
- [x] **CUE-03** `v0.0.1` Continue modes: do not continue, auto-continue (after pre-wait/post-wait), auto-follow (after action completes).
- [x] **CUE-04** `v0.0.1` Lifecycle states: idle, loaded, pre-waiting, running, paused, post-waiting, fading, stopping, broken — with defined, tested transitions.
- [x] **CUE-05** `v0.0.1` Operations on any cue: start, stop, pause, resume, load, load-to-time, reset, preview/audition.
- [x] **CUE-06** `v0.0.1` Broken state with human-readable reason(s), shown in the list and the inspector.
- [x] **CUE-07** `v0.0.1` Disarmed cues are skipped/ignored when triggered (still visible).
- [x] **CUE-08** `v0.0.1` Unique persistent internal ID per cue, independent of cue number (used by targets, OSC, scripts).
- [x] **CUE-09** `v0.0.1` Hotkey, MIDI, OSC, timecode and wall-clock triggers assignable per cue (see §10).
- [ ] **CUE-10** `v0.0.3` Default settings per cue type, configurable per workspace.

---

## 5. Cue Types

### 5.1 Media
- [x] **CT-01** `v0.0.1` **Audio** — plays an audio file through the routing matrix (see §7).
- [ ] **CT-02** `v0.0.3` **Mic** — routes live audio input(s) through the matrix with levels/fades/effects.
- [ ] **CT-03** `v0.0.2` **Video** — plays a video file or still image to a video surface (see §8).
- [ ] **CT-04** **Camera** — displays a live video input on a surface.
- [ ] **CT-05** **Text** — renders styled text to a video surface.
- ~~**CT-06** **Light** — sets lighting levels with a fade time (see §9).~~ *(retired: lighting deferred)*

### 5.2 Control
- [x] **CT-10** `v0.0.1` **Fade** — fades levels (audio), geometry/opacity (video) of a target cue/group, with duration and curve; option to stop target when done.
- [x] **CT-11** `v0.0.1` **Start** — starts its target.
- [x] **CT-12** `v0.0.1` **Stop** — stops its target (optionally with fade).
- [x] **CT-13** `v0.0.1` **Pause** — pauses its target.
- [x] **CT-14** `v0.0.1` **Load** — loads its target to a given time.
- [x] **CT-15** `v0.0.1` **Reset** — resets target to its saved state.
- [x] **CT-16** `v0.0.1` **Devamp** — exits the current loop of a target at the end of the loop, optionally starting the next cue.
- [x] **CT-17** `v0.0.1` **GoTo** — moves the playhead to the target (optionally and GO).
- [ ] **CT-18** **Target** — changes another cue's target at run time.
- [x] **CT-19** `v0.0.1` **Arm / Disarm** — arms or disarms a target.
- [x] **CT-20** `v0.0.1` **Wait** — does nothing for its duration (used for timing in sequences).
- [x] **CT-21** `v0.0.1` **Memo** — does nothing; notes for the operator.
- [x] **CT-22** `v0.0.1` **Group** — contains cues; modes: start all simultaneously (timeline), start first and enter, start first and go to next, playlist (sequential, optional loop/shuffle), start random child.
- [ ] **CT-23** `low` **Script** — runs a script in the embedded scripting language (see §13), sandboxed with timeout.

### 5.3 Show control output
- [x] **CT-30** `v0.0.1` **Network** — sends OSC (UDP/TCP) or raw UDP/TCP string messages to a configured destination; parameter fades for OSC values.
- [ ] **CT-31** **MIDI** — sends note, CC, program change, pitch bend, SysEx, MIDI Show Control (MSC) messages; CC fades.
- [ ] **CT-32** **MIDI File** — plays a Standard MIDI File to a MIDI output, synced to the cue clock.
- [ ] **CT-33** **Timecode** — generates LTC (via audio output) or MTC (via MIDI) at a chosen frame rate and start time.

---

## 6. Playback & Show Control
- [x] **PB-01** `v0.0.1` GO: starts the cue at the playhead and advances the playhead per continue mode.
- [x] **PB-02** `v0.0.1` Double-GO protection: configurable minimum interval between GOs.
- [x] **PB-03** `v0.0.1` Panic / Stop All: fades all cues out over a configurable time; a second press during the fade stops instantly.
- [x] **PB-04** `v0.0.1` Pause All / Resume All.
- [x] **PB-05** `v0.0.1` Playhead navigation (next/previous, jump to cue by number) without firing.
- [ ] **PB-06** Load-to-time: start the show (or a cue) from an arbitrary point with correct state, including what should already be playing (for rehearsal pick-ups).
- [x] **PB-07** `v0.0.1` Active Cues panel: running cues with elapsed/remaining time, progress, and per-cue stop/pause/fade-out controls. No meters here (QLab 5 has none).
- [ ] **PB-08** Per-cue live level adjustment while running (temporary, not saved unless chosen).
- [ ] **PB-09** Audition / preview: play a cue to a separate audition output without affecting show outputs or triggering follow-ons.
- [x] **PB-10** `v0.0.1` Global output overrides: independently disable MIDI, OSC/network, timecode output (for rehearsal/editing), with clear on-screen indication.
- [x] **PB-11** `v0.0.1` Per-patch main level and per-output level in the patch editor, within the workspace's output dB limits. *(Global master dropped 2026-10-29: QLab 5 has none.)*
- [x] **PB-12** `v0.0.1` Wall clock and per-show countdown/elapsed timers visible.

---

## 7. Audio Engine
- [x] **AUD-01** `v0.0.1` Device back-ends: CoreAudio (macOS); WASAPI and ASIO (Windows).
- [ ] **AUD-02** `v0.0.2` Multiple simultaneous output devices (a device per output patch, as QLab 5); aggregate/device-sync guidance where clocks differ.
- [x] **AUD-03** `v0.0.1` Arbitrary channel counts (at least 64 outputs per device).
- [x] **AUD-04** `v0.0.1` File formats: WAV, AIFF, FLAC, MP3, AAC/M4A, OGG; 16/24/32-bit int and float; any common sample rate with high-quality resampling to the device rate.
- [x] **AUD-05** `v0.0.1` Stream from disk with read-ahead buffering; optional preload-to-RAM per cue for short or critical files.
- [x] **AUD-06** `v0.0.1` Routing matrix per cue: file channels → cue outputs → patch outputs, with crosspoint levels (dB) and mute.
- [x] **AUD-07** `v0.0.1` Output patches: named logical outputs mapped to device channels, with per-output level, mute and polarity (no per-output delay: QLab 5 has none).
- [x] **AUD-08** `v0.0.1` Cue master level, per-channel levels, ganging of channels.
- [x] **AUD-09** `v0.0.1` Fade curves: linear, equal-power, S-curve, logarithmic, exponential; custom curves.
- [x] **AUD-10** `v0.0.1` Integrated fade-in/fade-out inside an Audio cue.
- [x] **AUD-11** `v0.0.1` Start/end trim (in/out points).
- [x] **AUD-12** `v0.0.1` Slices and loops: mark slice points; each slice can loop N times or infinitely; Devamp support.
- [ ] **AUD-13** `v0.0.3` Playback rate change, with optional pitch preservation.
- [x] **AUD-14** `v0.0.1` Waveform display with zoom; drag to set trims, fades, slice points.
- [ ] **AUD-15** Loudness analysis (LUFS/peak) and optional normalisation per cue (non-destructive).
- [ ] **AUD-16** Built-in effects per cue and per output: parametric EQ, compressor/limiter, delay, reverb, high/low-pass.
- ~~**AUD-17** Third-party plugin hosting: Audio Unit (macOS), VST3 (both). Plugins run in a way that a plugin crash cannot take down playback.~~ *(retired: not required)*
- ~~**AUD-18** Output protection: optional brick-wall limiter per output.~~ *(retired: not required)*
- [x] **AUD-19** `v0.0.1` Metering: peak meters beside the faders in the cue's levels and trim tabs and in the patch editor, as QLab 5; none in the Active Cues panel or the cue list.
- [ ] **AUD-20** `v0.0.3` Live input (Mic cues) with selectable input device/channels and low-latency monitoring.
- [ ] **AUD-21** `v0.0.2` Configurable audio buffer size per machine (app preferences, Hardware). ~~Sample-rate chooser and latency readout~~ *(dropped 2026-10-29: QLab 5 has no equivalent; the rate comes from the device.)*

---

## 8. Video Engine
- [ ] **VID-01** `v0.0.2` Video surfaces: named outputs mapped to one or more displays; windowed mode for editing.
- [ ] **VID-02** `v0.0.2` Multi-display output; multiple surfaces per display.
- [ ] **VID-03** `v0.0.2` Codecs: H.264, HEVC, ProRes, HAP (and HAP variants); still images (PNG, JPEG, TIFF); hardware decode where available.
- [ ] **VID-04** `v0.0.2` Per-cue geometry: position, scale, rotation, crop, opacity, layer order, blend mode.
- [ ] **VID-05** `v0.0.2` Fades of opacity and geometry via Fade cues; integrated fade-in/out.
- [ ] **VID-06** `v0.0.2` Video audio track routed through the audio engine, frame-synced.
- [ ] **VID-07** `v0.0.2` Loop, trim, playback rate, hold last frame.
- [ ] **VID-08** Corner-pin / keystone correction per surface.
- [ ] **VID-09** Edge blending and multi-projector surfaces.
- [ ] **VID-10** Video effects (colour adjust, blur, etc.).
- [ ] **VID-11** Inter-app video: Syphon (macOS), Spout (Windows), NDI (in and out).
- [ ] **VID-12** `v0.0.2` Black/"blackout" safety: surfaces render black (never desktop, never a frozen frame) when nothing is playing.

---

## 9. Lighting — DEFERRED

Lighting is out of scope until further notice. Items kept for reference only.

- ~~**LX-01** DMX output via Art-Net, sACN (E1.31) and USB DMX (ENTTEC DMX USB Pro / Open DMX).~~ *(retired: lighting deferred indefinitely)*
- ~~**LX-02** Multiple universes.~~ *(retired: lighting deferred indefinitely)*
- ~~**LX-03** Fixture patch with a fixture-definition library (import Open Fixture Library format).~~ *(retired: lighting deferred indefinitely)*
- ~~**LX-04** Light cues set fixture/group parameters with fade time and curve.~~ *(retired: lighting deferred indefinitely)*
- ~~**LX-05** Light Dashboard: live view and manual control of all fixture parameters.~~ *(retired: lighting deferred indefinitely)*
- ~~**LX-06** Fixture groups; HTP/LTP merging of concurrent cues.~~ *(retired: lighting deferred indefinitely)*
- ~~**LX-07** Light Fade/Stop via standard Fade/Stop cues; Panic blackout behaviour configurable.~~ *(retired: lighting deferred indefinitely)*

---

## 10. Triggers & Inputs
- [x] **TRG-01** `v0.0.1` Keyboard hotkeys for GO, Stop All, Pause/Resume All, playhead navigation, and per-cue triggers; fully remappable; work regardless of which panel has focus (in Show Mode).
- [ ] **TRG-02** `v0.0.3` MIDI input triggers: note, CC, program change, MSC; per-cue and global (GO/Stop via MIDI).
- [x] **TRG-03** `v0.0.1` OSC input triggers: per-cue and global.
- [ ] **TRG-04** Timecode input (LTC via audio, MTC via MIDI) triggering cues at specific timecode values; chase/freewheel handling for dropouts.
- [ ] **TRG-05** Wall-clock triggers (time of day, with day-of-week).
- [ ] **TRG-06** Generic USB/HID controllers (foot pedals, Stream Deck via Companion) through MIDI/OSC/hotkey mapping.
- [ ] **TRG-07** `v0.0.2` The key map includes QLab 5's cue-editing keys column: edit number, name, notes, target, cycle target type, pre-wait, action duration, post-wait, cycle continue mode, flag/unflag; plus Preview Selected as remappable; Esc and double-Esc shown read-only.

---

## 11. Network, Remote & Integration
- [x] **NET-01** `v0.0.1` OSC API for full control and query (GO, stop, playhead, cue properties, state), documented. Address space compatible with QLab 5's OSC dictionary so existing remotes, Stream Deck/Companion setups and scripts work unchanged where the feature exists.
- [x] **NET-02** `v0.0.1` OSC passcode / access control for the API.
- [ ] **NET-03** Web-based remote (phone/tablet browser) served by openLab: GO, Stop, playhead, active cues, notes. No app install required.
- [ ] **NET-04** `low` Bitfocus Companion module.
- [x] **NET-05** `v0.0.1` Status feedback over OSC (playhead changes, cue starts/stops) for external displays.

---

## 12. Redundancy
- [ ] **RED-01** Primary/backup mode: two machines run the same workspace; GO, stop, playhead and live edits on the primary are mirrored to the backup over the network.
- [ ] **RED-02** Heartbeat monitoring with visible link status on both machines.
- [ ] **RED-03** Workspace sync check: warn if primary and backup files differ.

---

## 13. Scripting & Extensibility
- [ ] **SCR-01** `low` Embedded, cross-platform scripting language (replacing QLab's AppleScript) for Script cues.
- [ ] **SCR-02** `low` Script API: read/modify cues, fire cues, move playhead, send OSC/MIDI, read state.
- [ ] **SCR-03** `low` Scripts run off the audio thread, with timeout and error isolation; a failing script reports an error and never blocks playback.

---

## 14. Editing & Workflow
- [x] **ED-01** `v0.0.1` Inspector panel for the selected cue(s), with tabs per concern (basics, triggers, time & loops, audio levels, effects, etc.).
- [x] **ED-02** `v0.0.1` Drag-and-drop media files from Finder/Explorer to create cues (one per file; multi-file drop creates multiple cues or a group).
- [x] **ED-03** `v0.0.1` Drag to reorder cues, move into/out of groups, drop onto a cue to set its target.
- [x] **ED-04** `v0.0.1` Multi-select with batch editing of shared properties.
- [x] **ED-05** `v0.0.1` Cut / copy / paste / duplicate cues, including across workspaces.
- [ ] **ED-06** `v0.0.2` Paste properties (selectively) from one cue to others.
- [x] **ED-07** `v0.0.1` Unlimited undo/redo covering every edit, including routing and settings.
- [x] **ED-08** `v0.0.1` Auto-numbering on cue creation (configurable start/increment) and bulk renumber.
- [x] **ED-09** `v0.0.1` Find bar (by name, number, notes, type, file): found count, previous/next, select found. ~~Find-and-replace~~ *(dropped 2026-10-29: QLab 5 has none.)*
- ~~**ED-10** Create fade/stop cues for selected cue in one action ("fade this out").~~ *(dropped 2026-10-29: QLab 5 has no built-in command; the Cues menu only creates an empty Fade cue.)*
- [ ] **ED-11** `v0.0.3` Timeline view for group cues showing child timing graphically, with drag editing.
- [x] **ED-12** `v0.0.1` Fully keyboard-operable editing (create cue types, navigate, edit fields).
- [x] **ED-13** `v0.0.1` Cue colours and flags (set per cue and per selection). ~~Filter/highlight by colour/flag/state~~ *(dropped 2026-10-29: QLab 5 has none; Find covers lookup.)*
- [x] **ED-14** `v0.0.1` Transport actions live where QLab 5 puts them, with no openLab Transport menu: sidebar top row (Reset/Pause/Resume/Panic All), cue right-click menu (Load, Preview, Panic, Move playhead), View menu (playhead moves, select/move to cue).

---

## 15. Show Mode & Operator Safety
- [x] **SM-01** `v0.0.1` Edit Mode vs Show Mode. Show Mode hides the cue-type toolbar and the inspector (as QLab 5), enlarges the GO button and standby display, and disables destructive actions.
- ~~**SM-02** Optional password to leave Show Mode.~~ *(retired: not required)*
- [x] **SM-03** `v0.0.1` Large, high-contrast standby/GO display usable at a distance in a dark booth.
- [x] **SM-04** `v0.0.1` Dark UI by default; brightness-appropriate for blacked-out booths.
- [x] **SM-05** `v0.0.1` Accidental-quit protection: confirm quit while cues are running or Show Mode is active.
- [x] **SM-06** `v0.0.1` Pre-show check: one action lists all broken cues, missing media, missing devices and unreachable targets.
- [x] **SM-07** `v0.0.1` Prevent system sleep, screen saver and display sleep while a workspace is open in Show Mode.

---

## 16. Diagnostics & Logging
- [x] **DX-01** `v0.0.1` Warnings listing every broken cue and system problem, click-through to the cue: a footer popover plus the status window (as QLab 5); no sidebar Warnings tab.
- [x] **DX-02** `v0.0.1` Show log: timestamped record of every GO, cue start/stop, trigger received, error — viewable in-app and written to disk.
- [x] **DX-03** `v0.0.1` System status: CPU, audio engine load, disk read health, xrun count, device status.
- [x] **DX-04** `v0.0.1` Crash reports captured locally (with workspace path and last log lines); sending is opt-in.
- [ ] **DX-05** Diagnostic bundle export (logs, settings, system info) for bug reports.

---

## 17. Preferences & UI
- [x] **UI-01** `v0.0.1` Settings split as in QLab 5. Application preferences (per machine): launch behaviour, autosave interval, audio devices and buffer size, MIDI devices. Workspace settings (travel with the show file, WS-06): GO protection, Panic time, backups, output patches and dB limits, hotkeys. ~~Theme~~ *(dropped 2026-10-29: QLab 5 has no theme control.)*
- [x] **UI-02** `v0.0.1` Resizable, dockable panels; layouts saved per machine.
- [ ] **UI-03** `v0.0.2` UI scaling / font size for small laptop screens and high-DPI displays.
- [x] **UI-04** `v0.0.1` Cue state is never conveyed by colour alone (icon/shape too) — colour-blind safe. A cue colour is a row highlight only, as in QLab; it is not a state.
- [ ] **UI-05** Basic screen-reader support (VoiceOver / Narrator) for core list and transport controls.
- [x] **UI-06** `v0.0.1` UI text externalised for future localisation.
- [x] **UI-07** `v0.0.1` Layout, workflow, terminology and keyboard shortcuts closely follow QLab 5 so existing QLab users can switch with minimal retraining (main window: toolbar + GO/standby area, cue list, inspector tabs at the bottom, sidebar for lists/carts/active cues).
- [x] **UI-08** `v0.0.1` All visual assets are original: no QLab icons, logos, artwork, screenshots, colour branding, product names or copied documentation text.

---

## 18. Distribution & Updates
- [x] **DST-01** `v0.0.1` macOS: `.dmg` (ad-hoc signed until an Apple Developer account exists; signed + notarised later); Windows: signed installer and a portable zip.
- [ ] **DST-02** Update check is opt-in, manual or on launch only, never during Show Mode; no silent auto-install.
- [x] **DST-03** `v0.0.1` Versioned releases with changelog; side-by-side install of multiple versions possible (so a show can stay pinned to a known-good version).

---

## 19. Quality Assurance (process requirements)
- [x] **QA-01** `v0.0.1` Unit tests for all engine logic (cue state machine, scheduling, continue modes, groups, fades, routing math, file format).
- [x] **QA-02** `v0.0.1` Scheduler and engine testable against a virtual clock — deterministic, no real-time sleeps in tests.
- [x] **QA-03** `v0.0.1` Offline render tests: run cue sequences headlessly, render audio to buffers, assert on sample-level output (timing, levels, fades).
- [x] **QA-04** `v0.0.1` Fuzz testing of workspace loader, media decoders, OSC and MIDI parsers. (v0.0.1: loader, decoders, OSC and the protocol are fuzzed in CI; there is no MIDI input in v0.0.1 — TRG-02 is untagged — so the MIDI parser gets its fuzz target when it exists.)
- [x] **QA-05** `v0.0.1` Soak tests (24 h continuous random cue playback) with memory/CPU/drift checks.
- [x] **QA-06** `v0.0.1` Performance benchmarks with regression gates in CI.
- [x] **QA-07** `v0.0.1` CI on macOS (Intel + Apple Silicon) and Windows for every commit; tests must pass to merge.
- [ ] **QA-08** Before 1.0 deployment: manual run on minimum-spec hardware (2014 Mac mini / Monterey). Not available during development, so until then the macOS 12 deployment target, Intel (x86_64) CI runs and CPU budgets in performance tests stand in for it.
- [x] **QA-09** `v0.0.1` Sanitizer builds (address, thread, undefined-behaviour) run in CI where the toolchain supports them.

---

## 20. Documentation
- [x] **DOC-01** `v0.0.1` User manual (getting started, every cue type, show-day checklist).
- [ ] **DOC-02** `v0.0.2` OSC API reference, written for end users.
- [x] **DOC-03** `v0.0.1` Workspace file format specification.
- [x] **DOC-04** `v0.0.1` Contributor guide (build, architecture, testing rules).

---

## 21. Release Scope Notes

### v0.0.1 — basic playback rehearsal (audio only, macOS)
Where a tagged requirement is only partly in v0.0.1, the limit is:
- **NFR-09 / QA-05** soak via accelerated virtual-clock runs (24 h simulated) plus a shorter real-time run.
- **NFR-31** Windows compiles and passes tests in CI; no installer, no hardware validation.
- **WS-02** at most 8 workspaces open; they share one audio device (AUD-03).
- **CL-05** cue lists only (carts later).
- **CUE-09** hotkey and OSC triggers only.
- **CT-10** audio level fades only (video fades in v0.0.2).
- **WS-06**: per-cue-type defaults deferred with CUE-10.
- **CT-30**: OSC send only; raw UDP/TCP string messages and OSC parameter fades later.
- **PB-10** OSC/network output override only (MIDI/timecode not yet present).
- **PB-12** wall clock only.
- **AUD-01** CoreAudio and WASAPI; ASIO later.
- **AUD-03** single device, up to 64 outputs.
- **AUD-04** OGG optional.
- **AUD-09** standard curves; custom curves later.
- **NET-01** core subset: GO, stop, panic, pause/resume, playhead, select, start/stop/pause cue by number/ID, basic cue properties (name, number, notes, levels, times).
- **UI-02** fixed QLab-like layout; docking later.
- **DST-01** macOS `.dmg` only.
- **DOC-01** getting-started guide only.
- **NFR-08** stays unticked until the manual device unplug and replug run is recorded (moved to v0.0.2, user decision 2026-11-13); CI's device-loss tests stand in until then. **NFR-23 / NFR-25**: the user ran the minimum-spec check on the 2014 Mac mini and reported it passed (2026-11-13; no figures recorded).
- **NFR-05 / NFR-06 / UI-01** autosave runs at a fixed 30 s interval; the number of backups kept is `backup_keep` in the per-machine `settings.json` (default 20). There is no preferences pane for either (issue cut, user decision 2026-11-08). Workspace Settings has General, Audio and OSC.
- **TRG-01 / ED-12** the default keys work and a show's key map is read from its settings; there is no key-map editor, and the cue-editing keys are N, Q and T only (issue cut 2026-11-08; the editor and the rest of the column are TRG-07, v0.0.2).
- **ED-14** the sidebar row, the cue menu and the View menu hold the transport actions as QLab 5 does; openLab's own Transport menu also stays in v0.0.1 (issue cut 2026-11-08).
- **DX-01** warnings show in the footer popover and the Pre-Show Check; the sidebar's Warnings tab also stays in v0.0.1 (issue cut 2026-11-08).
- **CL-07 / TRG-07 / UI-03 / AUD-21** moved to v0.0.2 (issue cut, user decision 2026-11-08): a cue list in a second window, the key-map editor with the cue-editing keys column, the row-size setting, and a configurable audio buffer size (v0.0.1 asks the device for 256 frames).
- **DOC-02** moved to v0.0.2 (user decision 2026-11-11): the end-user OSC reference, and with it the "documented" part of NET-01.
- **DST-02** has no release (user decision 2026-11-13): no update check in v0.0.1 or v0.0.2.

### v0.0.2 — audio parity, then basic video
Scope decided by the user on 2026-11-13; the work is the open issues of the `v0.0.2` milestone. Audio and editing parity with QLab 5 comes first, then video.
- **Audio and editing:** a device per output patch with several devices open at once (AUD-02, AUD-21) and the patch editor; the cue-editing keys and key-map editor (TRG-07); editing several selected cues and Paste Cue Properties (ED-06); auto-load, skip if disarmed, second colour, trigger enables, fade-and-stop others and duck others; a cue list in its own window and the row size (CL-07, UI-03); workspace settings as categories and one status window; the end-user OSC reference (DOC-02); Find Missing Files; the manual device-loss run (NFR-08).
- **Video:** Video cues (video files and stills), stages each sent to a chosen display, geometry, opacity and geometry fades (CT-10 video part), the video's audio through the audio engine, black when idle, a stage monitor window.
- **VID-01 / VID-02** a stage is one region on one display; stages spanning several displays, masks and warping are later.
- **VID-04** no blend modes (they sit with video effects, VID-10).
- **VID-07** no playback rate (with AUD-13, v0.0.3).
- **Not in v0.0.2:** Camera and Text cues, NDI, Syphon, video effects, edge blending; cue templates, carts, Mic cues, MIDI triggers, the group timeline view and playback rate are v0.0.3.
