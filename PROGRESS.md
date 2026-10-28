# openLab — Progress

Current release: **v0.0.1, basic playback rehearsal (audio only, macOS)**. Scope: `v0.0.1` requirements in [`REQUIREMENTS.md`](REQUIREMENTS.md), §21.

Tick a task only when implemented **and tested**. Bracketed IDs are requirements in `REQUIREMENTS.md`. Add new work as an unticked one-line task.
**Keep task lines short** (id, title, requirement IDs, key dependency): notes appended to a line conflict with every other open PR.
Notes and decisions go in the PR, the task's docs or `docs/handoff/`.

**Dependency order:** M0 → (M1 → M2) in parallel with M3 → M4 → M5 → M6. M5.1–M5.2 may start once the protocol (M4.1) is defined.

## Status
- Next up, in order: M4.15e (in review), M4.15f, M4.15g (+ M4.4d/e, M4.9), M4.15h, M4.15i, M4.10a, M4.4g, M4.2e, M1.17.
- Low and low-medium review findings are GitHub issues labelled `low`, not PROGRESS lines. Niche QLab-parity features are deferred to v0.0.2 (last section).
- Open QLab questions: `docs/todo/open-qlab-questions.md`.
- Full history, notes and decisions for every task (as of 2026-10-28): `docs/handoff/progress-archive.md`.

## M0 — Foundations
- [x] M0.1 Cargo workspace with `core`, `audio`, `engine` crates; lints per ARCHITECTURE §10; SPDX headers [NFR-41, NFR-42]
- [x] M0.2 `mac/` XcodeGen project (AppKit, unit + UI tests, macOS 12.0), universal engine build phase [NFR-30]
- [x] M0.3 Makefile (`lint`, `test`, `test-rust`, `mac`, `dmg`) [NFR-43]
- [x] M0.4 CI: Rust fmt/clippy/test on macOS arm64/Intel and Windows, `cargo deny`, mac build + tests [QA-07, NFR-31, NFR-42]
- [x] M0.5 LICENSE (GPLv3), `.gitignore`, README with build instructions [NFR-42, NFR-43]

## M1 — Core model & show file (`openlab-core`, persistence in `engine`)
- [x] M1.1 Workspace / cue list / cue model, all v0.0.1 cue kinds, stable UUIDs [WS-01, CL-01, CUE-01, CUE-02, CUE-03, CUE-07, CUE-08]
- [x] M1.2 Validation of loaded models (unique ids/numbers, valid targets, ranges) with structured warnings [NFR-03, CUE-06]
- [x] M1.3 JSON show file: serialise, `format_version`, migrations, fixtures, docs [NFR-40, NFR-32, DOC-03]
- [x] M1.4 Atomic save, rolling backups, autosave/recovery files [NFR-04, NFR-05, NFR-06]
- [x] M1.5 Command model with inverses, transactions, undo/redo stacks [ED-07]
- [x] M1.6 Editing commands (add/remove/move/reparent, properties, batch set, paste, renumber) [ED-03, ED-04, ED-05, ED-08]
- [x] M1.7 Media paths: relative/absolute, cross-platform, relink [WS-03, NFR-32]
- [x] M1.8 Per-machine settings + logical patch → device mapping [WS-06, WS-07]
- [x] M1.9 Fuzz target [QA-04]
- [x] M1.10 Editing commands uphold model invariants [NFR-03]
- [x] M1.12 Commands for the rest of the model so undo covers it [ED-07]
- [x] M1.11 Unicode normalisation (NFC/NFD) and case-insensitive comparison of media paths across macOS/Windows [NFR-32]
- [x] M1.13 Recovery scan for a show file that cannot be loaded [NFR-05]
- [x] M1.14 Per-machine settings: where UI-owned parts (window layout, recent files) live [WS-06]
- [x] M1.15 Windows `write_atomic` bounded retry on transient rename failure [NFR-04, NFR-31]
- [x] M1.16 M1-C review Lows: preserve metadata/symlinks on atomic replace, etc.
- [ ] M1.17 Run `relink` find-in-folder on the document worker; surface `truncated` (PR #27) [WS-03, NFR-04]
- [x] M1.19 `Workspace::apply` Transaction rollback snapshot without cloning the workspace
- [x] M1.20 Rename the workspace (`SetWorkspaceName`) and name length/character rules
- [x] M1.21 Engine: on `RollbackFailed` log, mark dirty and resend the whole model

## M2 — Cue engine (pure logic, virtual clock)
- [x] M2.1 Cue lifecycle state machine with all states and transitions [CUE-04, CUE-05]
- [x] M2.2 Pre-wait / action / post-wait timing and continue modes [CUE-02, CUE-03]
- [x] M2.3 Playhead, GO, double-GO protection, playhead navigation [PB-01, PB-02, PB-05, CL-06]
- [x] M2.4 Group cues: timeline, start-first-and-enter, start-first-and-go-to-next, playlist, random [CT-22]
- [x] M2.5 Control cues: Start, Stop, Pause, Load, Reset, Devamp, GoTo, Arm, Disarm, Wait, Memo [CT-11..CT-21]
- [x] M2.6 Fade cue logic (audio levels; stop-target-when-done) [CT-10]
- [x] M2.7 Panic (fade all, second press = hard stop), Pause All / Resume All [PB-03, PB-04]
- [x] M2.8 Broken-cue detection (missing media, missing device, invalid target) [CUE-06, NFR-07]
- [x] M2.9 Property tests over random workspaces + input sequences [QA-01, QA-02]
- [x] M2.10 Preview/audition operation on a cue (CUE-05 remainder; PB-09)
- [x] M2.11 Playhead inside groups and jump-by-number to grouped cues (with M2.4)
- [x] M2.12 Cue engine cleanups from the M2-A review (Lows)
- [x] M2.13 Second trigger on an active cue is ignored; a broken cue never auto-follows
- [x] M2.14 Per-cue second-trigger options (QLab 5)
- [x] M2.15 Control cues on groups (with M2.5)
- [x] M2.16 Confirm group details against QLab 5 (M2-B open questions)
- [x] M2.17 Confirm control/Fade details against QLab 5 (M2-C open questions)
- [x] M2.18 Confirm M2-D details against QLab 5
- [x] M2.20 Confirm preview/audition details against QLab 5 (M2-E open questions)
- [x] M2.19 Static problems recomputed incrementally, not on every `handle`
- [x] M2.22 Property suite: tighten `waits_for_the_operator` with `has_runtime(cue)`
- [x] M2.23 Property sweep: mixer held a RunId the engine forgot
- [x] M2.24 Match QLab 5 where the QLab checks (answers 2026-10-27) showed openLab differs
- [x] M2.25 (R4) Notice missing media without waiting for playback (watch thread)
  - [x] M2.25b A permanently dead volume must not starve probes of other volumes
- [x] M2.26 (F4) A Fade cue with nothing to fade is broken (`nothing_to_fade`)
- [x] M2.28 Match QLab 5, second batch of QLab checks
- [x] M2.31 A stream failure during a Panic/stop fade must not start another cue
- [x] M2.32 Group preview with duplicate ids previewed a non-audio cue
- [x] M2.33 An end report dated before a Pause All must not start follow-on cues while paused
- [x] M2.34 A voice ending on the Pause All sample ends the cue, not the pause

## M3 — Audio engine (`openlab-audio`)
- [x] M3.1 Offline render driver + RT-safety harness (`assert_no_alloc`, `catch_unwind` in callback) [NFR-02, QA-03]
- [x] M3.2 Mixer: voice pool, time-stamped commands, sample-accurate starts/stops [NFR-21, NFR-22]
- [x] M3.3 Decode (WAV, AIFF, FLAC, MP3, AAC), resample, disk streaming threads, priming [AUD-04, AUD-05]
- [x] M3.4 Levels, routing matrix, ganging, output patches (level/mute/delay/polarity), masters [AUD-06, AUD-07, AUD-08, PB-11]
- [x] M3.5 Fade ramps + curves (linear, equal-power, S, log, exp); integrated fade in/out; trims [AUD-09, AUD-10, AUD-11]
- [x] M3.6 Slices, loops (N / infinite), devamp [AUD-12, CT-16]
- [x] M3.7 Meters + xrun counter [AUD-19, NFR-27]
- [x] M3.8 Device layer (cpal): device list, rate/buffer, up to 64 outputs, hot-unplug recovery [AUD-01, AUD-03, AUD-21, NFR-08]
- [x] M3.9 Waveform overview data (peaks at multiple zoom levels, computed off the audio thread) [AUD-14]
- [x] M3.10 CPU budget test [NFR-23, QA-06]
- [x] M3.19 CPU margin on the Intel runner [NFR-23, QA-06]
- [x] M3.11 Per-voice output patch selection [AUD-07]
- [x] M3.13 Loop cache [AUD-12, NFR-23]
- [x] M3.14 Trim AAC encoder priming in MP4 (edit list / iTunSMPB) [AUD-04]
- [x] M3.15 Fuzz target [QA-04]
- [x] M3.16 symphonia RIFF/AIFF `u16` overflow on absurd headers (decode fuzz) [QA-04]
- [x] M3.17 Minimum loop length [AUD-12]
- [x] M3.18 Voice pause/resume in the mixer (`CommandKind` has no pause) [CUE-05]
- [x] M3.23 64-channel media decode [AUD-03]
- [x] M3.24 Confirm against QLab 5: devamp during a non-looping slice [CT-16]
- [x] M3.12 Wrap the device callback in `assert_no_alloc` in debug builds; panic in callback [NFR-02]
- [x] M3.20 Device-layer hardware check moved to M6.5
- [x] M3.21 Engine binary installs `assert_no_alloc::AllocDisabler` in debug builds [NFR-02]
- [x] M3.22 Device-loss policy (superseded by M4.2g) [NFR-08]
- [x] M3.25 Flaky `devamp_from_a_full_ring_never_starves_at_the_exit` under CPU load [QA-03]
- [x] M3.26 Decode fuzz out-of-memory: 1 Hz WAV sized a huge resampler chunk [QA-04, AUD-04]
- [x] M3.27 Fuzz: symphonia RIFF extensible-fmt shift overflow
- [x] M3.28 Fuzz: non-WAVE RIFF start hid an overflowing WAV header from the pre-check

## M4 — Engine service (`openlab-engine`)
- [x] M4.1 Protocol types in core, golden fixtures, `docs/protocol.md` [NFR-41]
- [x] M4.1a Compute `model_changed` upserts/deletes in the control thread
- [x] M4.1c M4.1a review Lows (empty Transaction, etc.)
- [x] M4.1b Stop All vs Panic: no `stop_all` action (QLab check P6)
- [x] M4.1d Panic Selected: fade out only the selected cues (QLab check P6/Q7)
- [x] M4.2 Control thread: wires CueEngine, mixer and disk threads (ARCH §3.2) [NFR-20, NFR-21, NFR-22, NFR-11]
  - [x] M4.2a Control loop (`engine/src/control/`, docs/control.md) [NFR-20, NFR-21, NFR-22, NFR-11, NFR-02]
  - [x] M4.2b Devices: list, hot-plug, loss and reopen
  - [x] M4.2g Device loss follows QLab: cues on a lost device stop at once (X1)
  - [x] M4.2g Device watchdog: a stream that stops calling back without an error
  - [x] M4.2j Control loop must not spin while the sample clock stands still
  - [x] M4.2c Auto-follow after an audio cue was up to two blocks late
  - [x] M4.2f Patch add/remove/reorder must keep mixer matrix slots stable
  - [ ] M4.2e Progress for cues without a voice (pre-wait, Wait, post-wait countdown)
- [x] M4.3 WebSocket server [NFR-01]
- [x] M4.4 Workspace requests [WS-02, WS-08, NFR-05]
  - [x] M4.4a open/new/revert that resets revision must resync every client
  - [x] M4.4c Map a new workspace default patch to the device first two outputs [WS-07]
  - [ ] M4.4g Request to set/clear a patch mapping on this machine (after M4.4c)
  - [x] M4.4h Flake shared with M4.7d (fixed by M2.28)
  - [x] M4.4i Flaky `control_the_control_loop_never_waits_for_the_disk` (BrokenPipe on FIFO)
  - [ ] M4.4d Final recovery write on shutdown with unsaved changes (absorbed by M4.15g)
  - [ ] M4.4e `host::run_show` skips the recovery check (absorbed by M4.15g)
- [x] M4.4f Close + no-workspace state (superseded by M4.15f/g)
- [x] M4.4i Flaky `control_media` FIFO test
  - [x] M4.4j Flaky stalled-tail hard stop / reset tests under load
  - [x] M4.4k `persist/backup.rs` must check the show path is a regular file
- [x] M4.5 OSC server [NET-01, NET-02, NET-05, TRG-03, DOC-02]
  - [x] M4.5a OSC listeners (UDP+TCP 53000, replies to 53001, updates fan-out)
  - [x] M4.5b OSC layer matches QLab 5.5.3 measured behaviour [NET-01, NET-02, NET-05]
  - [x] M4.5c OSC passcode rights (View / Edit / Control; show-file change)
  - [x] M4.5d Known-address lockout exemption (M4.5a) [NET-02]
- [x] M4.6 Network (OSC send) cue + OSC output override [CT-30, PB-10]
  - [x] M4.6a Edit a Network cue [CT-30, ED-07]
  - [x] M4.6b QLab check: TCP framing for Network cue sends (SLIP vs length-prefixed) [CT-30]
  - [ ] M4.6c Remove `EngineWarning::Unsupported` / `WireWarning::Unsupported` (protocol cleanup)
- [x] M4.7 Show log to disk, status (CPU, engine load, disk, xruns, devices), local crash capture [DX-02, DX-03, DX-04]
  - [x] M4.7a `engine_load` in `status` (DX-03)
  - [x] M4.7b Crash capture beyond panics (DX-04)
  - [x] M4.7c Live show-log view (DX-02)
  - [x] M4.7d Flaky `save_robust` test (fixed by M2.28 `create_tmp`)
  - [x] M4.7e Run-marker crash-report test no longer flakes on a held lock
- [x] M4.8 Pre-show check + warnings feed [SM-06, DX-01, NFR-07]
- [ ] M4.9 Lifecycle: `engine.json`, UI relaunch on crash, graceful shutdown, sleep prevention (with M4.15g) [NFR-01, SM-07]
- [x] M4.10 Integration tests over real sockets; fuzz targets for OSC + protocol [QA-04]
  - [x] M4.10a Document requests in socket session tests and the `protocol` fuzz body
- [x] M4.11 Accelerated 24 h soak test [NFR-09, QA-05]
  - [x] M4.11a Shorter real-time soak, `#[ignore]`, nightly
  - [x] M4.11b Decide `Load` with `at` past the cue length
- [x] M4.13 Wire previews (CUE-05)
  - [x] M4.11c (moot) Post-wait of a cue loaded past its end
- [x] M4.12 Performance [NFR-24, NFR-25]
- [x] M4.12b Switching `preload` to true on a probed file triggers a preload
- [x] M4.12a Incremental per-edit problem rebuild and `MediaCatalog::sync`
- [x] M4.12c Remaining per-edit work that grows with the show (callgrind at 2,000 cues)
- [x] M4.12g `BatchSet` without cloning the whole workspace
- [x] M4.2h M4.2g review Lows
- [x] M4.2i Flaky `control_feedback::devamp_leaves_the_loop_and_the_cue_ends`
- [x] M4.2k Device tests check `device_down()` every idle tick (race)
- [x] M4.14 Send `Input::StartFailed` instead of `ActionFailed` before any audio
- [x] M4.16 `voices.rs` `finished`: an end on the pause sample is an end
- [ ] M4.15 Several workspaces open at once, one engine process (sub-tasks a to j)
  - [x] M4.15a Docs for multi-workspace
  - [x] M4.15b (M) Mixer sharing prep, still one workspace
  - [x] M4.15c (S) `SaveWriter`: one pending autosave per recovery target
  - [x] M4.15d (L) Split `Control` into shared `Engine` + `Doc`, one doc
  - [x] M4.15d1 (S-M; with M4.15e) Per-doc probe pool, `MediaWatch`, `NetOut`, `Observe` states
  - [x] M4.15e (L; in review) N docs through a test API: DocId, routing, close, cap of 8
  - [x] M4.15e1 Engine-wide preload budget across docs [AUD-05]
  - [x] M4.15f (L; after M4.15e) Protocol and hub: bound connections, `workspaces`, `opened`/`closed`, `close`
  - [ ] M4.15g (M; after M4.15f) Engine starts with no docs, final recovery write for every dirty doc
  - [ ] M4.15h (M-L; after M4.5b, M4.5c) OSC over several docs: one listener, mirror per doc, routing
  - [ ] M4.15i (S; after M4.15h) Perf gate: two 2,000-cue docs

## M5 — macOS app (Swift + AppKit)
- [x] M5.1 `EngineClient` (connect, reconnect, codec) + `WorkspaceStore`; fixture contract tests [NFR-01]
  - [x] M5.1a `EngineClient` constraints from the server (docs/network.md)
- [ ] M5.2 Engine lifecycle in app: spawn/attach, reconnect, crash alarm + restore (M4.15: rebind each window) [NFR-01, NFR-05]
- [x] M5.3 Main window shell [UI-07, UI-08, SM-04, CL-06]
- [x] M5.4 Cue list `NSOutlineView` [CL-02, CL-03, ED-13, UI-04, NFR-24]
  - [x] Measure `WorkspaceStore` apply cost with 2,000 cues
  - [ ] Decode snapshots off the main actor (`EngineClient`)
  - [ ] Cue list follow-ups: configurable columns, filter/highlight, duration column [ED-13]
- [x] M5.5 Sidebar [CL-01, CL-05, PB-07, PB-12]
  - [ ] M5.5a Sidebar New List / New Cart buttons (need cue-list create commands)
- [x] M5.6 Inspector: basics (number, name, notes, colour, timings, continue mode, target) [ED-01, CUE-01..CUE-03]
- [x] M5.7 Inspector: triggers (hotkey, OSC); hotkey conflict precedence [CUE-09, TRG-01, TRG-03]
  - [ ] M5.7a "Hotkey used by workspace" warning for transport-key hotkeys [TRG-01]
- [ ] M5.8 Inspector: time & loops with waveform editor (trims, fades, slices) [AUD-10, AUD-11, AUD-12, AUD-14]
- [ ] M5.9 Inspector: audio levels + routing matrix; Fade cue settings [AUD-06, AUD-08, CT-10]
- [ ] M5.10 Editing: drag-drop, reorder, batch edit, copy/paste, undo/redo, renumber, find, keyboard [ED-02..ED-05, ED-07..ED-10, ED-12]
  - [x] M5.10b Editing part 2: Finder drops, copy/cut/paste across workspaces
  - [ ] M5.10c Editing part 3: batch edit, find, fade-this-out, drag to list, Network cue creation [ED-02..ED-05, ED-07..ED-10, ED-12]
- [ ] M5.11 Transport: GO/Stop/Panic/Pause/Resume, remappable hotkeys [PB-01..PB-05, TRG-01]
  - [ ] M5.11a Key-map editor UI (after M5.11c) [TRG-01]
  - [x] M5.11b Stop All sends the engine `stop_all` for the key window workspace
  - [ ] M5.11c Engine command to edit `settings.hotkeys` + selected-cue key fields
  - [ ] M5.11e Hotkey matching on non-US layouts and shifted punctuation
  - [ ] M5.11d Panic Selected / Panic All double-tap; Reset All [PB-03]
- [ ] M5.12 Show Mode, quit confirmation (partial) [SM-01, SM-03, SM-05]
- [ ] M5.12b Show Mode enlarged GO/standby display, high contrast [SM-01, SM-03]
- [ ] M5.13a OSC passcode table UI (after M4.5c) [NET-02]
- [ ] M5.13 Preferences: audio device, rate/buffer, autosave, backups, panic time; patch editor [UI-01, AUD-21, AUD-07, WS-07]
- [x] M5.14 Warnings panel, log view, status display, relink missing media [DX-01, DX-02, DX-03, WS-03]
  - [ ] M5.14a Live show log: follow `log_entry` pushes (from M4.7c)
  - [ ] M5.14b Crash reports in the UI: reveal crash folder (from M4.7b)
- [ ] M5.15 UI scaling, `Localizable.strings`, VoiceOver basics on list + transport [UI-03, UI-06]
- [ ] M5.16 XCUITest end-to-end flows against a real headless engine [QA-01]
- [ ] M5.19 Windows for several workspaces (M4.15; partial) [SM-05]
- [ ] M5.20 Launcher Connect and Templates tabs (no backend yet)
- [x] M5.18 Preview/audition button and key (inspector, Time & Loops tab and cue menu) [CUE-05]
- [x] M5.17 Reopen main window after close (Dock click / Window menu)
- [ ] M5.21 Output override indicator and switch [PB-10]

## M6 — Release v0.0.1
- [ ] M6.1 Universal `.app`, embedded engine, ad-hoc signed, `.dmg` script, version stamping, app icon [DST-01, DST-03, NFR-30]
- [ ] M6.2 Docs [DOC-01..DOC-04]
- [ ] M6.3 Release checklist run
- [ ] M6.4 Tick all v0.0.1 requirements in `REQUIREMENTS.md`; tag `v0.0.1`
- [ ] M6.5 Manual hardware check of device loss (CoreAudio, WASAPI) [NFR-08]
- [ ] M6.6 (user decision) Opt-in crash report sending, off by default [DX-04]

## Deferred to v0.0.2 (niche QLab parity; user decision 2026-10-28)
- [ ] M2.21 PB-09 audition output: workspace setting for the audition patch/device
- [ ] M2.27 Confirm against QLab 5 the open M2.24 points (playlist child break, trim floor)
- [ ] M2.30 Whole-cue play count besides the slices' play count
- [ ] M2.29 (R1/R3/R4) Follow media across rename/move; relink when a missing file returns
  - [x] M2.29a Docs for the design
  - [x] M2.25a Watch thread: `watch.rs`, pass rules, catalog `key_changes` + `mark_missing`
  - [ ] M2.29b Follow (macOS): `fileid.rs`, `in_trash`, locate, `rekey`, model edit outside undo (after M2.25a)
  - [x] M2.29c Cue display name follows its file (R2)
  - [ ] M2.29d Show file v5 `media_hints`, volume UUID, hint table (R5)
  - [ ] M2.29e Mac UI: File > Find Missing Files... (M5 agent; after M1.17)
  - [ ] M2.29f (later) Windows file-ID follow
  - [ ] M2.29g (later) Follow the show file itself when moved or renamed
- [ ] M4.15j (S-M) Per-workspace OSC listen port (show-file change)
