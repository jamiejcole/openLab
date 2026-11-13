// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

extension Cue {
    /// Disarmed with "skip if disarmed" set: passed over as though it were not there (QLab 5,
    /// manual p. 128). An armed cue's box has no effect.
    var isSkipped: Bool { raw["armed"] == .bool(false) && raw["skip_if_disarmed"] == .bool(true) }
}

extension WorkspaceModel {
    /// The cue a GO starts while the playhead rests on `id`: that cue, or when it is skipped the next
    /// cue that is there, further along its level, else after the group around it, and so on
    /// outwards; nil when none is left. The engine walks the same way (core `playhead::first_present`),
    /// so what stands by is what GO starts.
    func firstPresent(from id: ModelID) -> Cue? {
        for list in cueLists {
            if let path = Self.path(to: id, in: list.cues) { return Self.firstPresent(path) }
        }
        return nil
    }

    /// The levels from the list down to `id`, each with the index taken in it.
    private static func path(to id: ModelID, in level: [Cue]) -> [(level: [Cue], index: Int)]? {
        for (i, cue) in level.enumerated() {
            if cue.id == id { return [(level, i)] }
            if let inner = path(to: id, in: cue.children) { return [(level, i)] + inner }
        }
        return nil
    }

    private static func firstPresent(_ start: [(level: [Cue], index: Int)]) -> Cue? {
        var path = start
        guard let last = path.last, last.level.indices.contains(last.index) else { return nil }
        if !last.level[last.index].isSkipped { return last.level[last.index] }
        while let (level, index) = path.last {
            if let next = level.indices.first(where: { $0 > index && !level[$0].isSkipped }) { return level[next] }
            path.removeLast()
        }
        return nil
    }
}
