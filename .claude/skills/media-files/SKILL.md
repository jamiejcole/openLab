---
name: media-files
description: "Load when changing or reviewing when media probes run, the probe pool and preload scheduling, the stalled-volume gate, the media watch thread and its watchdog, missing/back/changed-file detection, or relink (engine/crates/engine/src/control/media/, control/watch/, persist/relink.rs; core relink.rs)."
---

# Media probes, volume gate, media watch, relink

Anything here must survive a slow, hung or vanished disk or network share without stopping the show or breaking healthy cues. Stored path spelling is `docs/file-format.md` section 4.3; path conversion rules are in skill `persistence`.
Hold limits for a GO on an unanswered probe (`PROBE_HOLD_LIMIT`) and on a stream that never opens (`STREAM_START_LIMIT`), and priming: skill `engine-control`.

## Must not break
- **The control thread never stats, reads or walks a file or folder.** Probes, preloads, watch stats and relink walks run on their own threads.
- **A healthy disk never makes a cue broken.** A probe that is merely slow is never failed early. Nothing playing is touched by a re-probe.
- **The system volume is never gated; the show file's volume is exempt only while one doc is open** (with several, see "Never gated").
- **A volume counts as stalled only on corroboration** (the watch says so, or two different files' probes are stuck on it). One slow file is not a dead mount.
- **A half-copied file is never probed**: Back and Changed need two identical watch passes.
- **Relink never guesses.** Ambiguous matches are left to the operator; a match whose cue changed meanwhile is dropped (the edit wins); one relink is one undo step.
- **The watch cannot be starved or silenced by one hung volume**: stuck stats are abandoned and replaced, bounded, with a notice.
- A stuck thread is **leaked, never joined**; no code path blocks on it.

## Probe order and stuck workers
- Probes go out in workspace order on a low-priority queue. Every priming pass (playhead move, edit, probe answer) puts the files of the cues it wants primed on the high-priority queue, which workers always empty first. After a restart mid-show the cue at the playhead is not behind a hundred others.
- A copy of a job a worker already took (the urgent copy priming sends) is dropped, keyed by (doc, probe or preload, file), so a probe and a preload of one file never shadow each other.
- A worker busy on one file longer than `PROBE_TIMEOUT` (wall clock; the only wall-clock rule in the loop) is abandoned and replaced, at most `MAX_ABANDONED_WORKERS` times. At the limit one `error` is logged and further stuck probes just wait; their starts fail after the hold limit.
- A replacement that cannot start (out of threads) is logged once as an error and retried every tick until the pool is whole again (then `info`).
- Each give-up is one `error` log per replaced worker, saying whether a probe or a preload stuck and on which file (last 200 characters), plus the workspace's name while several are open.
- A file with no answer yet is reported `unsupported` ("could not be read in time") meanwhile; a late answer still counts.
- A re-probe (after a stream failure, or a watch event) **keeps the last answer**, and so the cues' primes and any preload, until the new answer comes. If it too gets stuck, the last answer stands: a ready file stays available, a failed one keeps its problem, and nothing playing is touched. A long file on a slow disk gets past the timeout as well; a start on a volume that really hangs fails by `STREAM_START_LIMIT`.
- After `MAX_STUCK_JOBS_PER_FILE` of a file's probes have stuck with no answer since, only a watch event asks again. Why: one hung file whose stream keeps failing must not use up the workers. The next stream failure otherwise asks afresh.

## Stalled-volume gate
A volume is the mount a path is on (`bucket_of`, same names as the watch: `/Volumes/<name>`, `/mnt/<name>`, `/media/...`, a drive or share). Everything else, the system disk included, is `/`, so all local files in the user's folders share one volume.

**Never gated** (probes there run as if no gate existed: a stuck worker is replaced, a GO waits the usual hold):
- `/` (it mixes the local disk with automounts and cloud folders, so a verdict there is meaningless, and most media live there);
- on Windows the system drive (`%SystemDrive%`, `C:` if unset, compared without regard to case);
- the volume holding the open doc's show file (found from the show file's own path, so a show at a volume's root counts), only while ONE doc is open. With several the workers are shared, so that volume is held back once two of its probes are stuck past the watchdog, never on the media watch's word alone. Held-back jobs release urgent first.

**When a volume is stalled** (only other volumes can be):
- Stalled only when corroborated: the media watch reports it (`MediaWatch::stalled_volumes`, fed to the catalog every tick; its `media_volume_not_responding` is the only notice), or two different files' probes are stuck on it. Preloads never count. A probe-side verdict is an `info` log only, in different words from the watch's notice.
- The volume gets at most one trial job in flight. Its other jobs wait in a deferred list: no worker taken, one job per file and kind, an urgent request stays urgent. The files stay pending with any last answer. **Nothing is failed early**, so a GO on such a file is held and fails by the usual hold limit (plays if the share answers in time).
- Jobs on healthy volumes always run.
- A trial that gets stuck stops blocking the next one: its worker is abandoned as above and the next trial waits `RETRY_FIRST` (30 s), doubling to `RETRY_MAX` (5 min). A trial that fails with an I/O error waits the same way.
- Waiting files are never stranded: a stalled volume always gets its next trial after the backoff. Any answer from the volume (even "file missing") ends the probe-side stall, forgets stuck probes that never return, and releases every waiting job. A volume the watch sees healthy again stops being stalled at once.
- `reset()` (a new workspace in the only open doc) forgets all volume state; jobs still running from before cannot touch the new state (epoch). With other docs open the volume state is shared and stays, and only the resetting doc's claims and waiting jobs go, so a job that never returns (a FIFO, a hard-mounted share) cannot stall a volume for ever.
- Tests name the volume they need (`probe_volume`): a checkout on a Windows CI runner is on `D:`, neither the system drive nor a show's volume, so there it is an ordinary other volume and correctly stalls.
- QLab does not mark media unsupported for a slow read; this follows it. QLab's behaviour on a non-answering volume is otherwise undocumented to us.

## Media watch
Design: path first, no file-watching API, no OS bookmarks (opaque per-OS blobs in a cross-platform JSON file; resolving one can mount or block a volume). A stat is metadata only.

**Sharing.** One watch serves every doc (`control/watch_share.rs`, `WatchShare`). Each catalog's key changes name files by that doc's keys (media references relative to its show folder). The watch thread is told each file once, by path, under an id of the engine's, with a count of the doc keys naming it.
- One doc letting go of a file (edit, reset, close) leaves the watch running for the others. An event goes to every doc key that names its file, under that doc's key and id; an event of an earlier watch id of the file is dropped.
- A doc's reset is the watch's own `Reset` when no other doc watches anything; otherwise only that doc's files are let go.
- The watch's notices and logs are the engine's: every doc lists the stalled-volume notices.

**Jobs.** `MediaWatch` (`control/watch/`) sends the watch thread `Watch`/`Unwatch` jobs for the keys the catalog gains and loses (`MediaCatalog::take_key_changes`, drained each tick), and a `Reset` when the catalog resets (the show saved into another folder). A generation number drops a batch of an older generation.
- Each `Watch` carries a watch id (the catalog entry's first probe ticket). Every event carries it back, and the catalog ignores an event whose id is not its entry's current one (a file unwatched and watched again by an undo while a pass was under way).
- A reference with no place on disk (relative, in a show never saved) is not watched; its probe has already failed.

**Detection pacing.** A full pass covers every file once in `max(period, n x 2 ms)`, `period` 3 s, in chunks of 64 with a pause between them. Jobs are taken between chunks and before each stat. A stat is a syscall (4-7 us on a slow CI Intel runner), so this keeps the watch near 0.3% of a core (2,000 files: 4 s).

**Batches.** The worker answers `WatchAnswer::Batch { generation, events }`, one per pass and only when non-empty, so an unchanged show costs control nothing (`watch_batches()` stays 0 over 2,000 files). Batches are kept until the next tick and handled at its start, before probe answers go to the engine.

| Event | Control does |
|---|---|
| `Gone { key, trashed }` | `MediaCatalog::mark_missing` (as a NotFound probe: probe failed, preload dropped, budget returned, the ticket moves on so an in-flight probe answer is ignored). `Missing` goes in the tick's single `Availability`, so the cue is broken within the tick |
| `Back` / `Changed` / `Unreadable` | `MediaCatalog::recheck`: a fresh probe even if one is under way (it may have read the old file). It goes to the high-priority queue when the cue is near a playhead. The answer becomes `Availability` as for any probe; an unreadable file probes as `media_unsupported` |

A moved file is simply `Gone`; there is no follow.

**Pass rules** (`WatchCore::record`, pure):
- A file that is not there, or not a regular file, is `Gone` on the **first** pass that sees it (a file in the Trash breaks its cues within seconds).
- A different file at the path (identity, size or modified time), or a file back, is reported only when two passes in a row see it the same.
- An unreadable path is reported once per change.
- The first pass after a file is watched takes a file it finds silently (its probe is under way; if that probe finds no file, see Failed probes).
- Playing cues are unaffected: the voice holds an open file, and a static problem shows once the run ends.
- Windows and network/FAT volumes: Gone, Back, Changed and Unreadable by stat only.

**Watchdog** (every tick, `MediaWatch::supervise`, on the monotonic `WatchConfig::clock`). The watcher publishes each stat under way (a number of its own, and the volume, taken from the path text because a stat could hang on exactly that volume).
- A stat is stalled once the watchdog has seen the watcher in that same stat for `WATCH_STALL` (10 s), timed from when it first saw it, so a clock jump does not stall a stat that was under way.
- The watch state (files, pass position, events found so far) lives in one `WatchCore` shared by the watchers, locked only between stats, and the job queue is shared too. A replacement carries on where the stuck one stopped and loses nothing. Jobs are taken and the volume's skip checked before every stat.
- **Abandon.** Decided under the watcher's busy lock as a compare-and-set on the stat judged stalled. The watcher clears its stat and reads the flag under the same lock, so a stat that returns as the watchdog fires is either answered (not abandoned) or reports its volume (abandoned), never neither. Control queues `Skip { volume }` and starts a fresh watcher (`openlab-watch-<n>`). The volume's files keep their last status; the notice `media_volume_not_responding` names it (and a `warn` log).
- **Return.** The abandoned thread's stat returns, sends `WatchAnswer::BucketAnswered { healthy }` and ends (`abandoned_watchers()` counts threads still stuck). `healthy` (a file, or no file) means `Unskip` and the notice goes.
  An error (a dead share's ETIMEDOUT) keeps the volume skipped and retries after `RETRY_FIRST`, doubling to `RETRY_MAX`: a retry is an `Unskip { trial }` whose next stat is reported in the batch's `volumes`; a failing trial waits again. The notice stands from the first stall until the volume answers, through every retry (no flapping).
- **Cap.** At most `MAX_ABANDONED_WATCHERS` threads stuck at once. A stall beyond that abandons nothing: `media_watch_stopped` (naming the volume) and an `error` log stand until that stat returns; the volume is then skipped and retried like a failing one, so it cannot hold the only watcher again.
- A watcher thread that ends on its own (a bug) is logged (`error`) and restarted once; if it ends again, `media_watch_stopped` stays.

**Failed probes.** A probe answer that fails tells the watch (`KeyChange::ProbeFailed`).
- No file: the watch takes the file as gone, so a file that appears later is reported `Back` (after two passes) even if it appeared before the watch first looked (a show opened before its drive is mounted).
- Another error while the path is unreadable: the file's return is reported even if unchanged.
- Otherwise a file unreadable for a while that comes back unchanged is not reported again (its re-probe on `Unreadable` already read it). A failed probe of a readable file changes nothing: no re-probe loop on a file that stats but cannot be decoded.

**Tests.** The watch takes an injected `WatchFs` (`FakeFs`: file identities, the Trash, denied and hung paths; feature `test-support`, never in the shipping binary). With `ControlConfig::watch.mode = Manual`, `OfflineShow::watch_pass()` runs one pass on the test thread and handles its batch in a tick. Watchdog tests run the thread on `FakeFs` with a `ManualClock` and wait for messages, never sleeping.

## Relink (WS-03)
Core (`relink.rs`):
- `missing_media(workspace, show_dir)` lists Audio cues whose file does not exist.
- `find_in_folder(workspace, show_dir, folder, limits)` scans `folder` and matches missing files by normalised file name. Defaults: 16 levels deep, 200,000 files, 50,000 folders, 20 s. Symlinked folders are not entered, so links cannot loop. The clock is injectable (`scan_folder_with`). The result is a `RelinkPlan`: `matches`, `ambiguous` (several candidates, never guessed), `unmatched`, and whether a limit truncated the scan. Nothing is changed.
- `relink_command(workspace, relinks)` turns chosen relinks (a plan's matches, one picked from `ambiguous`, or a single cue) into one `Command::Transaction` of `SetCueProperty` edits: a single undo step through `History`.

Control side: `relink { folder }` (bound connection, its doc; WS-03, NFR-04).
- `Control::relink` clones the workspace and starts an `openlab-relink` thread. The thread checks the folder (`not_found`; `bad_request` if it is a file), runs `find_in_folder` within `ControlConfig::relink_limits`, and posts `DocAnswer::Relinked`. With no media missing it does not walk the folder and answers at once.
- **The control thread never touches the folder.** It drops matches whose cue no longer has the reference the walk started from, applies the rest with `relink_command` through `History` (one undo step, `model_changed`, dirty) and answers with the counts and `truncated`.
- The walk has a slot of its own beside the document-request slot (`Docs::relink`; one walk engine-wide, a second `relink` is `command_refused`). So `open`, `open_recovery` and `scan_recovery` are neither refused nor queued behind a walk that takes its whole time budget. The walk works on its copy and its matches are checked against the doc when applied, so nothing races an open or an edit.
- The stall check allows the walk's own time budget on top of `doc_timeout`. A walk stuck on a hung volume fails with `io`; its thread is abandoned and counted with the stuck document workers (`MAX_STUCK_WORKERS`: past it, requests that read are refused with `io`).
- If the thread cannot start the request answers `internal`, changes nothing and leaves no walk counted, so the next `relink` is not refused (`ControlConfig::fail_relink_spawn`, feature `test-support` only).
- If the doc closes during the walk the answer is `no_workspace`.
