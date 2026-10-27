// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The inspector against the fake engine, which applies name edits and sends the `model_changed`:
/// a rename shows in the list only after the command went out and came back.
@MainActor
final class InspectorUITests: XCTestCase {
    func testRenamingACueShowsInTheList() {
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
        XCTAssertTrue(app.staticTexts["No cue selected"].exists)
        app.staticTexts["Intro"].click()
        let name = app.textFields["inspector-name"]
        XCTAssertTrue(name.waitForExistence(timeout: 5))
        XCTAssertEqual(name.value as? String, "Intro")
        name.click()
        name.typeKey("a", modifierFlags: .command)
        name.typeText("Thunder\r")

        XCTAssertTrue(app.staticTexts["Thunder"].waitForExistence(timeout: 10), "the list shows the new name")
        XCTAssertFalse(app.staticTexts["Intro"].exists)
    }
}
