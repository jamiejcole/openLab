// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The GO key on the real engine: Space starts the cue at the playhead (the demo show's Intro).
@MainActor
final class TransportUITests: XCTestCase {
    func testSpaceStartsTheCueAtThePlayhead() {
        let app = launchDemo()
        XCTAssertEqual(cueStates("Running", in: app).count, 0, "nothing plays when the show opens")

        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])

        XCTAssertTrue(cueState("Running", of: "Intro", in: app).waitForExistence(timeout: 10), "Intro runs after GO")
        XCTAssertEqual(cueStates("Running", in: app).count, 1, "and nothing else")
    }

    /// Opening the workspace status window makes it the key window: with it in front (a click in its
    /// log too), Space fires no GO. A click back on the workspace makes Space fire GO again. With it
    /// key, Esc stops the running cue and leaves the window open.
    func testTheStatusWindowInFrontTakesSpaceAClickOnTheWorkspaceGivesGoBackAndEscPanics() {
        let app = launchDemo()
        let workspace = app.windows.matching(identifier: "main-window").firstMatch

        app.menuBars.menuBarItems["Window"].click()
        app.menuBars.menuItems["Workspace Status"].click()
        let status = app.descendants(matching: .any).matching(identifier: "status-window").firstMatch
        XCTAssertTrue(status.waitForExistence(timeout: 10), "the status window opens")
        // Before any click in it, which would make any panel key.
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        XCTAssertFalse(
            cueStates("Running", in: app).firstMatch.waitForExistence(timeout: 2), "opened, it is key: no GO")
        tabButton("Logs", in: app).click()
        let log = app.tables.matching(identifier: "log-table").firstMatch
        XCTAssertTrue(log.waitForExistence(timeout: 10), "on its Logs page")
        log.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.9)).click()
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        XCTAssertFalse(
            cueStates("Running", in: app).firstMatch.waitForExistence(timeout: 2), "no GO with the status window key")

        // The title bar, clear of the status window floating over the list.
        workspace.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0))
            .withOffset(CGVector(dx: 0, dy: 10)).click()
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        XCTAssertTrue(
            cueState("Running", of: "Intro", in: app).waitForExistence(timeout: 10),
            "GO fires with the workspace key again")
        XCTAssertEqual(cueStates("Running", in: app).count, 1, "one GO, the Spaces before fired none")
        XCTAssertTrue(status.exists, "the status window is still open")

        app.menuBars.menuBarItems["Window"].click()
        app.menuBars.menuItems["Workspace Status"].click()
        // Key again with no click in it: a GO would start the Fade after Intro, and Intro would be fading.
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        XCTAssertFalse(
            waitFor("exists == false", cueState("Running", of: "Intro", in: app), timeout: 3),
            "brought up again, it is key: no GO")
        app.typeKey(XCUIKeyboardKey.escape.rawValue, modifierFlags: [])
        XCTAssertTrue(waitFor("count == 0", cueStates("Running", in: app)), "Esc stopped Intro")
        XCTAssertTrue(status.exists, "and left the status window open")
    }
}
