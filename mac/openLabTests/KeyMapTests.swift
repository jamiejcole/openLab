// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

@MainActor
final class KeyMapTests: XCTestCase {
    private var defaults: UserDefaults!

    override func setUp() async throws { defaults = AppControllerTests.scratchDefaults() }

    func testDefaultsFollowQLab() {
        let map = KeyMap(store: defaults)
        XCTAssertEqual(map.chord(for: .go), KeyChord(" "))
        XCTAssertEqual(map.chord(for: .pauseAll), KeyChord("["))
        XCTAssertEqual(map.chord(for: .resumeAll), KeyChord("]"))
        XCTAssertNil(map.chord(for: .stopAll))
        XCTAssertNil(map.chord(for: .panic))
    }

    func testRemapPersistsAndChangesTheDispatchedKey() {
        let map = KeyMap(store: defaults)
        var changes = 0
        map.onChange = { changes += 1 }
        XCTAssertEqual(map.bind(.go, to: KeyChord("g")), nil)
        XCTAssertEqual(changes, 1)
        XCTAssertEqual(map.command(for: KeyChord("g")), .go)
        XCTAssertNil(map.command(for: KeyChord(" ")))
        let reloaded = KeyMap(store: defaults)
        XCTAssertEqual(reloaded.chord(for: .go), KeyChord("g"))
        reloaded.unbind(.go)
        XCTAssertNil(KeyMap(store: defaults).chord(for: .go))
        reloaded.resetToDefaults()
        XCTAssertEqual(KeyMap(store: defaults).chord(for: .go), KeyChord(" "))
    }

    func testDuplicateBindingIsRejectedAndLeavesTheMapUnchanged() {
        let map = KeyMap(store: defaults)
        XCTAssertEqual(map.bind(.stopAll, to: KeyChord("[")), .duplicate(of: .pauseAll))
        XCTAssertNil(map.chord(for: .stopAll))
        XCTAssertEqual(map.bind(.go, to: KeyChord(" ")), nil, "rebinding a command to its own key is fine")
    }

    func testCueHotkeyConflictIsRejected() {
        let map = KeyMap(store: defaults)
        map.isCueHotkey = { $0 == KeyChord("x") }
        XCTAssertEqual(map.bind(.stopAll, to: KeyChord("x")), .cueHotkey)
    }

    func testEscapeAndBareArrowsAreReserved() {
        let map = KeyMap(store: defaults)
        XCTAssertEqual(map.bind(.go, to: KeyChord(KeyChord.escape)), .reserved)
        XCTAssertEqual(map.bind(.go, to: KeyChord(KeyChord.escape, .command)), .reserved)
        XCTAssertEqual(map.bind(.panic, to: KeyChord("q")), .reserved)
        XCTAssertEqual(map.bind(.go, to: KeyChord(KeyChord.down)), .reserved)
        XCTAssertEqual(map.bind(.go, to: KeyChord(KeyChord.down, .command)), nil)
    }

    func testDamagedOrConflictingStoredMapFallsBackToDefaults() throws {
        defaults.set(Data("nonsense".utf8), forKey: KeyMap.defaultsKey)
        XCTAssertEqual(KeyMap(store: defaults).bindings, KeyMap.defaults)
        let clash = ["go": KeyChord("a"), "pauseAll": KeyChord("a")]
        defaults.set(try JSONEncoder().encode(clash), forKey: KeyMap.defaultsKey)
        XCTAssertEqual(KeyMap(store: defaults).bindings, KeyMap.defaults)
        let esc = ["go": KeyChord(KeyChord.escape)]
        defaults.set(try JSONEncoder().encode(esc), forKey: KeyMap.defaultsKey)
        XCTAssertEqual(KeyMap(store: defaults).bindings, KeyMap.defaults)
    }

    func testTypingClassification() {
        XCTAssertTrue(KeyChord(" ").typesCharacters)
        XCTAssertTrue(KeyChord("a", .shift).typesCharacters)
        XCTAssertFalse(KeyChord("a", .command).typesCharacters)
    }
}
