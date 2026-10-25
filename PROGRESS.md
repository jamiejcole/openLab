# openLab — Progress

Current release: **v0.0.1 — basic playback rehearsal (audio only, macOS)**. Scope: requirements tagged `v0.0.1` in [`REQUIREMENTS.md`](REQUIREMENTS.md), limits in §21.

How to use: tasks are ticked by whoever completes them, only when implemented **and tested**. Requirement IDs in brackets link each task to `REQUIREMENTS.md`. New work discovered along the way is added as an unticked task with a note.

**Dependency order:** M0 → (M1 → M2) in parallel with M3 → M4 → M5 → M6. M5.1–M5.2 may start once the protocol (M4.1) is defined.


## In flight (orchestrator handoff — updated 2026-10-25)
If the session was cut off, resume with the `orchestrate` skill. Both branches below have review findings being fixed; they must be re-verified (and re-reviewed if they had Highs) before merging to `main`.

- **M1-A** (M1.1, M1.2, M1.3, M1.9) — branch `worktree-agent-af082c68c54c05df6` (worktree `.claude/worktrees/agent-af082c68c54c05df6`). Review: 1 High, 4 Medium. Fixes requested:
  - H1 non-finite OSC float makes a loadable show unsaveable → repair to 0.0; audit all floats; regression + fuzz corpus.
  - M1 Start/GoTo(then_go) cycles and ancestor-targeting → validation warning; note runtime bound in M2.5.
  - M2 add to v1: OutputPatch per-output level/mute/delay/polarity + patch master; crosspoint/cue-output mute; settings: global master, transport hotkey map, auto-number start/increment. CT-30 raw UDP/TCP strings OUT of v0.0.1 (§21).
  - M3 bounds: times ≤ 24 h, channel_count 1..=64, autosave 5 s–1 h, min_go 0–10 s, panic 0–60 s, slices sorted/deduped/in-range, indices in range.
  - M4 untick WS-01. Lows: Windows-shaped-only path conversion, fuzz dict/artifacts/pinned nightly, lint fuzz crate, doc fixes, M1.4 saves on writer thread.
- **M3-A** (M3.1, M3.2, M3.4, M3.5 + M3.11) — branch `worktree-agent-a2975cec37287bb34` (worktree `.claude/worktrees/agent-a2975cec37287bb34`). Review: 1 High, 5 Medium. Fixes requested:
  - H1 start/routing race → StartVoice carries full initial routing + patch index (boxed, returned via return queue); never unity by default; gang/multi-commands atomic. Fold in M3.11 per-voice patch.
  - M1 O(log n) pending queue ordered by (at, seq). M2 controller scheduling horizon (default 500 ms, `pump(now)`), pending-full counter/event. M3 delay retarget crossfade click. M4 gain ceiling +24 dB. M5 missing error-path/no_alloc tests. All Lows.
  - Output level/mute/polarity move AFTER the delay line (instant operator mute).
- Merging both will conflict on `engine/Cargo.lock` (regenerate) and possibly PROGRESS.md.
- Next after merge: M1-B (M1.5, M1.6, M1.10 commands/undo), M1-C (M1.4, M1.7, M1.8, M1.11 persistence I/O), M3-B (M3.3 decode/streaming, M3.6 loops), then M2.
- Process notes: agent hand-back delivery can fail — briefs give a report path (`<scratchpad>/reports/<batch>.md`); `.claude/settings.local.json` holds the Bash allowlist (local only).

---

## M0 — Foundations
- [x] M0.1 Cargo workspace with `core`, `audio`, `engine` crates; lints per ARCHITECTURE §10; SPDX headers [NFR-41, NFR-42]
- [x] M0.2 `mac/` XcodeGen project (AppKit app, unit + UI test targets, macOS 12.0 target), engine build phase producing a universal binary [NFR-30]
- [x] M0.3 Makefile (`lint`, `test`, `test-rust`, `mac`, `dmg`) [NFR-43]
- [x] M0.4 CI: Rust fmt/clippy/test on macOS arm64, macOS Intel, Windows; `cargo deny`; mac build + tests; pinned Xcode [QA-07, NFR-31, NFR-42]
- [x] M0.5 LICENSE (GPLv3), `.gitignore`, README with build instructions [NFR-42, NFR-43]

## M1 — Core model & show file (`openlab-core`, persistence in `engine`)
- [ ] M1.1 Workspace / cue list / cue model with all v0.0.1 cue kinds, stable UUIDs, targets by id [WS-01, CL-01, CUE-01, CUE-02, CUE-03, CUE-07, CUE-08]
- [ ] M1.2 Validation of loaded models (unique ids/numbers, valid targets, ranges) with structured warnings [NFR-03, CUE-06]
- [ ] M1.3 JSON show file: serialise/deserialise, `format_version`, migration chain, fixtures, `docs/file-format.md` [NFR-40, NFR-32, DOC-03]
- [ ] M1.4 Atomic save, rolling backups, autosave/recovery files [NFR-04, NFR-05, NFR-06]
- [ ] M1.5 Command model with inverses, transactions, undo/redo stacks [ED-07]
- [ ] M1.6 Editing commands: add/remove/move/reparent cues, set properties, batch set, paste, renumber, auto-numbering [ED-03, ED-04, ED-05, ED-08]
- [ ] M1.7 Media path handling: relative/absolute, cross-platform, relink (single + find-in-folder) [WS-03, NFR-32]
- [ ] M1.8 Per-machine settings + logical patch → device mapping [WS-06, WS-07]
- [ ] M1.9 Fuzz target: show-file loader [QA-04]

## M2 — Cue engine (pure logic, virtual clock)
- [ ] M2.1 Cue lifecycle state machine with all states and transitions [CUE-04, CUE-05]
- [ ] M2.2 Pre-wait / action / post-wait timing and continue modes (do-not-continue, auto-continue, auto-follow) [CUE-02, CUE-03]
- [ ] M2.3 Playhead, GO, double-GO protection, playhead navigation [PB-01, PB-02, PB-05, CL-06]
- [ ] M2.4 Group cues: timeline, start-first-and-enter, start-first-and-go-to-next, playlist (loop/shuffle), random [CT-22]
- [ ] M2.5 Control cues: Start, Stop, Pause, Load, Reset, Devamp, GoTo, Arm, Disarm, Wait, Memo [CT-11..CT-21]
- [ ] M2.6 Fade cue logic (audio levels; stop-target-when-done) [CT-10]
- [ ] M2.7 Panic (fade all, second press = hard stop), Pause All / Resume All [PB-03, PB-04]
- [ ] M2.8 Broken-cue detection (missing media, missing device, invalid target) [CUE-06, NFR-07]
- [ ] M2.9 Property tests over random workspaces + input sequences [QA-01, QA-02]

## M3 — Audio engine (`openlab-audio`)
- [ ] M3.1 Offline render driver + RT-safety harness (`assert_no_alloc`, `catch_unwind` in callback) [NFR-02, QA-03]
- [ ] M3.2 Mixer: voice pool, time-stamped commands, sample-accurate starts/stops [NFR-21, NFR-22]
- [ ] M3.3 Decode (WAV, AIFF, FLAC, MP3, AAC; OGG optional) + resample + disk streaming threads + priming + preload-to-RAM [AUD-04, AUD-05]
- [ ] M3.4 Levels, routing matrix, ganging, output patches (level/mute/delay/polarity), masters [AUD-06, AUD-07, AUD-08, PB-11]
- [ ] M3.5 Fade ramps + curves (linear, equal-power, S, log, exp); integrated fade in/out; trims [AUD-09, AUD-10, AUD-11]
- [ ] M3.6 Slices, loops (N / infinite), devamp [AUD-12, CT-16]
- [ ] M3.7 Meters + xrun counter [AUD-19, NFR-27]
- [ ] M3.8 Device layer (cpal): device list, sample rate / buffer size, up to 64 outputs, hot-unplug/replug recovery [AUD-01, AUD-03, AUD-21, NFR-08]
- [ ] M3.9 Waveform overview data (peaks at multiple zoom levels, computed off the audio thread) [AUD-14]
- [ ] M3.10 CPU budget test: 64 stereo voices with fades ≥ 10× real time on the Intel CI runner [NFR-23, QA-06]

## M4 — Engine service (`openlab-engine`)
- [ ] M4.1 Protocol types in core, golden fixtures, `docs/protocol.md` [NFR-41]
- [ ] M4.2 Control thread: wires CueEngine ↔ mixer ↔ disk threads; scheduling per ARCHITECTURE §3.2 [NFR-20, NFR-21, NFR-22, NFR-11]
- [ ] M4.3 WebSocket server: token auth, snapshot, model_changed/runtime/progress/meters, revision gaps [NFR-01]
- [ ] M4.4 Workspace requests: new/open/save/save-as/revert, recent files, recovery prompt flow [WS-02, WS-08, NFR-05]
- [ ] M4.5 OSC server: UDP+TCP 53000, QLab-compatible v0.0.1 subset, passcode, updates; `docs/osc.md` [NET-01, NET-02, NET-05, TRG-03, DOC-02]
- [ ] M4.6 Network (OSC send) cue + OSC output override [CT-30, PB-10]
- [ ] M4.7 Show log to disk, status (CPU, engine load, disk, xruns, devices), local crash capture [DX-02, DX-03, DX-04]
- [ ] M4.8 Pre-show check + warnings feed [SM-06, DX-01, NFR-07]
- [ ] M4.9 Lifecycle: `engine.json`, UI relaunch on crash, graceful shutdown, sleep prevention in Show Mode [NFR-01, SM-07]
- [ ] M4.10 Integration tests over real sockets; fuzz targets for OSC + protocol [QA-04]
- [ ] M4.11 Accelerated 24 h soak test [NFR-09, QA-05]
- [ ] M4.12 Performance: 2,000-cue workspace load < 3 s; idle CPU budget [NFR-24, NFR-25]

## M5 — macOS app (Swift + AppKit)
- [ ] M5.1 `EngineClient` (connect, reconnect, codec) + `WorkspaceStore`; fixture contract tests [NFR-01]
- [ ] M5.2 Engine lifecycle in app: spawn/attach, reconnect after restart, engine-crash alarm + restore [NFR-01, NFR-05]
- [ ] M5.3 Main window shell: QLab-style layout (toolbar, GO/standby area, sidebar, cue list, inspector), dark appearance [UI-07, UI-08, SM-04, CL-06]
- [ ] M5.4 Cue list `NSOutlineView`: columns, groups, colours, flags, state icons (not colour-only), 2,000-cue performance [CL-02, CL-03, ED-13, UI-04, NFR-24]
- [ ] M5.5 Sidebar: cue lists, Active Cues panel with per-cue controls, wall clock [CL-01, CL-05, PB-07, PB-12]
- [ ] M5.6 Inspector: basics (number, name, notes, colour, timings, continue mode, target) [ED-01, CUE-01..CUE-03]
- [ ] M5.7 Inspector: triggers (hotkey, OSC) [CUE-09, TRG-01, TRG-03]
- [ ] M5.8 Inspector: time & loops with waveform editor (trims, fades, slices) [AUD-10, AUD-11, AUD-12, AUD-14]
- [ ] M5.9 Inspector: audio levels + routing matrix; Fade cue settings [AUD-06, AUD-08, CT-10]
- [ ] M5.10 Editing: drag-drop files, reorder, drop-to-target, multi-select batch edit, copy/paste, undo/redo, renumber, find, fade-this-out, keyboard operation [ED-02..ED-05, ED-07..ED-10, ED-12]
- [ ] M5.11 Transport: GO/Stop/Panic/Pause/Resume, remappable hotkeys [PB-01..PB-05, TRG-01]
- [ ] M5.12 Show Mode, quit confirmation [SM-01, SM-03, SM-05]
- [ ] M5.13 Preferences: audio device, sample rate/buffer, autosave, backups, GO protection, panic time; patch editor + device mapping [UI-01, AUD-21, AUD-07, WS-07]
- [ ] M5.14 Warnings panel, log view, status display, relink missing media [DX-01, DX-02, DX-03, WS-03]
- [ ] M5.15 UI scaling, `Localizable.strings`, VoiceOver basics on list + transport [UI-03, UI-06]
- [ ] M5.16 XCUITest end-to-end flows against a real headless engine [QA-01]
- [ ] M5.17 Reopen main window after close (Dock click / Window menu) [NFR-01 operator safety] — from M0 review (L6)

## M6 — Release v0.0.1
- [ ] M6.1 Universal `.app` with embedded engine, ad-hoc signed, `.dmg` script, version stamping [DST-01, DST-03, NFR-30]
- [ ] M6.2 Docs: getting started, contributor guide; verify file-format/protocol/OSC docs complete [DOC-01..DOC-04]
- [ ] M6.3 Release checklist run: full test suite, soak, perf budgets, manual smoke test of a sample show
- [ ] M6.4 Tick all v0.0.1 requirements in `REQUIREMENTS.md`; tag `v0.0.1`
