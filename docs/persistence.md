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
<app-support>/openLab/Backups/<workspace-id>/<time>.openlab   rolling backups
<app-support>/openLab/Recovery/<workspace-id>.openlab    autosave
```

## Saving (NFR-04)

`save_workspace(dirs, path, workspace, now_ms, keep)`:

1. `save_show` serialises and checks the text loads back. A workspace that would not load back is
   refused with `PersistError::Serialize`; nothing is written, not even a backup.
2. If `path` exists and `keep > 0`, its current bytes are copied into the backup folder. A failed
   backup is a warning in `SaveReport`, not a failed save.
3. The new text is written to `<name>.openlab.tmp` beside the show file, fsynced, renamed over the
   original, and the folder is fsynced (Unix; best effort). On any error before the rename the temp
   file is removed, the original is untouched, and the backup made in step 2 is removed again so
   failed attempts do not push real backups out of the rotation.
4. Old backups beyond `keep` are deleted; the recovery file is removed (the show file now holds
   everything it did).

A temp file left by a crash is overwritten by the next save. `write_atomic_with` takes a hook called
after the temp write and after the fsync, for failure-injection tests.

Serialising plus the verify-reload costs up to about a second for a large show, so saves run on the
writer thread: the control thread clones the `Workspace` and calls `SaveWriter::save` /
`SaveWriter::autosave`, which return a channel with the result. Dropping the writer finishes queued
jobs.

## Backups (NFR-06)

Named `YYYYMMDD-HHMMSS-mmm.openlab` in UTC, so names sort in time order
(`backup_file_name`/`parse_backup_name`). `select_backups_to_delete(names, keep)` keeps the `keep`
newest. Files whose names are not in that form are never deleted. Two saves in the same millisecond get
consecutive names. `keep` is `MachineSettings::backup_keep` (default 20, at most 1000); 0 turns backups
off and leaves existing ones alone.

## Autosave and recovery (NFR-05)

- `autosave_due(dirty_since, last_autosave, now, interval)` says when to autosave; `interval` is the
  workspace's `autosave_interval`.
- `write_recovery` writes `Recovery/<workspace-id>.openlab` atomically and never touches the show file.
  `clear_recovery` removes it (after a save or clean close).
- `check_recovery(dirs, workspace_id, show_path)` compares file modification times with
  `decide_recovery`:

| Situation | Result |
|---|---|
| No recovery file | `None` |
| Recovery file newer than the show file, or the show file is missing | `Offer`, carrying the parsed workspace |
| Recovery file not newer (equal counts as not newer) | `Stale`; the file is deleted |
| Recovery file unreadable, invalid, or for another workspace | `Unusable { warning }`; the file is renamed `.corrupt` so the warning is not repeated |

The restore prompt itself is the UI's (M4.4).

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
`settings.json.corrupt` before anything overwrites it. Unknown keys are ignored.

`resolve_patch(settings, workspace_id, patch, devices)` gives `Mapped`, `Unmapped` (ask the operator
once), `DeviceMissing`, `ChannelsMissing` or `OutputCountMismatch`. The last three leave the patch
silent and its cues broken, never a crash.

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

Non-UTF-8 file names cannot be stored in a JSON show file; they are skipped (and counted) when
scanning.

## Relink (WS-03)

- `missing_media(workspace, show_dir)` lists Audio cues whose file does not exist.
- `find_in_folder(workspace, show_dir, folder, limits)` scans `folder` (default: 16 levels deep, 200 000
  files; symlinked folders are not entered, so links cannot loop) and matches missing files by
  normalised file name. The result is a `RelinkPlan`: `matches`, `ambiguous` (several candidates; never
  guessed) and `unmatched`, plus whether a limit truncated the scan. Nothing is changed.
- `relink_command(workspace, relinks)` turns chosen relinks (a plan's matches, or one the operator picked
  from `ambiguous`, or a single cue) into one `Command::Transaction` of `SetCueProperty` edits, so the
  relink is a single undo step through `History`.
