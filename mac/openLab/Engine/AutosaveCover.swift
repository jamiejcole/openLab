// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// The autosaves a window's show is known to be at least as new as, by autosave time: ms since
/// the epoch on this Mac's clock, as `scan_recovery` lists them. Never by a show file's time,
/// which may come from a file server's clock (that is why the engine sets autosaves aside
/// instead of deleting them).
struct AutosaveCover: Equatable, Sendable {
    /// The window's own workspace: its last save in this app run, or the autosave it was restored
    /// from (`open` with `restore`).
    private(set) var own: Int64?
    /// Autosaves opened into this window with `open_recovery`, by their workspace id: the window
    /// may have another one (the engine gives a copy of an open workspace a fresh id).
    private(set) var restored: [ModelID: Int64] = [:]

    mutating func coverOwn(upTo ms: Int64) { own = Swift.max(own ?? ms, ms) }

    mutating func coverRestored(_ workspace: ModelID, upTo ms: Int64) {
        restored[workspace] = Swift.max(restored[workspace] ?? ms, ms)
    }

    mutating func merge(_ other: AutosaveCover) {
        if let ms = other.own { coverOwn(upTo: ms) }
        for (workspace, ms) in other.restored { coverRestored(workspace, upTo: ms) }
    }

    /// `entry` holds nothing newer than this window, whose workspace id is `workspace`. An entry
    /// with no time is never covered.
    func covers(_ entry: RecoveryEntryInfo, workspace: ModelID?) -> Bool {
        guard let ms = entry.modifiedMs else { return false }
        if entry.workspaceId == workspace, let own, ms <= own { return true }
        if let r = restored[entry.workspaceId], ms <= r { return true }
        return false
    }
}

enum AutosaveOffer {
    /// What a window holds, for the offer: its workspace id (nil while its show has not loaded)
    /// and what it covers.
    struct Window {
        var workspace: ModelID?
        var cover: AutosaveCover
    }

    /// The `scan_recovery` entries to offer (NFR-05). A current autosave is offered when no
    /// window shows its workspace or waits to reopen it (`held`): an open one belongs to that
    /// window. One set aside as stale (not newer than its file when opened) is offered even when
    /// its workspace is open, since file times are unreliable and it may hold the newest work
    /// (#332), unless a window is known to be at least as new: saved after it, or restored from
    /// it or a later autosave.
    static func offerable(_ entries: [RecoveryEntryInfo], held: Set<ModelID>, windows: [Window])
        -> [RecoveryEntryInfo]
    {
        entries.filter { entry in
            guard entry.stale else { return !held.contains(entry.workspaceId) }
            return !windows.contains { $0.cover.covers(entry, workspace: $0.workspace) }
        }
    }
}
