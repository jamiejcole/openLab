// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// The cue kinds the toolbar and Cues menu can add (the model's kinds that need no choice first;
/// Network needs a destination patch and has no toolbar button).
enum NewCueKind: String, CaseIterable, Sendable {
    case audio, group, memo, wait, fade, start, stop, pause, load, reset, devamp, goto, arm, disarm

    var titleKey: String { "cue.type.\(rawValue)" }

    /// Core's wire name for the kind.
    var wireType: String { self == .goto ? "go_to" : rawValue }

    /// The `kind` object with defaults. Audio needs a patch (core requires one) and gets one cue
    /// output per patch output.
    func kindJSON(patch: ModelObject?) -> JSONValue? {
        let fade = JSONValue.object(["duration": .double(0), "curve": .string("linear")])
        var fields: [String: JSONValue] = ["type": .string(wireType)]
        switch self {
        case .audio:
            guard let patch else { return nil }
            let count = patch.raw["outputs"]?.arrayValue?.count ?? 0
            let output = JSONValue.object(["db": .double(0), "mute": .bool(false)])
            fields.merge([
                "media": .null, "patch": .string(patch.id.raw), "start_time": .double(0), "end_time": .null,
                "fade_in": fade, "fade_out": fade, "slices": .array([]), "last_slice_play_count": .int(1),
                "levels": .object([
                    "master_db": .double(0), "outputs": .array(Array(repeating: output, count: count)),
                    "crosspoints": .array([]), "gangs": .array([]),
                ]),
                "preload": .bool(false),
            ]) { a, _ in a }
        case .group:
            fields.merge(["mode": .object(["type": .string("timeline")]), "children": .array([])]) { a, _ in a }
        case .memo: break
        case .wait: fields["duration"] = .double(5)
        case .fade:
            fields.merge([
                "target": .null, "duration": .double(3), "curve": .string("linear"),
                "levels": .object(["master_db": .null, "output_db": .array([]), "crosspoints": .array([])]),
                "stop_target_when_done": .bool(false),
            ]) { a, _ in a }
        case .stop: fields.merge(["target": .null, "fade_out": fade]) { a, _ in a }
        case .load: fields.merge(["target": .null, "load_to_time": .double(0)]) { a, _ in a }
        case .devamp:
            fields.merge([
                "target": .null, "start_next_cue": .bool(false), "stop_target_when_slice_ends": .bool(false),
            ]) { a, _ in a }
        case .goto: fields.merge(["target": .null, "then_go": .bool(false)]) { a, _ in a }
        case .start, .pause, .reset, .arm, .disarm: fields["target"] = .null
        }
        return .object(fields)
    }

    /// A whole new cue (no number: `auto_number` gives it one).
    func cueJSON(id: ModelID, patch: ModelObject?) -> JSONValue? {
        guard let kind = kindJSON(patch: patch) else { return nil }
        return .object([
            "id": .string(id.raw), "number": .null, "name": .string(""), "notes": .string(""),
            "color": .string("none"), "flagged": .bool(false), "armed": .bool(true), "pre_wait": .double(0),
            "post_wait": .double(0), "continue_mode": .string("do_not_continue"),
            "triggers": .object(["hotkey": .null, "osc": .null]), "second_trigger": .string("nothing"),
            "second_trigger_on_release": .bool(false), "kind": kind,
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

    /// `AddCue` with the next automatic number (ED-08).
    static func addCue(_ cue: JSONValue, list: ModelID, parent: ModelID?, index: Int) -> Command {
        Command(
            .object([
                "type": .string("add_cue"), "at": position(list: list, parent: parent, index: index), "cue": cue,
                "auto_number": .bool(true),
            ]))
    }

    /// `Paste`: several prepared cues (fresh ids; `CueEditor.preparePaste`) in order, one undo step.
    static func paste(at: JSONValue, cues: [JSONValue]) -> Command {
        Command(.object(["type": .string("paste"), "at": at, "cues": .array(cues)]))
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

    static func renumber(_ ids: [ModelID], start: Double, increment: Double) -> Command {
        Command(
            .object([
                "type": .string("renumber"), "ids": .array(ids.map { .string($0.raw) }), "start": .double(start),
                "increment": .double(increment),
            ]))
    }

    /// One undo step; a single command is sent as itself.
    static func transaction(_ commands: [Command]) -> Command? {
        guard let first = commands.first else { return nil }
        if commands.count == 1 { return first }
        return Command(.object(["type": .string("transaction"), "commands": .array(commands.map(\.json))]))
    }
}
