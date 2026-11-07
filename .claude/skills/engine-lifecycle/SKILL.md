---
name: engine-lifecycle
description: "Load when changing or reviewing engine start/shutdown, engine.json, instance lock, signals, run marker, crash capture, show log and status, sleep prevention, app relaunch, Pre-Show Check window (Preshow*.swift), EngineAlarm.swift, mac supervisor (engine/crates/engine/src/host/, diag/, platform/)."
---

# Engine process lifecycle

`host::run(Options)`: `openlab-engine [--open SHOW_FILE]... [--app-support DIR] [--watch-stdin] [--relaunch-app BUNDLE_ID]`. `--version` prints the version and exits 0. Any other argument, a flag without its value, or an invalid bundle id is a usage error: usage line on stderr, exit 2, before anything starts. The engine starts with **no workspace open**; the client decides (recovery check, last workspaces, none) with `new` / `open` / `open_recovery`.

## Must not break

- **A hung audio system, disk or client must not stop the process from answering a signal or exiting.** One deadline bounds the whole exit; past it the exit is forced.
- **The first act of a start is the instance lock**, before the log, run marker, device, port or `engine.json`. Two engines must never race for the device, OSC and the file.
- **`engine.json` is removed last** on a graceful exit, so a start until then is refused naming this engine and the app sees an engine to wait for, not a gap to start another into.
- **An engine that is stopping is never attached to or signalled by the app**; the app waits for its pid.
- **The app never signals an engine it has talked to**, and never relaunches an app that is still running.
- **A panic on the control thread is a crash**: non-zero exit, run marker and `engine.json` left behind.
- **No signal handler for crashes**, ever (below).
- **Nothing is sent off the machine** by crash or log code; there is no network code for it.
- Nothing in diagnostics runs on the audio thread or waits for a disk, except the panic hook (writes a file after a caught audio-thread panic) and `LogWriter::finish`'s bounded flush.

## Start order

1. Block the shutdown signals (`platform::signals`) before any thread exists, so every thread inherits the mask.
2. Instance lock, then the signal thread, so a start stuck in the audio system still answers a signal.
3. Show log, panic hook, run marker (see `references/diagnostics.md`), then the `engine.json` check.
4. Default output device (skill `devices`), mixer, UI server, OSC, `Control::empty`.
5. `--open` requests. From here a signal is a graceful shutdown. Then `engine.json`, the stdin thread (`--watch-stdin`), the control thread.
- `--open` (dev and tests) sends the app-scope `open` without a recovery choice and prints each outcome on stderr (`opened PATH (doc N)`, `could not open PATH: ...`, `not opened PATH: an autosave newer ...`); a failure leaves the engine running. A stderr line `listening on 127.0.0.1:PORT` says it is up.

## `engine.json` and the instance lock

- Location `<app-support>/openLab/engine.json` (`host::engine_file`, `AppDirs::engine_file_path`). Fields `pid`, `port`, `token`, `version`, `protocol_version`, `started_ms`, `stopping`. The app reads `port`, `token`, `pid`, `stopping` and ignores the rest. A file without `stopping` reads as `false`.
- Written by `persist::write_engine_file` (atomic: private temp `engine.json.<pid>-<n>.tmp`, mode 0600, fsync, rename, folder sync; temps from crashed runs a minute or more old are removed). A reader never sees half a file; the token is the user's only. Removed on graceful exit only while it still holds this run's pid and token; a crash leaves it. No app-support folder: no file, a stderr line says so. Not writable: the start fails (no client could find the engine).
- **Stale** = its pid holds no run-marker lock (`run_marker::is_live`; the lock goes with the process). A start replaces a stale file.
- **`stopping: true` contract:** a client must not attach; wait for that pid to exit, then find or start the next engine.
- **Lock:** exclusive lock on `<app-support>/openLab/engine.lock` (`host::instance_lock`; std `File::try_lock`: `flock` on Unix, `LockFileEx` on Windows), held with the open file for the process's life, so a crash releases it and a leftover file never refuses a start. The holder writes its pid into it.
- A start that finds the lock held fails at once with `another openLab engine is running (pid N, port P)` (exit 1), touching nothing of the other's. Pid and port come from `engine.json` when it is the holder's; otherwise the message says `starting or stopping`.
- Where the lock cannot be taken (unwritable folder, filesystem without locks, a non-file named `engine.lock`) a stderr line says so and the start goes on with the older check: a start while `engine.json` names a live engine fails the same way, and the app attaches to that engine (or, if it does not answer, ends it by pid before starting another).

## Graceful shutdown

Triggers: `goodbye { shutdown: true }` from any client, `SIGTERM`, `SIGINT`, `SIGHUP` (macOS: a `sigwait` thread sends `Inbox::Shutdown` and logs the signal), or stdin closing with `--watch-stdin`. Windows and Linux have no signal handling (the default action ends the process at once; the Windows UI sends `goodbye { shutdown: true }`).
1. The first trigger, or the end of the control loop however it ended, sets the `net::Stopping` flag at once, long before the server stops. From then on an app connection that ends is not relaunched (logged `info`), a waiting relaunch gives up, and an `openlab-stopping` thread rewrites `engine.json` with `stopping: true`.
   - That mark runs on its own thread so the control loop and the final recovery writes never wait for it. It holds this run's `engine.json` while writing, so on a hung disk the final removal waits behind it until the forced exit; the file left then names a dead pid, which the next start treats as stale. A mark arriving after the file was taken out for removal writes nothing.
2. The loop ends. `Control::shutdown` hard-stops every doc and writes the recovery file of every doc with unsaved changes, bounded by `SHUTDOWN_WAIT`. These writes must run before the docs are dropped (see skill `persistence`).
3. Control state drops (writer drops bounded by `persist::DROP_WAIT`; the device thread drops the stream), the server stops, the log flushes (bounded, `LogWriter::finish`), the run marker goes, `engine.json` is removed last, exit 0.

**Forced exit** (`host::stop`):
- A signal before the control loop runs (a start stuck in a device open) exits at once with 0: no workspace is open yet.
- Once shutdown is under way (after `goodbye`, stdin or a signal), a first signal only says so on stderr and a second exits at once with 1 ("press Ctrl-C again").
- `EXIT_DEADLINE` runs from the first signal while running, or the end of the control loop, whichever comes first. It is sized as final recovery writes + the two writer drops + the log flush + 1 s spare, and **must stay under the app's 25 s before SIGKILL**. Change one of those bounds and you must re-check this sum.
- A forced exit removes this run's `engine.json`, leaves the run marker (the next start reports the unclean end), says why on stderr and ends with `_exit` (`platform::exit::exit_now`; `std::process::exit` on Windows), so no exit handler that a thread stuck in the audio system could block runs.

## Run marker and crash capture

- At start the engine writes `<app-support>/openLab/Running/running-<pid>.json` (`pid`, `started_ms`, `version`) and holds an exclusive file lock on it for its whole life (`diag/run_marker.rs`). It is written, locked and synced under a temporary name, then renamed (the lock belongs to the open file and survives), so a concurrent start never finds it empty or unlocked. Dead temps are removed by a later start once unlocked and a minute old.
- With no app-support folder, or an unwritable `Running` folder, the engine runs without a marker and the start goes on; that run leaves no crash notice.
- A graceful end of `host::run` (including a start that fails with an error) removes it. A control-thread panic returns `HostError::ControlPanicked` (non-zero exit) and leaves it.
- **The next start locks each leftover marker it finds.** One it can lock belongs to a dead run (the OS drops a lock when the process ends however it ends): it records one `warn` show-log record (kind `engine`) "previous run ended without a clean shutdown (crash, kill or power loss): ...", naming that run's crash reports (at most 3), prints the same line to stderr, and removes the marker, so each unclean exit is reported once. A marker a live engine holds cannot be locked and is left alone.
- The record is the engine's, not a workspace's: it is in `log_tail` whichever workspace is open. No client is connected at start, so there is no live push for it.
- Bounds so a junk folder cannot slow or hang a start: only regular files are read (a FIFO is left alone), markers over 4 KiB are not read, 32 leftovers per start (oldest first), 10,000 folder entries looked at. An empty or unreadable marker is still reported ("its details were not readable"). An extreme `started_ms` saturates. A filesystem without locks treats every marker as over: missing a crash is worse than a false notice.
- Covered: aborts (double panic, `process::abort`, OOM abort), SIGSEGV/SIGBUS/SIGILL, SIGKILL (force quit, OOM killer), a hang the user or UI kills, power cut, kernel panic. A panic on another thread is the panic hook's.
- **Why no signal handler:** only async-signal-safe calls may run in a SIGSEGV/SIGBUS/SIGABRT handler (no allocation, locks, formatting or `std` I/O beyond a raw `write(2)`). 
  A useful report needs all of those, and a mistake turns a crash into a deadlock. A hung engine holding the audio device is worse than a dead one: the UI cannot tell it from a busy one and cannot relaunch it. 
  Rust's std has no signal API, so it would need `unsafe` FFI to `sigaction`/`sigaltstack` and, on Windows, `SetUnhandledExceptionFilter`. SIGKILL and power loss cannot be caught at all. The marker covers all of them the same way with no code running at crash time. macOS's own reporter still writes `~/Library/Logs/DiagnosticReports/openlab-engine-*.ips`.
- The app shows the crash folder and the unclean-exit footer (see `references/diagnostics.md`).

## App relaunch (engine side; `net/relaunch.rs`, `platform/relaunch.rs`)

`--relaunch-app BUNDLE_ID` is given by the app that starts the engine. Without it (dev, tests) an app connection ending is only logged. For an **app connection** that ends without `goodbye` (UI crash or dead connection; logged `warn`, playback goes on):
- A task waits `Relaunch::delay` (1 s), then relaunches unless an app connection is registered again (`Hub::app_connections`) or the engine is stopping or the server has stopped.
- Relaunch runs on a blocking thread (`open -b <id>` on macOS; it brings the app to front, which an app relaunched after a crash needs on macOS 14+ for keyboard GO to reach it). **At most 3 in any 60 s** (`net::relaunch::Budget`; beyond it an `error` log and no relaunch). One waiter at a time.
- **An app that still runs is not relaunched:** the app connection's `hello` `pid` is noted with the process name (`platform::relaunch::process_name`, libproc `proc_name`; the latest app connection's wins; no `pid`, or no such process, notes nothing).
  While a process with that pid and name lives (reconnecting slowly, or hung) the check waits another delay, again and again (logged once, `warn`), until a connection is back, the server stops, or the process is gone (then relaunch through the budget). 
  `open -b` on a running app would only pull it to the front mid-show.
- The name check keeps a pid reused by another program from counting as the app.
- Off macOS no name is known and the app counts as gone (relaunching is not available there yet).
- Never relaunches: a connection bound to a doc (a window); a `goodbye` with or without `shutdown`; an app connection ending once the engine is stopping (an `info` record, also when a waiting relaunch gives up), so the app is never started onto an engine about to exit.

## Sleep prevention (`platform::power`)

Each wake the loop works out whether any doc is in Show Mode or has an active cue (`CueEngine::has_active_cues`) and, when that changes, says so on `ControlConfig::awake`. The `openlab-power` thread holds an IOKit `PreventUserIdleDisplaySleep` assertion while it is true (no idle system sleep, display sleep or screen saver), logging each change and a failure once.
Shutdown lets it go; the OS drops it if the process dies. Not on Windows yet (a log warning when it would be needed). Test engines on the virtual device never hold it.

## Tests

- `tests/cli.rs` runs the binary: `engine.json` private with this pid, `--open` of a missing file leaves the engine running, SIGTERM exits 0 and removes the file and marker. It skips with a note when the machine has no output device. `tests/diag_crash.rs` panics a child with the hook and reads the report; a child that aborts with its marker written is reported by the next start.
- Use child processes and injected clocks, not sleeps (CLAUDE.md). `host::stop`, `host::engine_file`, `run_marker`, `platform::signals`, `platform::exit` and `tests/net/ws.rs` (no relaunch once stopping; waiting relaunch gives up) cover the rules above.

## References

- `references/diagnostics.md`: read when changing the show log (`control/observe.rs`, `diag/`), `log_tail`/`log_entry` pushes, `status`, the crash hook, or log rotation.
- `references/mac-supervision.md`: read when changing the app's engine start, attach, reattach, quit or alarm (`mac/openLab/Engine/`, `AppLaunch.swift`) or the Pre-Show Check window.
