// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// Record a hotkey on a cue in the inspector, then press it: the fake engine receives the `hotkey`,
/// matches it to that cue and moves it from Loaded to Running.
@MainActor
final class TriggersUITests: XCTestCase {
    private func wait(_ format: String, _ object: Any) -> Bool {
        XCTWaiter().wait(
            for: [XCTNSPredicateExpectation(predicate: NSPredicate(format: format), object: object)], timeout: 10)
            == .completed
    }

    func testRecordedHotkeyStartsTheCue() {
        let app = XCUIApplication()
        app.launchArguments += ["-OpenLabEngine", "fake"]
        app.launch()
        let windows = app.windows.matching(identifier: "main-window")
        XCTAssertTrue(windows.element(boundBy: 1).waitForExistence(timeout: 10))
        app.typeKey("w", modifierFlags: .command)  // the empty second workspace is in front: close it
        XCTAssertTrue(wait("count == 1", windows))
        let loaded = app.images.matching(identifier: "cue-state").matching(NSPredicate(format: "label == 'Loaded'"))
        XCTAssertTrue(loaded.firstMatch.waitForExistence(timeout: 10))

        XCTAssertTrue(app.staticTexts["Door slam"].waitForExistence(timeout: 10))
        app.staticTexts["Door slam"].click()
        app.tabGroups["inspector-tabs"].tabs.element(boundBy: 1).click()  // Basics, Triggers
        let hotkey = app.staticTexts["inspector-hotkey"]
        XCTAssertTrue(hotkey.waitForExistence(timeout: 5))
        hotkey.click()
        hotkey.typeKey("j", modifierFlags: [])
        XCTAssertTrue(wait("value == 'J'", hotkey), "the recorded key shows once the engine answered")

        app.typeKey("j", modifierFlags: [])
        XCTAssertTrue(wait("count == 0", loaded), "the hotkey started the cue")
    }
}
