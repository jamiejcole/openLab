// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// NFR-01: the fake engine crashes 3 s after launch. The alarm comes up without stopping the
/// app, a new engine is started, and both workspace windows are still there, reopened.
@MainActor
final class EngineAlarmUITests: XCTestCase {
    func testEngineCrashShowsTheAlarmAndKeepsTheWindows() {
        let app = launchFake(["-OpenLabFakeCrashAfter", "3"])
        let windows = app.windows.matching(identifier: "main-window")
        XCTAssertTrue(windows.element(boundBy: 1).waitForExistence(timeout: 10))

        let title = app.staticTexts["engine-alarm-title"]
        XCTAssertTrue(title.waitForExistence(timeout: 15), "the alarm is up")
        XCTAssertEqual(title.value as? String, "The engine crashed")

        // The new engine came up and the windows were reopened, not duplicated.
        let detail = app.staticTexts["engine-alarm-detail"]
        XCTAssertTrue(waitFor("value CONTAINS 'your workspaces were reopened'", detail, timeout: 15))
        XCTAssertEqual(windows.count, 2)

        // Not modal: the workspace windows still take keys while it is up (a key that acts on
        // the key window's workspace adds a cue there); it goes when dismissed.
        let cues = app.outlines.matching(identifier: "cue-list").outlineRows
        XCTAssertEqual(cues.count, 0, "the reopened workspaces are empty")
        windows.element(boundBy: 0).click()
        app.typeKey("0", modifierFlags: .command)  // New Group Cue
        XCTAssertTrue(cues.firstMatch.waitForExistence(timeout: 5), "the key reached the workspace window")
        XCTAssertTrue(title.exists, "the alarm is still up")
        app.buttons["engine-alarm-dismiss"].click()
        XCTAssertTrue(waitFor("exists == false", title, timeout: 5))
        XCTAssertEqual(windows.count, 2)
    }
}
