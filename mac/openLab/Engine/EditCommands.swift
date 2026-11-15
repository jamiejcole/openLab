// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// One editable cue property: core's `CueProperty`, the subset the UI sends.
/// Enum-like values (colour, continue mode) stay the engine's snake_case strings.
enum CueProperty: Hashable, Sendable {
    case name(String)
    case notes(String)
    case color(String)
    /// The colour shown once the cue has been started (`none`: no second colour).
    case secondColor(String)
    case flagged(Bool)
    case armed(Bool)
    case autoLoad(Bool)
    case skipIfDisarmed(Bool)
    case preWait(Double)
    case postWait(Double)
    case continueMode(String)
    case target(ModelID?)
    /// `triggers { hotkey, hotkey_enabled }`; the hotkey is a show-file hotkey object
    /// (`KeyChord.hotkeyJSON`). With `enabled` false the key is kept but starts nothing.
    case triggers(hotkey: JSONValue?, enabled: Bool)
    case secondTrigger(String)
    case secondTriggerOnRelease(Bool)
    /// "Fade and stop others": on, `scope` (`peers`, `list`, `all`) and the fade time in seconds
    /// (0 to 24 h). Kept while off.
    case fadeAndStopOthers(enabled: Bool, scope: String, duration: Double)
    /// "Duck or boost others": on, the change in dB (-120 to +12; negative ducks) and the ramp time
    /// in seconds. Kept while off.
    case duckOthers(enabled: Bool, db: Double, duration: Double)
    /// The whole kind (a different audio file, say); never to or from a Group.
    case kind(JSONValue)
    /// Named fields of the cue's kind alone (`levels`, `curve`, `slices`, ...): the engine merges them
    /// into the kind it has now, so another client's edit to the rest of the kind is kept. Not `type`,
    /// `children` or `media`.
    case kindFields([String: JSONValue])
    /// An audio or video cue's file alone (an absolute path; the engine stores it relative to the
    /// show folder when inside it); nil clears it. Levels, routing, fades, slices and the stage stay.
    case media(String?)
    /// Network cues only: the network patch id, the OSC address pattern, and the arguments, each
    /// `{type: int|float|string|bool, value}` (core's `OscArg`).
    case destination(ModelID)
    case address(String)
    case args([JSONValue])
    /// Video cues only: the stage the picture is drawn on; nil is "none" (the cue is broken).
    case stage(ModelID?)
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
        case .secondColor(let v): return p("second_color", .string(v))
        case .flagged(let v): return p("flagged", .bool(v))
        case .armed(let v): return p("armed", .bool(v))
        case .autoLoad(let v): return p("auto_load", .bool(v))
        case .skipIfDisarmed(let v): return p("skip_if_disarmed", .bool(v))
        case .preWait(let v): return p("pre_wait", .double(v))
        case .postWait(let v): return p("post_wait", .double(v))
        case .continueMode(let v): return p("continue_mode", .string(v))
        case .target(let v): return p("target", v.map { .string($0.raw) } ?? .null)
        case .triggers(let hotkey, let enabled):
            return p("triggers", .object(["hotkey": hotkey ?? .null, "hotkey_enabled": .bool(enabled)]))
        case .secondTrigger(let v): return p("second_trigger", .string(v))
        case .secondTriggerOnRelease(let v): return p("second_trigger_on_release", .bool(v))
        case .fadeAndStopOthers(let enabled, let scope, let duration):
            return p(
                "fade_and_stop_others",
                .object(["enabled": .bool(enabled), "scope": .string(scope), "duration": .double(duration)]))
        case .duckOthers(let enabled, let db, let duration):
            return p(
                "duck_others", .object(["enabled": .bool(enabled), "db": .double(db), "duration": .double(duration)]))
        case .kind(let v): return p("kind", v)
        case .kindFields(let v): return p("kind_fields", .object(v))
        case .media(let v): return p("media", v.map { .string($0) } ?? .null)
        case .destination(let v): return p("destination", .string(v.raw))
        case .address(let v): return p("address", .string(v))
        case .args(let v): return p("args", .array(v))
        case .stage(let v): return p("stage", v.map { .string($0.raw) } ?? .null)
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

/// A key of the workspace key map (`settings.hotkeys`): core's `TransportKey`. The transport and
/// selected-cue keys come first, then the cue-editing keys, in the engine's field order.
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
    case previewSelected = "preview_selected"
    case panicSelected = "panic_selected"
    case editNumber = "edit_number"
    case editName = "edit_name"
    case editNotes = "edit_notes"
    case editTarget = "edit_target"
    case editPreWait = "edit_pre_wait"
    case editDuration = "edit_duration"
    case editPostWait = "edit_post_wait"
    case cycleContinue = "cycle_continue"
    case toggleFlag = "toggle_flag"
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

    /// `SetWorkspaceSetting { new_cues_auto_load }`: cues made from now on auto-load (existing cues
    /// keep theirs).
    static func setNewCuesAutoLoad(_ on: Bool) -> Command {
        let setting = JSONValue.object(["type": .string("new_cues_auto_load"), "value": .bool(on)])
        return Command(.object(["type": .string("set_workspace_setting"), "setting": setting]))
    }

    /// `SetWorkspaceSetting { hotkeys { value } }`: the whole key map in one undo step (a swap,
    /// or Reset). `hotkeys` is the `settings.hotkeys` object with every key, null for none.
    static func setKeyMap(_ hotkeys: JSONValue) -> Command {
        let setting = JSONValue.object(["type": .string("hotkeys"), "value": hotkeys])
        return Command(.object(["type": .string("set_workspace_setting"), "setting": setting]))
    }

    /// `SetPatchProperty` of one property of a patch, as the engine takes it.
    static func setPatchProperty(_ patch: ModelID, _ property: [String: JSONValue]) -> Command {
        Command(
            .object([
                "type": .string("set_patch_property"), "id": .string(patch.raw), "property": .object(property),
            ]))
    }

    /// `SetPatchProperty { output_name }`: what cue output `output` is called (empty: its number).
    static func setPatchOutputName(_ patch: ModelID, output: Int, name: String) -> Command {
        setPatchProperty(
            patch, ["type": .string("output_name"), "output": .int(Int64(output)), "value": .string(name)])
    }

    /// `SetPatchProperty { channel_count }`: how many cue outputs the patch has (1 to 128).
    static func setPatchOutputCount(_ patch: ModelID, _ count: Int) -> Command {
        setPatchProperty(patch, ["type": .string("channel_count"), "value": .int(Int64(count))])
    }

    /// `SetPatchProperty` of a patch's main level (`output` nil) or one output's level, in dB (-120 is
    /// silence). The engine puts it within the workspace volume limits.
    static func setPatchLevel(_ patch: ModelID, output: Int?, db: Double) -> Command {
        var property: [String: JSONValue] = ["value": .double(db)]
        if let output {
            property["type"] = .string("output_level")
            property["output"] = .int(Int64(output))
        } else {
            property["type"] = .string("master_db")
        }
        return setPatchProperty(patch, property)
    }

    /// `RenameWorkspace { name }`: the show title, undoable. At most 1024 Unicode scalars; a newline
    /// is allowed, other control characters are not; the engine refuses anything
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

    /// `RenameCueList { id, name }`.
    static func renameCueList(id: ModelID, name: String) -> Command {
        Command(.object(["type": .string("rename_cue_list"), "id": .string(id.raw), "name": .string(name)]))
    }

    /// `RemoveCueList { id }`: the engine refuses a list that holds cues and the only list.
    static func removeCueList(id: ModelID) -> Command {
        Command(.object(["type": .string("remove_cue_list"), "id": .string(id.raw)]))
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
