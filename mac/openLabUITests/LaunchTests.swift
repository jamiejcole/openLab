// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@MainActor
final class LaunchTests: XCTestCase {
    /// With the real engine and no workspace open the launcher is what the operator sees.
    func testLaunchWithNoWorkspaceShowsLauncher() {
        let app = XCUIApplication()
        app.launch()
        XCTAssertTrue(app.windows["launcher-window"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["launcher-new"].exists)
        XCTAssertTrue(app.buttons["launcher-open"].exists)
    }

    /// The fake engine needs no engine process and opens two workspaces, one window each.
    func testLaunchWithFakeEngineShowsOneWindowPerWorkspace() {
        let app = XCUIApplication()
        app.launchArguments += ["-OpenLabEngine", "fake"]
        app.launch()
        XCTAssertTrue(app.windows.matching(identifier: "main-window").element(boundBy: 1).waitForExistence(timeout: 10))
        XCTAssertEqual(app.windows.matching(identifier: "main-window").count, 2)
        XCTAssertFalse(app.windows["launcher-window"].exists)
    }

    /// Closing one workspace leaves the other running; closing the last shows the launcher.
    func testClosingWorkspacesLeavesTheOtherThenShowsLauncher() {
        let app = XCUIApplication()
        app.launchArguments += ["-OpenLabEngine", "fake"]
        app.launch()
        let windows = app.windows.matching(identifier: "main-window")
        XCTAssertTrue(windows.element(boundBy: 1).waitForExistence(timeout: 10))

        closeFrontWorkspace(app)
        XCTAssertTrue(waitForCount(windows, 1))
        XCTAssertFalse(app.windows["launcher-window"].exists)

        closeFrontWorkspace(app)
        XCTAssertTrue(waitForCount(windows, 0))
        XCTAssertTrue(app.windows["launcher-window"].waitForExistence(timeout: 10))
    }

    /// Cmd-W; the fake's first workspace has a cue playing, so closing it asks for confirmation.
    private func closeFrontWorkspace(_ app: XCUIApplication) {
        app.typeKey("w", modifierFlags: .command)
        let close = app.dialogs.buttons["Close"]
        if close.waitForExistence(timeout: 2) { close.click() }
    }

    private func waitForCount(_ query: XCUIElementQuery, _ count: Int) -> Bool {
        let predicate = NSPredicate(format: "count == %d", count)
        let exp = XCTNSPredicateExpectation(predicate: predicate, object: query)
        return XCTWaiter().wait(for: [exp], timeout: 10) == .completed
    }
}
