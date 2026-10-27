// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// The text fields of the inspector's Basics tab.
enum InspectorField: Hashable, CaseIterable {
    case number, name, notes, preWait, postWait, target
}

/// Turns what the user typed into a model command, or rejects it locally so nothing is sent.
/// Pure: the view controller supplies the cue and the model.
enum InspectorEdit {
    enum Rejection: Error, Equatable {
        case invalidTime
        case duplicateNumber
        case unknownTarget
        case selfTarget
        case notApplicable
    }

    /// Kinds with a cue target (core's `CueKind::has_target`).
    static let targetKinds: Set<String> = [
        "fade", "start", "stop", "pause", "load", "reset", "devamp", "go_to", "arm", "disarm",
    ]

    /// What the field shows for `cue`.
    static func display(_ field: InspectorField, cue: Cue, model: WorkspaceModel) -> String {
        switch field {
        case .number: return cue.number ?? ""
        case .name: return cue.name
        case .notes: return cue.raw["notes"]?.stringValue ?? ""
        case .preWait: return InspectorTime.format(cue.raw["pre_wait"]?.doubleValue ?? 0)
        case .postWait: return InspectorTime.format(cue.raw["post_wait"]?.doubleValue ?? 0)
        case .target:
            guard let id = cue.raw["kind"]?["target"]?.stringValue else { return "" }
            guard let target = model.cue(ModelID(id)) else { return loc("inspector.target.missing") }
            return target.number ?? target.name
        }
    }

    /// The command for `text` in `field`; `.success(nil)` when it changes nothing.
    static func command(
        _ field: InspectorField, text: String, cue: Cue, model: WorkspaceModel
    ) -> Result<Command?, Rejection> {
        if text == display(field, cue: cue, model: model) { return .success(nil) }
        switch field {
        case .name: return .success(.setProperty(cue.id, .name(text)))
        case .notes: return .success(.setProperty(cue.id, .notes(text)))
        case .number:
            // Whitespace around a number is never meant; empty clears it (core refuses an empty number).
            let n = text.trimmingCharacters(in: .whitespacesAndNewlines)
            if n.isEmpty { return .success(cue.number == nil ? nil : .setNumber(cue.id, nil)) }
            if n == cue.number { return .success(nil) }
            if let other = model.cue(number: n), other.id != cue.id { return .failure(.duplicateNumber) }
            return .success(.setNumber(cue.id, n))
        case .preWait, .postWait:
            guard let seconds = InspectorTime.parse(text) else { return .failure(.invalidTime) }
            if seconds == (cue.raw[field == .preWait ? "pre_wait" : "post_wait"]?.doubleValue ?? 0) {
                return .success(nil)
            }
            return .success(.setProperty(cue.id, field == .preWait ? .preWait(seconds) : .postWait(seconds)))
        case .target:
            guard targetKinds.contains(cue.kindType) else { return .failure(.notApplicable) }
            let n = text.trimmingCharacters(in: .whitespacesAndNewlines)
            if n.isEmpty { return .success(.setProperty(cue.id, .target(nil))) }
            guard let target = model.cue(number: n) else { return .failure(.unknownTarget) }
            if target.id == cue.id { return .failure(.selfTarget) }
            return .success(.setProperty(cue.id, .target(target.id)))
        }
    }
}

extension WorkspaceModel {
    /// The cue numbered exactly `number` (numbers are unique in a workspace).
    func cue(number: String) -> Cue? {
        func find(_ cues: [Cue]) -> Cue? {
            for c in cues {
                if c.number == number { return c }
                if let hit = find(c.children) { return hit }
            }
            return nil
        }
        for l in cueLists { if let hit = find(l.cues) { return hit } }
        return nil
    }
}
