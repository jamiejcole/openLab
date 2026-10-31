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
    /// Wait and Fade cues only: the action's length in seconds (0 to 24 h), without resending the
    /// whole kind.
    case duration(Double)
    /// Load cues only: the time the target is loaded to, in seconds (0 to 24 h).
    case loadToTime(Double)

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
        case .duration(let v): return p("duration", .double(v))
        case .loadToTime(let v): return p("load_to_time", .double(v))
        }
    }
}

/// A key of the workspace key map (`settings.hotkeys`): core's `TransportKey` (docs/commands.md).
enum WorkspaceKey: String, CaseIterable, Hashable, Sendable {
    case go
    case stopAll = "stop_all"
    case pauseAll = "pause_all"
    case resumeAll = "resume_all"
    case loadSelected = "load_selected"
    case pauseResumeSelected = "pause_resume_selected"
    case startSelected = "start_selected"
    case stopSelected = "stop_selected"
    case resetSelected = "reset_selected"
}

extension Command {
    /// `SetWorkspaceSetting { hotkey { key, value } }`: one key of the map; nil clears it. The
    /// engine refuses Esc, Up/Down with any modifiers, a key already in the map and a cue's hotkey
    /// (`command_refused`, the message names the clash).
    static func setWorkspaceKey(_ key: WorkspaceKey, _ hotkey: JSONValue?) -> Command {
        let setting = JSONValue.object([
            "type": .string("hotkey"), "key": .string(key.rawValue), "value": hotkey ?? .null,
        ])
        return Command(.object(["type": .string("set_workspace_setting"), "setting": setting]))
    }

    /// `SetWorkspaceSetting` of one number: `panic_duration` (0 to 60 s) or `min_go_interval`
    /// (0 to 10 s). The engine refuses a value out of range.
    static func setWorkspaceNumber(_ type: String, _ value: Double) -> Command {
        let setting = JSONValue.object(["type": .string(type), "value": .double(value)])
        return Command(.object(["type": .string("set_workspace_setting"), "setting": setting]))
    }

    /// `SetWorkspaceSetting { hotkeys { value } }`: the whole key map in one undo step (a swap,
    /// or Reset). `hotkeys` is the `settings.hotkeys` object with every key, null for none.
    static func setKeyMap(_ hotkeys: JSONValue) -> Command {
        let setting = JSONValue.object(["type": .string("hotkeys"), "value": hotkeys])
        return Command(.object(["type": .string("set_workspace_setting"), "setting": setting]))
    }

    /// `RenameWorkspace { name }`: the show title, undoable. At most 1024 Unicode scalars; a newline
    /// is allowed, other control characters are not (docs/commands.md); the engine refuses anything
    /// else.
    static func renameWorkspace(_ name: String) -> Command {
        Command(.object(["type": .string("rename_workspace"), "name": .string(name)]))
    }

    /// `AddCueList { index, id, name }`: a new empty list (CL-01). The client makes the id.
    static func addCueList(id: ModelID, name: String, index: Int) -> Command {
        Command(
            .object([
                "type": .string("add_cue_list"), "index": .int(Int64(index)), "id": .string(id.raw),
                "name": .string(name),
            ]))
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
