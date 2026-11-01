// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@MainActor
final class LaunchTests: XCTestCase {
    /// With an engine that has no workspace open the launcher is what the operator sees.
    func testLaunchWithNoWorkspaceShowsLauncher() {
        let app = launchApp(opening: [])
        XCTAssertTrue(app.windows["launcher-window"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["launcher-new"].exists)
        XCTAssertTrue(app.buttons["launcher-open"].exists)
    }

    /// The app starts its engine, which has two shows open: one window each, no launcher.
    func testLaunchShowsOneWindowPerWorkspace() {
        let app = launchApp()
        let windows = app.windows.matching(identifier: "main-window")
        XCTAssertTrue(windows.element(boundBy: 1).waitForExistence(timeout: 10))
        XCTAssertEqual(windows.count, 2)
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10), "the demo show's cues are listed")
        XCTAssertFalse(app.windows["launcher-window"].exists)
    }

    /// Closing one workspace leaves the other running; closing the last shows the launcher.
    func testClosingWorkspacesLeavesTheOtherThenShowsLauncher() {
        let app = launchApp()
        let windows = app.windows.matching(identifier: "main-window")
        XCTAssertTrue(windows.element(boundBy: 1).waitForExistence(timeout: 10))

        closeFrontWorkspace(app)
        XCTAssertTrue(waitFor("count == 1", windows))
        XCTAssertFalse(app.windows["launcher-window"].exists)

        closeFrontWorkspace(app)
        XCTAssertTrue(waitFor("count == 0", windows))
        XCTAssertTrue(app.windows["launcher-window"].waitForExistence(timeout: 10))
    }

    /// Cmd-W, confirming if the app asks (a workspace with a cue playing asks first).
    private func closeFrontWorkspace(_ app: XCUIApplication) {
        app.typeKey("w", modifierFlags: .command)
        let close = app.dialogs.buttons["Close"]
        if close.waitForExistence(timeout: 2) { close.click() }
    }
}
