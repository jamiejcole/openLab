# Show log, status and crash reports (`openlab-engine::diag`)

Show log, system status and local crash capture. Nothing here runs on the audio thread and nothing leaves the machine. Wire shapes (`LogEntry`, `log_tail`, `log_entry`, `status`) are in `docs/protocol.md`.

## Show log

One structured record per event, made on the control thread (`control/observe.rs`) from what the loop does. Common fields: `time_ms` (wall clock UTC from `EpochClock`), `show_time` (seconds of audio rendered), `level`, `kind`, `message`.

| `kind` | When |
|---|---|
| `trigger` | A transport action from a client (`ui`) or OSC peer (`osc <ip>`), logged just before it is acted on. Edits and requests are not triggers. Rate-limited per source. For OSC it names the workspace: an un-prefixed GO is one record per open doc |
| `go` | A GO, before it runs; `cue` = the cue at the playhead, or none (the engine then warns) |
| `transport` | Every other transport action |
| `cue_start` | A cue's state becomes `pre_waiting` or `running` from idle, loaded or broken. A resume or the end of a pre-wait is not a start |
| `cue_stop` | A cue returns to idle after running, pausing, waiting, fading or stopping |
| `cue_fail` | A cue breaks **at run time** (`voice_rejected`, `stream_failed`, `device_lost`). Static problems are not run-time failures |
| `device` | Device lost, reopen attempts, restored, stuck list |
| `document` | Workspace opened or created, saved, save failed |
| `engine` | Everything else the loop reports: cue-engine warnings, faults, lost events, clients not keeping up |

- **Trigger limit:** 5 per source per 10 s (wall clock); the rest are counted and one record at window end says `N more input(s) not logged`. Why: a chatty controller must not flush GO history out of the ring or rotated files. `go` records are not limited. At most 256 sources tracked; later ones share one `other sources` entry.
- Every message the loop sends as `log` is still sent unchanged and also recorded; the other kinds are only in the log. A client reads history with `log_tail` and follows with `log_entry`.
- **Workspace prefix:** while more than one workspace is open, a record a workspace caused (cue start/stop/fail, GO, transport, document event, warning) begins with its name in brackets, `[Hamlet] cue 5 "Storm" started` (`[workspace <doc>]` if unnamed). Engine records (device, health, triggers) and every record with one workspace open have no prefix. The crash report's workspace line lists every open show file, `; `-separated.
- **Ring:** newest `RING_CAPACITY` records feed `log_tail` and the crash report. Messages are cut at 2 KiB.

### Live push

- When `ControlConfig::push_log` is set (the host sets it; offline tests leave it off), `Control::record` sends each record as `log_entry { seq, entry }`. `seq` is the record's number in this run, counted by `ShowLog` under the ring's lock, so a client joins `log_tail` and pushes without duplicates and sees any gap.
- Pushes are lossy (`Outbound::try_lossy`: refused while the outbound channel is half full, like `meters`; the hub drops them per client on the same rule without marking it lagged) and limited to 200 delivered pushes per 10 s wall-clock window (the trigger window). **A refused push spends no budget**, so a client that falls behind does not use it up.
- The newest record the control thread could not hand over is kept and offered again each tick until budget and channel allow.
- A push the hub drops for a client is gone for that client. `status.log_seq` (newest record's number) shows the gap within a status period; the client reads it with `log_tail`.
- **Lost pushes are not counted in the "clients were not keeping up" notice**: that notice is itself a pushed record and would report its own loss.

### On disk

- `openlab.log` in `AppDirs::logs_dir()`: `~/Library/Logs/openLab` on macOS (Console.app lists it), `<app-support>/openLab/Logs` elsewhere. One JSON object per line, appended across runs. With no app-support folder, or an unwritable log folder, the engine runs with the ring only. A file that cannot be opened is retried on the next record.
- **The control thread never waits for the disk.** The writer thread `openlab-log` owns the file; the control thread only `try_send`s into a bounded channel (4096 lines). If the channel is full or the file cannot be written (disk full, folder gone), the record is **dropped and counted**: `log_dropped` in `status`, `dropped` in `log_tail`.
- The control thread logs a `warn` ("the show log fell behind the disk", with the number lost) when the count rises, at most once per 10 s; that notice is itself a record a full queue refuses, so it counts itself as seen. The writer puts a `warn` line into the file once it catches up.
- At shutdown the host waits at most 3 s for the writer to drain (`LogWriter::finish`), so a hung disk cannot hold the process up.
- Rotation: before a line would take `openlab.log` past the size limit it becomes `openlab.1.log`, `.1` becomes `.2`, and so on; the oldest is deleted (`LogLimits`, `diag/writer.rs`).
- The engine does not use the `tracing` crate: records are structured values the UI shows and nothing else logs through it (decision in skill `architecture`, dependencies).

## Status

Sent when `ControlConfig::status` is set (the host does; offline tests do not).
- **Cadence is wall-clock** (`EpochClock`), so a lost device, whose audio clock has stopped, still reports: at least every second, early when `xruns`, disk health, the primary's state or any output device's opening or state changes, never closer than 250 ms.
- The control thread only reads counters that already exist, and only when a status is due (each tick: a few atomics, the device name and the output states packed into one number, `diag::outputs_fingerprint`, to decide). The CPU probe and the rest are gathered when one goes out.
- `cpu_percent` is process CPU seconds (`diag::cpu`: `/proc/self/stat` on Linux, `getrusage` on macOS, `GetProcessTimes` on Windows) differenced over wall time. The macOS and Windows calls are two small `extern` declarations with `// SAFETY:` comments; only CI exercises them.
- `engine_load` is `null` when no callback ran since the previous status (device lost) and offline (measurement in skill `devices`). `disk.healthy` is false for ten seconds after any underrun rise.
- `device.state` (`ok`, `lost`, `failed`; `absent` offline) stays `ok` while the stall watchdog's first stage is up; the stall is a workspace warning and `warn` log only.
- `status` is lossy like `meters`. Field list and meanings: `docs/protocol.md`.

## Crash reports

`diag::crash::install` (in `host::run` after the instance lock and the log writer, before the run marker; only when an app-support folder exists) sets a panic hook that writes `crash-<UTC>-<pid>.txt` into `AppDirs::crash_dir()` (`~/Library/Logs/openLab/Crashes` on macOS, beside the logs a user is asked to attach; `<app-support>/openLab/Crashes` elsewhere).

It holds: version, time, OS and architecture, open workspace path(s), thread, panic message and location, backtrace, and the last 200 show-log lines from the ring.

Then the previous hook runs (the message still reaches stderr).

The hook must not make things worse:
- It only `try_lock`s the ring and workspace path (a panic inside the log's own critical section just gives a report without log lines).
- It writes with `create_new`, never over an older report.
- It ignores a panic raised while it runs (a global flag).
- It writes at most 3 reports per run: a panic the audio callback catches can repeat every block, and the hook then runs on that thread, already in a fault state with its block silenced.

Aborts and signals leave no file (no code of ours runs); the run marker catches them on the next start (SKILL.md, "Run marker and crash capture"). Sending reports is opt-in and **nothing is sent**: there is no network code for it.

## macOS app side

- Help > Show Crash Reports in Finder reveals `~/Library/Logs/openLab/Crashes` and `~/Library/Logs/DiagnosticReports` (those that exist; a message when neither does), so a report can be attached by hand (`revealCrashReports`, `AppController+Windows.swift`).
- When the first workspace window opens, the app reads `log_tail`; if it holds the run-marker record (kind `engine`, "previous run ended without a clean shutdown"), the footer says so, never an alert. The record stays in the Show Log window.

## Tests

`tests/diag/log.rs` (every kind and its fields, ring bound, a stuck writer drops and counts while the cue still plays on its sample, file equals ring), `tests/diag/status.rs`, `tests/diag_crash.rs`, `diag::run_marker` unit tests, `tests/net/ws.rs` and `tests/net/osc.rs` (triggers announced with their source).
