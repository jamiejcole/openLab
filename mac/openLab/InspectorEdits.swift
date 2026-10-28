// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// The text fields of the inspector's Basics tab.
enum InspectorField: Hashable, CaseIterable {
    case number, name, notes, preWait, postWait, target, osc
}

/// Turns what the user typed into a model command, or rejects it locally so nothing is sent.
/// Pure: the view controller supplies the cue and the model.
enum InspectorEdit {
    enum Rejection: Error, Equatable {
        case invalidTime
        case decimalComma
        case duplicateNumber
        case unknownTarget
        case selfTarget
        case notApplicable
        case invalidOSC
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
        case .osc: return cue.raw["triggers"]?["osc"]?.stringValue ?? ""
        case .target:
            guard let id = cue.raw["kind"]?["target"]?.stringValue else { return "" }
            guard let target = model.cue(ModelID(id)) else { return loc("inspector.target.missing") }
            return target.number ?? target.displayName
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
            guard let seconds = InspectorTime.parse(text) else {
                let dotted = text.replacingOccurrences(of: ",", with: ".")
                return .failure(text.contains(",") && InspectorTime.parse(dotted) != nil ? .decimalComma : .invalidTime)
            }
            if seconds == (cue.raw[field == .preWait ? "pre_wait" : "post_wait"]?.doubleValue ?? 0) {
                return .success(nil)
            }
            return .success(.setProperty(cue.id, field == .preWait ? .preWait(seconds) : .postWait(seconds)))
        case .osc:
            let a = text.trimmingCharacters(in: .whitespaces)
            if a.isEmpty { return .success(setTriggers(cue, osc: .some(nil))) }
            guard isValidOSCAddress(a) else { return .failure(.invalidOSC) }
            return .success(a == display(.osc, cue: cue, model: model) ? nil : setTriggers(cue, osc: .some(a)))
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

extension InspectorEdit {
    /// An OSC address that starts a cue: `/`, then printable ASCII with no spaces (QLab's OSC is
    /// ASCII only) and none of the pattern characters, since the trigger is matched literally.
    static func isValidOSCAddress(_ a: String) -> Bool {
        let banned = "*?[]{}#,".unicodeScalars
        return a.hasPrefix("/") && a.count > 1 && a.count <= 256
            && a.unicodeScalars.allSatisfy { $0.value > 0x20 && $0.value < 0x7F && !banned.contains($0) }
    }

    /// `triggers` with the hotkey and/or OSC address replaced (`.some(nil)` clears), the other kept.
    static func setTriggers(_ cue: Cue, hotkey: JSONValue?? = nil, osc: String?? = nil) -> Command {
        let current = cue.raw["triggers"]
        let kept: JSONValue? = current?["hotkey"].flatMap { $0 == .null ? nil : $0 }
        return .setProperty(
            cue.id, .triggers(hotkey: hotkey ?? kept, osc: osc ?? current?["osc"]?.stringValue))
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
