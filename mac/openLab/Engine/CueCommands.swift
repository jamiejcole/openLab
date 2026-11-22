// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// The cue kinds the toolbar and Cues menu can add (the model's kinds that need no choice first;
/// Audio takes the first audio patch and Network the first network patch).
enum NewCueKind: String, CaseIterable, Sendable {
    case audio, video, group, memo, wait, fade, start, stop, pause, load, reset, devamp, goto, target, arm, disarm
    case network

    var titleKey: String { "cue.type.\(rawValue)" }

    /// Core's wire name for the kind.
    var wireType: String { self == .goto ? "go_to" : rawValue }

    /// The `kind` object with defaults. Audio needs a patch (core requires one) and gets one cue
    /// output per patch output, with file channel n routed to cue output n at 0 dB (QLab 5's default
    /// Audio cue levels; QLab 5 manual pp. 181-182, Levels tab and default levels, and p. 190, signal
    /// flow through crosspoint (1,1)). Core's crosspoints are sparse, so a cue sent with none plays
    /// silent. A mono file plays on output 1 alone; inputs beyond the file's channels are ignored by
    /// the mixer. The engine plays this exact message in its tests (client/command_add_new_audio_cue).
    /// A Video cue starts on `stage` (the caller gives the workspace's first stage: QLab 5 p. 212
    /// names no default), with no file and no duration; with no stage it has none and shows broken.
    /// For its movie's sound it takes `patch` and that patch's default levels as an Audio cue does;
    /// with no patch it has none, which only matters once its file has sound.
    func kindJSON(patch: ModelObject?, networkPatch: ModelObject? = nil, stage: ModelID? = nil) -> JSONValue? {
        let fade = JSONValue.object(["duration": .double(0), "curve": .string("linear")])
        var fields: [String: JSONValue] = ["type": .string(wireType)]
        switch self {
        case .audio:
            guard let patch else { return nil }
            fields.merge([
                "media": .null, "patch": .string(patch.id.raw), "start_time": .double(0), "end_time": .null,
                "fade_in": fade, "fade_out": fade, "slices": .array([]), "last_slice_play_count": .int(1),
                "levels": Self.defaultLevels(patch: patch), "preload": .bool(false),
            ]) { a, _ in a }
        case .video:
            fields.merge([
                "media": .null, "stage": stage.map { .string($0.raw) } ?? .null, "duration": .null,
                "patch": patch.map { .string($0.id.raw) } ?? .null, "levels": Self.defaultLevels(patch: patch),
                "audio_track": .int(0),
            ]) { a, _ in a }
        case .group:
            fields.merge(["mode": .object(["type": .string("timeline")]), "children": .array([])]) { a, _ in a }
        case .memo: break
        case .wait: fields["duration"] = .double(5)
        case .fade:
            fields.merge([
                "target": .null, "duration": .double(3), "curve": .string("linear"),
                "levels": .object(["master_db": .null, "output_db": .array([]), "crosspoints": .array([])]),
                "stop_target_when_done": .bool(false), "mode": .string("absolute"),
            ]) { a, _ in a }
        case .stop: fields.merge(["target": .null, "fade_out": fade]) { a, _ in a }
        case .load: fields.merge(["target": .null, "load_to_time": .double(0)]) { a, _ in a }
        case .devamp:
            fields.merge([
                "target": .null, "start_next_cue": .bool(false), "stop_target_when_slice_ends": .bool(false),
                // The engine's default mode, the one a show from before the modes loads with.
                "devamp_mode": .string("current_slice"),
            ]) { a, _ in a }
        case .goto: fields.merge(["target": .null, "then_go": .bool(false)]) { a, _ in a }
        // The cue it retargets and the cue that becomes that cue's target (QLab 5, manual p. 686).
        case .target: fields.merge(["target": .null, "new_target": .null]) { a, _ in a }
        case .network:
            // No address yet: core keeps a Network cue without a valid message and shows it broken
            // until it is edited, so nothing is sent by accident.
            guard let networkPatch else { return nil }
            fields.merge(["destination": .string(networkPatch.id.raw), "address": .string(""), "args": .array([])]) {
                a, _ in a
            }
        case .start, .pause, .reset, .arm, .disarm: fields["target"] = .null
        }
        return .object(fields)
    }

    /// A new cue's levels on `patch`: main and every cue output at 0 dB, file channel n to cue
    /// output n at 0 dB; with no patch, the main level alone.
    static func defaultLevels(patch: ModelObject?) -> JSONValue {
        let count = patch?.raw["outputs"]?.arrayValue?.count ?? 0
        let output = JSONValue.object(["db": .double(0), "mute": .bool(false)])
        // Only the channels a file can have: a route from a later one is refused by the engine.
        let diagonal = (0..<min(count, LevelSet.maxInputs)).map { n -> JSONValue in
            .object(["input": .int(Int64(n)), "output": .int(Int64(n)), "db": .double(0), "mute": .bool(false)])
        }
        return .object([
            "master_db": .double(0), "outputs": .array(Array(repeating: output, count: count)),
            "crosspoints": .array(diagonal), "gangs": .array([]),
        ])
    }

    /// A whole new cue (no number: `auto_number` gives it one).
    /// `target`: the cue a new cue that takes a target starts with.
    func cueJSON(
        id: ModelID, patch: ModelObject?, networkPatch: ModelObject? = nil, target: ModelID? = nil,
        stage: ModelID? = nil
    ) -> JSONValue? {
        guard var kind = kindJSON(patch: patch, networkPatch: networkPatch, stage: stage) else { return nil }
        if let target, case .object(var fields) = kind, fields["target"] != nil {
            fields["target"] = .string(target.raw)
            kind = .object(fields)
        }
        return .object([
            "id": .string(id.raw), "number": .null, "name": .string(""), "notes": .string(""),
            "color": .string("none"), "second_color": .string("none"), "flagged": .bool(false),
            "armed": .bool(true), "auto_load": .bool(false), "skip_if_disarmed": .bool(false), "pre_wait": .double(0),
            "post_wait": .double(0), "continue_mode": .string("do_not_continue"),
            // The hotkey box starts unticked beside an empty field, as QLab's does.
            "triggers": .object(["hotkey": .null, "hotkey_enabled": .bool(false)]),
            "second_trigger": .string("nothing"),
            "second_trigger_on_release": .bool(false),
            // Core's defaults for a new cue (`FadeAndStopOthers::default`): off, peers, 1 s.
            "fade_and_stop_others": .object([
                "enabled": .bool(false), "scope": .string("peers"), "duration": .double(1),
            ]),
            // `DuckOthers::default`: off, -12 dB, 1 s.
            "duck_others": .object(["enabled": .bool(false), "db": .double(-12), "duration": .double(1)]),
            "kind": kind,
        ])
    }
}

extension Command {
    static func position(list: ModelID, parent: ModelID?, index: Int) -> JSONValue {
        .object([
            "list": .string(list.raw), "parent": parent.map { .string($0.raw) } ?? .null,
            "index": .int(Int64(index)),
        ])
    }

    /// `AddNetworkPatch` at `index`.
    static func addNetworkPatch(_ patch: JSONValue, index: Int) -> Command {
        Command(.object(["type": .string("add_network_patch"), "index": .int(Int64(index)), "patch": patch]))
    }

    /// `RemoveNetworkPatch`: refused while a cue still sends to it.
    static func removeNetworkPatch(_ id: ModelID) -> Command {
        Command(.object(["type": .string("remove_network_patch"), "id": .string(id.raw)]))
    }

    /// `SetNetworkPatchProperty`: `property` is `name`, `host`, `port`, `protocol` or `interface`.
    static func setNetworkPatch(_ id: ModelID, _ property: String, _ value: JSONValue) -> Command {
        Command(
            .object([
                "type": .string("set_network_patch_property"), "id": .string(id.raw),
                "property": .object(["type": .string(property), "value": value]),
            ]))
    }

    /// `AddCue` with the next automatic number (ED-08).
    static func addCue(_ cue: JSONValue, list: ModelID, parent: ModelID?, index: Int) -> Command {
        Command(
            .object([
                "type": .string("add_cue"), "at": position(list: list, parent: parent, index: index), "cue": cue,
                "auto_number": .bool(true),
            ]))
    }

    /// `PasteClipboard`: the raw copied cues, in order, one undo step. The engine makes the fresh ids
    /// and repairs targets, numbers and patches for this workspace (core's `prepare_paste`).
    /// `sourceDir` is the folder of the show the cues were copied from (nil for a show never saved): the
    /// engine reads their relative media paths against it so the files still play here.
    static func pasteClipboard(at: JSONValue, cues: [JSONValue], sourceDir: String? = nil) -> Command {
        var json: JSONValue = .object(["type": .string("paste_clipboard"), "at": at, "cues": .array(cues)])
        if let sourceDir { json.set("source_dir", .string(sourceDir)) }
        return Command(json)
    }

    static func removeCue(_ id: ModelID) -> Command {
        Command(.object(["type": .string("remove_cue"), "id": .string(id.raw)]))
    }

    static func moveCue(_ id: ModelID, index: Int) -> Command {
        Command(.object(["type": .string("move_cue"), "id": .string(id.raw), "index": .int(Int64(index))]))
    }

    static func reparentCue(_ id: ModelID, list: ModelID, parent: ModelID?, index: Int) -> Command {
        Command(
            .object([
                "type": .string("reparent_cue"), "id": .string(id.raw),
                "to": position(list: list, parent: parent, index: index),
            ]))
    }

    /// The numbers go out as prefix + number + suffix; an empty prefix or suffix is left off the wire.
    static func renumber(
        _ ids: [ModelID], start: Double, increment: Double, prefix: String = "", suffix: String = ""
    ) -> Command {
        var json: [String: JSONValue] = [
            "type": .string("renumber"), "ids": .array(ids.map { .string($0.raw) }), "start": .double(start),
            "increment": .double(increment),
        ]
        if !prefix.isEmpty { json["prefix"] = .string(prefix) }
        if !suffix.isEmpty { json["suffix"] = .string(suffix) }
        return Command(.object(json))
    }

    /// One undo step; a single command is sent as itself.
    static func transaction(_ commands: [Command]) -> Command? {
        guard let first = commands.first else { return nil }
        if commands.count == 1 { return first }
        return Command(.object(["type": .string("transaction"), "commands": .array(commands.map(\.json))]))
    }
}
