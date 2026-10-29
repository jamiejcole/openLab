// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// The autosaves that were loaded into a window, each by its exact identity. Only loading
/// establishes that a window holds an autosave's work: a time does not (a window saved after an
/// autosave set aside from its file never had that autosave's edits).
struct AutosaveCover: Equatable, Sendable {
    /// One autosave as `scan_recovery` lists it.
    struct Loaded: Hashable, Sendable {
        var workspace: ModelID
        var pathHash: String?
        var modifiedMs: Int64
    }

    /// Opened with `open_recovery` (the entry's workspace id: the window may have another one,
    /// since the engine gives a copy of an open workspace a fresh id).
    private(set) var loaded: Set<Loaded> = []
    /// Restored with `open` + `restore`, by the offer's time: the window's own workspace and show
    /// path (the app is not told that path's hash).
    private(set) var ownLoaded: Set<Int64> = []

    mutating func load(_ entry: RecoveryEntryInfo) {
        guard let ms = entry.modifiedMs else { return }
        loaded.insert(Loaded(workspace: entry.workspaceId, pathHash: entry.pathHash, modifiedMs: ms))
    }

    mutating func loadOwn(modifiedMs ms: Int64) { ownLoaded.insert(ms) }

    mutating func merge(_ other: AutosaveCover) {
        loaded.formUnion(other.loaded)
        ownLoaded.formUnion(other.ownLoaded)
    }

    /// `entry` is an autosave this window (whose workspace id is `workspace`) loaded. An entry
    /// with no time is never covered.
    func covers(_ entry: RecoveryEntryInfo, workspace: ModelID?) -> Bool {
        guard let ms = entry.modifiedMs else { return false }
        if loaded.contains(Loaded(workspace: entry.workspaceId, pathHash: entry.pathHash, modifiedMs: ms)) {
            return true
        }
        return entry.workspaceId == workspace && ownLoaded.contains(ms)
    }
}

enum AutosaveOffer {
    /// What a window holds, for the offer: its workspace id (nil while its show has not loaded)
    /// and the autosaves loaded into it.
    struct Window {
        var workspace: ModelID?
        var cover: AutosaveCover
    }

    /// The `scan_recovery` entries to offer (NFR-05). A current autosave is offered when no
    /// window shows its workspace or waits to reopen it (`held`): an open one belongs to that
    /// window. One set aside as stale (not newer than its file when opened) is offered even when
    /// its workspace is open, since file times are unreliable and it may hold the newest work
    /// (#332), unless that very autosave was loaded into a window.
    static func offerable(_ entries: [RecoveryEntryInfo], held: Set<ModelID>, windows: [Window])
        -> [RecoveryEntryInfo]
    {
        entries.filter { entry in
            guard entry.stale else { return !held.contains(entry.workspaceId) }
            return !windows.contains { $0.cover.covers(entry, workspace: $0.workspace) }
        }
    }
}
