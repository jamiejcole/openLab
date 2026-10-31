// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// Record a hotkey on a cue in the inspector, then press it: the engine receives the `hotkey`,
/// matches it to that cue and starts it.
@MainActor
final class TriggersUITests: XCTestCase {
    func testRecordedHotkeyStartsTheCue() {
        let app = launchDemo()
        XCTAssertEqual(cueStates("Running", in: app).count, 0)

        selectCue("Rain", in: app)
        app.tabGroups["inspector-tabs"].tabs.element(boundBy: 1).click()  // Basics, Triggers
        let hotkey = app.staticTexts["inspector-hotkey"]
        XCTAssertTrue(hotkey.waitForExistence(timeout: 5))
        hotkey.click()
        hotkey.typeKey("j", modifierFlags: [])
        XCTAssertTrue(waitFor("value == 'J'", hotkey), "the recorded key shows once the engine answered")

        app.typeKey("j", modifierFlags: [])
        XCTAssertTrue(cueState("Running", of: "Rain", in: app).waitForExistence(timeout: 10), "the hotkey started Rain")
    }
}
