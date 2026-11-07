# macOS app: engine supervision

Files: `mac/openLab/Engine/EngineSupervisor.swift`, `EngineProcesses.swift`, `EngineSession.swift`, `mac/openLab/EngineAlarm.swift` (alarm window; outside `Engine/`), `EngineLaunch.swift`, `AutosaveCover.swift`, `mac/openLab/AppLaunch.swift`. The engine side (`engine.json`, `stopping`, relaunch, deadlines) is in SKILL.md.

## Must not break

- **Never signal an engine this app run has talked to.** A dropped connection is retried with backoff for as long as its pid lives.
- **Never attach to or signal a `stopping: true` engine**; wait for its pid.
- **Never end an engine that merely answers slowly**: a socket that opens but sends no `welcome` within 10 s is busy (its control loop may be stalled while the audio thread plays), not silent.
- **Quit closes with `discard_changes` only after every prompt is answered**, so a later cancelled prompt never loses edits.
- A reused pid is never signalled: liveness is `kill(pid, 0)` plus the process's executable name (`openlab-engine`).
- The alarm never takes the keys: Esc and transport keys keep working mid-show.

## Launch

- **One copy of the app runs.** A second one with the same bundle id (another download, `open -n`) builds no menu and no engine session, fronts the copy launched first (earliest launch date, then lowest pid) and exits, since its Quit would end the shows the first runs. The unit-test host skips this.
- Fronting waits until the first copy has finished launching. From macOS 14 activation is cooperative: yield (`yieldActivation(to:)`) then request the first's activation (`activate(from:options:)`); on 12 and 13 activate it ignoring other apps.
- No `engine.json`, an unreadable one, or one whose pid is not a live `openlab-engine` starts the engine: `posix_spawn`, own process group, stdin and stdout on `/dev/null`, stderr appended to `engine-stderr.log` beside `engine.json`, with `--relaunch-app <bundle id>`. A stale file also marks the autosaves for offering. A live engine that answers is attached.
- stderr is a file, not a pipe, because the engine outlives the app and a pipe with no reader fails its writes. Emptied at an engine start when over 1 MiB. The app logs new lines to its engine log from a utility queue (reads at most 64 KiB, cuts lines at 2000 characters, at most 200 lines per start). If the app cannot open the file (read-only, a directory, gone), the engine gets `/dev/null` and still starts.

## Start deadline

An engine this app spawned that stays alive but writes no `engine.json` within 90 s (a cold start on a slow machine takes tens of seconds; never to an engine this run reached):
- Raises the alarm "did not start in time" (taken down when an engine answers), gets SIGTERM (SIGKILL after 25 s) on that pid only, and is replaced within the 3-starts-a-minute limit (past it the crash-loop alarm stays).
- Each consecutive engine that misses the deadline doubles the next one's (90, 180, 360 s, capped at 1024 times the first); a connection resets it, so a start of any finite length eventually runs.

A spawned engine that writes the file but fails 3 connection attempts in a row (refused, reset, no `welcome`, or an error before it) raises the "not responding" alarm, with its own message saying a new engine has not answered and will be ended and replaced; taken down when an engine answers. A valid start listens before writing the file, so this means a dead listener or about 30 s of no `welcome`.
This holds at launch and for a replacement after a crash. Nothing plays on it, so if it has not answered one start deadline after its file was first seen, it is ended and replaced as above. Each consecutive engine missing the deadline doubles the next one's (90, 180, 360 s, capped at 1024 times the first); a connection resets it, so a start of any finite length eventually runs.

## Stopping and unresponsive engines

- `stopping: true`: no connection opened, no signal. The app polls for the pid to exit (up to 25 s, just past the engine's 24 s `EXIT_DEADLINE`), then starts a new engine with no alarm. Still alive after that: reported once ("still shutting down", taken down when an engine answers) and waited for.
- After such a wait the new engine's autosaves are always offered (none: nothing shown): the stopped engine may have been forced out before its final recovery writes finished, and a forced exit removes `engine.json` like a clean one.
- An engine this app was connected to that leaves a `stopping: true` file behind (killed, or removal blocked on a hung disk) is alarmed as a stop, not a crash.
- An engine this app did not spawn: connections refused or reset 3 times while no connection has been made in this app run: SIGTERM, 25 s, SIGKILL, then a new engine (all logged). After 3 attempts that open a socket but get no `welcome`, the alarm says the engine is not responding and the app keeps retrying without ending it.

## Running, crash and reattach

- Once a talked-to engine's pid is gone, every window stops showing that run's cues as live (running, paused and waiting states, progress and countdowns cleared: those cues stopped with it). The alarm goes up as a crash when `engine.json` was left behind, else as a stop, and a new engine starts (at most 3 a minute, then an alarm). The alarm is a floating panel above every window with a critical Dock bounce, not modal, never key by itself.
- **Reattach:** when the app connection reaches another run (another token), every window's store is detached and rebound one at a time.
  - A window with a path is reopened with `open`. On `recovery_offer`: `restore` if the window had unsaved changes, else `ignore`. On `already_open` (a reopen cut short): the doc that has it, counted restored when that doc has unsaved changes, and for a window that had unsaved changes as edits lost when it is clean.
  - A window with unsaved changes and no autosave newer than its file is reopened from the file; the alarm and its log say its edits are lost.
  - An untitled window uses `open_recovery` of its workspace id's autosave (a clean one uses `new`). A window that cannot be rebound closes and is listed in the alarm.
  - A bound connection is pinned to its run's token and never says `hello` with an old doc id to a new run.
- Then `scan_recovery` entries no open window holds are offered behind the alarm's Restore button (one prompt each, at most 8 workspaces). The same offer is in File > Recover Autosaves... whenever the engine is connected, so a dismissed or never-made offer (a quit during reattach, an untitled workspace) stays reachable.
- A File > New or Open during the reattach gets its window at once, unless a waiting window will claim the path just opened (the engine answers `opened` before it lists the doc, so the decision uses the requested path). For the same reason a `workspaces` list that omits a doc closes its window only if an earlier list of that engine run named the doc.

### Which autosaves are offered (`AutosaveCover.swift`)

- A current autosave of an open workspace belongs to its window and is not offered: matched by workspace id and the `path_hash` of `workspaces` (another copy of the show, same id at another path, is offered). A window known to be untitled holds only its hashless autosave. Where the hash is unknown (an engine without the field, a Save As not yet in the list) the workspace id alone decides.
- An autosave set aside as stale (`stale: true`) is offered even when its workspace is open, since it may hold the newest work (file times are unreliable), and its prompt says it was set aside. 
  Exception: one that was itself loaded into a window in this run (via `open_recovery`, matched by workspace id, path hash and time; or `open` + `restore`, matched by the offer's time and the window's workspace id). A time alone never hides one: a window saved after a set-aside autosave never held its edits.
- When the alarm offers a set-aside autosave of a workspace the reattach reopened from its file with edits lost, the line says an earlier autosave was set aside and can be restored (matched by workspace id). The window's own log line does not claim the edits are gone for good.

## Quit

1. The prompts. 2. `close { doc, discard_changes: true }` for each workspace answered Don't Save, only once no later prompt cancelled the quit (the shutdown would otherwise write a recovery file for it); a failed or stalled close is logged and the quit goes on. 3. `goodbye { shutdown: true }`. The engine leaving after it is no alarm.
- An engine this run started that no connection has reached yet (a quit while it comes up, or Quit Anyway while a new one starts after a crash) gets SIGTERM and no "not responding" prompt: left alone it would run on with the audio device open, since only a dropped app connection makes it relaunch the app.
- No engine is started once the quit is decided. An engine this run talked to, or one it did not start, is never signalled.

## Test engine

The app's tests run the real engine as the Debug-only test engine (`-OpenLabEngine test`, `EngineLaunch.swift`): on the virtual device (skill `devices`), with an Application Support folder of its own, ending with the app (`--watch-stdin`), never relaunching it. Tests: `mac/openLabTests/EngineLifecycleTests.swift`, `EngineSessionTests.swift`, `openLabUITests/EngineAlarmUITests.swift`.

## Pre-Show Check window (app half; engine half is `preshow_check`, skill `engine-control`)

- Window > Pre-Show Check (Shift-Cmd-W), enabled while the workspace's engine connection is up, opens a floating panel that never takes the keys from the workspace window (`PreshowReport.swift`, `PreshowWindowController.swift`).
- Entries are grouped as broken cues, media files, patches without a device, network destinations and engine warnings. Empty groups are left out, the engine's order is kept, information-only entries are dimmed.
- Choosing an entry makes its workspace window key (so the next GO goes there, not to another open workspace) and selects its cue without moving the playhead; in Edit Mode it shows the inspector (Basics tab for file and target problems).
- A cue deleted since the check is marked and cannot be chosen. Check Again reruns it; a lost connection or failed request clears the list rather than leave an old report standing.
- Tests: `PreshowTests`, `PreshowUITests`.
