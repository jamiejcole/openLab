// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// Record a hotkey on a cue in the inspector, then press it: the fake engine receives the `hotkey`,
/// matches it to that cue and moves it from Loaded to Running.
@MainActor
final class TriggersUITests: XCTestCase {
    func testRecordedHotkeyStartsTheCue() {
        let app = launchDemo()
        let loaded = app.images.matching(identifier: "cue-state").matching(NSPredicate(format: "label == 'Loaded'"))
        XCTAssertTrue(loaded.firstMatch.waitForExistence(timeout: 10))

        XCTAssertTrue(app.staticTexts["Door slam"].waitForExistence(timeout: 10))
        app.staticTexts["Door slam"].click()
        app.tabGroups["inspector-tabs"].tabs.element(boundBy: 1).click()  // Basics, Triggers
        let hotkey = app.staticTexts["inspector-hotkey"]
        XCTAssertTrue(hotkey.waitForExistence(timeout: 5))
        hotkey.click()
        hotkey.typeKey("j", modifierFlags: [])
        XCTAssertTrue(waitFor("value == 'J'", hotkey), "the recorded key shows once the engine answered")

        app.typeKey("j", modifierFlags: [])
        XCTAssertTrue(waitFor("count == 0", loaded), "the hotkey started the cue")
    }
}
