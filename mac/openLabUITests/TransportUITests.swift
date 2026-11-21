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

    /// The log in front (the workspace status window on its Logs page) never takes the keyboard: a
    /// click in it, then Space, still fires GO.
    func testSpaceSendsGoWithTheLogWindowInFront() {
        let app = launchDemo()

        app.menuBars.menuBarItems["Window"].click()
        app.menuBars.menuItems["Workspace Status"].click()
        let status = app.descendants(matching: .any).matching(identifier: "status-window").firstMatch
        XCTAssertTrue(status.waitForExistence(timeout: 10), "the status window opens")
        tabButton("Logs", in: app).click()
        let log = app.tables.matching(identifier: "log-table").firstMatch
        XCTAssertTrue(log.waitForExistence(timeout: 10), "on its Logs page")
        // The table's own area, not a label: a table view asks for key on a click.
        log.coordinate(
            withNormalizedOffset: CGVector(dx: 0.5, dy: 0.9)
        ).click()
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])

        XCTAssertTrue(
            cueState("Running", of: "Intro", in: app).waitForExistence(timeout: 10),
            "GO fired with the log window in front")
    }
}
