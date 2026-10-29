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

/// The engine's `path_hash` (openlab-core `media_path`): 16 hex digits of FNV-1a over the path
/// lexically normalised, NFC and lower case. It names the recovery file of one copy of a show,
/// and the engine does not report it in `workspaces`, so the app derives it for the open windows.
/// A mismatch can only make an autosave be offered that a window holds (visible), never hide one.
enum RecoveryPathHash {
    static func of(_ path: String) -> String {
        var hash: UInt64 = 0xcbf2_9ce4_8422_2325
        for byte in comparisonKey(path).utf8 {
            hash ^= UInt64(byte)
            hash = hash &* 0x0100_0000_01b3
        }
        let hex = String(hash, radix: 16)
        return String(repeating: "0", count: 16 - hex.count) + hex
    }

    static func comparisonKey(_ path: String) -> String {
        normalize(path).precomposedStringWithCanonicalMapping.lowercased().precomposedStringWithCanonicalMapping
    }

    /// `.`, `..` and repeated slashes collapsed; exactly two leading slashes are a network share
    /// root, one or three or more are the root; `..` above an absolute root is dropped.
    private static func normalize(_ path: String) -> String {
        var root = ""
        var rest = Substring(path)
        if path.hasPrefix("//"), !path.hasPrefix("///") {
            root = "//"
            rest = path.dropFirst(2)
        } else if path.hasPrefix("/") {
            root = "/"
            rest = path.dropFirst()
        }
        var comps: [Substring] = []
        for comp in rest.split(separator: "/", omittingEmptySubsequences: true) {
            switch comp {
            case ".": continue
            case "..":
                if let last = comps.last, last != ".." {
                    comps.removeLast()
                } else if root.isEmpty {
                    comps.append(comp)
                }
            default: comps.append(comp)
            }
        }
        return root + comps.joined(separator: "/")
    }
}

enum AutosaveOffer {
    /// What a window holds, for the offer: its workspace id (nil while its show has not loaded),
    /// the show path it has (nil: untitled), and the autosaves loaded into it.
    struct Window {
        var workspace: ModelID?
        var path: String?
        var cover: AutosaveCover
    }

    /// The `scan_recovery` entries to offer (NFR-05). A current autosave is held, so not offered,
    /// when a window shows its workspace id and that copy of the show: the entry's path hash is
    /// the window's path (an entry with no hash, or a window with no path, matches by workspace
    /// id alone). A copy of a show shares the workspace id, so the current autosave of another
    /// copy is offered even while this one is open (#359). One set aside as stale (not newer than
    /// its file when opened) is offered even when its workspace is open, since file times are
    /// unreliable and it may hold the newest work (#332), unless that very autosave was loaded
    /// into a window.
    static func offerable(_ entries: [RecoveryEntryInfo], windows: [Window]) -> [RecoveryEntryInfo] {
        entries.filter { entry in
            guard entry.stale else { return !windows.contains { holds($0, entry) } }
            return !windows.contains { $0.cover.covers(entry, workspace: $0.workspace) }
        }
    }

    private static func holds(_ window: Window, _ entry: RecoveryEntryInfo) -> Bool {
        guard window.workspace == entry.workspaceId else { return false }
        guard let path = window.path, let hash = entry.pathHash else { return true }
        return RecoveryPathHash.of(path) == hash
    }
}
