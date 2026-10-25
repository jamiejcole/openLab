# openLab — Progress

Current release: **v0.0.1 — basic playback rehearsal (audio only, macOS)**. Scope: requirements tagged `v0.0.1` in [`REQUIREMENTS.md`](REQUIREMENTS.md), limits in §21.

How to use: tasks are ticked by whoever completes them, only when implemented **and tested**. Requirement IDs in brackets link each task to `REQUIREMENTS.md`. New work discovered along the way is added as an unticked task with a note.

**Dependency order:** M0 → (M1 → M2) in parallel with M3 → M4 → M5 → M6. M5.1–M5.2 may start once the protocol (M4.1) is defined.


## In flight (orchestrator handoff — updated 2026-10-25)
If the session was cut off, resume with the `orchestrate` skill (see its Token budget section: Sonnet agents by default, one agent at a time).

- **M1-A** (M1.1–M1.3, M1.9) — MERGED (3 review rounds). Core model, validation/repair (idempotent, linear SCC cycle check), show-file load/save in core.
- **M3-A** — MERGED. Engine-facing API: boxed `VoiceStart` carries full routing; `send_batch` takes (time, command) pairs; control loop must call `MixerController::pump()` every tick (500 ms horizon); never schedule voice starts beyond the next block.
- **M3-B** (M3.3, M3.6, M3.15 decode fuzz, M3.16) — MERGED (Opus impl + review, 3 rounds: H1 devamp starvation, H-R1 damaged-file seek cost; 3 Mediums fixed). User decision: damaged media plays with warnings (`Probed { info, warnings }`), only unplayable files fail. For M2/M4.2 wiring: `probe` fully decodes compressed files (1–2 s per 30-min file) — never call it on the control thread; auto-preload files with `preload_advised()` within budget; workspace-wide preload budget (L-R3).
- **M1-B** (M1.5, M1.6, M1.10) — MERGED (PR #1, 1 review round: 0 High / 0 Medium). Lows for later: no-op edits recorded as undo steps (do with M1.12); commands don't check references by design, so the M2 cue engine must tolerate self-targets/trigger cycles; every transaction clones the workspace as rollback snapshot (revisit with M1.12 coalescing); `Command` wire format needs protocol fixtures + `docs/protocol.md` in M4.1.
- **M1-C** (M1.4, M1.7, M1.8, M1.11) — implemented: pure logic in core (`media_path`, `backup`, `recovery`, `settings`, `relink`), I/O in `engine/src/persist/`; see `docs/persistence.md`. Pending review.
- **M1-C** (M1.4, M1.7, M1.8, M1.11) — MERGED (Sonnet impl + review, 0 High / 4 Medium fixed). Persistence lives in `crates/engine/src/persist/` (docs/persistence.md). Recovery/backups keyed by workspace id + path hash (ARCHITECTURE §5 updated); stale recovery moved aside, never deleted on mtime. For M4.4: `SaveWriter::save(..., revision)` / `autosave(ws, show_path, revision)`; `check_recovery` returns `RecoveryReport { check, other_copies }`. Defaults chosen (user may override): relink never guesses between duplicate names; window layout/recent files stay UI-side (M1.14).
- CI red after M1-C merge (macOS non-UTF-8 test name, Windows-only import and path-separator test assumptions, upstream symphonia MP4 `stts` overflow found by fuzz smoke) — fixed on main; MP4 `stts` pre-check added like M3.16; CI now runs tests with `--no-fail-fast`.
- Next: M2 (cue engine — Opus for the state machine, M2.1–M2.3 first). Low from M1-A review3: SCC tests lack small multi-cycle/tail cases — add with M2.5.
- Process notes: agent hand-back delivery can fail — briefs give a report path (`<scratchpad>/reports/<batch>.md`); `.claude/settings.local.json` holds the Bash allowlist (local only).

---

## M0 — Foundations
- [x] M0.1 Cargo workspace with `core`, `audio`, `engine` crates; lints per ARCHITECTURE §10; SPDX headers [NFR-41, NFR-42]
- [x] M0.2 `mac/` XcodeGen project (AppKit app, unit + UI test targets, macOS 12.0 target), engine build phase producing a universal binary [NFR-30]
- [x] M0.3 Makefile (`lint`, `test`, `test-rust`, `mac`, `dmg`) [NFR-43]
- [x] M0.4 CI: Rust fmt/clippy/test on macOS arm64, macOS Intel, Windows; `cargo deny`; mac build + tests; pinned Xcode [QA-07, NFR-31, NFR-42]
- [x] M0.5 LICENSE (GPLv3), `.gitignore`, README with build instructions [NFR-42, NFR-43]

## M1 — Core model & show file (`openlab-core`, persistence in `engine`)
- [x] M1.1 Workspace / cue list / cue model with all v0.0.1 cue kinds, stable UUIDs, targets by id [WS-01, CL-01, CUE-01, CUE-02, CUE-03, CUE-07, CUE-08]
- [x] M1.2 Validation of loaded models (unique ids/numbers, valid targets, ranges) with structured warnings [NFR-03, CUE-06]
- [x] M1.3 JSON show file: serialise/deserialise, `format_version`, migration chain, fixtures, `docs/file-format.md` [NFR-40, NFR-32, DOC-03]
- [x] M1.4 Atomic save, rolling backups, autosave/recovery files [NFR-04, NFR-05, NFR-06] — saves clone the workspace and run `save_show` on the writer thread, never the control thread (serialise + verify-reload costs up to ~1 s on minimum spec; from M1-A review L6)
- [x] M1.5 Command model with inverses, transactions, undo/redo stacks [ED-07]
- [x] M1.6 Editing commands: add/remove/move/reparent cues, set properties, batch set, paste, renumber, auto-numbering [ED-03, ED-04, ED-05, ED-08]
- [x] M1.7 Media path handling: relative/absolute, cross-platform, relink (single + find-in-folder) [WS-03, NFR-32] — includes converting native Windows relative paths (backslashes) to forward slashes when a path is created; the loader only converts Windows absolute paths (M1-A review L2)
- [x] M1.8 Per-machine settings + logical patch → device mapping [WS-06, WS-07]
- [x] M1.9 Fuzz target: show-file loader [QA-04]
- [x] M1.10 Editing commands uphold model invariants: refuse nesting beyond `MAX_GROUP_DEPTH`, non-finite times/levels and duplicate ids (otherwise `save_show` refuses to write) [NFR-03] — from M1.1–M1.3
- [ ] M1.12 Commands for the rest of the model so undo covers it (ED-07 says "including routing and settings"): workspace settings, output and network patches, cue lists (add/remove/rename/reorder), and fine-grained cue edits that fader drags can coalesce into one undo step [ED-07] — from M1-B; M1.5/M1.6 cover the cue tree and per-cue properties only
- [x] M1.11 Unicode normalisation (NFC/NFD) and case-insensitive comparison of media paths across macOS/Windows [NFR-32] — from M1.3; with M1.7

- [ ] M1.13 Recovery for a show file that cannot be loaded: `check_recovery` needs the workspace id, which comes from the show file. Add a scan of `Recovery/` (list ids, times, names) so a corrupt or deleted show file can still be recovered, and a way to read a backup's workspace id from its folder name. [NFR-05] — from M1-C
- [ ] M1.14 Per-machine settings: the UI-owned parts (window layout, recent files) are not in `MachineSettings`; decide where they live (UI defaults vs. `settings.json`) when M5 needs them. [WS-06] — from M1-C
- [ ] M1.15 Windows: `write_atomic` relies on `rename` replacing an open/locked destination (antivirus or indexer holding the file can make it fail transiently); add a short bounded retry once Windows CI runs the persistence tests. [NFR-04, NFR-31] — from M1-C

- [ ] M1.16 M1-C review Lows left open: (L3) atomic replace swaps the inode, so mode/ownership/xattrs/Finder tags are lost and a symlinked or read-only show file is replaced on Unix — preserve metadata or document; (L4) a backup `.tmp` left by a crash is never rotated and a backwards clock step can make the newest backup rotate out first; (L6) `///x` on Posix parses as a `//` share root and drive-relative `C:foo` is a plain relative name; (L7) `scan_folder` is bounded by depth/count but not time or directory count — run it off the control thread and consider a deadline; crash-leftover `*.tmp` files beside a show are never cleaned — from M1-C review

## M2 — Cue engine (pure logic, virtual clock)
- [x] M2.1 Cue lifecycle state machine with all states and transitions [CUE-04, CUE-05]
- [x] M2.2 Pre-wait / action / post-wait timing and continue modes (do-not-continue, auto-continue, auto-follow) [CUE-02, CUE-03]
- [x] M2.3 Playhead, GO, double-GO protection, playhead navigation [PB-01, PB-02, PB-05, CL-06]
- [ ] M2.4 Group cues: timeline, start-first-and-enter, start-first-and-go-to-next, playlist (loop/shuffle), random [CT-22]
- [ ] M2.5 Control cues: Start, Stop, Pause, Load, Reset, Devamp, GoTo, Arm, Disarm, Wait, Memo [CT-11..CT-21] — CueEngine must still bound chained triggering at runtime (depth/iteration limit): load-time validation warns on Start/GoTo cycles, but edits can create them too (M1-A review M1)
- [ ] M2.6 Fade cue logic (audio levels; stop-target-when-done) [CT-10] — ignore Fade `output_db`/crosspoint indices beyond the target's outputs (validation only bounds them by 64; M1-A review 2 L4)
- [ ] M2.7 Panic (fade all, second press = hard stop), Pause All / Resume All [PB-03, PB-04]
- [ ] M2.8 Broken-cue detection (missing media, missing device, invalid target) [CUE-06, NFR-07]
- [ ] M2.9 Property tests over random workspaces + input sequences [QA-01, QA-02] — a small engine proptest exists (`tests/cue_engine_prop.rs`): extend it, don't duplicate it
- [ ] M2.10 Preview/audition operation on a cue (CUE-05 remainder; PB-09): plays to an audition output without triggering continues — found missing in M2.1
- [ ] M2.11 Playhead inside groups and jump-by-number to grouped cues (with M2.4): M2.3 limits the playhead to top-level cues
- [ ] M2.12 Cue engine cleanups from the M2-A review (Lows):
  - L3: if a trigger cycle in the call's first deadline pass used up the chain budget, a GO's start is dropped with only a `ChainLimit` warning while `last_go` and the playhead still advance. Check the budget before committing the GO, or report `GoIgnored`.
  - L4: `LoadVoice.offset` uses the pre-wait at load time, `StartVoice.offset` the pre-wait at start; editing the pre-wait of a cue loaded to a time makes the primed stream disagree with the start offset. Freeze the pre-wait/action split at load (keep `action_offset` in `Loaded`).
  - L5: every call with an active cue builds a set of all cue ids and recounts the tree for `chain_limit`; fine at expected sizes, skip when the workspace revision is unchanged once revisions exist (M4).
- [ ] M2.13 Decide (user): second trigger on an active cue (restart today; QLab 5 has a per-cue setting) and whether a cue that breaks mid-play keeps its armed continue (cancelled today) — review M2, see the pending-decision note in docs/cue-engine.md

## M3 — Audio engine (`openlab-audio`)
- [x] M3.1 Offline render driver + RT-safety harness (`assert_no_alloc`, `catch_unwind` in callback) [NFR-02, QA-03]
- [x] M3.2 Mixer: voice pool, time-stamped commands, sample-accurate starts/stops [NFR-21, NFR-22]
- [x] M3.3 Decode (WAV, AIFF, FLAC, MP3, AAC; OGG optional) + resample + disk streaming threads + priming + preload-to-RAM [AUD-04, AUD-05] — engine wiring (probe on load, Loaded state) comes with M2
- [x] M3.4 Levels, routing matrix, ganging, output patches (level/mute/delay/polarity), masters [AUD-06, AUD-07, AUD-08, PB-11] — cue outputs beyond the patch's output count are silent (kept in the model)
- [x] M3.5 Fade ramps + curves (linear, equal-power, S, log, exp); integrated fade in/out; trims [AUD-09, AUD-10, AUD-11]
- [x] M3.6 Slices, loops (N / infinite), devamp [AUD-12, CT-16] — mixer and disk-stream side; the Devamp cue itself is M2.5
- [ ] M3.7 Meters + xrun counter [AUD-19, NFR-27]
- [ ] M3.8 Device layer (cpal): device list, sample rate / buffer size, up to 64 outputs, hot-unplug/replug recovery [AUD-01, AUD-03, AUD-21, NFR-08]
- [ ] M3.9 Waveform overview data (peaks at multiple zoom levels, computed off the audio thread) [AUD-14]
- [ ] M3.10 CPU budget test: 64 stereo voices with fades ≥ 10× real time on the Intel CI runner [NFR-23, QA-06]
- [x] M3.11 Per-voice output patch selection: `VoiceStart.patch` picks one of `MixerConfig.patches` cue-output → device-output matrices [AUD-07] — from M3.4, done in the M3-A review fixes
- [ ] M3.13 Loop cache: keep a short loop slice's decoded frames after its first play and unroll later plays from memory, instead of a seek (fresh demuxer + pre-roll, 0.03–1.6 ms) per play. Makes tight loops cheap and looped AAC plays bit-identical (noise substitution differs after each seek) [AUD-12, NFR-23] — from M3-B
- [ ] M3.14 Trim AAC encoder priming in MP4 (edit list / iTunSMPB): symphonia 0.6.1's isomp4 reader ignores it, so AAC cues start ~1000–2000 frames late (end padding is trimmed to the container length since the M3-B review round) [AUD-04] — from M3-B; may need an upstream fix or a small atom reader
- [x] M3.15 Fuzz target: decoders (probe, preload, stream, seek; whatever probes must play as probed) [QA-04] — from M3-B; findings are kept in `crates/audio/tests/fuzz-findings/` and replayed by `cargo test`
- [x] M3.16 symphonia 0.6.1 RIFF/AIFF readers compute `(bits / 8) * channels` in `u16` (`wave/chunks.rs:100`, `common.rs`), which overflows on absurd headers (a panic with overflow checks, as in cargo-fuzz). WAV `fmt ` and AIFF `COMM` channel count (1–64) and bits (≤ 64) are now checked before symphonia sees the file (`Corrupt`); `tests/fuzz-findings/wav-channel-overflow.wav` no longer reaches symphonia. Same class in the MP4 reader: `stts` sums sample counts in `u32` and count x delta in `u64` unchecked (`symphonia-format-isomp4-0.6.1/src/atoms/stts.rs:41`); `mp4_check.rs` walks moov>trak>mdia>minf>stbl (depth/box/entry limits, no allocation from file lengths) and rejects overflowing tables as `Corrupt` before symphonia parses them; `tests/fuzz-findings/mp4-stts-overflow.m4a`. Report both upstream when convenient [QA-04] — from M3-B review M3
- [ ] M3.17 Minimum loop length: `plan()` accepts 1-frame loops, but each play of a looping slice costs a seek (fresh demuxer plus pre-roll, about 8 ms near the end of a 30 min MP3) and one `LoopReached` event, so very short loops in long compressed files underrun and sub-ms loops can fill the event queue. Enforce a minimum (or warn) until M3.13's loop cache [AUD-12] — from M3-B review L3
- [ ] M3.18 Voice pause/resume in the mixer (`CommandKind` has no pause): the cue engine emits `PauseVoice`/`ResumeVoice` with the action offset (CUE-05). Either add a sample-exact pause that keeps the voice and its ring, or have M4.2 stop and restart from `offset` (costs a seek; the engine ignores the stop's end report, and the resumed voice gets a fresh id) — from M2-A
- Note (M3-B review L6): M3.3 is ticked, but AAC in MP4 still plays its encoder priming (M3.14), so AUD-04 is not complete for AAC.
- Cleanup notes from the M3-B re-review (Low, not done):
  - L-R2: loop exit buffers are one per distinct exit (`exit_ms` × channels × 4 B each, up to 255 of them: ~24 MB stereo, ~780 MB at 64 channels), allocated without a fallible check and built before priming. Cap the total, or build lazily for the next few loops.
  - L-R3: `DEFAULT_PRELOAD_BUDGET` is per preload; the engine wiring (M2) needs a workspace-wide preload budget.
  - L-R4: decode fuzz throughput fell (mutated inputs are mostly damaged, and damaged seeks verify checkpoints or decode from the start); consider fewer seeks for damaged inputs in `fuzz_decode`.
  - L-R5: a devamp while a loop exit's buffer is still playing (within `exit_ms` of leaving a loop) is ignored (`DevampIgnored`); e.g. vamp A, a bridge under 250 ms, vamp C.
  - L-R6: a long damaged file classed `Seeking::FromStart` (all damaged AAC) still gaps in its own deep loops/start offsets (other cues unaffected); a 30-min stereo file exceeds the 512 MiB default preload budget. M2 wiring: show `SlowSeeks` prominently on the cue, consider a larger budget when `preload_advised()`.
  - L-R7: a seek-index checkpoint whose 256-frame window is digital silence verifies at any alignment; skip all-zero/near-constant checkpoints when verifying.
- [ ] M3.12 Wrap the device callback in `assert_no_alloc` in debug builds (today a dev-dependency used by the offline-render tests) [NFR-02] — from M3.1, do with M3.8. Also: a panic inside `apply`/`start_voice` while a `Box<VoiceStart>` is a local frees it on the audio thread during unwinding. This happens only on the fault path, which already allocates for the panic payload; it is the same fault-path RT budget (M3-A review 2, L3)

## M4 — Engine service (`openlab-engine`)
- [ ] M4.1 Protocol types in core, golden fixtures, `docs/protocol.md` [NFR-41]
- [ ] M4.2 Control thread: wires CueEngine ↔ mixer ↔ disk threads; scheduling per ARCHITECTURE §3.2 [NFR-20, NFR-21, NFR-22, NFR-11] — see docs/cue-engine.md: set `EngineConfig::lookahead` to one block (window end excluded) and tick when `now + lookahead > next_deadline()`; `RunId` can be the `VoiceId` (each `ResumeVoice` carries a fresh `run` plus the `paused` id it continues; tag later feedback with the fresh one); map `VoiceFinished` → `ActionEnded` and `VoiceRejected`/stream errors → `ActionFailed`; `LoadVoice`/`UnloadVoice` prepare/release streams; the mixer has no pause (M3.18): if `PauseVoice` is carried out as a stop, its `VoiceFinished { Stopped }` is not the action ending — forwarding it is harmless (the engine ignores ends dated at or after the pause while paused, and any report for the paused id after the resume), and `ResumeVoice` restarts from `offset` as a new voice with the fresh id; a dropped `VoiceFinished` (`dropped_events` > 0) leaves an audio cue running forever — reconcile; keep cues near the playhead primed (§3.5)
- [ ] M4.3 WebSocket server: token auth, snapshot, model_changed/runtime/progress/meters, revision gaps [NFR-01]
- [ ] M4.4 Workspace requests: new/open/save/save-as/revert, recent files, recovery prompt flow [WS-02, WS-08, NFR-05] — wire `persist::SaveWriter` (clone workspace on control thread; pass `now_ms`; call `autosave_due` each tick), `check_recovery` on open, `load_machine_settings` at start; this is where NFR-04/05/06 and WS-03/06/07 become end-user behaviour and can be ticked
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
- [ ] M5.7 Inspector: triggers (hotkey, OSC) [CUE-09, TRG-01, TRG-03] — define hotkey conflict precedence (cue trigger vs. transport key vs. another cue) and warn on conflicts (M1-A review 2 L2)
- [ ] M5.8 Inspector: time & loops with waveform editor (trims, fades, slices) [AUD-10, AUD-11, AUD-12, AUD-14]
- [ ] M5.9 Inspector: audio levels + routing matrix; Fade cue settings [AUD-06, AUD-08, CT-10]
- [ ] M5.10 Editing: drag-drop files, reorder, drop-to-target, multi-select batch edit, copy/paste, undo/redo, renumber, find, fade-this-out, keyboard operation [ED-02..ED-05, ED-07..ED-10, ED-12]
- [ ] M5.11 Transport: GO/Stop/Panic/Pause/Resume, remappable hotkeys [PB-01..PB-05, TRG-01] — transport keys follow the same conflict precedence as M5.7
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
