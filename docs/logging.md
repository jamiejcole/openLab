# Show log, status and crash reports (`openlab-engine::diag`, M4.7)

DX-02 (show log), DX-03 (system status), DX-04 (local crash capture). Nothing here runs on the audio thread, and nothing leaves the machine.

## Show log (DX-02)

One structured record per event, made on the control thread (`control/observe.rs`) from what the loop does:

| `kind` | When | Fields beyond the common ones |
|---|---|---|
| `trigger` | a transport action reached the engine from a client (`ui`) or an OSC peer (`osc <ip>`), logged just before it is acted on; edits and requests are not triggers. Rate-limited per source (below) | `source`, `message` = `transport go`, `transport start`... |
| `go` | a GO, before it runs | `cue` = the cue at the playhead, or none (the engine then warns) |
| `transport` | every other transport action | `cue` when the action names one |
| `cue_start` | a cue's state becomes `pre_waiting` or `running` from idle, loaded or broken (a resume or the end of a pre-wait is not a start) | `cue` |
| `cue_stop` | a cue returns to idle after running, pausing, waiting, fading or stopping | `cue` |
| `cue_fail` | a cue breaks at run time (`voice_rejected`, `stream_failed`, `device_lost`); static problems are not run-time failures | `cue`, reason in `message` |
| `device` | device lost, reopen attempts, restored, grace over, stuck list | |
| `document` | workspace opened or created, saved, save failed | |
| `engine` | everything else the loop already reported: warnings from the cue engine (`warning {...}`), faults, lost events, clients not keeping up, ... | `cue` for warnings that name one |

Trigger records are limited to 5 per source per 10 s (wall clock); the rest are counted, and when the window ends one record says `N more input(s) not logged` for that source. A chatty controller therefore cannot flush the GO history (the `go` records are not limited) out of the ring or the rotated files. At most 256 sources are tracked; later ones share one `other sources` entry.

Common fields: `time_ms` (wall clock UTC from the control config's `EpochClock`), `show_time` (seconds of audio rendered), `level`, `kind`, `message`. The record type is the protocol's `LogEntry` ([protocol.md](protocol.md)).

Every message the loop used to send as `log` is still sent, unchanged (and now also recorded); the new kinds are only in the log. A client reads recent history with `log_tail` and follows new records live with `log_entry` (below).

### Live push (M4.7c)

When `ControlConfig::push_log` is set (the host sets it; offline tests leave it off), `Control::record` sends each new record as a `log_entry { seq, entry }` message ([protocol.md](protocol.md)): `seq` is the record's number in this run (`ShowLog` counts them under the ring's lock, and `log_tail` answers with the newest one), so a client joins the tail and the pushes without duplicates and sees any gap. Pushes are lossy (`Outbound::try_lossy`: refused while the outbound channel is half full, like `meters`; the network hub drops them per client on the same rule without marking it lagged) and limited to 200 delivered pushes per 10 s wall-clock window, the trigger records' window (a refused push spends nothing, so a client that falls behind does not use up the budget). The newest record the control thread could not hand over is kept and offered again each tick until the budget and the channel allow (a budget hold-back: when the window ends). A push the hub drops for one client is gone for that client; `status` carries `log_seq`, the newest record's number, so the client sees the gap within a status period (about a second) without waiting for the next record, and reads it with `log_tail`. Lost pushes are not counted in the "clients were not keeping up" notice: that notice is a pushed record too and would report its own loss.

### In memory

While more than one workspace is open (M4.15e), a record a workspace caused (a cue started, stopped or failed, a GO, a transport action, a document event, a warning) begins with its workspace's name in brackets, `[Hamlet] cue 5 "Storm" started` (`[workspace <doc>]` when it has no name); engine records (the device, health, triggers) and every record while one workspace is open read as before. The crash report's workspace line lists every open show file, `; `-separated.

A ring of the newest 1000 records (`RING_CAPACITY`). It feeds `log_tail` and the crash report. Messages are cut at 2 KiB.

### On disk

`openlab.log` in the log folder, one JSON object per line (the `LogEntry` form), appended across runs. A writer thread (`openlab-log`) owns the file; the control thread only `try_send`s into a bounded channel (4096 lines). When the channel is full, or the file cannot be written (disk full, folder gone), the record is **dropped and counted**; nothing ever waits for the disk. The count is `log_dropped` in `status` and `dropped` in `log_tail`; the control thread logs a `warn` ("the show log fell behind the disk", with the number lost since the last notice) when it rises, at most once per 10 s (the notice is itself a record a full queue refuses, so it counts itself as seen and is rate-limited), and the writer puts a `warn` line into the file itself once it catches up. A file that cannot be opened is retried on the next record. At shutdown the host waits at most 3 s for the writer to drain (`LogWriter::finish`), so a hung disk cannot hold the process up.

Rotation: before a line would take `openlab.log` past 4 MiB it becomes `openlab.1.log`, `.1` becomes `.2`, and so on; at most 8 files are kept (`LogLimits`), the oldest is deleted. The folder comes from `AppDirs::logs_dir()`: `~/Library/Logs/openLab` on macOS (M4.7c; the platform's convention, and Console.app lists it), `<app-support>/openLab/Logs` on Windows (`%APPDATA%`) and Linux. No migration: v0.0.1 has not shipped, and an engine that finds older files under `Application Support/openLab/Logs` leaves them there (they are not read back, so nothing is lost but disk space). With no app-support folder, or an unwritable log folder, the engine runs with the ring only.

The engine does not use the `tracing` crate for this: records are structured values with fields the UI shows, nothing else in the engine logs through it, and the writer is small. (ARCHITECTURE §12 records the decision.)

## Status (DX-03)

`status` messages ([protocol.md](protocol.md)) are sent when `ControlConfig::status` is set (the host does; offline tests do not). The cadence is wall-clock (`EpochClock`), so a lost device, whose audio clock has stopped, still reports: at least every second, and early when `xruns`, the disk's health or the device's state changes, never closer than 250 ms. The control thread only reads counters that exist already, and only when a status is due: each tick it reads a few atomics and borrows the device name to decide (`Peek`); the CPU probe and the rest are gathered when one goes out.

| Field | Source |
|---|---|
| `cpu_percent` | process CPU seconds (`diag::cpu`: `/proc/self/stat` on Linux, `getrusage` on macOS, `GetProcessTimes` on Windows) differenced over the wall time since the previous status. The macOS and Windows calls are two small `extern` declarations with `// SAFETY:` comments (no new dependency); only CI exercises them. |
| `engine_load` | the worst audio callback since the previous status: render time over the block's duration (`DeviceMonitor::take_engine_load`, measured in `OutputHost::render`, M4.7a; docs/audio.md). `null` when no callback ran since the previous status (device lost) and offline. 1.0 means a callback took its whole block. |
| `xruns` | `MixerController::xruns()` |
| `disk.streams` | `DiskStreamer::open_streams()` |
| `disk.underrun_frames`, `disk.healthy` | `MixerController::underrun_frames()`; unhealthy for ten seconds after any rise |
| `device` | the open stream's name, rate, block and channels, the host's callback latency, and its state (`ok`, `lost`, `failed`; `absent` offline) |
| `log_dropped` | the log sink's drop count |
| `log_seq` | `ShowLog::seq()`: the newest record's number, as in `log_entry` and `log_tail` (0: none yet) |

While the stall watchdog's first stage is up (`device_stalled`, docs/control.md "Devices") `status.device.state` stays `ok`: the stall is a workspace warning and a `warn` log only, until the stream is lost (`lost`) or recovers.

`status` travels the lossy path like `meters`: dropped first when the outbound channel is half full, the next one replaces it.

## Crash reports (DX-04)

`diag::crash::install` (called first thing by `host::run`, when an app-support folder exists) sets a panic hook that writes `crash-<UTC>-<pid>.txt` into `AppDirs::crash_dir()` (`~/Library/Logs/openLab/Crashes` on macOS, beside the logs a user is asked to attach; `<app-support>/openLab/Crashes` elsewhere) with: version, time, OS and architecture, the open workspace path, the thread, the panic message and location, a backtrace, and the last 200 show-log lines from the ring. Then the previous hook runs (the message still reaches stderr).

The hook must not make things worse: it only `try_lock`s the ring and the workspace path (a panic inside the log's own critical section just gives a report without log lines), it writes with `create_new` (never over an older report), it ignores a panic raised while it runs (a global flag), and it writes at most 3 reports per run (a panic the audio callback catches can repeat every block; the hook runs on that thread, which is already in a fault state with its block silenced).

Aborts and signals (SIGSEGV, an out-of-memory abort, a double panic, SIGKILL, a power cut) leave no report file: no code of ours runs then. They are caught on the next start instead (below). Sending reports is opt-in and **nothing is sent in v0.0.1**: there is no network code for it (the opt-in setting and "send report" action are UI work, M6).

### Unclean exits (M4.7b)

`diag::run_marker`: at start the engine writes `<app-support>/openLab/Running/running-<pid>.json` (`pid`, `started_ms`, `version`) and holds an exclusive file lock on it while it runs (written and locked under a temporary name, then renamed into place); a graceful end of `host::run` (SIGTERM, SIGINT, SIGHUP, `goodbye { shutdown }`), including a start that fails with an error, removes it. A panic on the control thread is not a graceful end: `host::run` returns `ControlPanicked` (non-zero exit) and leaves the marker. The next start locks each leftover marker it finds (one a live engine still holds cannot be locked and is left alone), records one `warn` show-log record (kind `engine`): "previous run ended without a clean shutdown (crash, kill or power loss): openLab engine <version>, pid <pid>, started <UTC>; ..." naming that run's crash reports if its panic hook wrote any, prints the same line to stderr, and removes the marker. The show log is the engine's, not a workspace's, so the record is in `log_tail` whichever workspace is open. Bounds: only regular files are read, markers over 4 KiB are not read, 32 leftovers per start. No signal handler: why, and the other choices, are in [handoff/decisions/m4-7b.md](handoff/decisions/m4-7b.md).

## Tests

`tests/diag/log.rs` (every event kind and its fields, `log_tail`, ring bound, a stuck writer drops and counts while the cue still plays on its sample, the file equals the ring), `tests/diag/status.rs` (cadence, early send, fake CPU, counters, device state), `tests/diag_crash.rs` (a child process panics with the hook installed and the parent reads the report; a child that aborts with its marker written is reported by the next start), `diag::run_marker` unit tests (clean shutdown, leftover reported once with pid and start, a live engine's marker left alone, unreadable markers, a FIFO, extreme start times, the rename into place, crash reports named), `host` unit tests (marker removed on a graceful end and on a failed start, kept after a control-thread panic), `persist` unit tests (folder layout per platform), unit tests in `diag/` (rotation, formatting, cadence), `tests/net/ws.rs` / `tests/net/osc.rs` (triggers announced with their source), `core/tests/protocol.rs` (fixtures).
