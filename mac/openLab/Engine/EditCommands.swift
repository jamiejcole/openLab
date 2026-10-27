// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// One editable cue property: core's `CueProperty` (docs/commands.md), the subset the UI sends.
/// Enum-like values (colour, continue mode) stay the engine's snake_case strings.
enum CueProperty: Hashable, Sendable {
    case name(String)
    case notes(String)
    case color(String)
    case flagged(Bool)
    case armed(Bool)
    case preWait(Double)
    case postWait(Double)
    case continueMode(String)
    case target(ModelID?)
    /// `triggers { hotkey, osc }`; the hotkey is a show-file hotkey object (`KeyChord.hotkeyJSON`).
    case triggers(hotkey: JSONValue?, osc: String?)
    case secondTrigger(String)
    case secondTriggerOnRelease(Bool)

    var json: JSONValue {
        func p(_ type: String, _ value: JSONValue) -> JSONValue { .object(["type": .string(type), "value": value]) }
        switch self {
        case .name(let v): return p("name", .string(v))
        case .notes(let v): return p("notes", .string(v))
        case .color(let v): return p("color", .string(v))
        case .flagged(let v): return p("flagged", .bool(v))
        case .armed(let v): return p("armed", .bool(v))
        case .preWait(let v): return p("pre_wait", .double(v))
        case .postWait(let v): return p("post_wait", .double(v))
        case .continueMode(let v): return p("continue_mode", .string(v))
        case .target(let v): return p("target", v.map { .string($0.raw) } ?? .null)
        case .triggers(let hotkey, let osc):
            return p("triggers", .object(["hotkey": hotkey ?? .null, "osc": osc.map { .string($0) } ?? .null]))
        case .secondTrigger(let v): return p("second_trigger", .string(v))
        case .secondTriggerOnRelease(let v): return p("second_trigger_on_release", .bool(v))
        }
    }
}

extension Command {
    /// `SetCueProperty { id, property }`.
    static func setProperty(_ id: ModelID, _ property: CueProperty) -> Command {
        Command(.object(["type": .string("set_cue_property"), "id": .string(id.raw), "property": property.json]))
    }

    /// `SetCueNumbers` for one cue; nil clears the number.
    static func setNumber(_ id: ModelID, _ number: String?) -> Command {
        let entry = JSONValue.object(["id": .string(id.raw), "number": number.map { .string($0) } ?? .null])
        return Command(.object(["type": .string("set_cue_numbers"), "numbers": .array([entry])]))
    }
}
