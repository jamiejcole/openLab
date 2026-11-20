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

    /// Launcher flow: a real click on New opens an untitled workspace in its own window and the
    /// launcher goes away.
    func testClickingNewOnTheLauncherOpensAWorkspaceAndClosesTheLauncher() {
        let app = launchApp(opening: [])
        let launcher = app.windows["launcher-window"]
        XCTAssertTrue(launcher.waitForExistence(timeout: 10))
        let new = app.buttons["launcher-new"]
        XCTAssertTrue(waitFor("isEnabled == true", new, timeout: 20), "usable once the engine is ready")
        new.click()
        XCTAssertTrue(app.windows.matching(identifier: "main-window").element(boundBy: 0).waitForExistence(timeout: 10))
        XCTAssertTrue(waitFor("exists == false", launcher, timeout: 10), "the launcher closes")
    }

    /// The launcher with Demo listed as the one recent show.
    private func launcherWithADemoRecent() -> XCUIApplication {
        let app = launchApp(opening: [], recent: ["Demo"])
        XCTAssertTrue(app.windows["launcher-window"].waitForExistence(timeout: 10))
        XCTAssertTrue(
            app.tables["launcher-recents"].tableRows.firstMatch.waitForExistence(timeout: 10), "Demo is listed")
        return app
    }

    /// Launcher flow: a double-click on a recent show opens it in a workspace window and the
    /// launcher closes.
    func testDoubleClickingARecentShowOnTheLauncherOpensItAndClosesTheLauncher() {
        let app = launcherWithADemoRecent()
        app.tables["launcher-recents"].tableRows.firstMatch.doubleClick()
        XCTAssertTrue(app.windows.matching(identifier: "main-window").element(boundBy: 0).waitForExistence(timeout: 10))
        XCTAssertTrue(waitFor("exists == false", app.windows["launcher-window"], timeout: 10), "the launcher closes")
    }

    /// Launcher flow: select a recent show and press Return: it opens and the launcher closes.
    func testReturnOnTheSelectedRecentShowOpensItAndClosesTheLauncher() {
        let app = launcherWithADemoRecent()
        app.tables["launcher-recents"].tableRows.firstMatch.click()
        app.typeKey(.return, modifierFlags: [])
        XCTAssertTrue(app.windows.matching(identifier: "main-window").element(boundBy: 0).waitForExistence(timeout: 10))
        XCTAssertTrue(waitFor("exists == false", app.windows["launcher-window"], timeout: 10), "the launcher closes")
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
