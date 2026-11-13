---
name: video
description: "Load when changing or reviewing video: stages (core video.rs), the per-machine stage display choice (settings.rs), the engine-stage host link (core stage_link.rs), the stage host process openlab-stage (engine/crates/video: windows, displays, Metal drawing, test card), the engine's stage host supervisor, or video tests. The audio clock anchor video follows is skill devices."
---

# Video (stages and the stage host)

QLab 5 model: manual pp. 207-212 (Video cues), 225-230 (stages), 241-243 (Fade cues). Design decided by the user on 2026-11-13: decode and drawing live in a third process, the stage host; macOS decodes with AVFoundation/VideoToolbox (HAP in-house) and draws with Metal; video follows the audio clock; no video on Windows in v0.0.2. What is built so far: stages, their display choice, the stage host with black output windows and the test card.

## Must not break
- A stage with keep-black on and its display connected shows black whenever nothing plays on it: never the desktop, never a frozen frame (VID-12).
- The engine links no GUI framework and no video decoder. Everything that touches windows, displays, the GPU or (later) media is in `openlab-stage`, so a crash or hang there never stops sound.
- Every line crossing the link is validated on both ends (`stage_link::decode_*`): a bad line is a warning, never a panic, a hang or unbounded memory.
- The stage host exits when its stdin ends, so it never outlives the engine.
- The stage host never activates or takes key focus: the operator's keys stay with the app.

## Stages (`openlab-core` `video.rs`, `model.rs`)
- `Workspace.stages: Vec<Stage>`: `id` (`StageId`), `name`, `width`/`height` in pixels (1 to `MAX_STAGE_PIXELS` 16384, p. 208), `keep_black` (default true: QLab's "keep rendering between cues", p. 227). At most `MAX_STAGES` (64). File format §4.7.
- Load: a size outside the range is clamped (`StageSize` warning), stages past 64 are dropped (`TooManyStages`), two with one id refuse the file (`DuplicateStageId`), odd names warn (`stages.name`).
- Which display shows a stage is per machine: `settings.json` `workspaces.<id>.stages.<stage id>` = `StageMapping { display, name, width, height }` (the display's id plus what it was when chosen). `MachineSettings::stage_mapping` / `set_stage_mapping` (refuses empty or over-long text, control characters, sizes out of range). A damaged entry in the file drops only itself with a warning.

## The link (`openlab-core` `stage_link.rs`; contract `docs/protocol.md` "Stage host link")
- One JSON object per line; `LINK_VERSION` 1; lines at most `MAX_LINE_BYTES` (64 KiB).
- Engine to stage host (`ToStage`): `hello`, `stages` (the full set, replacing the last: id, size, display id or none, keep_black), `ping`, `quit`; test support `test_card`, `probe`, `hang`.
- Stage host to engine (`FromStage`): `hello` (version, pid, `video`: false where the build has no video output), `displays` (after hello and on every change), `stage_status` (`shown`, `hidden`, `no_display`, `display_missing`, `failed`), `pong`, `log`; test support `probed`.
- Limits checked after parsing: at most `MAX_DISPLAYS` (32) displays and `MAX_LINK_STAGES` stages, text 1 to `MAX_TEXT_BYTES` (512) bytes without control characters, sizes 1 to 16384, probe points 1 to 64 inside the stage.

## The stage host (`engine/crates/video`, binary `openlab-stage`)
- `host.rs` is the same on every platform: `Host<P: Platform>` keeps the stages, the displays and each stage's state, and reports each state change once. A stage is shown only with its display connected and (while black) keep-black on; otherwise hidden with the reason. `serve` reads lines with `read_line` (never holds more than one line's limit; an over-long line is skipped and reported) and ends at end of input or a failed write.
- macOS (`platform/mac`): an accessory app (no Dock icon or menu bar) whose windows come up with `orderFrontRegardless`, borderless, never key, ignoring the mouse, at screen-saver level over the display's whole frame (covering its menu bar and Dock), on every Space, out of Mission Control and window cycling. Never system full-screen. AppKit and Metal live on the main thread; a reader thread decodes lines and queues them on the main queue.
- Displays: CoreGraphics' active list, named by ColorSync's display UUID (stable across restarts and reconnection), pixel size from the display mode, name from AppKit's screen (else "Built-in Display" / "Display N"). A reconfiguration callback re-reads them and moves every stage.
- Drawing (`gpu.rs`): Metal clears the stage to black and draws each picture as a quad (nearest-neighbour sampling). A stage of another shape than its display is fitted, shape kept, black bars elsewhere (assumed; see skill `qlab-behaviour`).
- Other platforms (`platform/none.rs`): `video: false`, no displays, every stage `failed` with the reason.

## Tests
- Test support only (never in a shipped stage host: `scripts/rust-checks.sh` checks): `--virtual-displays ID:WxH,...` runs offscreen with those displays and no windows; `test_card` draws the test card (`card.rs`: a white guard cell, the frame number's 16 bits, a white guard cell across the top tenth; red, green, blue, yellow quadrants); `probe` draws the stage offscreen (at most 1024 px a side, the stage's shape) and reads colours back; `probed` carries the barcode's frame (or none) and the asked points' RGBA.
- Tests assert what a stage shows by reading the barcode and a few point colours: never a stored image, never a hash.
- `gpu.rs` tests draw on the machine's GPU (they fail, not skip, without Metal); `displays.rs` tests read the real display list and open no window. No test opens a window.
