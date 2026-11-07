---
name: architecture
description: "Load when a change crosses threads, processes or crates, or touches the engine/UI split, doc multiplexing, sample-clock scheduling, Show Mode errors or Windows portability (engine/crates/*). Also for adding a dependency. Start/stop is skill engine-lifecycle; the mac app is skill mac-app."
---

# openLab architecture

Two programs on one machine: `openlab-engine` (headless Rust: audio, timing, model, undo, OSC) and the macOS app (Swift/AppKit thin client). They talk over a local WebSocket (wire: `docs/protocol.md`).

```
engine process
  audio thread (cpal callback) <-- rtrb SPSC (time-stamped commands) -- control thread
   mixer, voices, fades, matrix                                          owns Engine + N Docs
   patches, meters --> events/atomics -------------------------------->  (Workspace, CueEngine, undo)
        ^ SPSC rings of decoded audio                                        ^ crossbeam inbox
  disk threads (decode, resample, buffer)                                    |
  network threads: WebSocket server (UI), OSC UDP/TCP 53000 ----------------+
        | WebSocket, JSON, 127.0.0.1, token auth
  openLab.app (Swift + AppKit): renders mirrored state, sends commands
```

## Must not break
- **The engine is the single source of truth.** It owns model, undo, cue runtime state, timing and audio. A UI mirrors state and sends commands; it never computes show behaviour.
- **UI faults cannot stop playback** (NFR-01). If the UI dies the engine keeps playing and relaunches it.
- **The audio thread never blocks.** Rules are in CLAUDE.md; fault confinement is below.
- **The control thread does no disk I/O and nothing that can hang** (stat, decode, device open, name look-up, socket connect). Helper threads answer through the control inbox.
- **The device sample counter is the master clock.** Wake jitter on the control thread must never shift audio.
- **One engine process holds every open workspace** (limit in REQUIREMENTS §21) sharing one device, clock and mixer.
- **Device loss hard-stops every cue and never auto-resumes.** Only the operator's GO restarts the show.
- **A caught fault is confined to its unit** (voice, patch, output, master, doc, peer) and reported; the show keeps running.
- **No modal alert for a workspace in Show Mode.** Safety confirmations stay modal.
- **An engine crash is the one unrecoverable case**, so the engine stays small and free of GUI frameworks.

## Crates
- `crates/core` (`openlab-core`): model, commands/undo, cue state machine, scheduler, show-file (de)serialisation and migrations, protocol types. No I/O, threads, clocks. `#![forbid(unsafe_code)]`.
- `crates/audio`: device layer, decode/resample, disk streaming, real-time mixer, offline render driver.
- `crates/engine` (binary): control thread, wiring core to audio, WebSocket server, OSC, persistence I/O, logging, lifecycle.
- Other dirs: `fuzz/` (fuzz targets), `engine/fixtures/` (golden JSON), `mac/` (app, XcodeGen), `mac/TestShows/`.
- Keep the crate count at three. Add a module before a crate.
- OS-specific code lives in a `platform` module per crate behind `cfg` (paths, sleep prevention, relaunch, RT priority).
- Show-file paths are relative with forward slashes or absolute; compare them case-insensitively on Windows (NFR-31/32). The protocol is UI-agnostic, so a Windows UI does not touch the engine.
- Video is planned: the engine will own output windows so picture survives UI crashes. Do not assume the engine is windowless forever.

## Threads (who owns what)
| Thread | Owns | May block |
|---|---|---|
| Audio (cpal callback) | `Mixer`, voice pool, ramps, output patches | Never. `rtrb` queues and atomics only |
| Control (one event loop) | shared `Engine` (mixer controller, voice and patch-slot allocators, device state) and N `Doc`s (`Workspace`, `CueEngine`, undo, scheduler) | Briefly; no disk I/O |
| Disk pool, disk-open pool | decoders, resamplers; opening files so a hung open never holds a worker | Yes |
| Network (tokio) | WebSocket sessions, OSC sockets | Async; forwards to the control inbox |
| Writer | log file, atomic saves | Yes |
| Helpers: media probe pool, media watch (`openlab-watch`), device open/list threads, document worker, net send (one per Network-cue destination), net look-up | blocking work of one kind | Yes |
- Every helper answers through the control inbox. A control-side watchdog abandons a stalled watch worker.
- One document read runs at a time engine-wide; a second is `command_refused`.
- The document worker builds an opened doc off-thread as `DocParts`, so installing it on control is an insert plus probe jobs, within one audio block. A closed doc is dropped off the control thread.
- Saves: control only clones the workspace and polls; the writer thread writes.
- The control loop ticks every doc and wakes on an inbox message, the earliest `wait_hint`, or a 5 ms tick. Subsystem detail: skills `engine-control`, `audio-mixer`, `audio-streaming`, `devices`, `media-files`, `persistence`, `engine-ws-server`, `osc`.

## Clock and scheduling (NFR-20/21/22)
- Show files store seconds (f64). At runtime all timing (waits, fades, loop points) is sample time at the device rate; conversion happens when scheduling.
- The audio thread publishes `samples_rendered` after each callback.
- Control never "sleeps until" a cue time. It sends time-stamped commands (`StartVoice { at, .. }`); the mixer applies them at the exact sample within the block. That makes simultaneous starts sample-accurate.
- GO: `start = now_sample + one block`. Latency is one queue hop plus at most one block.
- The cue engine commits deadlines before `now + lookahead` (one block, end excluded). A deadline already passed starts at `now` without shifting later cues. Semantics: skill `cue-engine`.

## Core is pure (NFR-41, QA-02)
- `CueEngine::handle(ws, input, now) -> Vec<Action>`: inputs in, actions out. The engine binary executes `Action`s.
- Tests drive it on a virtual clock. Property tests check: no cue in two states, every started cue stops or loops, playhead always valid, undo then redo is identity.

## Real-time fault confinement
- The callback body runs inside `catch_unwind`. A panic outputs silence for that block, sets a fault flag and the control thread reports it loudly.
- The mixer records the unit it is working on and blames it. A unit that faults repeatedly (`FAULT_LIMIT` in `FAULT_WINDOW` blocks) is dropped or disabled and reported, so one bad voice cannot silence the show.
- Panics must not cross the FFI boundary into the OS audio thread.
- Stages and constants: skill `audio-mixer`.
- One exception to "no syscalls": the callback reads `Instant::now` twice per block for engine load (DX-03). It is inside the `assert_no_alloc` guard and `catch_unwind`; results leave through atomics only.

## Audio pipeline (diagram only)
```
file -> disk thread: symphonia decode -> rubato resample to device rate -> ring buffer (~2 s)
audio thread: voice reads ring -> trims/loops/slices -> cue fades -> channel levels
  -> routing matrix (file ch -> cue outputs -> patch outputs) -> Fade-cue ramps
  -> output patch (level, mute, polarity) -> master -> device -> meters (atomics)
```
- The mixer sees voices and patch matrices, never docs. `VoiceId`s come from one engine-wide counter with an owner map to `DocId`; events for an unowned voice (a closed doc's) are ignored.
- Cues near the playhead are kept primed. A cold cue starts when its buffer fills and shows a warning, never a failure.
- Offline render driver: the same mixer callback in a loop into memory. Gate: 64 streaming stereo voices with fades render at 10x real time or better on one core of the CI runner (2x headroom over the 32-voice NFR-23 target on a 2014 Mac mini).

## Model and commands
- Targets reference cue `id` (stable Uuid), never the editable number.
- Levels are dB (f64); -120 dB or lower means silence. Files write -120.
- Every mutation is a serialisable `Command`; applying returns its inverse. The UI, OSC edits and any future mirroring all use the same command stream. Multi-step edits are one transaction, one undo step. Skill `edit-commands`.

## Documents, connections, OSC
- One WebSocket connection is bound to one doc (`hello { workspace }`) or unbound (app connection listing open workspaces). Revisions and `runtime_seq` count per doc. A revision gap means the client re-requests a snapshot. Rules and messages: `docs/protocol.md` section 2.
- App-scope actions go on any connection and apply to every doc: `panic` and hotkey triggers. GO, Pause All and other transport keys act on one doc, the front window's. The engine has no "front workspace".
- OSC listens on 53000 (UDP and SLIP-framed TCP), replies to UDP senders on 53001. An un-prefixed message goes to every doc; `/workspace/{id}` picks one; `/connect` state is per doc. Skill `osc`.
- The OSC output override is an in-memory engine flag, never stored in a file.
- Settings split: per-machine settings (device mapping, engine prefs) live in `<app-support>/openLab/settings.json`, not the show file; UI-owned state (window layout, recents) lives in the app's UserDefaults. Skill `persistence`.

## Process lifecycle and supervision (NFR-01, SM-05, SM-07)
- The engine is inside the app bundle at `Contents/MacOS/openlab-engine`. It starts with no workspace open; the app decides what to open by sending `new`/`open`/`open_recovery`.
- Launch: read `engine.json` (`{port, token, pid}`, mode 0600). If a live engine answers, connect (UI restart). Otherwise spawn the engine detached in its own process group.
- Normal quit: confirm if cues run, then `goodbye { shutdown: true }`; the engine writes a final recovery file for every dirty doc and exits.
- UI crash (socket drops without `goodbye`): the engine keeps playing, logs it and relaunches the app after 1 s with `open -b <bundle id>` (brings it to the front so keyboard GO works), at most 3 times a minute. If the pid the app gave in `hello` is still running with the same process name, do not relaunch; wait for it to reconnect. The relaunched UI gets a full snapshot.
- Engine crash: the UI raises an unmissable alarm (floating non-modal always-on-top panel, sound, critical Dock bounce). It never takes keyboard focus, so transport keys keep working in windows bound to a live engine. It relaunches the engine and offers recovery.
- One process widens the crash blast radius: a crash stops every open show. Accepted, as QLab shares one device, clock and port. Mitigations: no-panic rules, fuzz targets, soak runs, a recovery file per dirty doc.
- The engine asserts sleep prevention in Show Mode (IOPMAssertion), so it holds while the UI restarts. Details: skill `engine-lifecycle`.

## Error policy
- Failures become broken-cue state, warnings or log entries: never a crash, never a modal in Show Mode.
- Lints are enforced in `engine/Cargo.toml` `[workspace.lints.clippy]`; `engine/clippy.toml` relaxes them in tests only (integration-test crates and support modules allow them at file top).
- Nothing writes to stdout/stderr except the logging path. The one exception is CLI output in `main` (`--version`) via `std::io::Write` with write errors handled (a closed pipe must not panic).
- Mac app, Show Mode workspace: errors and notices become a log record plus a footer line (`AppController.raise`, `WorkspaceWindowController.showNotice`), never an alert or sheet. Edit Mode errors show on that workspace. An error naming no workspace goes to the footers of Show Mode windows (`ShowModeGatedAlerts`).
- Still modal: close/quit while playing or in Show Mode, quit while the engine is unreachable, unsaved changes, the autosave decisions (open-time offer, Recover Autosaves, the alarm's Restore), and the engine alarm.

## CI pin
- Xcode is pinned in CI. The pinned Xcode's minimum deployment target is exactly 12.0. A future Xcode that drops 12.0 must not be adopted without a Monterey plan.

## Reference files
- Skill `mac-app` holds the macOS app structure (windows, `EngineClient`, `WorkspaceStore`, meters, Show Mode alerts).
- `references/dependencies.md`: read when adding, removing or changing a Cargo dependency; every dependency needs a row there.
