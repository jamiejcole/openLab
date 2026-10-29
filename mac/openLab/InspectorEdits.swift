// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// The inspector's text fields: the Basics tab's, the Triggers tab's OSC address and the Load Time
/// tab's time.
enum InspectorField: Hashable, CaseIterable {
    case number, name, notes, preWait, postWait, target, osc, duration, loadTime
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
        /// The cue's duration comes from its content (or it has none): only Wait and Fade cues.
        case noEditableDuration
        /// Only Load cues have a load time.
        case noLoadTime
        /// QLab loads to that long before the end; openLab has no end time for every target yet.
        case negativeLoadTime

        /// The `Localizable.strings` key of the message shown for it.
        var messageKey: String {
            switch self {
            case .invalidTime: return "inspector.invalid.time"
            case .duplicateNumber: return "inspector.invalid.duplicateNumber"
            case .unknownTarget: return "inspector.invalid.unknownTarget"
            case .selfTarget: return "inspector.invalid.selfTarget"
            case .notApplicable: return "inspector.invalid.notApplicable"
            case .decimalComma: return "inspector.invalid.decimalComma"
            case .invalidOSC: return "inspector.invalid.osc"
            case .noEditableDuration: return "inspector.invalid.noDuration"
            case .noLoadTime: return "inspector.invalid.noLoadTime"
            case .negativeLoadTime: return "inspector.invalid.negativeLoadTime"
            }
        }
    }

    /// Kinds whose duration is set by hand (QLab 5: "some cues" have an editable duration; an Audio
    /// cue's comes from its file and trims, a control cue's action is instant).
    static let durationKinds: Set<String> = ["wait", "fade"]

    /// Kinds with a cue target (core's `CueKind::has_target`).
    static let targetKinds: Set<String> = [
        "fade", "start", "stop", "pause", "load", "reset", "devamp", "go_to", "arm", "disarm",
    ]

    /// What the field shows for `cue`.
    static func display(_ field: InspectorField, cue: Cue, model: WorkspaceModel) -> String {
        switch field {
        case .number: return cue.number ?? ""
        // Real text, as in QLab: an unnamed cue shows the name it follows. Committing the text
        // unchanged sends nothing (below), so the derived name is never written back.
        case .name: return cue.displayName
        case .notes: return cue.raw["notes"]?.stringValue ?? ""
        case .preWait: return InspectorTime.format(cue.raw["pre_wait"]?.doubleValue ?? 0)
        case .postWait: return InspectorTime.format(cue.raw["post_wait"]?.doubleValue ?? 0)
        case .osc: return cue.raw["triggers"]?["osc"]?.stringValue ?? ""
        case .duration:
            guard durationKinds.contains(cue.kindType) else { return "" }
            return InspectorTime.format(cue.raw["kind"]?["duration"]?.doubleValue ?? 0)
        case .loadTime:
            guard cue.kindType == "load" else { return "" }
            return InspectorTime.format(cue.raw["kind"]?["load_to_time"]?.doubleValue ?? 0)
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
        case .name:
            // Clearing an unnamed cue's field changes nothing: it goes on following its file.
            if text.isEmpty && cue.name.isEmpty { return .success(nil) }
            return .success(.setProperty(cue.id, .name(text)))
        case .notes: return .success(.setProperty(cue.id, .notes(text)))
        case .number:
            // Whitespace around a number is never meant; empty clears it (core refuses an empty number).
            let n = text.trimmingCharacters(in: .whitespacesAndNewlines)
            if n.isEmpty { return .success(cue.number == nil ? nil : .setNumber(cue.id, nil)) }
            if n == cue.number { return .success(nil) }
            if let other = model.cue(number: n), other.id != cue.id { return .failure(.duplicateNumber) }
            return .success(.setNumber(cue.id, n))
        case .preWait, .postWait:
            let seconds: Double
            switch time(text) {
            case .failure(let r): return .failure(r)
            case .success(let v): seconds = v
            }
            if seconds == (cue.raw[field == .preWait ? "pre_wait" : "post_wait"]?.doubleValue ?? 0) {
                return .success(nil)
            }
            return .success(.setProperty(cue.id, field == .preWait ? .preWait(seconds) : .postWait(seconds)))
        case .duration: return durationCommand(text: text, cue: cue)
        case .loadTime:
            guard cue.kindType == "load" else { return .failure(.noLoadTime) }
            let t = text.trimmingCharacters(in: .whitespaces)
            if t.hasPrefix("-"), case .success = time(String(t.dropFirst())) { return .failure(.negativeLoadTime) }
            switch time(text) {
            case .failure(let r): return .failure(r)
            case .success(let seconds):
                if seconds == (cue.raw["kind"]?["load_to_time"]?.doubleValue ?? 0) { return .success(nil) }
                return .success(.setProperty(cue.id, .loadToTime(seconds)))
            }
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
    /// Seconds from what the user typed (`InspectorTime.parse`), or why not: a decimal comma has
    /// its own message, since it is the likeliest slip.
    static func time(_ text: String) -> Result<Double, Rejection> {
        if let seconds = InspectorTime.parse(text) { return .success(seconds) }
        let dotted = text.replacingOccurrences(of: ",", with: ".")
        return .failure(text.contains(",") && InspectorTime.parse(dotted) != nil ? .decimalComma : .invalidTime)
    }

    /// A Wait or Fade cue's duration from typed text, for the inspector and the cue list's Duration
    /// column alike; `.success(nil)` when it is the duration the cue already has.
    static func durationCommand(text: String, cue: Cue) -> Result<Command?, Rejection> {
        guard durationKinds.contains(cue.kindType) else { return .failure(.noEditableDuration) }
        switch time(text) {
        case .failure(let r): return .failure(r)
        case .success(let seconds):
            if seconds == (cue.raw["kind"]?["duration"]?.doubleValue ?? 0) { return .success(nil) }
            return .success(.setProperty(cue.id, .duration(seconds)))
        }
    }

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
