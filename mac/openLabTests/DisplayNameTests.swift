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
}
