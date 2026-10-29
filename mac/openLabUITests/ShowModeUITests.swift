// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// Show Mode against the fake engine: the inspector goes read-only, Space still fires GO.
@MainActor
final class ShowModeUITests: XCTestCase {
    func testShowModeLocksTheInspectorAndSpaceStillGoes() {
        let app = launchDemo()

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
        XCTAssertTrue(waitFor("count == 0", loaded), "GO still fires in Show Mode")

        app.radioGroups["mode-switch"].radioButtons["Edit"].click()
        XCTAssertTrue(waitFor("isEnabled == true", name, timeout: 5), "leaving Show Mode unlocks editing")
    }
}
