// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// NFR-01: the fake engine crashes 3 s after launch. The alarm comes up without stopping the
/// app, a new engine is started, and both workspace windows are still there, reopened.
@MainActor
final class EngineAlarmUITests: XCTestCase {
    func testEngineCrashShowsTheAlarmAndKeepsTheWindows() {
        let app = XCUIApplication()
        app.launchArguments += ["-OpenLabEngine", "fake", "-OpenLabFakeCrashAfter", "3"]
        app.launch()
        let windows = app.windows.matching(identifier: "main-window")
        XCTAssertTrue(windows.element(boundBy: 1).waitForExistence(timeout: 10))

        let title = app.staticTexts["engine-alarm-title"]
        XCTAssertTrue(title.waitForExistence(timeout: 15), "the alarm is up")
        XCTAssertEqual(title.value as? String, "The engine crashed")

        // The new engine came up and the windows were reopened, not duplicated.
        let detail = app.staticTexts["engine-alarm-detail"]
        let reopened = NSPredicate(format: "value CONTAINS %@", "your workspaces were reopened")
        let exp = XCTNSPredicateExpectation(predicate: reopened, object: detail)
        XCTAssertEqual(XCTWaiter().wait(for: [exp], timeout: 15), .completed)
        XCTAssertEqual(windows.count, 2)

        // Not modal: the workspace windows still take keys while it is up; it goes when dismissed.
        windows.element(boundBy: 0).click()
        app.typeKey(XCUIKeyboardKey.escape, modifierFlags: [])
        app.buttons["engine-alarm-dismiss"].click()
        let gone = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: title)
        XCTAssertEqual(XCTWaiter().wait(for: [gone], timeout: 5), .completed)
        XCTAssertEqual(windows.count, 2)
    }
}
