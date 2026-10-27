<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# M5 answers for M4: recent files, close, protocol additions

From the M5 (macOS app) orchestrator. These answers are for the M4 orchestrator to read and fold into the real docs and tasks. This file is not meant to be merged.

## 1. Recent files (WS-08): the app owns them

**Decision:** recent files and the open-on-launch option belong to the app. The engine keeps nothing. M4.4b is dropped: no `list_recent`, and no recent-files list in the engine's machine settings.

**Reasons:**
- macOS already provides this. `NSDocumentController.shared.noteNewRecentDocumentURL(_:)` fills File ▸ Open Recent and the Dock menu, and remembers the files per user. QLab's Open Recent works the same way.
- It follows the earlier M1-C split, which left recent files to the UI.
- Every UI writes its own list. Engine-side storage would mean a list in two places plus a protocol request, for no gain.
- Open-on-launch is the app deciding what to send after `welcome`. On launch the app sends `open { path }` for the most recent file. If that file has a newer autosave, the existing `recovery_offer` flow covers it.

**Engine changes:**
- Remove `recent_files` from machine settings, with its load-time cleanup, its pruning and its recording on open or save-as, along with the tests for them. Old settings files that still contain the key should keep loading; ignoring unknown keys is enough.
- Drop M4.4b. Open-on-launch is the app's preference too.
- Mark WS-08 as app-side in docs/persistence.md.

**App side (M5, for information):**
- The app records a path when an `open`, `save_as` or `open_recovery`-then-save `result` succeeds. It takes the path from the broadcast `document.path`, so a workspace opened by another client is recorded too.
- A recent file that has gone missing comes back from `open` as `io`. The app then shows an error and removes the entry.

## 2. Close (WS-02): no `close` request for v0.0.1

**Decision:** no engine `close` request and no engine state with no workspace in v0.0.1. In the app:
- **Close (⌘W):** closes the main window only. The workspace stays loaded and playing. The Dock icon or Window ▸ Main Window (⌘0) brings the window back. This already exists (M5.17, PR #7).
- **Replacing the workspace:** New, Open or Quit. If there are unsaved changes, these go through the unsaved-changes sheet (see 3).

**Reasons:**
- §21 limits v0.0.1 to one open workspace. The engine therefore always holds exactly one, and a state with no workspace would add an engine state, a UI state and new error paths for nothing an operator needs.
- It is safer in the booth. In QLab, closing a workspace stops its cues. Here an accidental ⌘W mid-show must never stop playback (NFR-01).
- QLab shows a launch/welcome window when no workspace is open. That belongs with multiple open workspaces (full WS-02, after v0.0.1). The explicit close and the empty state should be designed then, together with that window.

**This differs from QLab** (Close stops and unloads the workspace). §21 should say: *WS-02 v0.0.1: Close closes the window; the workspace stays loaded.* The M5 orchestrator is confirming this with the user. Until then, treat it as the plan.

**Engine changes:** none. Keep `no_workspace` for its current meaning (revert of a never-saved show).

## 3. Protocol additions: fine for the app, with three requests

Nothing in the list is awkward for the app's flow. How the app will use it:

- **Unsaved changes:**
  - The app shows its Save / Don't Save / Cancel sheet *before* sending `new`, `open` or `revert`, using the mirrored `document.dirty`.
  - Save sends `save`, waits for its `result`, then repeats the request.
  - Don't Save repeats the request with `discard_changes: true`.
  - An `unsaved_changes` error is only a backstop for a race, such as another client editing in between. The app answers it by showing the same sheet.
- **Recovery offer:** a `recovery_offer` result shows a sheet using `workspace_name` and `modified_ms`. The app then repeats the same `open` (same `path` and `discard_changes`) with `recovery: "restore"` or `"ignore"`. The two round trips are fine.
- **Show that cannot be opened:** after `bad_show_file`, the app sends `scan_recovery` and offers the entries in a list. Choosing one sends `open_recovery` with that entry's `workspace_id`, `path_hash` and `modified_ms`. The result is untitled, so the next Save becomes Save As.
- **Launch after a crash (NFR-05):** the app sends `scan_recovery` after `welcome` and offers the entries. Stale ones are offered too, marked as older, because persistence.md notes that a stale file may be the only copy of newer work. persistence.md says the scan lists every recovery file. The protocol.md table says it is "for a show that cannot be opened", which reads narrower.
- **Snapshot broadcast:** the app's `WorkspaceStore` already treats any `snapshot` as "replace everything". Selection, inspector and playhead UI reset from it. The documented order (`snapshot`, then `result` on the requester's connection) is what the app will rely on.
- **M5.1a limits:**
  - The app sets `URLSessionWebSocketTask.maximumMessageSize` to 32 MiB, matching `MAX_SERVER_FRAME_BYTES`, and decodes snapshots off the main thread.
  - RequestIds come from a `UInt64` counter starting at 1, which can never reach `u64::MAX`.
  - At most one `snapshot` request is outstanding.

**Requests to the engine (none blocks M4.4):**
1. **`scan_recovery` wording.** Make the protocol.md row say it lists every recovery file, including the autosave of a never-saved workspace, as persistence.md already does. NFR-05's launch offer relies on this.
2. **`recovery_offer` should echo `path`.** The app can match the offer to its pending `open` without keeping extra state, and it helps if two opens ever overlap. Optional; the app can cope without it.
3. **`modified_ms` units and clock.** Say in protocol.md that it is milliseconds since the Unix epoch (wall clock, UTC), so the app can format it with `Date(timeIntervalSince1970:)`.
