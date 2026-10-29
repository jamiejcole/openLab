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
    /// path (the offer does not carry that path's hash).
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

    /// A workspace a window shows or waits to reopen, with the hash the engine names its autosave
    /// by (`workspaces`' `path_hash`). nil: never saved, or not known (an engine that does not send
    /// it, or a path the engine has not listed yet).
    struct Holder: Hashable {
        var workspace: ModelID
        var pathHash: String?

        /// A copy of a show has its workspace id, so the id alone does not say whose autosave it
        /// is: the hash does, when both sides have one (#359).
        func holds(_ entry: RecoveryEntryInfo) -> Bool {
            guard entry.workspaceId == workspace else { return false }
            guard let pathHash, let other = entry.pathHash else { return true }
            return pathHash == other
        }
    }

    /// The `scan_recovery` entries to offer (NFR-05). A current autosave is offered when no
    /// window shows or waits to reopen its workspace at its path (`held`): an open one belongs to
    /// that window, and another copy's (same workspace id, other path) is nobody's. One set aside
    /// as stale (not newer than its file when opened) is offered even when its workspace is open,
    /// since file times are unreliable and it may hold the newest work (#332). Neither is offered
    /// once that very autosave was loaded into a window.
    static func offerable(_ entries: [RecoveryEntryInfo], held: Set<Holder>, windows: [Window])
        -> [RecoveryEntryInfo]
    {
        entries.filter { entry in
            if windows.contains(where: { $0.cover.covers(entry, workspace: $0.workspace) }) { return false }
            return entry.stale || !held.contains { $0.holds(entry) }
        }
    }
}
