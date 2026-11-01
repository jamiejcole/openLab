// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// NFR-01: the engine is killed (as a crash ends it) once both workspaces are up. The alarm comes
/// up without stopping the app, a new engine is started, and both workspace windows are still
/// there, their shows reopened.
@MainActor
final class EngineAlarmUITests: XCTestCase {
    func testEngineCrashShowsTheAlarmAndKeepsTheWindows() {
        let app = launchApp()
        let windows = app.windows.matching(identifier: "main-window")
        XCTAssertTrue(windows.element(boundBy: 1).waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10), "the demo show is loaded")

        killEngine()
        let title = app.staticTexts["engine-alarm-title"]
        XCTAssertTrue(title.waitForExistence(timeout: 15), "the alarm is up")
        XCTAssertEqual(title.value as? String, "The engine crashed")

        // The new engine came up and the windows were reopened, not duplicated.
        let detail = app.staticTexts["engine-alarm-detail"]
        XCTAssertTrue(waitFor("value CONTAINS 'your workspaces were reopened'", detail, timeout: 15))
        XCTAssertEqual(windows.count, 2)
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10), "the demo show is back")

        // Not modal: the workspace windows still take keys while it is up (a key that acts on
        // the key window's workspace adds a cue there); it goes when dismissed.
        let second = app.windows.matching(NSPredicate(format: "identifier == 'main-window' AND title == 'Second'"))
            .firstMatch
        let rows = second.outlines.matching(identifier: "cue-list").outlineRows
        XCTAssertEqual(rows.count, 0, "the reopened second show is empty")
        second.click()
        app.typeKey("0", modifierFlags: .command)  // New Group Cue
        XCTAssertTrue(rows.firstMatch.waitForExistence(timeout: 5), "the key reached the workspace window")
        XCTAssertTrue(title.exists, "the alarm is still up")
        app.buttons["engine-alarm-dismiss"].click()
        XCTAssertTrue(waitFor("exists == false", title, timeout: 5))
        XCTAssertEqual(windows.count, 2)
    }
}
