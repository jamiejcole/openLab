---
name: video
description: "Load when changing or reviewing video: stages and their per-machine display (core video.rs, settings.rs), the stage host link (core stage_link.rs), the stage host openlab-stage (engine/crates/video), its supervisor (engine stage.rs, control/stages.rs), or video tests."
---

# Video (stages and the stage host)

QLab 5 model: manual pp. 207-212 (Video cues), 225-230 (stages), 241-243 (Fade cues).

Design decided by the user on 2026-11-13: decode and drawing live in a third process, the stage host; macOS decodes with AVFoundation/VideoToolbox (HAP in-house) and draws with Metal; video follows the audio clock; no video on Windows in v0.0.2.

Built so far: stages, their per-machine display, the stage host with black output windows, its supervision by the engine, and the test card read back in tests. Video cues, decode, the clock and fades are not built.

## Must not break
- A stage with keep-black on and its display connected shows black whenever nothing plays on it, never a frozen frame (VID-12), while the stage host runs. The desktop does show: from the engine's death until a relaunched engine's stage host is up; between a stage host crash and its relaunch (about 150 ms); and for the whole pause after repeated crashes (`retry_after`). The app-side black backstop that covers these is issue #667.
- The engine links no GUI framework and no video decoder. Everything that touches windows, displays, the GPU or (later) media is in `openlab-stage`, so a crash or hang there never stops sound.
- Every line crossing the link is validated on both ends (`stage_link::decode_*`): a bad line is a warning, never a panic, a hang or unbounded memory.
- The stage host exits when its stdin ends, so it never outlives the engine.
- The stage host never activates or takes key focus: the operator's keys stay with the app.

## Stages (`openlab-core` `video.rs`, `model.rs`)
- `Workspace.stages: Vec<Stage>`: `id` (`StageId`), `name`, `width`/`height` in pixels (1 to `MAX_STAGE_PIXELS` 16384, p. 208), `keep_black` (QLab's "keep rendering between cues", p. 227; default off, so a stage on the operator's display never covers it between cues). At most `MAX_STAGES` (64). File format §4.7.
- `layer` 0-1000 (default 1): of stages on the same display, a higher layer is drawn on top; on one layer the name sorting first is on top (p. 227; `stack_order`). Edits: `AddStage`, `RemoveStage`, `MoveStage`, `SetStageProperty` (skill `edit-commands`).
- Load: a size of 0 or above 16384 is clamped (`StageSize` warning), a layer from 1001 to 65535 too (`StageLayer`); a negative, fractional or out-of-type number refuses the file as a wrong type; stages past 64 are dropped (`TooManyStages`), two with one id refuse the file (`DuplicateStageId`), odd names warn (`stages.name`).
- Which display shows a stage is per machine: `settings.json` `workspaces.<id>.stages.<stage id>` = `StageMapping { display, name, width, height }` (the display's id plus what it was when chosen). `MachineSettings::stage_mapping` / `set_stage_mapping` (refuses empty or over-long text, control characters, sizes out of range). A damaged entry in the file drops only itself with a warning.

## The link (`openlab-core` `stage_link.rs`; contract `docs/protocol.md` "Stage host link")
- One JSON object per line; `LINK_VERSION` 1; lines at most `MAX_LINE_BYTES` (512 KiB: a full `MAX_LINK_STAGES` set with the longest display ids fits; tested).
- Engine to stage host (`ToStage`): `hello`, `stages` (the full set, replacing the last: id, size, display id or none, keep_black), `ping`, `quit`; test support `test_card`, `probe`, `hang`, `set_displays` (virtual displays plugged or unplugged).
- Stage host to engine (`FromStage`): `hello` (version, pid, `video`: false where the build has no video output), `displays` (after hello and on every change), `stage_status` (`shown`, `hidden`, `no_display`, `display_missing`, `failed`), `pong`, `log`; test support `probed`.
- Limits checked after parsing: at most `MAX_DISPLAYS` (32) displays and `MAX_LINK_STAGES` stages, text 1 to `MAX_TEXT_BYTES` (512) bytes without control characters, sizes 1 to 16384, probe points 1 to 64 inside the stage.

## The stage host (`engine/crates/video`, binary `openlab-stage`)
- `host.rs` is the same on every platform: `Host<P: Platform>` keeps the stages, the displays and each stage's state. A stage is shown only with its display connected and (while black) keep-black on; otherwise hidden with the reason. `serve` reads lines with `read_line` (never holds more than one line's limit; an over-long line is skipped and reported) and ends at end of input or a failed write.
- The stages shown on one display are drawn in one window (per display, not per stage), bottom first by `stack`; a stage keeping black is a black quad over what is below it. Test support `probe_display` reads a display's composite.
- After a `stages` message the host reports every stage's state (so the engine knows each was dealt with); after a display change only the states that changed.
- macOS (`platform/mac`): an accessory app (no Dock icon or menu bar) whose windows come up with `orderFrontRegardless`, borderless, never key, ignoring the mouse, at screen-saver level over the display's whole frame (covering its menu bar and Dock), on every Space, out of Mission Control and window cycling. Never system full-screen.
- AppKit and Metal live on the main thread; a reader thread decodes lines and queues them on the main queue. At the end of input (or a failed write) the process ends with `_exit` from whichever thread sees it: no exit handlers or stdio flush, so a hung main thread cannot keep it alive.
- Displays: CoreGraphics' active list, named by ColorSync's display UUID (stable across restarts and reconnection), pixel size from the display mode, name from AppKit's screen (else "Built-in Display" / "Display N"). A reconfiguration callback re-reads them and moves every stage, and reads them once more `SECOND_DISPLAY_READ` (500 ms) later, in case AppKit's screen list had not caught up (a re-plugged display would otherwise stay missing until the next display event).
- Drawing (`gpu.rs`): Metal clears the stage to black and draws each picture as a quad (nearest-neighbour sampling). A stage of another shape than its display is fitted, shape kept, black bars elsewhere (assumed; see skill `qlab-behaviour`).
- Other platforms (`platform/none.rs`): `video: false`, no displays, every stage `failed` with the reason.

## The engine side (`openlab-engine` `stage.rs`, `control/stages.rs`)
- `stage::StageHost` is the supervisor: thread `openlab-stage-link` spawns the stage host (own process group, stdin/stdout/stderr piped), with a writer thread (bounded queue of `WRITE_QUEUE` lines; full means the child stopped reading: killed), a reader thread (`read_line` + `decode_from_stage`; bad lines become warnings, at most `MAX_BAD_LINES_LOGGED` per child) and a stderr thread (first `MAX_STDERR_LINES` lines to the log).
- A `stage_status` for a stage the stage host was not given is dropped (supervisor), and the control thread keeps states only for stages it sent, so child input cannot grow engine memory.
- Report log lines (stage states, displays, the stage host's `log`) go through `Engine::report_line`: at most `MAX_REPORT_LINES_PER_SECOND` (20) a second by the control loop's clock, the rest counted and said as one warning on the first tick after that second (`flush_report_lines`), quiet or not; the supervisor also caps `log` messages at `MAX_LOGS_PER_SECOND`. A `displays` list equal to the last is not logged again. A message that fails to encode is logged as an error.
- Health: `hello` must be answered within `hello_within` (20 s: a cold start on a loaded machine or a slow disk is not a hang) and every `ping` (each `ping_every`, 1 s) within `answer_within` (5 s); a child that does not, sends more than `MAX_LINES_PER_SECOND`, or closes its output is killed (reaped on a thread of its own) and replaced at once with the last stage set.
- Starts older than `restart_window` are forgotten before a crash is counted (`forget_old_starts`), so a crash after a long healthy run is relaunched at once. More than `max_restarts` (5) starts in `restart_window` (60 s) pauses video output for `retry_after` (30 s), logged as an error. A stage host of another link version is never started again.
- `StageHost`'s drop sends `quit`; the stage host gets 1 s, then is killed. Its stdin closing also ends it, so it never outlives the engine however the engine ends.
- The control thread: `sync_stages` (every tick) builds every open doc's stages with this machine's display ids and hands the set to the supervisor when it changed. An idle tick compares only each doc's stages and a count of display-choice changes (`Stages::synced`, `mappings`) and does no more. The stage host starts with the control loop (`Control::empty`), so its displays are known when a new workspace is made. A changed stage's state is forgotten until the stage host reports it again.
- `StageEvent`s arrive through the inbox (`Inbox::Stage`): states and displays are kept for `list_stage_mappings`/`list_displays`; a state a stage cannot be seen in (`no_display`, `display_missing`, `failed`) is a `warn` log line, `shown`/`hidden` `info`; a stop is `warn` (restarting) or `error` (paused).
- An engine on the virtual device (`--virtual-device`) runs a stage host only with `--virtual-displays ID:WxH,...` (test-support builds), passed on to it: test engines never open windows on the machine's displays, and their scripts stay deterministic.
- Where the stage host is: `Contents/Helpers/openLab Stage.app/Contents/MacOS/openlab-stage` beside the engine's `Contents/MacOS`, else `openlab-stage` beside the engine (development). Missing: video output off, one `info` log line at start (a build or development engine without it is not a show fault). Tests set `ControlConfig::stage_host`.
- A new workspace gets one stage per listed display (QLab 5 manual p. 226): "Stage 1", "Stage 2"... in display order, sized as the display, keep-black off, each shown on its display on this machine (`Engine::add_display_stages`, from `fresh_workspace`).
- Push: `push_video_outputs` (every tick) sends each workspace's connections `video_outputs { displays, video?, stages }` when a report, a stage set or a display choice changed, as a state message, at most once per `PUSH_EVERY_MS` (100 ms) by the control clock. A doc connection's `snapshot` request (the bind's too) is followed by its `video_outputs` (`Control::video_outputs`), so a connection that binds after the last push has it. The app keeps it in `WorkspaceStore.videoOutputs` and draws it on the workspace settings Video page (skill `mac-app`).
- Requests (`docs/protocol.md`): `list_displays` (app-scope), `list_stage_mappings`, `set_stage_mapping { stage, display? }` (the display must be listed now; stored in `settings.json`, refused `io` when settings cannot be saved).

## Tests
- Test support only (never in a shipped stage host: `scripts/rust-checks.sh` checks): `--virtual-displays ID:WxH,...` runs offscreen with those displays and no windows.
- `test_card` draws the test card (`card.rs`: a white guard cell, the frame number's 16 bits, a white guard cell across the top tenth; red, green, blue, yellow quadrants); `probe` draws the stage offscreen (at most 1024 px a side, the stage's shape) and reads colours back; `probed` carries the barcode's frame (or none) and the asked points' RGBA.
- Tests assert what a stage shows by reading the barcode and a few point colours: never a stored image, never a hash.
- `tests/output_window.rs` (its own `main`, on the main thread) builds the shipped output window for the main screen's frame without ordering it front and checks level, collection behaviour, never key or main, mouse ignored, no shadow, borderless, frame equal to the screen's, not visible. Without a screen it reports a skip (a failure on CI).
- `gpu.rs` tests draw on the machine's GPU (they fail, not skip, without Metal); `displays.rs` tests read the real display list and open no window. No test opens a window.
- Real-engine tests: `engine/crates/video/tests/stages.rs` (macOS) runs the control loop on the offline driver (the engine's `tests/show` helpers) with the real test-support stage host (`CARGO_BIN_EXE_openlab-stage`, `--virtual-displays`) and reads stages back with `stage_probe`.
- Covered there: black, the test card, display choice, no display, keep-black off, a display unplugged and back mid-cue, a killed and a hung stage host replaced while a cue's sound plays on, the stage host gone after shutdown.
- Also covered there: the windowed stage host exiting when its input closes, a SIGKILLed test engine leaving no stage host, a crash after a long healthy run relaunched at once (injected `StageClock`), a reporting burst bounded in log lines and memory, and `/bin/sh` stand-ins for a flooding, a log-bursting and a silent stage host. `OfflineShow::settle_stages`, `wait_for` and `send_until` wait on the stage host (hang guards).
- The link codec is fuzzed: `fuzz/fuzz_targets/stage_link.rs`, body `fuzz/src/stage_link_input.rs`, run on stable by core's `tests/fuzz_stage_link.rs` over `fuzz/stage-link-seeds`.
