<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# M5 answers for M4: recent files, close, protocol additions

From the M5 (macOS app) orchestrator. These answers are for the M4 orchestrator to read and fold into the real docs and tasks. This file is not meant to be merged.

## 1. Recent files (WS-08): the app owns them

**Decision:** recent files and the open-on-launch option belong to the app. The engine keeps nothing. M4.4b is dropped: no `list_recent`, and no recent-files list in the engine's machine settings.

**Reasons:**
- macOS already provides this. `NSDocumentController.shared.noteNewRecentDocumentURL(_:)` fills File ▸ Open Recent and the Dock menu, and remembers the files per user. QLab's Open Recent works the same way.
- It follows the earlier M1-C split, which left recent files to the UI.
- Every UI writes its own list. Engine-side storage would mean a list in two places plus a protocol request, for no gain.
- Open-on-launch is the app deciding what to send after `welcome`. On launch, if the option is on, the app sends `open { path }` for the most recent file. Otherwise it shows the welcome window (see 2). If that file has a newer autosave, the existing `recovery_offer` flow covers it.

**Engine changes:**
- Remove `recent_files` from machine settings, with its load-time cleanup, its pruning and its recording on open or save-as, along with the tests for them. Old settings files that still contain the key should keep loading; ignoring unknown keys is enough.
- Drop M4.4b. Open-on-launch is the app's preference too.
- Mark WS-08 as app-side in docs/persistence.md.

**App side (M5, for information):**
- The app records a path when an `open`, `save_as` or `open_recovery`-then-save `result` succeeds. It takes the path from the broadcast `document.path`, so a workspace opened by another client is recorded too.
- A recent file that has gone missing comes back from `open` as `io`. The app then shows an error and removes the entry.

## 2. Close (WS-02): QLab-style close and a "no workspace" state

**Decision (confirmed by the user):** Close works the way it does in QLab. Closing the workspace stops its playback and unloads it. The app then shows an empty welcome window. Under §21's one-workspace limit this means the engine can be in a state with **no workspace** loaded.

**Reasons:** the user chose to follow QLab here (UI-07). Operators expect Close to unload the show, and it gives a clean launch state too.

**Engine changes:**
- **New request `close { discard_changes? }`.** If there are unsaved edits it refuses with `unsaved_changes`, like `new`, `open` and `revert`. Otherwise it stops every running and paused cue at once (as a hard stop, not a fade), drops the workspace, stops autosaving it, and answers `result`. With `discard_changes: true` it also deletes the autosave of the dropped edits, as the other requests already do. A `close` when no workspace is open succeeds and does nothing.
- **Snapshot without a workspace.** `snapshot.workspace` and `snapshot.document` become nullable (null means no workspace). After a `close`, every client gets that snapshot (to the requester before its `result`), the same as after `open`. `welcome` carries it too, so a client that connects while nothing is open can tell. Lists and states are empty, and `revision` and `runtime_seq` keep rising. `PROTOCOL_VERSION` can stay 1, because no released client depends on these fields being non-null (the Swift client is written after this). If you would rather bump the version, say so.
- **Engine start.** The engine starts with no workspace and waits for the app to send `new`, `open` or `open_recovery`. If it starts with an empty workspace today, change that. Starting empty is what makes the launch decision (welcome window, open-on-launch, crash recovery) belong to the app. Please confirm this fits M4.2 start-up and the supervision in M5.2.
- **Requests that need a workspace** (edits, transport, `save`, `save_as`, `relink`, `snapshot`-dependent commands) answer `no_workspace` while none is loaded. That code already exists. `new`, `open`, `open_recovery`, `scan_recovery`, `list_devices` and `close` keep working.
- **OSC with no workspace:** treat it the same as a workspace id that matches nothing. What QLab does in that case is being measured in a separate session (`qlab-osc-findings.md`). Follow those findings in docs/osc.md.
- Update docs/protocol.md (the `close` row, nullable snapshot fields, the no-workspace rules) and regenerate fixtures (`client/close.json`, `server/snapshot_no_workspace.json`). The Swift client follows in M5.1.

**App side (M5, for information):**
- **⌘W and File ▸ Close** send `close`.
  - If the workspace has unsaved changes, the app first shows the Save / Don't Save / Cancel sheet.
  - If cues are running, it first asks for confirmation ("Closing stops N running cues"). In Show Mode, closing needs that confirmation every time (SM-05 spirit).
- **Welcome window:** when the snapshot has no workspace, the app hides the main window and shows a small welcome window offering:
  - New Workspace;
  - Open…;
  - the recent files list (app-owned, see 1);
  - any recovery entries from `scan_recovery`.
  It shows the same window at launch when there's no open-on-launch file.
- M5.17's current behaviour (⌘W closes only the window; the Dock icon or ⌘0 brings it back) changes. While a workspace is open, the Dock icon or ⌘0 brings the main window back. With no workspace, they show the welcome window. That becomes a new M5 task, made once `close` exists.

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
