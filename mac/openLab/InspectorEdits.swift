// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// The inspector's text fields: the Basics tab's and the Load Time
/// tab's time, and the Network cue's message.
enum InspectorField: Hashable, CaseIterable {
    case number, name, notes, preWait, postWait, target, duration, loadTime, message
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
        /// The engine would mark the cue broken: a Fade or Devamp cannot act on that kind of cue.
        case wrongTargetKind
        case notApplicable
        /// The cue's duration comes from its content (or it has none): only Wait and Fade cues.
        case noEditableDuration
        /// Only Load cues have a load time.
        case noLoadTime
        /// QLab loads to that long before the end; openLab has no end time for every target yet.
        case negativeLoadTime
        /// Only Network cues have a message.
        case noMessage
        /// The typed arguments cannot be read: an open quote, or an argument type the engine cannot send.
        case invalidMessage

        /// The `Localizable.strings` key of the message shown for it.
        var messageKey: String {
            switch self {
            case .invalidTime: return "inspector.invalid.time"
            case .duplicateNumber: return "inspector.invalid.duplicateNumber"
            case .unknownTarget: return "inspector.invalid.unknownTarget"
            case .selfTarget: return "inspector.invalid.selfTarget"
            case .wrongTargetKind: return "inspector.invalid.wrongTargetKind"
            case .notApplicable: return "inspector.invalid.notApplicable"
            case .decimalComma: return "inspector.invalid.decimalComma"
            case .noEditableDuration: return "inspector.invalid.noDuration"
            case .noLoadTime: return "inspector.invalid.noLoadTime"
            case .negativeLoadTime: return "inspector.invalid.negativeLoadTime"
            case .noMessage: return "inspector.invalid.noMessage"
            case .invalidMessage: return "inspector.invalid.networkMessage"
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
    static func display(
        _ field: InspectorField, cue: Cue, model: WorkspaceModel, media: CueMediaDuration? = nil
    ) -> String {
        switch field {
        case .number: return cue.number ?? ""
        // Real text, as in QLab: an unnamed cue shows the name it follows. Committing the text
        // unchanged sends nothing (below), so the derived name is never written back.
        case .name: return cue.displayName
        case .notes: return cue.raw["notes"]?.stringValue ?? ""
        case .preWait: return CueTime.format(cue.raw["pre_wait"]?.doubleValue ?? 0)
        case .postWait: return CueTime.format(cue.raw["post_wait"]?.doubleValue ?? 0)
        case .duration:
            // An Audio cue's length is read-only: what its file gives, as the cue list shows it.
            if cue.kindType == "audio" { return media?.shownDuration.map(CueTime.format) ?? "" }
            guard durationKinds.contains(cue.kindType) else { return "" }
            return CueTime.format(cue.raw["kind"]?["duration"]?.doubleValue ?? 0)
        case .loadTime:
            guard cue.kindType == "load" else { return "" }
            return CueTime.format(cue.raw["kind"]?["load_to_time"]?.doubleValue ?? 0)
        case .message:
            guard cue.kindType == "network" else { return "" }
            let kind = cue.raw["kind"]
            return NetworkMessage.format(
                address: kind?["address"]?.stringValue ?? "", args: kind?["args"]?.arrayValue ?? [])
        case .target:
            guard let id = cue.raw["kind"]?["target"]?.stringValue else { return "" }
            // A target that is gone shows an empty field; the note beside it says why.
            guard let target = model.cue(ModelID(id)) else { return "" }
            // The number only; the name shows beside the field (QLab 5, manual p. 127).
            return target.number ?? ""
        }
    }

    /// The command for `text` in `field`; `.success(nil)` when it changes nothing.
    static func command(
        _ field: InspectorField, text: String, cue: Cue, model: WorkspaceModel, typed: Bool = false
    ) -> Result<Command?, Rejection> {
        // An empty field over a target that is gone is a target to clear only when the operator emptied it:
        // a field that was only passed through must not change the cue.
        let dangling = typed && field == .target && hasMissingTarget(cue, model: model)
        if text == display(field, cue: cue, model: model) && !dangling { return .success(nil) }
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
        case .message: return messageCommand(text: text, cue: cue)
        case .target:
            guard targetKinds.contains(cue.kindType) else { return .failure(.notApplicable) }
            let n = text.trimmingCharacters(in: .whitespacesAndNewlines)
            if n.isEmpty { return .success(.setProperty(cue.id, .target(nil))) }
            guard let target = model.cue(number: n) else { return .failure(.unknownTarget) }
            if let rejection = targetRejection(of: cue, target: target) { return .failure(rejection) }
            return .success(.setProperty(cue.id, .target(target.id)))
        }
    }
}

extension InspectorEdit {
    /// Whether `cue` names a target that is no longer in the show.
    static func hasMissingTarget(_ cue: Cue, model: WorkspaceModel) -> Bool {
        guard let id = cue.raw["kind"]?["target"]?.stringValue else { return false }
        return model.cue(ModelID(id)) == nil
    }

    /// Why `cue` cannot act on `target`, nil when it can: the one check for every way of setting a
    /// cue target by hand (the inspector's field, the Target column, a drag). The kinds are core's
    /// `can_target`: Fade takes Audio and Group, Devamp takes Audio and a playlist Group.
    static func targetRejection(of cue: Cue, target: Cue) -> Rejection? {
        if target.id == cue.id { return .selfTarget }
        return targetRejection(ofKind: cue.kindType, target: target)
    }

    /// The same for a cue of `kind` that is not `target` itself (a cue about to be made).
    static func targetRejection(ofKind kind: String, target: Cue) -> Rejection? {
        guard targetKinds.contains(kind) else { return .notApplicable }
        switch kind {
        case "fade":
            return ["audio", "group"].contains(target.kindType) ? nil : .wrongTargetKind
        case "devamp":
            let playlist = target.kindType == "group" && target.raw["kind"]?["mode"]?["type"]?.stringValue == "playlist"
            return target.kindType == "audio" || playlist ? nil : .wrongTargetKind
        default: return nil
        }
    }

    /// Seconds from what the user typed (`CueTime.parse`), or why not: a decimal comma has
    /// its own message, since it is the likeliest slip.
    static func time(_ text: String) -> Result<Double, Rejection> {
        if let seconds = CueTime.parse(text) { return .success(seconds) }
        let dotted = text.replacingOccurrences(of: ",", with: ".")
        return .failure(text.contains(",") && CueTime.parse(dotted) != nil ? .decimalComma : .invalidTime)
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

    /// A Network cue's message from typed text: the address and the arguments, each sent only if it
    /// changed. A malformed address is accepted (the cue shows broken until fixed, as in QLab);
    /// arguments that cannot be read are rejected, since the model has no way to hold them.
    static func messageCommand(text: String, cue: Cue) -> Result<Command?, Rejection> {
        guard cue.kindType == "network" else { return .failure(.noMessage) }
        guard case .success(let message) = NetworkMessage.parse(text) else { return .failure(.invalidMessage) }
        let kind = cue.raw["kind"]
        var commands: [Command] = []
        if message.address != (kind?["address"]?.stringValue ?? "") {
            commands.append(.setProperty(cue.id, .address(message.address)))
        }
        // Compared as the text they show, so 5 and 5.0 are told apart and an echo's number form is not.
        if NetworkMessage.format(address: "/", args: message.args)
            != NetworkMessage.format(address: "/", args: kind?["args"]?.arrayValue ?? [])
        {
            commands.append(.setProperty(cue.id, .args(message.args)))
        }
        return .success(Command.transaction(commands))
    }

    /// `triggers` with the hotkey replaced (`.some(nil)` clears it) and/or the hotkey trigger
    /// switched, whatever is not named kept as the cue has it (a show without the switch has it on).
    static func setTriggers(_ cue: Cue, hotkey: JSONValue?? = nil, enabled: Bool? = nil) -> Command {
        let current = cue.raw["triggers"]
        let kept: JSONValue? = current?["hotkey"].flatMap { $0 == .null ? nil : $0 }
        let on = enabled ?? (current?["hotkey_enabled"] != .bool(false))
        return .setProperty(cue.id, .triggers(hotkey: hotkey ?? kept, enabled: on))
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
