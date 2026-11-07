---
name: persistence
description: "Load when changing or reviewing how a show is saved, backed up, autosaved, recovered, opened or closed, or per-machine settings.json (engine/crates/engine/src/persist/, control/docs.rs; core backup.rs, recovery.rs, settings.rs). Stored media path form is skill show-file-format."
---

# Persistence: save, backup, recovery, documents, settings

Requirements: NFR-04, NFR-05, NFR-06, NFR-32, WS-03, WS-06, WS-07. Decisions are pure and live in `openlab-core` (`backup.rs`, `recovery.rs`, `settings.rs`, `media_path.rs`); file work is in `crates/engine/src/persist/`.
Every I/O function takes its directories as arguments (`AppDirs::new(root)` is injectable) and times are passed in as Unix milliseconds. Nothing here reads the clock, so tests use temp dirs and virtual time.
The show-file JSON schema and the stored media-path spelling are in `docs/file-format.md` (section 4.3 for media). Wire messages for open/close/recovery are in `docs/protocol.md` ("Opening and closing workspaces").

## Must not break
- **Never overwrite a user file.** A save replaces the show file only by an atomic rename of a fresh temp file; a backup or a moved-aside stale file never replaces an existing name (`rename_no_replace`).
- **Never delete the only copy of work.** Recovery files are removed only after the save that supersedes them has been renamed into place and the folder synced. A failed save leaves them.
- **A workspace that would not load back is never written**, not even as a backup.
- **A failed save changes nothing**: original untouched, temp file removed, the backup made for that attempt removed again (failed attempts must not rotate real backups out).
- **Stale and corrupt recovery files are moved or renamed aside, never deleted on a timestamp comparison** (file times lie on network and removable volumes).
- **No two threads write the same file at once**; no disk call stalls the control thread (see `references/writer-queue.md`).
- **Backups are ordered by the time in the file name, never by mtime**; names not in the backup form are never deleted.
- **The engine never writes `settings.json` when it could not read or preserve it.**
- **One show file is open in at most one doc.**

## Folder layout
`<app-support>` is `~/Library/Application Support` (macOS) or `%APPDATA%` (Windows), from `dirs`.

```
<app-support>/openLab/settings.json                                       per-machine settings
<app-support>/openLab/Backups/<workspace-id>-<path-hash>/<time>.openlab   rolling backups
<app-support>/openLab/Recovery/<workspace-id>-<path-hash>.openlab         autosave of a saved show
<app-support>/openLab/Recovery/<workspace-id>.openlab                     autosave of a never-saved show
```

- `<path-hash>` is `path_hash`: 16 hex digits (FNV-1a) of the show path's comparison key, so it ignores case, NFC/NFD and `..`.
- Why a hash: duplicating a show keeps its workspace id, so the id alone cannot tell the copies apart. With the hash each copy has its own backups and recovery file.

## Saving (`save_workspace`, NFR-04)
1. `save_show` serialises and checks the text loads back; failure is `PersistError::Serialize` and nothing is written.
2. If the file exists and `keep > 0`, its current bytes go to the backup folder. A failed backup is a warning in `SaveReport`, not a failed save.
3. New text goes to `<name>.<pid>-<n>.tmp` beside the show file: unique per write, opened with `create_new` (never follows a symlink or overwrites another file), fsynced, renamed over the original, then the folder is fsynced (Unix, best effort). Any error before the rename removes the temp file, leaves the original and removes the step-2 backup again.
4. Backups beyond `keep` are deleted; recovery files for this workspace and show path (and the pre-first-save one) and their moved-aside stale files are removed, only now.
5. Save As: the recovery file at the path the workspace was saved away from is removed too, only if this engine run wrote it. One already there at open and kept with `ignore` holds edits that are nowhere else and is never touched. A path another open workspace holds, saves to, or has an `open` reading is skipped and looked at again after the next save or autosave.
Removal waits until the new path holds every edit: at once if no edit was made while the save was written, else after the next successful save or autosave there (`Control::clear_left_autosaves`).

Details of the replace:
- **Windows lock retry.** An antivirus scanner or indexer can briefly hold the file. The rename is retried up to 5 times in all, waiting 10, 20, 40, 80 ms, only for `PermissionDenied` / `ERROR_ACCESS_DENIED` / `ERROR_SHARING_VIOLATION` / `ERROR_LOCK_VIOLATION`. Any other error fails at once. Other platforms never retry.
- **Unix mode.** The old file's permission mode is set on the temp file before the rename. Owner and group are the saving user's. Extended attributes, Finder tags, ACLs, creation date and Windows alternate data streams are lost (new inode).
- **Symlinks.** A symlinked show path: the target is replaced and the link left alone, with the temp beside the target. A link that cannot be resolved (dangling, loop) is an error, never replaced by a regular file.
- **Read-only.** A destination with no write bit is refused with `PermissionDenied` ("the file is read-only"), left untouched, and the attempt's backup is removed.
- **Leftover temps.** A crash-leftover `<name>.<pid>-<n>.tmp` is not touched by the next write. If its name is one the write would use (same pid reused), the write takes the next `<n>` (up to 16 names). After a successful save, temps beside the show file that belong to another process and are at least 60 s older than the file just saved are removed. Younger ones may be another instance mid-write.
  This process's own temps and a user's own `.tmp` files (the name must match exactly) are never removed.

## Backups (NFR-06)
- Named `YYYYMMDD-HHMMSS-mmm.openlab` in UTC, so names sort in time order. `select_backups_to_delete(names, keep)` keeps the `keep` newest.
- Two saves in the same millisecond get consecutive names. A backup is written and synced under a temp name, then put in place with `rename_no_replace` (a hard link, which fails on an existing name, then the temp removed; on a file system without hard links, a check then a rename). A taken name tries the next millisecond, up to 1000; all taken means a failed backup (the save warns and goes on).
- Stale recovery files are moved aside the same way (up to 100 names; all taken leaves the file in place).
- Why name order: a clock stepped backwards must not make the newest backup rotate out first.
- Rotation also removes backup temps (`<backup name>.<pid>-<n>.tmp`) left by a crash, provided the backup name is not newer than the newest finished backup (a newer one may be an instance still writing).
- `keep` is `MachineSettings::backup_keep` (default 20, at most 1000). 0 turns backups off and leaves existing ones alone.

## Autosave and recovery (NFR-05)
- `write_recovery` writes the bound recovery file atomically and never touches the show file. `clear_recovery` removes it and the files moved aside from it after a save. `discard_recovery` removes only the current file (`close` with `discard_changes`).
- `check_recovery(dirs, workspace_id, show_path)` compares file modification times with `decide_recovery`:

| Situation | Result |
|---|---|
| No recovery file for this show path | `None` |
| Recovery newer than the show file, or the show file missing | `Offer`, carrying the parsed workspace |
| Recovery not newer (equal counts as not newer) | `Stale`: moved aside to `<stem>.<modified-ms>.stale.openlab`, newest 5 kept, never deleted. A successful save of the workspace deletes them |
| Unreadable, invalid, or for another workspace | `Unusable { warning }`; renamed `.corrupt` (`.corrupt.1`, ... if one exists) so the warning is not repeated and earlier evidence is kept |

- `other_copies`: recovery files with the same workspace id but another show path (a duplicated show, or work from before the first save). Never offered to this file, never touched; reported as a `recovery_other_copies` notice at open.
- The restore flow is the protocol's: `open` answers `recovery_offer` while a newer autosave exists and nothing is opened; the client repeats it with `recovery: restore` or `ignore`.
- `restore` opens the autosave bound to the show path and dirty; the autosave file stays until a save (or the next autosave) replaces it, so a crash right after restoring loses nothing.
- `ignore` opens the saved file and leaves the autosave in place (offered again next time).
- `close` with `discard_changes` over unsaved edits removes the autosave this doc wrote of the dropped edits through the writer, after any autosave write in flight. One restored into the doc and not written since, and files moved aside, stay and are offered again. There is no `revert`, and `new`/`open` replace nothing.
- Notices: `recovery_other_copies`, `recovery_unusable`, and `load_repair` for load repairs (at most 50, then a summary).

### When the show file cannot be loaded
`check_recovery` needs the workspace id, which comes from the show file. When that file is corrupt or gone, `scan_recovery(dirs, max_entries)` lists `Recovery/` instead.
- Read-only. One entry per autosave, newest first, with the id and hash from the file name and a show name read from the file when it parses. A damaged file is still listed without a name.
- Files moved aside as stale are listed `stale: true` with the time from their name. Why: a stale file may be the only copy of newer work. `.corrupt` / `.corrupt.N` files are hidden.
- Names are parsed strictly (lower-case hyphenated UUID, optional `-` and 16 lower-case hex digits; `parse_recovery_file_name` / `parse_store_name`). Anything else is skipped with a warning.
- Bounds: at most `max_entries` returned (`MAX_RECOVERY_ENTRIES` default); newest by mtime among the first 5000 directory entries (more is `truncated`); no recursion; a file over `MAX_NAME_READ_BYTES` (4 MiB) is listed without a name instead of being parsed.
- It parses files, so it runs on the document worker, never the control thread. `open_recovery` opens one entry, dirty.
- An autosave names its show file: `core::save_recovery` writes a top-level `show_path` (UTF-8 paths only; a recovery file's name holds only a hash of the path, and a show file never has the key).
- `open_recovery` binds to that file, as `open` with `restore`, so relative media resolves, Save goes to the file and the app reopens it next launch, only when all hold: it is the current autosave, not one set aside; `decide_recovery` says it is newer than the file; `show_path` is absolute, at most 4096 bytes, and hashes to the hash in the file's name; the file reads, loads and holds the entry's workspace id; no other doc holds it.
- Otherwise it is **untitled**: a save chooses a path, the autosave stays until then, and a Save As clears the never-saved autosave of the workspace id. When it named a file that could not be used, a `recovery_untitled` notice says why. A file that does not answer holds up the whole request, as an `open` would (`io`, past the read timeout).

## Documents in the control loop (`control/docs.rs`)
Request shapes: `docs/protocol.md`. `new`, `open` and `open_recovery` open another doc beside the others; nothing is replaced.
- **Nothing blocks the control thread.** A save or autosave is a clone of the workspace handed to `SaveWriter`, tagged with the revision. The loop polls answers every tick and answers `save`/`save_as` only after the data is durable (a `document` broadcast with the new path and dirty flag goes first).
- Saves of one file in flight collapse in the writer (older answer `Superseded`). The loop answers the older request with the outcome of the newest save of that file whose revision is at least its own.
- A failed write answers `io` and changes nothing (still dirty, same path). A save of a doc closed meanwhile is answered when it finishes and changes nothing.
- **Reads run on the document worker** (`open`, `open_recovery`, `scan_recovery`). Only regular files are read: opened non-blocking (`O_NONBLOCK` on Unix) and the type checked on the opened handle, so a FIFO or device (even one swapped in after a path check) is `io`, never waited on. One such request at a time (`command_refused` for a second).
- **A read that stalls** is bounded: after `ControlConfig::doc_timeout` (20 s, on the injected `EpochClock`) the request answers `io`, the worker is abandoned (its late answer carries an old ticket and is ignored) and a fresh worker takes the queue. With 4 abandoned workers still stuck (`MAX_STUCK_WORKERS`), requests that read are refused with `io` until one returns.
  A timeout firing while the worker is idle (answer already in the inbox) leaves the request to that answer. An edit made while a file is read stays in its doc; the file opens beside it.
- **One show file, one doc.** Claimed files are every doc's show path, every save in flight (a Save As claims its path when written), the file an `open` in flight reads, and closed docs' saves still running (`Control::claimed_paths`).
  `open` and `save_as` of a claimed file answer `already_open`: first on the control thread by spelling (no disk access), then off it by the file itself (`persist::file_key`: canonical path of the file or its folder), by the document worker before it reads and the writer before it writes (`PersistError::OpenElsewhere`), and once more when a read doc is installed. Why file identity: a symlink or `/var` for `/private/var` must be caught.
- **Opening.** A workspace whose id an open doc already has (a copy of a show) is installed with a fresh id and dirty, with an `info` log. The requester gets `opened { doc }`; the doc's snapshot goes to the connections bound to it and the `workspaces` list to app connections.
- **The `workspaces` list.** After each wake (and at once after open/close) the loop compares each doc's id, workspace id, name, path and dirty flag with what it last listed (no allocation if unchanged) and sends `workspaces` (`doc: None`, app connections only) when they differ. A list that did not fit the outbound channel is resent at the next wake. `Control::new` lists its first doc before any of that doc's reports.
- **Closing.** The doc is removed, the requester gets its `result`, then `closed { doc }` goes out tagged with the doc (resent at the next wake if it did not fit), so the network layer writes it to the doc's connections and closes them after the result.
- **Revision and seq are per doc.** `revision` starts at 1, `runtime_seq` at 0; a connection sees one doc's.
- **Document state.** `document` (`path`, `dirty`, `can_undo`, `can_redo`): dirty is `revision != saved_revision`. Opening an autosave installs it dirty (`saved_revision = revision - 1`).
- **Autosave pacing.** Each tick, if the workspace is dirty, no autosave is in flight, the revision was not already autosaved, and `autosave_due(dirty_since, last_autosave, now, interval)` says so (`interval` is `autosave_interval`, at least 1 s; `now` is `ControlConfig::clock`, tests inject `ManualClock`), the loop hands a clone to the writer, bound to the show path (or the workspace id before the first save).
  So the first autosave is one interval after the workspace became dirty, later ones one interval after the previous. A failed autosave is logged once per run of failures and retried at the next due time. The writer drops an autosave whose revision a manual save already made durable.
- **Save As to another folder rebases relative media** so references name the same files. The copy being written is rebased first (`relink_command` on the clone).
  When the write succeeds the live workspace gets the same relinks without an undo step: history is cleared, `revision` rises, the doc's clients get a `snapshot`, the media catalog resets to the new folder (every file probed again) and the workspace is clean unless edited meanwhile.
  An absolute path literally inside the new folder is made relative; one outside it is never touched (stored form: `docs/file-format.md` section 4.3). Relative references of a never-saved workspace have no folder to resolve against and are left as they are.
  For the first Save As of a never-saved show history is kept (every path was absolute; undo restores a spelling that still names its file).
- **Recent files are the app's** (WS-08): the engine keeps no list, and `load_settings` drops a `recent_files` key.
- **Shutdown** writes a final recovery file for every doc with unsaved changes, waited for, and must come before the docs are dropped (see `references/writer-queue.md`).

## Per-machine settings (WS-06, WS-07)
`settings.json` is never part of a show file. Settings that travel with the show (hotkeys, autosave interval, passcode) stay in the show file.

```json
{
  "settings_version": 1,
  "backup_keep": 20,
  "output_device": "Scarlett 2i2",
  "workspaces": {
    "<workspace-id>": {
      "patches": { "<patch-id>": { "device": "Scarlett 18i20", "channels": [4, 5] } }
    }
  }
}
```
- `channels[i]` is the device channel (0-based) for logical output `i` of the patch.
- `load_settings` never fails: unreadable content gives defaults and a warning; invalid entries are dropped with a warning; `backup_keep` is clamped. Workspace and patch ids are normalised to lower-case hyphenated form. Unknown keys are ignored but kept on rewrite (`MachineSettings::extra`).
- A file that produced warnings is first copied to `settings.json.corrupt` (`.corrupt.1`, ... a few at most) before anything overwrites it.
- **Auto-map.** A patch with no entry on this machine (new workspace, show from another Mac, patch added later) is mapped to the first N outputs of the device the mixer plays on, when it has at least N, and the file is rewritten atomically off the control thread. Existing entries are never overwritten. The operator's `set_patch_mapping` also rewrites the file. Default-mapping rules: skill `engine-control`.
- An entry `{"device": "", "channels": []}` means the operator cleared the mapping. It is kept, so the patch stays unmapped and is not auto-mapped.
- **An external edit of `settings.json` while the engine runs is overwritten at the next rewrite** (it writes the in-memory copy).
- If the file could not be read at start (an I/O error, not "missing"), or it produced warnings and its `.corrupt` copy could not be made, the engine keeps default mappings in memory only, warns once and never writes the file (`ControlConfig::machine_writable`). If the settings thread cannot start or has stopped, mappings stay in memory with a warning.
- At shutdown the engine waits at most `DROP_WAIT` (5 s) for the settings thread's pending write. A write stuck on a hung volume is left to finish on its own, so it never holds up quit or relaunch.
- `resolve_patch(settings, workspace_id, patch, devices)` gives `Mapped`, `Unmapped` (ask the operator once), `DeviceMissing`, `ChannelsMissing` or `OutputCountMismatch`. The last three leave the patch silent and its cues broken, never a crash.
- Recent files and open-on-launch (WS-08) are app-side. Per-cue-type defaults are deferred (REQUIREMENTS §21).

## Media paths (WS-03, NFR-32)
Stored spelling (forward slashes, NFC, relative only when literally inside the show folder) is `docs/file-format.md` section 4.3. Implementation rules:
- `portable_ref(native, show_dir, style)` builds the stored `MediaRef`. Windows backslashes become `/` (relative too); on macOS a backslash is a file-name character and is kept.
- An absolute path becomes relative only when it is literally under the show folder: same components byte for byte, no case folding, no `.` or `..`. Why: a different case may be another folder on a case-sensitive volume, and `..` may pass through a symlink; deciding either needs the file system, and the control thread that stores the path never touches it. Such a path stays absolute exactly as sent.
- A path already relative is kept as written (`../Audio/x.wav` stays), so copying a cue never changes how a hand-written path reads. `resolve(media, show_dir)` is the way back.
- `comparison_key` (lexical normalisation, NFC, `to_lowercase`; not full Unicode folding) decides whether two paths name the same file on macOS/Windows (find-in-folder, relink, `path_hash`). Storing a path relative does not use it.
- Exactly two leading slashes are a network share root (`//server/share`); three or more are one root (`///x` is `/x`).
- A drive-relative `C:foo` is not absolute: a relative name whose first component is the literal `C:foo`. It resolves inside the show folder to a file that does not exist, so the cue shows missing. It never reads the current directory of drive C.
- Non-UTF-8 file names cannot be stored in JSON and are skipped (and counted) when scanning.
- Where the engine converts client-sent paths (`Command::prepare`): skill `edit-commands`. Missing-file detection and relink: skill `media-files`.

## References
- `references/writer-queue.md`: read when touching `persist/writer.rs`, the writer's coalescing, stall watchdog, shutdown final write or `SaveWriter` drop behaviour.
