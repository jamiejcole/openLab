// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// Show Mode on the real engine: the inspector is hidden, Space still fires GO.
@MainActor
final class ShowModeUITests: XCTestCase {
    func testShowModeHidesTheInspectorAndSpaceStillGoes() {
        let app = launchDemo()

        app.staticTexts["Intro"].click()
        let name = app.textFields["inspector-name"]
        XCTAssertTrue(name.waitForExistence(timeout: 5))
        XCTAssertTrue(name.isEnabled)

        app.radioGroups["mode-switch"].radioButtons["Show"].click()
        XCTAssertTrue(app.staticTexts["show-mode-indicator"].waitForExistence(timeout: 5))
        XCTAssertTrue(waitFor("exists == false", name, timeout: 5), "the inspector is hidden in Show Mode")

        let running = cueStates("Running", in: app)
        XCTAssertEqual(running.count, 0)
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        XCTAssertTrue(running.firstMatch.waitForExistence(timeout: 10), "GO still fires in Show Mode")

        app.radioGroups["mode-switch"].radioButtons["Edit"].click()
        XCTAssertTrue(waitFor("exists == true", name, timeout: 5), "leaving Show Mode brings the inspector back")
    }
}
