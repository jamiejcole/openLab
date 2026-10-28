# Persistence: saving, backups, recovery, settings, media paths

Requirements: NFR-04, NFR-05, NFR-06, NFR-32, WS-03, WS-06, WS-07. The decisions are pure and live in
`openlab-core` (`backup.rs`, `recovery.rs`, `settings.rs`, `media_path.rs`, `relink.rs`); the file work
is in `crates/engine/src/persist/`. Every I/O function takes its directories as arguments, and
`AppDirs::new(root)` is injectable, so tests use temp dirs. Times are passed in as milliseconds since
the Unix epoch; nothing here reads the clock.

## Folder layout

`<app-support>` is `~/Library/Application Support` (macOS) or `%APPDATA%` (Windows), from `dirs`.

```
<app-support>/openLab/settings.json                      per-machine settings
<app-support>/openLab/Backups/<workspace-id>-<path-hash>/<time>.openlab   rolling backups
<app-support>/openLab/Recovery/<workspace-id>-<path-hash>.openlab         autosave
<app-support>/openLab/Recovery/<workspace-id>.openlab                     autosave, never saved
```

`<path-hash>` is `path_hash` of the show file's path: 16 hex digits (FNV-1a) of its comparison key, so
it ignores case, NFC/NFD and `..`. Duplicating a show keeps its workspace id, so the id alone cannot
tell the copies apart: with the hash, each copy has its own backups and its own recovery file.

## Saving (NFR-04)

`save_workspace(dirs, path, workspace, now_ms, keep)`:

1. `save_show` serialises and checks the text loads back. A workspace that would not load back is
   refused with `PersistError::Serialize`; nothing is written, not even a backup.
2. If `path` exists and `keep > 0`, its current bytes are copied into the backup folder. A failed
   backup is a warning in `SaveReport`, not a failed save.
3. The new text is written to `<name>.<pid>-<n>.tmp` beside the show file (unique per write, opened
   with `create_new`, so it never follows a symlink or overwrites another file), fsynced, renamed over the
   original, and the folder is fsynced (Unix; best effort). On any error before the rename the temp
   file is removed, the original is untouched, and the backup made in step 2 is removed again so
   failed attempts do not push real backups out of the rotation.
4. Old backups beyond `keep` are deleted; the recovery files for this workspace and show path (and the
   pre-first-save one) and their moved-aside stale files are removed. This happens only after the rename
   and folder sync succeeded, so a failed save leaves the recovery file in place.

Details of the replace in step 3:

- **Windows lock retry.** On Windows an antivirus scanner or search indexer can briefly hold the
  show file, and the rename then fails with a sharing or access error. The rename is retried up to 5
  times in all, waiting 10, 20, 40 and 80 ms (150 ms at most), only for `PermissionDenied` /
  `ERROR_ACCESS_DENIED` / `ERROR_SHARING_VIOLATION` / `ERROR_LOCK_VIOLATION`. Any other error fails at
  once. Other platforms never retry. The rename, the sleep and the policy are injected in tests.
- **Unix permissions.** The new file gets the old file's permission mode (set on the temp file before
  the rename), so a `0640` show stays `0640`. Owner and group are not carried over: they are the saving
  user's.
- **Symlinks.** If the show path is a symlink, the file it points to is replaced and the link is left
  alone; the temp file is made beside the target. A link whose target cannot be resolved (dangling,
  loop) is an error, never replaced by a regular file.
- **Read-only.** A destination with no write permission bit is refused with a `PermissionDenied`
  error ("the file is read-only") and left untouched; the backup made for that attempt is removed
  again.
- **Not preserved (v0.0.1).** Extended attributes, Finder tags, ACLs and the creation date belong to the
  old inode and are lost when the file is replaced. Windows alternate data streams likewise.
- **Leftover temp files.** A `<name>.<pid>-<n>.tmp` left by a crash is not touched by the write that
  follows it; if its name is the one a write would use (a leftover of an earlier process with the
  same pid), the write takes the next `<n>`, up to 16 names, rather than failing. After a successful save, those beside the show file that belong to another process and
  whose modification time is at least 60 s older than the file just saved are removed; younger ones may
  be another instance's write in progress. This process's own temp files and a user's own `.tmp` files
  (the name must match exactly) are never removed.

`write_atomic_with` takes a hook called after the temp write and after the fsync, for
failure-injection tests.

Serialising plus the verify-reload costs up to about a second for a large show, so saves run on the
writer thread: the control thread clones the `Workspace` and calls `SaveWriter::save` /
`SaveWriter::autosave`, tagged with the workspace's edit revision, and gets a channel with the result.
The queue is bounded by coalescing, because the disk may be slow or hung while autosave keeps firing:

- one pending manual save per target file (a newer one replaces it; the old answer is `Superseded`);
- one pending autosave per recovery target, i.e. per workspace id and show path (the pair that names the
  recovery file; a newer one replaces only its own target's, the old answer is `Ok(None)`), so several open
  workspaces never displace each other's autosaves;
- at most `MAX_PENDING_TARGETS` (16) targets wait in the normal queue; a new target beyond that moves the
  oldest waiting autosave to a write-through queue that runs first, so the cap itself never costs an
  autosave. That queue still coalesces per target and is capped at `MAX_FORCED_TARGETS` (32): during a
  long disk hang, closed workspaces and Save As paths add targets beyond the open ones (8). When it is
  full the oldest forced autosave is shed (`Ok(None)`, counted by `shed_autosaves()`); a later autosave of
  a still-open workspace replaces it anyway. Shutdown still discards pending autosaves, so a caller that
  needs a final autosave (M4.15g) must wait on its reply;
- manual saves run before pending autosaves, and an autosave whose revision is not newer than the last
  durably saved one of its workspace is dropped (`Ok(None)`), so saved state never gets a recovery file;
- `clear_recovery(workspace, show_path)` runs after the job in flight of its target (no two writer threads
  ever write the same workspace's or show file's files at once), removes that target's file, and cancels
  only that target's pending autosave;
- a job stuck on a hung volume for the document timeout is answered `Stalled` and its thread abandoned; a
  fresh thread serves the queue (at most `MAX_WRITERS`, 5, threads in all), so other shows go on saving,
  and the stuck show's saves and autosaves are refused at once until its write returns
  (`SaveWriter::check_stalled`; docs/control.md "A save that stalls");
- dropping the writer finishes the running jobs and pending manual saves, and discards pending
  autosaves.

## Backups (NFR-06)

Named `YYYYMMDD-HHMMSS-mmm.openlab` in UTC, so names sort in time order
(`backup_file_name`/`parse_backup_name`). `select_backups_to_delete(names, keep)` keeps the `keep`
newest. Files whose names are not in that form are never deleted. Two saves in the same millisecond get
consecutive names. **A backup never replaces a file**: it is written and synced under a temp name, then
put in place with `rename_no_replace` (a hard link, which fails on an existing name, then the temp name
removed; on a file system without hard links, a check then a rename), trying the next millisecond's name
when one is taken, up to 1000; every name taken is a failed backup (the save warns and goes on). Stale
recovery files are moved aside the same way (up to 100 names; all taken leaves the file in place). Order is always the time in the file name, never the file's modification time, so a
clock stepped backwards cannot make the newest backup rotate out first. Rotation also removes backup temp
files (`<backup name>.<pid>-<n>.tmp`) left by a crash, provided the backup name is not newer than the
newest finished backup (a newer one may be an instance still writing). `keep` is `MachineSettings::backup_keep` (default 20, at most 1000); 0 turns backups
off and leaves existing ones alone.

## Autosave and recovery (NFR-05)

- `autosave_due(dirty_since, last_autosave, now, interval)` says when to autosave; `interval` is the
  workspace's `autosave_interval`.
- At shutdown (M4.15g) every workspace with unsaved changes gets a final recovery write, waited for
  (bounded: docs/control.md "Documents"), so a quit or `SIGTERM` loses nothing an autosave interval
  would have; the next launch's `open` offers it, and `scan_recovery` lists it.
- `write_recovery` writes the bound recovery file atomically and never touches the show file.
  `clear_recovery` removes it (after a save or clean close).
- `check_recovery(dirs, workspace_id, show_path)` compares file modification times with
  `decide_recovery`:

| Situation | Result |
|---|---|
| No recovery file for this show path | `None` |
| Recovery file newer than the show file, or the show file is missing | `Offer`, carrying the parsed workspace |
| Recovery file not newer (equal counts as not newer) | `Stale`: moved aside to `<stem>.<modified-ms>.stale.openlab`, newest 5 kept; **not deleted** (file times are unreliable on network and removable volumes). A successful save of the workspace deletes them. |
| Recovery file unreadable, invalid, or for another workspace | `Unusable { warning }`; the file is renamed `.corrupt` (`.corrupt.1`, … if one exists) so the warning is not repeated and earlier evidence is kept |

`check_recovery` also returns `other_copies`: recovery files with the same workspace id but another show
path (a duplicated show, or work from before the first save). They are never offered to this file and
never touched; the engine reports them as a `recovery_other_copies` notice when the show is opened.

The restore prompt is the protocol's (M4.4): `open` answers `recovery_offer` while a newer autosave exists and nothing is opened; the client repeats it with `recovery: restore` or `ignore` ([protocol.md](protocol.md) "Opening and closing workspaces"). Restoring opens the autosave, bound to the show path and dirty; the autosave file stays until a save (or the next autosave) replaces it, so a crash right after restoring loses nothing. `ignore` opens the saved file and leaves the autosave in place (it is offered again next time). `close` with `discard_changes` over unsaved edits removes the autosave of the dropped edits through the writer thread, after any autosave write in flight (M4.15f: there is no `revert`, and `new`/`open` replace nothing). `other_copies` of a recovery check are summed in a `recovery_other_copies` notice in the workspace warnings; an unusable autosave is a `recovery_unusable` notice. Load repairs of the opened show are `load_repair` notices (at most 50, then a summary).

### When the show file cannot be loaded

`check_recovery` needs the workspace id, which comes from the show file. When that file is corrupt or
gone, `scan_recovery(dirs, max_entries)` lists `Recovery/` instead: one `RecoveryEntry` per autosave
(`workspace_id` and `path_hash` from the file name, `show_name` read from the file when it parses and holds
that workspace, `modified_ms`, `stale`, `path`), newest first. It is read-only. A damaged file is still
listed, with the id from its name and no show name. Files moved aside as stale (`<stem>.<ms>.stale.openlab`)
are listed with `stale: true` and the time from their name (the file's own mtime if that does not parse):
file times are unreliable, so a stale file may be the only copy of newer work. `.corrupt` / `.corrupt.N`
files are hidden. Names are parsed by core's `parse_recovery_file_name` / `parse_store_name` (strict:
lower-case hyphenated UUID, optional `-` and 16 lower-case hex digits; the same parser reads the workspace
id of a `Backups/<id>-<hash>` folder name). Anything else is skipped with a warning in
`RecoveryScan::warnings`. Bounds: at most `max_entries` files returned (`MAX_RECOVERY_ENTRIES` is a default),
newest first by file mtime among the first 5000 directory entries (a folder with more is `truncated`, and
which of the excess are seen depends on the file system); no recursion (`Recovery/` is flat); a file over
`MAX_NAME_READ_BYTES` (4 MiB) is listed without a name instead of being parsed. Names are read by parsing
files, so call it off the control thread: the `scan_recovery` request runs it on the document worker and `open_recovery` opens one entry as an untitled, dirty workspace (a save has to choose a path; the entry's autosave stays until then, and a save-as clears the never-saved autosave of the workspace id).

## Per-machine settings (WS-06, WS-07)

`settings.json` is never part of a show file:

```json
{
  "settings_version": 1,
  "backup_keep": 20,
  "workspaces": {
    "<workspace-id>": {
      "patches": { "<patch-id>": { "device": "Scarlett 18i20", "channels": [4, 5] } }
    }
  }
}
```

`channels[i]` is the device channel (0-based) for logical output `i` of the patch. `load_settings`
never fails: unreadable content gives the defaults and a warning; invalid entries are dropped with a
warning; `backup_keep` is clamped. The engine keeps a copy of a file that produced warnings as
`settings.json.corrupt` (`.corrupt.1`, … if one exists, a few at most) before anything overwrites it. Workspace and patch ids in the file are normalised to lower-case hyphenated form on load. Unknown keys are ignored.

The engine adds mappings itself in one case (M4.4c): a patch with no entry on this machine (a new workspace, a show from another Mac, a patch added later) is mapped to the first N outputs of the device the mixer plays on, when the device has at least N, and the file is rewritten (atomically, off the control thread; other entries are kept). Existing entries are never overwritten. An entry `{"device": "", "channels": []}` means the operator cleared the patch's mapping (`set_patch_mapping`, M4.4g): it is kept as an entry, so the patch stays unmapped and is not mapped by default; an older build drops it with a warning on load. The operator's `set_patch_mapping` also rewrites the file. Rules and edge cases: docs/control.md, Voices: "Default patch mapping". The rewrite keeps keys this version does not know (`MachineSettings::extra`), but it writes the engine's in-memory copy: **an external edit of `settings.json` while the engine runs is overwritten at the next auto-map**. If the file could not be read at start (an I/O error, not "missing"), or it produced warnings and its `.corrupt` copy could not be made, the engine keeps default mappings in memory only, warns once, and never writes the file (`ControlConfig::machine_writable`, `load_machine_settings_checked`). If the settings thread cannot start or has stopped, mappings stay in memory with a warning. At shutdown the engine waits for the settings thread to finish the pending write at most 5 s (`DROP_WAIT`, as for the save writer); a write stuck on a hung volume is then left to finish on its own, so it never holds up quit or a relaunch.

`resolve_patch(settings, workspace_id, patch, devices)` gives `Mapped`, `Unmapped` (ask the operator
once), `DeviceMissing`, `ChannelsMissing` or `OutputCountMismatch`. The last three leave the patch
silent and its cues broken, never a crash.

**Recent files and open-on-launch (WS-08) are app-side**: the engine keeps no list. (A `recent_files` key written by an earlier build is ignored when the file is loaded; a later write drops it.)

Settings that travel with the show (hotkeys, autosave interval, passcode, …) stay in the show file
(WS-06); per-cue-type defaults are deferred (REQUIREMENTS §21).

## Media paths (WS-03, NFR-32)

Show files store forward-slash paths, relative to the show folder when the media is inside it and
absolute otherwise. `portable_ref(native, show_dir, style)` builds the stored `MediaRef` from what the
OS gives: Windows backslashes become forward slashes (relative paths too; a backslash on macOS is a file
name character and is kept), the text is made NFC, `.` and `..` are resolved lexically, and the path is
made relative if it lies inside the show folder. `resolve(media, show_dir)` is the way back.

`comparison_key` (lexical normalisation, NFC, lower case) decides whether two paths name the same file
on macOS or Windows; folder membership is judged the same way. Case folding is `to_lowercase`, not
full Unicode folding.

Path parsing rules: exactly two leading slashes are a network share root (`//server/share`); three or
more are one root (`///x` is `/x`, as POSIX says). A drive-relative `C:foo` is not absolute: it is a
relative name whose first component is the literal `C:foo`, so it resolves inside the show folder to a
file that does not exist and the cue shows as missing (it never reads the current directory of drive C).

Non-UTF-8 file names cannot be stored in a JSON show file; they are skipped (and counted) when
scanning.

## Media watch and follow (M2.25, M2.29)

Design: path first, no file watcher, no bookmarks (opaque per-OS blobs in a cross-platform JSON file, and resolving one can mount or block). Decisions: `docs/handoff/decisions/m2-29.md`.

- **Detection**: the `openlab-watch` thread stats every catalog media file (metadata only), a full pass every `max(3 s, n x 2 ms)` (2,000 files: 4 s; a stat is a syscall, 4-7 us on the Intel CI runner, so this keeps the watch near 0.3% of a core), paced in chunks of 64. It sends one batched event list per pass, only when non-empty; an idle show costs control nothing. Control never stats.
- **Two-pass rule**: Gone is reported on the first pass (a file in the Trash breaks its cues within seconds). Back and Changed are reported only after two identical passes, so a half-copied file is never probed. Back and Changed re-probe the file; an unreadable file becomes `media_unsupported`.
- **Follow** (M2.29b, not implemented yet: until then a moved file is Gone) (macOS, `MNT_LOCAL` volumes only): when the stored path is gone, the file is located by its (device, inode) with `fsgetpath(2)`. Accepted only if it is a regular file, not in a Trash (`.Trash`, `.Trashes`, `$Recycle.Bin`), has the same size and modified time as last seen, and a re-stat gives the same (device, inode). Otherwise the key stays Gone (a file edited and then moved is never followed: never play the wrong file). A symlinked media path is never followed. A case-only rename keeps the stored spelling.
- **Path first**: if the old path exists again (a new file there), the cue stays on the path; a different file at the path is Changed. A new file at the old path wins over the renamed one.
- **Follow is outside undo**, like Arm/Disarm: the relink is applied with its inverse discarded, `revision` rises, `model_changed` is sent and the document is dirty. `History::rewrite_media(map)` rewrites the old reference to the new one in the undo and redo stacks, so undo never walks back to a missing file. The catalog keeps the probe and the preload under the new key (no re-probe) and publishes availability for old and new in the same tick, so the cue never shows broken. At most 256 keys follow per tick. One show-log info line per follow.
- **Hints (planned, M2.29d)**: format_version 3 adds top-level `media_hints` (key = stored `MediaRef` text; volume UUID, file id, size, modified time), advisory and written on every save, so a file moved while the show was closed is found at open. Hints that cannot be resolved on this machine are kept and written back; invalid ones are dropped at load.
- **Windows and network/FAT volumes**: Gone, Back, Changed and Unreadable by stat only; no follow. Windows file IDs: M2.29f. Following the show file itself when it moves: M2.29g.

## Relink (WS-03)

- `missing_media(workspace, show_dir)` lists Audio cues whose file does not exist.
- `find_in_folder(workspace, show_dir, folder, limits)` scans `folder` (default: 16 levels deep, 200 000
  files, 50 000 folders, 20 s; symlinked folders are not entered, so links cannot loop; the clock is
  injectable, `scan_folder_with`) and matches missing files by
  normalised file name. The result is a `RelinkPlan`: `matches`, `ambiguous` (several candidates; never
  guessed) and `unmatched`, plus whether a limit truncated the scan. Nothing is changed.
- `relink_command(workspace, relinks)` turns chosen relinks (a plan's matches, or one the operator picked
  from `ambiguous`, or a single cue) into one `Command::Transaction` of `SetCueProperty` edits, so the
  relink is a single undo step through `History`.
- The `relink` request (M1.17) runs `find_in_folder` on the document worker and applies the matches with `relink_command`; see [control.md](control.md) "Relink". Its `truncated` flag is the scan's.
