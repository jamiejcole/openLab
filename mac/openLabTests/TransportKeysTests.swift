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
        XCTAssertEqual(keys.chord(for: .pauseResumeSelected), KeyChord("p"))
        XCTAssertEqual(keys.chord(for: .loadSelected), KeyChord("l"))
        XCTAssertEqual(keys.chord(for: .previewSelected), KeyChord("v"))
        XCTAssertEqual(keys.chord(for: .panicSelected), KeyChord("s"))
        XCTAssertNil(keys.chord(for: .resetAll))
        XCTAssertNil(keys.chord(for: .stopAll))
        XCTAssertNil(keys.chord(for: .panic))
        XCTAssertTrue(keys.warnings.isEmpty)
    }

    func testTheEnginesNewWorkspaceDefaultsMapToTheAppBehaviour() throws {
        let model = try settings(
            """
            {"go":\(hk("space")),"stop_all":\(hk("escape")),"pause_all":\(hk("[")),"resume_all":\(hk("]"))}
            """)
        let keys = TransportKeys(settings: model)
        XCTAssertEqual(keys.chord(for: .go), KeyChord(" "))
        XCTAssertNil(keys.chord(for: .stopAll), "Esc stays the app-wide panic")
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
            settings: try settings(#"{"go":\#(hk("a")),"stop_all":\#(hk("a")),"pause_all":\#(hk("p"))}"#))
        XCTAssertEqual(keys.chord(for: .go), KeyChord("a"))
        XCTAssertNil(keys.chord(for: .stopAll))
        XCTAssertEqual(keys.chord(for: .pauseAll), KeyChord("p"))
        XCTAssertNil(keys.chord(for: .pauseResumeSelected), "its default P is Pause All's here: the later field loses")
        XCTAssertEqual(keys.chord(for: .resumeAll), KeyChord("]"), "cue hotkeys do not take transport keys")
        XCTAssertEqual(keys.warnings.count, 2, "\(keys.warnings)")
        XCTAssertTrue(keys.warnings[1].contains(loc("menu.transport.pauseResumeSelected")), "\(keys.warnings)")
        // The show log reads them: localised words and menu titles, never field names (L26).
        XCTAssertEqual(
            keys.warnings.first,
            String(
                format: loc("hotkeys.warn.unusable"), loc("menu.transport.stopAll"),
                String(format: loc("hotkeys.reason.duplicate"), loc("menu.transport.go"))))
        XCTAssertFalse(keys.warnings.contains { $0.contains("hotkeys.") || $0.contains("_") }, "\(keys.warnings)")
    }

    func testEverySelectedCueKeyComesFromTheModel() throws {
        let keys = TransportKeys(
            settings: try settings(
                #"{"load_selected":\#(hk("k")),"pause_resume_selected":null,"start_selected":\#(hk("j")),"stop_selected":\#(hk("i", ["shift"])),"reset_selected":\#(hk("u")),"preview_selected":\#(hk("y")),"panic_selected":null}"#
            ))
        XCTAssertEqual(keys.chord(for: .loadSelected), KeyChord("k"))
        XCTAssertNil(keys.chord(for: .pauseResumeSelected))
        XCTAssertEqual(keys.chord(for: .startSelected), KeyChord("j"))
        XCTAssertEqual(keys.chord(for: .stopSelected), KeyChord("i", [.shift]))
        XCTAssertEqual(keys.chord(for: .resetSelected), KeyChord("u"))
        XCTAssertEqual(keys.chord(for: .previewSelected), KeyChord("y"), "remapped")
        XCTAssertNil(keys.chord(for: .panicSelected), "cleared")
        XCTAssertNil(keys.command(for: KeyChord("v")), "the old default is not kept")
        XCTAssertNil(keys.command(for: KeyChord("l")), "the old default is not kept")
        XCTAssertTrue(keys.warnings.isEmpty, "\(keys.warnings)")
        // Absent fields take the engine's new-workspace defaults.
        let defaults = TransportKeys(settings: try settings("{}"))
        XCTAssertEqual(defaults.chord(for: .loadSelected), KeyChord("l"))
        XCTAssertEqual(defaults.chord(for: .pauseResumeSelected), KeyChord("p"))
        XCTAssertNil(defaults.chord(for: .startSelected))
        XCTAssertEqual(defaults.chord(for: .previewSelected), KeyChord("v"))
        XCTAssertEqual(defaults.chord(for: .panicSelected), KeyChord("s"))
    }

    func testAPreviewKeyRepeatedByAnotherKeyIsDroppedWithAWarning() throws {
        let keys = TransportKeys(settings: try settings(#"{"preview_selected":\#(hk("s"))}"#))
        XCTAssertEqual(keys.chord(for: .previewSelected), KeyChord("s"), "first field wins")
        XCTAssertNil(keys.chord(for: .panicSelected))
        XCTAssertEqual(keys.warnings.count, 1)
    }

    func testUnsupportedAndMalformedHotkeysWarnNotCrash() throws {
        let keys = TransportKeys(settings: try settings(#"{"go":{"key":"f13"},"pause_all":7,"resume_all":{}}"#))
        XCTAssertNil(keys.chord(for: .go))
        XCTAssertEqual(keys.warnings.count, 3)
        XCTAssertEqual(TransportKeys(settings: .string("nonsense")).chord(for: .go), KeyChord(" "))
    }

    func testValidateIsPure() {
        let existing: [WorkspaceKey: KeyChord] = [.go: KeyChord(" "), .editDuration: KeyChord("d")]
        XCTAssertEqual(TransportKeys.validate(KeyChord(" "), for: .pauseAll, in: existing), .duplicate(of: .go))
        XCTAssertNil(TransportKeys.validate(KeyChord(" "), for: .go, in: existing), "its own key")
        // The two columns are one map: a cue-editing key and a workspace key never share a chord.
        XCTAssertEqual(TransportKeys.validate(KeyChord(" "), for: .editName, in: existing), .duplicate(of: .go))
        XCTAssertEqual(
            TransportKeys.validate(KeyChord("d"), for: .go, in: existing), .duplicate(of: .editDuration))
        XCTAssertNil(
            TransportKeys.validate(KeyChord("s", .command), for: .go, in: [:]), "a menu command's key is allowed")
        XCTAssertEqual(TransportKeys.validate(KeyChord("\u{7F}"), for: .go, in: [:]), .unsupported)
        XCTAssertEqual(TransportKeys.validate(KeyChord(KeyChord.escape), for: .go, in: [:]), .reserved)
        XCTAssertEqual(TransportKeys.validate(KeyChord(KeyChord.down), for: .go, in: [:]), .reserved)
        // Up and Down are refused with any modifiers: there are no arrow keys in the key map.
        for mods: NSEvent.ModifierFlags in [.command, .shift, .option, .control, [.command, .shift]] {
            XCTAssertEqual(TransportKeys.validate(KeyChord(KeyChord.down, mods), for: .go, in: [:]), .reserved)
            XCTAssertEqual(TransportKeys.validate(KeyChord(KeyChord.up, mods), for: .go, in: [:]), .reserved)
        }
        XCTAssertEqual(TransportKeys.validate(KeyChord(KeyChord.escape, .shift), for: .go, in: [:]), .reserved)
        XCTAssertNil(TransportKeys.validate(KeyChord(KeyChord.left, .command), for: .go, in: [:]))
        XCTAssertEqual(
            TransportKeys.validate(KeyChord("x"), for: .go, in: [:], isCueHotkey: { _ in true }), .cueHotkey)
    }

    func testAStaleArrowKeyInAShowFileWarnsAndBindsNothing() throws {
        let keys = TransportKeys(
            settings: try settings(#"{"go":\#(hk("down", ["command", "shift"])),"pause_all":\#(hk("up", ["option"]))}"#)
        )
        XCTAssertNil(keys.chord(for: .go))
        XCTAssertNil(keys.chord(for: .pauseAll))
        XCTAssertEqual(keys.warnings.count, 2)
    }

    func testTypingClassification() {
        XCTAssertTrue(KeyChord(" ").typesCharacters)
        XCTAssertTrue(KeyChord("a", .shift).typesCharacters)
        XCTAssertFalse(KeyChord("a", .command).typesCharacters)
    }
}
