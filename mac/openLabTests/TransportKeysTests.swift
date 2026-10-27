// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

@MainActor
final class TransportKeysTests: XCTestCase {
    private func settings(_ hotkeys: String) throws -> JSONValue {
        .object(["hotkeys": try XCTUnwrap(JSONValue.parse(hotkeys))])
    }

    private func hk(_ key: String, _ mods: [String] = []) -> String { AppControllerTests.hotkey(key, mods) }

    func testNoSettingsGivesQLabDefaults() {
        let keys = TransportKeys(settings: nil)
        XCTAssertEqual(keys.chord(for: .go), KeyChord(" "))
        XCTAssertEqual(keys.chord(for: .pauseAll), KeyChord("["))
        XCTAssertEqual(keys.chord(for: .resumeAll), KeyChord("]"))
        XCTAssertEqual(keys.chord(for: .playheadNext), KeyChord(KeyChord.down, [.command, .shift]))
        XCTAssertEqual(keys.chord(for: .pauseResumeSelected), KeyChord("p"))
        XCTAssertEqual(keys.chord(for: .loadSelected), KeyChord("l"))
        XCTAssertNil(keys.chord(for: .stopAll))
        XCTAssertNil(keys.chord(for: .panic))
        XCTAssertTrue(keys.warnings.isEmpty)
    }

    func testTheEnginesNewWorkspaceDefaultsMapToTheAppBehaviour() throws {
        let model = try settings(
            """
            {"go":\(hk("space")),"stop_all":\(hk("escape")),"pause_all":\(hk("[")),"resume_all":\(hk("]")),
             "playhead_next":\(hk("down")),"playhead_previous":\(hk("up"))}
            """)
        let keys = TransportKeys(settings: model)
        XCTAssertEqual(keys.chord(for: .go), KeyChord(" "))
        XCTAssertNil(keys.chord(for: .stopAll), "Esc stays the app-wide panic")
        XCTAssertNil(keys.chord(for: .playheadNext), "bare arrows: the selection moves the playhead")
        XCTAssertNil(keys.chord(for: .playheadPrevious))
        XCTAssertTrue(keys.warnings.isEmpty)
    }

    func testNullAbsentAndModifiers() throws {
        let keys = TransportKeys(
            settings: try settings(#"{"go":null,"stop_all":\#(hk("s", ["command", "alt"]))}"#))
        XCTAssertNil(keys.chord(for: .go))
        XCTAssertEqual(keys.chord(for: .stopAll), KeyChord("s", [.command, .option]))
        XCTAssertEqual(keys.chord(for: .pauseAll), KeyChord("["))
    }

    func testDuplicateReservedAndCueHotkeyAreRefusedWithWarnings() throws {
        let keys = TransportKeys(
            settings: try settings(#"{"go":\#(hk("a")),"stop_all":\#(hk("a")),"pause_all":\#(hk("p"))}"#),
            isCueHotkey: { $0 == KeyChord("]") })
        XCTAssertEqual(keys.chord(for: .go), KeyChord("a"))
        XCTAssertNil(keys.chord(for: .stopAll))
        XCTAssertEqual(keys.chord(for: .pauseAll), KeyChord("p"))
        XCTAssertNil(keys.chord(for: .pauseResumeSelected), "the fixed P key yields to the show's key")
        XCTAssertNil(keys.chord(for: .resumeAll), "a cue hotkey owns ]")
        XCTAssertEqual(keys.warnings.count, 3, "\(keys.warnings)")
    }

    func testUnsupportedAndMalformedHotkeysWarnNotCrash() throws {
        let keys = TransportKeys(settings: try settings(#"{"go":{"key":"f13"},"pause_all":7,"resume_all":{}}"#))
        XCTAssertNil(keys.chord(for: .go))
        XCTAssertEqual(keys.warnings.count, 3)
        XCTAssertEqual(TransportKeys(settings: .string("nonsense")).chord(for: .go), KeyChord(" "))
    }

    func testValidateIsPure() {
        let existing: [TransportCommand: KeyChord] = [.go: KeyChord(" ")]
        XCTAssertEqual(TransportKeys.validate(KeyChord(" "), for: .pauseAll, in: existing), .duplicate(of: .go))
        XCTAssertNil(TransportKeys.validate(KeyChord(" "), for: .go, in: existing), "its own key")
        XCTAssertEqual(TransportKeys.validate(KeyChord(KeyChord.escape), for: .go, in: [:]), .reserved)
        XCTAssertEqual(TransportKeys.validate(KeyChord("q"), for: .panic, in: [:]), .reserved)
        XCTAssertEqual(TransportKeys.validate(KeyChord(KeyChord.down), for: .go, in: [:]), .reserved)
        XCTAssertNil(TransportKeys.validate(KeyChord(KeyChord.down, .command), for: .go, in: [:]))
        XCTAssertEqual(
            TransportKeys.validate(KeyChord("x"), for: .go, in: [:], isCueHotkey: { _ in true }), .cueHotkey)
    }

    func testTypingClassification() {
        XCTAssertTrue(KeyChord(" ").typesCharacters)
        XCTAssertTrue(KeyChord("a", .shift).typesCharacters)
        XCTAssertFalse(KeyChord("a", .command).typesCharacters)
    }
}
