// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// Show Mode against the fake engine: the inspector goes read-only, Space still fires GO.
@MainActor
final class ShowModeUITests: XCTestCase {
    func testShowModeLocksTheInspectorAndSpaceStillGoes() {
        let app = XCUIApplication()
        app.launchArguments += ["-OpenLabEngine", "fake"]
        app.launch()
        let windows = app.windows.matching(identifier: "main-window")
        XCTAssertTrue(windows.element(boundBy: 1).waitForExistence(timeout: 10))
        app.typeKey("w", modifierFlags: .command)  // the empty second workspace is in front: close it
        let one = NSPredicate(format: "count == 1")
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: one, object: windows)], timeout: 10),
            .completed)

        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        app.staticTexts["Intro"].click()
        let name = app.textFields["inspector-name"]
        XCTAssertTrue(name.waitForExistence(timeout: 5))
        XCTAssertTrue(name.isEnabled)

        app.radioGroups["mode-switch"].radioButtons["Show"].click()
        XCTAssertTrue(app.staticTexts["show-mode-indicator"].waitForExistence(timeout: 5))
        XCTAssertFalse(name.isEnabled, "the inspector is read-only in Show Mode")

        let states = app.images.matching(identifier: "cue-state")
        let loaded = states.matching(NSPredicate(format: "label == 'Loaded'"))
        XCTAssertTrue(loaded.firstMatch.waitForExistence(timeout: 10))
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        let gone = NSPredicate(format: "count == 0")
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: gone, object: loaded)], timeout: 10),
            .completed, "GO still fires in Show Mode")

        app.radioGroups["mode-switch"].radioButtons["Edit"].click()
        let enabled = NSPredicate(format: "isEnabled == true")
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: enabled, object: name)], timeout: 5),
            .completed, "leaving Show Mode unlocks editing")
    }
}
