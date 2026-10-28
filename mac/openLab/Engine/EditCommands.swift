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
    /// The whole kind (a different audio file, say); never to or from a Group.
    case kind(JSONValue)
    /// An audio cue's file alone (an absolute path; the engine stores it relative to the show folder
    /// when inside it); nil clears it. Levels, routing, fades and slices stay.
    case media(String?)
    /// Network cues only: the network patch id, the OSC address pattern, and the arguments, each
    /// `{type: int|float|string|bool, value}` (core's `OscArg`).
    case destination(ModelID)
    case address(String)
    case args([JSONValue])
    /// Group cues only: core's `GroupMode` (`type`: timeline, start_first_and_enter,
    /// start_first_and_go_to_next, playlist, start_random; `loop`/`shuffle` for a playlist only).
    case groupMode(type: String, loop: Bool, shuffle: Bool)

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
        case .kind(let v): return p("kind", v)
        case .media(let v): return p("media", v.map { .string($0) } ?? .null)
        case .destination(let v): return p("destination", .string(v.raw))
        case .address(let v): return p("address", .string(v))
        case .args(let v): return p("args", .array(v))
        case .groupMode(let type, let loop, let shuffle):
            var mode: [String: JSONValue] = ["type": .string(type)]
            if type == "playlist" {
                mode["loop"] = .bool(loop)
                mode["shuffle"] = .bool(shuffle)
            }
            return p("group_mode", .object(mode))
        }
    }
}

extension Command {
    /// `RenameWorkspace { name }`: the show title, undoable. At most 256 characters, no control
    /// characters (docs/commands.md); the engine refuses anything else.
    static func renameWorkspace(_ name: String) -> Command {
        Command(.object(["type": .string("rename_workspace"), "name": .string(name)]))
    }

    /// `SetCueProperty { id, property }`.
    static func setProperty(_ id: ModelID, _ property: CueProperty) -> Command {
        Command(.object(["type": .string("set_cue_property"), "id": .string(id.raw), "property": property.json]))
    }

    /// `SetCueNumbers` clearing the number of each cue (one undo step).
    static func clearNumbers(_ ids: [ModelID]) -> Command {
        Command(
            .object([
                "type": .string("set_cue_numbers"),
                "numbers": .array(ids.map { .object(["id": .string($0.raw), "number": .null]) }),
            ]))
    }

    /// `SetCueNumbers` for one cue; nil clears the number.
    static func setNumber(_ id: ModelID, _ number: String?) -> Command {
        let entry = JSONValue.object(["id": .string(id.raw), "number": number.map { .string($0) } ?? .null])
        return Command(.object(["type": .string("set_cue_numbers"), "numbers": .array([entry])]))
    }
}
