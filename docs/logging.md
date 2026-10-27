# Show log, status and crash reports (`openlab-engine::diag`, M4.7)

DX-02 (show log), DX-03 (system status), DX-04 (local crash capture). Nothing here runs on the audio thread, and nothing leaves the machine.

> **M4.7 status (WIP note for the next orchestrator).** Engine side is complete and green on branch `m4-7-log-status` (fmt, clippy native + windows-gnu, `cargo test --workspace`: 69 result lines ok). Not yet done: adversarial review, PR, `make test` for Swift (M5 must decode `status` / `log_tail`), and the PROGRESS follow-ups M4.7a (engine_load needs an audio-thread timer), M4.7b (abort/signal capture, opt-in sending), M4.7c (live log push). DX-02/03/04 stay unticked. macOS/Windows CPU FFI in `diag/cpu.rs` only type-checks locally (macOS cargo check passed).

## Show log (DX-02)

One structured record per event, made on the control thread (`control/observe.rs`) from what the loop does:

| `kind` | When | Fields beyond the common ones |
|---|---|---|
| `trigger` | an input reached the engine from a client (`ui`: every `transport` message) or an OSC peer (`osc <ip>`: every input), logged just before the input is acted on | `source`, `message` = `transport go`, `transport start`... |
| `go` | a GO, before it runs | `cue` = the cue at the playhead, or none (the engine then warns) |
| `transport` | every other transport action | `cue` when the action names one |
| `cue_start` | a cue's state becomes `pre_waiting` or `running` from idle, loaded or broken (a resume or the end of a pre-wait is not a start) | `cue` |
| `cue_stop` | a cue returns to idle after running, pausing, waiting, fading or stopping | `cue` |
| `cue_fail` | a cue breaks at run time (`voice_rejected`, `stream_failed`, `device_lost`); static problems are not run-time failures | `cue`, reason in `message` |
| `device` | device lost, reopen attempts, restored, grace over, stuck list | |
| `document` | workspace opened or created, saved, save failed | |
| `engine` | everything else the loop already reported: warnings from the cue engine (`warning {...}`), faults, lost events, clients not keeping up, ... | `cue` for warnings that name one |

Common fields: `time_ms` (wall clock UTC from the control config's `EpochClock`), `show_time` (seconds of audio rendered), `level`, `kind`, `message`. The record type is the protocol's `LogEntry` ([protocol.md](protocol.md)).

Every message the loop used to send as `log` is still sent, unchanged (and now also recorded); the new kinds are only in the log. A client reads recent history with `log_tail`; a live view refreshes it on `runtime`/`log`/`status` messages or on a timer (live push of records is not part of v0.0.1).

### In memory

A ring of the newest 1000 records (`RING_CAPACITY`). It feeds `log_tail` and the crash report. Messages are cut at 2 KiB.

### On disk

`<app-support>/openLab/Logs/openlab.log`, one JSON object per line (the `LogEntry` form), appended across runs. A writer thread (`openlab-log`) owns the file; the control thread only `try_send`s into a bounded channel (4096 lines). When the channel is full, or the file cannot be written (disk full, folder gone), the record is **dropped and counted**; nothing ever waits for the disk. The count is `log_dropped` in `status` and `dropped` in `log_tail`; the control thread logs a `warn` ("the show log fell behind the disk") when it rises, and the writer puts a `warn` line into the file itself once it catches up. A file that cannot be opened is retried on the next record.

Rotation: before a line would take `openlab.log` past 4 MiB it becomes `openlab.1.log`, `.1` becomes `.2`, and so on; at most 8 files are kept (`LogLimits`), the oldest is deleted. The folder comes from `AppDirs::logs_dir()` (the same app-support root as backups and recovery). With no app-support folder, or an unwritable log folder, the engine runs with the ring only.

The engine does not use the `tracing` crate for this: records are structured values with fields the UI shows, nothing else in the engine logs through it, and the writer is small. (ARCHITECTURE §12 lists `tracing` as approved, not required.)

## Status (DX-03)

`status` messages ([protocol.md](protocol.md)) are sent when `ControlConfig::status` is set (the host does; offline tests do not). The cadence is wall-clock (`EpochClock`), so a lost device, whose audio clock has stopped, still reports: at least every second, and early when `xruns`, the disk's health or the device's state changes, never closer than 250 ms. The control thread only reads counters that exist already:

| Field | Source |
|---|---|
| `cpu_percent` | process CPU seconds (`diag::cpu`: `/proc/self/stat` on Linux, `getrusage` on macOS, `GetProcessTimes` on Windows) differenced over the wall time since the previous status. The macOS and Windows calls are two small `extern` declarations with `// SAFETY:` comments (no new dependency); only CI exercises them. |
| `engine_load` | `null`. Render time over block time needs a timer in the audio callback, which M4.7 may not change: PROGRESS M4.7a. |
| `xruns` | `MixerController::xruns()` |
| `disk.streams` | `DiskStreamer::open_streams()` |
| `disk.underrun_frames`, `disk.healthy` | `MixerController::underrun_frames()`; unhealthy for ten seconds after any rise |
| `device` | the open stream's name, rate, block and channels, the host's callback latency, and its state (`ok`, `lost`, `failed`; `absent` offline) |
| `log_dropped` | the log sink's drop count |

`status` travels the lossy path like `meters`: dropped first when the outbound channel is half full, the next one replaces it.

## Crash reports (DX-04)

`diag::crash::install` (called first thing by `host::run_show`, when an app-support folder exists) sets a panic hook that writes `<app-support>/openLab/Crashes/crash-<UTC>-<pid>.txt` with: version, time, OS and architecture, the open workspace path, the thread, the panic message and location, a backtrace, and the last 200 show-log lines from the ring. Then the previous hook runs (the message still reaches stderr).

The hook must not make things worse: it only `try_lock`s the ring and the workspace path (a panic inside the log's own critical section just gives a report without log lines), it writes with `create_new` (never over an older report), it ignores a panic raised while it runs (a global flag), and it writes at most 3 reports per run (a panic the audio callback catches can repeat every block; the hook runs on that thread, which is already in a fault state with its block silenced).

Not covered, by design for v0.0.1: aborts and signals (SIGSEGV, an out-of-memory abort, a double panic) leave no report; the UI sees the engine exit and relaunches it (M4.9). Sending reports is opt-in and **nothing is sent in v0.0.1**: there is no network code for it. PROGRESS M4.7b lists the follow-ups.

## Tests

`tests/diag_log.rs` (every event kind and its fields, `log_tail`, ring bound, a stuck writer drops and counts while the cue still plays on its sample, the file equals the ring), `tests/diag_status.rs` (cadence, early send, fake CPU, counters, device state), `tests/diag_crash.rs` (a child process panics with the hook installed; the parent reads the report), unit tests in `diag/` (rotation, formatting, cadence), `tests/net_ws.rs` / `tests/net_osc.rs` (triggers announced with their source), `core/tests/protocol.rs` (fixtures).
