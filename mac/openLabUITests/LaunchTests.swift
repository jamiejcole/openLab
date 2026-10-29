// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@MainActor
final class LaunchTests: XCTestCase {
    /// With an engine that has no workspace open the launcher is what the operator sees. (The
    /// fake: a UI test must not start a real engine, which outlives the app and relaunches it.)
    func testLaunchWithNoWorkspaceShowsLauncher() {
        let app = launchFake(["-OpenLabFakeEmpty"])
        XCTAssertTrue(app.windows["launcher-window"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["launcher-new"].exists)
        XCTAssertTrue(app.buttons["launcher-open"].exists)
    }

    /// The fake engine needs no engine process and opens two workspaces, one window each.
    func testLaunchWithFakeEngineShowsOneWindowPerWorkspace() {
        let app = launchFake()
        XCTAssertTrue(app.windows.matching(identifier: "main-window").element(boundBy: 1).waitForExistence(timeout: 10))
        XCTAssertEqual(app.windows.matching(identifier: "main-window").count, 2)
        XCTAssertFalse(app.windows["launcher-window"].exists)
    }

    /// Closing one workspace leaves the other running; closing the last shows the launcher.
    func testClosingWorkspacesLeavesTheOtherThenShowsLauncher() {
        let app = launchFake()
        let windows = app.windows.matching(identifier: "main-window")
        XCTAssertTrue(windows.element(boundBy: 1).waitForExistence(timeout: 10))

        closeFrontWorkspace(app)
        XCTAssertTrue(waitFor("count == 1", windows))
        XCTAssertFalse(app.windows["launcher-window"].exists)

        closeFrontWorkspace(app)
        XCTAssertTrue(waitFor("count == 0", windows))
        XCTAssertTrue(app.windows["launcher-window"].waitForExistence(timeout: 10))
    }

    /// Cmd-W; the fake's first workspace has a cue playing, so closing it asks for confirmation.
    private func closeFrontWorkspace(_ app: XCUIApplication) {
        app.typeKey("w", modifierFlags: .command)
        let close = app.dialogs.buttons["Close"]
        if close.waitForExistence(timeout: 2) { close.click() }
    }
}
