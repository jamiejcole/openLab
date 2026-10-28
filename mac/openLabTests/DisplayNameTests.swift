// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// `Cue.displayName` against the cases core's `display_name` is tested with (M2.29c, QLab check R2).
final class DisplayNameTests: XCTestCase {
    private final class Anchor {}

    private func cases() throws -> [JSONValue] {
        let url = try XCTUnwrap(Bundle(for: Anchor.self).url(forResource: "display_name", withExtension: "json"))
        let json = try XCTUnwrap(JSONValue.parse(try String(contentsOf: url, encoding: .utf8)))
        return try XCTUnwrap(json["cases"]?.arrayValue)
    }

    /// What core stores for a media path: a Windows drive path comes in with `/` separators
    /// (`MediaRef::new`). Anything else is kept as is, so a backslash stays a file-name character.
    private func stored(_ text: String) -> String {
        let chars = Array(text)
        let isDrive = chars.count > 2 && chars[0].isLetter && chars[1] == ":" && chars[2] == "\\"
        return isDrive ? text.replacingOccurrences(of: "\\", with: "/") : text
    }

    func testEveryFixtureCase() throws {
        let all = try cases()
        XCTAssertGreaterThanOrEqual(all.count, 6)
        for c in all {
            let label = c["label"]?.stringValue ?? "?"
            var kind: [String: JSONValue] = ["type": .string(c["audio"] == .bool(true) ? "audio" : "memo")]
            if c["audio"] == .bool(true), let media = c["media"]?.stringValue {
                kind["media"] = .string(stored(media))
            }
            let cue = try Cue(
                json: .object([
                    "id": .string("c1"), "name": .string(c["name"]?.stringValue ?? ""), "kind": .object(kind),
                ]))
            XCTAssertEqual(cue.displayName, c["expected"]?.stringValue, label)
        }
    }

    func testRenamingTheFileChangesTheDisplayNameButNotATypedName() throws {
        func cue(name: String, media: String) throws -> Cue {
            try Cue(
                json: .object([
                    "id": .string("c1"), "name": .string(name),
                    "kind": .object(["type": .string("audio"), "media": .string(media)]),
                ]))
        }
        XCTAssertEqual(try cue(name: "", media: "/s/a.wav").displayName, "a.wav")
        XCTAssertEqual(try cue(name: "", media: "/s/b.wav").displayName, "b.wav")
        XCTAssertEqual(try cue(name: "Typed", media: "/s/b.wav").displayName, "Typed")
        XCTAssertEqual(try cue(name: "", media: "/s/b.wav").name, "", "`name` is the typed name only")
    }

    func testSlashBeforeACombiningMarkStillSeparates() throws {
        let cue = try Cue(
            json: .object([
                "id": .string("c1"), "name": .string(""),
                "kind": .object(["type": .string("audio"), "media": .string("/s/dir/\u{301}x.wav")]),
            ]))
        XCTAssertEqual(cue.displayName, "\u{301}x.wav")
    }

    private func unnamedAudio(media: String, name: String = "") throws -> (Cue, WorkspaceModel) {
        let cue = try Cue(
            json: .object([
                "id": .string("c1"), "name": .string(name),
                "kind": .object(["type": .string("audio"), "media": .string(media)]),
            ]))
        let model = WorkspaceModel(
            id: "w", name: "w", settings: .object([:]), patches: [], networkPatches: [],
            cueLists: [CueList(id: "l", name: "l", cues: [cue])])
        return (cue, model)
    }

    private func nameCommand(_ text: String, _ cue: Cue, _ model: WorkspaceModel) throws -> Command? {
        try InspectorEdit.command(.name, text: text, cue: cue, model: model).get()
    }

    /// Rules 1-2: the field shows the file name as text; leaving it untouched writes nothing.
    func testUnnamedCueFieldShowsTheFileNameAndUntouchedSendsNothing() throws {
        let (cue, model) = try unnamedAudio(media: "/s/rain.wav")
        let shown = InspectorEdit.display(.name, cue: cue, model: model)
        XCTAssertEqual(shown, "rain.wav")
        XCTAssertNil(try nameCommand(shown, cue, model))
    }

    /// Rule 3: a changed text sets the typed name.
    func testChangedTextSetsTheName() throws {
        let (cue, model) = try unnamedAudio(media: "/s/rain.wav")
        let command = try XCTUnwrap(try nameCommand("Thunder", cue, model))
        XCTAssertEqual(command.json["property"]?["type"]?.stringValue, "name")
        XCTAssertEqual(command.json["property"]?["value"]?.stringValue, "Thunder")
    }

    /// Rule 4: clearing a named cue's field sets name "" and the field then shows the file name.
    func testClearingANamedFieldSetsAnEmptyNameAndShowsTheFileNameAgain() throws {
        let (named, model) = try unnamedAudio(media: "/s/rain.wav", name: "Thunder")
        let command = try XCTUnwrap(try nameCommand("", named, model))
        XCTAssertEqual(command.json["property"]?["value"]?.stringValue, "")
        let (cleared, model2) = try unnamedAudio(media: "/s/rain.wav")
        XCTAssertEqual(InspectorEdit.display(.name, cue: cleared, model: model2), "rain.wav")
        // Clearing an already-unnamed cue's field changes nothing; the view puts the file name back.
        XCTAssertNil(try nameCommand("", cleared, model2))
    }

    /// Rule 5: a renamed file shows in the field (render refreshes fields that are not being edited).
    func testFileRenameChangesTheShownName() throws {
        let (a, ma) = try unnamedAudio(media: "/s/a.wav")
        let (b, mb) = try unnamedAudio(media: "/s/b.wav")
        XCTAssertEqual(InspectorEdit.display(.name, cue: a, model: ma), "a.wav")
        XCTAssertEqual(InspectorEdit.display(.name, cue: b, model: mb), "b.wav")
        let (typed, mt) = try unnamedAudio(media: "/s/b.wav", name: "Typed")
        XCTAssertEqual(InspectorEdit.display(.name, cue: typed, model: mt), "Typed")
    }
}
