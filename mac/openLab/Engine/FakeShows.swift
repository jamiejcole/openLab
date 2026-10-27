// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// Workspaces for the fake engine: the UI-test demo show and a large one for the 2,000-cue checks.
extension FakeEngine {
    static let demoListID = "00000000-0000-0000-0000-000000000001"

    /// A cue as the engine sends it (`kind` is the object under `kind`).
    static func cueJSON(
        id: String, number: String?, name: String, kind: JSONValue, color: String = "none", flagged: Bool = false,
        armed: Bool = true, preWait: Double = 0, postWait: Double = 0, continueMode: String = "do_not_continue"
    ) -> JSONValue {
        .object([
            "id": .string(id), "number": number.map(JSONValue.string) ?? .null, "name": .string(name),
            "notes": .string(""), "color": .string(color), "flagged": .bool(flagged), "armed": .bool(armed),
            "pre_wait": .double(preWait), "post_wait": .double(postWait), "continue_mode": .string(continueMode),
            "kind": kind,
        ])
    }

    /// An audio cue's `kind` with every field the engine sends, so message sizes are realistic.
    static func audioKind(_ media: String) -> JSONValue {
        let fade = JSONValue.object(["duration": .double(0), "curve": .string("linear")])
        let output = JSONValue.object(["db": .double(0), "mute": .bool(false)])
        return .object([
            "type": .string("audio"), "media": .string(media),
            "patch": .string("00000000-0000-0000-0000-000000000001"), "start_time": .double(0), "end_time": .null,
            "fade_in": fade, "fade_out": fade, "slices": .array([]), "last_slice_play_count": .int(1),
            "levels": .object([
                "master_db": .double(0), "outputs": .array([output, output]), "crosspoints": .array([]),
                "gangs": .array([]),
            ]),
            "preload": .bool(false),
        ])
    }

    static func groupKind(_ children: [JSONValue]) -> JSONValue {
        .object([
            "type": .string("group"), "mode": .object(["type": .string("timeline")]), "children": .array(children),
        ])
    }

    /// The protocol fixture's show (cue 1 running, a fade cue with a missing target at the playhead)
    /// plus a collapsed group of three cues: loaded, paused (blue, flagged) and broken by a lost device.
    static func demoSnapshot(base: JSONValue? = nil) -> JSONValue {
        var s = base ?? bundledSnapshot() ?? emptySnapshot(name: "Demo")
        let kids = [
            cueJSON(
                id: "00000000-0000-0000-0000-000000000021", number: "2.1", name: "Door slam",
                kind: audioKind("media/d.wav")),
            cueJSON(
                id: "00000000-0000-0000-0000-000000000022", number: "2.2", name: "Rain", kind: audioKind("media/r.wav"),
                color: "blue", flagged: true, continueMode: "auto_follow"),
            cueJSON(
                id: "00000000-0000-0000-0000-000000000023", number: "2.3", name: "Radio", kind: audioKind("media/o.wav")
            ),
        ]
        let group = cueJSON(
            id: "00000000-0000-0000-0000-000000000020", number: "2", name: "Act One", kind: groupKind(kids),
            color: "green")
        let cues = (s["workspace"]?["cue_lists"]?.arrayValue?.first?["cues"]?.arrayValue ?? []) + [group]
        s["workspace"]?.set(
            "cue_lists",
            .array([
                .object([
                    "id": .string(demoListID), "name": .string("Main"), "cues": .array(cues),
                ])
            ]))
        let states =
            (s["cue_states"]?.arrayValue ?? []) + [
                .object(["cue": .string("00000000-0000-0000-0000-000000000021"), "state": .string("loaded")]),
                .object(["cue": .string("00000000-0000-0000-0000-000000000022"), "state": .string("paused")]),
                .object([
                    "cue": .string("00000000-0000-0000-0000-000000000023"), "state": .string("broken"),
                    "reason": .string("device_lost"),
                ]),
            ]
        s.set("cue_states", .array(states))
        s.set("previewing", .array([]))  // the demo show starts with nothing previewing
        return s
    }

    /// `count` cues in all: flat cues with a group of ten children after every thirty top-level cues.
    static func bigSnapshot(cues count: Int) -> JSONValue {
        var s = emptySnapshot(name: "Big")
        var top: [JSONValue] = []
        var made = 0
        func next(_ name: String) -> JSONValue {
            made += 1
            return cueJSON(
                id: String(format: "00000000-0000-0000-0001-%012d", made), number: "\(made)", name: "\(name) \(made)",
                kind: audioKind("media/\(made).wav"), color: made % 7 == 0 ? "red" : "none", flagged: made % 11 == 0,
                postWait: made % 5 == 0 ? 1.5 : 0)
        }
        while made < count {
            if top.count % 31 == 30 && made + 11 <= count {
                let kids = (0..<10).map { _ in next("Child") }
                made += 1
                top.append(
                    cueJSON(
                        id: String(format: "00000000-0000-0000-0001-%012d", made), number: "\(made)",
                        name: "Group \(made)",
                        kind: groupKind(kids)))
            } else {
                top.append(next("Cue"))
            }
        }
        s["workspace"]?.set(
            "cue_lists",
            .array([.object(["id": .string(demoListID), "name": .string("Main"), "cues": .array(top)])]))
        return s
    }
}
